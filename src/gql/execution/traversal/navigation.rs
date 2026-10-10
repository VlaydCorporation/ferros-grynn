//! Occurrence-aware graph navigation for the traversal VM.

use super::*;

const GRAPH_SCAN_WINDOW: usize = 256;

pub(super) const fn is_navigation(kind: TraverseStepKind) -> bool {
	matches!(
		kind,
		TraverseStepKind::Vertices
			| TraverseStepKind::Edges
			| TraverseStepKind::Out
			| TraverseStepKind::In
			| TraverseStepKind::Both
			| TraverseStepKind::OutEdges
			| TraverseStepKind::InEdges
			| TraverseStepKind::BothEdges
			| TraverseStepKind::InVertex
			| TraverseStepKind::OutVertex
			| TraverseStepKind::Hyper
			| TraverseStepKind::MetaInner
	)
}

impl<Read: QueryRead> TraversalMachine<'_, Read> {
	pub(super) fn navigate(
		&mut self,
		stage: usize,
		step: &PreparedStep,
		traverser: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		match step.kind {
			TraverseStepKind::Vertices => self.start_graph_scan(stage, traverser, AtomKind::Node),
			TraverseStepKind::Edges => self.start_graph_scan(stage, traverser, AtomKind::Edge),
			TraverseStepKind::MetaInner => self.meta_inner(traverser),
			TraverseStepKind::InVertex | TraverseStepKind::OutVertex => {
				self.edge_endpoints(step.kind, traverser)
			}
			TraverseStepKind::Out
			| TraverseStepKind::In
			| TraverseStepKind::Both
			| TraverseStepKind::OutEdges
			| TraverseStepKind::InEdges
			| TraverseStepKind::BothEdges
			| TraverseStepKind::Hyper => self.expand(step, traverser),
			_ => Err(FerrosGrynnError::invalid_argument(
				"A non-navigation step reached the graph navigator.",
			)),
		}
	}

	fn start_graph_scan(
		&mut self,
		stage: usize,
		traverser: Traverser,
		kind: AtomKind,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let Value::GraphRef(reference) = &traverser.current else {
			return Err(FerrosGrynnError::traversal_source_type_mismatch(
				"GraphRef".to_owned(),
				traverser.current.kind().to_string(),
			));
		};
		let reference = Arc::new(reference.as_ref().clone());
		let scan = GraphScan {
			input: traverser,
			reference,
			kind,
			offset: 0,
		};
		let StageState::GraphScan(pending) = &mut self.states[stage] else {
			return Err(FerrosGrynnError::invalid_argument(
				"Traversal graph scan state is unavailable.",
			));
		};
		*pending = Some(scan);
		self.resume_graph_scan(stage)
	}

	pub(super) fn resume_graph_scan(&mut self, stage: usize) -> FerrosGrynnResult<Vec<Traverser>> {
		let StageState::GraphScan(pending) = &mut self.states[stage] else {
			return Err(FerrosGrynnError::invalid_argument(
				"Traversal graph scan state is unavailable.",
			));
		};
		let Some(mut scan) = pending.take() else {
			return Ok(Vec::new());
		};
		let (atoms, origin, snapshot, has_more) = {
			let graph = self.read.graph(self.snapshot, &scan.reference)?;
			let (ids, has_more) = match scan.kind {
				AtomKind::Node => graph_scan_page(graph.node_ids()?, scan.offset)?,
				AtomKind::Edge => graph_scan_page(graph.edge_ids()?, scan.offset)?,
			};
			let atoms = ids
				.into_iter()
				.map(|id| {
					graph.atom_ref(id)?.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"Traversal graph iterator yielded an absent atom",
						)
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			(atoms, graph.identity(), graph.snapshot_id(), has_more)
		};
		if has_more {
			scan.offset = scan.offset.checked_add(GRAPH_SCAN_WINDOW).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("traversal graph scan offset")
			})?;
			let StageState::GraphScan(pending) = &mut self.states[stage] else {
				return Err(FerrosGrynnError::invalid_argument(
					"Traversal graph scan state changed while it was being read.",
				));
			};
			*pending = Some(scan.clone());
		}
		let mut output = Vec::with_capacity(atoms.len());
		for atom in atoms {
			let occurrence = AtomOccurrenceRef::new(atom, scan.reference.clone());
			let mut next = scan.input.clone();
			next.current = Value::AtomOccurrence(Box::new(occurrence));
			self.seed_trace(&mut next, origin, snapshot)?;
			output.push(next);
		}
		Ok(output)
	}

	fn expand(
		&mut self,
		step: &PreparedStep,
		traverser: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let occurrence = occurrence(&traverser.current)?.clone();
		let source = self.bound_atom(&occurrence)?;
		let directions: &[crate::syn::MatchDirection] = match step.kind {
			TraverseStepKind::Out | TraverseStepKind::OutEdges => {
				&[crate::syn::MatchDirection::Forward, crate::syn::MatchDirection::Undirected]
			}
			TraverseStepKind::In | TraverseStepKind::InEdges => {
				&[crate::syn::MatchDirection::Backward, crate::syn::MatchDirection::Undirected]
			}
			TraverseStepKind::Both | TraverseStepKind::BothEdges | TraverseStepKind::Hyper => &[
				crate::syn::MatchDirection::Forward,
				crate::syn::MatchDirection::Backward,
				crate::syn::MatchDirection::Undirected,
			],
			_ => &[],
		};
		let candidates = if source.atom.kind() == Some(AtomKind::Edge) && !self.atom_semantics {
			Vec::new()
		} else if let Some(physical) = self.read.physical_traversal_candidates(
			self.snapshot,
			occurrence.graph(),
			source.atom,
			directions,
		)? {
			self.control.record_physical_traversal(
				physical.resident_pages,
				physical.wal_pages,
				physical.sync_home_pages,
				physical.ioring_home_pages,
			);
			physical.candidates
		} else if let Some(candidates) = self.local_candidates(source, directions)? {
			candidates
		} else {
			let index = self.scoped_index(occurrence.graph())?;
			let mut candidates = Vec::new();
			for direction in directions {
				for candidate in index.candidates(source, *direction) {
					if !candidates.contains(&candidate) {
						candidates.push(candidate);
					}
				}
			}
			candidates
		};
		let edge_output = matches!(
			step.kind,
			TraverseStepKind::OutEdges
				| TraverseStepKind::InEdges
				| TraverseStepKind::BothEdges
				| TraverseStepKind::Hyper
		);
		let expected_label = step.arguments.first().and_then(Value::as_str);
		#[allow(
			clippy::mutable_key_type,
			reason = "occurrence ordering uses immutable graph and atom identities"
		)]
		let mut emitted_edges = std::collections::BTreeSet::new();
		let mut output = Vec::new();
		for candidate in candidates {
			self.control.record_path_expansion()?;
			let edge = self.bound_occurrence(candidate.edge)?;
			if let Some(label) = expected_label
				&& self.field_value(&Value::AtomOccurrence(Box::new(edge.clone())), "label")?
					!= Value::from(label)
			{
				continue;
			}
			if step.kind == TraverseStepKind::Hyper && !self.edge_is_hyper(candidate.edge)? {
				continue;
			}
			let mut next = traverser.clone();
			if edge_output {
				if !emitted_edges.insert(edge.clone()) {
					continue;
				}
				if self.path_tracking && next.trace.is_none() {
					let (origin, snapshot) = {
						let graph = self.read.graph(self.snapshot, occurrence.graph())?;
						(graph.identity(), graph.snapshot_id())
					};
					self.seed_trace(&mut next, origin, snapshot)?;
				}
				next.current = Value::AtomOccurrence(Box::new(edge));
			} else {
				let target = self.bound_occurrence(candidate.target)?;
				next.current = Value::AtomOccurrence(Box::new(target.clone()));
				self.append_trace(
					&mut next,
					&occurrence,
					target,
					edge,
					candidate.direction,
					candidate.role,
				)?;
			}
			output.push(next);
		}
		Ok(output)
	}

	fn local_candidates(
		&mut self,
		source: BoundAtom,
		directions: &[crate::syn::MatchDirection],
	) -> FerrosGrynnResult<Option<Vec<ScopedTraversalCandidate>>> {
		let endpoint_index = if source.atom.kind() == Some(AtomKind::Edge) && self.atom_semantics {
			let reference = self.read.graph_reference(self.snapshot, source.graph)?;
			let graph = self.read.graph(self.snapshot, &reference)?;
			TraversalIndex::for_edge_endpoint(&graph, &EdgeEndpoint::Local(source.atom))?
				.map(Rc::new)
		} else {
			None
		};
		let index = if let Some(index) = endpoint_index {
			Some(index)
		} else if let Some(index) = self.local_indexes.get(&source.graph) {
			index.clone()
		} else {
			let reference = self.read.graph_reference(self.snapshot, source.graph)?;
			let graph = self.read.graph(self.snapshot, &reference)?;
			let semantics = if self.atom_semantics {
				TraversalSemantics::Atom
			} else {
				TraversalSemantics::Node
			};
			let index = TraversalIndex::new_for_expansion(&graph, semantics)?;
			let index = (!index.has_nested_endpoints()).then(|| Rc::new(index));
			self.local_indexes.insert(source.graph, index.clone());
			index
		};
		let Some(index) = index else {
			return Ok(None);
		};
		let reference = Arc::new(self.read.graph_reference(self.snapshot, source.graph)?);
		let graph = self.read.graph(self.snapshot, &reference)?;
		let mut candidates = Vec::new();
		let direction = if directions.contains(&crate::syn::MatchDirection::Forward)
			&& directions.contains(&crate::syn::MatchDirection::Backward)
		{
			TraversalDirection::Both
		} else if directions.contains(&crate::syn::MatchDirection::Forward) {
			TraversalDirection::Forward
		} else {
			TraversalDirection::Backward
		};
		for step in index.steps(&EdgeEndpoint::Local(source.atom), direction) {
			let EdgeEndpoint::Local(target) = step.target() else {
				return Ok(None);
			};
			let candidate = ScopedTraversalCandidate {
				target: BoundAtom::new(source.graph, *target),
				edge: BoundAtom::new(source.graph, step.via_edge()),
				direction: step.direction(),
				role: step.role(),
				topology: step.topology(),
				cross_level: false,
			};
			for atom in [candidate.target, candidate.edge] {
				if let std::collections::btree_map::Entry::Vacant(entry) =
					self.occurrences.entry(atom)
				{
					let atom_ref = graph.atom_ref(atom.atom)?.ok_or_else(|| {
						FerrosGrynnError::atom_does_not_exist("atom", atom.atom.to_string())
					})?;
					entry.insert(AtomOccurrenceRef::new(atom_ref, reference.clone()));
				}
			}
			if !candidates.contains(&candidate) {
				candidates.push(candidate);
			}
		}
		Ok(Some(candidates))
	}

	fn edge_endpoints(
		&mut self,
		kind: TraverseStepKind,
		traverser: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let edge_occurrence = occurrence(&traverser.current)?.clone();
		if edge_occurrence.atom().kind() != AtomKind::Edge {
			return Err(FerrosGrynnError::traversal_source_type_mismatch(
				"edge occurrence".to_owned(),
				format!("{} occurrence", edge_occurrence.atom().kind()),
			));
		}
		let parent = Arc::new(edge_occurrence.graph().clone());
		let graph = self.read.graph(self.snapshot, &parent)?;
		let edge_id = graph.atom_id(edge_occurrence.atom())?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("edge", edge_occurrence.atom().id().to_string())
		})?;
		let edge = graph
			.edge(edge_id)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge_id.to_string()))?;
		let topology = edge.topology()?;
		let endpoints = match (kind, topology.as_ref()) {
			(
				TraverseStepKind::OutVertex,
				EdgeTopology::Directed {
					source,
					..
				}
				| EdgeTopology::Bidirectional {
					source,
					..
				},
			) => source.as_slice(),
			(
				TraverseStepKind::InVertex,
				EdgeTopology::Directed {
					target,
					..
				}
				| EdgeTopology::Bidirectional {
					target,
					..
				},
			) => target.as_slice(),
			(
				_,
				EdgeTopology::Undirected {
					endpoints,
				},
			) => endpoints.as_slice(),
			_ => &[],
		};
		let fields = MatchGraphFields::new(
			&*self.read,
			self.snapshot,
			self.read.property_dictionary(self.snapshot),
			parent.clone(),
		);
		let mut targets = Vec::with_capacity(endpoints.len());
		for endpoint in endpoints {
			self.control.record_path_expansion()?;
			let bound =
				resolve_scoped_endpoint(&*self.read, self.snapshot, &fields, &parent, endpoint)?;
			if !self.atom_semantics && bound.atom.kind() != Some(AtomKind::Node) {
				continue;
			}
			targets.push(bound);
		}
		drop(fields);
		drop(topology);
		drop(edge);
		drop(graph);
		let mut output = Vec::with_capacity(targets.len());
		for bound in targets {
			let target = self.bound_occurrence(bound)?;
			let mut next = traverser.clone();
			next.current = Value::AtomOccurrence(Box::new(target.clone()));
			self.append_trace(
				&mut next,
				&edge_occurrence,
				target,
				edge_occurrence.clone(),
				if kind == TraverseStepKind::InVertex {
					PathDirection::Forward
				} else {
					PathDirection::Reverse
				},
				if kind == TraverseStepKind::InVertex {
					EndpointRole::Target
				} else {
					EndpointRole::Source
				},
			)?;
			output.push(next);
		}
		Ok(output)
	}

	fn meta_inner(&self, mut traverser: Traverser) -> FerrosGrynnResult<Vec<Traverser>> {
		let occurrence = occurrence(&traverser.current)?.clone();
		let owner = self.bound_atom(&occurrence)?;
		if let Some(physical) =
			self.read.physical_inner_graph(self.snapshot, occurrence.graph(), owner.atom)?
		{
			self.control.record_physical_traversal(
				physical.resident_pages,
				physical.wal_pages,
				physical.sync_home_pages,
				physical.ioring_home_pages,
			);
			let Some(graph) = physical.graph else {
				return Ok(Vec::new());
			};
			traverser.current =
				Value::GraphRef(Box::new(self.read.graph_reference(self.snapshot, graph)?));
			return Ok(vec![traverser]);
		}
		let root = Arc::new(occurrence.graph().clone());
		let fields = MatchGraphFields::new(
			&*self.read,
			self.snapshot,
			self.read.property_dictionary(self.snapshot),
			root,
		);
		let value = fields.inner_graph(&occurrence)?;
		if value.is_none() {
			return Ok(Vec::new());
		}
		traverser.current = value;
		Ok(vec![traverser])
	}

	fn scoped_index(&mut self, root: &GraphRef) -> FerrosGrynnResult<Rc<ScopedTraversalIndex>> {
		if let Some(index) = self.scoped_indexes.get(&root.id()) {
			return Ok(index.clone());
		}
		let root = Arc::new(root.clone());
		let fields = MatchGraphFields::new(
			&*self.read,
			self.snapshot,
			self.read.property_dictionary(self.snapshot),
			root.clone(),
		);
		let semantics = if self.atom_semantics {
			TraversalSemantics::Atom
		} else {
			TraversalSemantics::Node
		};
		let index = Rc::new(ScopedTraversalIndex::build(
			&*self.read,
			self.snapshot,
			root.clone(),
			&fields,
			semantics,
			self.control,
		)?);
		self.scoped_indexes.insert(root.id(), index.clone());
		Ok(index)
	}

	fn bound_atom(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<BoundAtom> {
		let graph = self.read.graph(self.snapshot, occurrence.graph())?;
		let atom = graph.atom_id(occurrence.atom())?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist(
				occurrence.atom().kind().to_string(),
				occurrence.atom().id().to_string(),
			)
		})?;
		Ok(BoundAtom::new(occurrence.graph().id(), atom))
	}

	fn bound_occurrence(&mut self, atom: BoundAtom) -> FerrosGrynnResult<AtomOccurrenceRef> {
		if let Some(occurrence) = self.occurrences.get(&atom) {
			return Ok(occurrence.clone());
		}
		let reference = Arc::new(self.read.graph_reference(self.snapshot, atom.graph)?);
		let graph = self.read.graph(self.snapshot, &reference)?;
		let bound = atom;
		let atom = graph
			.atom_ref(atom.atom)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", atom.atom.to_string()))?;
		let occurrence = AtomOccurrenceRef::new(atom, reference);
		self.occurrences.insert(bound, occurrence.clone());
		Ok(occurrence)
	}

	pub(super) fn field_value(&self, value: &Value, field: &str) -> FerrosGrynnResult<Value> {
		match value {
			Value::Object(object) => Ok(object.get(field).cloned().unwrap_or(Value::None)),
			Value::Record(record) => {
				self.read.record_field(self.snapshot, record, None, field, Some(self.control))
			}
			Value::AtomOccurrence(occurrence) => {
				let dictionary = self.read.property_dictionary(self.snapshot);
				let key = dictionary.get(field);
				if (field == "properties" || key.is_some())
					&& let Some(physical) = self.read.physical_occurrence_fields(
						self.snapshot,
						occurrence.graph(),
						self.bound_atom(occurrence)?.atom,
					)? {
					self.control.record_physical_traversal(
						physical.resident_pages,
						physical.wal_pages,
						physical.sync_home_pages,
						physical.ioring_home_pages,
					);
					return if field == "properties" {
						Ok(Value::Object(properties_object(&physical.properties, &dictionary)?))
					} else {
						Ok(key
							.and_then(|key| physical.properties.get(key).cloned())
							.unwrap_or(Value::None))
					};
				}
				let root = Arc::new(occurrence.graph().clone());
				let fields = MatchGraphFields::new(&*self.read, self.snapshot, dictionary, root);
				fields.field(self.bound_atom(occurrence)?, field)
			}
			value => Err(FerrosGrynnError::traversal_source_type_mismatch(
				"object, record, or atom occurrence".to_owned(),
				value.kind().to_string(),
			)),
		}
	}

	fn edge_is_hyper(&self, edge: BoundAtom) -> FerrosGrynnResult<bool> {
		let reference = self.read.graph_reference(self.snapshot, edge.graph)?;
		let graph = self.read.graph(self.snapshot, &reference)?;
		let edge = graph
			.edge(edge.atom)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge.atom.to_string()))?;
		Ok(edge.topology()?.is_hyper())
	}

	fn seed_trace(
		&mut self,
		traverser: &mut Traverser,
		origin: crate::values::Uuid,
		snapshot: crate::graph::SnapshotId,
	) -> FerrosGrynnResult<()> {
		if !self.path_tracking || traverser.trace.is_some() {
			return Ok(());
		}
		let occurrence = occurrence(&traverser.current)?.clone();
		traverser.trace = Some(self.push_trace_node(
			None,
			PathStep::new(occurrence, None, PathDirection::Forward, EndpointRole::Source),
			Some(Number::Int(0)),
		)?);
		traverser.trace_origin = Some(origin);
		traverser.trace_snapshot = Some(snapshot);
		Ok(())
	}

	fn append_trace(
		&mut self,
		traverser: &mut Traverser,
		source: &AtomOccurrenceRef,
		target: AtomOccurrenceRef,
		edge: AtomOccurrenceRef,
		direction: PathDirection,
		role: EndpointRole,
	) -> FerrosGrynnResult<()> {
		if !self.path_tracking && self.custom_cost.is_none() {
			return Ok(());
		}
		if self.path_tracking && traverser.trace.is_none() {
			let (origin, snapshot) = {
				let graph = self.read.graph(self.snapshot, source.graph())?;
				(graph.identity(), graph.snapshot_id())
			};
			traverser.trace = Some(self.push_trace_node(
				None,
				PathStep::new(source.clone(), None, PathDirection::Forward, EndpointRole::Source),
				Some(Number::Int(0)),
			)?);
			traverser.trace_origin = Some(origin);
			traverser.trace_snapshot = Some(snapshot);
		}
		let weight = if let Some(cost) = self.custom_cost {
			let fields = MatchGraphFields::new(
				&*self.read,
				self.snapshot,
				self.read.property_dictionary(self.snapshot),
				Arc::new(edge.graph().clone()),
			);
			custom_path_contribution(
				cost,
				edge.clone(),
				traverser.trace_cost,
				MatchPredicateContext {
					slots: self.slots,
					source: self.source,
					fields: &fields,
				},
				self.control,
			)?
		} else {
			self.edge_weight(&edge)?
		};
		traverser.trace_cost = match binary(
			BinaryOperator::Add,
			Value::Number(traverser.trace_cost),
			Value::Number(weight),
		)? {
			Value::Number(value) => value,
			_ => {
				return Err(FerrosGrynnError::invalid_argument(
					"Traversal path weight addition did not return a number.",
				));
			}
		};
		if !self.path_tracking {
			return Ok(());
		}
		traverser.trace = Some(self.push_trace_node(
			traverser.trace,
			PathStep::new(target, Some(edge), direction, role),
			Some(traverser.trace_cost),
		)?);
		self.control.record_match_trace_node();
		Ok(())
	}

	fn edge_weight(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Number> {
		let graph = self.read.graph(self.snapshot, occurrence.graph())?;
		let edge = graph.atom_id(occurrence.atom())?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("edge", occurrence.atom().id().to_string())
		})?;
		graph
			.edge(edge)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge.to_string()))?
			.weight()
			.map_or(Ok(Number::Int(1)), Ok)
	}

	pub(super) fn materialize_path(&self, traverser: &Traverser) -> FerrosGrynnResult<Path> {
		let trace = traverser.trace.ok_or_else(|| {
			FerrosGrynnError::invalid_argument(
				"This traversal did not retain a path for the current traverser.",
			)
		})?;
		self.trace_arena.materialize(
			trace,
			traverser.trace_origin.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("Traversal path origin is unavailable.")
			})?,
			traverser.trace_snapshot.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("Traversal path snapshot is unavailable.")
			})?,
		)
	}
}

fn graph_scan_page(
	ids: impl Iterator<Item = FerrosGrynnResult<AtomId>>,
	offset: usize,
) -> FerrosGrynnResult<(Vec<AtomId>, bool)> {
	let mut page =
		ids.skip(offset).take(GRAPH_SCAN_WINDOW + 1).collect::<FerrosGrynnResult<Vec<_>>>()?;
	let has_more = page.len() > GRAPH_SCAN_WINDOW;
	if has_more {
		page.pop();
	}
	Ok((page, has_more))
}

#[cfg(test)]
mod tests {
	use super::{GRAPH_SCAN_WINDOW, graph_scan_page};
	use crate::graph::{AtomId, AtomKind};

	#[test]
	fn graph_scan_pages_are_bounded_and_resumable() {
		let ids = || (0..600).map(|slot| AtomId::from_parts(AtomKind::Node, slot, 0));
		let (first, first_more) = graph_scan_page(ids(), 0).unwrap();
		let (second, second_more) = graph_scan_page(ids(), GRAPH_SCAN_WINDOW).unwrap();
		let (third, third_more) = graph_scan_page(ids(), GRAPH_SCAN_WINDOW * 2).unwrap();
		assert_eq!(first.len(), GRAPH_SCAN_WINDOW);
		assert_eq!(second.len(), GRAPH_SCAN_WINDOW);
		assert_eq!(third.len(), 600 - GRAPH_SCAN_WINDOW * 2);
		assert!(first_more && second_more && !third_more);
		assert_eq!(first[0].slot(), 0);
		assert_eq!(second[0].slot(), GRAPH_SCAN_WINDOW as u64);
		assert_eq!(third[0].slot(), (GRAPH_SCAN_WINDOW * 2) as u64);
	}
}

fn occurrence(value: &Value) -> FerrosGrynnResult<&AtomOccurrenceRef> {
	let Value::AtomOccurrence(occurrence) = value else {
		return Err(FerrosGrynnError::traversal_source_type_mismatch(
			"atom occurrence".to_owned(),
			value.kind().to_string(),
		));
	};
	Ok(occurrence)
}
