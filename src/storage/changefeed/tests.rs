//! Independent whole-group fixtures and malformed-byte/resource boundaries.

use std::{collections::BTreeMap, sync::Arc};

use super::{CodecBudget, group, image, values};
use crate::{
	changefeed::{
		ChangeBatch,
		ChangeEndpoint,
		ChangeEntry,
		ChangeGraphState,
		ChangeIdentity,
		ChangeImage,
		ChangeOverlay,
		ChangefeedGap,
		ChangefeedGapReason,
		ChangefeedId,
	},
	datastore::CapturedCommit,
	gql_status::FerrosGrynnResult,
	graph::{
		AtomKind,
		AtomOccurrenceRef,
		AtomRef,
		EdgeDirection,
		GraphId,
		GraphKind,
		GraphRef,
		GraphStorageMode,
	},
	values::{DateTime, Number, Object, RecordId, RecordIdPart, Value, storage::EncodeContext},
};

/// Constructs one owned global fixture without depending on datastore records.
fn record(number: Value) -> FerrosGrynnResult<Arc<ChangeImage>> {
	let id = RecordId::new("RemovedClass", RecordIdPart::Int(17))?;
	let mut properties = Object::new();
	properties.insert("value", number);
	properties.insert(
		"when",
		Value::DateTime("2020-01-01T00:00:00[Europe/Paris]".parse::<DateTime>().map_err(
			|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()),
		)?),
	);
	Ok(Arc::new(ChangeImage {
		identity: ChangeIdentity::Record(AtomRef::new(AtomKind::Node, id)),
		properties,
		label: None,
		overlays: BTreeMap::new(),
		graph_state: None,
	}))
}

/// Uses a stamp above JavaScript's exact integer range and a fixed UTC time.
fn captured(original: bool) -> FerrosGrynnResult<CapturedCommit> {
	let before = record(Value::from(1_i64))?;
	let after = record(Value::from(1.0_f64))?;
	let source = ChangefeedId::from_uuid(uuid::Uuid::from_u128(19).into())?;
	let entry = ChangeEntry {
		ordinal: 0,
		operation: 3,
		before: Some(before),
		after: Some(after.clone()),
	};
	let net = ChangeBatch {
		source,
		versionstamp: (1_u64 << 54).into(),
		committed_at: DateTime::default(),
		entries: vec![entry.clone()],
	};
	let full = ChangeBatch {
		source,
		versionstamp: net.versionstamp(),
		committed_at: DateTime::default(),
		entries: vec![entry, ChangeEntry {
			ordinal: 1,
			operation: 4,
			before: Some(after.clone()),
			after: Some(after),
		}],
	};
	Ok(CapturedCommit {
		revision: crate::datastore::SchemaVersion::new(7).ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("fixture revision")
		})?,
		net,
		full: Some(full),
		include_original: original,
		gap: None,
	})
}

#[test]
fn changefeed_group_roundtrips_exact_numeric_tags_and_historical_full_noop() -> FerrosGrynnResult<()>
{
	for original in [false, true] {
		let expected = captured(original)?;
		let bytes = group::encode(&expected, &mut CodecBudget::new(1024 * 1024, 0)?)?;
		let decoded = group::decode(&bytes, &mut CodecBudget::new(1024 * 1024, bytes.len())?)?;
		assert_eq!(group::inspect(&bytes[..80], bytes.len() as u64)?.revision, 7);
		assert_eq!(decoded.commit.net, expected.net);
		assert_eq!(decoded.commit.full, expected.full);
		assert_eq!(decoded.commit.include_original, original);
		let entry = &decoded.commit.net.entries()[0];
		assert!(matches!(
			entry.before().and_then(|before| before.properties().get("value")),
			Some(Value::Number(Number::Int(1)))
		));
		assert!(
			matches!(entry.after().and_then(|after| after.properties().get("value")), Some(Value::Number(Number::Float(value))) if *value == 1.0)
		);
		assert_eq!(decoded.commit.full.as_ref().map(|full| full.entries().len()), Some(2));
		assert_eq!(&bytes[..8], &[b'F', b'G', b'C', b'B', 1, 0, 2 | u8::from(original), 0]);
		assert_eq!(&bytes[8..24], uuid::Uuid::from_u128(19).as_bytes());
		assert_eq!(&bytes[24..32], &(1_u64 << 54).to_le_bytes());
		assert_eq!(&bytes[32..48], &[0; 16]);
		assert_eq!(&bytes[48..52], &7_u32.to_le_bytes());
		assert_eq!(&bytes[64..72], &[1, 0, 0, 0, 2, 0, 0, 0]);
	}
	Ok(())
}

#[test]
fn changefeed_group_rejects_truncation_flags_counts_lengths_ordinals_and_budget()
-> FerrosGrynnResult<()> {
	let bytes = group::encode(&captured(false)?, &mut CodecBudget::new(1024 * 1024, 0)?)?;
	for length in 0..bytes.len() {
		assert!(group::decode(&bytes[..length], &mut CodecBudget::new(1024 * 1024, 0)?).is_err());
	}
	for (offset, replacement) in
		[(4, 2_u8), (6, 0x80), (24, 0), (48, 0), (64, 0xff), (72, 3), (73, 1)]
	{
		let mut bad = bytes.clone();
		bad[offset] = replacement;
		if offset == 24 {
			bad[24..32].fill(0);
		}
		if offset == 48 {
			bad[48..52].fill(0);
		}
		assert!(
			group::decode(&bad, &mut CodecBudget::new(1024 * 1024, 0)?).is_err(),
			"offset {offset}"
		);
	}
	for offset in [52, 56] {
		let mut bad = bytes.clone();
		bad[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(group::decode(&bad, &mut CodecBudget::new(1024 * 1024, 0)?).is_err());
	}
	let dictionary_length = u32::from_le_bytes(
		bytes[52..56]
			.try_into()
			.map_err(|_error| super::invalid("fixture dictionary length is truncated"))?,
	) as usize;
	let mut bad = bytes.clone();
	bad[80 + dictionary_length] = 1;
	assert!(group::decode(&bad, &mut CodecBudget::new(1024 * 1024, 0)?).is_err());
	assert!(group::decode(&bytes, &mut CodecBudget::new(1, 0)?).is_err());
	assert!(group::encode(&captured(false)?, &mut CodecBudget::new(1, 0)?).is_err());
	Ok(())
}

#[test]
fn changefeed_control_group_has_complete_header_and_no_image_rows() -> FerrosGrynnResult<()> {
	let mut commit = captured(false)?;
	commit.net.entries.clear();
	commit.full = None;
	commit.gap = Some(ChangefeedGap {
		source: commit.net.source(),
		reason: ChangefeedGapReason::ImportSuppressed,
		from: Some(commit.net.versionstamp()),
		through: Some(commit.net.versionstamp()),
	});
	let bytes = group::encode(&commit, &mut CodecBudget::new(1024, 0)?)?;
	assert_eq!(bytes.len(), 96);
	assert_eq!(&bytes[52..64], &[16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
	assert_eq!(&bytes[64..80], &[0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]);
	assert_eq!(&bytes[80..], &[0; 16]);
	assert_eq!(group::decode(&bytes, &mut CodecBudget::new(1024, 0)?)?.commit.gap, commit.gap);
	Ok(())
}

#[test]
fn changefeed_image_retains_renamed_refs_hyperedge_endpoint_order_and_hidden_overlay()
-> FerrosGrynnResult<()> {
	let graph_id = GraphId::new(31).ok_or_else(|| super::invalid("fixture graph id is zero"))?;
	let old =
		GraphRef::new(graph_id, "old_name", GraphKind::Metagraph, GraphStorageMode::InMemory)?;
	let new =
		GraphRef::new(graph_id, "new_name", GraphKind::Metagraph, GraphStorageMode::DiskBacked)?;
	let atom = AtomRef::new(AtomKind::Edge, RecordId::new("DeletedEdge", RecordIdPart::Int(7))?);
	let terminal = AtomOccurrenceRef::new(atom.clone(), Arc::new(new.clone()));
	let owner = AtomOccurrenceRef::new(
		AtomRef::new(AtomKind::Node, RecordId::new("DeletedNode", RecordIdPart::Int(9))?),
		Arc::new(old.clone()),
	);
	let endpoint = ChangeEndpoint {
		occurrence: terminal,
		owners: vec![owner],
	};
	let mut properties = Object::new();
	properties.insert("old", Value::from(old.clone()));
	properties.insert("new", Value::from(new.clone()));
	let mut overlays = BTreeMap::new();
	overlays.insert("masked".into(), ChangeOverlay::Hidden);
	overlays.insert("local".into(), ChangeOverlay::Value(Value::from(0_i64)));
	let expected = ChangeImage {
		identity: ChangeIdentity::Occurrence(AtomOccurrenceRef::new(atom, Arc::new(old.clone()))),
		properties,
		label: Some("historical".into()),
		overlays,
		graph_state: Some(ChangeGraphState::Edge {
			direction: EdgeDirection::Bidirectional,
			source: vec![endpoint.clone(), endpoint.clone()],
			target: vec![endpoint],
			weight: Some(Number::Int(0)),
			transition_graph: Some(new.clone()),
		}),
	};
	let mut context = EncodeContext::default();
	let bytes = image::encode(&expected, &mut context, &mut CodecBudget::new(1024 * 1024, 0)?)?;
	let dictionary = values::encode_dictionary(&context, &mut CodecBudget::new(1024 * 1024, 0)?)?;
	let restored = values::decode_dictionary(&dictionary, &mut CodecBudget::new(1024 * 1024, 0)?)?;
	assert_eq!(restored.graph_references().len(), 2);
	let decoded = image::decode(&bytes, &restored, &mut CodecBudget::new(1024 * 1024, 0)?)?;
	assert_eq!(decoded.as_ref(), &expected);
	assert_eq!(decoded.properties().get("old"), Some(&Value::from(old.clone())));
	assert_eq!(decoded.properties().get("new"), Some(&Value::from(new.clone())));
	let Some(ChangeGraphState::Edge {
		source,
		target,
		..
	}) = decoded.graph_state()
	else {
		return Err(super::invalid("fixture decoded edge state is absent"));
	};
	assert_eq!(source.len(), 2);
	assert_eq!(target.len(), 1);
	assert_eq!(source[0].occurrence().graph(), &new);
	assert_eq!(source[0].owners()[0].graph(), &old);
	// Primary conversion retains the original strict graph-id contract.
	assert!(
		EncodeContext::default()
			.register_graph(
				old,
				crate::values::storage::DiskGraphRef::new(
					GraphId::new(1).ok_or_else(|| super::invalid("fixture slot is zero"))?
				)
			)
			.is_err()
	);
	Ok(())
}

#[test]
fn changefeed_canonical_conversion_admits_dictionary_expansion_before_cloning()
-> FerrosGrynnResult<()> {
	let mut context = EncodeContext::default();
	let mut object = Object::new();
	object.insert("x".repeat(8192), Value::from(1_i64));
	let bytes = values::encode(
		&Value::Object(object),
		&mut context,
		&mut CodecBudget::new(1024 * 1024, 0)?,
	)?;
	let mut budget = CodecBudget::new(8192, 0)?;
	assert!(values::decode(&bytes, &context, &mut budget).is_err());
	let decoded = values::decode(&bytes, &context, &mut CodecBudget::new(1024 * 1024, 0)?)?;
	assert_eq!(decoded.as_object().map(Object::len), Some(1));
	Ok(())
}

#[test]
fn changefeed_geometry_validation_is_admitted_before_kernel_allocation() -> FerrosGrynnResult<()> {
	let point = crate::values::Position::new([1.0, 2.0])?;
	let value = Value::Geometry(crate::values::Geometry::MultiPoint(vec![point; 2000]));
	let mut context = EncodeContext::default();
	assert!(values::encode(&value, &mut context, &mut CodecBudget::new(1024, 0)?).is_err());
	let bytes = values::encode(&value, &mut context, &mut CodecBudget::new(1024 * 1024, 0)?)?;
	let (storage, allocated) =
		crate::values::storage::codec::CanonicalCodec::decode_with_allocation(
			&bytes,
			crate::values::storage::codec::DecodeLimits::default(),
		)?;
	let reservation = storage.runtime_allocation_bytes(&context.decoder())?;
	assert!(reservation >= 2000 * 16);
	let mut conversion_budget = CodecBudget::new(allocated + reservation - 1, 0)?;
	assert!(values::decode(&bytes, &context, &mut conversion_budget).is_err());
	assert_eq!(conversion_budget.used, allocated);
	assert_eq!(values::decode(&bytes, &context, &mut CodecBudget::new(1024 * 1024, 0)?)?, value);
	Ok(())
}

#[test]
fn changefeed_dictionary_and_singleton_object_admit_complete_tree_nodes() -> FerrosGrynnResult<()> {
	let mut dictionary = Vec::new();
	dictionary.extend_from_slice(&0_u32.to_le_bytes());
	dictionary.extend_from_slice(&1_u32.to_le_bytes());
	dictionary.extend_from_slice(&1_u64.to_le_bytes());
	dictionary.push(b'C');
	dictionary.extend_from_slice(&[0; 8]);
	let mut budget = CodecBudget::new(300, 0)?;
	assert!(values::decode_dictionary(&dictionary, &mut budget).is_err());
	assert_eq!(budget.used, 0);
	let decoded = values::decode_dictionary(&dictionary, &mut CodecBudget::new(16 * 1024, 0)?)?;
	assert_eq!(decoded.class_names().len(), 1);
	let arbitrary_name = "x".repeat(8192);
	let mut timezone_context = EncodeContext::default();
	assert!(
		values::prepare_time_zone(
			&arbitrary_name,
			&mut timezone_context,
			&mut CodecBudget::new(512, 0)?
		)
		.is_err()
	);
	assert_eq!(timezone_context.time_zone_names().len(), 0);
	let mut zones = Vec::new();
	zones.extend_from_slice(&[0; 8]);
	zones.extend_from_slice(&1_u32.to_le_bytes());
	zones.extend_from_slice(&12_u64.to_le_bytes());
	zones.extend_from_slice(b"Europe/Paris");
	zones.extend_from_slice(&0_u32.to_le_bytes());
	let mut zone_budget = CodecBudget::new(350, 0)?;
	assert!(values::decode_dictionary(&zones, &mut zone_budget).is_err());
	assert_eq!(zone_budget.used, 0);
	assert_eq!(
		values::decode_dictionary(&zones, &mut CodecBudget::new(16 * 1024, 0)?)?
			.time_zone_names()
			.len(),
		1
	);
	let mut context = EncodeContext::default();
	let mut object = Object::new();
	object.insert("x", Value::from(1_i64));
	let bytes = values::encode(
		&Value::Object(object),
		&mut context,
		&mut CodecBudget::new(1024 * 1024, 0)?,
	)?;
	let (storage, allocated) =
		crate::values::storage::codec::CanonicalCodec::decode_with_allocation(
			&bytes,
			crate::values::storage::codec::DecodeLimits::default(),
		)?;
	let reservation = storage.runtime_allocation_bytes(&context.decoder())?;
	assert!(reservation >= 4096);
	let mut budget = CodecBudget::new(allocated + 1024, 0)?;
	assert!(values::decode(&bytes, &context, &mut budget).is_err());
	assert_eq!(budget.used, allocated);
	Ok(())
}

#[test]
fn changefeed_outer_property_names_share_the_reader_text_cap_before_intern() -> FerrosGrynnResult<()>
{
	let name = crate::values::Text::from("x".repeat(16 * 1024 * 1024 + 1));
	let mut context = EncodeContext::default();
	let mut budget = CodecBudget::new(256 * 1024 * 1024, 0)?;
	assert!(values::property_slot(&mut context, &name, &mut budget).is_err());
	assert_eq!(budget.used, 0);
	assert_eq!(context.property_dictionary().len(), 0);
	assert!(values::append_text(&mut Vec::new(), &name, &mut budget).is_err());
	assert_eq!(budget.used, 0);
	Ok(())
}

#[test]
fn changefeed_large_images_roundtrip_whole_commit_continuations() -> FerrosGrynnResult<()> {
	let mut commit = captured(false)?;
	commit.full = None;
	commit.net.entries[0].before = Some(record(Value::from("before".repeat(10_000)))?);
	commit.net.entries[0].after = Some(record(Value::from("after".repeat(10_000)))?);
	let bytes = group::encode(&commit, &mut CodecBudget::new(16 * 1024 * 1024, 0)?)?;
	let (locator, pages) = super::encode_pages(
		&bytes,
		commit.net.versionstamp().get(),
		1,
		&mut CodecBudget::new(16 * 1024 * 1024, bytes.len())?,
	)?;
	assert!(locator.pages > 1);
	let mut reads = 0;
	let mut read = |number: crate::storage::page::PageNo,
	                output: &mut crate::storage::page::AlignedPage| {
		reads += 1;
		output.as_bytes_mut().copy_from_slice(pages[(number.get() - 1) as usize].as_bytes());
		Ok(())
	};
	let restored = super::decode_pages(
		locator,
		u64::from(locator.pages) + 1,
		&mut read,
		&mut CodecBudget::new(16 * 1024 * 1024, 0)?,
	)?;
	assert_eq!(reads, locator.pages);
	assert_eq!(restored, bytes);
	assert_eq!(
		group::decode(&restored, &mut CodecBudget::new(16 * 1024 * 1024, restored.len())?)?
			.commit
			.net,
		commit.net
	);
	Ok(())
}

#[test]
fn changefeed_writer_rejects_canonical_record_and_geometry_depth_mismatch() -> FerrosGrynnResult<()>
{
	let id = RecordId::new("C", RecordIdPart::Int(1))?;
	for wrappers in [63, 64] {
		let mut value = Value::from(id.clone());
		for _index in 0..wrappers {
			value = Value::from(vec![value]);
		}
		let mut context = EncodeContext::default();
		let encoded = values::encode(&value, &mut context, &mut CodecBudget::new(1024 * 1024, 0)?);
		if wrappers == 63 {
			assert_eq!(
				values::decode(&encoded?, &context, &mut CodecBudget::new(1024 * 1024, 0)?)?,
				value
			);
		} else {
			assert!(encoded.is_err());
		}
	}
	let mut geometry = crate::values::Geometry::Point(crate::values::Position::new([1.0, 2.0])?);
	for _index in 0..64 {
		geometry = crate::values::Geometry::Collection(vec![geometry]);
	}
	let value = Value::from(vec![Value::Geometry(geometry)]);
	assert!(
		values::encode(
			&value,
			&mut EncodeContext::default(),
			&mut CodecBudget::new(1024 * 1024, 0)?
		)
		.is_err()
	);
	Ok(())
}
