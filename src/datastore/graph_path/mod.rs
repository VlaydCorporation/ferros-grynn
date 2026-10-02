//! Immutable directed distances over one captured incidence projection.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
	datastore::{
		DatabaseRoot,
		GraphIndex,
		GraphIndexCost,
		GraphIndexDefinition,
		GraphRoot,
		OccurrenceState,
		PathIndexAlgorithm,
		graph_index_relation::build_graph_index_relation,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		DirectedLandmarks,
		DistanceInterval,
		EdgeEndpoint,
		LandmarkBuildLimits,
		LandmarkDistanceRows,
		ProjectedRelation,
		TraversalDirection,
		TraversalSemantics,
		visit_endpoint_steps,
	},
	storage::{DerivedSectionKind, GraphArtifactIdentity, LandmarkPathHeader, LandmarkPathLayout},
	values::{Number, storage::DiskAtomRef},
};

/// Complete semantic definition and canonical vertices with directed distance
/// rows.
#[derive(Debug)]
pub struct LandmarkPathArtifact {
	/// Checked full file projection, including metadata pages.
	pub layout: LandmarkPathLayout,
	/// Captured local vertices in GlobalAtomId order.
	pub vertices: Vec<AtomId>,
	/// Local lookup, kept inside the snapshot adapter.
	pub ordinal_by_atom: BTreeMap<AtomId, usize>,
	/// Deterministic landmark-major from/to distances.
	pub distances: DirectedLandmarks,
	/// Full bytes disambiguating definition hashes.
	pub descriptor: Vec<u8>,
}

impl LandmarkPathArtifact {
	/// Builds an admitted immutable projection using the shared graph kernels.
	///
	/// # Errors
	/// Rejects ownership, invalid costs, budgets or cancellation before
	/// publication.
	pub fn build<Check>(
		database: &DatabaseRoot,
		index: &GraphIndex,
		revision: u64,
		scratch_bytes: usize,
		check: &mut Check,
	) -> FerrosGrynnResult<Self>
	where
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		check()?;
		let (count, cost) = definition(index)?;
		let graph = index
			.graph()
			.and_then(|id| database.graph(id))
			.ok_or_else(|| invalid("path index graph is absent"))?;
		let descriptor = index.descriptor_with_limit(scratch_bytes)?;
		let (cost_kind, function, function_version) = cost_metadata(cost);
		let layout = LandmarkPathLayout::new(LandmarkPathHeader {
			identity: GraphArtifactIdentity {
				index: index.id().ok_or_else(|| invalid("path index identity is absent"))?.get(),
				version: index.version().get(),
				graph: graph.reference().id().get(),
				revision,
				fingerprint: rapidhash::v3::rapidhash_v3(&descriptor),
			},
			vertices: graph
				.occurrences()
				.filter(|occurrence| {
					index.mode() == TraversalSemantics::Atom || occurrence.local_id().is_node()
				})
				.count() as u64,
			requested_landmarks: u32::from(count),
			descriptor_bytes: descriptor.len() as u64,
			max_bytes: index
				.max_bytes()
				.ok_or_else(|| invalid("landmark path requires MAX_BYTES"))?,
			cost_kind,
			function,
			function_version,
			direction: direction(index),
			mode: u8::from(index.mode() == TraversalSemantics::Atom),
			has_cross_level: false,
		})?;
		let evaluator_bytes = if matches!(cost, GraphIndexCost::Function { .. }) {
			scratch_bytes / 4
		} else {
			0
		};
		let fixed = descriptor
			.len()
			.checked_add(65_536)
			.and_then(|bytes| bytes.checked_add(evaluator_bytes))
			.ok_or_else(|| resource(scratch_bytes))?;
		let relation_budget =
			scratch_bytes.checked_sub(fixed).ok_or_else(|| resource(scratch_bytes))?;
		let relation = build_graph_index_relation(database, index, relation_budget, check)?;
		let live = relation
			.vertices
			.len()
			.checked_mul(512)
			.and_then(|bytes| {
				relation
					.forward
					.targets
					.len()
					.checked_mul(128)
					.and_then(|arcs| bytes.checked_add(arcs))
			})
			.and_then(|bytes| bytes.checked_add(fixed))
			.ok_or_else(|| resource(scratch_bytes))?;
		if live > scratch_bytes {
			return Err(resource(scratch_bytes));
		}
		let weights = if cost == GraphIndexCost::Hop {
			None
		} else {
			let evaluator = if evaluator_bytes != 0 {
				Some(crate::gql::PureFunctionEvaluator::for_builder(
					database,
					evaluator_bytes,
					crate::storage::check_external_sort_cancelled,
				)?)
			} else {
				None
			};
			Some(projected_costs(
				graph,
				index,
				&relation,
				cost,
				scratch_bytes,
				evaluator.as_ref(),
				check,
			)?)
		};
		let distances = DirectedLandmarks::build(
			&relation,
			weights.as_deref(),
			count,
			LandmarkBuildLimits {
				result_bytes: layout.header.max_bytes,
				scratch_bytes,
				live_scratch_bytes: live,
			},
			check,
		)?;
		let layout = LandmarkPathLayout::new(LandmarkPathHeader {
			has_cross_level: relation.has_cross_level,
			..layout.header
		})?;
		Ok(Self {
			layout,
			vertices: relation.vertices,
			ordinal_by_atom: relation.ordinal_by_atom,
			distances,
			descriptor,
		})
	}

	/// Restores complete structural invariants against the recovered graph
	/// snapshot.
	///
	/// # Errors
	/// Rejects incompatible semantic bytes, identities, ordinals or distance
	/// cells.
	pub fn restore<Check>(
		graph: &GraphRoot,
		index: &GraphIndex,
		layout: LandmarkPathLayout,
		data: [Vec<u8>; 5],
		scratch_bytes: usize,
		check: &mut Check,
	) -> FerrosGrynnResult<Self>
	where
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		check()?;
		layout.admit_decode(scratch_bytes)?;
		let (count, cost) = definition(index)?;
		let header = layout.header;
		let descriptor = index.descriptor_with_limit(scratch_bytes)?;
		let (cost_kind, function, function_version) = cost_metadata(cost);
		let has_cross_level = graph.occurrences().filter(|occurrence| index.edge_labels().len() == 0 || index.edge_labels().any(|label| occurrence.label() == Some(label))).any(|occurrence| matches!(occurrence.state(), OccurrenceState::Edge { topology, .. } if topology.endpoints().any(|endpoint| endpoint.is_cross_level())));
		if header.identity.index != index.id().map_or(0, |id| id.get())
			|| header.identity.version != index.version().get()
			|| header.identity.graph != graph.reference().id().get()
			|| Some(graph.reference().id()) != index.graph()
			|| header.identity.revision != index.build_revision()
			|| header.identity.fingerprint != rapidhash::v3::rapidhash_v3(&descriptor)
			|| data[4] != descriptor
			|| header.requested_landmarks != u32::from(count)
			|| Some(header.max_bytes) != index.max_bytes()
			|| (header.cost_kind, header.function, header.function_version)
				!= (cost_kind, function, function_version)
			|| header.direction != direction(index)
			|| header.has_cross_level != has_cross_level
			|| header.mode != u8::from(index.mode() == TraversalSemantics::Atom)
			|| header.vertices
				!= graph
					.occurrences()
					.filter(|occurrence| header.mode == 1 || occurrence.local_id().is_node())
					.count() as u64
		{
			return Err(invalid("landmark path artifact and captured definition differ"));
		}
		for (section, bytes) in layout.sections.iter().zip(&data) {
			if section.count.checked_mul(u64::from(section.width)) != Some(bytes.len() as u64) {
				return Err(invalid("path section length differs from metadata"));
			}
		}
		drop(descriptor);
		let [vertex_bytes, landmark_bytes, from_bytes, to_bytes, descriptor] = data;
		let mut vertices = reserve(vertex_bytes.len() / 8, scratch_bytes)?;
		let mut ordinal_by_atom = BTreeMap::new();
		let mut previous = None;
		for bytes in vertex_bytes.as_chunks::<8>().0 {
			check()?;
			let reference = DiskAtomRef::from_raw(u64::from_le_bytes(*bytes))?;
			let slot = usize::try_from(reference.slot()).map_err(|cause| {
				invalid("path vertex exceeds platform").with_technical_source(cause)
			})?;
			let generation = graph
				.slot_generation(reference.kind(), slot)
				.ok_or_else(|| invalid("path vertex slot is absent"))?;
			let atom = AtomId::from_parts(reference.kind(), reference.slot(), generation)?;
			let occurrence = graph
				.occurrence(atom)
				.filter(|_| header.mode == 1 || atom.is_node())
				.ok_or_else(|| invalid("path vertex is absent from selected projection"))?;
			let global = occurrence.global_id().raw();
			if previous.is_some_and(|before| before >= global) {
				return Err(invalid("path vertices are not canonical distinct identities"));
			}
			previous = Some(global);
			ordinal_by_atom.insert(atom, vertices.len());
			vertices.push(atom);
		}
		let mut landmarks = reserve(landmark_bytes.len() / 8, scratch_bytes)?;
		let mut distinct = BTreeSet::new();
		for bytes in landmark_bytes.as_chunks::<8>().0 {
			check()?;
			let ordinal = usize::try_from(u64::from_le_bytes(*bytes)).map_err(|cause| {
				invalid("landmark ordinal exceeds platform").with_technical_source(cause)
			})?;
			if ordinal >= vertices.len() || !distinct.insert(ordinal) {
				return Err(invalid("landmark ordinal is absent or repeated"));
			}
			landmarks.push(ordinal);
		}
		let from = decode_distances(&from_bytes, cost_kind, vertices.len(), scratch_bytes, check)?;
		let to = decode_distances(&to_bytes, cost_kind, vertices.len(), scratch_bytes, check)?;
		for (row, ordinal) in landmarks.iter().enumerate() {
			check()?;
			let cell = row
				.checked_mul(vertices.len())
				.and_then(|start| start.checked_add(*ordinal))
				.ok_or_else(|| invalid("landmark seed cell overflows"))?;
			for distances in [&from, &to] {
				let zero = match distances {
					LandmarkDistanceRows::Hops(values) => values.get(cell) == Some(&0),
					LandmarkDistanceRows::Intervals(values) => {
						values.get(cell) == Some(&DistanceInterval::ZERO)
					}
				};
				if !zero {
					return Err(invalid("landmark self distance is not exact zero"));
				}
			}
		}
		let distances = DirectedLandmarks {
			vertices: vertices.len(),
			landmarks,
			from,
			to,
		};
		Ok(Self {
			layout,
			vertices,
			ordinal_by_atom,
			distances,
			descriptor,
		})
	}

	/// Encodes a complete physical element after checking the section extent.
	///
	/// # Errors
	/// Rejects incompatible sections, absent ordinals or widths.
	pub fn encode_element(
		&self,
		kind: DerivedSectionKind,
		ordinal: u64,
		output: &mut [u8],
	) -> FerrosGrynnResult<()> {
		let section = self
			.layout
			.sections
			.iter()
			.find(|section| section.kind == kind)
			.ok_or_else(|| invalid("path section is incompatible"))?;
		if ordinal >= section.count || output.len() != usize::from(section.width) {
			return Err(invalid("path element ordinal or width is invalid"));
		}
		let ordinal = usize::try_from(ordinal).map_err(|cause| {
			invalid("path ordinal exceeds platform").with_technical_source(cause)
		})?;
		let value = match kind {
			DerivedSectionKind::PathVertexMap => {
				let atom =
					self.vertices.get(ordinal).ok_or_else(|| invalid("path vertex is absent"))?;
				DiskAtomRef::from_parts(
					atom.kind().ok_or_else(|| invalid("path vertex kind is absent"))?,
					atom.slot(),
				)?
				.raw()
			}
			DerivedSectionKind::LandmarkRefs => *self
				.distances
				.landmarks
				.get(ordinal)
				.ok_or_else(|| invalid("path landmark is absent"))?
				as u64,
			DerivedSectionKind::LandmarkDistanceFrom | DerivedSectionKind::LandmarkDistanceTo => {
				let rows = if kind == DerivedSectionKind::LandmarkDistanceFrom {
					&self.distances.from
				} else {
					&self.distances.to
				};
				match rows {
					LandmarkDistanceRows::Hops(values) if output.len() == 8 => {
						*values.get(ordinal).ok_or_else(|| invalid("hop distance is absent"))?
					}
					LandmarkDistanceRows::Intervals(values) if output.len() == 16 => {
						let interval = values
							.get(ordinal)
							.ok_or_else(|| invalid("cost distance is absent"))?;
						DistanceInterval::new(interval.lower, interval.upper)?;
						output[..8].copy_from_slice(&interval.lower.to_le_bytes());
						output[8..].copy_from_slice(&interval.upper.to_le_bytes());
						return Ok(());
					}
					_ => return Err(invalid("path distance encoding is incompatible")),
				}
			}
			DerivedSectionKind::DefinitionDescriptor => {
				output[0] = *self
					.descriptor
					.get(ordinal)
					.ok_or_else(|| invalid("path descriptor byte is absent"))?;
				return Ok(());
			}
			_ => return Err(invalid("path element section is incompatible")),
		};
		output.copy_from_slice(&value.to_le_bytes());
		Ok(())
	}
}

/// Resolves a materialized landmark definition without cloning patterns.
fn definition(index: &GraphIndex) -> FerrosGrynnResult<(u16, GraphIndexCost)> {
	match index.definition() {
		GraphIndexDefinition::Path {
			algorithm: PathIndexAlgorithm::LandmarkSssp(count),
			cost,
			..
		} => Ok((*count, *cost)),
		_ => Err(invalid("path builder requires LANDMARK_SSSP")),
	}
}
/// Durable additive cost tags.
fn cost_metadata(cost: GraphIndexCost) -> (u8, u32, u32) {
	match cost {
		GraphIndexCost::Hop => (1, 0, 0),
		GraphIndexCost::Weighted => (2, 0, 0),
		GraphIndexCost::Function {
			function,
			version,
		} => (3, function.get(), version.get()),
	}
}
/// Durable direction tag.
fn direction(index: &GraphIndex) -> u8 {
	match index.direction() {
		TraversalDirection::Forward => 0,
		TraversalDirection::Backward => 1,
		TraversalDirection::Both => 2,
	}
}

/// Collapses parallel connectors using their least exact compatible additive
/// cost.
fn projected_costs<Check>(
	graph: &GraphRoot,
	index: &GraphIndex,
	relation: &ProjectedRelation,
	cost: GraphIndexCost,
	scratch_bytes: usize,
	evaluator: Option<&crate::gql::PureFunctionEvaluator<'_>>,
	check: &mut Check,
) -> FerrosGrynnResult<Vec<Number>>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let mut costs = reserve(relation.forward.targets.len(), scratch_bytes)?;
	costs.resize(relation.forward.targets.len(), None);
	for occurrence in graph.occurrences() {
		check()?;
		if index.edge_labels().len() != 0
			&& !index.edge_labels().any(|label| occurrence.label() == Some(label))
		{
			continue;
		}
		let OccurrenceState::Edge {
			topology,
			weight,
			..
		} = occurrence.state()
		else {
			continue;
		};
		let weight =
			match cost {
				GraphIndexCost::Weighted => weight
					.filter(|weight| !matches!(weight, Number::Float(value) if value.is_nan()))
					.unwrap_or(Number::Int(1)),
				GraphIndexCost::Function {
					function,
					version,
				} => evaluator
					.as_ref()
					.ok_or_else(|| invalid("COST evaluator is absent"))?
					.path_cost(function, version, graph.reference(), occurrence.local_id())?,
				GraphIndexCost::Hop => Number::Int(1),
			};
		DistanceInterval::from_number(weight)?;
		for endpoint in topology.endpoints() {
			let Some(source) =
				endpoint.local_id().and_then(|atom| relation.ordinal_by_atom.get(&atom).copied())
			else {
				continue;
			};
			visit_endpoint_steps(
				occurrence.local_id(),
				topology,
				endpoint,
				index.mode(),
				index.direction(),
				|step| {
					check()?;
					let EdgeEndpoint::Local(target) = step.target() else {
						return Ok(());
					};
					let target = *relation
						.ordinal_by_atom
						.get(target)
						.ok_or_else(|| invalid("path cost target is absent"))?;
					let start = relation.forward.offsets[source];
					let end = relation.forward.offsets[source + 1];
					let cell = start
						+ relation.forward.targets[start..end]
							.binary_search(&target)
							.map_err(|_cause| invalid("path cost arc is absent"))?;
					if costs[cell].is_none_or(|before| weight < before) {
						costs[cell] = Some(weight);
					}
					Ok(())
				},
			)?;
		}
	}
	let mut result = reserve(costs.len(), scratch_bytes)?;
	for cost in costs {
		check()?;
		result.push(cost.ok_or_else(|| invalid("projected arc has no cost"))?);
	}
	Ok(result)
}

/// Checks complete distance cells, including canonical infinity and zero
/// bounds.
fn decode_distances<Check>(
	bytes: &[u8],
	cost: u8,
	vertices: usize,
	scratch_bytes: usize,
	check: &mut Check,
) -> FerrosGrynnResult<LandmarkDistanceRows>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	if cost == 1 {
		let mut values = reserve(bytes.len() / 8, scratch_bytes)?;
		for bytes in bytes.as_chunks::<8>().0 {
			check()?;
			let value = u64::from_le_bytes(*bytes);
			if value != u64::MAX && value >= vertices as u64 {
				return Err(invalid("finite hop distance exceeds shortest simple path length"));
			}
			values.push(value);
		}
		Ok(LandmarkDistanceRows::Hops(values))
	} else {
		let mut values = reserve(bytes.len() / 16, scratch_bytes)?;
		for bytes in bytes.as_chunks::<16>().0 {
			check()?;
			let lower = f64::from_le_bytes([
				bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
			]);
			let upper = f64::from_le_bytes([
				bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
				bytes[15],
			]);
			values.push(DistanceInterval::new(lower, upper)?);
		}
		Ok(LandmarkDistanceRows::Intervals(values))
	}
}
/// Fallible allocation after aggregate admission.
fn reserve<T>(count: usize, scratch_bytes: usize) -> FerrosGrynnResult<Vec<T>> {
	let mut values = Vec::new();
	values
		.try_reserve_exact(count)
		.map_err(|cause| resource(scratch_bytes).with_technical_source(cause))?;
	Ok(values)
}
/// Typed bounded workspace failure.
fn resource(bytes: usize) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded("landmark path scratch bytes", bytes as u64)
}
/// Typed physical or semantic corruption failure.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests;
