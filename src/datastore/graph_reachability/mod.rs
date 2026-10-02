//! Reachability labels over the shared immutable condensation relation.

use super::ReachabilityAlgorithm;
mod artifact;
pub use artifact::ReachabilityArtifact;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{GraphKind, ProjectedCondensation, RelationCsr},
};

/// Physical page accounting includes all section headers and padding.
const PAGE_BYTES: u64 = 16_384;
/// The approved fixed-array section begins at byte 64.
const ARRAY_BYTES: u64 = PAGE_BYTES - 64;

/// Only definitive labels may terminate an unbounded reachability query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReachabilityDecision {
	/// An exact label proves reachability.
	Yes,
	/// A sound negative label proves absence.
	No,
	/// Exact bounded traversal must refine this answer.
	Maybe,
}

/// Resource authority supplied by the artifact builder before any label write.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReachabilityBuildLimits {
	/// Complete artifact ceiling, including common pages and metadata.
	pub(super) max_bytes: u64,
	/// Header, maps and condensation sections already projected by storage.
	pub(super) common_pages: u64,
	/// Separate effective maintenance scratch ceiling.
	pub(super) scratch_bytes: usize,
	/// Conservative admission of the live source relation and SCC structures.
	pub(super) live_scratch_bytes: usize,
}

/// A fixed 24-byte FERRARI interval, including the canonical unused tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReachabilityInterval {
	/// Inclusive lower postorder rank.
	pub lower: u64,
	/// Inclusive upper postorder rank.
	pub upper: u64,
	/// 0=Exact, 1=Approximate, 2=unused.
	pub kind: u8,
}

/// Algorithm-owned labels; physical status remains solely in the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReachabilityLabels {
	/// Exact forest intervals, optionally built on the reversed relation.
	TreeCover {
		/// One inclusive subtree interval per component.
		intervals: Vec<(u64, u64)>,
		/// Whether intervals describe the reversed forest.
		reversed: bool,
	},
	/// Component-major min-post intervals in all dimensions.
	Grail {
		/// Number of independently ordered dimensions.
		dimensions: u8,
		/// Component-major inclusive intervals for every dimension.
		intervals: Vec<(u64, u64)>,
	},
	/// Component-major bounded covers and the exact target postorder ranks.
	Ferrari {
		/// Maximum number of covers per component.
		max_ranges: u8,
		/// Exact target ranks used to test covers.
		postorder: Vec<u64>,
		/// Component-major padded exact/approximate covers.
		intervals: Vec<ReachabilityInterval>,
	},
	/// Component-major out/in bitsets, with `bytes` per half.
	Bfl {
		/// Byte width of each directional filter.
		bytes: u16,
		/// Component-major outgoing then incoming bitsets.
		filters: Vec<u8>,
	},
	/// Exact chain coordinates and the component-by-chain minimum matrix.
	ChainCover {
		/// Chain identity and position of each component.
		coordinates: Vec<(u64, u64)>,
		/// Number of canonical disjoint chains.
		chains: usize,
		/// Component-by-chain minimum reachable positions.
		cells: Vec<u64>,
	},
	/// AUTO could not fit a side-file; retain the exact traversal strategy.
	OnDemand {
		/// Number of SCCs retained for exact query admission.
		components: usize,
	},
}

/// Exact answer and work counters for one label lookup plus refinement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReachabilityAnswer {
	/// Whether the target is reachable, including the seed itself.
	pub(super) reachable: bool,
	/// Whether a Maybe label invoked the exact fallback.
	pub(super) fallback: bool,
	/// Component arcs examined during refinement.
	pub(super) expansions: u64,
}

/// Builds one explicit algorithm or AUTO's approved projected-byte choice.
///
/// # Errors
/// Returns a typed resource error before oversized label allocation, an invalid
/// parameter/family error, or the caller's cooperative cancellation error.
pub(super) fn build_reachability_labels(
	dag: &ProjectedCondensation,
	family: GraphKind,
	requested: ReachabilityAlgorithm,
	limits: ReachabilityBuildLimits,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<ReachabilityLabels> {
	let count = dag.representatives.len();
	let tree = matches!(family, GraphKind::Tree | GraphKind::RootedTree | GraphKind::Forest);
	if matches!(requested, ReachabilityAlgorithm::OnDemand) {
		return Ok(ReachabilityLabels::OnDemand {
			components: count,
		});
	}
	if matches!(requested, ReachabilityAlgorithm::TreeCover) && !tree {
		return Err(FerrosGrynnError::invalid_argument("TREE_COVER requires a tree family"));
	}
	let workspace = count
		.checked_mul(64)
		.and_then(|bytes| {
			dag.forward.targets.len().checked_mul(8).and_then(|edges| bytes.checked_add(edges))
		})
		.ok_or_else(|| scratch_error(limits.scratch_bytes))?;
	let limits = ReachabilityBuildLimits {
		live_scratch_bytes: limits
			.live_scratch_bytes
			.checked_add(workspace)
			.ok_or_else(|| scratch_error(limits.scratch_bytes))?,
		..limits
	};
	check_scratch(limits, 0)?;
	let mut chain_coordinates = None;
	let algorithm = if requested == ReachabilityAlgorithm::Auto {
		if tree {
			if projected_bytes(limits, &[(count, 16)])? <= limits.max_bytes {
				ReachabilityAlgorithm::TreeCover
			} else {
				ReachabilityAlgorithm::OnDemand
			}
		} else {
			let (coordinates, chains) = chain_cover(dag, limits, check)?;
			let cells =
				count.checked_mul(chains).ok_or_else(|| artifact_error(limits.max_bytes))?;
			chain_coordinates = Some((coordinates, chains));
			if projected_bytes(limits, &[(count, 16), (cells, 8)])? <= limits.max_bytes {
				ReachabilityAlgorithm::ChainCover
			} else if projected_bytes(limits, &[
				(count.checked_mul(4).ok_or_else(|| artifact_error(limits.max_bytes))?, 24),
				(count, 8),
			])? <= limits.max_bytes
			{
				ReachabilityAlgorithm::Ferrari(4)
			} else if projected_bytes(limits, &[(count, 40)])? <= limits.max_bytes {
				ReachabilityAlgorithm::Bfl(160)
			} else {
				ReachabilityAlgorithm::OnDemand
			}
		}
	} else {
		requested
	};
	match algorithm {
		ReachabilityAlgorithm::OnDemand => Ok(ReachabilityLabels::OnDemand {
			components: count,
		}),
		ReachabilityAlgorithm::TreeCover => {
			admit_labels(
				limits,
				&[(count, 16)],
				count.checked_mul(16).ok_or_else(|| scratch_error(limits.scratch_bytes))?,
			)?;
			let reversed = if dag.reverse.offsets.windows(2).all(|row| row[1] - row[0] <= 1) {
				false
			} else if dag.forward.offsets.windows(2).all(|row| row[1] - row[0] <= 1) {
				true
			} else {
				return Err(FerrosGrynnError::invalid_argument(
					"TREE_COVER relation is not an oriented forest",
				));
			};
			let csr = if reversed {
				&dag.reverse
			} else {
				&dag.forward
			};
			let post = postorder(csr, 0, limits, check)?;
			let mut intervals = filled(count, (0, 0), limits)?;
			for ordinal in 0..count {
				let source = dag.topological_order[if reversed {
					ordinal
				} else {
					count - ordinal - 1
				}];
				check()?;
				let mut lower = post[source];
				for target in csr.neighbours(source) {
					lower = lower.min(intervals[target].0);
				}
				intervals[source] = (lower, post[source]);
			}
			Ok(ReachabilityLabels::TreeCover {
				intervals,
				reversed,
			})
		}
		ReachabilityAlgorithm::Grail(dimensions) => {
			if !(1..=32).contains(&dimensions) {
				return Err(FerrosGrynnError::invalid_argument(
					"GRAIL dimensions must be in 1..=32",
				));
			}
			let entries = count
				.checked_mul(usize::from(dimensions))
				.ok_or_else(|| artifact_error(limits.max_bytes))?;
			admit_labels(
				limits,
				&[(entries, 16)],
				entries.checked_mul(16).ok_or_else(|| scratch_error(limits.scratch_bytes))?,
			)?;
			let mut intervals = filled(entries, (0, 0), limits)?;
			for dimension in 0..usize::from(dimensions) {
				let post = postorder(&dag.forward, dimension as u64 + 1, limits, check)?;
				for source in dag.topological_order.iter().rev() {
					check()?;
					let mut lower = post[*source];
					for target in dag.forward.neighbours(*source) {
						lower =
							lower.min(intervals[target * usize::from(dimensions) + dimension].0);
					}
					intervals[*source * usize::from(dimensions) + dimension] =
						(lower, post[*source]);
				}
			}
			Ok(ReachabilityLabels::Grail {
				dimensions,
				intervals,
			})
		}
		ReachabilityAlgorithm::Ferrari(max_ranges) => {
			if max_ranges == 0 {
				return Err(FerrosGrynnError::invalid_argument(
					"FERRARI ranges must be in 1..=255",
				));
			}
			let width = usize::from(max_ranges);
			let entries =
				count.checked_mul(width).ok_or_else(|| artifact_error(limits.max_bytes))?;
			let bytes = entries
				.checked_mul(24)
				.and_then(|bytes| count.checked_mul(8).and_then(|aux| bytes.checked_add(aux)))
				.ok_or_else(|| scratch_error(limits.scratch_bytes))?;
			admit_labels(limits, &[(entries, 24), (count, 8)], bytes)?;
			let post = postorder(&dag.forward, 0, limits, check)?;
			let unused = ReachabilityInterval {
				lower: u64::MAX,
				upper: 0,
				kind: 2,
			};
			let mut intervals = filled(entries, unused, limits)?;
			for source in dag.topological_order.iter().rev() {
				check()?;
				let mut row = vec![ReachabilityInterval {
					lower: post[*source],
					upper: post[*source],
					kind: 0,
				}];
				for target in dag.forward.neighbours(*source) {
					check()?;
					row.extend(
						intervals[target * width..(target + 1) * width]
							.iter()
							.copied()
							.filter(|range| range.kind != 2),
					);
					compress_ranges(&mut row, width);
				}
				intervals[*source * width..*source * width + row.len()].copy_from_slice(&row);
			}
			Ok(ReachabilityLabels::Ferrari {
				max_ranges,
				postorder: post,
				intervals,
			})
		}
		ReachabilityAlgorithm::Bfl(bits) => {
			if !(64..=4096).contains(&bits) || !bits.is_multiple_of(8) {
				return Err(FerrosGrynnError::invalid_argument(
					"BFL bits must be a multiple of eight in 64..=4096",
				));
			}
			let bytes = usize::from(bits / 8);
			let entries =
				count.checked_mul(bytes * 2).ok_or_else(|| artifact_error(limits.max_bytes))?;
			admit_labels(limits, &[(count, (bytes * 2) as u64)], entries)?;
			let mut filters = filled(entries, 0_u8, limits)?;
			for component in 0..count {
				let bit = (rapidhash::v3::rapidhash_v3(&(component as u64).to_le_bytes())
					% u64::from(bits)) as usize;
				filters[component * bytes * 2 + bit / 8] |= 1 << (bit % 8);
				filters[component * bytes * 2 + bytes + bit / 8] |= 1 << (bit % 8);
			}
			for source in dag.topological_order.iter().rev() {
				check()?;
				for target in dag.forward.neighbours(*source) {
					for byte in 0..bytes {
						filters[*source * bytes * 2 + byte] |= filters[target * bytes * 2 + byte];
					}
				}
			}
			for source in &dag.topological_order {
				check()?;
				for target in dag.forward.neighbours(*source) {
					for byte in 0..bytes {
						filters[target * bytes * 2 + bytes + byte] |=
							filters[*source * bytes * 2 + bytes + byte];
					}
				}
			}
			Ok(ReachabilityLabels::Bfl {
				bytes: bits / 8,
				filters,
			})
		}
		ReachabilityAlgorithm::ChainCover => {
			let (coordinates, chains) = match chain_coordinates {
				Some(cover) => cover,
				None => chain_cover(dag, limits, check)?,
			};
			let entries =
				count.checked_mul(chains).ok_or_else(|| artifact_error(limits.max_bytes))?;
			let bytes = entries
				.checked_mul(8)
				.and_then(|bytes| {
					count.checked_mul(16).and_then(|coords| bytes.checked_add(coords))
				})
				.ok_or_else(|| scratch_error(limits.scratch_bytes))?;
			admit_labels(limits, &[(count, 16), (entries, 8)], bytes)?;
			let mut cells = filled(entries, u64::MAX, limits)?;
			for source in dag.topological_order.iter().rev() {
				check()?;
				let (chain, position) = coordinates[*source];
				cells[*source * chains + chain as usize] = position;
				for target in dag.forward.neighbours(*source) {
					for chain in 0..chains {
						cells[*source * chains + chain] =
							cells[*source * chains + chain].min(cells[target * chains + chain]);
					}
				}
			}
			Ok(ReachabilityLabels::ChainCover {
				coordinates,
				chains,
				cells,
			})
		}
		ReachabilityAlgorithm::Auto => {
			Err(FerrosGrynnError::invalid_argument("AUTO reachability selection did not resolve"))
		}
	}
}

impl ReachabilityLabels {
	/// Returns the component extent represented by these labels.
	pub(super) fn component_count(&self) -> usize {
		match self {
			Self::TreeCover {
				intervals,
				..
			} => intervals.len(),
			Self::Grail {
				dimensions,
				intervals,
			} => intervals.len() / usize::from(*dimensions),
			Self::Ferrari {
				postorder,
				..
			} => postorder.len(),
			Self::Bfl {
				bytes,
				filters,
			} => filters.len() / (usize::from(*bytes) * 2),
			Self::ChainCover {
				coordinates,
				..
			} => coordinates.len(),
			Self::OnDemand {
				components,
			} => *components,
		}
	}

	/// Answers only what the selected label proves.
	///
	/// # Errors
	/// Rejects source or target references outside the component extent.
	pub(super) fn decision(
		&self,
		source: usize,
		target: usize,
	) -> FerrosGrynnResult<ReachabilityDecision> {
		if source >= self.component_count() || target >= self.component_count() {
			return Err(FerrosGrynnError::invalid_argument(
				"reachability component reference is outside the artifact",
			));
		}
		if source == target {
			return Ok(ReachabilityDecision::Yes);
		}
		let result = match self {
			Self::TreeCover {
				intervals,
				reversed,
			} => {
				let (source, target) = if *reversed {
					(target, source)
				} else {
					(source, target)
				};
				if contains(intervals[source], intervals[target]) {
					ReachabilityDecision::Yes
				} else {
					ReachabilityDecision::No
				}
			}
			Self::Grail {
				dimensions,
				intervals,
			} => {
				let dimensions = usize::from(*dimensions);
				if (0..dimensions).all(|dimension| {
					contains(
						intervals[source * dimensions + dimension],
						intervals[target * dimensions + dimension],
					)
				}) {
					ReachabilityDecision::Maybe
				} else {
					ReachabilityDecision::No
				}
			}
			Self::Ferrari {
				max_ranges,
				postorder,
				intervals,
			} => {
				let width = usize::from(*max_ranges);
				match intervals[source * width..(source + 1) * width].iter().find(|range| {
					range.kind != 2
						&& range.lower <= postorder[target]
						&& postorder[target] <= range.upper
				}) {
					Some(range) if range.kind == 0 => ReachabilityDecision::Yes,
					Some(_) => ReachabilityDecision::Maybe,
					None => ReachabilityDecision::No,
				}
			}
			Self::Bfl {
				bytes,
				filters,
			} => {
				let bytes = usize::from(*bytes);
				if (0..bytes).all(|byte| {
					filters[target * bytes * 2 + byte] & !filters[source * bytes * 2 + byte] == 0
						&& filters[source * bytes * 2 + bytes + byte]
							& !filters[target * bytes * 2 + bytes + byte]
							== 0
				}) {
					ReachabilityDecision::Maybe
				} else {
					ReachabilityDecision::No
				}
			}
			Self::ChainCover {
				coordinates,
				chains,
				cells,
			} => {
				let (chain, position) = coordinates[target];
				if cells[source * chains + chain as usize] <= position {
					ReachabilityDecision::Yes
				} else {
					ReachabilityDecision::No
				}
			}
			Self::OnDemand {
				..
			} => ReachabilityDecision::Maybe,
		};
		Ok(result)
	}

	/// Refines every Maybe through a bounded exact traversal of the same DAG.
	/// This proves existence only; it does not construct a path or prove its
	/// depth.
	///
	/// # Errors
	/// Returns a resource/cancellation error instead of a partial answer.
	pub(super) fn reachable(
		&self,
		dag: &ProjectedCondensation,
		source: usize,
		target: usize,
		max_expansions: u64,
		scratch_bytes: usize,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<ReachabilityAnswer> {
		let answer = self.decision(source, target)?;
		if answer != ReachabilityDecision::Maybe {
			return Ok(ReachabilityAnswer {
				reachable: answer == ReachabilityDecision::Yes,
				fallback: false,
				expansions: 0,
			});
		}
		let count = self.component_count();
		if count != dag.representatives.len() {
			return Err(FerrosGrynnError::invalid_argument(
				"reachability labels and condensation extents differ",
			));
		}
		if count.checked_mul(24).is_none_or(|bytes| bytes > scratch_bytes) {
			return Err(scratch_error(scratch_bytes));
		}
		let limits = ReachabilityBuildLimits {
			max_bytes: u64::MAX,
			common_pages: 0,
			scratch_bytes,
			live_scratch_bytes: 0,
		};
		let mut visited = filled(count, false, limits)?;
		visited[source] = true;
		let mut stack = Vec::new();
		stack
			.try_reserve_exact(count)
			.map_err(|error| scratch_error(scratch_bytes).with_technical_source(error))?;
		stack.push(source);
		let mut expansions = 0;
		while let Some(current) = stack.pop() {
			check()?;
			for next in dag.forward.neighbours(current) {
				if expansions == max_expansions {
					return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
						"reachability refinement expansions",
						max_expansions,
					));
				}
				expansions += 1;
				if next == target {
					return Ok(ReachabilityAnswer {
						reachable: true,
						fallback: true,
						expansions,
					});
				}
				if visited[next] {
					continue;
				}
				visited[next] = true;
				match self.decision(next, target)? {
					ReachabilityDecision::Yes => {
						return Ok(ReachabilityAnswer {
							reachable: true,
							fallback: true,
							expansions,
						});
					}
					ReachabilityDecision::No => {}
					ReachabilityDecision::Maybe => stack.push(next),
				}
			}
		}
		Ok(ReachabilityAnswer {
			reachable: false,
			fallback: true,
			expansions,
		})
	}
}

/// Inclusive interval containment, shared by exact and negative labels.
fn contains(source: (u64, u64), target: (u64, u64)) -> bool {
	source.0 <= target.0 && target.1 <= source.1
}

/// Deterministic seeded DFS postorder over a DAG, with complete active frames.
fn postorder(
	csr: &RelationCsr,
	seed: u64,
	limits: ReachabilityBuildLimits,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Vec<u64>> {
	let count = csr.offsets.len().saturating_sub(1);
	let mut visited = filled(count, false, limits)?;
	for target in &csr.targets {
		visited[*target] = true;
	}
	let mut roots = (0..count).collect::<Vec<_>>();
	// Begin at DAG sources so each forest subtree occupies one contiguous
	// range.
	roots.sort_unstable_by_key(|vertex| (visited[*vertex], shuffle_key(seed, *vertex), *vertex));
	visited.fill(false);
	let mut targets = csr.targets.clone();
	for row in csr.offsets.windows(2) {
		targets[row[0]..row[1]]
			.sort_unstable_by_key(|vertex| (shuffle_key(seed, *vertex), *vertex));
	}
	let mut rank = filled(count, 0_u64, limits)?;
	let mut next_rank = 1_u64;
	for root in roots {
		check()?;
		if visited[root] {
			continue;
		}
		visited[root] = true;
		let mut stack = vec![(root, csr.offsets[root])];
		while let Some((source, next)) = stack.last_mut() {
			check()?;
			if *next < csr.offsets[*source + 1] {
				let target = targets[*next];
				*next += 1;
				if !visited[target] {
					visited[target] = true;
					stack.push((target, csr.offsets[target]));
				}
			} else {
				rank[*source] = next_rank;
				next_rank =
					next_rank.checked_add(1).ok_or_else(|| scratch_error(limits.scratch_bytes))?;
				stack.pop();
			}
		}
	}
	Ok(rank)
}

/// Portable ordering keys keep seeded dimensions reproducible on reopen.
fn shuffle_key(seed: u64, vertex: usize) -> u64 {
	let mut bytes = [0; 16];
	bytes[..8].copy_from_slice(&seed.to_le_bytes());
	bytes[8..].copy_from_slice(&(vertex as u64).to_le_bytes());
	rapidhash::v3::rapidhash_v3(&bytes)
}

/// Greedy vertex-disjoint directed chains in deterministic topological order.
fn chain_cover(
	dag: &ProjectedCondensation,
	limits: ReachabilityBuildLimits,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<(Vec<(u64, u64)>, usize)> {
	let count = dag.representatives.len();
	let mut coordinates = filled(count, (u64::MAX, 0), limits)?;
	let mut chains = 0;
	for start in &dag.topological_order {
		check()?;
		if coordinates[*start].0 != u64::MAX {
			continue;
		}
		let mut vertex = *start;
		let mut position = 0;
		loop {
			check()?;
			coordinates[vertex] = (chains as u64, position);
			let Some(next) =
				dag.forward.neighbours(vertex).find(|target| coordinates[*target].0 == u64::MAX)
			else {
				break;
			};
			vertex = next;
			position += 1;
		}
		chains += 1;
	}
	Ok((coordinates, chains))
}

/// Merges overlapping covers, then introduces only marked approximate gaps.
fn compress_ranges(row: &mut Vec<ReachabilityInterval>, limit: usize) {
	row.sort_unstable_by_key(|range| (range.lower, range.upper, range.kind));
	let mut write = 0;
	for read in 0..row.len() {
		let current = row[read];
		if write != 0 && current.lower <= row[write - 1].upper.saturating_add(1) {
			row[write - 1].upper = row[write - 1].upper.max(current.upper);
			row[write - 1].kind |= current.kind;
		} else {
			row[write] = current;
			write += 1;
		}
	}
	row.truncate(write);
	// ponytail: greedy gap compression preserves correctness; use optimal
	// covers when refinement counts justify their additional build cost.
	while row.len() > limit {
		let Some((index, _)) = row
			.windows(2)
			.enumerate()
			.min_by_key(|(index, pair)| (pair[1].lower - pair[0].upper - 1, *index))
		else {
			break;
		};
		row[index].upper = row[index + 1].upper;
		row[index].kind = 1;
		row.remove(index + 1);
	}
}

/// Computes full physical bytes from exact element widths, without allocation.
fn projected_bytes(
	limits: ReachabilityBuildLimits,
	sections: &[(usize, u64)],
) -> FerrosGrynnResult<u64> {
	let mut pages = limits.common_pages;
	for (count, width) in sections {
		let capacity = ARRAY_BYTES
			.checked_div(*width)
			.filter(|capacity| *capacity != 0)
			.ok_or_else(|| artifact_error(limits.max_bytes))?;
		pages = pages
			.checked_add((*count as u64).div_ceil(capacity))
			.ok_or_else(|| artifact_error(limits.max_bytes))?;
	}
	pages.checked_mul(PAGE_BYTES).ok_or_else(|| artifact_error(limits.max_bytes))
}

/// Separates artifact page admission from transient builder memory.
fn admit_labels(
	limits: ReachabilityBuildLimits,
	sections: &[(usize, u64)],
	label_bytes: usize,
) -> FerrosGrynnResult<()> {
	if projected_bytes(limits, sections)? > limits.max_bytes {
		return Err(artifact_error(limits.max_bytes));
	}
	let metadata = sections
		.iter()
		.try_fold(0_usize, |total, (count, _)| total.checked_add(*count))
		.and_then(|count| count.checked_mul(64))
		.and_then(|metadata| metadata.checked_add(label_bytes))
		.ok_or_else(|| scratch_error(limits.scratch_bytes))?;
	check_scratch(limits, metadata)
}

/// Checks the aggregate live and prospective scratch reservation.
fn check_scratch(limits: ReachabilityBuildLimits, additional: usize) -> FerrosGrynnResult<()> {
	if limits
		.live_scratch_bytes
		.checked_add(additional)
		.is_none_or(|bytes| bytes > limits.scratch_bytes)
	{
		return Err(scratch_error(limits.scratch_bytes));
	}
	Ok(())
}

/// Fallible fixed-capacity allocation after the caller's aggregate admission.
fn filled<T: Clone>(
	count: usize,
	value: T,
	limits: ReachabilityBuildLimits,
) -> FerrosGrynnResult<Vec<T>> {
	let mut values = Vec::new();
	values
		.try_reserve_exact(count)
		.map_err(|error| scratch_error(limits.scratch_bytes).with_technical_source(error))?;
	values.resize(count, value);
	Ok(values)
}

/// Reports the complete artifact cap, never a partial label set.
fn artifact_error(limit: u64) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded("reachability artifact bytes", limit)
}
/// Reports the separately configured scratch cap.
fn scratch_error(limit: usize) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded("reachability scratch bytes", limit as u64)
}

#[cfg(test)]
mod tests;
