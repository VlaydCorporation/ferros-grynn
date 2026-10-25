//! Owned logical operation images shared by capture, replay and event dispatch.

use std::{collections::BTreeMap, sync::Arc};

use fg_runtime::__internal::ValueInternal as _;

use crate::{
	changefeed::{ChangeEndpoint, ChangeGraphState, ChangeIdentity, ChangeImage, ChangeOverlay},
	datastore::{DatabaseRoot, GlobalAtomId, OccurrenceStateRef, OverlayValue},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomOccurrenceRef, AtomRef, EdgeEndpoint, GraphId, GraphRef},
	values::Object,
};

/// Aggregate capture scratch admission, performed before copies and growth.
pub(super) struct CaptureBudget {
	/// Bytes admitted for all retained images and target metadata.
	pub(super) bytes: usize,
	/// Maximum scratch shared by capture and replay output.
	pub(super) maximum: usize,
}

impl CaptureBudget {
	/// Charges a checked allocation before it is performed.
	pub(super) fn admit(&mut self, additional: usize) -> FerrosGrynnResult<()> {
		let actual = self
			.bytes
			.checked_add(additional)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed capture bytes"))?;
		if actual > self.maximum {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed capture scratch",
				actual as u64,
				self.maximum as u64,
			));
		}
		self.bytes = actual;
		Ok(())
	}

	/// Admits amortized vector growth before allocating its spare slots.
	pub(super) fn reserve<T>(&mut self, buffer: &mut Vec<T>) -> FerrosGrynnResult<()> {
		if buffer.len() < buffer.capacity() {
			return Ok(());
		}
		let capacity =
			buffer.capacity().max(4).checked_mul(2).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("changefeed vector capacity")
			})?;
		self.admit(
			capacity
				.checked_mul(std::mem::size_of::<T>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed vector bytes"))?,
		)?;
		buffer.try_reserve_exact(capacity - buffer.len()).map_err(|error| {
			FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed allocation",
				self.bytes as u64,
				self.maximum as u64,
			)
			.with_technical_source(error)
		})?;
		Ok(())
	}
}

/// Projects a global or graph-local image using this exact replay root.
pub(super) fn image(
	root: &DatabaseRoot,
	global: GlobalAtomId,
	graph: Option<GraphId>,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<Option<Arc<ChangeImage>>> {
	let Some(atom) = root.atom(global) else {
		return Ok(None);
	};
	let occurrence = match graph {
		Some(graph) => {
			let Some(local) = root.membership(graph, global) else {
				return Ok(None);
			};
			Some(
				root.occurrence(graph, local)
					.ok_or_else(|| invalid("changefeed occurrence is absent"))?,
			)
		}
		None => None,
	};
	budget.admit(512usize.saturating_add(
		crate::values::runtime::estimated_record_id_heap_bytes(atom.record_id()),
	))?;
	let atom_ref = AtomRef::new(atom.kind(), atom.record_id().clone());
	let identity = match graph {
		Some(graph) => ChangeIdentity::Occurrence(AtomOccurrenceRef::new(
			atom_ref,
			Arc::new(graph_reference(root, graph)?),
		)),
		None => ChangeIdentity::Record(atom_ref),
	};
	let mut properties = Object::new();
	for (key, base) in atom.properties() {
		let value =
			occurrence.map_or(Some(base), |occurrence| occurrence.effective_value(Some(base), key));
		if let Some(value) = value {
			let name = root
				.dictionary
				.resolve(key)
				.ok_or_else(|| invalid("changefeed property name is absent"))?;
			budget.admit(
				128usize.saturating_add(name.len()).saturating_add(value.estimated_heap_bytes()),
			)?;
			properties.insert(name.clone(), value.clone());
		}
	}
	let mut overlays = BTreeMap::new();
	if let Some(overlay) = occurrence.and_then(|occurrence| occurrence.overlay()) {
		for (key, decision) in overlay.iter() {
			let name = root
				.dictionary
				.resolve(key)
				.ok_or_else(|| invalid("changefeed overlay name is absent"))?;
			let bytes = match decision {
				OverlayValue::Value(value) => value.estimated_heap_bytes(),
				OverlayValue::Hidden => 0,
			};
			budget.admit(
				256usize.saturating_add(name.len()).saturating_add(bytes.saturating_mul(2)),
			)?;
			let decision = match decision {
				OverlayValue::Value(value) => {
					properties.insert(name.clone(), value.clone());
					ChangeOverlay::Value(value.clone())
				}
				OverlayValue::Hidden => ChangeOverlay::Hidden,
			};
			overlays.insert(name.clone(), decision);
		}
	}
	let label = occurrence.and_then(|occurrence| occurrence.label());
	budget.admit(label.map_or(0, |label| label.len()))?;
	let graph_state = occurrence
		.map(|occurrence| -> FerrosGrynnResult<_> {
			match occurrence.state().view() {
				OccurrenceStateRef::Node {
					nested_graph,
				} => Ok(ChangeGraphState::Node {
					nested_graph: nested_graph
						.map(|graph| graph_reference(root, graph))
						.transpose()?,
				}),
				OccurrenceStateRef::Edge {
					topology,
					weight,
					transition_graph,
				} => {
					let graph = graph.ok_or_else(|| invalid("changefeed edge has no graph"))?;
					let mut source = Vec::new();
					let mut target = Vec::new();
					budget.admit(
						topology
							.endpoints()
							.count()
							.checked_mul(std::mem::size_of::<ChangeEndpoint>())
							.ok_or_else(|| {
								FerrosGrynnError::operation_overflow("changefeed endpoints")
							})?,
					)?;
					source.try_reserve_exact(topology.source().len()).map_err(|error| {
						invalid("changefeed endpoint allocation failed")
							.with_technical_source(error)
					})?;
					target.try_reserve_exact(topology.target().len()).map_err(|error| {
						invalid("changefeed endpoint allocation failed")
							.with_technical_source(error)
					})?;
					for endpoint in topology.source() {
						source.push(qualified_endpoint(root, graph, endpoint, budget)?);
					}
					for endpoint in topology.target() {
						target.push(qualified_endpoint(root, graph, endpoint, budget)?);
					}
					Ok(ChangeGraphState::Edge {
						direction: topology.direction(),
						source,
						target,
						weight: *weight,
						transition_graph: transition_graph
							.map(|graph| graph_reference(root, graph))
							.transpose()?,
					})
				}
			}
		})
		.transpose()?;
	Ok(Some(Arc::new(ChangeImage {
		identity,
		properties,
		label: label.cloned(),
		overlays,
		graph_state,
	})))
}

/// Copies a validated historical graph reference within the admitted fixed
/// charge.
fn graph_reference(root: &DatabaseRoot, graph: GraphId) -> FerrosGrynnResult<GraphRef> {
	root.schema
		.graph(graph)
		.map(|graph| graph.reference().clone())
		.ok_or_else(|| invalid("changefeed referenced graph is absent"))
}

/// Converts a local occurrence to its owned public qualified identity.
fn occurrence_ref(
	root: &DatabaseRoot,
	graph: GraphId,
	local: AtomId,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<AtomOccurrenceRef> {
	let occurrence = root
		.occurrence(graph, local)
		.ok_or_else(|| invalid("changefeed endpoint occurrence is absent"))?;
	let atom = root
		.atom(occurrence.global_id())
		.ok_or_else(|| invalid("changefeed endpoint base record is absent"))?;
	budget.admit(512usize.saturating_add(
		crate::values::runtime::estimated_record_id_heap_bytes(atom.record_id()),
	))?;
	Ok(AtomOccurrenceRef::new(
		AtomRef::new(atom.kind(), atom.record_id().clone()),
		Arc::new(graph_reference(root, graph)?),
	))
}

/// Preserves repeated routes to shared children and edge-owned transition
/// graphs.
fn qualified_endpoint(
	root: &DatabaseRoot,
	origin: GraphId,
	endpoint: &EdgeEndpoint,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<ChangeEndpoint> {
	let depth = match endpoint {
		EdgeEndpoint::Local(_) => 0,
		EdgeEndpoint::Nested(qualified) => qualified.scope().len(),

		_ => {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"Unsupported runtime variant at the workspace boundary.",
			));
		}
	};
	budget.admit(
		depth
			.checked_mul(std::mem::size_of::<AtomOccurrenceRef>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed containment route"))?,
	)?;
	let mut owners = Vec::new();
	owners.try_reserve_exact(depth).map_err(|error| {
		invalid("changefeed route allocation failed").with_technical_source(error)
	})?;
	let (graph, occurrence) =
		root.resolve_incidence_endpoint(origin, endpoint, |graph, owner| {
			owners.push(occurrence_ref(root, graph, owner.local_id(), budget)?);
			Ok(())
		})?;
	Ok(ChangeEndpoint {
		occurrence: occurrence_ref(root, graph, occurrence.local_id(), budget)?,
		owners,
	})
}

/// Compares images without collapsing numeric storage tags or decimal scales.
pub(super) fn same_image(left: Option<&ChangeImage>, right: Option<&ChangeImage>) -> bool {
	match (left, right) {
		(None, None) => true,
		(Some(left), Some(right)) => {
			left.identity == right.identity
				&& same_object(&left.properties, &right.properties)
				&& left.label == right.label
				&& left.overlays.len() == right.overlays.len()
				&& left.overlays.iter().zip(&right.overlays).all(|((lk, lv), (rk, rv))| {
					lk == rk
						&& match (lv, rv) {
							(ChangeOverlay::Value(lv), ChangeOverlay::Value(rv)) => {
								same_value(lv, rv)
							}
							(ChangeOverlay::Hidden, ChangeOverlay::Hidden) => true,
							_ => false,
						}
				})
				&& match (&left.graph_state, &right.graph_state) {
					(
						Some(ChangeGraphState::Edge {
							direction: ld,
							source: ls,
							target: lt,
							weight: lw,
							transition_graph: lg,
						}),
						Some(ChangeGraphState::Edge {
							direction: rd,
							source: rs,
							target: rt,
							weight: rw,
							transition_graph: rg,
						}),
					) => {
						ld == rd
							&& ls == rs
							&& lt == rt
							&& lg == rg
							&& match (lw, rw) {
								(None, None) => true,
								(Some(lw), Some(rw)) => same_number(*lw, *rw),
								_ => false,
							}
					}
					(left, right) => left == right,
				}
		}
		_ => false,
	}
}

/// Compares named properties in canonical key order.
pub(super) use crate::values::runtime::{same_number, same_object, same_value};

/// Typed malformed normalized-state error.
fn invalid(detail: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(detail)
}
