//! Snapshot neighbourhood payload and its checked physical section adapter.

use super::{
	NeighbourhoodBuildLimits,
	NeighbourhoodLabels,
	NeighbourhoodLists,
	build_neighbourhood_labels,
};
use crate::{
	datastore::{
		DatabaseRoot,
		GraphIndex,
		GraphIndexDefinition,
		GraphRoot,
		NeighbourhoodAlgorithm,
		graph_index_relation::build_graph_index_relation,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomKind,
		ProjectedRelation,
		RelationCsr,
		TraversalDirection,
		TraversalSemantics,
	},
	math::HyperLogLog,
	storage::{
		DerivedSectionKind,
		GraphArtifactIdentity,
		NeighbourhoodHeader,
		NeighbourhoodLayout,
		NeighbourhoodTag,
	},
	values::storage::DiskAtomRef,
};

/// One immutable graph-local projection and its exact/approximate payload.
#[derive(Debug)]
pub struct NeighbourhoodArtifact {
	/// Complete page projection including canonical definition bytes.
	pub layout: NeighbourhoodLayout,
	/// Checked vertex and incidence views used for landmark refinement.
	pub relation: ProjectedRelation,
	/// Exact lists or approximate sketches, never interchanged by consumers.
	pub labels: NeighbourhoodLabels,
	/// Complete semantic bytes disambiguating fingerprint collisions.
	pub descriptor: Vec<u8>,
}

impl NeighbourhoodArtifact {
	/// Restores complete checked sections against the captured logical
	/// snapshot.
	///
	/// # Errors
	/// Rejects ownership, definition, vertex/CSR/list/register references or
	/// scratch.
	pub fn restore<Check>(
		graph: &GraphRoot,
		index: &GraphIndex,
		layout: NeighbourhoodLayout,
		data: [Vec<u8>; 8],
		scratch_bytes: usize,
		check: &mut Check,
	) -> FerrosGrynnResult<Self>
	where
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		check()?;
		layout.admit_decode(scratch_bytes)?;
		let header = layout.header;
		let descriptor = index.descriptor_with_limit(scratch_bytes)?;
		let GraphIndexDefinition::Neighbourhood {
			radius,
			algorithm,
		} = *index.definition()
		else {
			return Err(invalid("neighbourhood artifact belongs to another definition family"));
		};
		let compatible = match algorithm {
			NeighbourhoodAlgorithm::Auto | NeighbourhoodAlgorithm::Exact => {
				header.algorithm == NeighbourhoodTag::Exact
			}
			NeighbourhoodAlgorithm::Sketch(precision) => {
				header.algorithm == NeighbourhoodTag::Sketch
					&& header.parameter == u32::from(precision)
			}
			NeighbourhoodAlgorithm::Landmark(count) => {
				header.algorithm == NeighbourhoodTag::Landmark
					&& header.parameter == u32::from(count)
			}
			NeighbourhoodAlgorithm::OnDemand => false,
		};
		if !compatible
			|| header.identity.index != index.id().map_or(0, |id| id.get())
			|| header.identity.version != index.version().get()
			|| Some(graph.reference().id()) != index.graph()
			|| header.identity.graph != graph.reference().id().get()
			|| header.identity.revision != index.build_revision()
			|| header.identity.fingerprint != rapidhash::v3::rapidhash_v3(&descriptor)
			|| data[7] != descriptor
			|| u32::from(header.radius) != radius
			|| Some(header.max_bytes) != index.max_bytes()
			|| header.node_capacity != graph.slot_capacity(AtomKind::Node) as u64
			|| header.edge_capacity != graph.slot_capacity(AtomKind::Edge) as u64
			|| header.mode != u8::from(index.mode() == TraversalSemantics::Atom)
			|| header.direction
				!= match index.direction() {
					TraversalDirection::Forward => 0,
					TraversalDirection::Backward => 1,
					TraversalDirection::Both => 2,
				}
			|| header.vertices
				!= graph
					.occurrences()
					.filter(|occurrence| header.mode == 1 || occurrence.local_id().is_node())
					.count() as u64
		{
			return Err(invalid("neighbourhood artifact and captured definition differ"));
		}
		for (section, bytes) in layout.sections.iter().zip(&data) {
			if section.count.checked_mul(u64::from(section.width)) != Some(bytes.len() as u64) {
				return Err(invalid("neighbourhood section length differs from metadata"));
			}
		}
		drop(descriptor);
		let [vertex_bytes, offsets, targets, centers, row_offsets, members, registers, descriptor] =
			data;
		let mut vertices = reserve(vertex_bytes.len() / 8, scratch_bytes)?;
		let mut previous = None;
		for bytes in vertex_bytes.as_chunks::<8>().0 {
			check()?;
			let reference = DiskAtomRef::from_raw(u64::from_le_bytes(*bytes))?;
			let slot = usize::try_from(reference.slot()).map_err(|cause| {
				invalid("neighbourhood vertex slot exceeds platform").with_technical_source(cause)
			})?;
			let generation = graph
				.slot_generation(reference.kind(), slot)
				.ok_or_else(|| invalid("neighbourhood vertex slot is absent"))?;
			let atom = AtomId::from_parts(reference.kind(), reference.slot(), generation)?;
			let occurrence =
				graph.occurrence(atom).filter(|_| header.mode == 1 || atom.is_node()).ok_or_else(
					|| invalid("neighbourhood vertex is absent from selected projection"),
				)?;
			let global = occurrence.global_id().raw();
			if previous.is_some_and(|previous| previous >= global) {
				return Err(invalid(
					"neighbourhood vertices are not canonical distinct identities",
				));
			}
			previous = Some(global);
			vertices.push(atom);
		}
		let relation = ProjectedRelation::from_csr(
			vertices,
			RelationCsr {
				offsets: words(&offsets, scratch_bytes)?,
				targets: words(&targets, scratch_bytes)?,
			},
			header.has_cross_level,
			scratch_bytes,
			check,
		)?;
		let labels = if header.algorithm == NeighbourhoodTag::Sketch {
			let width = 1_usize << header.parameter;
			let mut sketches = reserve(registers.len() / width, scratch_bytes)?;
			for bytes in registers.chunks_exact(width) {
				check()?;
				let mut owned = reserve(width, scratch_bytes)?;
				owned.extend_from_slice(bytes);
				sketches.push(HyperLogLog::from_registers(header.parameter as u8, owned)?);
			}
			NeighbourhoodLabels::Sketch(sketches)
		} else {
			let lists = NeighbourhoodLists {
				centers: words(&centers, scratch_bytes)?,
				offsets: words(&row_offsets, scratch_bytes)?,
				members: words(&members, scratch_bytes)?,
			};
			check_lists(&lists, relation.vertices.len(), usize::from(header.radius), check)?;
			if header.algorithm == NeighbourhoodTag::Exact {
				NeighbourhoodLabels::Exact(lists)
			} else {
				NeighbourhoodLabels::Landmark(lists)
			}
		};
		Ok(Self {
			layout,
			relation,
			labels,
			descriptor,
		})
	}

	/// Builds one projection within physical and workspace budgets.
	///
	/// # Errors
	/// Propagates ownership, relation, cancellation and resource errors.
	pub fn build<Check>(
		database: &DatabaseRoot,
		index: &GraphIndex,
		revision: u64,
		scratch_bytes: usize,
		check: &mut Check,
	) -> FerrosGrynnResult<Option<Self>>
	where
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		let GraphIndexDefinition::Neighbourhood {
			radius,
			algorithm,
		} = *index.definition()
		else {
			return Err(invalid("neighbourhood builder received another definition family"));
		};
		if index.is_on_demand() {
			return Ok(None);
		}
		let graph = index
			.graph()
			.and_then(|graph| database.graph(graph))
			.ok_or_else(|| invalid("neighbourhood definition has no graph"))?;
		let descriptor = index.descriptor_with_limit(scratch_bytes)?;
		let fixed = descriptor.len().checked_add(65_536).ok_or_else(|| resource(scratch_bytes))?;
		let relation_budget =
			scratch_bytes.checked_sub(fixed).ok_or_else(|| resource(scratch_bytes))?;
		let relation = build_graph_index_relation(database, index, relation_budget, check)?;
		let vertices = relation.vertices.len() as u64;
		let (tag, parameter, centers, register_bytes) = match algorithm {
			NeighbourhoodAlgorithm::Sketch(precision) => (
				NeighbourhoodTag::Sketch,
				u32::from(precision),
				0,
				vertices
					.checked_mul(u64::from(radius))
					.and_then(|rows| rows.checked_mul(1_u64 << precision))
					.ok_or_else(|| resource(scratch_bytes))?,
			),
			NeighbourhoodAlgorithm::Landmark(count) => {
				(NeighbourhoodTag::Landmark, u32::from(count), vertices.min(u64::from(count)), 0)
			}
			_ => (NeighbourhoodTag::Exact, 0, vertices, 0),
		};
		let header = NeighbourhoodHeader {
			identity: GraphArtifactIdentity {
				index: index
					.id()
					.ok_or_else(|| invalid("neighbourhood definition has no identity"))?
					.get(),
				version: index.version().get(),
				graph: graph.reference().id().get(),
				revision,
				fingerprint: rapidhash::v3::rapidhash_v3(&descriptor),
			},
			node_capacity: graph.slot_capacity(AtomKind::Node) as u64,
			edge_capacity: graph.slot_capacity(AtomKind::Edge) as u64,
			vertices,
			edges: relation.forward.targets.len() as u64,
			centers,
			members: centers
				.checked_mul(u64::from(radius))
				.ok_or_else(|| resource(scratch_bytes))?,
			register_bytes,
			max_bytes: u64::MAX,
			descriptor_bytes: descriptor.len() as u64,
			parameter,
			radius: u8::try_from(radius).map_err(|cause| {
				invalid("neighbourhood radius exceeds materialized range")
					.with_technical_source(cause)
			})?,
			algorithm: tag,
			direction: match index.direction() {
				TraversalDirection::Forward => 0,
				TraversalDirection::Backward => 1,
				TraversalDirection::Both => 2,
			},
			mode: u8::from(index.mode() == TraversalSemantics::Atom),
			has_cross_level: relation.has_cross_level,
		};
		let initial = NeighbourhoodLayout::new(header)?;
		let common_pages = [0, 1, 2, 7].into_iter().try_fold(1_u64, |pages, section| {
			let section = initial.sections[section];
			pages
				.checked_add(section.count.div_ceil(section.capacity()?))
				.ok_or_else(|| resource(scratch_bytes))
		})?;
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
					.and_then(|edges| bytes.checked_add(edges))
			})
			.and_then(|bytes| bytes.checked_add(fixed))
			.ok_or_else(|| resource(scratch_bytes))?;
		let limits = NeighbourhoodBuildLimits {
			max_bytes: index
				.max_bytes()
				.ok_or_else(|| invalid("materialized neighbourhood requires MAX_BYTES"))?,
			common_pages,
			scratch_bytes,
			live_scratch_bytes: live,
		};
		let labels = build_neighbourhood_labels(&relation, algorithm, radius, limits, check)?;
		let (centers, members) = match &labels {
			NeighbourhoodLabels::Exact(lists) | NeighbourhoodLabels::Landmark(lists) => {
				(lists.centers.len() as u64, lists.members.len() as u64)
			}
			NeighbourhoodLabels::Sketch(_) => (0, 0),
			NeighbourhoodLabels::OnDemand => return Ok(None),
		};
		let layout = NeighbourhoodLayout::new(NeighbourhoodHeader {
			centers,
			members,
			max_bytes: limits.max_bytes,
			..header
		})?;
		Ok(Some(Self {
			layout,
			relation,
			labels,
			descriptor,
		}))
	}

	/// Encodes only a complete element named by the validated layout.
	///
	/// # Errors
	/// Rejects a missing/incompatible section, ordinal or forged payload shape.
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
			.find(|section| section.kind == kind && section.count != 0)
			.ok_or_else(|| invalid("neighbourhood section is absent"))?;
		if ordinal >= section.count || output.len() != usize::from(section.width) {
			return Err(invalid("neighbourhood element ordinal or width is invalid"));
		}
		let ordinal = usize::try_from(ordinal).map_err(|cause| {
			invalid("neighbourhood ordinal exceeds platform").with_technical_source(cause)
		})?;
		let value = match kind {
			DerivedSectionKind::NeighbourhoodVertexMap => {
				let atom = self
					.relation
					.vertices
					.get(ordinal)
					.ok_or_else(|| invalid("neighbourhood vertex is absent"))?;
				DiskAtomRef::from_parts(
					atom.kind().ok_or_else(|| invalid("neighbourhood vertex has no kind"))?,
					atom.slot(),
				)?
				.raw()
			}
			DerivedSectionKind::CondensationCsrOffsets => *self
				.relation
				.forward
				.offsets
				.get(ordinal)
				.ok_or_else(|| invalid("neighbourhood CSR offset is absent"))?
				as u64,
			DerivedSectionKind::CondensationCsrTargets => *self
				.relation
				.forward
				.targets
				.get(ordinal)
				.ok_or_else(|| invalid("neighbourhood CSR target is absent"))?
				as u64,
			DerivedSectionKind::NeighbourhoodCenters
			| DerivedSectionKind::NeighbourhoodOffsets
			| DerivedSectionKind::NeighbourhoodMembers
			| DerivedSectionKind::NeighbourhoodLandmarkMembers => {
				let (NeighbourhoodLabels::Exact(lists) | NeighbourhoodLabels::Landmark(lists)) =
					&self.labels
				else {
					return Err(invalid("neighbourhood list section belongs to another algorithm"));
				};
				let values = match kind {
					DerivedSectionKind::NeighbourhoodCenters => &lists.centers,
					DerivedSectionKind::NeighbourhoodOffsets => &lists.offsets,
					_ => &lists.members,
				};
				*values
					.get(ordinal)
					.ok_or_else(|| invalid("neighbourhood list element is absent"))?
					as u64
			}
			DerivedSectionKind::NeighbourhoodRegisters => {
				let NeighbourhoodLabels::Sketch(sketches) = &self.labels else {
					return Err(invalid(
						"neighbourhood register section belongs to another algorithm",
					));
				};
				let width = 1_usize << self.layout.header.parameter;
				output[0] = *sketches
					.get(ordinal / width)
					.and_then(|sketch| sketch.registers().get(ordinal % width))
					.ok_or_else(|| invalid("neighbourhood register is absent"))?;
				return Ok(());
			}
			DerivedSectionKind::DefinitionDescriptor => {
				output[0] = *self
					.descriptor
					.get(ordinal)
					.ok_or_else(|| invalid("neighbourhood descriptor byte is absent"))?;
				return Ok(());
			}
			_ => return Err(invalid("neighbourhood section kind is incompatible")),
		};
		output.copy_from_slice(&value.to_le_bytes());
		Ok(())
	}
}

/// Validates sorted members, row offsets, seed inclusion and monotone radii.
fn check_lists<Check>(
	lists: &NeighbourhoodLists,
	vertices: usize,
	radius: usize,
	check: &mut Check,
) -> FerrosGrynnResult<()>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	if lists.centers.iter().any(|center| *center >= vertices)
		|| lists.centers.len().checked_mul(radius).and_then(|rows| rows.checked_add(1))
			!= Some(lists.offsets.len())
		|| lists.centers.windows(2).any(|pair| pair[0] >= pair[1])
		|| lists.offsets.first() != Some(&0)
		|| lists.offsets.last() != Some(&lists.members.len())
		|| lists.offsets.windows(2).any(|pair| pair[0] > pair[1] || pair[1] > lists.members.len())
	{
		return Err(invalid("neighbourhood centers or row offsets are invalid"));
	}
	for (ordinal, center) in lists.centers.iter().enumerate() {
		let mut previous: &[usize] = &[];
		for depth in 0..radius {
			check()?;
			let row = ordinal * radius + depth;
			let members = &lists.members[lists.offsets[row]..lists.offsets[row + 1]];
			if members.iter().any(|member| *member >= vertices)
				|| members.windows(2).any(|pair| pair[0] >= pair[1])
				|| members.binary_search(center).is_err()
				|| previous.iter().any(|member| members.binary_search(member).is_err())
			{
				return Err(invalid("neighbourhood row members or radius inclusion are invalid"));
			}
			previous = members;
		}
	}
	Ok(())
}

/// Parses complete checked words with platform-width admission.
fn words(bytes: &[u8], scratch_bytes: usize) -> FerrosGrynnResult<Vec<usize>> {
	let mut words = reserve(bytes.len() / 8, scratch_bytes)?;
	for bytes in bytes.as_chunks::<8>().0 {
		words.push(usize::try_from(u64::from_le_bytes(*bytes)).map_err(|cause| {
			invalid("neighbourhood ordinal exceeds platform").with_technical_source(cause)
		})?);
	}
	Ok(words)
}

/// Fallible allocation after complete artifact workspace admission.
fn reserve<T>(count: usize, scratch_bytes: usize) -> FerrosGrynnResult<Vec<T>> {
	let mut values = Vec::new();
	values
		.try_reserve_exact(count)
		.map_err(|cause| resource(scratch_bytes).with_technical_source(cause))?;
	Ok(values)
}

/// Typed captured-definition or reference failure.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(reason)
}
/// Typed aggregate maintenance admission failure.
fn resource(bytes: usize) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded(
		"neighbourhood build scratch bytes",
		bytes as u64,
	)
}
