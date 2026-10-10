//! Independent revision-map and snapshot ownership regressions.

use super::*;
use crate::{
	changefeed::ChangefeedSource,
	datastore::{AnalyzerId, ClassId, FunctionId, GlobalAtomId, SchemaId},
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
fn versions_match_all_conflict_namespaces_and_preserve_snapshots() -> FerrosGrynnResult<()> {
	let mut versions = Arc::new(VersionStore::default());
	let mut oracle = BTreeMap::new();
	let keys = keys()?;
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
		assert_eq!(
			versions.primary.iter().chain(versions.live.iter()).collect::<Vec<_>>(),
			oracle.iter().collect::<Vec<_>>()
		);
		assert_eq!(versions.live.len(), 3);
		assert_eq!(versions.primary.len(), keys.len() - 3);
	}
	Ok(())
}

/// Independent controls and primary writes copy only their own domain.
#[test]
fn versions_copy_only_the_mutated_domain() -> FerrosGrynnResult<()> {
	let mut original = VersionStore::default();
	for key in keys()? {
		original.insert(key, 7);
	}
	let mut control = original.clone();
	control.insert(ConflictKey::LiveCatalogRange, 8);
	assert!(Arc::ptr_eq(&control.primary, &original.primary));
	assert!(!Arc::ptr_eq(&control.live, &original.live));
	assert_eq!(original.get(&ConflictKey::LiveCatalogRange), Some(&7));
	let mut primary = original.clone();
	primary.insert(ConflictKey::PropertyDictionary, 9);
	assert!(Arc::ptr_eq(&primary.live, &original.live));
	assert!(!Arc::ptr_eq(&primary.primary, &original.primary));
	assert_eq!(original.get(&ConflictKey::PropertyDictionary), Some(&7));
	assert!(!primary.same_primary(&original));
	let mut independently_equal = VersionStore::default();
	for key in keys()? {
		independently_equal.insert(key, 7);
	}
	assert!(!Arc::ptr_eq(&independently_equal.primary, &original.primary));
	assert!(independently_equal.same_primary(&original));
	assert!(independently_equal.same_live(&original));
	Ok(())
}

/// Rebase preserves committed controls even at the provisional data stamp.
#[test]
fn versions_rebase_restamps_only_private_data_and_admits_both_trees() -> FerrosGrynnResult<()> {
	let mut original = VersionStore::default();
	for key in keys()? {
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
	assert!(Arc::ptr_eq(&candidate.live, &current.live));
	assert!(!Arc::ptr_eq(&candidate.primary, &private_snapshot.primary));
	let mut control_only = original.clone();
	control_only.rebase(&current, 10, 11, false);
	assert!(Arc::ptr_eq(&control_only.primary, &original.primary));
	assert!(control_only.rebase_allocation_bytes(false)? < original.rebase_allocation_bytes(true)?);
	let width = std::mem::size_of::<(ConflictKey, u64)>();
	assert_eq!(
		original.allocation_bytes()?,
		crate::values::storage::tree_allocation_bytes(original.primary.len(), width)?
			+ crate::values::storage::tree_allocation_bytes(original.live.len(), width)?
	);
	Ok(())
}
