use std::{
	collections::{BTreeMap, BTreeSet},
	num::NonZeroU16,
};

use super::*;
use crate::{
	datastore::{
		AnalyzerId,
		AnalyzerTokenizer,
		ClassIndexKind,
		ClassIndexOptions,
		ClassIndexState,
		ClassSchemaMode,
		DiskAnnIndexOptions,
		FulltextIndexOptions,
		FunctionEffect,
		FunctionId,
		HnswIndexOptions,
		IndexScope,
		VectorDistance,
	},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, GraphKind, GraphTopologyConstraints},
	storage::types::{DataFileId, DataFileKind},
	values::{
		PropertyKeyId,
		Type,
		VectorKind,
		storage::{StorageValue, codec::CanonicalCodec},
	},
};

/// Verifies fgdc v15 round trips recursive types bindings and graph properties.
#[test]
fn fgdc_v15_round_trips_recursive_types_bindings_and_graph_properties() {
	use crate::test_assertions::AssertResult as _;

	let catalog = fixture().assert_ok();
	let bytes = catalog.encode().assert_ok();
	assert_eq!(&bytes[..4], b"FGDC");
	assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 15);
	assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
}

/// Creates a valid small entry so corruption tests change only one invariant.
fn live_binding_fixture(
	id: u128,
	expression: crate::live::LiveExpr,
) -> FerrosGrynnResult<LiveBindingCatalogEntry> {
	use std::sync::Arc;

	use crate::{
		live::{LiveBindingId, LiveBindingOptions, LiveState, LiveVersion},
		storage::live_descriptor::{self, LiveDescriptor},
		values::{Object, Value},
	};
	let dependencies = crate::live::dependencies::expression_dependencies(
		&expression,
		&mut crate::live::dependencies::BindingAdmission::new(&LiveBindingOptions::default()),
	)?;
	let descriptor = live_descriptor::encode(
		&LiveDescriptor::Binding {
			expression: expression.clone(),
			parameters: Object::new(),
		},
		1024 * 1024,
	)?;
	let value = Value::from(9_007_199_254_740_993_i64);
	let value_descriptor = live_descriptor::encode_binding_value(&value, 1024 * 1024)?;
	Ok(LiveBindingCatalogEntry {
		id: LiveBindingId::from_uuid(uuid::Uuid::from_u128(id).into())?,
		version: LiveVersion::default(),
		state: LiveState::Active,
		defined_at: 1,
		published_at: 2,
		value_at: 2,
		options: LiveBindingOptions::default(),
		expression: Arc::new(expression),
		parameters: Arc::new(Object::new()),
		value: Arc::new(value),
		descriptor: descriptor.into(),
		value_descriptor: value_descriptor.into(),
		dependencies,
		functions: Vec::new(),
		record_sources: Vec::new(),
		diagnostic: None,
	})
}

/// Large exact values and fault heads survive checked binding sections.
#[test]
fn fgdc_live_bindings_check_headers_lengths_limits_and_fault_bounds() {
	use crate::{
		live::{LiveExpr, LiveState},
		test_assertions::AssertResult as _,
		values::Value,
	};
	let mut catalog = fixture().assert_ok();
	catalog
		.live_bindings
		.push(live_binding_fixture(42, LiveExpr::Literal(Value::from(1_i64))).assert_ok());
	let bytes = catalog.encode().assert_ok();
	assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
	assert_eq!(&bytes[128..132], &1_u32.to_le_bytes());
	let entry = &catalog.live_bindings[0];
	let start = bytes.len() - 104 - entry.descriptor.len() - entry.value_descriptor.len();
	for offset in [
		start + 48,
		start + 49,
		start + 52,
		start + 72,
		start + 80,
		start + 88,
		start + 92,
		start + 96,
		start + 100,
		132,
	] {
		let mut malformed = bytes.clone();
		malformed[offset] = 255;
		assert!(DatabaseCatalog::decode(&malformed).is_err(), "offset {offset}");
	}
	for offset in [start, start + 16, start + 56, start + 64] {
		let mut malformed = bytes.clone();
		let length = if offset == start {
			16
		} else {
			8
		};
		malformed[offset..offset + length].fill(0);
		assert!(DatabaseCatalog::decode(&malformed).is_err(), "zero at {offset}");
	}
	let mut huge_count = bytes.clone();
	huge_count[128..132].copy_from_slice(&u32::MAX.to_le_bytes());
	assert!(DatabaseCatalog::decode(&huge_count).is_err());
	for length in start..bytes.len() {
		assert!(DatabaseCatalog::decode(&bytes[..length]).is_err());
	}
	catalog.live_bindings[0].state = LiveState::Paused;
	catalog.live_bindings[0].diagnostic = Some(("FG-G1F09".into(), "evaluation failed".into()));
	assert_eq!(DatabaseCatalog::decode(&catalog.encode().assert_ok()).assert_ok(), catalog);
	catalog.live_bindings[0].value_at = 3;
	assert!(catalog.encode().is_err());
}

/// Runtime-invalid dependencies must not become Active merely through reopen.
#[test]
fn fgdc_live_record_owner_tails_round_trip_and_reject_malformed_fields() {
	use crate::{
		live::{LiveExpr, LiveQuery, LiveSource},
		test_assertions::AssertResult as _,
	};
	let mut catalog = fixture().assert_ok();
	let source = catalog.classes[0].clone();
	let expression = LiveExpr::Select(Box::new(LiveQuery::new(LiveSource::Record(
		format!("{}:a", source.name).parse().assert_ok(),
	))));
	let mut entry = live_binding_fixture(42, expression).assert_ok();
	entry.record_sources.push((source.name.clone(), source.id));
	catalog.live_bindings.push(entry);
	let bytes = catalog.encode().assert_ok();
	assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
	let entry = &catalog.live_bindings[0];
	let start = bytes.len()
		- 104
		- 8
		- source.name.len()
		- entry.descriptor.len()
		- entry.value_descriptor.len();
	for offset in [start + 104, start + 108, start + 112] {
		let mut malformed = bytes.clone();
		if offset == start + 104 {
			malformed[offset..offset + 4].fill(0);
		} else {
			malformed[offset] = 255;
		}
		assert!(DatabaseCatalog::decode(&malformed).is_err(), "owner offset {offset}");
	}
	catalog.live_bindings[0].record_sources.clear();
	assert!(catalog.encode().is_err());
}

/// Runtime-invalid dependencies must not become Active merely through reopen.
#[test]
fn fgdc_live_bindings_reject_cycles_sources_and_invalid_function_references() {
	use std::sync::Arc;

	use crate::{
		live::{
			LiveBindingId,
			LiveExpr,
			LiveId,
			LiveOptions,
			LiveProjection,
			LiveQuery,
			LiveSource,
			LiveState,
			LiveVersion,
		},
		storage::live_descriptor::{self, LiveDescriptor},
		test_assertions::AssertResult as _,
		values::Value,
	};
	let mut catalog = fixture().assert_ok();
	let first = LiveBindingId::from_uuid(uuid::Uuid::from_u128(1).into()).assert_ok();
	let second = LiveBindingId::from_uuid(uuid::Uuid::from_u128(2).into()).assert_ok();
	catalog.live_bindings = vec![
		live_binding_fixture(1, LiveExpr::Binding(second)).assert_ok(),
		live_binding_fixture(2, LiveExpr::Binding(first)).assert_ok(),
	];
	assert!(catalog.encode().is_err());
	catalog.live_bindings =
		vec![live_binding_fixture(1, LiveExpr::Literal(Value::Null)).assert_ok()];
	catalog.live_bindings[0]
		.functions
		.push((FunctionId::from_schema_id(catalog.classes[0].id.into()), SchemaVersion::INITIAL));
	assert!(catalog.encode().is_err());
	catalog.live_bindings[0].functions.clear();
	let query = LiveQuery::new(LiveSource::Class(catalog.classes[0].id))
		.with_projection(LiveProjection::Value(LiveExpr::Binding(second)));
	let descriptor =
		live_descriptor::encode(&LiveDescriptor::Query(query.clone()), 1024 * 1024).assert_ok();
	catalog.live_queries.push(LiveCatalogEntry {
		graph_checkpoint: None,
		id: LiveId::from_uuid(uuid::Uuid::from_u128(3).into()).assert_ok(),
		version: LiveVersion::default(),
		state: LiveState::Active,
		defined_at: 1,
		starting_after: 1,
		published_at: 1,
		acknowledged: 1,
		options: LiveOptions::default(),
		query: Arc::new(query),
		descriptor: descriptor.into(),
		source_class: Some(catalog.classes[0].id),
		capture_source: crate::changefeed::ChangefeedSource::Class(catalog.classes[0].id),
		capture_scope_name: None,
		rhs_scope_anchors: Vec::new(),
		functions: Vec::new(),
		record_sources: Vec::new(),
		diagnostic: None,
	});
	assert!(catalog.encode().is_err());
}

/// Private coverage epochs are authoritative and cannot impersonate another
/// source, configuration, query version or lifecycle.
#[test]
fn fgdc_live_history_epochs_bounds_and_malformed_sections() {
	use crate::{
		changefeed::{
			ChangefeedCaptureEpoch,
			ChangefeedConfig,
			ChangefeedId,
			ChangefeedSource,
			ChangefeedState,
		},
		datastore::{LiveCaptureEpoch, LiveCapturePolicy, LiveHistoryMetadata},
		live::{LiveId, LiveOptions, LiveQuery, LiveSource, LiveState, LiveVersion},
		storage::live_descriptor::{self, LiveDescriptor},
		test_assertions::AssertResult as _,
	};
	let mut catalog = fixture().assert_ok();
	let config = ChangefeedConfig::from_nanoseconds(1234).assert_ok();
	let feed = ChangefeedId::from_uuid(uuid::Uuid::from_u128(42).into()).assert_ok();
	let id = LiveId::from_uuid(uuid::Uuid::from_u128(43).into()).assert_ok();
	let query = LiveQuery::new(LiveSource::Class(catalog.classes[0].id));
	catalog.live_queries.push(LiveCatalogEntry {
		graph_checkpoint: None,
		id,
		version: LiveVersion::default(),
		state: LiveState::Active,
		defined_at: 1,
		starting_after: 1,
		published_at: 1,
		acknowledged: 1,
		options: LiveOptions::default(),
		descriptor: live_descriptor::encode(&LiveDescriptor::Query(query.clone()), 1024 * 1024)
			.assert_ok()
			.into(),
		query: std::sync::Arc::new(query),
		source_class: Some(catalog.classes[0].id),
		capture_source: crate::changefeed::ChangefeedSource::Class(catalog.classes[0].id),
		capture_scope_name: None,
		rhs_scope_anchors: Vec::new(),
		functions: Vec::new(),
		record_sources: Vec::new(),
		diagnostic: None,
	});
	catalog.changefeeds.push(ChangefeedCatalogEntry {
		id: feed,
		source: ChangefeedSource::Class(catalog.classes[0].id),
		name: "person".into(),
		config,
		state: ChangefeedState::Active,
		version: SchemaVersion::INITIAL,
		defined_at: 2,
		published_at: 2,
		last_stamp: 0,
		purged_through: 0,
		purged_time_ns: 0,
		retained_page: 0,
		retained_groups: 0,
		closed_at: None,
		closed_time_ns: None,
		epochs: vec![ChangefeedCaptureEpoch {
			revision: SchemaVersion::INITIAL,
			started_at: 2.into(),
			configuration: Some(config),
		}],
		segments: Vec::new(),
	});
	catalog.live_history.push(LiveHistoryCatalogEntry {
		id,
		metadata: LiveHistoryMetadata {
			version: LiveVersion::default(),
			last_stamp: 10,
			purged_through: 0,
			purged_time_ns: 0,
			retained_page: 0,
			retained_groups: 0,
			epochs: vec![LiveCaptureEpoch {
				started_at: 2,
				policy: Some(
					LiveCapturePolicy::from_source(feed, SchemaVersion::INITIAL, config)
						.assert_ok(),
				),
			}],
		},
		segments: Vec::new(),
	});
	let bytes = catalog.encode().assert_ok();
	assert_eq!(&bytes[132..136], &1_u32.to_le_bytes());
	assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
	let offset = bytes.len() - super::live_history::MIN_BYTES;
	for end in offset..bytes.len() {
		assert!(DatabaseCatalog::decode(&bytes[..end]).is_err());
	}
	for position in [offset + 80 + 9, offset + 80 + 10, offset + 80 + 11] {
		let mut malformed = bytes.clone();
		malformed[position] = 1;
		assert!(DatabaseCatalog::decode(&malformed).is_err());
	}
	for position in [132, offset + 72, offset + 76] {
		let mut malformed = bytes.clone();
		malformed[position..position + 4].copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(DatabaseCatalog::decode(&malformed).is_err());
	}
	for (position, length) in [(offset + 16, 8), (offset + 80 + 12, 4), (offset + 80 + 16, 16)] {
		let mut malformed = bytes.clone();
		malformed[position..position + length].fill(0);
		assert!(DatabaseCatalog::decode(&malformed).is_err());
	}
	let mut wrong_source = catalog.clone();
	wrong_source.live_history[0].metadata.epochs[0]
		.policy
		.as_mut()
		.ok_or_else(|| codec::invalid("fixture capture absent"))
		.assert_ok()
		.source = ChangefeedId::from_uuid(uuid::Uuid::from_u128(99).into()).assert_ok();
	assert!(wrong_source.encode().is_err());
	let mut wrong_retention = catalog.clone();
	wrong_retention.live_history[0].metadata.epochs[0]
		.policy
		.as_mut()
		.ok_or_else(|| codec::invalid("fixture capture absent"))
		.assert_ok()
		.retention_ns = 5678;
	assert!(wrong_retention.encode().is_err());
	let mut future = catalog.clone();
	future.live_history[0].metadata.last_stamp = 11;
	assert!(future.live_history[0].metadata.validate(1, 10).is_err());
	let mut closed = catalog;
	closed.live_queries[0].state = LiveState::Closed;
	closed.live_queries[0].published_at = 11;
	assert!(closed.encode().is_err(), "close must end durable capture");
	closed.live_history[0].metadata.epochs.push(LiveCaptureEpoch {
		started_at: 11,
		policy: None,
	});
	assert_eq!(DatabaseCatalog::decode(&closed.encode().assert_ok()).assert_ok(), closed);
}

/// Verifies fgdc changefeed origin bounds and reserved bytes are checked.
#[test]
fn fgdc_changefeed_origin_bounds_and_reserved_bytes_are_checked() {
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedId, ChangefeedSource, ChangefeedState},
		test_assertions::AssertResult as _,
	};
	let mut catalog = fixture().assert_ok();
	catalog.changefeeds.push(ChangefeedCatalogEntry {
		id: ChangefeedId::from_uuid(uuid::Uuid::from_u128(42).into()).assert_ok(),
		source: ChangefeedSource::Class(catalog.classes[0].id),
		name: "person".into(),
		config: ChangefeedConfig::from_nanoseconds(1_234)
			.assert_ok()
			.with_full(true)
			.with_include_original(true),
		state: ChangefeedState::Active,
		version: SchemaVersion::INITIAL,
		defined_at: 1,
		published_at: 2,
		last_stamp: u64::MAX,
		purged_through: 9,
		purged_time_ns: 0,
		retained_page: 0,
		retained_groups: 0,
		closed_at: None,
		closed_time_ns: None,
		segments: Vec::new(),
		epochs: vec![crate::changefeed::ChangefeedCaptureEpoch {
			revision: SchemaVersion::INITIAL,
			started_at: 2.into(),
			configuration: Some(
				ChangefeedConfig::from_nanoseconds(1_234)
					.assert_ok()
					.with_full(true)
					.with_include_original(true),
			),
		}],
	});
	let bytes = catalog.encode().assert_ok();
	assert_eq!(&bytes[88..104], catalog.database_uuid.as_bytes());
	assert_eq!(&bytes[104..120], &catalog.committed_at_ns.to_le_bytes());
	assert_eq!(&bytes[120..124], &1_u32.to_le_bytes());
	assert_eq!(&bytes[124..128], &[0; 4]);
	assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
	let descriptor = bytes.len() - 152 - 32 - "person".len();
	assert_eq!(&bytes[descriptor..descriptor + 16], uuid::Uuid::from_u128(42).as_bytes());
	assert_eq!(&bytes[descriptor + 16..descriptor + 20], &[0, 0, 3, 0]);
	for offset in
		[124, descriptor + 16, descriptor + 17, descriptor + 18, descriptor + 19, descriptor + 32]
	{
		let mut malformed = bytes.clone();
		malformed[offset] = u8::MAX;
		assert!(DatabaseCatalog::decode(&malformed).is_err(), "offset {offset}");
	}
	for end in descriptor..bytes.len() {
		assert!(DatabaseCatalog::decode(&bytes[..end]).is_err());
	}
	let mut huge_count = bytes.clone();
	huge_count[120..124].copy_from_slice(&u32::MAX.to_le_bytes());
	assert!(DatabaseCatalog::decode(&huge_count).is_err());
	let mut nil_origin = bytes;
	nil_origin[88..104].fill(0);
	assert!(DatabaseCatalog::decode(&nil_origin).is_err());
	catalog.changefeeds[0].state = ChangefeedState::Retired;
	catalog.changefeeds[0].closed_at = Some(2);
	catalog.changefeeds[0].closed_time_ns = Some(catalog.committed_at_ns);
	catalog.changefeeds[0].epochs[0].configuration = None;
	catalog.classes.clear();
	assert_eq!(DatabaseCatalog::decode(&catalog.encode().assert_ok()).assert_ok(), catalog);
	for source in [
		ChangefeedSource::Class(ClassId::from_storage(catalog.next_schema_id as u32).assert_ok()),
		ChangefeedSource::Graph(
			GraphId::new(catalog.next_graph_id)
				.ok_or_else(|| codec::invalid("fixture graph clock is zero"))
				.assert_ok(),
		),
	] {
		let mut future = catalog.clone();
		future.changefeeds[0].source = source;
		assert!(future.encode().is_err());
	}
	catalog.changefeeds[0].name = "invalid\0name".into();
	assert!(catalog.encode().is_err());
}

/// Logical descriptors and exact whole-commit positions survive catalog round
/// trips.
#[test]
fn fgdc_live_recipe_options_and_watermarks_are_checked() {
	use std::sync::Arc;

	use crate::{
		live::{
			LiveId,
			LiveOptions,
			LiveOverflowPolicy,
			LivePath,
			LiveQuery,
			LiveSince,
			LiveSource,
			LiveState,
			LiveVersion,
		},
		storage::live_descriptor::{self, LiveDescriptor},
		test_assertions::AssertResult as _,
	};
	let mut catalog = fixture().assert_ok();
	let query = Arc::new(LiveQuery::new(LiveSource::Class(catalog.classes[0].id)).with_omit(vec![
		LivePath::field("id").assert_ok(),
		LivePath::new(vec!["private".into(), "token".into()]).assert_ok(),
	]));
	let descriptor: Arc<[u8]> =
		live_descriptor::encode(&LiveDescriptor::Query(query.as_ref().clone()), 1024 * 1024)
			.assert_ok()
			.into();
	let stamp = (1_u64 << 53) + 1;
	let id = LiveId::from_uuid(uuid::Uuid::from_u128(42).into()).assert_ok();
	for since in [
		LiveSince::Now,
		LiveSince::Versionstamp(stamp.into()),
		LiveSince::Timestamp("2026-10-04T00:00:00Z[UTC]".parse().assert_ok()),
		LiveSince::Cursor(
			crate::live::LiveCursor::new(
				catalog.database_uuid.into(),
				id,
				LiveVersion::default(),
				stamp.into(),
			)
			.assert_ok(),
		),
	] {
		catalog.live_queries = vec![LiveCatalogEntry {
			graph_checkpoint: None,
			id,
			version: LiveVersion::default(),
			state: LiveState::Active,
			defined_at: stamp,
			starting_after: stamp,
			published_at: stamp,
			acknowledged: stamp,
			options: LiveOptions::default()
				.with_since(since)
				.with_overflow(LiveOverflowPolicy::Block),
			query: query.clone(),
			descriptor: descriptor.clone(),
			source_class: Some(catalog.classes[0].id),
			capture_source: crate::changefeed::ChangefeedSource::Class(catalog.classes[0].id),
			capture_scope_name: None,
			rhs_scope_anchors: Vec::new(),
			functions: Vec::new(),
			record_sources: Vec::new(),
			diagnostic: None,
		}];
		let bytes = catalog.encode().assert_ok();
		assert_eq!(&bytes[124..128], &1_u32.to_le_bytes());
		assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
		let mut bad_paths = bytes.clone();
		let descriptor_start = bytes.len() - descriptor.len();
		bad_paths[descriptor_start + 48..descriptor_start + 52]
			.copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(DatabaseCatalog::decode(&bad_paths).is_err());
		let start = bytes.len()
			- 160
			- descriptor.len()
			- match catalog.live_queries[0].options.since() {
				LiveSince::Now => 0,
				LiveSince::Versionstamp(_) => 8,
				LiveSince::Cursor(_) => 56,
				LiveSince::Timestamp(_) => {
					let encoded = &bytes[bytes.len() - descriptor.len()..];
					assert_eq!(encoded, descriptor.as_ref());
					continue;
				}
			};
		for offset in [
			start + 16,
			start + 56,
			start + 57,
			start + 58,
			start + 59,
			start + 108,
			start + 112,
			start + 120,
			start + 144,
			start + 145,
			start + 146,
			start + 147,
		] {
			let mut malformed = bytes.clone();
			malformed[offset] = if offset == start + 16 {
				0
			} else {
				255
			};
			assert!(DatabaseCatalog::decode(&malformed).is_err(), "offset {offset}");
		}
		let mut huge_count = bytes.clone();
		huge_count[124..128].copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(DatabaseCatalog::decode(&huge_count).is_err());
		let mut reserved_class = bytes.clone();
		reserved_class[start + 60..start + 64].copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(DatabaseCatalog::decode(&reserved_class).is_err());
		assert_eq!(&bytes[start + 144..start + 148], &[0, 0, 0, 0]);
		assert_eq!(
			&bytes[start + 148..start + 156],
			&u64::from(catalog.classes[0].id.get()).to_le_bytes(),
		);
		for raw in [0, u64::from(u32::MAX), u64::from(u32::MAX) + 1] {
			let mut malformed = bytes.clone();
			malformed[start + 148..start + 156].copy_from_slice(&raw.to_le_bytes());
			assert!(DatabaseCatalog::decode(&malformed).is_err(), "capture class {raw}");
		}
		let mut graph_owner = bytes.clone();
		graph_owner[start + 144] = 1;
		assert!(DatabaseCatalog::decode(&graph_owner).is_err(), "mismatched graph owner");
		let mut old_version = bytes.clone();
		old_version[4..6].copy_from_slice(&10_u16.to_le_bytes());
		assert!(DatabaseCatalog::decode(&old_version).is_err());
	}
	let entry = &mut catalog.live_queries[0];
	entry.options = entry.options.clone().with_since(LiveSince::Versionstamp((stamp - 1).into()));
	entry.starting_after = stamp - 1;
	entry.acknowledged = stamp - 1;
	let bytes = catalog.encode().assert_ok();
	assert_eq!(
		DatabaseCatalog::decode(&bytes).assert_ok(),
		catalog,
		"historical ACK precedes registration"
	);
	let mut malformed = catalog.clone();
	malformed.live_queries[0].acknowledged = stamp - 2;
	assert!(malformed.encode().is_err());
	let mut malformed = catalog.clone();
	malformed.live_queries[0].starting_after = stamp + 1;
	assert!(malformed.encode().is_err());
	let mut malformed = catalog.clone();
	malformed.live_queries[0].options = LiveOptions::default();
	assert!(malformed.encode().is_err(), "NOW cannot impersonate historical capture");
	catalog.live_queries.push(catalog.live_queries[0].clone());
	assert!(catalog.encode().is_err());
}

/// Qualified capture owners have canonical bytes independent of class policy.
#[test]
fn fgdc_live_capture_graph_owners_are_checked() {
	use crate::{
		changefeed::ChangefeedSource,
		live::{LiveId, LiveOptions, LiveQuery, LiveSource, LiveState, LiveVersion},
		storage::live_descriptor::{self, LiveDescriptor},
		test_assertions::AssertResult as _,
		values::Value,
	};
	let mut catalog = fixture().assert_ok();
	let class = catalog.classes[0].id;
	let name = catalog.classes[0].name.clone();
	let graph = &catalog.graphs[0];
	let reference =
		crate::graph::GraphRef::new(graph.id, graph.name.clone(), graph.kind, graph.storage_mode)
			.assert_ok();
	for source in [
		LiveSource::Graph(reference.clone()),
		LiveSource::GraphClass {
			graph: reference.clone(),
			class,
		},
		LiveSource::GraphRecord {
			graph: reference.clone(),
			record: format!("{name}:a").parse().assert_ok(),
		},
		LiveSource::GraphClassExpr {
			graph: Box::new(crate::live::LiveExpr::Literal(Value::from(reference.name().as_str()))),
			class,
		},
		LiveSource::GraphRecordExpr {
			graph: Box::new(crate::live::LiveExpr::Literal(Value::from(reference.name().as_str()))),
			record: format!("{name}:a").parse().assert_ok(),
		},
	] {
		let is_graph = matches!(source, LiveSource::Graph(_));
		let is_record =
			matches!(source, LiveSource::GraphRecord { .. } | LiveSource::GraphRecordExpr { .. });
		let scope_name = source.graph_expression().map(|_| reference.name().clone());
		let query = LiveQuery::new(source);
		let descriptor =
			live_descriptor::encode(&LiveDescriptor::Query(query.clone()), 1 << 20).assert_ok();
		catalog.live_queries = vec![LiveCatalogEntry {
			graph_checkpoint: None,
			id: LiveId::from_uuid(uuid::Uuid::from_u128(42).into()).assert_ok(),
			version: LiveVersion::default(),
			state: LiveState::Active,
			defined_at: 1,
			starting_after: 1,
			published_at: 1,
			acknowledged: 1,
			options: LiveOptions::default(),
			query: std::sync::Arc::new(query),
			descriptor: descriptor.into(),
			source_class: if is_graph {
				None
			} else {
				Some(class)
			},
			capture_source: ChangefeedSource::Graph(reference.id()),
			capture_scope_name: scope_name,
			rhs_scope_anchors: Vec::new(),
			functions: Vec::new(),
			record_sources: if is_record {
				vec![(name.clone(), class)]
			} else {
				Vec::new()
			},
			diagnostic: None,
		}];
		let bytes = catalog.encode().assert_ok();
		assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
		let entry = &catalog.live_queries[0];
		let captures = if is_record {
			8 + name.len()
		} else {
			0
		};
		let scope_name_len = entry.capture_scope_name.as_ref().map_or(0, |name| name.len());
		let start = bytes.len() - 160 - captures - entry.descriptor.len() - scope_name_len;
		assert_eq!(&bytes[start + 144..start + 148], &[1, 0, 0, 0]);
		assert_eq!(&bytes[start + 148..start + 156], &reference.id().get().to_le_bytes());
		assert_eq!(&bytes[start + 156..start + 160], &(scope_name_len as u32).to_le_bytes());
		let mut bad_length = bytes.clone();
		bad_length[start + 156..start + 160].copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(DatabaseCatalog::decode(&bad_length).is_err());
		if scope_name_len != 0 {
			for invalid in [0, 255] {
				let mut bad_name = bytes.clone();
				bad_name[bytes.len() - scope_name_len] = invalid;
				assert!(DatabaseCatalog::decode(&bad_name).is_err());
			}
			let mut oversized_name = catalog.clone();
			oversized_name.live_queries[0].capture_scope_name = Some("x".repeat(129).into());
			assert!(oversized_name.encode().is_err());
		} else {
			let mut unexpected_name = catalog.clone();
			unexpected_name.live_queries[0].capture_scope_name = Some("scope".into());
			assert!(unexpected_name.encode().is_err());
		}
		for raw in [0, catalog.next_graph_id, u64::MAX] {
			let mut malformed = bytes.clone();
			malformed[start + 148..start + 156].copy_from_slice(&raw.to_le_bytes());
			assert!(DatabaseCatalog::decode(&malformed).is_err(), "capture graph {raw}");
		}
		let mut wrong_source = catalog.clone();
		wrong_source.live_queries[0].capture_source = ChangefeedSource::Class(class);
		assert!(wrong_source.encode().is_err(), "graph query cannot inherit class capture");
	}
}

/// Captured owners, function versions and fault images survive exact reopen.
#[test]
fn fgdc_live_query_captures_and_faults_reject_corrupted_tails() {
	use std::sync::Arc;

	use crate::{
		live::{
			LiveExpr,
			LiveId,
			LiveOptions,
			LiveProjection,
			LiveQuery,
			LiveSource,
			LiveState,
			LiveVersion,
		},
		storage::live_descriptor::{self, LiveDescriptor},
		test_assertions::AssertResult as _,
		values::Value,
	};
	let mut catalog = fixture().assert_ok();
	let source = catalog.classes[0].clone();
	let function = &catalog.functions[0];
	let query = Arc::new(
		LiveQuery::new(LiveSource::Record(format!("{}:a", source.name).parse().assert_ok()))
			.with_projection(LiveProjection::Value(LiveExpr::Call {
				name: "fn::increment".into(),
				arguments: vec![LiveExpr::Literal(Value::from(1_i64))],
			})),
	);
	let descriptor: Arc<[u8]> =
		live_descriptor::encode(&LiveDescriptor::Query(query.as_ref().clone()), 1024 * 1024)
			.assert_ok()
			.into();
	catalog.live_queries.push(LiveCatalogEntry {
		graph_checkpoint: None,
		id: LiveId::from_uuid(uuid::Uuid::from_u128(42).into()).assert_ok(),
		version: LiveVersion::default(),
		state: LiveState::Active,
		defined_at: 1,
		starting_after: 1,
		published_at: 2,
		acknowledged: 1,
		options: LiveOptions::default(),
		query,
		descriptor: descriptor.clone(),
		source_class: Some(source.id),
		capture_source: crate::changefeed::ChangefeedSource::Class(source.id),
		capture_scope_name: None,
		rhs_scope_anchors: Vec::new(),
		functions: vec![(function.id, function.version)],
		record_sources: vec![(source.name.clone(), source.id)],
		diagnostic: None,
	});
	let bytes = catalog.encode().assert_ok();
	assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
	let start = bytes.len() - 160 - 8 - 8 - source.name.len() - descriptor.len();
	for offset in [
		start + 128,
		start + 132,
		start + 136,
		start + 140,
		start + 160,
		start + 160,
		start + 164,
		start + 168,
		start + 172,
	] {
		let mut malformed = bytes.clone();
		malformed[offset..offset + 4].fill(255);
		assert!(DatabaseCatalog::decode(&malformed).is_err(), "capture offset {offset}");
	}
	for length in start..bytes.len() {
		assert!(DatabaseCatalog::decode(&bytes[..length]).is_err());
	}
	let mut incompatible = catalog.clone();
	incompatible.live_queries[0].functions[0].1 =
		SchemaVersion::new(2).ok_or_else(|| codec::invalid("fixture version")).assert_ok();
	assert!(incompatible.encode().is_err());
	let mut incompatible = catalog.clone();
	incompatible.live_queries[0].record_sources[0].0 = "other".into();
	assert!(incompatible.encode().is_err());
	catalog.live_queries[0].state = LiveState::Paused;
	catalog.live_queries[0].diagnostic = Some(("FG-G1F09".into(), "evaluation failed".into()));
	let bytes = catalog.encode().assert_ok();
	assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
	let tail = bytes.len() - "FG-G1F09".len() - "evaluation failed".len();
	for offset in [tail, tail + 8] {
		let mut malformed = bytes.clone();
		malformed[offset] = 255;
		assert!(DatabaseCatalog::decode(&malformed).is_err());
	}
	catalog.live_queries[0].diagnostic = None;
	assert!(catalog.encode().is_err());
	// The anchor tail follows the descriptor and precedes the diagnostic.
	let entry = &mut catalog.live_queries[0];
	entry.diagnostic = Some(("FG-G1F09".into(), "evaluation failed".into()));
	entry.query =
		Arc::new(entry.query.as_ref().clone().with_joins(vec![crate::live::LiveJoin::new(
			LiveSource::GraphClassExpr {
				class: source.id,
				graph: Box::new(LiveExpr::Literal("scope".into())),
			},
			"r",
			crate::live::LiveJoinKind::Cross,
			None,
		).assert_ok()]));
	entry.descriptor =
		live_descriptor::encode(&LiveDescriptor::Query(entry.query.as_ref().clone()), 1024 * 1024)
			.assert_ok()
			.into();
	entry.rhs_scope_anchors = vec![crate::datastore::LiveRhsScopeAnchor {
		source: 1,
		graph: catalog.graphs[0].id,
		name: "scope".into(),
	}];
	let entry_length = 160
		+ 8
		+ 8
		+ source.name.len()
		+ entry.descriptor.len()
		+ 29
		+ "FG-G1F09".len()
		+ "evaluation failed".len();
	let bytes = catalog.encode().assert_ok();
	assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
	let start = bytes.len() - entry_length;
	assert_eq!(bytes[start + 59], 1);
	for flags in [0, 2, 255] {
		let mut malformed = bytes.clone();
		malformed[start + 59] = flags;
		assert!(DatabaseCatalog::decode(&malformed).is_err());
	}
}

/// Writer admission uses the aggregate recovery cap for many compact recipes.
#[test]
fn fgdc_live_writer_rejects_catalog_exceeding_recovery_scratch() {
	use std::sync::Arc;

	use crate::{
		live::{
			LiveExpr,
			LiveField,
			LiveId,
			LiveOptions,
			LiveProjection,
			LiveQuery,
			LiveSource,
			LiveState,
			LiveVersion,
		},
		storage::live_descriptor::{self, LiveDescriptor},
		test_assertions::AssertResult as _,
		values::Value,
	};
	let mut catalog = fixture().assert_ok();
	let class = catalog.classes[0].id;
	let query =
		Arc::new(LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Fields(
			(0..30_000).map(|_| LiveField::new(LiveExpr::Literal(Value::Null))).collect(),
		)));
	let descriptor: Arc<[u8]> =
		live_descriptor::encode(&LiveDescriptor::Query(query.as_ref().clone()), 256 * 1024 * 1024)
			.assert_ok()
			.into();
	assert!(descriptor.len() < 1024 * 1024);
	for ordinal in 1..=20 {
		catalog.live_queries.push(LiveCatalogEntry {
			graph_checkpoint: None,
			id: LiveId::from_uuid(uuid::Uuid::from_u128(ordinal).into()).assert_ok(),
			version: LiveVersion::default(),
			state: LiveState::Active,
			defined_at: 1,
			starting_after: 1,
			published_at: 1,
			acknowledged: 1,
			options: LiveOptions::default(),
			query: query.clone(),
			descriptor: descriptor.clone(),
			source_class: Some(class),
			capture_source: crate::changefeed::ChangefeedSource::Class(class),
			capture_scope_name: None,
			rhs_scope_anchors: Vec::new(),
			functions: Vec::new(),
			record_sources: Vec::new(),
			diagnostic: None,
		});
		if ordinal == 4 {
			let bytes = catalog.encode().assert_ok();
			assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
		}
	}
	assert!(catalog.encode().is_err());
}

/// Trusted segment bounds remain decodable independently of file contents.
#[test]
fn fgdc_changefeed_segment_descriptors_check_ids_bounds_lengths_and_reserved() {
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedId, ChangefeedSource, ChangefeedState},
		storage::{HistoryOwner, HistorySegment, changefeed::SegmentHeader},
		test_assertions::AssertResult as _,
	};
	let mut catalog = fixture().assert_ok();
	let id = ChangefeedId::from_uuid(uuid::Uuid::from_u128(42).into()).assert_ok();
	let config = ChangefeedConfig::from_nanoseconds(1_234).assert_ok();
	let segment = HistorySegment {
		file: DataFileId::new(99, DataFileKind::Changefeed)
			.ok_or_else(|| codec::invalid("fixture file identity invalid"))
			.assert_ok(),
		header: SegmentHeader {
			database: catalog.database_uuid,
			owner: HistoryOwner::Changefeed(id),
			segment: uuid::Uuid::from_u128(51),
			pages: 2,
			commits: 1,
			first_stamp: 10,
			last_stamp: 10,
			first_time: 0,
			last_time: 0,
		},
	};
	catalog.changefeeds.push(ChangefeedCatalogEntry {
		id,
		source: ChangefeedSource::Class(catalog.classes[0].id),
		name: "person".into(),
		config,
		state: ChangefeedState::Active,
		version: SchemaVersion::INITIAL,
		defined_at: 1,
		published_at: 1,
		last_stamp: 10,
		purged_through: 0,
		purged_time_ns: 0,
		retained_page: 0,
		retained_groups: 0,
		closed_at: None,
		closed_time_ns: None,
		epochs: vec![crate::changefeed::ChangefeedCaptureEpoch {
			revision: SchemaVersion::INITIAL,
			started_at: 1.into(),
			configuration: Some(config),
		}],
		segments: vec![segment],
	});
	let bytes = catalog.encode().assert_ok();
	assert_eq!(DatabaseCatalog::decode(&bytes).assert_ok(), catalog);
	let start = bytes.len() - 128;
	assert_eq!(&bytes[start..start + 8], &segment.file.get().to_le_bytes());
	assert_eq!(&bytes[start + 56..start + 64], &2_u64.to_le_bytes());
	assert_eq!(&bytes[start + 120..], &[0; 8]);
	for offset in [start + 120, start + 127] {
		let mut malformed = bytes.clone();
		malformed[offset] = u8::MAX;
		assert!(DatabaseCatalog::decode(&malformed).is_err(), "offset {offset}");
	}
	for (offset, value) in [(start + 56, 1_u64), (start + 64, 0_u64), (start + 64, 2_u64)] {
		let mut malformed = bytes.clone();
		malformed[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
		assert!(DatabaseCatalog::decode(&malformed).is_err(), "offset {offset}, value {value}");
	}
	let mut count = bytes.clone();
	count[start - 4..start].copy_from_slice(&u32::MAX.to_le_bytes());
	assert!(DatabaseCatalog::decode(&count).is_err());
	for end in start..bytes.len() {
		assert!(DatabaseCatalog::decode(&bytes[..end]).is_err());
	}
	let mut bad = catalog.clone();
	bad.changefeeds[0].segments[0].header.owner = HistoryOwner::Changefeed(
		ChangefeedId::from_uuid(uuid::Uuid::from_u128(43).into()).assert_ok(),
	);
	assert!(bad.encode().is_err());
	let mut bad = catalog.clone();
	bad.changefeeds[0].segments[0].header.last_stamp = 11;
	assert!(bad.encode().is_err());
	let mut bad = catalog.clone();
	bad.changefeeds[0].segments[0].header.owner =
		HistoryOwner::Live(crate::live::LiveId::from_uuid(id.as_uuid()).assert_ok());
	assert!(bad.encode().is_err(), "LIVE history cannot enter the user-visible source catalog");
	let mut bad = catalog.clone();
	bad.changefeeds[0].segments.push(segment);
	assert!(bad.encode().is_err());
}

/// Verifies fgdc v12 round trips fulltext hnsw and disk ann payloads.
#[test]
fn fgdc_v12_round_trips_fulltext_hnsw_and_disk_ann_payloads() {
	use crate::test_assertions::AssertResult as _;

	let mut catalog = fixture().assert_ok();
	let class = &mut catalog.classes[0];
	class.fields.extend([
		ClassFieldCatalogEntry {
			id: SchemaId::new(9).ok_or_else(|| codec::invalid("invalid title field")).assert_ok(),
			version: SchemaVersion::INITIAL,
			name: "title".into(),
			value_type: Type::String,
			property_key: PropertyKeyId::new(1),
		},
		ClassFieldCatalogEntry {
			id: SchemaId::new(10).ok_or_else(|| codec::invalid("invalid vector field")).assert_ok(),
			version: SchemaVersion::INITIAL,
			name: "embedding".into(),
			value_type: Type::Vector(VectorKind::F32),
			property_key: PropertyKeyId::new(2),
		},
		ClassFieldCatalogEntry {
			id: SchemaId::new(11)
				.ok_or_else(|| codec::invalid("invalid compact field"))
				.assert_ok(),
			version: SchemaVersion::INITIAL,
			name: "compact".into(),
			value_type: Type::Vector(VectorKind::F16),
			property_key: PropertyKeyId::new(3),
		},
	]);
	let binding = |instance| -> FerrosGrynnResult<PhysicalIndexBinding> {
		Ok(PhysicalIndexBinding {
			file_id: DataFileId::new(instance, DataFileKind::Index)
				.ok_or_else(|| codec::invalid("invalid specialized fixture file"))?,
			root: crate::storage::page::PageNo::new(0),
		})
	};
	class.indexes.extend([
		ClassIndexCatalogEntry {
			id: IndexId::new(12)
				.ok_or_else(|| codec::invalid("invalid Fulltext index"))
				.assert_ok(),
			version: SchemaVersion::INITIAL,
			name: "title_search".into(),
			kind: ClassIndexKind::Fulltext,
			state: ClassIndexState::Ready,
			maintenance: crate::datastore::IndexMaintenanceMode::Immediate,
			build_revision: 3,
			scope: IndexScope::Base,
			fields: vec![PropertyKeyId::new(1)],
			options: ClassIndexOptions::Fulltext(
				FulltextIndexOptions::new(
					AnalyzerId::new(8)
						.ok_or_else(|| codec::invalid("invalid analyzer"))
						.assert_ok(),
					SchemaVersion::INITIAL,
					1.4,
					0.6,
					true,
				)
				.assert_ok(),
			),
			physical: Some(binding(6).assert_ok()),
			delta: Some(binding(7).assert_ok()),
		},
		ClassIndexCatalogEntry {
			id: IndexId::new(13).ok_or_else(|| codec::invalid("invalid HNSW index")).assert_ok(),
			version: SchemaVersion::INITIAL,
			name: "embedding_hnsw".into(),
			kind: ClassIndexKind::Hnsw,
			state: ClassIndexState::Ready,
			maintenance: crate::datastore::IndexMaintenanceMode::Immediate,
			build_revision: 3,
			scope: IndexScope::Base,
			fields: vec![PropertyKeyId::new(2)],
			options: ClassIndexOptions::Hnsw(
				HnswIndexOptions::new(
					VectorKind::F32,
					NonZeroU16::new(128)
						.ok_or_else(|| codec::invalid("invalid dimension"))
						.assert_ok(),
					VectorDistance::Cosine,
					NonZeroU16::new(16).ok_or_else(|| codec::invalid("invalid M")).assert_ok(),
					NonZeroU16::new(180).ok_or_else(|| codec::invalid("invalid EFC")).assert_ok(),
				)
				.assert_ok(),
			),
			physical: Some(binding(8).assert_ok()),
			delta: Some(binding(9).assert_ok()),
		},
		ClassIndexCatalogEntry {
			id: IndexId::new(14).ok_or_else(|| codec::invalid("invalid DiskANN index")).assert_ok(),
			version: SchemaVersion::INITIAL,
			name: "compact_disk".into(),
			kind: ClassIndexKind::DiskAnn,
			state: ClassIndexState::Ready,
			maintenance: crate::datastore::IndexMaintenanceMode::Deferred,
			build_revision: 3,
			scope: IndexScope::Base,
			fields: vec![PropertyKeyId::new(3)],
			options: ClassIndexOptions::DiskAnn(
				DiskAnnIndexOptions::new(
					VectorKind::F16,
					NonZeroU16::new(768)
						.ok_or_else(|| codec::invalid("invalid dimension"))
						.assert_ok(),
					VectorDistance::CosineNormalized,
					NonZeroU16::new(48).ok_or_else(|| codec::invalid("invalid degree")).assert_ok(),
					NonZeroU16::new(120)
						.ok_or_else(|| codec::invalid("invalid build list"))
						.assert_ok(),
					1.3,
					true,
				)
				.assert_ok(),
			),
			physical: Some(binding(10).assert_ok()),
			delta: Some(binding(11).assert_ok()),
		},
	]);
	catalog.next_schema_id = 15;
	let decoded = DatabaseCatalog::decode(&catalog.encode().assert_ok()).assert_ok();
	assert_eq!(decoded, catalog);
}

/// Verifies fgdc rejects old version truncation and duplicate schema identity.
#[test]
fn fgdc_rejects_old_version_truncation_and_duplicate_schema_identity() {
	use crate::test_assertions::AssertResult as _;

	let catalog = fixture().assert_ok();
	let mut old = catalog.encode().assert_ok();
	old[4..6].copy_from_slice(&9_u16.to_le_bytes());
	assert!(DatabaseCatalog::decode(&old).is_err());

	let mut truncated = catalog.encode().assert_ok();
	truncated.pop();
	assert!(DatabaseCatalog::decode(&truncated).is_err());

	let mut duplicate = catalog;
	duplicate.parameters[0].id = duplicate.classes[0].fields[0].id;
	assert!(duplicate.encode().is_err());
}

/// Verifies fgdc rejects duplicate or unsorted graph properties.
#[test]
fn fgdc_rejects_duplicate_or_unsorted_graph_properties() {
	use crate::test_assertions::AssertResult as _;

	let mut catalog = fixture().assert_ok();
	catalog.graphs[0].properties.push(GraphPropertyCatalogEntry {
		key: PropertyKeyId::new(0),
		value: vec![1],
	});
	assert!(catalog.encode().is_err());
}

/// Verifies fgdc v12 allows one internal child through multiple owner scopes.
#[test]
fn fgdc_v12_allows_one_internal_child_through_multiple_owner_scopes() {
	use crate::test_assertions::AssertResult as _;

	let catalog = fixture_with_shared_child().assert_ok();
	let decoded = DatabaseCatalog::decode(&catalog.encode().assert_ok()).assert_ok();
	let root = &decoded.graphs[0];
	assert_eq!(root.arena.graph_by_slot()[&1], root.arena.graph_by_slot()[&2]);
	assert_ne!(root.arena.scope_by_slot()[&1], root.arena.scope_by_slot()[&2]);
	assert_eq!(decoded.graphs[1].owner_root, Some(root.id));
}

/// Verifies fgdc v12 rejects duplicate scope and noncanonical internal name.
#[test]
fn fgdc_v12_rejects_duplicate_scope_and_noncanonical_internal_name() {
	use crate::test_assertions::AssertResult as _;

	let mut duplicate_scope = fixture_with_shared_child().assert_ok();
	let first_scope = duplicate_scope.graphs[0].arena.scope_by_slot()[&1].clone();
	edit_arena(&mut duplicate_scope.graphs[0].arena, |_graphs, scopes| {
		scopes.insert(2, first_scope);
	});
	assert!(duplicate_scope.encode().is_err());

	let mut invalid_name = fixture_with_shared_child().assert_ok();
	invalid_name.graphs[1].name = "$fg$build_wrong".into();
	assert!(invalid_name.encode().is_err());
}

/// Verifies fgdc v12 rejects unknown constraint bits and invalid roots.
#[test]
fn fgdc_v12_rejects_unknown_constraint_bits_and_invalid_roots() {
	use crate::test_assertions::AssertResult as _;

	let catalog = fixture().assert_ok();
	let mut unknown_bits = catalog.encode().assert_ok();
	let graph_header = first_graph_header(&unknown_bits, &catalog.graphs[0]).assert_ok();
	unknown_bits[graph_header + 30..graph_header + 32].copy_from_slice(&u16::MAX.to_le_bytes());
	assert!(DatabaseCatalog::decode(&unknown_bits).is_err());

	let mut wrong_family = fixture().assert_ok();
	wrong_family.graphs[0].root = Some(vec![1]);
	assert!(wrong_family.encode().is_err());

	let mut missing_root = fixture().assert_ok();
	missing_root.graphs[0].kind = GraphKind::RootedTree;
	assert!(missing_root.encode().is_ok());
}

/// Verifies fgdc v12 rejects invalid internal ownership and scope ancestry.
#[test]
fn fgdc_v12_rejects_invalid_internal_ownership_and_scope_ancestry() {
	use crate::test_assertions::AssertResult as _;

	let mut missing_owner = fixture_with_shared_child().assert_ok();
	missing_owner.graphs[1].owner_root = GraphId::new(99);
	assert!(missing_owner.encode().is_err());

	let mut orphan = fixture_with_shared_child().assert_ok();
	edit_arena(&mut orphan.graphs[0].arena, |graphs, scopes| {
		for slot in [1, 2] {
			graphs.remove(&slot);
			scopes.remove(&slot);
		}
	});
	assert!(orphan.encode().is_err());

	let mut missing_parent = fixture_with_shared_child().assert_ok();
	let owner = missing_parent.graphs[0].arena.scope_by_slot()[&1][0];
	let nested = AtomId::from_parts(AtomKind::Node, 2, 0).assert_ok();
	edit_arena(&mut missing_parent.graphs[0].arena, |_graphs, scopes| {
		scopes.insert(2, vec![owner, nested].into_boxed_slice());
	});
	assert!(missing_parent.encode().is_err());

	let mut cycle = fixture_with_shared_child().assert_ok();
	let first = cycle.graphs[0].arena.scope_by_slot()[&1][0];
	let second = cycle.graphs[0].arena.scope_by_slot()[&2][0];
	edit_arena(&mut cycle.graphs[0].arena, |_graphs, scopes| {
		scopes.insert(2, vec![first, second].into_boxed_slice());
	});
	assert!(cycle.encode().is_err());
}

/// Returns first graph header for this module.
fn first_graph_header(bytes: &[u8], graph: &GraphCatalogEntry) -> FerrosGrynnResult<usize> {
	let mut prefix = Vec::new();
	prefix.extend_from_slice(&graph.id.get().to_le_bytes());
	prefix.extend_from_slice(&graph.version.get().to_le_bytes());
	prefix.extend_from_slice(&graph.published_at.to_le_bytes());
	prefix.extend_from_slice(&graph.generation.to_le_bytes());
	bytes
		.windows(prefix.len())
		.position(|window| window == prefix)
		.ok_or_else(|| codec::invalid("fixture graph header is absent"))
}

/// Builds the fixture fixture used by nearby scenarios.
fn fixture() -> FerrosGrynnResult<DatabaseCatalog> {
	let graph = GraphId::new(7).ok_or_else(|| codec::invalid("invalid fixture graph"))?;
	let index = |instance| -> FerrosGrynnResult<PhysicalIndexBinding> {
		Ok(PhysicalIndexBinding {
			file_id: DataFileId::new(instance, DataFileKind::Index)
				.ok_or_else(|| codec::invalid("invalid fixture file"))?,
			root: crate::storage::page::PageNo::new(0),
		})
	};
	Ok(DatabaseCatalog {
		bridges: None,
		event_outbox_segments: Vec::new(),
		events: Vec::new(),
		database_uuid: uuid::Uuid::from_u128(1),
		committed_at_ns: 123_456_789,
		changefeeds: Vec::new(),
		live_queries: Vec::new(),
		live_bindings: Vec::new(),
		live_history: Vec::new(),
		next_graph_id: 8,
		next_schema_id: 9,
		time_zones: vec!["Europe/London".into()].into(),
		atom_store: AtomStoreCatalogBinding {
			generation: 11,
			primary: index(1)?,
			reverse_membership: index(2)?,
		},
		classes: vec![ClassCatalogEntry {
			id: ClassId::new(1).ok_or_else(|| codec::invalid("invalid fixture class"))?,
			version: SchemaVersion::INITIAL,
			published_at: 1,
			name: "person".into(),
			kind: AtomKind::Node,
			mode: ClassSchemaMode::Schemafull,
			fields: vec![ClassFieldCatalogEntry {
				id: SchemaId::new(2).ok_or_else(|| codec::invalid("invalid fixture field"))?,
				version: SchemaVersion::INITIAL,
				name: "home".into(),
				value_type: Type::Option(Box::new(Type::GraphRef(GraphKind::Graph))),
				property_key: PropertyKeyId::new(0),
			}],
			indexes: vec![ClassIndexCatalogEntry {
				id: IndexId::new(3).ok_or_else(|| codec::invalid("invalid fixture index"))?,
				version: SchemaVersion::INITIAL,
				name: "by_home".into(),
				kind: ClassIndexKind::Unique,
				state: ClassIndexState::Ready,
				maintenance: crate::datastore::IndexMaintenanceMode::Immediate,
				build_revision: 1,
				scope: IndexScope::Base,
				fields: vec![PropertyKeyId::new(0)],
				options: crate::datastore::ClassIndexOptions::Scalar,
				physical: Some(index(3)?),
				delta: None,
			}],
		}],
		graphs: vec![GraphCatalogEntry {
			tensor_revision: 0,
			id: graph,
			version: SchemaVersion::INITIAL,
			published_at: 1,
			generation: 11,
			name: "social".into(),
			kind: GraphKind::Graph,
			storage_mode: GraphStorageMode::DiskBacked,
			topology_constraints: GraphTopologyConstraints::empty(),
			label_index: (
				IndexId::new(4).ok_or_else(|| codec::invalid("invalid label index"))?,
				SchemaVersion::INITIAL,
			),
			adjacency_index: (
				IndexId::new(5).ok_or_else(|| codec::invalid("invalid adjacency index"))?,
				SchemaVersion::INITIAL,
			),
			edge_endpoint_index: None,
			edge_endpoint: None,
			membership: index(4)?,
			indexes: Vec::new(),
			index_bindings: BTreeMap::new(),
			owner_root: None,
			root: None,
			properties: vec![GraphPropertyCatalogEntry {
				key: PropertyKeyId::new(0),
				value: vec![1],
			}],
			arena: GraphArenaBindings::new(
				BTreeMap::from([(0, graph)]),
				BTreeMap::from([(0, Box::default())]),
			),
		}],
		parameters: vec![ParameterCatalogEntry {
			id: SchemaId::new(6).ok_or_else(|| codec::invalid("invalid fixture parameter"))?,
			version: SchemaVersion::INITIAL,
			published_at: 1,
			name: "answer".into(),
			value: vec![0],
			comment: Some("durable parameter".into()),
		}],
		functions: vec![FunctionCatalogEntry {
			id: FunctionId::new(7).ok_or_else(|| codec::invalid("invalid fixture function"))?,
			version: SchemaVersion::INITIAL,
			published_at: 1,
			namespace: "fn".into(),
			name: "increment".into(),
			parameters: vec![FunctionParameterCatalogEntry {
				name: "value".into(),
				value_type: Type::Int,
			}],
			result: Type::Int,
			effect: FunctionEffect::Pure,
			cost: false,
			max_depth: None,
			body: "{ RETURN $value + 1; }".into(),
			comment: Some("fixture function".into()),
		}],
		analyzers: vec![AnalyzerCatalogEntry {
			id: AnalyzerId::new(8).ok_or_else(|| codec::invalid("invalid fixture analyzer"))?,
			version: SchemaVersion::INITIAL,
			published_at: 1,
			name: "english".into(),
			tokenizers: BTreeSet::from([AnalyzerTokenizer::Blank, AnalyzerTokenizer::Punct]),
			filters: vec![
				AnalyzerFilterCatalogEntry::Lowercase,
				AnalyzerFilterCatalogEntry::Mapper(CanonicalCodec::encode(
					StorageValue::Object(vec![(
						PropertyKeyId::new(0),
						StorageValue::String("drive".into()),
					)])
					.as_ref(),
				)?),
			],
			function: None,
			comment: Some("fixture analyzer".into()),
		}],
	})
}

/// Verifies fgdc tensor bindings require the dedicated family without weakening
/// indexes.
#[test]
fn fgdc_tensor_bindings_require_the_dedicated_family_without_weakening_indexes() {
	use crate::{
		datastore::{GraphIndex, TensorCacheFormat},
		graph::tensor::TensorScope,
		test_assertions::AssertResult as _,
	};
	let mut catalog = fixture().assert_ok();
	let graph = &mut catalog.graphs[0];
	let mut definition =
		GraphIndex::new_tensor("tensor", TensorScope::Local, TensorCacheFormat::Csr, 1 << 20)
			.assert_ok();
	let id = IndexId::new(9).ok_or_else(|| codec::invalid("invalid fixture index")).assert_ok();
	definition.id = Some(id);
	definition.graph = Some(graph.id);
	definition.state = ClassIndexState::Ready;
	definition.build_revision = 11;
	graph.indexes.push(definition);
	let binding = PhysicalIndexBinding {
		file_id: DataFileId::new(20, DataFileKind::TensorCache)
			.ok_or_else(|| codec::invalid("invalid tensor file"))
			.assert_ok(),
		root: crate::storage::page::PageNo::new(0),
	};
	graph.index_bindings.insert(id, binding);
	catalog.next_schema_id = 10;
	let bytes = catalog.encode().assert_ok();
	let decoded = DatabaseCatalog::decode(&bytes).assert_ok();
	assert_eq!(decoded.graphs[0].index_bindings[&id], binding);
	catalog.graphs[0]
		.index_bindings
		.get_mut(&id)
		.ok_or_else(|| codec::invalid("missing tensor binding"))
		.assert_ok()
		.file_id = DataFileId::new(20, DataFileKind::Index)
		.ok_or_else(|| codec::invalid("invalid ordinary file"))
		.assert_ok();
	assert!(catalog.encode().is_err());
	catalog.graphs[0].index_bindings.insert(id, binding);
	catalog.atom_store.primary = binding;
	assert!(catalog.encode().is_err());
}

/// Returns fixture with shared child for this module.
fn fixture_with_shared_child() -> FerrosGrynnResult<DatabaseCatalog> {
	let mut catalog = fixture()?;
	let root = catalog.graphs[0].id;
	let child = GraphId::new(8).ok_or_else(|| codec::invalid("invalid child graph"))?;
	let owner_a = AtomId::from_parts(AtomKind::Node, 0, 0)?;
	let owner_b = AtomId::from_parts(AtomKind::Node, 1, 0)?;
	catalog.next_graph_id = 9;
	catalog.next_schema_id = 11;
	edit_arena(&mut catalog.graphs[0].arena, |graphs, scopes| {
		graphs.extend([(1, child), (2, child)]);
		scopes
			.extend([(1, vec![owner_a].into_boxed_slice()), (2, vec![owner_b].into_boxed_slice())]);
	});
	catalog.graphs.push(GraphCatalogEntry {
		tensor_revision: 0,
		id: child,
		version: SchemaVersion::INITIAL,
		published_at: 1,
		generation: 12,
		name: format!("$fg$build_{:016x}_{:016x}", root.get(), child.get()).into(),
		kind: GraphKind::Graph,
		storage_mode: GraphStorageMode::DiskBacked,
		topology_constraints: GraphTopologyConstraints::empty(),
		label_index: (
			IndexId::new(9).ok_or_else(|| codec::invalid("invalid label index"))?,
			SchemaVersion::INITIAL,
		),
		adjacency_index: (
			IndexId::new(10).ok_or_else(|| codec::invalid("invalid adjacency index"))?,
			SchemaVersion::INITIAL,
		),
		edge_endpoint_index: None,
		edge_endpoint: None,
		membership: PhysicalIndexBinding {
			file_id: DataFileId::new(5, DataFileKind::Index)
				.ok_or_else(|| codec::invalid("invalid child membership"))?,
			root: crate::storage::page::PageNo::new(0),
		},
		owner_root: Some(root),
		indexes: Vec::new(),
		index_bindings: BTreeMap::new(),
		root: None,
		properties: Vec::new(),
		arena: GraphArenaBindings::new(
			BTreeMap::from([(0, child)]),
			BTreeMap::from([(0, Box::default())]),
		),
	});
	Ok(catalog)
}

/// Reconstructs canonical input for corruption tests without a production
/// mutator.
fn edit_arena(
	arena: &mut GraphArenaBindings,
	edit: impl FnOnce(&mut BTreeMap<u32, GraphId>, &mut BTreeMap<u32, Box<[AtomId]>>),
) {
	let mut graphs = arena.graph_by_slot().clone();
	let mut scopes = arena.scope_by_slot().clone();
	edit(&mut graphs, &mut scopes);
	*arena = GraphArenaBindings::new(graphs, scopes);
}
