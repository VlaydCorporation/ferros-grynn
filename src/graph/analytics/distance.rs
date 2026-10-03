//! Shared all-source positive-distance scratch for harmonic and Brandes
//! metrics.
#![allow(clippy::cast_precision_loss, reason = "Only final centrality ratios are floating scores")]

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BinaryHeap},
};

use super::WeightSelection;
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::tensor::{
		TensorProjection,
		control::{Control, invalid, overflow},
	},
	math::{add_numbers, number_is_finite},
	values::Number,
};

/// Detects costs whose rounded addition can merge unequal path prefixes.
pub(super) fn needs_full_state(
	projection: &TensorProjection,
	weights: WeightSelection,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<bool> {
	if weights == WeightSelection::Unweighted {
		return Ok(false);
	}
	for arc in &projection.arcs {
		control.step()?;
		if !matches!(projection.connector_weights[arc.connector], Number::Int(_)) {
			return Ok(true);
		}
	}
	Ok(false)
}

/// One reusable DFS frame; projected vertex reuse is tracked in a bitmap.
#[derive(Clone, Copy)]
struct CentralityFrame {
	/// Current vertex.
	node: usize,
	/// Next outgoing original connector to expand.
	next: usize,
	/// Exact original Number cost of this complete prefix.
	cost: Number,
}

/// Original transition retained independently of cost equality or rounding.
#[derive(Clone, Copy)]
struct CentralityArc {
	/// Source qualified ordinal.
	source: usize,
	/// Target qualified ordinal.
	target: usize,
	/// Original additive cost.
	cost: Number,
}

/// Enumerates vertex-acyclic projected paths without prefix dominance pruning.
/// Rounded costs may merge unequal prefixes, so a predecessor DAG is unsound.
/// Work and scratch ceilings are checked before allocation and expansion.
// ponytail: non-Int costs require bounded path enumeration; certify safe
// arithmetic domains when measured workloads need a faster path.
pub(super) fn full_state_scores(
	projection: &TensorProjection,
	brandes: bool,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<Vec<f64>> {
	let n = projection.atoms.len();
	let mut arcs = control.array(projection.arcs.len())?;
	let mut previous = None;
	for arc in &projection.arcs {
		control.step()?;
		let key = (arc.source, arc.target, arc.connector);
		if previous == Some(key) {
			continue;
		}
		previous = Some(key);
		let cost = projection.connector_weights[arc.connector];
		if !number_is_finite(cost) || cost <= Number::Int(0) {
			return Err(invalid("weighted centrality requires strictly positive finite costs"));
		}
		arcs.push(CentralityArc {
			source: arc.source,
			target: arc.target,
			cost,
		});
	}
	let mut offsets = control.array(n.checked_add(1).ok_or_else(overflow)?)?;
	let mut at = 0;
	for row in 0..n {
		control.step()?;
		offsets.push(at);
		while at < arcs.len() && arcs[at].source == row {
			control.step()?;
			at += 1;
		}
	}
	offsets.push(at);
	let mut values = control.filled(n, None)?;
	let mut counts = control.filled(n, 0_u64)?;
	let mut through = control.filled(
		if brandes {
			n.checked_mul(n).ok_or_else(overflow)?
		} else {
			0
		},
		0_u64,
	)?;
	let mut visited = control.filled(n, false)?;
	let mut stack = control.array::<CentralityFrame>(n)?;
	let mut scores = control.filled(n, 0.0)?;
	for source in 0..n {
		for row in 0..n {
			control.step()?;
			values[row] = None;
			counts[row] = 0;
		}
		for count in &mut through {
			control.step()?;
			*count = 0;
		}
		values[source] = Some(Number::Int(0));
		visited[source] = true;
		stack.push(CentralityFrame {
			node: source,
			next: offsets[source],
			cost: Number::Int(0),
		});
		while let Some(frame) = stack.last_mut() {
			control.step()?;
			if frame.next == offsets[frame.node + 1] {
				visited[frame.node] = false;
				stack.pop();
				continue;
			}
			let arc = arcs[frame.next];
			frame.next += 1;
			if visited[arc.target] {
				continue;
			}
			let cost = add_numbers(frame.cost, arc.cost)?;
			if cost <= frame.cost {
				return Err(FerrosGrynnError::operation_overflow(
					"positive Number distance progression",
				));
			}
			let better = values[arc.target].is_none_or(|before| cost < before);
			let equal = values[arc.target] == Some(cost);
			if better {
				values[arc.target] = Some(cost);
				counts[arc.target] = 0;
				if brandes {
					for count in &mut through[arc.target * n..(arc.target + 1) * n] {
						control.step()?;
						*count = 0;
					}
				}
			}
			if brandes && (better || equal) {
				counts[arc.target] = counts[arc.target].checked_add(1).ok_or_else(overflow)?;
				for middle in stack.iter().skip(1) {
					control.step()?;
					let count = &mut through[arc.target * n + middle.node];
					*count = count.checked_add(1).ok_or_else(overflow)?;
				}
			}
			visited[arc.target] = true;
			stack.push(CentralityFrame {
				node: arc.target,
				next: offsets[arc.target],
				cost,
			});
		}
		for target in 0..n {
			control.step()?;
			if target == source {
				continue;
			}
			if let Some(distance) = values[target] {
				if brandes {
					for (middle, score) in scores.iter_mut().enumerate() {
						control.step()?;
						*score = super::scalar::finite(
							*score + through[target * n + middle] as f64 / counts[target] as f64,
						)?;
					}
				} else {
					scores[source] = super::scalar::finite(
						scores[source]
							+ 1.0
								/ distance.as_f64().ok_or_else(|| {
									invalid("distance cannot be represented as a floating score")
								})?,
					)?;
				}
			}
		}
	}
	Ok(scores)
}

/// Minimum cost and multiplicity for one ordered qualified pair.
#[derive(Clone, Copy)]
struct DistanceArc {
	/// Source qualified ordinal.
	source: usize,
	/// Target qualified ordinal.
	target: usize,
	/// Minimum original connector cost, never summed adjacency strength.
	cost: Number,
	/// Distinct equally minimal connectors for this pair.
	count: u64,
}

/// Bounded reusable CSR and Dijkstra state, allocated once for all sources.
pub(super) struct Distances {
	/// Exact Number distances after the last complete source search.
	pub(super) values: Vec<Option<Number>>,
	/// Checked shortest-path counters for the last complete source search.
	pub(super) counts: Vec<u64>,
	/// Canonical nondecreasing settled distance order.
	pub(super) order: Vec<usize>,
	/// Canonical minimum-cost ordered pairs.
	arcs: Vec<DistanceArc>,
	/// Exclusive outgoing row offsets.
	offsets: Vec<usize>,
	/// Exclusive incoming row offsets.
	incoming_offsets: Vec<usize>,
	/// Incoming positions in the shared outgoing arc array.
	incoming_ordinals: Vec<usize>,
	/// Reused exact-distance priority queue with capacity m+1.
	heap: BinaryHeap<Reverse<(Number, usize)>>,
	/// Last-search settled flags.
	settled: Vec<bool>,
}

impl Distances {
	/// Validates original costs and builds minimum-cost/multiplicity CSR.
	pub(super) fn new(
		projection: &TensorProjection,
		weights: WeightSelection,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<Self> {
		let mut cells = BTreeMap::<(usize, usize), (Number, u64)>::new();
		let mut previous = None;
		for arc in &projection.arcs {
			control.step()?;
			let key = (arc.source, arc.target, arc.connector);
			if previous == Some(key) {
				continue;
			}
			previous = Some(key);
			let cost = match weights {
				WeightSelection::Unweighted => Number::Int(1),
				WeightSelection::Stored => projection.connector_weights[arc.connector],
			};
			if !number_is_finite(cost) || cost <= Number::Int(0) {
				return Err(invalid(
					"weighted harmonic and Brandes distances require strictly positive finite \
					 connector costs",
				));
			}
			match cells.get_mut(&(arc.source, arc.target)) {
				Some((before, count)) if cost < *before => {
					*before = cost;
					*count = 1;
				}
				Some((before, count)) if cost == *before => {
					*count = count.checked_add(1).ok_or_else(overflow)?;
				}
				Some(_) => {}
				None => {
					control.admit(192)?;
					cells.insert((arc.source, arc.target), (cost, 1));
				}
			}
		}
		let n = projection.atoms.len();
		let m = cells.len();
		let mut arcs = control.array(m)?;
		let mut offsets = control.array(n.checked_add(1).ok_or_else(overflow)?)?;
		let mut cells = cells.into_iter().peekable();
		for source in 0..n {
			control.step()?;
			offsets.push(arcs.len());
			while cells.peek().is_some_and(|(pair, _)| pair.0 == source) {
				control.step()?;
				if let Some(((_, target), (cost, count))) = cells.next() {
					arcs.push(DistanceArc {
						source,
						target,
						cost,
						count,
					});
				}
			}
		}
		offsets.push(arcs.len());
		let mut incoming_offsets =
			control.filled(n.checked_add(1).ok_or_else(overflow)?, 0_usize)?;
		for arc in &arcs {
			control.step()?;
			incoming_offsets[arc.target + 1] =
				incoming_offsets[arc.target + 1].checked_add(1).ok_or_else(overflow)?;
		}
		for row in 0..n {
			control.step()?;
			incoming_offsets[row + 1] = incoming_offsets[row + 1]
				.checked_add(incoming_offsets[row])
				.ok_or_else(overflow)?;
		}
		let mut cursor = control.array(n)?;
		cursor.extend_from_slice(&incoming_offsets[..n]);
		let mut incoming_ordinals = control.filled(m, 0_usize)?;
		for (ordinal, arc) in arcs.iter().enumerate() {
			control.step()?;
			incoming_ordinals[cursor[arc.target]] = ordinal;
			cursor[arc.target] = cursor[arc.target].checked_add(1).ok_or_else(overflow)?;
		}
		let heap = BinaryHeap::from(control.array(m.checked_add(1).ok_or_else(overflow)?)?);
		Ok(Self {
			values: control.filled(n, None)?,
			counts: control.filled(n, 0_u64)?,
			order: control.array(n)?,
			settled: control.filled(n, false)?,
			arcs,
			offsets,
			incoming_offsets,
			incoming_ordinals,
			heap,
		})
	}

	/// Visits incoming minimum-cost pairs without duplicating arc storage.
	pub(super) fn incoming(&self, row: usize) -> impl Iterator<Item = (usize, Number, u64)> + '_ {
		self.incoming_ordinals[self.incoming_offsets[row]..self.incoming_offsets[row + 1]]
			.iter()
			.map(|ordinal| {
				let arc = self.arcs[*ordinal];
				(arc.source, arc.cost, arc.count)
			})
	}

	/// Reuses the admitted queue and vectors for one exact source search.
	pub(super) fn search(
		&mut self,
		source: usize,
		count_paths: bool,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<()> {
		if source >= self.values.len() {
			return Err(invalid("centrality source ordinal is outside its domain"));
		}
		self.heap.clear();
		self.order.clear();
		for row in 0..self.values.len() {
			control.step()?;
			self.values[row] = None;
			self.counts[row] = 0;
			self.settled[row] = false;
		}
		self.values[source] = Some(Number::Int(0));
		self.counts[source] = 1;
		self.heap.push(Reverse((Number::Int(0), source)));
		while let Some(Reverse((distance, node))) = self.heap.pop() {
			control.work(
				1 + usize::try_from(self.arcs.len().checked_add(1).ok_or_else(overflow)?.ilog2())
					.map_err(|_cause| overflow())?,
			)?;
			if self.settled[node] || self.values[node] != Some(distance) {
				continue;
			}
			self.settled[node] = true;
			self.order.push(node);
			for at in self.offsets[node]..self.offsets[node + 1] {
				control.step()?;
				let arc = self.arcs[at];
				let next = add_numbers(distance, arc.cost)?;
				if next <= distance {
					return Err(FerrosGrynnError::operation_overflow(
						"positive Number distance progression",
					));
				}
				if self.values[arc.target].is_some_and(|before| next > before) {
					continue;
				}
				let count = if count_paths {
					self.counts[node].checked_mul(arc.count).ok_or_else(overflow)?
				} else {
					0
				};
				match self.values[arc.target] {
					Some(before) if next > before => {}
					Some(before) if next == before => {
						if count_paths {
							self.counts[arc.target] =
								self.counts[arc.target].checked_add(count).ok_or_else(overflow)?;
						}
					}
					_ => {
						self.values[arc.target] = Some(next);
						self.counts[arc.target] = count;
						self.heap.push(Reverse((next, arc.target)));
					}
				}
			}
		}
		Ok(())
	}
}
