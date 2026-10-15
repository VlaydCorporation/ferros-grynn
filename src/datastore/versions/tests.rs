//! Independent revision-map and snapshot ownership regressions.

use super::*;
use crate::{
	changefeed::ChangefeedSource,
	datastore::{AnalyzerId, ClassId, EventId, FunctionId, GlobalAtomId, JobId, SchemaId},
	graph::{AtomId, AtomKind, GraphId},
	live::{LiveBindingId, LiveId},
};

/// Covers every current FCW/SSI namespace with valid fixed identities.
fn keys() -> FerrosGrynnResult<Vec<ConflictKey>> {
	let class = ClassId::from_storage(1)?;
	let schema = SchemaId::from_storage(1)?;
	let graph = GraphId::new(1).ok_or_else(|| FerrosGrynnError::invalid_argument("test graph"))?;
	let atom = AtomId::from_raw(1);
	let global = GlobalAtomId::from_storage(1)?;
	Ok(vec![
		ConflictKey::PropertyDictionary,
		ConflictKey::GraphDefinition(graph),
		ConflictKey::GraphCatalogRange,
		ConflictKey::SchemaClass(class),
		ConflictKey::ClassCatalogRange,
		ConflictKey::SchemaParameter(schema),
		ConflictKey::ParameterCatalogRange,
		ConflictKey::SchemaFunction(FunctionId::from_storage(1)?),
		ConflictKey::FunctionCatalogRange,
		ConflictKey::SchemaAnalyzer(AnalyzerId::from_storage(1)?),
		ConflictKey::AnalyzerCatalogRange,
		ConflictKey::SchemaEvent(EventId::from_storage(1)?),
		ConflictKey::EventCatalogRange,
		ConflictKey::EventInvocation(JobId::from_uuid(uuid::Uuid::from_u128(1).into())),
		ConflictKey::EventOutboxRange,
		ConflictKey::BaseAtom(global),
		ConflictKey::BaseClassRange(class),
		ConflictKey::GraphMembership(graph, global),
		ConflictKey::GraphOverlay(graph, global),
		ConflictKey::GraphIncidence(graph, atom),
		ConflictKey::GraphClassRange(graph, class),
		ConflictKey::GraphKindRange(graph, AtomKind::Node),
		ConflictKey::ChangefeedDefinition(ChangefeedSource::Class(class)),
		ConflictKey::ChangefeedCatalogRange,
		ConflictKey::LiveDefinition("00000000-0000-0000-0000-000000000001".parse::<LiveId>()?),
		ConflictKey::LiveBinding("00000000-0000-0000-0000-000000000002".parse::<LiveBindingId>()?),
		ConflictKey::LiveCatalogRange,
	])
}

/// Interleaved revisions match an independent single-map oracle exactly.
#[test]
fn versions_match_all_conflict_namespaces_and_preserve_snapshots() {
	use crate::test_assertions::AssertResult as _;

	let mut versions = Arc::new(VersionStore::default());
	let mut oracle = BTreeMap::new();
	let keys = keys().assert_ok();
	for revision in 1..=3 {
		let previous = Arc::clone(&versions);
		let previous_oracle = oracle.clone();
		for key in keys.iter().rev() {
			Arc::make_mut(&mut versions).insert(*key, revision);
			oracle.insert(*key, revision);
		}
		for key in &keys {
			assert_eq!(versions.get(key), oracle.get(key));
			assert_eq!(previous.get(key), previous_oracle.get(key));
		}
		let combined = versions
			.primary
			.iter()
			.chain(versions.controls.live.iter())
			.chain(versions.controls.events.iter())
			.collect::<BTreeMap<_, _>>();
		assert_eq!(combined.into_iter().collect::<Vec<_>>(), oracle.iter().collect::<Vec<_>>());
		assert_eq!(versions.controls.live.len(), 3);
		assert_eq!(versions.controls.events.len(), 2);
		assert_eq!(versions.primary.len(), keys.len() - 5);
	}
}

/// Independent controls and primary writes copy only their own domain.
#[test]
fn versions_copy_only_the_mutated_domain() {
	use crate::test_assertions::AssertResult as _;

	let mut original = VersionStore::default();
	for key in keys().assert_ok() {
		original.insert(key, 7);
	}
	let mut control = original.clone();
	control.insert(ConflictKey::LiveCatalogRange, 8);
	assert!(Arc::ptr_eq(&control.primary, &original.primary));
	assert!(!Arc::ptr_eq(&control.controls, &original.controls));
	assert!(Arc::ptr_eq(&control.controls.events, &original.controls.events));
	assert_eq!(original.get(&ConflictKey::LiveCatalogRange), Some(&7));
	let mut primary = original.clone();
	primary.insert(ConflictKey::PropertyDictionary, 9);
	assert!(Arc::ptr_eq(&primary.controls, &original.controls));
	assert!(!Arc::ptr_eq(&primary.primary, &original.primary));
	assert_eq!(original.get(&ConflictKey::PropertyDictionary), Some(&7));
	let mut event = original.clone();
	event.insert(ConflictKey::EventOutboxRange, 10);
	assert!(Arc::ptr_eq(&event.primary, &original.primary));
	assert!(Arc::ptr_eq(&event.controls.live, &original.controls.live));
	assert!(!Arc::ptr_eq(&event.controls.events, &original.controls.events));
	assert!(!primary.same_primary(&original));
	let mut independently_equal = VersionStore::default();
	for key in keys().assert_ok() {
		independently_equal.insert(key, 7);
	}
	assert!(!Arc::ptr_eq(&independently_equal.primary, &original.primary));
	assert!(independently_equal.same_primary(&original));
	assert!(independently_equal.same_controls(&original));
}

/// Rebase preserves committed controls even at the provisional data stamp.
#[test]
fn versions_rebase_restamps_only_private_data_and_admits_both_trees() {
	use crate::test_assertions::AssertResult as _;

	let mut original = VersionStore::default();
	for key in keys().assert_ok() {
		original.insert(key, 7);
	}
	let mut current = original.clone();
	current.insert(ConflictKey::LiveCatalogRange, 10);
	let mut candidate = original.clone();
	candidate.insert(ConflictKey::PropertyDictionary, 10);
	let private_snapshot = candidate.clone();
	assert!(candidate.primary_has_revision(10));
	candidate.rebase(&current, 10, 11, true);
	assert_eq!(candidate.get(&ConflictKey::PropertyDictionary), Some(&11));
	assert_eq!(candidate.get(&ConflictKey::LiveCatalogRange), Some(&10));
	assert_eq!(private_snapshot.get(&ConflictKey::PropertyDictionary), Some(&10));
	assert!(Arc::ptr_eq(&candidate.controls, &current.controls));
	assert!(!Arc::ptr_eq(&candidate.primary, &private_snapshot.primary));
	let mut control_only = original.clone();
	control_only.rebase(&current, 10, 11, false);
	assert!(Arc::ptr_eq(&control_only.primary, &original.primary));
	assert!(
		control_only.rebase_allocation_bytes(false).assert_ok()
			< original.rebase_allocation_bytes(true).assert_ok()
	);
	let width = std::mem::size_of::<(ConflictKey, u64)>();
	assert_eq!(
		original.allocation_bytes().assert_ok(),
		crate::values::storage::tree_allocation_bytes(original.primary.len(), width).assert_ok()
			+ crate::values::storage::tree_allocation_bytes(original.controls.live.len(), width)
				.assert_ok()
			+ crate::values::storage::tree_allocation_bytes(original.controls.events.len(), width)
				.assert_ok()
	);
}

/// Verifies versions retire only unreachable event keys and keep old snapshots.
#[test]
fn versions_retire_only_unreachable_event_keys_and_keep_old_snapshots() {
	use crate::{
		datastore::{
			EventAsyncOptions,
			EventCause,
			EventDefinition,
			EventExecution,
			EventInvocation,
			EventOutbox,
			EventTarget,
		},
		test_assertions::AssertResult as _,
	};
	let mut definition = EventDefinition::new(
		"a",
		EventTarget::Class(ClassId::from_storage(1).assert_ok()),
		"true",
		"1",
	)
	.assert_ok()
	.with_execution(EventExecution::Async(EventAsyncOptions::default()));
	definition.id = Some(EventId::from_storage(2).assert_ok());
	let mut invocation = EventInvocation::queued(
		&definition,
		EventCause::next(None, definition.execution()).assert_ok(),
		Arc::from([1_u8]),
		5,
	)
	.assert_ok();
	let kept = JobId::from_uuid(uuid::Uuid::from_u128(1).into());
	invocation.id = kept;
	let mut outbox = EventOutbox::default();
	outbox.enqueue(Arc::new(invocation), 7).assert_ok();
	let mut original = VersionStore::default();
	for key in keys().assert_ok() {
		original.insert(key, 7);
	}
	for index in 2..=20_000_u128 {
		original.insert(
			ConflictKey::EventInvocation(JobId::from_uuid(uuid::Uuid::from_u128(index).into())),
			7,
		);
	}
	let mut current = original.clone();
	current.retain_event_invocations(&outbox);
	assert_eq!(current.controls.events.len(), 2);
	assert_eq!(original.controls.events.len(), 20_001);
	assert!(Arc::ptr_eq(&current.primary, &original.primary));
	assert!(Arc::ptr_eq(&current.controls.live, &original.controls.live));
	assert_eq!(current.get(&ConflictKey::EventInvocation(kept)), Some(&7));
	assert!(current.event_copy_bytes(1).assert_ok() < original.event_copy_bytes(1).assert_ok());
}
