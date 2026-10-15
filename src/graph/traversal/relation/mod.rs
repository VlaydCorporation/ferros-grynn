//! Bounded local incidence projections and deterministic SCC condensation.

use std::collections::{BTreeMap, BTreeSet};

use super::{connectivity::strong_components_with_steps, index::visit_endpoint_steps};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, EdgeTopology, TraversalDirection, TraversalSemantics},
};

/// Conservative aggregate admission includes SCC stacks, hash tables, CSR
/// arrays, maps and overlapping intermediate/output allocations.
const VERTEX_SCRATCH_BYTES: usize = 512;
/// Includes pair-vector growth, forward/reverse CSR and condensation pairs.
const ARC_SCRATCH_BYTES: usize = 128;
/// Fixed builder metadata and empty CSR sentinels.
const FIXED_SCRATCH_BYTES: usize = 1024;

/// Compact ordinal adjacency shared by relation and condensation builders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationCsr {
	/// Exclusive row offsets, including the final target count.
	pub offsets: Vec<usize>,
	/// Sorted distinct target ordinals in each row.
	pub targets: Vec<usize>,
}

impl RelationCsr {
	/// Validates sorted distinct rows and all offsets/references, allowing
	/// cycles and self arcs in the general incidence relation.
	///
	/// # Errors
	/// Rejects malformed CSR or propagates cooperative cancellation.
	pub fn validate<Check>(&self, vertices: usize, check: &mut Check) -> FerrosGrynnResult<()>
	where
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		check()?;
		if self.offsets.len()
			!= vertices.checked_add(1).ok_or_else(|| invalid("CSR vertex extent overflows"))?
			|| self.offsets.first() != Some(&0)
			|| self.offsets.last() != Some(&self.targets.len())
			|| self.offsets.windows(2).any(|row| row[0] > row[1] || row[1] > self.targets.len())
		{
			return Err(invalid("relation CSR offsets are invalid"));
		}
		for source in 0..vertices {
			check()?;
			let row = &self.targets[self.offsets[source]..self.offsets[source + 1]];
			if row.iter().any(|target| *target >= vertices)
				|| row.windows(2).any(|pair| pair[0] >= pair[1])
			{
				return Err(invalid("relation CSR targets are noncanonical"));
			}
		}
		Ok(())
	}

	/// Returns neighbours of one checked ordinal without allocating.
	pub fn neighbours(&self, vertex: usize) -> impl Iterator<Item = usize> + '_ {
		self.offsets
			.get(vertex)
			.zip(vertex.checked_add(1).and_then(|next| self.offsets.get(next)))
			.into_iter()
			.flat_map(|(start, end)| self.targets[*start..*end].iter().copied())
	}
}

/// One graph-local NODE/ATOM projection. Qualified endpoints are not flattened.
#[derive(Debug)]
pub struct ProjectedRelation {
	/// Canonical durable identity ordering, including isolated local atoms.
	pub vertices: Vec<AtomId>,
	/// Logical atom identity to dense ordinal.
	pub ordinal_by_atom: BTreeMap<AtomId, usize>,
	/// Selected direction and connector-filter relation.
	pub forward: RelationCsr,
	/// Reverse of the selected relation, used by the shared SCC kernel.
	pub reverse: RelationCsr,
	/// Consumers must retain exact scoped traversal for cross-level requests.
	pub has_cross_level: bool,
}

/// Deterministic component identity and acyclic graph of one local relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedCondensation {
	/// Dense vertex ordinal to canonical component ordinal.
	pub component_by_vertex: Vec<usize>,
	/// Atom with the smallest supplied durable identity in each component.
	pub representatives: Vec<AtomId>,
	/// Distinct forward component arcs.
	pub forward: RelationCsr,
	/// Distinct reverse component arcs.
	pub reverse: RelationCsr,
	/// Deterministic Kahn order, breaking ties by canonical component ordinal.
	pub topological_order: Vec<usize>,
}

impl ProjectedRelation {
	/// Restores a general checked relation and its ordinal/reverse views.
	///
	/// # Errors
	/// Rejects duplicate vertices, malformed CSR, scratch or cancellation.
	pub fn from_csr<Check>(
		vertices: Vec<AtomId>,
		forward: RelationCsr,
		has_cross_level: bool,
		scratch_bytes: usize,
		check: &mut Check,
	) -> FerrosGrynnResult<Self>
	where
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		admit(vertices.len(), forward.targets.len(), scratch_bytes)?;
		forward.validate(vertices.len(), check)?;
		let ordinal_by_atom = vertices
			.iter()
			.enumerate()
			.map(|(ordinal, atom)| (*atom, ordinal))
			.collect::<BTreeMap<_, _>>();
		if ordinal_by_atom.len() != vertices.len() {
			return Err(invalid("relation has duplicate vertices"));
		}
		let mut pairs = Vec::new();
		pairs
			.try_reserve_exact(forward.targets.len())
			.map_err(|cause| resource(scratch_bytes).with_technical_source(cause))?;
		for source in 0..vertices.len() {
			check()?;
			pairs.extend(forward.neighbours(source).map(|target| (target, source)));
		}
		pairs.sort_unstable();
		let reverse = build_csr(vertices.len(), &pairs, scratch_bytes)?;
		Ok(Self {
			vertices,
			ordinal_by_atom,
			forward,
			reverse,
			has_cross_level,
		})
	}

	/// Builds a local relation from the existing incidence representation.
	/// Callers filter connector labels and supply stable durable vertex keys;
	/// the datastore adapter uses GlobalAtomId, independent of local slot
	/// order.
	///
	/// # Errors
	/// Returns resource/cancellation errors before exceeding scratch admission,
	/// or a structural error for missing or repeated local vertices.
	pub fn build<
		'a,
		VerticesArg: Iterator<Item = AtomId> + Clone,
		TopologiesArg: Iterator<Item = (AtomId, &'a EdgeTopology)>,
		CanonicalKeyArg: FnMut(AtomId) -> FerrosGrynnResult<u64>,
		CheckArg: FnMut() -> FerrosGrynnResult<()>,
	>(
		vertices: VerticesArg,
		topologies: TopologiesArg,
		mode: TraversalSemantics,
		direction: TraversalDirection,
		scratch_bytes: usize,
		mut canonical_key: CanonicalKeyArg,
		check: &mut CheckArg,
	) -> FerrosGrynnResult<Self> {
		let count = vertices
			.clone()
			.filter(|atom| mode == TraversalSemantics::Atom || atom.is_node())
			.count();
		admit(count, 0, scratch_bytes)?;
		let mut ordered = Vec::new();
		ordered
			.try_reserve_exact(count)
			.map_err(|error| resource(scratch_bytes).with_technical_source(error))?;
		for atom in vertices.filter(|atom| mode == TraversalSemantics::Atom || atom.is_node()) {
			check()?;
			if ordered.len() == count {
				return Err(invalid("projected vertex iterator changed after counting"));
			}
			ordered.push((canonical_key(atom)?, atom));
		}
		ordered.sort_unstable();
		if ordered.windows(2).any(|pair| pair[0].0 == pair[1].0) {
			return Err(invalid("duplicate durable vertex identity in projected relation"));
		}
		let atoms = ordered.into_iter().map(|(_, atom)| atom).collect::<Vec<_>>();
		let ordinals = atoms
			.iter()
			.enumerate()
			.map(|(ordinal, atom)| (*atom, ordinal))
			.collect::<BTreeMap<_, _>>();
		if ordinals.len() != atoms.len() {
			return Err(invalid("duplicate local vertex in projected relation"));
		}
		let mut pairs = Vec::new();
		let mut has_cross_level = false;
		for (connector, topology) in topologies {
			check()?;
			has_cross_level |= topology.endpoints().any(|endpoint| endpoint.is_cross_level());
			for endpoint in topology.endpoints() {
				let Some(source) = endpoint
					.local_id()
					.filter(|atom| mode == TraversalSemantics::Atom || atom.is_node())
				else {
					continue;
				};
				let source = *ordinals
					.get(&source)
					.ok_or_else(|| invalid("source vertex is absent from projected relation"))?;
				visit_endpoint_steps(connector, topology, endpoint, mode, direction, |step| {
					check()?;
					let Some(target) = step.target().local_id() else {
						return Ok(());
					};
					let target = *ordinals.get(&target).ok_or_else(|| {
						invalid("target vertex is absent from projected relation")
					})?;
					let next_count =
						pairs.len().checked_add(1).ok_or_else(|| resource(scratch_bytes))?;
					admit(count, next_count, scratch_bytes)?;
					pairs
						.try_reserve(1)
						.map_err(|error| resource(scratch_bytes).with_technical_source(error))?;
					pairs.push((source, target));
					Ok(())
				})?;
			}
		}
		pairs.sort_unstable();
		pairs.dedup();
		let forward = build_csr(count, &pairs, scratch_bytes)?;
		for pair in &mut pairs {
			*pair = (pair.1, pair.0);
		}
		pairs.sort_unstable();
		let reverse = build_csr(count, &pairs, scratch_bytes)?;
		Ok(Self {
			vertices: atoms,
			ordinal_by_atom: ordinals,
			forward,
			reverse,
			has_cross_level,
		})
	}

	/// Runs the shared SCC kernel, then canonicalizes component identities.
	///
	/// # Errors
	/// Propagates scratch/cancellation errors and rejects inconsistent
	/// relations.
	pub fn condensation<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
		&self,
		scratch_bytes: usize,
		check: &mut CheckArg,
	) -> FerrosGrynnResult<ProjectedCondensation> {
		admit(self.vertices.len(), self.forward.targets.len(), scratch_bytes)?;
		let mut components = strong_components_with_steps(
			0..self.vertices.len(),
			|vertex, direction| {
				let csr = if direction == TraversalDirection::Forward {
					&self.forward
				} else {
					&self.reverse
				};
				csr.neighbours(vertex)
			},
			check,
		)?;
		for component in &mut components {
			component.sort_unstable();
		}
		components.sort_unstable_by_key(|component| component.first().copied());
		let mut component_by_vertex = vec![usize::MAX; self.vertices.len()];
		let mut representatives = Vec::new();
		representatives
			.try_reserve_exact(components.len())
			.map_err(|error| resource(scratch_bytes).with_technical_source(error))?;
		for (ordinal, component) in components.iter().enumerate() {
			check()?;
			let representative = component
				.first()
				.and_then(|vertex| self.vertices.get(*vertex))
				.copied()
				.ok_or_else(|| invalid("SCC kernel produced an empty or foreign component"))?;
			representatives.push(representative);
			for vertex in component {
				component_by_vertex[*vertex] = ordinal;
			}
		}
		let mut pairs = Vec::new();
		for source in 0..self.vertices.len() {
			check()?;
			for target in self.forward.neighbours(source) {
				let pair = (component_by_vertex[source], component_by_vertex[target]);
				if pair.0 != pair.1 {
					pairs
						.try_reserve(1)
						.map_err(|error| resource(scratch_bytes).with_technical_source(error))?;
					pairs.push(pair);
				}
			}
		}
		pairs.sort_unstable();
		pairs.dedup();
		let forward = build_csr(components.len(), &pairs, scratch_bytes)?;
		drop(pairs);
		ProjectedCondensation::from_csr(
			component_by_vertex,
			representatives,
			forward,
			scratch_bytes,
			check,
		)
	}
}

impl ProjectedCondensation {
	/// Restores a checked condensation and derives its reverse/topological
	/// views. The same boundary is used by the SCC builder and durable
	/// artifact reader.
	///
	/// # Errors
	/// Rejects malformed references, noncanonical CSR, cycles or scratch limits
	/// before exposing the relation to reachability labels.
	pub fn from_csr<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
		component_by_vertex: Vec<usize>,
		representatives: Vec<AtomId>,
		forward: RelationCsr,
		scratch_bytes: usize,
		check: &mut CheckArg,
	) -> FerrosGrynnResult<Self> {
		let count = representatives.len();
		admit(count, forward.targets.len(), scratch_bytes)?;
		if forward.offsets.len() != count.checked_add(1).ok_or_else(|| resource(scratch_bytes))?
			|| forward.offsets.first() != Some(&0)
			|| forward.offsets.last() != Some(&forward.targets.len())
			|| forward
				.offsets
				.windows(2)
				.any(|row| row[0] > row[1] || row[1] > forward.targets.len())
			|| component_by_vertex.iter().any(|component| *component >= count)
		{
			return Err(invalid("condensation CSR offsets or components are invalid"));
		}
		let mut offsets = Vec::new();
		offsets
			.try_reserve_exact(count + 1)
			.map_err(|error| resource(scratch_bytes).with_technical_source(error))?;
		offsets.resize(count + 1, 0_usize);
		for source in 0..count {
			check()?;
			let row = &forward.targets[forward.offsets[source]..forward.offsets[source + 1]];
			if row.iter().any(|target| *target >= count || *target == source)
				|| row.windows(2).any(|pair| pair[0] >= pair[1])
			{
				return Err(invalid("condensation CSR targets are noncanonical"));
			}
			for target in row {
				offsets[*target + 1] += 1;
			}
		}
		for component in 1..offsets.len() {
			offsets[component] += offsets[component - 1];
		}
		let mut targets = Vec::new();
		targets
			.try_reserve_exact(forward.targets.len())
			.map_err(|error| resource(scratch_bytes).with_technical_source(error))?;
		targets.resize(forward.targets.len(), 0_usize);
		let mut cursor = Vec::new();
		cursor
			.try_reserve_exact(offsets.len())
			.map_err(|error| resource(scratch_bytes).with_technical_source(error))?;
		cursor.extend_from_slice(&offsets);
		for source in 0..count {
			check()?;
			for target in forward.neighbours(source) {
				targets[cursor[target]] = source;
				cursor[target] += 1;
			}
		}
		let reverse = RelationCsr {
			offsets,
			targets,
		};
		let mut indegree =
			reverse.offsets.windows(2).map(|pair| pair[1] - pair[0]).collect::<Vec<_>>();
		let mut ready = indegree
			.iter()
			.enumerate()
			.filter(|(_, degree)| **degree == 0)
			.map(|(vertex, _)| vertex)
			.collect::<BTreeSet<_>>();
		let mut order = Vec::new();
		order
			.try_reserve_exact(count)
			.map_err(|error| resource(scratch_bytes).with_technical_source(error))?;
		while let Some(source) = ready.pop_first() {
			check()?;
			order.push(source);
			for target in forward.neighbours(source) {
				indegree[target] = indegree[target]
					.checked_sub(1)
					.ok_or_else(|| invalid("condensation indegree underflows"))?;
				if indegree[target] == 0 {
					ready.insert(target);
				}
			}
		}
		if order.len() != count {
			return Err(invalid("condensation contains a cycle"));
		}
		Ok(ProjectedCondensation {
			component_by_vertex,
			representatives,
			forward,
			reverse,
			topological_order: order,
		})
	}
}

/// Packs sorted distinct arcs after aggregate admission, with fallible vectors.
fn build_csr(
	vertices: usize,
	pairs: &[(usize, usize)],
	budget: usize,
) -> FerrosGrynnResult<RelationCsr> {
	let count = vertices.checked_add(1).ok_or_else(|| resource(budget))?;
	let mut offsets = Vec::new();
	offsets
		.try_reserve_exact(count)
		.map_err(|error| resource(budget).with_technical_source(error))?;
	offsets.resize(count, 0_usize);
	let mut targets = Vec::new();
	targets
		.try_reserve_exact(pairs.len())
		.map_err(|error| resource(budget).with_technical_source(error))?;
	for (source, target) in pairs {
		if *source >= vertices || *target >= vertices {
			return Err(invalid("CSR arc refers outside its vertex extent"));
		}
		offsets[*source + 1] =
			offsets[*source + 1].checked_add(1).ok_or_else(|| resource(budget))?;
		targets.push(*target);
	}
	for index in 1..offsets.len() {
		offsets[index] =
			offsets[index].checked_add(offsets[index - 1]).ok_or_else(|| resource(budget))?;
	}
	Ok(RelationCsr {
		offsets,
		targets,
	})
}

/// Rejects aggregate scratch growth before allocating or inserting a pair.
fn admit(vertices: usize, arcs: usize, budget: usize) -> FerrosGrynnResult<()> {
	let bytes = vertices
		.checked_mul(VERTEX_SCRATCH_BYTES)
		.and_then(|bytes| {
			arcs.checked_mul(ARC_SCRATCH_BYTES).and_then(|arcs| bytes.checked_add(arcs))
		})
		.and_then(|bytes| bytes.checked_add(FIXED_SCRATCH_BYTES))
		.ok_or_else(|| resource(budget))?;
	if bytes > budget {
		return Err(resource(budget));
	}
	Ok(())
}

/// Typed admission failure with the caller's effective scratch ceiling.
fn resource(budget: usize) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded(
		"graph index relation scratch",
		budget as u64,
	)
}
/// Structural errors belong to the existing graph diagnostics.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("graph index relation", reason)
}

#[cfg(test)]
mod tests;
