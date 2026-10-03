//! Seed-local complete path batches through the qualified MATCH adapter.

use super::{algorithms::non_negative_usize, *};
use crate::graph::tensor::{
	TensorScope,
	control::{invalid, overflow, resource},
};

impl<Read: QueryRead> TraversalMachine<'_, Read> {
	/// Keeps seed environments/bulk while each pair retains every minimal
	/// route.
	pub(super) fn execute_all_shortest(
		&self,
		step: &PreparedStep,
		input: Vec<Traverser>,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let mut arguments_account = self.control.barrier_account();
		let argument_bytes = step.arguments.iter().try_fold(16_384_usize, |bytes, value| {
			bytes
				.checked_add(value.estimated_heap_bytes().checked_mul(4).ok_or_else(overflow)?)
				.ok_or_else(overflow)
		})?;
		if !arguments_account.reserve(argument_bytes) {
			return Err(resource(
				"all-shortest arguments",
				self.control.barrier_memory_limit() as u64,
			));
		}
		self.control.record_graph_work(
			u64::try_from((argument_bytes - 16_384) / 4).map_err(|_cause| overflow())?,
		)?;
		let empty = Object::new();
		let options = match step.arguments.get(1) {
			None => &empty,
			Some(Value::Object(value)) => value,
			Some(_) => return Err(invalid("all-shortest options must be an object")),
		};
		if options.keys().any(|key| {
			!matches!(key.as_str(), "scope" | "direction" | "labels" | "min_depth" | "max_depth")
		}) {
			return Err(invalid("unknown all-shortest option"));
		}
		let scope = match options.get("scope").and_then(Value::as_str).unwrap_or("LOCAL") {
			"LOCAL" => TensorScope::Local,
			"ROOTED" => TensorScope::Rooted,
			_ => return Err(invalid("all-shortest scope must be LOCAL or ROOTED")),
		};
		if options.get("scope").is_some_and(|value| value.as_str().is_none()) {
			return Err(invalid("all-shortest scope must be a string"));
		}
		let direction = match options.get("direction").and_then(Value::as_str).unwrap_or("OUT") {
			"OUT" => crate::syn::MatchDirection::Forward,
			"IN" => crate::syn::MatchDirection::Backward,
			"BOTH" => crate::syn::MatchDirection::Undirected,
			_ => return Err(invalid("all-shortest direction must be OUT, IN or BOTH")),
		};
		if options.get("direction").is_some_and(|value| value.as_str().is_none()) {
			return Err(invalid("all-shortest direction must be a string"));
		}
		let mut labels = std::collections::BTreeSet::new();
		if let Some(value) = options.get("labels") {
			let Value::Array(values) = value else {
				return Err(invalid("labels must be an array"));
			};
			for value in values.iter() {
				labels.insert(
					value.as_str().ok_or_else(|| invalid("labels must be strings"))?.into(),
				);
			}
		}
		let min = options
			.get("min_depth")
			.map(|value| non_negative_usize(Some(value)))
			.transpose()?
			.unwrap_or(0);
		let max =
			options.get("max_depth").map(|value| non_negative_usize(Some(value))).transpose()?;
		let mode = self.constraints.mode.unwrap_or(crate::syn::MatchPathMode::Trail);
		let weighted = step.kind == TraverseStepKind::AllWeightedShortestPaths;
		if weighted && mode == crate::syn::MatchPathMode::Walk && max.is_none() {
			return Err(invalid("ALL WEIGHTED SHORTEST WALK requires finite max_depth"));
		}
		if max.is_some_and(|max| min > max) {
			return Err(invalid("min_depth exceeds max_depth"));
		}
		let binding = |domain, selector, column| ResolvedMatchBinding {
			domain,
			selector,
			column: Some(column),
			label: None,
			properties: Vec::new(),
			predicate: None,
			meta: false,
		};
		let mut output = Vec::new();
		let mut memory = self.control.barrier_account();
		for traverser in input {
			self.control.check()?;
			let seed_bytes = traverser
				.estimated_heap_bytes()
				.checked_mul(4)
				.and_then(|bytes| bytes.checked_add(argument_bytes))
				.ok_or_else(overflow)?;
			if !arguments_account.reserve(seed_bytes) {
				return Err(resource(
					"all-shortest seed identities",
					self.control.barrier_memory_limit() as u64,
				));
			}
			{
				let seed = algorithms::occurrence(&traverser.current)?;
				let seed_ref = Arc::new(seed.graph().clone());
				let seed_graph = Rc::new(self.read.graph(self.snapshot, &seed_ref)?);
				let source = seed_graph
					.atom_id(seed.atom())?
					.ok_or_else(|| invalid("all-shortest seed is absent"))?;
				let target = match step.arguments.first() {
					Some(Value::AtomOccurrence(value)) => value.as_ref().clone(),
					Some(Value::Record(value)) => AtomOccurrenceRef::new(
						resolve_record_atom(seed_graph.as_ref(), value)?,
						seed_ref.clone(),
					),
					Some(Value::Atom(value)) => {
						AtomOccurrenceRef::new(value.as_ref().clone(), seed_ref.clone())
					}
					_ => return Err(invalid("all-shortest target must be a record or occurrence")),
				};
				let target_graph = self.read.graph(self.snapshot, target.graph())?;
				let target_id = target_graph
					.atom_id(target.atom())?
					.ok_or_else(|| invalid("all-shortest target is absent"))?;
				let root = Arc::new(self.graph.clone().unwrap_or_else(|| seed.graph().clone()));
				let fields = MatchGraphFields::new(
					&*self.read,
					self.snapshot,
					self.read.property_dictionary(self.snapshot),
					root.clone(),
				);
				let predicate = MatchPredicateContext {
					slots: self.slots,
					source: self.source,
					fields: &fields,
				};
				let mut row = Row::bindings(3);
				row.seed_binding(
					1,
					Value::from(target.clone()),
					BoundAtom::new(target.graph().id(), target_id),
					crate::syn::MatchMode::RepeatableElements,
				);
				let seed_cursor = SeedCursor::new(
					Box::new(OneRowCursor {
						row: Some(row),
					}),
					seed_graph,
					seed_ref,
					binding(
						if source.is_node() {
							EndpointDomain::Node
						} else {
							EndpointDomain::Edge
						},
						ResolvedMatchSelector::Record(seed.atom().id().clone()),
						0,
					),
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
					target: binding(
						if target_id.is_node() {
							EndpointDomain::Node
						} else {
							EndpointDomain::Edge
						},
						ResolvedMatchSelector::Record(target.atom().id().clone()),
						1,
					),
					direction,
					min: u32::try_from(min).map_err(|_cause| overflow())?,
					max: max
						.map(u32::try_from)
						.transpose()
						.map_err(|_cause| overflow())?
						.unwrap_or(u32::MAX),
					path_column: Some(2),
					match_mode: crate::syn::MatchMode::RepeatableElements,
					path_mode: mode,
					objective: Some(if weighted {
						crate::syn::MatchPathObjective::AllWeightedShortest
					} else {
						crate::syn::MatchPathObjective::AllShortest
					}),
					custom_cost: self.custom_cost,
					filters: self.constraints.filters.clone(),
				};
				let mut cursor = TensorShortestPathCursor::new(
					Box::new(seed_cursor),
					&*self.read,
					self.snapshot,
					&fields,
					root,
					expand,
					predicate,
					None,
					if self.atom_semantics {
						TraversalSemantics::Atom
					} else {
						TraversalSemantics::Node
					},
					scope,
					false,
					labels.clone(),
					traverser.bulk.get(),
					step.tensor_cache.clone(),
				);
				while let Some(mut row) = cursor.next(self.control)? {
					let bytes = traverser
						.estimated_heap_bytes()
						.checked_add(row.estimated_heap_bytes())
						.and_then(|value| value.checked_add(4 * size_of::<Traverser>()))
						.ok_or_else(overflow)?;
					if !memory.reserve(bytes) {
						return Err(resource(
							"all-shortest retained traversal",
							self.control.barrier_memory_limit() as u64,
						));
					}
					let mut value = traverser.clone();
					value.current = std::mem::replace(&mut row.values[2], Value::None);
					output.try_reserve_exact(1).map_err(|cause| {
						resource(
							"all-shortest traversal allocation",
							self.control.barrier_memory_limit() as u64,
						)
						.with_technical_source(cause)
					})?;
					output.push(value);
				}
			}
			arguments_account.release(seed_bytes);
		}
		Ok(output)
	}
}
