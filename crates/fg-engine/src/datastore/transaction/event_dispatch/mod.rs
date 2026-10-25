//! Immediate operation-image dispatch, independent of commit replay and feed
//! policy.

#![allow(
	clippy::mutable_key_type,
	reason = "owned RecordId values remain immutable while used as logical target keys"
)]

use std::{collections::BTreeMap, sync::Arc};

use fg_runtime::__internal::ValueInternal as _;

use super::{
	Mutation,
	Transaction,
	operation_image::{CaptureBudget, image, same_image},
};
use crate::{
	changefeed::{ChangeGraphState, ChangeIdentity, ChangeImage},
	datastore::{DatabaseRoot, EventDefinition, EventTarget, GlobalAtomId},
	gql::{BarrierAccount, ExecutionControl, QueryExecutionOptions},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Array, Object, RecordId, Value},
};

/// Identity-ordered logical targets; local addresses never become event
/// identities.
type Targets = BTreeMap<(EventTarget, RecordId), GlobalAtomId>;

/// Bounded raw field inputs retained only across one mutation and its handlers.
pub(super) struct EventInputs {
	/// Graph-local inputs override inherited base inputs for the same record.
	values: BTreeMap<RecordId, BTreeMap<Option<crate::graph::GraphId>, Object>>,
	/// Shared caller admission remains held while nested handlers run.
	account: BarrierAccount,
	/// Transaction scratch admission includes keys and overwritten copies.
	bytes: usize,
}

impl EventInputs {
	/// Keeps the disabled path free of an input map or memory account.
	pub(super) fn new(control: Option<&ExecutionControl>) -> Self {
		Self {
			values: BTreeMap::new(),
			account: control.map_or_else(
				|| BarrierAccount::new(256 * 1024 * 1024),
				ExecutionControl::barrier_account,
			),
			bytes: 0,
		}
	}

	/// Admits both query and transaction scratch before any copy.
	fn admit(&mut self, bytes: usize, maximum: usize) -> FerrosGrynnResult<()> {
		let total = self
			.bytes
			.checked_add(bytes)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT input bytes"))?;
		if total > maximum || !self.account.reserve(bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"EVENT input scratch",
				total as u64,
			));
		}
		self.bytes = total;
		Ok(())
	}
}

#[cfg(test)]
mod tests;

impl Transaction {
	/// Declares caller input before defaults or minimal property diffs erase
	/// it.
	pub(crate) fn declare_event_input(
		&mut self,
		record: &RecordId,
		graph: Option<crate::graph::GraphId>,
	) -> FerrosGrynnResult<()> {
		let Some(inputs) = self.event_inputs.as_mut() else {
			return Ok(());
		};
		let maximum = self
			.store
			.inner
			.transaction_limits()
			.max_write_set_bytes
			.saturating_sub(self.write_bytes);
		inputs.admit(
			256usize.saturating_add(crate::values::runtime::estimated_record_id_heap_bytes(record)),
			maximum,
		)?;
		inputs.values.entry(record.clone()).or_default().entry(graph).or_default();
		Ok(())
	}

	/// Captures each evaluated explicit field, including equality assignments.
	pub(crate) fn capture_event_input_field(
		&mut self,
		record: &RecordId,
		graph: Option<crate::graph::GraphId>,
		name: &str,
		value: &Value,
	) -> FerrosGrynnResult<()> {
		let Some(inputs) = self.event_inputs.as_mut() else {
			return Ok(());
		};
		let maximum = self
			.store
			.inner
			.transaction_limits()
			.max_write_set_bytes
			.saturating_sub(self.write_bytes);
		inputs.admit(
			256usize
				.saturating_add(name.len())
				.saturating_add(value.estimated_heap_bytes().saturating_mul(2))
				.saturating_add(crate::values::runtime::estimated_record_id_heap_bytes(record)),
			maximum,
		)?;
		inputs
			.values
			.entry(record.clone())
			.or_default()
			.entry(graph)
			.or_default()
			.insert(name, value.clone());
		Ok(())
	}

	/// Import event execution remains opt-in; ordinary operations use schema
	/// policy.
	pub(super) fn has_operation_events(&self) -> bool {
		(self.import_constraints.is_none() || self.import_events.is_some())
			&& !self.working.schema.events.is_empty()
	}

	/// Installs a query's actual child control only while its mutation
	/// executes.
	pub(crate) fn with_event_control<T>(
		&mut self,
		control: &ExecutionControl,
		apply: impl FnOnce(&mut Self) -> FerrosGrynnResult<T>,
	) -> FerrosGrynnResult<T> {
		if !self.has_operation_events() {
			return apply(self);
		}
		let previous = self.event_control.replace(Box::new(control.clone()));
		let result = apply(self);
		self.event_control = previous;
		result
	}

	/// Evaluates frozen contexts after one completed mutation, before
	/// publication.
	pub(super) fn dispatch_operation_events(
		&mut self,
		before: &DatabaseRoot,
		mutation_start: usize,
	) -> FerrosGrynnResult<()> {
		if self.import_constraints.is_some() && self.import_events.is_none()
			|| before.schema.events.is_empty()
		{
			return Ok(());
		}
		let control = self
			.event_control
			.as_deref()
			.cloned()
			.unwrap_or_else(|| ExecutionControl::new(&QueryExecutionOptions::default(), None));
		control.check()?;
		self.record_read(crate::datastore::ConflictKey::EventCatalogRange);
		let maximum = control
			.barrier_memory_available()
			.min(self.store.inner.transaction_limits().max_write_set_bytes);
		let mut budget = CaptureBudget {
			bytes: 0,
			maximum,
		};
		let targets = self.event_targets(before, mutation_start, &mut budget, &control)?;
		let mut calls = Vec::new();
		for ((target, record), _global) in targets {
			control.check()?;
			control.admit_reactive_work(1)?;
			let old = event_image(before, target, &record, &mut budget)?;
			let new = event_image(&self.working, target, &record, &mut budget)?;
			if same_image(old.as_deref(), new.as_deref()) {
				continue;
			}
			for (root, image) in [(before, old.as_deref()), (&self.working, new.as_deref())] {
				if let Some(image) = image {
					self.record_event_image_reads(root, image, &control)?;
				}
			}
			let related = self.mutations[mutation_start..].iter().any(|mutation| match mutation {
				Mutation::AttachEdge {
					graph,
					global,
					..
				} => {
					self.working.atom(*global).is_some_and(|atom| atom.record_id() == &record)
						&& match target {
							EventTarget::Class(_) => true,
							EventTarget::Graph(owner) => owner == *graph,
						}
				}
				_ => false,
			});
			let kind = match (old.is_some(), new.is_some(), related) {
				(false, true, true) => "RELATE",
				(false, true, false) => "CREATE",
				(true, false, _) => "DELETE",
				(true, true, _) => "UPDATE",
				(false, false, _) => continue,
			};
			let input = self.event_inputs.as_ref().and_then(|inputs| {
				let graph = match target {
					EventTarget::Class(_) => None,
					EventTarget::Graph(graph) => Some(graph),
				};
				let fields = inputs.values.get(&record)?;
				fields.get(&graph).or_else(|| fields.get(&None)).or_else(|| {
					matches!(target, EventTarget::Class(_))
						.then(|| fields.values().next())
						.flatten()
				})
			});
			let input = match input {
				Some(input) => {
					budget.admit(object_copy_bytes(input))?;
					Value::Object(input.clone())
				}
				None => operation_input(
					&self.working,
					&self.mutations[mutation_start..],
					&record,
					&mut budget,
				)?,
			};
			budget.admit(std::mem::size_of::<[Value; 5]>() + 64)?;
			let arguments = Arc::new(event_arguments(
				target,
				kind,
				old.as_deref(),
				new.as_deref(),
				input,
				&mut budget,
			)?);
			for event in before.schema.events().filter(|event| event.target() == target) {
				control.admit_reactive_work(1)?;
				budget.reserve(&mut calls)?;
				budget.admit(std::mem::size_of::<(Arc<EventDefinition>, Arc<[Value; 5]>)>())?;
				if let Some(id) = event.id() {
					self.record_read(crate::datastore::ConflictKey::SchemaEvent(id));
				}
				calls.push((Arc::clone(event), Arc::clone(&arguments)));
			}
		}
		if calls.is_empty() {
			return Ok(());
		}
		if self.event_depth >= control.max_control_depth {
			return Err(FerrosGrynnError::control_limit_exceeded(
				"EVENT recursion",
				u64::from(control.max_control_depth),
			));
		}
		let mut account = control.barrier_account();
		if !account.reserve(budget.bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"EVENT contexts",
				budget.bytes as u64,
			));
		}
		let previous_control = self.event_control.replace(Box::new(control.clone()));
		let depth = self.event_depth;
		self.event_depth += 1;
		let result = calls.into_iter().try_for_each(|(event, arguments)| {
			crate::gql::execute_event(self, &event, &arguments, &control, &before.schema)
				.map(|_matched| ())
		});
		self.event_depth = depth;
		self.event_control = previous_control;
		result
	}

	/// Frozen context fields are reads even when source projection bypasses
	/// query cursors.
	fn record_event_image_reads(
		&self,
		root: &DatabaseRoot,
		image: &ChangeImage,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		match image.identity() {
			ChangeIdentity::Record(atom) => {
				if let Some(global) = root.resolve_atom(atom.id()) {
					self.record_read(crate::datastore::ConflictKey::BaseAtom(global));
				}
			}
			ChangeIdentity::Occurrence(occurrence) => {
				self.record_event_occurrence_reads(root, occurrence);
			}
		}
		match image.graph_state() {
			Some(ChangeGraphState::Node {
				nested_graph: Some(graph),
			}) => {
				self.record_read(crate::datastore::ConflictKey::GraphDefinition(graph.id()));
			}
			Some(ChangeGraphState::Edge {
				source,
				target,
				transition_graph,
				..
			}) => {
				if let Some(graph) = transition_graph {
					self.record_read(crate::datastore::ConflictKey::GraphDefinition(graph.id()));
				}
				for endpoint in source.iter().chain(target) {
					control.admit_reactive_work(1)?;
					self.record_event_occurrence_reads(root, endpoint.occurrence());
					for owner in endpoint.owners() {
						control.admit_reactive_work(1)?;
						self.record_event_occurrence_reads(root, owner);
					}
				}
			}
			Some(ChangeGraphState::Node {
				nested_graph: None,
			})
			| None => {}
		}
		Ok(())
	}

	/// Tracks every qualified owner using the same captured logical root.
	fn record_event_occurrence_reads(
		&self,
		root: &DatabaseRoot,
		occurrence: &crate::graph::AtomOccurrenceRef,
	) {
		let graph = occurrence.graph().id();
		self.record_read(crate::datastore::ConflictKey::GraphDefinition(graph));
		if let Some(global) = root.resolve_atom(occurrence.atom().id()) {
			self.record_read(crate::datastore::ConflictKey::BaseAtom(global));
			self.record_read(crate::datastore::ConflictKey::GraphMembership(graph, global));
			self.record_read(crate::datastore::ConflictKey::GraphOverlay(graph, global));
			if let Some(local) = root.membership(graph, global) {
				self.record_read(crate::datastore::ConflictKey::GraphIncidence(graph, local));
			}
		}
	}

	/// Adds explicit targets plus inherited occurrence changes from reverse
	/// memberships.
	fn event_targets(
		&self,
		before: &DatabaseRoot,
		mutation_start: usize,
		budget: &mut CaptureBudget,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Targets> {
		let mut targets = Targets::new();
		for mutation in &self.mutations[mutation_start..] {
			control.admit_reactive_work(1)?;
			if let Some((global, graph)) = mutation_target(mutation, before, &self.working) {
				let Some(atom) = self.working.atom(global).or_else(|| before.atom(global)) else {
					continue;
				};
				let target = graph.map_or(EventTarget::Class(atom.class_id()), EventTarget::Graph);
				add_target(&mut targets, before, target, atom.record_id(), global, budget)?;
				if graph.is_none() {
					for root in [before, &self.working] {
						for graph in root
							.memberships(global)
							.into_iter()
							.flat_map(|memberships| memberships.keys())
						{
							control.admit_reactive_work(1)?;
							add_target(
								&mut targets,
								before,
								EventTarget::Graph(*graph),
								atom.record_id(),
								global,
								budget,
							)?;
						}
					}
				}
			}
			match mutation {
				Mutation::RemoveGraph(graph) => {
					for schema in before.schema.all_graphs().filter(|schema| {
						schema.id() == *graph || schema.owner_root() == Some(*graph)
					}) {
						self.add_graph_event_targets(
							&mut targets,
							before,
							schema.id(),
							budget,
							control,
						)?;
					}
				}
				Mutation::InstallBuiltGraphTree(tree) => {
					for graph in tree.graph_ids() {
						self.add_graph_event_targets(&mut targets, before, graph, budget, control)?;
					}
				}
				_ => {}
			}
		}
		Ok(targets)
	}

	/// Includes complete graph replacement and retirement without an unbounded
	/// ID buffer.
	fn add_graph_event_targets(
		&self,
		targets: &mut Targets,
		before: &DatabaseRoot,
		graph: crate::graph::GraphId,
		budget: &mut CaptureBudget,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		for root in [before, &self.working] {
			for occurrence in
				root.graph(graph).into_iter().flat_map(crate::datastore::GraphRoot::occurrences)
			{
				control.admit_reactive_work(1)?;
				let atom = root.atom(occurrence.global_id()).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("EVENT occurrence has no base atom")
				})?;
				add_target(
					targets,
					before,
					EventTarget::Graph(graph),
					atom.record_id(),
					occurrence.global_id(),
					budget,
				)?;
			}
		}
		Ok(())
	}
}

/// Uses each operation's mutations because the outer feed intentionally
/// coalesces targets.
fn mutation_target(
	mutation: &Mutation,
	before: &DatabaseRoot,
	after: &DatabaseRoot,
) -> Option<(GlobalAtomId, Option<crate::graph::GraphId>)> {
	match mutation {
		Mutation::CreateAtom {
			id,
			..
		} => Some((*id, None)),
		Mutation::SetBaseProperty {
			global,
			..
		}
		| Mutation::RemoveBaseProperty {
			global,
			..
		}
		| Mutation::DeleteAtom {
			global,
		} => Some((*global, None)),
		Mutation::AttachNode {
			global,
			graph,
			..
		}
		| Mutation::AttachEdge {
			global,
			graph,
			..
		}
		| Mutation::Detach {
			global,
			graph,
		} => Some((*global, Some(*graph))),
		Mutation::SetOverlay {
			graph,
			local,
			..
		}
		| Mutation::HideProperty {
			graph,
			local,
			..
		}
		| Mutation::ResetProperty {
			graph,
			local,
			..
		}
		| Mutation::SetOccurrenceLabel {
			graph,
			local,
			..
		}
		| Mutation::ReplaceOccurrenceState {
			graph,
			local,
			..
		} => after
			.occurrence(*graph, *local)
			.or_else(|| before.occurrence(*graph, *local))
			.map(|occurrence| (occurrence.global_id(), Some(*graph))),
		Mutation::DefineLiveBinding(_)
		| Mutation::ReplaceLiveBinding {
			..
		}
		| Mutation::RemoveLiveBinding(_)
		| Mutation::RegisterLive(_)
		| Mutation::CloseLive(_)
		| Mutation::AcknowledgeLive(_)
		| Mutation::ConfigureChangefeed {
			..
		}
		| Mutation::InternProperty {
			..
		}
		| Mutation::DefineClass(_)
		| Mutation::DefineParameter(_)
		| Mutation::DefineFunction(_)
		| Mutation::DefineEvent(_)
		| Mutation::DefineAnalyzer(_)
		| Mutation::DefineGraph(_)
		| Mutation::ReplaceClass(_)
		| Mutation::ReplaceParameter(_)
		| Mutation::ReplaceFunction(_)
		| Mutation::ReplaceEvent(_)
		| Mutation::ReplaceAnalyzer(_)
		| Mutation::ReplaceGraph(_)
		| Mutation::InstallBuiltGraphTree(_)
		| Mutation::RemoveClass(_)
		| Mutation::RemoveParameter(_)
		| Mutation::RemoveFunction(_)
		| Mutation::RemoveEvent(_)
		| Mutation::EnqueueEvent(_)
		| Mutation::TransitionEvent {
			..
		}
		| Mutation::RemoveAnalyzer(_)
		| Mutation::RemoveGraph(_)
		| Mutation::RegisterBridgeDatabase(_)
		| Mutation::RegisterBridgeMapping(_)
		| Mutation::TransitionBridgeMapping {
			..
		} => None,
	}
}

/// Admits a canonical target key before cloning its owned RecordId.
fn add_target(
	targets: &mut Targets,
	before: &DatabaseRoot,
	target: EventTarget,
	record: &RecordId,
	global: GlobalAtomId,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<()> {
	if !before.schema.events().any(|event| event.target() == target) {
		return Ok(());
	}
	budget.admit(
		128usize.saturating_add(crate::values::runtime::estimated_record_id_heap_bytes(record)),
	)?;
	targets.entry((target, record.clone())).or_insert(global);
	Ok(())
}

/// Projects the exact source incarnation without consulting another database
/// root.
fn event_image(
	root: &DatabaseRoot,
	target: EventTarget,
	record: &RecordId,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<Option<Arc<ChangeImage>>> {
	let Some(global) = root.resolve_atom(record) else {
		return Ok(None);
	};
	let graph = match target {
		EventTarget::Class(class) => {
			if !root.atom(global).is_some_and(|atom| atom.class_id() == class) {
				return Ok(None);
			}
			None
		}
		EventTarget::Graph(graph) => Some(graph),
	};
	image(root, global, graph, budget)
}

/// Forms ordinary record fields while retaining qualified occurrence identity.
fn record_value(image: &ChangeImage, budget: &mut CaptureBudget) -> FerrosGrynnResult<Value> {
	let record = match image.identity() {
		ChangeIdentity::Record(atom) => atom.id(),
		ChangeIdentity::Occurrence(occurrence) => occurrence.atom().id(),
	};
	budget.admit(
		object_copy_bytes(image.properties())
			.saturating_add(2048)
			.saturating_add(image.label().map_or(0, crate::values::Text::len))
			.saturating_add(crate::values::runtime::estimated_record_id_heap_bytes(record)),
	)?;
	let mut fields = image.properties().clone();
	let identity = match image.identity() {
		ChangeIdentity::Record(atom) => Value::from(atom.id().clone()),
		ChangeIdentity::Occurrence(occurrence) => Value::from(occurrence.clone()),
	};
	fields.insert("id", identity);
	if let ChangeIdentity::Occurrence(occurrence) = image.identity() {
		fields.insert("graph", Value::from(occurrence.graph().clone()));
		fields.insert("label", image.label().cloned().map_or(Value::None, Value::from));
	}
	if let Some(state) = image.graph_state() {
		match state {
			ChangeGraphState::Node {
				nested_graph,
			} => {
				budget.admit(
					nested_graph
						.as_ref()
						.map_or(0, |graph| 128usize.saturating_add(graph.name().len())),
				)?;
				fields
					.insert("nested_graph", nested_graph.clone().map_or(Value::None, Value::from));
			}
			ChangeGraphState::Edge {
				direction,
				source,
				target,
				weight,
				transition_graph,
			} => {
				budget.admit(
					transition_graph
						.as_ref()
						.map_or(0, |graph| 128usize.saturating_add(graph.name().len())),
				)?;
				fields.insert(
					"direction",
					Value::from(match direction {
						crate::graph::EdgeDirection::Directed => "directed",
						crate::graph::EdgeDirection::Bidirectional => "bidirectional",
						crate::graph::EdgeDirection::Undirected => "undirected",

						_ => {
							return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
								"Unsupported runtime variant at the workspace boundary.",
							));
						}
					}),
				);
				fields.insert("endpoints", endpoint_value(source, target, budget)?);
				fields.insert("weight", weight.map_or(Value::None, Value::Number));
				fields.insert(
					"transition_graph",
					transition_graph.clone().map_or(Value::None, Value::from),
				);
			}
		}
	}
	Ok(Value::Object(fields))
}

/// Includes B-tree slot overhead omitted by the public value estimate.
fn object_copy_bytes(object: &Object) -> usize {
	object.iter().fold(0, |bytes, (name, value)| {
		bytes.saturating_add(
			128usize
				.saturating_add(name.len())
				.saturating_add(value.estimated_heap_bytes().saturating_mul(2)),
		)
	})
}

/// Keeps source/target arrays aligned with the exact ordered containment
/// routes.
fn endpoint_value(
	source: &[crate::changefeed::ChangeEndpoint],
	target: &[crate::changefeed::ChangeEndpoint],
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<Value> {
	for endpoint in source.iter().chain(target) {
		budget.admit(512usize.saturating_add(
			crate::values::runtime::estimated_record_id_heap_bytes(
				endpoint.occurrence().atom().id(),
			),
		))?;
		for owner in endpoint.owners() {
			budget.admit(256usize.saturating_add(
				crate::values::runtime::estimated_record_id_heap_bytes(owner.atom().id()),
			))?;
		}
	}
	Ok(Value::from(Object::from_iter([
		(
			"source",
			Value::from(Array::new(
				source.iter().map(|endpoint| Value::from(endpoint.occurrence().clone())).collect(),
			)),
		),
		(
			"target",
			Value::from(Array::new(
				target.iter().map(|endpoint| Value::from(endpoint.occurrence().clone())).collect(),
			)),
		),
		(
			"source_paths",
			Value::from(Array::new(
				source
					.iter()
					.map(|endpoint| {
						Value::from(Array::new(
							endpoint.owners().iter().cloned().map(Value::from).collect(),
						))
					})
					.collect(),
			)),
		),
		(
			"target_paths",
			Value::from(Array::new(
				target
					.iter()
					.map(|endpoint| {
						Value::from(Array::new(
							endpoint.owners().iter().cloned().map(Value::from).collect(),
						))
					})
					.collect(),
			)),
		),
	])))
}

/// Builds the five reserved parameters; graph RELATE exposes logical
/// references.
fn event_arguments(
	target: EventTarget,
	kind: &str,
	before: Option<&ChangeImage>,
	after: Option<&ChangeImage>,
	input: Value,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<[Value; 5]> {
	let mut old =
		before.map(|image| record_value(image, budget)).transpose()?.unwrap_or(Value::None);
	let mut new =
		after.map(|image| record_value(image, budget)).transpose()?.unwrap_or(Value::None);
	let value = if matches!(target, EventTarget::Graph(_)) && kind == "RELATE" {
		old = Value::Null;
		new = Value::Null;
		let mut value = Object::new();
		if let Some(image) = after {
			if let ChangeIdentity::Occurrence(edge) = image.identity() {
				value.insert("edge", Value::from(edge.clone()));
			}
			if let Some(ChangeGraphState::Edge {
				source,
				target,
				..
			}) = image.graph_state()
			{
				value.insert("endpoints", endpoint_value(source, target, budget)?);
			}
		}
		Value::Object(value)
	} else {
		let value = if kind == "DELETE" {
			&old
		} else {
			&new
		};
		budget.admit(value.estimated_heap_bytes().saturating_mul(2).saturating_add(256))?;
		value.clone()
	};
	Ok([Value::from(kind), old, new, value, input])
}

/// Keeps explicit caller-supplied writes separate from unrelated before/after
/// fields.
fn operation_input(
	root: &DatabaseRoot,
	mutations: &[Mutation],
	record: &RecordId,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<Value> {
	let mut input = Object::new();
	for mutation in mutations {
		let fields = match mutation {
			Mutation::CreateAtom {
				record_id,
				properties,
				..
			} if record_id == record => Some(properties),
			_ => None,
		};
		if let Some(fields) = fields {
			for (key, value) in fields {
				insert_input(&mut input, root, key, value, budget)?;
			}
		}
		if let Mutation::SetBaseProperty {
			global,
			key,
			value,
		} = mutation
			&& root.atom(*global).is_some_and(|atom| atom.record_id() == record)
		{
			insert_input(&mut input, root, *key, value, budget)?;
		}
		if let Mutation::SetOverlay {
			graph,
			local,
			key,
			value,
		} = mutation
			&& root
				.occurrence(*graph, *local)
				.and_then(|occurrence| root.atom(occurrence.global_id()))
				.is_some_and(|atom| atom.record_id() == record)
		{
			insert_input(&mut input, root, *key, value, budget)?;
		}
	}
	Ok(Value::Object(input))
}

/// Admits dictionary-name and property copies before constructing input fields.
fn insert_input(
	input: &mut Object,
	root: &DatabaseRoot,
	key: crate::values::PropertyKeyId,
	value: &Value,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<()> {
	let name = root.dictionary.resolve(key).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("EVENT input field has no dictionary name")
	})?;
	budget
		.admit(128usize.saturating_add(name.len()).saturating_add(value.estimated_heap_bytes()))?;
	input.insert(name.clone(), value.clone());
	Ok(())
}
