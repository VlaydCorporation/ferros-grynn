//! Snapshot artifact construction and the logical-to-physical section adapter.

use super::{
	ReachabilityBuildLimits,
	ReachabilityInterval,
	ReachabilityLabels,
	build_reachability_labels,
	filled,
};
use crate::{
	datastore::{
		DatabaseRoot,
		GraphIndex,
		GraphIndexDefinition,
		GraphRoot,
		ReachabilityAlgorithm,
		graph_index_relation::build_graph_index_relation,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomKind,
		ProjectedCondensation,
		RelationCsr,
		TraversalDirection,
		TraversalSemantics,
	},
	storage::{
		DerivedSectionKind,
		GraphArtifactIdentity,
		ReachabilityHeader,
		ReachabilityLayout,
		ReachabilityTag,
	},
	values::storage::DiskAtomRef,
};

/// One complete immutable artifact, shared by snapshot roots and the page
/// writer.
#[derive(Debug)]
pub struct ReachabilityArtifact {
	/// Exact checked layout including canonical definition and page metadata.
	pub layout: ReachabilityLayout,
	/// Sparse local node slot to SCC; holes retain the NULL sentinel.
	pub node_components: Vec<u64>,
	/// Sparse local edge slot to SCC; NODE mode has only NULL entries.
	pub edge_components: Vec<u64>,
	/// Same condensation used by label construction and exact refinement.
	pub condensation: ProjectedCondensation,
	/// Algorithm-owned labels, never used without a compatible Ready
	/// definition.
	pub labels: ReachabilityLabels,
	/// Complete bytes disambiguate fingerprint collisions.
	pub descriptor: Vec<u8>,
}

impl ReachabilityArtifact {
	/// Builds a single definition with separate artifact and scratch admission.
	/// AUTO returning ON_DEMAND produces no artifact.
	///
	/// # Errors
	/// Returns a definition, relation, cancellation or resource error before
	/// publishing any labels or pages.
	pub fn build<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
		database: &DatabaseRoot,
		index: &GraphIndex,
		revision: u64,
		scratch_bytes: usize,
		check: &mut CheckArg,
	) -> FerrosGrynnResult<Option<Self>> {
		let GraphIndexDefinition::Reachability {
			algorithm,
		} = *index.definition()
		else {
			return Err(FerrosGrynnError::invalid_argument(
				"reachability builder received another index family",
			));
		};
		let graph = index.graph().and_then(|id| database.graph(id)).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("reachability definition has no graph")
		})?;
		let descriptor = index.descriptor_with_limit(scratch_bytes)?;
		let identity = GraphArtifactIdentity {
			index: index
				.id()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("reachability definition has no identity")
				})?
				.get(),
			version: index.version().get(),
			graph: graph.reference().id().get(),
			revision,
			fingerprint: rapidhash::v3::rapidhash_v3(&descriptor),
		};
		let node_count = graph.slot_capacity(AtomKind::Node);
		let edge_count = graph.slot_capacity(AtomKind::Edge);
		let maps_bytes = node_count
			.checked_add(edge_count)
			.and_then(|count| count.checked_mul(8))
			.and_then(|bytes| bytes.checked_add(descriptor.len()))
			.and_then(|bytes| bytes.checked_add(65_536))
			.ok_or_else(|| resource(scratch_bytes))?;
		if maps_bytes > scratch_bytes {
			return Err(resource(scratch_bytes));
		}
		let relation =
			build_graph_index_relation(database, index, scratch_bytes - maps_bytes, check)?;
		let condensation = relation.condensation(scratch_bytes - maps_bytes, check)?;
		let count = condensation.representatives.len();
		let header = ReachabilityHeader {
			identity,
			node_capacity: node_count as u64,
			edge_capacity: edge_count as u64,
			components: count as u64,
			edges: condensation.forward.targets.len() as u64,
			algorithm: ReachabilityTag::TreeCover,
			parameter: 0,
			chains: 0,
			direction: match index.direction() {
				TraversalDirection::Forward => 0,
				TraversalDirection::Backward => 1,
				TraversalDirection::Both => 2,
			},
			mode: u8::from(index.mode() == TraversalSemantics::Atom),
			reversed: false,
			has_cross_level: relation.has_cross_level,
			max_bytes: u64::MAX,
			descriptor_bytes: descriptor.len() as u64,
		};
		let common = ReachabilityLayout::new(header)?;
		let common_pages = common.page_count - (count as u64).div_ceil(1020);
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
			.and_then(|bytes| bytes.checked_add(maps_bytes))
			.ok_or_else(|| resource(scratch_bytes))?;
		let limits = ReachabilityBuildLimits {
			max_bytes: index.max_bytes().unwrap_or(u64::MAX),
			common_pages,
			scratch_bytes,
			live_scratch_bytes: live,
		};
		let labels = build_reachability_labels(
			&condensation,
			graph.reference().kind(),
			algorithm,
			limits,
			check,
		)?;
		let (algorithm, parameter, chains, reversed) = match &labels {
			ReachabilityLabels::TreeCover {
				reversed,
				..
			} => (ReachabilityTag::TreeCover, 0, 0, *reversed),
			ReachabilityLabels::Grail {
				dimensions,
				..
			} => (ReachabilityTag::Grail, u32::from(*dimensions), 0, false),
			ReachabilityLabels::Ferrari {
				max_ranges,
				..
			} => (ReachabilityTag::Ferrari, u32::from(*max_ranges), 0, false),
			ReachabilityLabels::Bfl {
				bytes,
				..
			} => (ReachabilityTag::Bfl, u32::from(*bytes) * 8, 0, false),
			ReachabilityLabels::ChainCover {
				chains,
				..
			} => (ReachabilityTag::ChainCover, 0, *chains as u64, false),
			ReachabilityLabels::OnDemand {
				..
			} => return Ok(None),
		};
		let layout = ReachabilityLayout::new(ReachabilityHeader {
			algorithm,
			parameter,
			chains,
			reversed,
			max_bytes: limits.max_bytes,
			..header
		})?;
		let mut node_components = filled(node_count, u64::MAX, limits)?;
		let mut edge_components = filled(edge_count, u64::MAX, limits)?;
		for (ordinal, atom) in relation.vertices.iter().enumerate() {
			check()?;
			let map = if atom.is_node() {
				&mut node_components
			} else {
				&mut edge_components
			};
			let component = map.get_mut(atom.slot() as usize).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("reachability slot exceeds graph extent")
			})?;
			*component = condensation.component_by_vertex[ordinal] as u64;
		}
		Ok(Some(Self {
			layout,
			node_components,
			edge_components,
			condensation,
			labels,
			descriptor,
		}))
	}

	/// Restores checked sections against the same logical snapshot and
	/// definition.
	///
	/// # Errors
	/// Rejects incompatible metadata, references, CSR, label bounds or
	/// descriptor bytes before constructing a query-visible artifact.
	pub fn restore<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
		graph: &GraphRoot,
		index: &GraphIndex,
		layout: ReachabilityLayout,
		data: [Vec<u8>; 8],
		scratch_bytes: usize,
		check: &mut CheckArg,
	) -> FerrosGrynnResult<Self> {
		layout.admit_decode(scratch_bytes)?;
		let header = layout.header;
		let descriptor = index.descriptor_with_limit(scratch_bytes)?;
		if layout != ReachabilityLayout::new(header)?
			|| data[7] != descriptor
			|| header.identity.fingerprint != rapidhash::v3::rapidhash_v3(&descriptor)
			|| header.identity.index != index.id().map_or(0, |id| id.get())
			|| header.identity.version != index.version().get()
			|| Some(graph.reference().id()) != index.graph()
			|| header.identity.graph != graph.reference().id().get()
			|| header.identity.revision != index.build_revision()
			|| header.node_capacity != graph.slot_capacity(AtomKind::Node) as u64
			|| header.edge_capacity != graph.slot_capacity(AtomKind::Edge) as u64
			|| header.mode != u8::from(index.mode() == TraversalSemantics::Atom)
			|| header.direction
				!= match index.direction() {
					TraversalDirection::Forward => 0,
					TraversalDirection::Backward => 1,
					TraversalDirection::Both => 2,
				}
			|| header.max_bytes != index.max_bytes().unwrap_or(u64::MAX)
		{
			return Err(invalid("reachability artifact and captured definition differ"));
		}
		let GraphIndexDefinition::Reachability {
			algorithm,
		} = *index.definition()
		else {
			return Err(invalid("reachability artifact belongs to another definition family"));
		};
		let compatible = match algorithm {
			ReachabilityAlgorithm::Auto => true,
			ReachabilityAlgorithm::TreeCover => header.algorithm == ReachabilityTag::TreeCover,
			ReachabilityAlgorithm::Grail(parameter) => {
				header.algorithm == ReachabilityTag::Grail
					&& header.parameter == u32::from(parameter)
			}
			ReachabilityAlgorithm::Ferrari(parameter) => {
				header.algorithm == ReachabilityTag::Ferrari
					&& header.parameter == u32::from(parameter)
			}
			ReachabilityAlgorithm::Bfl(parameter) => {
				header.algorithm == ReachabilityTag::Bfl && header.parameter == u32::from(parameter)
			}
			ReachabilityAlgorithm::ChainCover => header.algorithm == ReachabilityTag::ChainCover,
			ReachabilityAlgorithm::OnDemand => false,
		};
		if !compatible
			|| header.algorithm == ReachabilityTag::TreeCover
				&& !matches!(
					graph.reference().kind(),
					crate::graph::GraphKind::Tree
						| crate::graph::GraphKind::RootedTree
						| crate::graph::GraphKind::Forest,
				) {
			return Err(invalid("reachability concrete algorithm is incompatible"));
		}
		for (section, bytes) in layout.sections.iter().zip(&data) {
			if section.count.checked_mul(u64::from(section.width)) != Some(bytes.len() as u64) {
				return Err(invalid("reachability section byte length differs from its header"));
			}
		}
		let limits = ReachabilityBuildLimits {
			max_bytes: header.max_bytes,
			common_pages: 0,
			scratch_bytes,
			live_scratch_bytes: 0,
		};
		let count = usize::try_from(header.components).map_err(|cause| {
			invalid("reachability component count exceeds platform").with_technical_source(cause)
		})?;
		let [nodes, edges, reps, offsets, targets, label_data, auxiliary, descriptor] = data;
		let node_components = words(&nodes, limits)?;
		let edge_components = words(&edges, limits)?;
		let mut minimum = filled(count, None::<(u64, AtomId)>, limits)?;
		for (kind, map) in [(AtomKind::Node, &node_components), (AtomKind::Edge, &edge_components)]
		{
			for (slot, component) in map.iter().enumerate() {
				check()?;
				let occurrence = graph
					.slot_generation(kind, slot)
					.map(|generation| AtomId::from_parts(kind, slot as u64, generation))
					.transpose()?
					.and_then(|atom| graph.occurrence(atom));
				let selected = kind == AtomKind::Node || header.mode == 1;
				if let Some(occurrence) = occurrence.filter(|_| selected) {
					let destination = minimum
						.get_mut(usize::try_from(*component).map_err(|cause| {
							invalid("reachability component reference exceeds platform")
								.with_technical_source(cause)
						})?)
						.ok_or_else(|| {
							invalid("reachability occurrence component is out of bounds")
						})?;
					let value = (occurrence.global_id().raw(), occurrence.local_id());
					if destination.is_none_or(|current| value.0 < current.0) {
						*destination = Some(value);
					}
				} else if *component != u64::MAX {
					return Err(invalid("reachability hole or excluded edge has a component"));
				}
			}
		}
		let mut representatives = filled(count, AtomId::from_parts(AtomKind::Node, 0, 1)?, limits)?;
		let mut previous = None;
		for (component, bytes) in reps.as_chunks::<8>().0.iter().enumerate() {
			check()?;
			let atom = DiskAtomRef::from_raw(number(bytes, 0))?;
			let (global, representative) =
				minimum[component].ok_or_else(|| invalid("reachability component is empty"))?;
			if previous.is_some_and(|previous| previous >= global)
				|| atom.slot() != representative.slot()
				|| Some(atom.kind()) != representative.kind()
			{
				return Err(invalid("reachability SCC representative is noncanonical"));
			}
			representatives[component] = representative;
			previous = Some(global);
		}
		let convert = |bytes: &[u8]| -> FerrosGrynnResult<Vec<usize>> {
			let mut result = Vec::new();
			result
				.try_reserve_exact(bytes.len() / 8)
				.map_err(|cause| resource(scratch_bytes).with_technical_source(cause))?;
			for bytes in bytes.as_chunks::<8>().0 {
				result.push(usize::try_from(number(bytes, 0)).map_err(|cause| {
					invalid("reachability CSR reference exceeds platform")
						.with_technical_source(cause)
				})?);
			}
			Ok(result)
		};
		let condensation = ProjectedCondensation::from_csr(
			Vec::new(),
			representatives,
			RelationCsr {
				offsets: convert(&offsets)?,
				targets: convert(&targets)?,
			},
			scratch_bytes,
			check,
		)?;
		let pairs = |bytes: &[u8]| -> FerrosGrynnResult<Vec<(u64, u64)>> {
			let mut result = Vec::new();
			result
				.try_reserve_exact(bytes.len() / 16)
				.map_err(|cause| resource(scratch_bytes).with_technical_source(cause))?;
			for bytes in bytes.as_chunks::<16>().0 {
				result.push((number(bytes, 0), number(bytes, 8)));
			}
			Ok(result)
		};
		let check_interval = |lower: u64, upper: u64| -> FerrosGrynnResult<()> {
			if lower == 0 || lower > upper || upper > header.components {
				return Err(invalid("reachability postorder interval is invalid"));
			}
			Ok(())
		};
		let labels = match header.algorithm {
			ReachabilityTag::TreeCover | ReachabilityTag::Grail => {
				let intervals = pairs(&label_data)?;
				for (lower, upper) in &intervals {
					check_interval(*lower, *upper)?;
				}
				if header.algorithm == ReachabilityTag::TreeCover {
					ReachabilityLabels::TreeCover {
						intervals,
						reversed: header.reversed,
					}
				} else {
					ReachabilityLabels::Grail {
						intervals,
						dimensions: header.parameter as u8,
					}
				}
			}
			ReachabilityTag::Ferrari => {
				let postorder = words(&auxiliary, limits)?;
				let mut seen = filled(count, false, limits)?;
				for rank in &postorder {
					if *rank == 0 || *rank > header.components || seen[*rank as usize - 1] {
						return Err(invalid("FERRARI postorder is not a permutation"));
					}
					seen[*rank as usize - 1] = true;
				}
				let mut intervals = Vec::new();
				intervals
					.try_reserve_exact(label_data.len() / 24)
					.map_err(|cause| resource(scratch_bytes).with_technical_source(cause))?;
				for bytes in label_data.as_chunks::<24>().0 {
					let range = ReachabilityInterval {
						lower: number(bytes, 0),
						upper: number(bytes, 8),
						kind: bytes[16],
					};
					if bytes[17..].iter().any(|byte| *byte != 0) || range.kind > 2 {
						return Err(invalid("FERRARI tag or reserved bytes are invalid"));
					}
					if range.kind == 2 {
						if range.lower != u64::MAX || range.upper != 0 {
							return Err(invalid("FERRARI unused interval is noncanonical"));
						}
					} else {
						check_interval(range.lower, range.upper)?;
					}
					intervals.push(range);
				}
				for row in intervals.chunks_exact(header.parameter as usize) {
					if row.windows(2).any(|pair| {
						pair[0].kind == 2 && pair[1].kind != 2
							|| pair[0].kind != 2
								&& pair[1].kind != 2
								&& pair[0].upper >= pair[1].lower
					}) {
						return Err(invalid("FERRARI ranges are noncanonical"));
					}
				}
				ReachabilityLabels::Ferrari {
					max_ranges: header.parameter as u8,
					postorder,
					intervals,
				}
			}
			ReachabilityTag::Bfl => ReachabilityLabels::Bfl {
				bytes: (header.parameter / 8) as u16,
				filters: label_data,
			},
			ReachabilityTag::ChainCover => {
				let coordinates = pairs(&label_data)?;
				let cells = words(&auxiliary, limits)?;
				if coordinates.iter().any(|(chain, position)| {
					*chain >= header.chains || *position >= header.components
				}) || cells.iter().any(|cell| *cell != u64::MAX && *cell >= header.components)
				{
					return Err(invalid("CHAIN_COVER reference is outside its extent"));
				}
				ReachabilityLabels::ChainCover {
					coordinates,
					cells,
					chains: header.chains as usize,
				}
			}
		};
		Ok(Self {
			layout,
			node_components,
			edge_components,
			condensation,
			labels,
			descriptor,
		})
	}

	/// Packs one logical element into its exact-width physical representation.
	///
	/// # Errors
	/// Rejects a foreign section, incorrect width or ordinal without a partial
	/// physical element. The complete writer keeps its generation private.
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
			.ok_or_else(|| invalid("reachability encoder section is absent"))?;
		if ordinal >= section.count || output.len() != usize::from(section.width) {
			return Err(invalid("reachability encoder width or ordinal is invalid"));
		}
		let ordinal = usize::try_from(ordinal).map_err(|cause| {
			invalid("reachability ordinal exceeds platform").with_technical_source(cause)
		})?;
		match kind {
			DerivedSectionKind::NodeComponentMap => {
				output.copy_from_slice(&self.node_components[ordinal].to_le_bytes());
			}
			DerivedSectionKind::EdgeComponentMap => {
				output.copy_from_slice(&self.edge_components[ordinal].to_le_bytes());
			}
			DerivedSectionKind::SccRepresentativeMap => {
				let atom = self.condensation.representatives[ordinal];
				output.copy_from_slice(
					&DiskAtomRef::from_parts(
						atom.kind().ok_or_else(|| {
							invalid("reachability representative has no atom kind")
						})?,
						atom.slot(),
					)?
					.raw()
					.to_le_bytes(),
				);
			}
			DerivedSectionKind::CondensationCsrOffsets => output.copy_from_slice(
				&(self.condensation.forward.offsets[ordinal] as u64).to_le_bytes(),
			),
			DerivedSectionKind::CondensationCsrTargets => output.copy_from_slice(
				&(self.condensation.forward.targets[ordinal] as u64).to_le_bytes(),
			),
			DerivedSectionKind::DefinitionDescriptor => output[0] = self.descriptor[ordinal],
			_ => match &self.labels {
				ReachabilityLabels::TreeCover {
					intervals,
					..
				}
				| ReachabilityLabels::Grail {
					intervals,
					..
				} => pack_pair(output, intervals[ordinal]),
				ReachabilityLabels::Ferrari {
					intervals,
					postorder,
					..
				} => {
					if kind == DerivedSectionKind::PostorderRanks {
						output.copy_from_slice(&postorder[ordinal].to_le_bytes());
					} else {
						let range = intervals[ordinal];
						pack_pair(&mut output[..16], (range.lower, range.upper));
						output[16] = range.kind;
						output[17..].fill(0);
					}
				}
				ReachabilityLabels::Bfl {
					bytes,
					filters,
				} => {
					let start = ordinal * usize::from(*bytes) * 2;
					output.copy_from_slice(&filters[start..start + output.len()]);
				}
				ReachabilityLabels::ChainCover {
					coordinates,
					cells,
					..
				} => {
					if kind == DerivedSectionKind::ChainCoordinates {
						pack_pair(output, coordinates[ordinal]);
					} else {
						output.copy_from_slice(&cells[ordinal].to_le_bytes());
					}
				}
				ReachabilityLabels::OnDemand {
					..
				} => return Err(invalid("ON_DEMAND cannot encode physical labels")),
			},
		}
		Ok(())
	}
}

/// Encodes a fixed pair without platform-dependent tuple serialization.
fn pack_pair(output: &mut [u8], value: (u64, u64)) {
	output[..8].copy_from_slice(&value.0.to_le_bytes());
	output[8..16].copy_from_slice(&value.1.to_le_bytes());
}
/// Decodes fixed words after complete section length admission.
fn words(bytes: &[u8], limits: ReachabilityBuildLimits) -> FerrosGrynnResult<Vec<u64>> {
	let mut values = filled(bytes.len() / 8, 0, limits)?;
	for (value, bytes) in values.iter_mut().zip(bytes.as_chunks::<8>().0) {
		*value = number(bytes, 0);
	}
	Ok(values)
}
/// Reads a word from a previously validated complete element.
fn number(bytes: &[u8], at: usize) -> u64 {
	u64::from_le_bytes([
		bytes[at],
		bytes[at + 1],
		bytes[at + 2],
		bytes[at + 3],
		bytes[at + 4],
		bytes[at + 5],
		bytes[at + 6],
		bytes[at + 7],
	])
}
/// Typed builder scratch admission failure.
fn resource(bytes: usize) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded("reachability scratch bytes", bytes as u64)
}
/// Typed logical-to-physical artifact mismatch.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
