//! Label metadata follows the same source/final namespace as immutable arrays.

use super::{IncidencePlan, invalid};
use crate::{
	datastore::{DatabaseRoot, GraphOccurrence},
	gql_status::FerrosGrynnResult,
	graph::GraphId,
	storage::{
		atom_page::PropertyRef,
		graph_index::LabelDictionary,
		local_store::{point_label, point_namespace::label_kind, point_occurrence::SlotState},
	},
};

impl IncidencePlan<'_, '_> {
	/// Resolves the accepted source independently of the final label namespace.
	///
	/// # Errors
	/// Rejects missing occurrences, replay identity mismatches and unresolved
	/// labels.
	pub(super) fn source_label(&self, state: SlotState) -> FerrosGrynnResult<Option<u32>> {
		slot_label(
			self.before,
			self.files.graph_id,
			self.namespace.map(|namespace| &namespace.before),
			state,
		)
	}

	/// Resolves the final replay image without cloning a label dictionary.
	///
	/// # Errors
	/// Rejects missing occurrences, replay identity mismatches and unresolved
	/// labels.
	pub(super) fn target_label(&self, state: SlotState) -> FerrosGrynnResult<Option<u32>> {
		slot_label(
			self.after,
			self.files.graph_id,
			self.namespace.map(|namespace| &namespace.after),
			state,
		)
	}

	/// Checks the old physical payload before detaching or replacing its owner.
	///
	/// # Errors
	/// Rejects replay identity, missing allocator, malformed payload and budget
	/// failures.
	pub(super) fn validate_source_overlay(
		&mut self,
		state: SlotState,
		reference: PropertyRef,
	) -> FerrosGrynnResult<()> {
		let overlay = slot_occurrence(self.before, self.files.graph_id, state)?
			.and_then(GraphOccurrence::overlay);
		if overlay.is_none() {
			return if reference == PropertyRef::None {
				Ok(())
			} else {
				Err(invalid("incidence source overlay presence disagrees with replay"))
			};
		}
		self.overlays
			.as_mut()
			.ok_or_else(|| invalid("incidence source overlay allocator is absent"))?
			.validate_source(
				self.store,
				self.state,
				(self.files, self.header.overlays_pages),
				reference,
				overlay,
				self.budget,
			)
	}

	/// Reuses only a validated same-owner payload; generations never inherit
	/// refs.
	///
	/// # Errors
	/// Rejects replay identity, missing allocator, bounded conversion and
	/// append failures.
	pub(super) fn target_overlay(
		&mut self,
		before: SlotState,
		after: SlotState,
		source: PropertyRef,
	) -> FerrosGrynnResult<PropertyRef> {
		let Some(target) = slot_occurrence(self.after, self.files.graph_id, after)? else {
			return Ok(PropertyRef::None);
		};
		let overlay = target.overlay();
		if before.occurrence == after.occurrence
			&& slot_occurrence(self.before, self.files.graph_id, before)?
				.and_then(GraphOccurrence::overlay)
				== overlay
		{
			return Ok(source);
		}
		let Some(overlay) = overlay else {
			return Ok(PropertyRef::None);
		};
		self.overlays
			.as_mut()
			.ok_or_else(|| invalid("incidence target overlay allocator is absent"))?
			.insert(overlay, self.budget)
	}
}

/// Free slots carry no label; unchanged namespaces use the existing borrowed
/// lookup.
fn slot_label(
	root: &DatabaseRoot,
	graph: GraphId,
	namespace: Option<&LabelDictionary>,
	state: SlotState,
) -> FerrosGrynnResult<Option<u32>> {
	let Some(occurrence) = slot_occurrence(root, graph, state)? else {
		return Ok(None);
	};
	let local = occurrence.local_id();
	let Some(name) = occurrence.label() else {
		return Ok(None);
	};
	let kind = local.kind().ok_or_else(|| invalid("incidence metadata kind is null"))?;
	if let Some(namespace) = namespace {
		return namespace
			.id(label_kind(kind), name.as_str())
			.map(Some)
			.ok_or_else(|| invalid("incidence label is absent from its namespace"));
	}
	point_label::label_id(root, graph, kind, Some(name))
}

/// One checked replay lookup serves labels and source/final payloads.
fn slot_occurrence(
	root: &DatabaseRoot,
	graph: GraphId,
	state: SlotState,
) -> FerrosGrynnResult<Option<&GraphOccurrence>> {
	let Some((local, global)) = state.occurrence else {
		return Ok(None);
	};
	let occurrence = root
		.occurrence(graph, local)
		.ok_or_else(|| invalid("incidence metadata occurrence is absent"))?;
	if occurrence.global_id() != global || local.generation() != state.generation {
		return Err(invalid("incidence metadata identity disagrees with replay"));
	}
	Ok(Some(occurrence))
}

#[cfg(test)]
/// Source/final namespace, replay identity and free-slot boundaries.
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore},
		graph::{AtomKind, EdgeTopology, GraphKind},
		storage::graph_index::LabelKind,
		test_assertions::{AssertOption as _, AssertResult as _},
		values::{PropertyMap, RecordId},
	};

	/// NODE/EDGE namespaces stay independent across source/final renumbering.
	#[test]
	fn incidence_metadata_resolves_both_sides_and_rejects_identity_mismatch() {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
		let graph = seed.define_graph("g", GraphKind::Graph).assert_ok().id();
		let records = ["item:m", "item:z", "link:m", "link:z"]
			.map(|name| name.parse::<RecordId>().assert_ok());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		let left = seed.attach_node(graph, &records[0], Some("M".into()), None).assert_ok();
		let right = seed.attach_node(graph, &records[1], Some("Z".into()), None).assert_ok();
		seed.attach_edge(
			graph,
			&records[2],
			Some("M".into()),
			EdgeTopology::directed([left], [right]),
			None,
			None,
		)
		.assert_ok();
		let edge = seed
			.attach_edge(
				graph,
				&records[3],
				Some("Z".into()),
				EdgeTopology::directed([left], [right]),
				None,
				None,
			)
			.assert_ok();
		seed.commit().assert_ok();
		let before = store.inner.current_root();
		let states = [
			(right, before.resolve_atom(&records[1]).assert_some()),
			(edge, before.resolve_atom(&records[3]).assert_some()),
		]
		.map(|(local, global)| SlotState {
			generation: local.generation(),
			occurrence: Some((local, global)),
		});
		let mut change = store.begin();
		for record in [&records[1], &records[3]] {
			change.set_occurrence_label(graph, record, Some("A".into())).assert_ok();
		}
		change.commit().assert_ok();
		let after = store.inner.current_root();
		let source = LabelDictionary::from_labels([
			(LabelKind::Node, "M"),
			(LabelKind::Node, "Z"),
			(LabelKind::Edge, "M"),
			(LabelKind::Edge, "Z"),
		])
		.assert_ok();
		let target = LabelDictionary::from_labels([
			(LabelKind::Node, "A"),
			(LabelKind::Node, "M"),
			(LabelKind::Edge, "A"),
			(LabelKind::Edge, "M"),
		])
		.assert_ok();
		for state in states {
			for namespace in [None, Some(&source)] {
				assert_eq!(slot_label(&before, graph, namespace, state).assert_ok(), Some(1));
			}
			for namespace in [None, Some(&target)] {
				assert_eq!(slot_label(&after, graph, namespace, state).assert_ok(), Some(0));
			}
			assert!(slot_label(&after, graph, Some(&source), state).is_err());
			assert!(
				slot_label(&before, graph, None, SlotState {
					generation: state.generation + 1,
					..state
				})
				.is_err()
			);
		}
		assert_eq!(
			slot_label(&before, graph, None, SlotState {
				generation: 7,
				occurrence: None
			})
			.assert_ok(),
			None
		);
		assert_ne!(
			states[0].occurrence.assert_some().0.kind(),
			states[1].occurrence.assert_some().0.kind()
		);
	}
}
