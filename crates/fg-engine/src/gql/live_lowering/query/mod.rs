//! Identity-preserving LIVE collection over the ordinary typed SELECT executor.

use std::{
	cell::{Cell, RefCell},
	collections::BTreeMap,
	path::PathBuf,
	sync::Arc,
};

use fg_runtime::__internal::{MaterializedGraphInternal as _, ValueInternal as _};

use super::{Lowering, span};
use crate::{
	datastore::{BindingAdmission, ClassId, ConflictKey, DatabaseRoot, FunctionId, SchemaVersion},
	gql::{execution::RootRead, ir::LogicalStatement},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveBindingId, LiveBindingOptions, LiveIdentity, LiveOptions, LiveQuery, LiveSource},
	syn::StatementKind,
	values::{Object, Text, Value},
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod scratch_tests;

#[cfg(test)]
mod window_tests;

#[cfg(test)]
mod graph_tests;

/// Exact owned rows and evidence from one immutable root evaluation.
pub struct QueryEvaluation {
	/// Successful capture owner from the same root as the exact result rows.
	pub(crate) capture_source: crate::changefeed::ChangefeedSource,
	/// Last successful string operand, retained with its resolved graph
	/// identity.
	pub(crate) capture_scope_name: Option<Text>,
	/// Successful String scopes of root JOIN declarations, separate from
	/// primary.
	pub(crate) rhs_scope_anchors: Vec<crate::datastore::LiveRhsScopeAnchor>,
	/// Source/group identities remain separate from projected payload values.
	pub(crate) rows: BTreeMap<LiveIdentity, Value>,
	/// Graph terminals use a separate owned result, never a runtime Value
	/// codec.
	pub(crate) graph: Option<crate::graph::MaterializedGraph>,
	/// Qualified logical content prepared for graph publication and replay.
	pub(crate) graph_image: Option<crate::live::LiveGraphImage>,
	/// Concrete transitive PURE versions; changed overloads require
	/// re-registration.
	pub(crate) functions: Vec<(FunctionId, SchemaVersion)>,
	/// Record-source namespaces retain their original class incarnation.
	pub(crate) record_sources: Vec<(Text, ClassId)>,
	/// Conservative query-wide dependencies include rejected WHERE rows.
	pub(crate) reads: Vec<ConflictKey>,
	/// Checked work consumed by lowering, compilation, reads and projection.
	pub(crate) work_used: u64,
}

/// Dependency admission occurs before each insertion; the infallible adapter
/// observer latches errors for the next work check and finalization.
struct Reads {
	/// Sorted logical predicates; only actual global scans set the coverage
	/// flag.
	keys: BTreeMap<ConflictKey, bool>,
	/// Checked node and final-vector admission within the query scratch limit.
	budget: BindingAdmission,
	/// First error prevents further growth and successful partial output.
	error: Option<FerrosGrynnError>,
}

impl Reads {
	/// Full global class ranges already observe every base-value mutation.
	fn covers_record(&self, root: &DatabaseRoot, key: ConflictKey) -> bool {
		let ConflictKey::BaseAtom(id) = key else {
			return false;
		};
		root.atom(id).is_some_and(|atom| {
			self.keys.get(&ConflictKey::BaseClassRange(atom.class_id())) == Some(&true)
		})
	}

	/// Promotes an already admitted range without allocating another key.
	fn mark_class_scan(&mut self, class: ClassId) {
		if let Some(scanned) = self.keys.get_mut(&ConflictKey::BaseClassRange(class)) {
			*scanned = true;
		}
	}

	/// Retains a key only after reserving both tree and returned-vector bytes.
	fn insert(&mut self, key: ConflictKey, remaining: &Cell<u64>, maximum: u64) {
		if self.error.is_some() || self.keys.contains_key(&key) {
			return;
		}
		let Some(work) = remaining.get().checked_sub(1) else {
			self.error = Some(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE evaluation work",
				maximum,
			));
			return;
		};
		remaining.set(work);
		// Include the marker array and alignment of an otherwise shared B-tree
		// node.
		match self.budget.admit(192 + 32, 0) {
			Ok(()) => {
				self.keys.insert(key, false);
			}
			Err(error) => self.error = Some(error),
		}
	}
}

/// Computes projected results without opening another datastore transaction.
#[allow(
	clippy::too_many_arguments,
	reason = "one immutable root binds query, parameters, budgets and dependency observer"
)]
#[allow(
	clippy::mutable_key_type,
	reason = "source identities are immutable and complete-domain aggregate keys are empty"
)]
pub fn evaluate_query<'root>(
	root: &'root Arc<DatabaseRoot>,
	query: &LiveQuery,
	bindings: &'root dyn Fn(LiveBindingId) -> FerrosGrynnResult<&'root Value>,
	options: &LiveOptions,
	scratch_root: PathBuf,
	observe: &dyn Fn(ConflictKey),
) -> FerrosGrynnResult<QueryEvaluation> {
	evaluate_query_controlled(
		root,
		query,
		bindings,
		options,
		scratch_root,
		observe,
		None,
		&[],
		None,
	)
}

/// Initial registration inherits interruption without capturing it for replay.
#[allow(
	clippy::too_many_arguments,
	reason = "one fixed root preserves evaluation policy and caller cancellation"
)]
#[allow(clippy::mutable_key_type, reason = "qualified identities are immutable during collection")]
pub fn evaluate_query_controlled<'root>(
	root: &'root Arc<DatabaseRoot>,
	query: &LiveQuery,
	bindings: &'root dyn Fn(LiveBindingId) -> FerrosGrynnResult<&'root Value>,
	options: &LiveOptions,
	scratch_root: PathBuf,
	observe: &dyn Fn(ConflictKey),
	scope_anchor: Option<(crate::graph::GraphId, &str)>,
	rhs_scope_anchors: &[crate::datastore::LiveRhsScopeAnchor],
	parent: Option<&crate::gql::ExecutionControl>,
) -> FerrosGrynnResult<QueryEvaluation> {
	if let Some(parent) = parent {
		parent.check()?;
	}
	let dependency_bytes = options.scratch_bytes() / 8;
	if dependency_bytes == 0 {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE dependency scratch",
			options.scratch_bytes() as u64,
		));
	}
	let read_options =
		LiveBindingOptions::default().with_budgets(dependency_bytes, options.work())?;
	let reads = RefCell::new(Reads {
		keys: BTreeMap::new(),
		budget: BindingAdmission::new(&read_options),
		error: None,
	});
	let mut lowering = Lowering {
		parent: parent.map(|control| control.child(None)),
		root,
		current_schema: None,
		parameters: Object::new(),
		next_literal: 0,
		captures: Vec::new(),
		account: crate::gql::options::BarrierAccount::new(
			options.scratch_bytes() - dependency_bytes,
		),
		maximum_scratch: options.scratch_bytes() - dependency_bytes,
		admitted_scratch: 0,
		expression_depth: 0,
		remaining_work: options.work(),
		bindings,
		record_sources: BTreeMap::new(),
		class_namespace: false,
		current_alias: None,
		current_logical_alias: None,
		multi_source: false,
		join_schemas: Vec::new(),
		next_scope: 0,
		independent_queries: BTreeMap::new(),
	};
	let statements = lowering
		.selects(query)?
		.into_iter()
		.map(|select| {
			let split_field_0 = StatementKind::Select(select);
			let split_field_1 = span();
			let split_field_2 = None;
			crate::syn::split_internal::new_statement(split_field_0, split_field_1, split_field_2)
		})
		.collect::<Vec<_>>();
	let (cache, control) = super::evaluation_runtime(&lowering, options.work())?;
	let remaining = Cell::new(lowering.remaining_work);
	let work = |amount: u64| {
		if let Some(error) = &reads.borrow().error {
			return Err(error.clone());
		}
		remaining.set(remaining.get().checked_sub(amount).ok_or_else(|| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE evaluation work",
				options.work(),
			)
		})?);
		Ok(())
	};
	let record = |key| {
		if reads.borrow().covers_record(root, key) {
			return;
		}
		reads.borrow_mut().insert(key, &remaining, options.work());
		if reads.borrow().error.is_none() {
			observe(key);
		}
	};
	let class_scan = |class| reads.borrow_mut().mark_class_scan(class);
	let read = RootRead::new(root, &control, &record, &work, scratch_root)
		.with_class_scan_observer(&class_scan);
	if lowering.class_namespace {
		record(ConflictKey::ClassCatalogRange);
	}
	let mut program = crate::gql::analysis::analyze_with_runtime(
		&{
			let split_field_0 = statements;
			crate::syn::split_internal::new_program(split_field_0)
		},
		"",
		&read,
		&lowering.parameters,
		&cache,
	)?;
	let compiled = cache
		.remaining_compile_work()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE compiler work is not bounded"))?;
	work(lowering.remaining_work - compiled)?;
	let fields = read.fields();
	fields.admit(
		program
			.slot_count
			.checked_mul(std::mem::size_of::<Value>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE query slots"))?,
	)?;
	for value in lowering.parameters.values() {
		fields.admit_value(value)?;
	}
	let slots = crate::gql::execution::initialize_slots(&program, &lowering.parameters);
	let mut resolved_capture = None;
	let mut capture_scope_name = None;
	for statement in &program.statements {
		crate::gql::optimizer::validate_deterministic_statement(statement)?;
	}
	fields.admit(
		query
			.joins()
			.len()
			.checked_mul(std::mem::size_of::<crate::datastore::LiveRhsScopeAnchor>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE RHS scope anchors"))?,
	)?;
	let mut resolved_rhs = Vec::new();
	resolved_rhs.try_reserve_exact(query.joins().len()).map_err(|error| {
		FerrosGrynnError::invalid_argument("LIVE RHS anchor allocation failed")
			.with_technical_source(error)
	})?;
	for (index, source) in query.sources().enumerate() {
		fields.work(1)?;
		if source.graph_expression().is_none() {
			continue;
		}
		let ordinal = u32::try_from(index)
			.map_err(|_error| FerrosGrynnError::operation_overflow("LIVE source ordinal"))?;
		let Some(LogicalStatement::Select(first)) = program.statements.first() else {
			return Err(FerrosGrynnError::invalid_storage_file("LIVE typed SELECT is missing"));
		};
		let input = if index == 0 {
			&first.source
		} else {
			&first
				.joins
				.get(index - 1)
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("LIVE typed JOIN source is missing")
				})?
				.source
		};
		let reference = input.kind.graph_scope().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("LIVE scope has no typed graph source")
		})?;
		let anchor = if index == 0 {
			scope_anchor
		} else {
			rhs_scope_anchors
				.binary_search_by_key(&ordinal, |anchor| anchor.source)
				.ok()
				.and_then(|index| rhs_scope_anchors.get(index))
				.map(|anchor| (anchor.graph, anchor.name.as_str()))
		};
		let (graph, name) = crate::gql::execution::resolve_root_graph_reference(
			&read, reference, &slots, &control, anchor,
		)?;
		if index == 0 {
			resolved_capture = Some(crate::changefeed::ChangefeedSource::Graph(graph.id()));
			capture_scope_name = name;
		} else if let Some(name) = name {
			resolved_rhs.push(crate::datastore::LiveRhsScopeAnchor {
				source: ordinal,
				graph: graph.id(),
				name,
			});
		}
		for statement in &mut program.statements {
			fields.work(1)?;
			let LogicalStatement::Select(plan) = statement else {
				return Err(FerrosGrynnError::invalid_storage_file("LIVE typed SELECT is missing"));
			};
			let input = if index == 0 {
				&mut plan.source
			} else {
				&mut plan
					.joins
					.get_mut(index - 1)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("LIVE typed JOIN source is missing")
					})?
					.source
			};
			let reference = input.kind.graph_scope_mut().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("LIVE scope has no typed graph source")
			})?;
			fields.admit(
				graph
					.name()
					.len()
					.checked_add(std::mem::size_of_val(&graph))
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE scope copy"))?,
			)?;
			*reference = crate::gql::ir::LogicalGraphReference::Static(graph.clone());
		}
	}

	for statement in &mut program.statements {
		if let LogicalStatement::Select(plan) = statement {
			crate::gql::execution::normalize_root_select(&read, plan, &slots)?;
		}
	}
	fields.admit(
		program
			.statements
			.len()
			.checked_mul(std::mem::size_of::<&crate::gql::ir::LogicalSelect>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE query plans"))?,
	)?;
	let plans = program
		.statements
		.iter()
		.filter_map(|statement| match statement {
			LogicalStatement::Select(plan) => Some(plan),
			_ => None,
		})
		.collect::<Vec<_>>();
	let mut rows = BTreeMap::new();
	let graph_plan = plans.first().copied().filter(|plan| plan.return_graph.is_some());
	let mut graph_assembly = graph_plan
		.map(|plan| {
			crate::gql::execution::GraphAssemblyAccumulator::for_select(
				&read, plan, &slots, &control,
			)
		})
		.transpose()?;
	let mut collect = |identity: LiveIdentity, value: Value| {
		fields.work(1)?;
		if let (Some(assembly), Some(plan)) = (&mut graph_assembly, graph_plan) {
			return assembly.add_root_row(&read, plan, value, &slots);
		}
		fields.admit_retained(
			value
				.estimated_heap_bytes()
				.checked_add(identity.estimated_heap_bytes())
				.and_then(|bytes| bytes.checked_add(256 + std::mem::size_of::<Value>()))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE result row"))?,
		)?;
		if rows.insert(identity, value).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"LIVE SELECT produced duplicate result identities",
			));
		}
		Ok(())
	};
	if plans.len() > 1 && plans[0].group.is_some() {
		crate::gql::execution::visit_root_aggregate_union(
			&read,
			&plans,
			&slots,
			&control,
			&mut collect,
		)?;
	} else if plans.len() > 1
		&& (plans[0].only
			|| !plans[0].order.is_empty()
			|| plans[0].start.is_some()
			|| plans[0].limit.is_some())
	{
		crate::gql::execution::visit_root_row_union(&read, &plans, &slots, &control, &mut collect)?;
	} else {
		for plan in plans {
			crate::gql::execution::visit_root_select(&read, plan, &slots, &control, &mut collect)?;
		}
	}
	let graph = graph_assembly
		.map(|assembly| {
			let plan = graph_plan.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("LIVE graph plan is missing")
			})?;
			let terminal = plan.return_graph.as_ref().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("LIVE graph terminal is missing")
			})?;
			assembly.finish_root(&read, terminal.contract)
		})
		.transpose()?;
	let graph_image = graph
		.as_ref()
		.map(|graph| {
			fields.admit_retained(graph.estimated_heap_bytes())?;
			crate::live::LiveGraphImage::capture(
				graph,
				&root.dictionary,
				&|bytes, work| {
					fields.admit_retained(bytes)?;
					fields.work(work)
				},
				&|atom, graph| read.graph_source_mapping(atom, graph),
			)
		})
		.transpose()?;
	let count = cache.named_dependency_count();
	fields.work(count as u64)?;
	fields.admit(
		count
			.checked_mul(192)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE query function captures"))?,
	)?;
	let functions = cache.named_dependencies().into_iter().collect();
	fields.admit(
		lowering
			.record_sources
			.len()
			.checked_mul(std::mem::size_of::<(Text, ClassId)>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE query source owners"))?,
	)?;
	let capture_source = match resolved_capture {
		Some(source) => source,
		None => {
			let owner = if let LiveSource::Record(record) = query.source() {
				lowering.record_sources.get(record.class()).copied()
			} else {
				None
			};
			crate::datastore::capture_source(query, owner)?
		}
	};
	let record_sources = lowering.record_sources.into_iter().collect();
	let reads = reads.into_inner();
	if let Some(error) = reads.error {
		return Err(error);
	}
	Ok(QueryEvaluation {
		capture_source,
		capture_scope_name,
		rhs_scope_anchors: resolved_rhs,
		rows,
		graph,
		graph_image,
		functions,
		record_sources,
		reads: reads.keys.into_keys().collect(),
		work_used: options.work() - remaining.get(),
	})
}
