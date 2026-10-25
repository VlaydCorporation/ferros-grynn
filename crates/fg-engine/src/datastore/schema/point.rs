//! Exact additive schema admission shared by replay and sparse publication.

use std::sync::Arc;

use super::{GraphSchema, SchemaCatalog, SchemaVersion, SystemIndexKind};

impl GraphSchema {
	/// Allows only the first endpoint capability and its single version step.
	pub(crate) fn is_endpoint_extension_of(&self, before: &Self) -> bool {
		self.version().get().checked_sub(1) == Some(before.version().get())
			&& before.system_indexes().len() == 2
			&& self.system_indexes().len() == 3
			&& before.system_indexes().eq(self.system_indexes().take(2))
			&& before.edge_endpoint_index().is_none()
			&& self.edge_endpoint_index().is_some_and(|index| {
				index.kind() == SystemIndexKind::EdgeEndpoint
					&& index.version() == SchemaVersion::INITIAL
			})
			&& self.reference() == before.reference()
			&& self.properties() == before.properties()
			&& self.topology_constraints() == before.topology_constraints()
			&& self.root() == before.root()
			&& self.owner_root() == before.owner_root()
			&& self.indexes().eq(before.indexes())
	}
}

impl SchemaCatalog {
	/// Preserves every unrelated namespace, definition and revision.
	pub(crate) fn is_endpoint_extension_of(&self, before: &Self, revision: u64) -> bool {
		// Shared reservations may leave gaps, including an older private ID.
		self.next_schema_id >= before.next_schema_id
			&& self.next_graph_id == before.next_graph_id
			&& self.graph_names == before.graph_names
			&& self.classes == before.classes
			&& self.class_names == before.class_names
			&& self.class_versions == before.class_versions
			&& self.parameters == before.parameters
			&& self.parameter_versions == before.parameter_versions
			&& self.functions == before.functions
			&& self.function_signatures == before.function_signatures
			&& self.function_versions == before.function_versions
			&& self.analyzers == before.analyzers
			&& self.analyzer_names == before.analyzer_names
			&& self.analyzer_versions == before.analyzer_versions
			&& self.events == before.events
			&& self.event_names == before.event_names
			&& self.graphs.len() == before.graphs.len()
			&& self.graph_versions.len() == before.graph_versions.len()
			&& before.graphs.iter().all(|(id, old)| {
				self.graphs.get(id).is_some_and(|new| {
					if Arc::ptr_eq(old, new) {
						self.graph_versions.get(id) == before.graph_versions.get(id)
					} else {
						new.is_endpoint_extension_of(old)
							&& new.published_at == revision
							&& self.graph_versions.get(id) == Some(&revision)
							&& new.edge_endpoint_index().is_some_and(|index| {
								u64::from(index.id().get()) < self.next_schema_id
							})
					}
				})
			})
	}
}

#[cfg(test)]
/// Pure admission tests use real accepted RAM catalogs, without physical I/O.
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore, SystemIndex},
		graph::{AtomKind, EdgeTopology, GraphKind},
		test_assertions::{AssertOption as _, AssertResult as _},
		values::{PropertyMap, RecordId},
	};

	/// Retains one unrelated graph and populated class/name/version namespaces.
	fn catalogs() -> (Arc<crate::datastore::DatabaseRoot>, Arc<crate::datastore::DatabaseRoot>) {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("node", AtomKind::Node, []).assert_ok()).assert_ok();
		seed.define_class(ClassSchema::new("edge", AtomKind::Edge, []).assert_ok()).assert_ok();
		let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
		seed.define_graph("other", GraphKind::Graph).assert_ok();
		let node = "node:a".parse::<RecordId>().assert_ok();
		seed.create_atom(node.clone(), PropertyMap::new()).assert_ok();
		let local = seed.attach_node(graph.id(), &node, None, None).assert_ok();
		let records = ["edge:e0", "edge:e1"].map(|name| name.parse::<RecordId>().assert_ok());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		let endpoint = seed
			.attach_edge(
				graph.id(),
				&records[0],
				None,
				EdgeTopology::directed([local], [local]),
				None,
				None,
			)
			.assert_ok();
		seed.commit().assert_ok();
		let before = store.inner.current_root();
		let mut write = store.begin();
		write
			.attach_edge(
				graph.id(),
				&records[1],
				None,
				EdgeTopology::directed([endpoint], [local]),
				None,
				None,
			)
			.assert_ok();
		write.commit().assert_ok();
		(before, store.inner.current_root())
	}

	/// Version, graph metadata and core capabilities cannot hide behind
	/// addition.
	#[test]
	fn endpoint_schema_admission_rejects_combined_graph_changes() {
		let (before, after) = catalogs();
		let old = before.schema.named_graph("g").assert_some();
		let new = after.schema.graph(old.id()).assert_some();
		assert!(new.is_endpoint_extension_of(&old));
		assert!(!old.is_endpoint_extension_of(&old));
		assert!(!new.is_endpoint_extension_of(&new));
		assert!(
			!new.replace_indexes(new.indexes().cloned().collect())
				.assert_ok()
				.is_endpoint_extension_of(&old)
		);
		let capability = new.edge_endpoint_index().assert_some();
		let rooted = old
			.replace_definition(
				None,
				old.topology_constraints(),
				Some("node:a".parse::<RecordId>().assert_ok()),
			)
			.assert_ok()
			.with_edge_endpoint_index(capability)
			.assert_ok();
		assert!(!rooted.is_endpoint_extension_of(&old));
		let versioned = SystemIndex::from_storage(
			capability.id(),
			SchemaVersion::new(2).assert_some(),
			SystemIndexKind::EdgeEndpoint,
		);
		let versioned = old
			.replace_indexes(old.indexes().cloned().collect())
			.assert_ok()
			.with_edge_endpoint_index(versioned)
			.assert_ok();
		assert!(!versioned.is_endpoint_extension_of(&old));
	}

	/// Other schema changes and wrong publication stamps retain full fallback.
	#[test]
	fn endpoint_schema_admission_preserves_catalog_and_allows_reservation_gaps() {
		let (before, after) = catalogs();
		assert!(after.schema.is_endpoint_extension_of(&before.schema, after.commit_ts));
		assert!(!after.schema.is_endpoint_extension_of(&before.schema, after.commit_ts + 1));
		let mut gap = (*after.schema).clone();
		gap.next_schema_id += 17;
		let mut earlier_reservation = (*before.schema).clone();
		earlier_reservation.next_schema_id = gap.next_schema_id;
		assert!(gap.is_endpoint_extension_of(&earlier_reservation, after.commit_ts));
		let mut changed = (*after.schema).clone();
		changed.next_graph_id += 1;
		assert!(!changed.is_endpoint_extension_of(&before.schema, after.commit_ts));
		let mut changed = (*after.schema).clone();
		changed.classes.clear();
		assert!(!changed.is_endpoint_extension_of(&before.schema, after.commit_ts));
		let mut changed = (*after.schema).clone();
		changed.graph_names.remove("other");
		assert!(!changed.is_endpoint_extension_of(&before.schema, after.commit_ts));
		let other = before.schema.named_graph("other").assert_some();
		let mut changed = (*after.schema).clone();
		changed.graph_versions.insert(other.id(), after.commit_ts);
		assert!(!changed.is_endpoint_extension_of(&before.schema, after.commit_ts));
	}
}
