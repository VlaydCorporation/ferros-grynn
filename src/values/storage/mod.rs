#![allow(
	dead_code,
	reason = "the storage adapter is internal until the page and columnar integration stage"
)]
#![allow(
	clippy::missing_docs_in_private_items,
	reason = "the crate-private storage DTO is documented as one format contract; field-level \
	          layout is defined by disk_storage_spec"
)]

//! Crate-private storage DTOs and runtime conversion contexts.

pub mod codec;
/// Reference-resolution contexts at the runtime-to-storage boundary.
mod context;
/// Internal storage value representations, separate from runtime SDK types.
mod dto;
mod order_key;
/// Explicit value tags and validated durable graph and atom references.
mod tags_refs;

use crate::gql_status::FerrosGrynnError;

/// Bit separating edge from node references above the 40-bit durable slot
/// field.
const DISK_ATOM_KIND_BIT: u64 = 1 << 40;
/// Mask selecting the 40-bit durable atom slot, without a generation field.
const DISK_ATOM_SLOT_MASK: u64 = DISK_ATOM_KIND_BIT - 1;
/// Mask of all valid durable atom-reference bits; higher bits must be zero.
const DISK_ATOM_ALLOWED_MASK: u64 = (DISK_ATOM_KIND_BIT << 1) - 1;
/// First time-zone identifier reserved for directly encoded fixed UTC offsets.
const FIRST_FIXED_TIME_ZONE_ID: u32 = 1_000_000;
/// Bias in seconds that maps negative fixed UTC offsets into nonnegative
/// identifiers.
const FIXED_TIME_ZONE_BIAS: i32 = 86_399;
/// Largest fixed UTC offset, in seconds, accepted by the storage context.
const MAX_FIXED_TIME_ZONE_OFFSET: i32 = 86_400;
/// Largest identifier reserved for dictionary-backed IANA time-zone names.
const MAX_IANA_TIME_ZONE_ID: u32 = FIRST_FIXED_TIME_ZONE_ID - 1;

pub use context::{DecodeContext, EncodeContext};
pub use dto::{
	StorageAtomRef,
	StorageBound,
	StorageDateTime,
	StorageDecimal,
	StorageDuration,
	StorageRange,
	StorageRecordId,
	StorageRecordIdPart,
	StorageRecordValue,
	StorageValue,
	StorageValueRef,
};
pub use order_key::{encode_index_key, encode_index_prefix, validate_index_type};
pub use tags_refs::{DiskAtomRef, DiskGraphRef, QualifiedAtomRef, TypeTag};

pub use crate::memory::tree_allocation_bytes;

/// Returns canonical f64 for this module.
fn canonical_f64(value: f64) -> u64 {
	if value.is_nan() {
		f64::NAN.to_bits()
	} else if value == 0.0 {
		0.0_f64.to_bits()
	} else {
		value.to_bits()
	}
}

/// Returns unresolved for this module.
fn unresolved(reference_type: &str, reference: String) -> FerrosGrynnError {
	FerrosGrynnError::unresolved_storage_reference(reference_type, reference)
}

#[cfg(test)]
/// Regression tests for storage behavior.
mod tests {
	use std::ops::Bound;

	use jiff::{
		Timestamp,
		tz::{Offset, TimeZone},
	};

	use super::{
		DiskGraphRef,
		EncodeContext,
		StorageDecimal,
		StorageValue,
		codec::{CanonicalCodec, DecodeLimits},
	};
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::values::{
		Array,
		AtomKind,
		AtomRef,
		GraphId,
		GraphKind,
		GraphRef,
		GraphStorageMode,
		Object,
		Range,
		RecordId,
		RecordIdPart,
		Set,
		Value,
	};

	/// Verifies runtime storage conversion preserves nested values and
	/// dictionaries.
	#[test]
	fn runtime_storage_conversion_preserves_nested_values_and_dictionaries() {
		let record = RecordId::new(
			"person",
			RecordIdPart::Object(Object::from_iter([("region", Value::from("eu"))])),
		)
		.assert_ok();
		let atom = AtomRef::new(AtomKind::Node, record.clone());
		let mut object = Object::new();
		object.insert("name", "Ada");
		object.insert("record", record);
		object.insert("set", Value::Set(Set::from(vec![3_i64, 1, 2])));
		object.insert(
			"range",
			Range::new(Bound::Included(1_i64.into()), Bound::Excluded(4_i64.into())).assert_ok(),
		);
		object.insert("atom", atom);

		let mut context = EncodeContext::default();
		context.register_class_name(41, "person".into()).assert_ok();
		let storage =
			StorageValue::from_runtime(Value::Object(object.clone()), &mut context).assert_ok();
		let encoded = CanonicalCodec::encode(storage.as_ref()).assert_ok();
		let storage = CanonicalCodec::decode(&encoded, DecodeLimits::default()).assert_ok();
		let runtime = storage.into_runtime(&context.decoder()).assert_ok();
		assert_eq!(runtime, Value::Object(object));
		assert_eq!(context.property_dictionary().len(), 5);
		assert_eq!(
			context.class_names().map(|(id, name)| (id, name.as_str())).collect::<Vec<_>>(),
			vec![(41, "person")]
		);
	}

	/// Verifies graph references use explicit context mapping.
	#[test]
	fn graph_references_use_explicit_context_mapping() {
		let id = GraphId::new(8).assert_some();
		let graph = GraphRef::new(id, "analytics", GraphKind::Dag, GraphStorageMode::DiskBacked)
			.assert_ok();
		let disk = DiskGraphRef::new(id);
		let mut context = EncodeContext::default();
		context.register_graph(graph.clone(), disk).assert_ok();

		let storage = StorageValue::from_graph_ref(&graph, &context).assert_ok();
		assert_eq!(storage.into_graph_ref(&context.decoder()).assert_ok(), graph);
	}

	/// Verifies rejects runtime only values and missing references.
	#[test]
	fn rejects_runtime_only_values_and_missing_references() {
		let mut context = EncodeContext::default();
		let regex = "a+".parse::<crate::values::Regex>().assert_ok();
		let error = StorageValue::from_runtime(Value::Regex(regex), &mut context).assert_err();
		assert_eq!(error.status().code().to_string(), "FG-22S05");

		let record = RecordId::new("person", RecordIdPart::Int(1)).assert_ok();
		let atom = AtomRef::new(AtomKind::Node, record);
		let error = StorageValue::from_runtime(Value::from(atom), &mut context).assert_err();
		assert_eq!(error.status().code().to_string(), "FG-22S04");
	}

	/// Verifies array storage conversion reuses owned elements.
	#[test]
	fn array_storage_conversion_reuses_owned_elements() {
		let mut context = EncodeContext::default();
		let value = Value::Array(Array::new(vec![
			Value::from("text"),
			Value::Bytes(crate::values::Bytes::from([1_u8, 2])),
		]));
		let storage = StorageValue::from_runtime(value.clone(), &mut context).assert_ok();
		assert_eq!(storage.into_runtime(&context.decoder()).assert_ok(), value);
	}

	/// Verifies datetime conversion preserves pre epoch nanos and time zone
	/// kind.
	#[test]
	fn datetime_conversion_preserves_pre_epoch_nanos_and_time_zone_kind() {
		let utc = "1969-12-31T23:59:59.500000000+00:00[UTC]"
			.parse::<crate::values::DateTime>()
			.assert_ok();
		let fixed = crate::values::DateTime::new(
			Timestamp::new(1_700_000_000, 123)
				.assert_ok()
				.to_zoned(TimeZone::fixed(Offset::from_seconds(19_800).assert_ok())),
		);
		let mut context = EncodeContext::default();

		let utc_storage =
			StorageValue::from_runtime(Value::DateTime(utc), &mut context).assert_ok();
		let utc_storage = match utc_storage {
			StorageValue::DateTime(utc_storage) => Some(utc_storage),
			_ => None,
		}
		.assert_some_with("datetime conversion changed the storage variant");
		assert_eq!(
			(utc_storage.seconds, utc_storage.nanos, utc_storage.time_zone_id),
			(-1, 500_000_000, 0),
		);

		let fixed_storage =
			StorageValue::from_runtime(Value::DateTime(fixed.clone()), &mut context).assert_ok();
		let fixed_runtime = fixed_storage.into_runtime(&context.decoder()).assert_ok();
		let fixed_runtime = match fixed_runtime {
			Value::DateTime(fixed_runtime) => Some(fixed_runtime),
			_ => None,
		}
		.assert_some_with("datetime decoding changed the runtime variant");
		assert_eq!(fixed_runtime, fixed);
		assert_eq!(fixed_runtime.as_inner().offset().seconds(), 19_800);
	}

	/// Verifies wider storage decimal fails without precision loss.
	#[test]
	fn wider_storage_decimal_fails_without_precision_loss() {
		let error = StorageDecimal {
			mantissa: i128::MAX,
			scale: 0,
		}
		.into_runtime()
		.assert_err();
		assert_eq!(error.status().code().to_string(), "FG-22S03");
		assert!(error.technical_source().is_some());
	}

	/// Verifies atom reference round trip does not need a physical address
	/// mapping.
	#[test]
	fn atom_reference_round_trip_does_not_need_a_physical_address_mapping() {
		let atom =
			AtomRef::new(AtomKind::Node, RecordId::new("person", RecordIdPart::Int(1)).assert_ok());
		let mut context = EncodeContext::default();
		context.register_class_name(7, "person".into()).assert_ok();
		let stored =
			StorageValue::from_runtime(Value::from(atom.clone()), &mut context).assert_ok();
		let bytes = CanonicalCodec::encode(stored.as_ref()).assert_ok();
		let decoded = CanonicalCodec::decode(&bytes, DecodeLimits::default()).assert_ok();
		assert_eq!(decoded.into_runtime(&context.decoder()).assert_ok(), Value::from(atom));
	}

	/// Verifies set conversion rejects cross number duplicates.
	#[test]
	fn set_conversion_rejects_cross_number_duplicates() {
		let context = EncodeContext::default();
		let value =
			StorageValue::Set(vec![StorageValue::Int(1), StorageValue::Float(1.0_f64.to_bits())]);
		let error = value.into_runtime(&context.decoder()).assert_err();
		assert_eq!(error.status().code().to_string(), "FG-22S02");
	}
}
