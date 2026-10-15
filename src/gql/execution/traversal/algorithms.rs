//! Adapters from traversal barriers to the shared graph algorithms.

use std::{
	borrow::Cow,
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
	sync::Arc,
};

use super::*;
use crate::{
	graph::{
		AtomRef,
		Condensation,
		CutSet,
		EdgeTopology,
		GraphKind,
		GraphStorageMode,
		SnapshotId,
		SpanningForest,
		bellman_ford_with_check,
		bfs_with_check,
		condensation_with_check,
		dijkstra_to_with_control,
		find_cuts_with_check,
		has_cycle_directed_with_check,
		has_cycle_undirected_with_check,
		minimum_spanning_forest_with_check,
		strongly_connected_components_with_check,
		topological_sort_with_check,
		weakly_connected_components_with_check,
	},
	values::{PropertyMap, Uuid},
};

impl<Read: QueryRead> TraversalMachine<'_, Read> {
	/// Executes algorithm for `TraversalMachine<'_, Read>`.
	pub(super) fn execute_algorithm(
		&self,
		kind: TraverseStepKind,
		step: &PreparedStep,
		input: Vec<Traverser>,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		if kind.is_tensor_analytics() {
			return self.execute_tensor_analytics(kind, step, input);
		}
		if matches!(
			kind,
			TraverseStepKind::AllShortestPaths | TraverseStepKind::AllWeightedShortestPaths
		) {
			return self.execute_all_shortest(step, input);
		}
		match kind {
			TraverseStepKind::KHop
			| TraverseStepKind::KHopEstimate
			| TraverseStepKind::Reachable
			| TraverseStepKind::ShortestPath
			| TraverseStepKind::WeightedShortestPath
			| TraverseStepKind::DistanceTo
			| TraverseStepKind::BellmanFord
			| TraverseStepKind::LowestCommonAncestor => self.execute_seed_algorithm(kind, step, input),
			TraverseStepKind::TopologicalSort
			| TraverseStepKind::StronglyConnectedComponents
			| TraverseStepKind::WeaklyConnectedComponents
			| TraverseStepKind::Condensation
			| TraverseStepKind::MinimumSpanningForest
			| TraverseStepKind::Cuts
			| TraverseStepKind::HasCycle => self.execute_induced_algorithm(kind, input),
			_ => Err(FerrosGrynnError::invalid_argument(
				"A non-algorithm step reached the traversal algorithm adapter.",
			)),
		}
	}

	/// Executes seed algorithm for `TraversalMachine<'_, Read>`.
	fn execute_seed_algorithm(
		&self,
		kind: TraverseStepKind,
		step: &PreparedStep,
		input: Vec<Traverser>,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let mut memory = self.control.barrier_account();
		let limit = || {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"algorithm result bytes",
				self.control.barrier_memory_limit() as u64,
			)
		};
		if !memory.reserve(input.len().checked_mul(3 * size_of::<Traverser>()).ok_or_else(limit)?) {
			return Err(limit());
		}
		let mut output = Vec::with_capacity(input.len());
		let mut retain = |output: &mut Vec<Traverser>, traverser: Traverser| {
			if !memory.reserve(traverser.estimated_heap_bytes()) {
				return Err(limit());
			}
			output.push(traverser);
			Ok(())
		};
		for mut traverser in input {
			self.control.check()?;
			let occurrence = occurrence(&traverser.current)?.clone();
			if kind == TraverseStepKind::KHopEstimate {
				let radius = u32::try_from(non_negative_usize(step.arguments.first())?).map_err(
					|_cause| FerrosGrynnError::invalid_argument("kHopEstimate radius exceeds u32"),
				)?;
				let mut memory = self.control.barrier_account();
				let scratch = self.control.barrier_memory_available();
				let estimate = self.read.k_hop_estimate(
					self.snapshot,
					&occurrence,
					if self.atom_semantics {
						crate::graph::TraversalSemantics::Atom
					} else {
						crate::graph::TraversalSemantics::Node
					},
					radius,
					step.neighbourhood_index,
					scratch,
					self.control.remaining_neighbourhood_work(),
					&mut |bytes| {
						if memory.reserve(bytes) {
							Ok(())
						} else {
							Err(FerrosGrynnError::traversal_resource_limit_exceeded(
								"neighbourhood scratch bytes",
								scratch as u64,
							))
						}
					},
					&mut || self.control.check(),
				)?;
				self.control.record_neighbourhood_estimate(
					estimate.approximate,
					estimate.work,
					estimate.artifact_bytes,
				)?;
				// The projection scratch may reserve all available bytes. Its
				// count result owns no projection buffers; release them before
				// admitting the retained output batch.
				drop(memory);
				traverser.current = Value::from(estimate.count);
				retain(&mut output, traverser)?;
				continue;
			}
			if occurrence.atom().kind() != AtomKind::Node {
				return Err(domain_error("seed-local graph algorithms require node occurrences"));
			}
			let graph_ref = Arc::new(occurrence.graph().clone());
			let graph = self.read.graph(self.snapshot, &graph_ref)?;
			let control = self.control;
			let mut check = || control.check();
			let source = graph.atom_id(occurrence.atom())?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist("node", occurrence.atom().id().to_string())
			})?;
			traverser.current = match kind {
				TraverseStepKind::KHop => {
					let depth = non_negative_usize(step.arguments.first())?;
					let nodes = bfs_with_check(
						&graph,
						source,
						TraversalDirection::Forward,
						Some(depth),
						&mut check,
					)?;
					Value::Array(occurrence_values(&graph, graph_ref, nodes)?.into())
				}
				TraverseStepKind::Reachable => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					Value::Bool(
						bfs_with_check(
							&graph,
							source,
							TraversalDirection::Forward,
							None,
							&mut check,
						)?
						.contains(&target),
					)
				}
				TraverseStepKind::ShortestPath => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					self.unweighted_path(
						&graph,
						graph_ref,
						source,
						target,
						step.landmark_path.as_ref(),
					)?
					.map_or(Value::None, |path| Value::Path(Box::new(path)))
				}
				TraverseStepKind::WeightedShortestPath | TraverseStepKind::DistanceTo => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					if self.custom_cost.is_some() {
						self.custom_weighted_result(
							Rc::new(graph),
							graph_ref,
							source,
							target,
							step,
						)?
					} else {
						self.weighted_result(
							&graph,
							graph_ref,
							source,
							target,
							step.landmark_path.as_ref(),
							kind == TraverseStepKind::WeightedShortestPath,
						)?
					}
				}
				TraverseStepKind::BellmanFord => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					bellman_ford_with_check(
						&graph,
						source,
						TraversalDirection::Forward,
						&mut check,
					)?
					.materialize_path(&graph, graph_ref, target)?
					.map_or(Value::None, |path| Value::Path(Box::new(path)))
				}
				TraverseStepKind::LowestCommonAncestor => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					lowest_common_ancestor(&graph, source, target, &mut check)?
						.map(|atom| occurrence_value(&graph, graph_ref, atom))
						.transpose()?
						.unwrap_or(Value::None)
				}
				_ => unreachable!("seed algorithm kind was matched above"),
			};
			retain(&mut output, traverser)?;
		}
		Ok(output)
	}

	/// Shares the bounded, full-state MATCH kernel for prefix-dependent COST.
	fn custom_weighted_result<Graph: GraphRead>(
		&self,
		graph: Rc<Graph>,
		reference: Arc<GraphRef>,
		source: AtomId,
		target: AtomId,
		step: &PreparedStep,
	) -> FerrosGrynnResult<Value> {
		let origin = graph.identity();
		let path_snapshot = graph.snapshot_id();
		let cost = self.custom_cost.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("custom shortest path has no COST")
		})?;
		let mut memory = self.control.barrier_account();
		let mut admit = |bytes| {
			if memory.reserve(bytes) {
				Ok(())
			} else {
				Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"custom shortest path scratch bytes",
					self.control.barrier_memory_limit() as u64,
				))
			}
		};
		let index = Rc::new(TraversalIndex::new_with_admission(
			graph.as_ref(),
			TraversalSemantics::Node,
			&mut admit,
			&mut || self.control.check(),
		)?);
		let binding = |domain, atom: AtomId, column| -> FerrosGrynnResult<_> {
			Ok(ResolvedMatchBinding {
				domain,
				selector: ResolvedMatchSelector::Record(
					graph
						.atom_ref(atom)?
						.ok_or_else(|| {
							FerrosGrynnError::atom_does_not_exist("atom", atom.to_string())
						})?
						.id()
						.clone(),
				),
				column: Some(column),
				label: None,
				properties: Vec::new(),
				predicate: None,
				meta: false,
			})
		};
		let path_mode = self.constraints.mode.unwrap_or(crate::syn::MatchPathMode::Trail);
		if path_mode == crate::syn::MatchPathMode::Walk && source != target {
			let reachable = index.breadth_first_with_control(
				EdgeEndpoint::Local(source),
				TraversalDirection::Forward,
				None,
				&mut admit,
				&mut || self.control.check(),
				&mut || self.control.record_path_expansion(),
			)?;
			if !reachable.contains(&EdgeEndpoint::Local(target)) {
				return Ok(Value::None);
			}
		}
		let max = match path_mode {
			crate::syn::MatchPathMode::Walk => u32::MAX,
			crate::syn::MatchPathMode::Trail => {
				u32::try_from(graph.edge_count()?).map_err(|cause| {
					FerrosGrynnError::operation_overflow("custom path edge bound")
						.with_technical_source(cause)
				})?
			}
			crate::syn::MatchPathMode::Acyclic => {
				u32::try_from(graph.node_count()?.saturating_sub(1)).map_err(|cause| {
					FerrosGrynnError::operation_overflow("custom path node bound")
						.with_technical_source(cause)
				})?
			}
			crate::syn::MatchPathMode::Simple => {
				u32::try_from(graph.node_count()?).map_err(|cause| {
					FerrosGrynnError::operation_overflow("custom path node bound")
						.with_technical_source(cause)
				})?
			}
		};
		let fields = MatchGraphFields::new(
			&*self.read,
			self.snapshot,
			self.read.property_dictionary(self.snapshot),
			reference.clone(),
		);
		let predicate = MatchPredicateContext {
			slots: self.slots,
			source: self.source,
			fields: &fields,
		};
		let seed = SeedCursor::new(
			Box::new(OneRowCursor {
				row: Some(Row::bindings(2)),
			}),
			graph.clone(),
			reference.clone(),
			binding(EndpointDomain::Node, source, 0)?,
			crate::syn::MatchMode::RepeatableElements,
			predicate,
			vec![source],
		);
		let expand = ResolvedPathExpand {
			edge: ResolvedMatchBinding {
				domain: EndpointDomain::Edge,
				selector: ResolvedMatchSelector::Any,
				column: None,
				label: None,
				properties: Vec::new(),
				predicate: None,
				meta: false,
			},
			target: binding(EndpointDomain::Node, target, 1)?,
			direction: crate::syn::MatchDirection::Forward,
			min: 0,
			max,
			path_column: None,
			match_mode: crate::syn::MatchMode::RepeatableElements,
			path_mode,
			objective: Some(crate::syn::MatchPathObjective::WeightedShortest),
			custom_cost: Some(cost),
			filters: self.constraints.filters.clone(),
		};
		let trace = (step.kind == TraverseStepKind::WeightedShortestPath)
			.then(|| Rc::new(RefCell::new(MatchPathTrace::new(self.control))));
		let input: Box<dyn RowCursor> = if let Some(trace) = &trace {
			Box::new(PathTraceStartCursor {
				input: Box::new(seed),
				graph: graph.clone(),
				graph_ref: reference.clone(),
				arena: trace.clone(),
			})
		} else {
			Box::new(seed)
		};
		let mut cursor = ShortestPathCursor::new(
			input,
			graph,
			reference,
			index,
			expand,
			predicate,
			trace.clone(),
			&*self.read,
			self.snapshot,
			step.landmark_path.clone(),
		)
		.with_relation_direction(TraversalDirection::Forward);
		let Some(row) = cursor.next(self.control)? else {
			return Ok(Value::None);
		};
		if step.kind == TraverseStepKind::DistanceTo {
			return Ok(Value::Number(row.trace_cost));
		}
		let path = row.trace.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("custom shortest path has no trace")
		})?;
		let trace = trace.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("custom shortest path has no trace arena")
		})?;
		Ok(Value::Path(Box::new(trace.borrow().materialize_with_admission(
			path,
			origin,
			path_snapshot,
			&mut admit,
			&mut || self.control.check(),
		)?)))
	}

	/// Executes induced algorithm for `TraversalMachine<'_, Read>`.
	fn execute_induced_algorithm(
		&self,
		kind: TraverseStepKind,
		input: Vec<Traverser>,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let (graph_ref, atoms) = induced_atoms(&input)?;
		let graph = self.read.graph(self.snapshot, &graph_ref)?;
		let mut nodes = BTreeSet::new();
		for atom in atoms {
			let id = graph.atom_id(&atom)?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist("node", atom.id().to_string())
			})?;
			if !id.is_node() {
				return Err(domain_error("induced graph algorithms require node occurrences"));
			}
			nodes.insert(id);
		}
		let control = self.control;
		let mut check = || control.check();
		let edges = induced_edges(&graph, &nodes, &mut check)?;
		let induced = InducedGraph {
			inner: &graph,
			nodes,
			edges,
		};
		let value = match kind {
			TraverseStepKind::TopologicalSort => Value::Array(
				occurrence_values(
					&induced,
					graph_ref.clone(),
					topological_sort_with_check(&induced, &mut check)?,
				)?
				.into(),
			),
			TraverseStepKind::StronglyConnectedComponents => components_value(
				&induced,
				graph_ref.clone(),
				strongly_connected_components_with_check(&induced, &mut check)?,
			)?,
			TraverseStepKind::WeaklyConnectedComponents => components_value(
				&induced,
				graph_ref.clone(),
				weakly_connected_components_with_check(&induced, &mut check)?,
			)?,
			TraverseStepKind::Condensation => condensation_value(
				&induced,
				graph_ref.clone(),
				condensation_with_check(&induced, &mut check)?,
			)?,
			TraverseStepKind::MinimumSpanningForest => spanning_value(
				&induced,
				graph_ref.clone(),
				minimum_spanning_forest_with_check(&induced, &mut check)?,
			)?,
			TraverseStepKind::Cuts => cuts_value(
				&induced,
				graph_ref.clone(),
				find_cuts_with_check(&induced, &mut check)?,
			)?,
			TraverseStepKind::HasCycle => {
				Value::Bool(if graph_is_directed(&induced, &mut check)? {
					has_cycle_directed_with_check(&induced, &mut check)?
				} else {
					has_cycle_undirected_with_check(&induced, &mut check)?
				})
			}
			_ => unreachable!("induced algorithm kind was matched above"),
		};
		Ok(vec![Traverser::new(value)])
	}

	/// Refines the existing Dijkstra queue using strictly greater completion
	/// bounds. Keeping its distance/node order preserves the first equal path.
	fn weighted_result<Graph: GraphRead>(
		&self,
		graph: &Graph,
		reference: Arc<GraphRef>,
		source: AtomId,
		target: AtomId,
		binding: Option<&crate::datastore::QueryLandmarkPath>,
		materialize: bool,
	) -> FerrosGrynnResult<Value> {
		let control = self.control;
		let mut memory = control.barrier_account();
		let mut admit = |bytes| {
			if memory.reserve(bytes) {
				Ok(())
			} else {
				Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"weighted path scratch bytes",
					control.barrier_memory_limit() as u64,
				))
			}
		};
		let vertices = graph.node_count()?;
		admit(
			vertices.checked_mul(256).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("weighted bound cache bytes")
			})?,
		)?;
		let remaining = u32::try_from(vertices).map_err(|cause| {
			FerrosGrynnError::operation_overflow("weighted completion steps")
				.with_technical_source(cause)
		})?;
		let binding = binding.filter(|binding| {
			binding.graph == reference.id()
				&& binding.mode == TraversalSemantics::Node
				&& binding.direction == TraversalDirection::Forward
				&& binding.cost == crate::datastore::GraphIndexCost::Weighted
				&& binding.edge_labels.is_empty()
		});
		let mut bounds = BTreeMap::new();
		let mut used = false;
		let mut missing = false;
		let mut refine = |node, prefix, upper| {
			let bound = if let Some(bound) = bounds.get(&node) {
				*bound
			} else {
				let bound = binding
					.map(|binding| self.read.path_lower_bound(self.snapshot, binding, node, target))
					.transpose()?
					.flatten();
				if let Some(bound) = bound {
					if !crate::math::number_is_finite(bound) || bound < Number::Int(0) {
						return Err(FerrosGrynnError::invalid_storage_file(
							"WEIGHT lower bound is not finite and nonnegative",
						));
					}
					control.record_landmark_lookup(if used {
						0
					} else {
						binding.map_or(0, |index| index.bytes)
					});
					used = true;
				} else {
					missing = true;
				}
				bounds.insert(node, bound);
				bound
			};
			if let (Some(bound), Some(upper)) = (bound, upper) {
				Ok(super::super::cursor::completion_bound(prefix, bound, remaining)? <= upper)
			} else {
				Ok(true)
			}
		};
		let (paths, pruning_safe) = dijkstra_to_with_control(
			graph,
			source,
			target,
			&mut admit,
			&mut refine,
			&mut || control.check(),
			&mut || control.record_path_expansion(),
		)?;
		if !pruning_safe || !used || missing {
			control.record_landmark_fallback();
		}
		if materialize {
			paths
				.materialize_path_with_admission(graph, reference, target, &mut admit)
				.map(|path| path.map_or(Value::None, |path| Value::Path(Box::new(path))))
		} else {
			Ok(paths.distance_to(target).map_or(Value::None, Value::Number))
		}
	}

	/// A* determines only the exact length. The second pass keeps the existing
	/// FIFO/connector order, pruning strictly greater completion bounds so
	/// equal shortest routes retain their original predecessor.
	fn unweighted_path<Graph: GraphRead>(
		&self,
		graph: &Graph,
		reference: Arc<GraphRef>,
		source: AtomId,
		target: AtomId,
		binding: Option<&crate::datastore::QueryLandmarkPath>,
	) -> FerrosGrynnResult<Option<Path>> {
		let control = self.control;
		let mut check = || control.check();
		let mut memory = control.barrier_account();
		let mut admit = |bytes| {
			if memory.reserve(bytes) {
				Ok(())
			} else {
				Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"shortest path scratch bytes",
					control.barrier_memory_limit() as u64,
				))
			}
		};
		let index = TraversalIndex::new_with_admission(
			graph,
			TraversalSemantics::Node,
			&mut admit,
			&mut check,
		)?;
		// BTree nodes, FIFO entries, predecessor steps and bound cache are all
		// linear in the live NODE projection; reserve before their first
		// insert.
		let state_bytes =
			3 * (size_of::<PathStep>() + size_of::<(AtomId, crate::graph::TraversalStep)>()) + 512;
		admit(
			graph.node_count()?.checked_mul(state_bytes).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("shortest path scratch bytes")
			})?,
		)?;
		let binding = binding.filter(|binding| binding.graph == reference.id());
		let initial_bound = binding
			.map(|binding| self.read.path_lower_bound(self.snapshot, binding, source, target))
			.transpose()?
			.flatten();
		let mut bounds = BTreeMap::new();
		let mut lower = |node| {
			if let Some(bound) = bounds.get(&node) {
				return Ok(*bound);
			}
			let bound = if node == source {
				initial_bound
			} else {
				binding
					.map(|binding| self.read.path_lower_bound(self.snapshot, binding, node, target))
					.transpose()?
					.flatten()
			};
			let bound = match bound {
				Some(Number::Int(bound)) if bound >= 0 => {
					control.record_landmark_lookup(if node == source {
						binding.map_or(0, |binding| binding.bytes)
					} else {
						0
					});
					usize::try_from(bound).map_err(|cause| {
						FerrosGrynnError::invalid_storage_file("HOP lower bound exceeds usize")
							.with_technical_source(cause)
					})?
				}
				Some(_) => {
					return Err(FerrosGrynnError::invalid_storage_file(
						"HOP lower bound is not a non-negative integer",
					));
				}
				None => 0,
			};
			bounds.insert(node, bound);
			Ok(bound)
		};
		let maximum = if initial_bound.is_some() {
			let distance = shortest_hop_distance(&index, source, target, control, &mut lower)?;
			if distance.is_none() {
				return Ok(None);
			}
			distance
		} else {
			control.record_landmark_fallback();
			None
		};
		let mut queue = VecDeque::from([(source, 0_usize)]);
		let mut predecessor = BTreeMap::<AtomId, (AtomId, crate::graph::TraversalStep)>::new();
		let mut seen = BTreeSet::from([source]);
		while let Some((current, depth)) = queue.pop_front() {
			check()?;
			if current == target {
				break;
			}
			for (candidate, step) in
				index.steps(&EdgeEndpoint::Local(current), TraversalDirection::Forward).enumerate()
			{
				control.record_path_expansion()?;
				if candidate & 1023 == 0 {
					check()?;
				}
				let Some(next) = step.target().local_id().filter(|id| id.is_node()) else {
					continue;
				};
				let depth = depth
					.checked_add(1)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("path length"))?;
				if let Some(maximum) = maximum
					&& depth.checked_add(lower(next)?).is_none_or(|bound| bound > maximum)
				{
					continue;
				}
				if seen.insert(next) {
					predecessor.insert(next, (current, step.clone()));
					queue.push_back((next, depth));
				}
			}
		}
		if !seen.contains(&target) {
			return Ok(None);
		}
		let mut chain = Vec::new();
		let mut current = target;
		while current != source {
			let (parent, step) = predecessor.get(&current).cloned().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("Shortest-path predecessor chain is incomplete.")
			})?;
			chain.push((current, step));
			current = parent;
		}
		chain.reverse();
		let source_atom = graph
			.atom_ref(source)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("node", source.to_string()))?;
		admit(source_atom.id().estimated_heap_bytes())?;
		let mut steps = vec![PathStep::new(
			AtomOccurrenceRef::new(source_atom, reference.clone()),
			None,
			PathDirection::Forward,
			EndpointRole::Source,
		)];
		for (node, link) in chain {
			let atom = graph
				.atom_ref(node)?
				.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("node", node.to_string()))?;
			let edge = graph.atom_ref(link.via_edge())?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist("edge", link.via_edge().to_string())
			})?;
			admit(
				atom.id().estimated_heap_bytes().saturating_add(edge.id().estimated_heap_bytes()),
			)?;
			steps.push(PathStep::new(
				AtomOccurrenceRef::new(atom, reference.clone()),
				Some(AtomOccurrenceRef::new(edge, reference.clone())),
				link.direction(),
				link.role(),
			));
		}
		Ok(Some(Path::new(
			graph.identity(),
			graph.snapshot_id(),
			steps,
			Some(Number::from(predecessor_distance(&predecessor, source, target)?)),
		)))
	}
}

/// Returns occurrence for this module.
pub(super) fn occurrence(value: &Value) -> FerrosGrynnResult<&AtomOccurrenceRef> {
	value.as_atom_occurrence().ok_or_else(|| {
		FerrosGrynnError::traversal_source_type_mismatch(
			"node occurrence".to_owned(),
			value.kind().to_string(),
		)
	})
}

/// Returns non negative usize for this module.
pub(super) fn non_negative_usize(value: Option<&Value>) -> FerrosGrynnResult<usize> {
	let value = value.ok_or_else(|| FerrosGrynnError::invalid_argument("Missing depth."))?;
	value
		.as_number()
		.and_then(|number| i64::try_from(number).ok())
		.and_then(|number| usize::try_from(number).ok())
		.ok_or_else(|| {
			FerrosGrynnError::traversal_source_type_mismatch(
				"non-negative integer".to_owned(),
				value.kind().to_string(),
			)
		})
}

/// Returns target id for this module.
fn target_id<Graph: GraphRead>(
	graph: &Graph,
	scope: &GraphRef,
	value: Option<&Value>,
) -> FerrosGrynnResult<AtomId> {
	let value = value.ok_or_else(|| FerrosGrynnError::invalid_argument("Missing target atom."))?;
	let atom = match value {
		Value::AtomOccurrence(target) => {
			if target.graph().id() != scope.id() {
				return Err(domain_error("algorithm source and target belong to different graphs"));
			}
			Cow::Borrowed(target.atom())
		}
		Value::Atom(atom) => Cow::Borrowed(atom.as_ref()),
		Value::Record(record) => Cow::Owned(resolve_record_atom(graph, record)?),
		_ => {
			return Err(FerrosGrynnError::traversal_source_type_mismatch(
				"record, atom or atom occurrence".to_owned(),
				value.kind().to_string(),
			));
		}
	};
	graph
		.atom_id(atom.as_ref())?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("node", atom.id().to_string()))
}

/// Returns occurrence value for this module.
fn occurrence_value<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	id: AtomId,
) -> FerrosGrynnResult<Value> {
	let atom = graph
		.atom_ref(id)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", id.to_string()))?;
	Ok(Value::AtomOccurrence(Box::new(AtomOccurrenceRef::new(atom, reference))))
}

/// Returns occurrence values for this module.
fn occurrence_values<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	ids: impl IntoIterator<Item = AtomId>,
) -> FerrosGrynnResult<Vec<Value>> {
	ids.into_iter().map(|id| occurrence_value(graph, reference.clone(), id)).collect()
}

/// Collects distinct node identities from traversers after checking that they
/// share one graph.
#[allow(
	clippy::mutable_key_type,
	reason = "AtomRef ordering uses immutable atom kind and RecordId fields"
)]
fn induced_atoms(input: &[Traverser]) -> FerrosGrynnResult<(Arc<GraphRef>, BTreeSet<AtomRef>)> {
	let first = input
		.first()
		.ok_or_else(|| domain_error("an induced graph algorithm requires at least one node"))?;
	let first = occurrence(&first.current)?;
	let graph = Arc::new(first.graph().clone());
	let mut atoms = BTreeSet::new();
	for traverser in input {
		let occurrence = occurrence(&traverser.current)?;
		if occurrence.graph().id() != graph.id() {
			return Err(domain_error("an induced graph cannot span multiple graph identities"));
		}
		if occurrence.atom().kind() != AtomKind::Node {
			return Err(domain_error("an induced graph accepts node occurrences only"));
		}
		atoms.insert(occurrence.atom().clone());
	}
	Ok((graph, atoms))
}

/// Returns induced edges for this module.
fn induced_edges<Graph: GraphRead>(
	graph: &Graph,
	nodes: &BTreeSet<AtomId>,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<BTreeSet<AtomId>> {
	let mut edges = BTreeSet::new();
	for (candidate, edge) in graph.edge_ids()?.enumerate() {
		if candidate & 1023 == 0 {
			check()?;
		}
		let edge = edge?;
		let edge_view = graph
			.edge(edge)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge.to_string()))?;
		let topology = edge_view.topology()?;
		let endpoints = binary_local_endpoints(topology.as_ref());
		if endpoints.is_some_and(|(left, right)| nodes.contains(&left) && nodes.contains(&right)) {
			edges.insert(edge);
		}
	}
	Ok(edges)
}

/// Returns binary local endpoints for this module.
fn binary_local_endpoints(topology: &EdgeTopology) -> Option<(AtomId, AtomId)> {
	let endpoints = match topology {
		EdgeTopology::Directed {
			source,
			target,
		}
		| EdgeTopology::Bidirectional {
			source,
			target,
		} if source.len() == 1 && target.len() == 1 => (&source[0], &target[0]),
		EdgeTopology::Undirected {
			endpoints,
		} if endpoints.len() == 2 => (&endpoints[0], &endpoints[1]),
		_ => return None,
	};
	Some((endpoints.0.local_id()?, endpoints.1.local_id()?))
}

/// Returns components value for this module.
fn components_value<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	components: Vec<Vec<AtomId>>,
) -> FerrosGrynnResult<Value> {
	components
		.into_iter()
		.map(|component| {
			occurrence_values(graph, reference.clone(), component)
				.map(|values| Value::Array(values.into()))
		})
		.collect::<FerrosGrynnResult<Array>>()
		.map(Value::Array)
}

/// Returns condensation value for this module.
fn condensation_value<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	value: Condensation,
) -> FerrosGrynnResult<Value> {
	let components = value
		.components()
		.iter()
		.map(|component| {
			occurrence_values(graph, reference.clone(), component.iter().copied())
				.map(|values| Value::Array(values.into()))
		})
		.collect::<FerrosGrynnResult<Array>>()?;
	let edges = value
		.edges()
		.iter()
		.map(|(source, target)| {
			Value::Array(Array::from(vec![Value::from(*source), Value::from(*target)]))
		})
		.collect::<Array>();
	let mut object = Object::new();
	object.insert("components", Value::Array(components));
	object.insert("edges", Value::Array(edges));
	Ok(Value::Object(object))
}

/// Returns spanning value for this module.
fn spanning_value<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	value: SpanningForest,
) -> FerrosGrynnResult<Value> {
	let mut object = Object::new();
	object.insert(
		"edges",
		Value::Array(occurrence_values(graph, reference, value.edges().iter().copied())?.into()),
	);
	object.insert("total_weight", Value::Number(value.total_weight()));
	object.insert("component_count", Value::from(value.component_count()));
	Ok(Value::Object(object))
}

/// Returns cuts value for this module.
fn cuts_value<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	value: CutSet,
) -> FerrosGrynnResult<Value> {
	let mut object = Object::new();
	object.insert(
		"articulation_points",
		Value::Array(
			occurrence_values(
				graph,
				reference.clone(),
				value.articulation_points().iter().copied(),
			)?
			.into(),
		),
	);
	object.insert(
		"bridges",
		Value::Array(occurrence_values(graph, reference, value.bridges().iter().copied())?.into()),
	);
	Ok(Value::Object(object))
}

/// Returns graph is directed for this module.
fn graph_is_directed<Graph: GraphRead>(
	graph: &Graph,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<bool> {
	for (candidate, edge) in graph.edge_ids()?.enumerate() {
		if candidate & 1023 == 0 {
			check()?;
		}
		let edge = edge?;
		let edge_view = graph
			.edge(edge)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge.to_string()))?;
		let topology = edge_view.topology()?;
		if !matches!(topology.as_ref(), EdgeTopology::Undirected { .. }) {
			return Ok(true);
		}
	}
	Ok(false)
}

/// Finds an exact HOP distance with reopening, using only admissible bounds.
/// Parent selection belongs to the subsequent canonical BFS pass.
fn shortest_hop_distance(
	index: &TraversalIndex,
	source: AtomId,
	target: AtomId,
	control: &ExecutionControl,
	lower: &mut impl FnMut(AtomId) -> FerrosGrynnResult<usize>,
) -> FerrosGrynnResult<Option<usize>> {
	let mut memory = control.barrier_account();
	let mut reserve = |bytes| {
		if memory.reserve(bytes) {
			Ok(())
		} else {
			Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"A* frontier bytes",
				control.barrier_memory_limit() as u64,
			))
		}
	};
	reserve(256)?;
	let mut queue = BinaryHeap::from([Reverse((lower(source)?, 0_usize, source))]);
	let mut distances = BTreeMap::from([(source, 0_usize)]);
	while let Some(Reverse((_priority, distance, node))) = queue.pop() {
		control.check()?;
		if distances.get(&node).is_some_and(|known| distance > *known) {
			continue;
		}
		if node == target {
			return Ok(Some(distance));
		}
		for step in index.steps(&EdgeEndpoint::Local(node), TraversalDirection::Forward) {
			control.record_path_expansion()?;
			let Some(next) = step.target().local_id().filter(|id| id.is_node()) else {
				continue;
			};
			let distance = distance
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("path length"))?;
			if distances.get(&next).is_none_or(|known| distance < *known) {
				let priority = distance
					.checked_add(lower(next)?)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("HOP completion bound"))?;
				// Three simultaneous heap buffers at growth plus one BTree
				// entry. Keeping the charge until this pass ends is
				// conservative.
				reserve(256)?;
				distances.insert(next, distance);
				queue.push(Reverse((priority, distance, next)));
			}
		}
	}
	Ok(None)
}

/// Returns predecessor distance for this module.
fn predecessor_distance(
	predecessor: &BTreeMap<AtomId, (AtomId, crate::graph::TraversalStep)>,
	source: AtomId,
	mut target: AtomId,
) -> FerrosGrynnResult<usize> {
	let mut distance = 0_usize;
	while target != source {
		target = predecessor.get(&target).map(|(parent, _)| *parent).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Shortest-path chain is incomplete.")
		})?;
		distance = distance
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("path length"))?;
	}
	Ok(distance)
}

/// Returns lowest common ancestor for this module.
fn lowest_common_ancestor<Graph: GraphRead>(
	graph: &Graph,
	left: AtomId,
	right: AtomId,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Option<AtomId>> {
	if !matches!(graph.kind(), GraphKind::Tree | GraphKind::RootedTree) {
		return Err(domain_error("tv::lca requires a tree or rooted-tree graph"));
	}
	let mut parents = BTreeMap::<AtomId, AtomId>::new();
	for (candidate, edge) in graph.edge_ids()?.enumerate() {
		if candidate & 1023 == 0 {
			check()?;
		}
		let edge = edge?;
		let edge_view = graph
			.edge(edge)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge.to_string()))?;
		let topology = edge_view.topology()?;
		let EdgeTopology::Directed {
			source,
			target,
		} = topology.as_ref()
		else {
			return Err(domain_error("tv::lca requires directed parent-child edges"));
		};
		if source.len() != 1 || target.len() != 1 {
			return Err(domain_error("tv::lca requires binary parent-child edges"));
		}
		let (Some(parent), Some(child)) = (source[0].local_id(), target[0].local_id()) else {
			return Err(domain_error("tv::lca does not accept cross-level edges"));
		};
		if parents.insert(child, parent).is_some() {
			return Err(domain_error("tv::lca requires at most one parent per node"));
		}
	}
	let mut left_ancestors = BTreeSet::new();
	let mut current = Some(left);
	while let Some(node) = current {
		check()?;
		if !left_ancestors.insert(node) {
			return Err(domain_error("tv::lca detected a cycle in the parent relation"));
		}
		current = parents.get(&node).copied();
	}
	let mut seen = BTreeSet::new();
	let mut current = Some(right);
	while let Some(node) = current {
		check()?;
		if left_ancestors.contains(&node) {
			return Ok(Some(node));
		}
		if !seen.insert(node) {
			return Err(domain_error("tv::lca detected a cycle in the parent relation"));
		}
		current = parents.get(&node).copied();
	}
	Ok(None)
}

/// Returns domain error for this module.
fn domain_error(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("traversal algorithm", reason)
}

/// Borrowed graph reader restricted to selected node and edge identifiers.
struct InducedGraph<'a, Graph: GraphRead> {
	/// Graph reader whose node and edge sets are restricted by this induced
	/// view.
	inner: &'a Graph,
	/// Selected node identifiers or retained physical node storage.
	nodes: BTreeSet<AtomId>,
	/// Selected edge identifiers or retained physical edge storage.
	edges: BTreeSet<AtomId>,
}

impl<Graph: GraphRead> GraphRead for InducedGraph<'_, Graph> {
	type Edge<'a>
		= Graph::Edge<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;
	type Node<'a>
		= Graph::Node<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;

	/// Returns identity for `InducedGraph<'_, Graph>`.
	fn identity(&self) -> Uuid {
		self.inner.identity()
	}

	/// Returns revision for `InducedGraph<'_, Graph>`.
	fn revision(&self) -> u64 {
		self.inner.revision()
	}

	/// Returns kind for `InducedGraph<'_, Graph>`.
	fn kind(&self) -> GraphKind {
		self.inner.kind()
	}

	/// Returns properties for `InducedGraph<'_, Graph>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		self.inner.properties()
	}

	/// Returns node ids for `InducedGraph<'_, Graph>`.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		Ok(Box::new(self.nodes.iter().copied().map(Ok)))
	}

	/// Returns edge ids for `InducedGraph<'_, Graph>`.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		Ok(Box::new(self.edges.iter().copied().map(Ok)))
	}

	/// Returns node for `InducedGraph<'_, Graph>`.
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		if self.nodes.contains(&id) {
			self.inner.node(id)
		} else {
			Ok(None)
		}
	}

	/// Returns edge for `InducedGraph<'_, Graph>`.
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		if self.edges.contains(&id) {
			self.inner.edge(id)
		} else {
			Ok(None)
		}
	}

	/// Returns storage mode for `InducedGraph<'_, Graph>`.
	fn storage_mode(&self) -> GraphStorageMode {
		self.inner.storage_mode()
	}

	/// Returns snapshot id for `InducedGraph<'_, Graph>`.
	fn snapshot_id(&self) -> SnapshotId {
		self.inner.snapshot_id()
	}

	/// Returns graph ref for `InducedGraph<'_, Graph>`.
	fn graph_ref(&self) -> Option<&GraphRef> {
		self.inner.graph_ref()
	}
}

#[cfg(test)]
/// Regression tests for algorithms behavior.
mod tests {
	use super::*;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies hop astar reopens improved vertices for inconsistent admissible
	/// bounds.
	#[test]
	fn hop_astar_reopens_improved_vertices_for_inconsistent_admissible_bounds() {
		let mut graph = crate::graph::Graph::<crate::graph::Mutable>::new();
		let nodes = (0..9)
			.map(|_| graph.add_node(None, PropertyMap::new()).assert_ok())
			.collect::<Vec<_>>();
		// The long prefix reaches v before c; h(c)=5 is admissible but
		// inconsistent with h(v)=0. Reopening v improves the answer to 6.
		for (source, target) in
			[(0, 1), (1, 2), (2, 3), (0, 4), (4, 3), (3, 5), (5, 6), (6, 7), (7, 8)]
		{
			graph
				.add_edge(
					EdgeTopology::directed([nodes[source]], [nodes[target]]),
					None,
					PropertyMap::new(),
				)
				.assert_ok();
		}
		let index = TraversalIndex::new(&graph, TraversalSemantics::Node).assert_ok();
		let control = ExecutionControl::new(&crate::gql::QueryExecutionOptions::default(), None);
		assert_eq!(
			shortest_hop_distance(&index, nodes[0], nodes[8], &control, &mut |node| Ok(
				if node == nodes[4] {
					5
				} else {
					0
				}
			))
			.assert_ok(),
			Some(6)
		);
	}
}
