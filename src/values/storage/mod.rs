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
mod context;
mod dto;
mod order_key;
mod tags_refs;

use crate::gql_status::FerrosGrynnError;

const DISK_ATOM_KIND_BIT: u64 = 1 << 40;
const DISK_ATOM_SLOT_MASK: u64 = DISK_ATOM_KIND_BIT - 1;
const DISK_ATOM_ALLOWED_MASK: u64 = (DISK_ATOM_KIND_BIT << 1) - 1;
const FIRST_FIXED_TIME_ZONE_ID: u32 = 1_000_000;
const FIXED_TIME_ZONE_BIAS: i32 = 86_399;
const MAX_FIXED_TIME_ZONE_OFFSET: i32 = 86_400;
const MAX_IANA_TIME_ZONE_ID: u32 = FIRST_FIXED_TIME_ZONE_ID - 1;

pub use context::{DecodeContext, EncodeContext};
pub use dto::{
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
pub use order_key::{encode_index_key, validate_index_type};
pub use tags_refs::{DiskAtomRef, DiskGraphRef, QualifiedAtomRef, TypeTag};

fn canonical_f64(value: f64) -> u64 {
	if value.is_nan() {
		f64::NAN.to_bits()
	} else if value == 0.0 {
		0.0_f64.to_bits()
	} else {
		value.to_bits()
	}
}

fn unresolved(reference_type: &str, reference: String) -> FerrosGrynnError {
	FerrosGrynnError::unresolved_storage_reference(reference_type, reference)
}

#[cfg(test)]
mod tests {
	use std::ops::Bound;

	use jiff::{
		Timestamp,
		tz::{Offset, TimeZone},
	};

	use super::{
		DiskAtomRef,
		DiskGraphRef,
		EncodeContext,
		StorageDecimal,
		StorageValue,
		codec::{CanonicalCodec, DecodeLimits},
	};
	use crate::values::{
		Array,
		AtomKind,
		AtomRef,
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

	#[test]
	fn runtime_storage_conversion_preserves_nested_values_and_dictionaries() {
		let record = RecordId::new(
			"person",
			RecordIdPart::Object(Object::from_iter([("region", Value::from("eu"))])),
		)
		.unwrap();
		let atom = AtomRef::new(AtomKind::Node, record.clone());
		let mut object = Object::new();
		object.insert("name", "Ada");
		object.insert("record", record);
		object.insert("set", Value::Set(Set::from(vec![3_i64, 1, 2])));
		object.insert(
			"range",
			Range::new(Bound::Included(1_i64.into()), Bound::Excluded(4_i64.into())).unwrap(),
		);
		object.insert("atom", atom.clone());

		let mut context = EncodeContext::default();
		context.register_atom(atom, DiskAtomRef::from_parts(AtomKind::Node, 7).unwrap()).unwrap();
		let storage =
			StorageValue::from_runtime(Value::Object(object.clone()), &mut context).unwrap();
		let encoded = CanonicalCodec::encode(storage.as_ref()).unwrap();
		let storage = CanonicalCodec::decode(&encoded, DecodeLimits::default()).unwrap();
		let runtime = storage.into_runtime(&context.decoder()).unwrap();
		assert_eq!(runtime, Value::Object(object));
		assert_eq!(context.property_dictionary().len(), 5);
	}

	#[test]
	fn graph_references_use_explicit_context_mapping() {
		let id = RecordId::new("graph", RecordIdPart::Int(1)).unwrap();
		let graph = GraphRef::new(id, GraphKind::Dag, GraphStorageMode::DiskBacked);
		let disk = DiskGraphRef::new(8, 2);
		let mut context = EncodeContext::default();
		context.register_graph(graph.clone(), disk).unwrap();

		let storage = StorageValue::from_graph_ref(&graph, &context).unwrap();
		assert_eq!(storage.into_graph_ref(&context.decoder()).unwrap(), graph);
	}

	#[test]
	fn rejects_runtime_only_values_and_missing_references() {
		let mut context = EncodeContext::default();
		let regex = "a+".parse::<crate::values::Regex>().unwrap();
		let error = StorageValue::from_runtime(Value::Regex(regex), &mut context).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22S05");

		let record = RecordId::new("person", RecordIdPart::Int(1)).unwrap();
		let atom = AtomRef::new(AtomKind::Node, record);
		let error = StorageValue::from_runtime(Value::from(atom), &mut context).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22S04");
	}

	#[test]
	fn array_storage_conversion_reuses_owned_elements() {
		let mut context = EncodeContext::default();
		let value = Value::Array(Array::new(vec![
			Value::from("text"),
			Value::Bytes(crate::values::Bytes::from([1_u8, 2])),
		]));
		let storage = StorageValue::from_runtime(value.clone(), &mut context).unwrap();
		assert_eq!(storage.into_runtime(&context.decoder()).unwrap(), value);
	}

	#[test]
	fn datetime_conversion_preserves_pre_epoch_nanos_and_time_zone_kind() {
		let utc =
			"1969-12-31T23:59:59.500000000+00:00[UTC]".parse::<crate::values::DateTime>().unwrap();
		let fixed = crate::values::DateTime::new(
			Timestamp::new(1_700_000_000, 123)
				.unwrap()
				.to_zoned(TimeZone::fixed(Offset::from_seconds(19_800).unwrap())),
		);
		let mut context = EncodeContext::default();

		let utc_storage = StorageValue::from_runtime(Value::DateTime(utc), &mut context).unwrap();
		let StorageValue::DateTime(utc_storage) = utc_storage else {
			panic!("datetime conversion changed the storage variant");
		};
		assert_eq!(
			(utc_storage.seconds, utc_storage.nanos, utc_storage.time_zone_id),
			(-1, 500_000_000, 0),
		);

		let fixed_storage =
			StorageValue::from_runtime(Value::DateTime(fixed.clone()), &mut context).unwrap();
		let fixed_runtime = fixed_storage.into_runtime(&context.decoder()).unwrap();
		let Value::DateTime(fixed_runtime) = fixed_runtime else {
			panic!("datetime decoding changed the runtime variant");
		};
		assert_eq!(fixed_runtime, fixed);
		assert_eq!(fixed_runtime.as_inner().offset().seconds(), 19_800);
	}

	#[test]
	fn wider_storage_decimal_fails_without_precision_loss() {
		let error = StorageDecimal {
			mantissa: i128::MAX,
			scale: 0,
		}
		.into_runtime()
		.unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22S03");
		assert!(error.technical_source().is_some());
	}

	#[test]
	fn conversion_context_rejects_conflicting_reference_mappings() {
		let first =
			AtomRef::new(AtomKind::Node, RecordId::new("person", RecordIdPart::Int(1)).unwrap());
		let second =
			AtomRef::new(AtomKind::Node, RecordId::new("person", RecordIdPart::Int(2)).unwrap());
		let disk = DiskAtomRef::from_parts(AtomKind::Node, 7).unwrap();
		let mut context = EncodeContext::default();
		context.register_atom(first, disk).unwrap();
		let error = context.register_atom(second, disk).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22S07");
	}

	#[test]
	fn set_conversion_rejects_cross_number_duplicates() {
		let context = EncodeContext::default();
		let value =
			StorageValue::Set(vec![StorageValue::Int(1), StorageValue::Float(1.0_f64.to_bits())]);
		let error = value.into_runtime(&context.decoder()).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22S02");
	}
}
