//! Checked syntax-to-owned recipes; computation stays in ordinary SELECT.

use std::collections::BTreeMap;

use super::{Analyzer, ScanTarget, SelectItem, SelectSourceKind, builtin_name};
use crate::{
	datastore::{BindingAdmission, ClassId},
	gql::ir::{
		LogicalLiveBinding,
		LogicalLiveCapture,
		LogicalLiveSelect,
		LogicalLiveSourceCapture,
		LogicalStatement,
		TypedExpr,
		TypedExprKind,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult, TextSpan},
	live::{
		LiveBinaryOperator,
		LiveBindingOptions,
		LiveExpr,
		LiveField,
		LiveJoin,
		LiveJoinKind,
		LiveNullsOrder,
		LiveOrder,
		LiveOrderDirection,
		LiveOutputMode,
		LiveOverflowPolicy,
		LivePath,
		LiveProjection,
		LiveQuery,
		LiveSource,
	},
	syn::{
		BinaryOperator,
		Expr,
		ExprKind,
		LiveOutputName,
		LiveOverflowName,
		LiveSelectStatement,
		SelectGroup,
		SelectNulls,
		SelectOrderDirection,
		SelectPath,
		SelectStart,
		SelectStatement,
		UnaryOperator,
	},
	values::Text,
};

/// One bounded conversion owns retained recipe admission and lexical slots.
struct Recipe<'analyzer, 'catalog> {
	/// Shared source/literal/type authority.
	analyzer: &'analyzer Analyzer<'catalog>,
	/// Unique names preserve the nearest lexical frame chosen by analysis.
	captures: BTreeMap<Box<str>, (u32, bool)>,
	/// Slots actually used by registration-only options, excluding
	/// closure-local names.
	operand_slots: BTreeMap<Box<str>, (u32, bool)>,
	/// Existing checked byte/work admission; no additional runtime or budget.
	budget: BindingAdmission,
	/// Root registration scopes retain their source declaration ownership.
	frozen_graphs: Vec<LogicalLiveSourceCapture>,
}

/// Registration-only freezing remains separate from durable scope recipes.
enum GraphScope {
	/// A named graph binds its stable identity during analysis.
	Static(
		/// A named graph binds its stable identity during analysis.
		crate::values::GraphRef,
	),
	/// A root ordinary parameter is reified at registration execution.
	Frozen,
	/// Read-only scope evaluation follows the normal query pipeline.
	Expression(
		/// Read-only scope evaluation follows the normal query pipeline.
		LiveExpr,
	),
}

impl Analyzer<'_> {
	/// Identity commands address the lexical binding rather than reading its
	/// value.
	pub(super) fn live_binding_identity(
		&self,
		name: &crate::syn::Identifier,
	) -> FerrosGrynnResult<TypedExpr> {
		let slot = self
			.slots
			.get(&name.value)
			.ok_or_else(|| self.semantic("Unknown LIVE LET identity.", name.span))?;
		if !slot.reactive && slot.ty != crate::values::Type::Uuid {
			return Err(
				self.semantic("LIVE LET identity requires a reactive binding or UUID.", name.span)
			);
		}
		Ok(TypedExpr {
			kind: TypedExprKind::Slot(slot.id),
			ty: crate::values::Type::Uuid,
			span: name.span,
		})
	}

	/// Uses the same bounded recipe compiler for independent reactive
	/// expressions.
	pub(super) fn live_binding(&self, expression: &Expr) -> FerrosGrynnResult<LogicalLiveBinding> {
		let defaults = LiveBindingOptions::default();
		let work = defaults.work();
		let initial_options = defaults.with_budgets(self.live_scratch, work)?;
		let mut recipe = Recipe {
			analyzer: self,
			captures: BTreeMap::new(),
			operand_slots: BTreeMap::new(),
			budget: BindingAdmission::new(&initial_options),
			frozen_graphs: Vec::new(),
		};
		let value = recipe.expression(expression, None, 0)?;
		let checked = recipe
			.preflight(false, |analyzer| analyzer.expr_context(expression, None, false, true, 0))?;
		if super::typed_expr_contains_aggregate(&checked) {
			return Err(
				self.semantic("LIVE LET aggregates require a SELECT source.", expression.span)
			);
		}
		crate::gql::optimizer::validate_deterministic_statement(&LogicalStatement::Expression {
			value: checked,
		})?;
		Ok(LogicalLiveBinding {
			retained_bytes: initial_options.scratch_bytes()
				- recipe.budget.remaining_options()?.scratch_bytes(),
			expression: value,
			captures: recipe
				.captures
				.into_iter()
				.map(|(name, (slot, reactive))| LogicalLiveCapture {
					name,
					slot,
					reactive,
				})
				.collect(),
		})
	}

	/// Validates ordinary SELECT, then retains a recoverable logical recipe.
	pub(super) fn live_select(
		&self,
		statement: &LiveSelectStatement,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalLiveSelect> {
		let defaults = LiveBindingOptions::default();
		let work = defaults.work();
		let initial_options = defaults.with_budgets(self.live_scratch, work)?;
		let mut recipe = Recipe {
			analyzer: self,
			captures: BTreeMap::new(),
			operand_slots: BTreeMap::new(),
			budget: BindingAdmission::new(&initial_options),
			frozen_graphs: Vec::new(),
		};
		let query =
			recipe.select(&statement.select, span, 0)?.with_output(match statement.output {
				LiveOutputName::Full => LiveOutputMode::Full,
				LiveOutputName::Diff => LiveOutputMode::Diff,
				LiveOutputName::Patch => LiveOutputMode::Patch,
			});
		let since = statement.since.as_ref().map(|value| recipe.option(value)).transpose()?;
		let buffer = statement.buffer.as_ref().map(|value| recipe.option(value)).transpose()?;
		let text = if since.is_none() && buffer.is_none() && recipe.frozen_graphs.is_empty() {
			""
		} else {
			self.operand_spelling(
				since
					.iter()
					.chain(&buffer)
					.map(|expression| expression.span)
					.chain(recipe.frozen_graphs.iter().map(|scope| scope.graph.span)),
			)?
		};
		recipe.budget.admit(text.len(), text.len() as u64)?;
		let operand_spelling = text.into();
		recipe.budget.admit(
			recipe
				.operand_slots
				.len()
				.saturating_mul(std::mem::size_of::<LogicalLiveCapture>() * 2),
			recipe.operand_slots.len() as u64,
		)?;
		Ok(LogicalLiveSelect {
			query,
			frozen_graphs: recipe.frozen_graphs,
			retained_bytes: initial_options.scratch_bytes()
				- recipe.budget.remaining_options()?.scratch_bytes(),
			captures: recipe
				.captures
				.into_iter()
				.map(|(name, (slot, reactive))| LogicalLiveCapture {
					name,
					slot,
					reactive,
				})
				.collect(),
			since,
			buffer,
			operand_spelling,
			operand_slots: recipe
				.operand_slots
				.into_iter()
				.map(|(name, (slot, reactive))| LogicalLiveCapture {
					name,
					slot,
					reactive,
				})
				.collect(),
			overflow: match statement.overflow {
				None | Some(LiveOverflowName::Gap) => LiveOverflowPolicy::Gap,
				Some(LiveOverflowName::Disconnect) => LiveOverflowPolicy::Disconnect,
				Some(LiveOverflowName::Block) => LiveOverflowPolicy::Block,
			},
		})
	}
}

impl Recipe<'_, '_> {
	/// Named scopes bind immediately; ordinary root parameters are frozen,
	/// while tracked and nested expressions retain their logical recipe.
	fn graph(&mut self, expr: &Expr, depth: usize, source: u32) -> FerrosGrynnResult<GraphScope> {
		if depth == 0
			&& let ExprKind::Parameter(name) = &expr.kind
			&& !self.analyzer.slots.get(name.value.as_ref()).is_some_and(|slot| slot.reactive)
		{
			self.expression(expr, None, depth)?;
			self.node(depth, std::mem::size_of::<LogicalLiveSourceCapture>())?;
			let graph = self.option(expr)?;
			self.frozen_graphs.try_reserve_exact(1).map_err(|error| {
				FerrosGrynnError::invalid_argument("LIVE frozen source allocation failed")
					.with_technical_source(error)
			})?;
			self.frozen_graphs.push(LogicalLiveSourceCapture {
				source,
				graph,
			});
			return Ok(GraphScope::Frozen);
		}
		if !matches!(expr.kind, ExprKind::Name(_)) {
			return Ok(GraphScope::Expression(self.expression(expr, None, depth)?));
		}
		match self.analyzer.graph_reference(expr, None)? {
			crate::gql::ir::LogicalGraphReference::Static(graph) => Ok(GraphScope::Static(graph)),
			crate::gql::ir::LogicalGraphReference::Expression(_) => {
				Err(self.analyzer.unsupported("dynamic LIVE graph scope", expr.span))
			}
		}
	}

	/// Every recursive node and owned recipe copy is admitted before
	/// allocation.
	fn node(&mut self, depth: usize, bytes: usize) -> FerrosGrynnResult<()> {
		if let Some(checks) = &self.analyzer.live_checks {
			checks.check()?;
		}
		if depth > 64 {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE syntax recipe depth",
				64,
			));
		}
		let bytes = bytes
			.checked_add(std::mem::size_of::<LiveExpr>() * 2)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE syntax recipe"))?;
		self.budget.admit(bytes, 1)
	}

	/// Current syntax features are never silently dropped from a descriptor.
	fn select(
		&mut self,
		select: &SelectStatement,
		span: TextSpan,
		depth: usize,
	) -> FerrosGrynnResult<LiveQuery> {
		if select.fetch_bridges.is_some()
			|| select.return_graph.as_ref().is_some_and(|graph| graph.bridge.is_some())
		{
			return Err(FerrosGrynnError::query_feature_not_supported(
				"WITH BRIDGE in durable LIVE recipes",
			));
		}
		let query = self.select_inner(select, span, depth)?;
		let checked = self.preflight(false, |analyzer| analyzer.select(select, span))?;
		crate::gql::optimizer::validate_deterministic_statement(&LogicalStatement::Select(
			checked,
		))?;
		Ok(query)
	}

	/// Only captured slots enter the temporary, bounded typed compiler scope.
	fn preflight<Output>(
		&mut self,
		all_slots: bool,
		check: impl FnOnce(&Analyzer<'_>) -> FerrosGrynnResult<Output>,
	) -> FerrosGrynnResult<Output> {
		let mut slots = BTreeMap::new();
		for (name, slot) in &self.analyzer.slots {
			if !all_slots && !self.captures.contains_key(name) {
				continue;
			}
			self.budget.admit(name.len() + 192, 1)?;
			crate::gql::live_lowering::admit_type_shape(&slot.ty, 0, 0, &mut |bytes, work| {
				self.budget.admit(bytes, work)
			})?;
			slots.insert(name.clone(), slot.clone());
		}
		let context_bytes = self
			.analyzer
			.function_stack
			.len()
			.checked_mul(std::mem::size_of::<crate::datastore::FunctionId>())
			.and_then(|bytes| {
				self.analyzer.function_overrides.len().checked_mul(192)?.checked_add(bytes)
			})
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE compiler context"))?;
		self.budget.admit(context_bytes, 1)?;
		let remaining = self.budget.remaining_options()?;
		let cache = crate::gql::closure::LanguageClosureState::bounded_with_work(
			remaining.scratch_bytes(),
			remaining.work(),
		)
		.with_compile_checks(self.analyzer.live_checks.clone());
		let slot_reads = if all_slots {
			let count = self.analyzer.next_slot as usize;
			self.budget.admit(count.saturating_add(32), count as u64)?;
			let mut reads = Vec::new();
			reads.try_reserve_exact(count).map_err(|error| {
				FerrosGrynnError::invalid_argument("LIVE option slot allocation failed")
					.with_technical_source(error)
			})?;
			reads.resize(count, false);
			Some(std::rc::Rc::new(std::cell::RefCell::new(reads)))
		} else {
			None
		};
		let analyzer = Analyzer {
			layout_shell_bytes: std::cell::Cell::new(0),
			slot_reads,
			source: self.analyzer.source,
			catalog: self.analyzer.catalog,
			slots,
			initial_slots: Vec::new(),
			next_slot: self.analyzer.next_slot,
			scopes: vec![Vec::new()],
			variables: self.analyzer.variables,
			statement_now: self.analyzer.statement_now.clone(),
			function_stack: self.analyzer.function_stack.clone(),
			function_overrides: self.analyzer.function_overrides.clone(),
			function_cache: Some(&cache),
			live_checks: self.analyzer.live_checks.clone(),
			live_scratch: self.analyzer.live_scratch,
			current_function: self.analyzer.current_function.clone(),
			current_function_has_explicit_result: self
				.analyzer
				.current_function_has_explicit_result,
		};
		let checked = check(&analyzer)?;
		for (id, _version) in cache.named_dependencies() {
			if let Some(function) = self.analyzer.catalog.function(id)?
				&& crate::syn::has_bridge_clause(function.body())?
			{
				return Err(FerrosGrynnError::query_feature_not_supported(
					"WITH BRIDGE function dependencies in durable LIVE recipes",
				));
			}
		}
		if let Some(reads) = &analyzer.slot_reads {
			let reads = reads.borrow();
			for (name, slot) in &self.analyzer.slots {
				if reads.get(slot.id as usize) == Some(&true)
					&& !self.operand_slots.contains_key(name)
				{
					self.budget.admit(name.len().saturating_add(256), 1)?;
					self.operand_slots.insert(name.clone(), (slot.id, slot.reactive));
				}
			}
		}
		if let Some(checks) = &self.analyzer.live_checks {
			checks.check()?;
		}
		let work = cache.remaining_compile_work().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("LIVE compiler work accounting is absent")
		})?;
		self.budget.admit(0, remaining.work() - work)?;
		if all_slots && let Some(target) = self.analyzer.function_cache {
			cache.promote_named_to(target)?;
		}
		Ok(checked)
	}

	/// Ordinary options share compiler checks without becoming query captures.
	fn option(&mut self, expression: &Expr) -> FerrosGrynnResult<super::TypedExpr> {
		let bytes = (expression.span.len as usize)
			.checked_mul(1024)
			.and_then(|bytes| bytes.checked_add(16_384))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE option syntax"))?;
		self.node(0, bytes)?;
		self.preflight(true, |analyzer| analyzer.expr(expression, None))
	}

	/// Converts one admitted atom source, preserving registration scope
	/// captures.
	fn source(
		&mut self,
		source: &crate::syn::SelectSource,
		span: TextSpan,
		depth: usize,
		slot: u32,
	) -> FerrosGrynnResult<LiveSource> {
		if let SelectSourceKind::Atoms(target) = &source.kind {
			self.node(
				depth,
				(source.span.len as usize)
					.checked_mul(4)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE source identity"))?,
			)?;
			let target = self.analyzer.scan_target(target)?;
			let schema = self.analyzer.catalog.class_definition(target.class())?;
			if let Some(alias) = &source.alias {
				let bytes = alias
					.value
					.len()
					.checked_mul(schema.fields().len().checked_add(5).ok_or_else(|| {
						FerrosGrynnError::operation_overflow("LIVE qualifier columns")
					})?)
					.and_then(|bytes| bytes.checked_mul(16))
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE source qualifier"))?;
				self.budget.admit(bytes, schema.fields().len() as u64)?;
			}
			crate::gql::live_lowering::admit_schema_shape(&schema, &mut |bytes, work| {
				self.budget.admit(bytes, work)
			})?;
		}
		Ok(match &source.kind {
			SelectSourceKind::Atoms(target) => match self.analyzer.scan_target(target)? {
				ScanTarget::Class(name) => {
					let class = self.analyzer.catalog.class_definition(&name)?;
					let id: ClassId = class.id().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("LIVE source class has no identity")
					})?;
					match source
						.graph
						.as_ref()
						.map(|graph| self.graph(graph, depth, slot))
						.transpose()?
					{
						Some(GraphScope::Static(graph)) => LiveSource::GraphClass {
							graph,
							class: id,
						},
						Some(GraphScope::Expression(graph)) => LiveSource::GraphClassExpr {
							graph: Box::new(graph),
							class: id,
						},
						Some(GraphScope::Frozen) | None => LiveSource::Class(id),
					}
				}
				ScanTarget::Record(record) => {
					match source
						.graph
						.as_ref()
						.map(|graph| self.graph(graph, depth, slot))
						.transpose()?
					{
						Some(GraphScope::Static(graph)) => LiveSource::GraphRecord {
							graph,
							record,
						},
						Some(GraphScope::Expression(graph)) => LiveSource::GraphRecordExpr {
							graph: Box::new(graph),
							record,
						},
						Some(GraphScope::Frozen) | None => LiveSource::Record(record),
					}
				}
			},
			SelectSourceKind::Graph(_) => {
				return Err(self
					.analyzer
					.unsupported("LIVE graph metadata rows outside the owned recipe", span));
			}
			_ => {
				return Err(self
					.analyzer
					.unsupported("LIVE source outside CLASS, Record or graph", span));
			}
		})
	}

	/// Admits syntax, literals and source types before typed analysis copies.
	fn select_inner(
		&mut self,
		select: &SelectStatement,
		span: TextSpan,
		depth: usize,
	) -> FerrosGrynnResult<LiveQuery> {
		self.node(depth, std::mem::size_of::<LiveQuery>())?;
		if select.access.is_some()
			|| select.order.iter().any(|order| order.random)
			|| matches!(select.start, Some(SelectStart::After { .. }))
			|| select.tempfiles
			|| !select.union_inputs.is_empty()
		{
			return Err(self
				.analyzer
				.unsupported("LIVE SELECT clauses outside the owned recipe", span));
		}
		let source = self.source(&select.source, span, depth, 0)?;
		let checked_source = self.analyzer.select_source(&select.source, None)?;
		let primary_alias = checked_source.alias;
		let mut input_schema = checked_source.schema;
		self.budget.admit(
			select
				.joins
				.len()
				.checked_mul(2 * std::mem::size_of::<LiveJoin>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE JOIN recipes"))?,
			select.joins.len() as u64,
		)?;
		let mut joins = Vec::with_capacity(select.joins.len());
		for (index, join) in select.joins.iter().enumerate() {
			let slot =
				u32::try_from(index).ok().and_then(|index| index.checked_add(1)).ok_or_else(
					|| FerrosGrynnError::operation_overflow("LIVE JOIN declaration slot"),
				)?;
			let source = self.source(&join.source, join.span, depth, slot)?;
			let checked = self.analyzer.select_source(&join.source, None)?;
			let count = checked.schema.columns.len();
			self.budget.admit(
				count
					.checked_mul(2 * std::mem::size_of::<super::Column>())
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE JOIN schema"))?,
				count as u64,
			)?;
			input_schema.columns.try_reserve_exact(count).map_err(|error| {
				FerrosGrynnError::invalid_argument("LIVE JOIN schema allocation failed")
					.with_technical_source(error)
			})?;
			input_schema.columns.extend(checked.schema.columns);
			self.node(
				depth,
				checked
					.alias
					.len()
					.checked_mul(2)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE JOIN alias"))?,
			)?;
			let predicate = join
				.predicate
				.as_ref()
				.map(|value| {
					self.expression(value, Some((primary_alias.as_ref(), &input_schema)), depth + 1)
				})
				.transpose()?;
			joins.push(LiveJoin::new(
				source,
				checked.alias.as_ref(),
				match join.kind {
					crate::syn::SelectJoinKind::Inner => LiveJoinKind::Inner,
					crate::syn::SelectJoinKind::Left => LiveJoinKind::Left,
					crate::syn::SelectJoinKind::Cross => LiveJoinKind::Cross,
				},
				predicate,
			)?);
		}
		let alias = Some((primary_alias.as_ref(), &input_schema));
		let projection = if let [SelectItem::Wildcard(_)] = select.projection.as_slice() {
			LiveProjection::Records
		} else {
			self.budget.admit(
				select
					.projection
					.len()
					.checked_mul(std::mem::size_of::<LiveField>() * 2)
					.ok_or_else(|| {
						FerrosGrynnError::operation_overflow("LIVE projection recipe")
					})?,
				select.projection.len() as u64,
			)?;
			let mut fields = Vec::with_capacity(select.projection.len());
			for item in &select.projection {
				let SelectItem::Expr {
					expr,
					alias: output,
				} = item
				else {
					return Err(self.analyzer.unsupported("mixed wildcard LIVE projection", span));
				};
				let expression = self.expression(expr, alias, depth + 1)?;
				if select.value {
					return self.finish_select(
						select,
						source,
						joins,
						LiveProjection::Value(expression),
						alias,
						depth,
					);
				}
				let mut field = LiveField::new(expression);
				if let Some(output) = output {
					self.node(depth, output.value.len())?;
					field = field.with_alias(LivePath::field(output.value.as_ref())?);
				}
				fields.push(field);
			}
			LiveProjection::Fields(fields)
		};
		self.finish_select(select, source, joins, projection, alias, depth)
	}

	/// WHERE, grouping, window, OMIT and FETCH retain ordinary SELECT
	/// semantics.
	fn finish_select(
		&mut self,
		select: &SelectStatement,
		source: LiveSource,
		joins: Vec<LiveJoin>,
		projection: LiveProjection,
		alias: Option<(&str, &super::Schema)>,
		depth: usize,
	) -> FerrosGrynnResult<LiveQuery> {
		let mut query = LiveQuery::new(source)
			.with_joins(joins)
			.with_projection(projection)
			.with_only(select.only);
		if let Some(alias) = &select.source.alias {
			let bytes = alias
				.value
				.len()
				.checked_mul(2)
				.and_then(|bytes| bytes.checked_add(std::mem::size_of::<Text>()))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE source alias"))?;
			self.node(depth, bytes)?;
			query = query.with_alias(alias.value.as_ref())?;
		}
		self.budget.admit(
			select
				.split
				.len()
				.checked_mul(2 * std::mem::size_of::<LiveExpr>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE SPLIT recipe"))?,
			select.split.len() as u64,
		)?;
		query = query.with_split(
			select
				.split
				.iter()
				.map(|value| self.expression(value, alias, depth + 1))
				.collect::<FerrosGrynnResult<_>>()?,
		);
		if let Some(filter) = &select.filter {
			query = query.with_filter(self.expression(filter, alias, depth + 1)?);
		}
		query = match &select.group {
			None => query,
			Some(SelectGroup::All(_)) => query.with_group_all(),
			Some(SelectGroup::By(keys)) => {
				self.budget.admit(
					keys.len()
						.checked_mul(2 * std::mem::size_of::<LiveExpr>())
						.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE group recipe"))?,
					keys.len() as u64,
				)?;
				query.with_group_by(
					keys.iter()
						.map(|key| self.expression(key, alias, depth + 1))
						.collect::<FerrosGrynnResult<_>>()?,
				)?
			}
		};
		self.budget.admit(
			select
				.order
				.len()
				.checked_mul(2 * std::mem::size_of::<LiveOrder>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE order recipe"))?,
			select.order.len() as u64,
		)?;
		let mut order = Vec::with_capacity(select.order.len());
		for key in &select.order {
			let recipe = if let ExprKind::Name(name) = &key.expression.kind {
				self.node(depth + 1, name.value.len())?;
				LiveOrder::by_name(name.value.as_ref())?
			} else {
				LiveOrder::new(self.expression(&key.expression, alias, depth + 1)?)
			};
			order.push(
				recipe
					.with_direction(match key.direction {
						SelectOrderDirection::Ascending => LiveOrderDirection::Ascending,
						SelectOrderDirection::Descending => LiveOrderDirection::Descending,
					})
					.with_nulls(match key.nulls {
						SelectNulls::First => LiveNullsOrder::First,
						SelectNulls::Last => LiveNullsOrder::Last,
					})
					.with_collation(key.collate)
					.with_numeric(key.numeric),
			);
		}
		query = query.with_order(order);
		if let Some(limit) = &select.limit {
			query = query.with_limit(self.expression(limit, None, depth + 1)?);
		}
		if let Some(SelectStart::Offset(offset)) = &select.start {
			query = query.with_start_offset(self.expression(offset, None, depth + 1)?);
		}
		let omit = self.paths(&select.omit, depth)?;
		let fetch = self.paths(&select.fetch, depth)?;
		if let Some(graph) = &select.return_graph {
			use crate::{
				graph::{GraphKind, GraphTopologyConstraint},
				syn::{GraphFamilyName, GraphValidatorName},
			};
			self.node(depth + 1, std::mem::size_of::<crate::live::LiveGraphResult>() * 2)?;
			let mut result = crate::live::LiveGraphResult::new(
				self.expression(&graph.nodes, None, depth + 1)?,
				self.expression(&graph.edges, None, depth + 1)?,
			);
			if let Some(contract) = &graph.contract {
				result = result
					.with_family(match contract.family {
						None => GraphKind::Subgraph,
						Some(GraphFamilyName::Graph) => GraphKind::Graph,
						Some(GraphFamilyName::Tree) => GraphKind::Tree,
						Some(GraphFamilyName::RootedTree) => GraphKind::RootedTree,
						Some(GraphFamilyName::Forest) => GraphKind::Forest,
						Some(GraphFamilyName::Dag) => GraphKind::Dag,
						Some(GraphFamilyName::Hyper) => GraphKind::Hypergraph,
						Some(GraphFamilyName::Meta) => GraphKind::Metagraph,
					})
					.with_validators(contract.validators.iter().map(|validator| match validator {
						GraphValidatorName::Simple => GraphTopologyConstraint::Simple,
						GraphValidatorName::Bipartite => GraphTopologyConstraint::Bipartite,
						GraphValidatorName::Complete => GraphTopologyConstraint::Complete,
					}));
			}
			if let Some(root) = &graph.root {
				result = result.with_root(self.expression(root, None, depth + 1)?);
			}
			query = query.with_graph_result(result);
		}
		Ok(query.with_omit(omit).with_fetch(fetch))
	}

	/// Both output operations retain literal segments after checked admission.
	fn paths(&mut self, paths: &[SelectPath], depth: usize) -> FerrosGrynnResult<Vec<LivePath>> {
		self.budget.admit(
			paths
				.len()
				.checked_mul(std::mem::size_of::<LivePath>() * 2)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE output paths recipe"))?,
			paths.len() as u64,
		)?;
		let mut result = Vec::with_capacity(paths.len());
		for path in paths {
			if path.segments.is_empty() || path.segments.len() > 64 {
				return Err(FerrosGrynnError::invalid_argument(
					"LIVE output path requires 1..64 segments",
				));
			}
			let bytes = path
				.segments
				.len()
				.checked_mul(std::mem::size_of::<Text>() * 2)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE output path"))?;
			self.budget.admit(bytes, path.segments.len() as u64)?;
			let mut segments = Vec::with_capacity(path.segments.len());
			for segment in &path.segments {
				let bytes =
					segment.value.len().checked_add(std::mem::size_of::<Text>() * 2).ok_or_else(
						|| FerrosGrynnError::operation_overflow("LIVE output path name"),
					)?;
				self.node(depth, bytes)?;
				segments.push(Text::from(segment.value.as_ref()));
			}
			result.push(LivePath::new(segments)?);
		}
		Ok(result)
	}

	/// Literal decoding and lexical slot resolution reuse ordinary analysis.
	fn expression(
		&mut self,
		expr: &Expr,
		alias: Option<(&str, &super::Schema)>,
		depth: usize,
	) -> FerrosGrynnResult<LiveExpr> {
		self.node(depth, 0)?;
		match &expr.kind {
			ExprKind::Range {
				start,
				end,
				inclusive_end,
			} => Ok(LiveExpr::Range {
				start: start
					.as_deref()
					.map(|value| self.expression(value, alias, depth + 1).map(Box::new))
					.transpose()?,
				end: end
					.as_deref()
					.map(|value| self.expression(value, alias, depth + 1).map(Box::new))
					.transpose()?,
				inclusive_end: *inclusive_end,
			}),
			ExprKind::Cast {
				target,
				expr: value,
			} => {
				crate::live::admit_cast_type_name(
					target.value.as_bytes(),
					depth + 1,
					&mut |bytes, work| self.budget.admit(bytes, work),
				)?;
				Ok(LiveExpr::Cast {
					target: target.value.as_ref().into(),
					value: Box::new(self.expression(value, alias, depth + 1)?),
				})
			}
			ExprKind::Array(values) | ExprKind::Set(values) => {
				self.budget.admit(
					values
						.len()
						.checked_mul(std::mem::size_of::<LiveExpr>() * 2)
						.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE array recipe"))?,
					values.len() as u64,
				)?;
				let values = values
					.iter()
					.map(|value| self.expression(value, alias, depth + 1))
					.collect::<FerrosGrynnResult<_>>()?;
				Ok(if matches!(expr.kind, ExprKind::Set(_)) {
					LiveExpr::Set(values)
				} else {
					LiveExpr::Array(values)
				})
			}
			ExprKind::Object(fields) => {
				self.budget.admit(
					fields
						.len()
						.checked_mul(std::mem::size_of::<(Text, LiveExpr)>() * 2)
						.ok_or_else(|| {
							FerrosGrynnError::operation_overflow("LIVE object recipe")
						})?,
					fields.len() as u64,
				)?;
				let mut output = Vec::with_capacity(fields.len());
				for field in fields {
					self.node(
						depth,
						field.name.value.len().checked_mul(2).ok_or_else(|| {
							FerrosGrynnError::operation_overflow("LIVE object name")
						})?,
					)?;
					output.push((
						Text::from(field.name.value.as_ref()),
						self.expression(&field.value, alias, depth + 1)?,
					));
				}
				Ok(LiveExpr::Object(output))
			}
			ExprKind::Literal(literal) => {
				self.node(
					depth,
					(expr.span.len as usize).checked_mul(2).ok_or_else(|| {
						FerrosGrynnError::operation_overflow("LIVE literal bytes")
					})?,
				)?;
				let value = self.analyzer.literal(literal, expr.span)?;
				self.budget.admit(value.estimated_heap_bytes(), 1)?;
				Ok(LiveExpr::Literal(value))
			}
			ExprKind::Parameter(name) => {
				let slot =
					self.analyzer.slots.get(&name.value).ok_or_else(|| {
						self.analyzer.semantic("Unknown LIVE capture.", name.span)
					})?;
				crate::gql::live_lowering::admit_type_shape(
					&slot.ty,
					0,
					depth + 1,
					&mut |bytes, work| self.budget.admit(bytes, work),
				)?;
				self.node(depth, name.value.len() * 2 + 96)?;
				self.captures.insert(name.value.clone(), (slot.id, slot.reactive));
				Ok(LiveExpr::Parameter(Text::from(name.value.as_ref())))
			}
			ExprKind::Name(name) => self.row_field(name.value.as_ref(), alias, depth, None),
			ExprKind::Field {
				base,
				field,
				null_safe,
			} => {
				if let ExprKind::Name(qualifier) = &base.kind
					&& alias.is_some_and(|(_, schema)| {
						schema
							.qualified_index(qualifier.value.as_ref(), field.value.as_ref())
							.is_some()
					}) {
					return self.row_field(
						field.value.as_ref(),
						alias,
						depth,
						Some(qualifier.value.as_ref()),
					);
				}
				self.node(
					depth,
					field
						.value
						.len()
						.checked_mul(2)
						.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE access field"))?,
				)?;
				Ok(LiveExpr::Access {
					base: Box::new(self.expression(base, alias, depth + 1)?),
					field: Text::from(field.value.as_ref()),
					null_safe: *null_safe,
				})
			}
			ExprKind::Index {
				base,
				index,
			} => Ok(LiveExpr::Index {
				base: Box::new(self.expression(base, alias, depth + 1)?),
				index: Box::new(self.expression(index, alias, depth + 1)?),
			}),
			ExprKind::Unary {
				op: UnaryOperator::Not,
				expr,
			} => Ok(LiveExpr::Not(Box::new(self.expression(expr, alias, depth + 1)?))),
			ExprKind::Unary {
				op: UnaryOperator::Truthy,
				expr,
			} => {
				self.node(depth + 1, 0)?;
				Ok(self.expression(expr, alias, depth + 2)?.truthy())
			}
			ExprKind::Conditional {
				condition,
				then_value,
				else_value,
			} => Ok(LiveExpr::Conditional {
				condition: Box::new(self.expression(condition, alias, depth + 1)?),
				then_value: Box::new(self.expression(then_value, alias, depth + 1)?),
				else_value: Box::new(self.expression(else_value, alias, depth + 1)?),
			}),
			ExprKind::Case {
				operand,
				branches,
				else_value,
			} => {
				let count = branches
					.len()
					.checked_mul(2)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE CASE branches"))?;
				self.budget.admit(
					branches
						.len()
						.checked_mul(std::mem::size_of::<(LiveExpr, LiveExpr)>() * 2)
						.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE CASE recipe"))?,
					count as u64,
				)?;
				Ok(LiveExpr::Case {
					operand: operand
						.as_deref()
						.map(|value| self.expression(value, alias, depth + 1).map(Box::new))
						.transpose()?,
					branches: branches
						.iter()
						.map(|(condition, value)| {
							Ok((
								self.expression(condition, alias, depth + 1)?,
								self.expression(value, alias, depth + 1)?,
							))
						})
						.collect::<FerrosGrynnResult<_>>()?,
					else_value: else_value
						.as_deref()
						.map(|value| self.expression(value, alias, depth + 1).map(Box::new))
						.transpose()?,
				})
			}
			ExprKind::Unary {
				op: UnaryOperator::Negate,
				expr,
			} => Ok(LiveExpr::Negate(Box::new(self.expression(expr, alias, depth + 1)?))),
			ExprKind::Unary {
				op: UnaryOperator::Positive,
				expr,
			} => Ok(LiveExpr::Positive(Box::new(self.expression(expr, alias, depth + 1)?))),
			ExprKind::Binary {
				left,
				op,
				right,
			} => {
				let op = match op {
					BinaryOperator::Equal => LiveBinaryOperator::Equal,
					BinaryOperator::NotEqual => LiveBinaryOperator::NotEqual,
					BinaryOperator::Less => LiveBinaryOperator::Less,
					BinaryOperator::LessOrEqual => LiveBinaryOperator::LessOrEqual,
					BinaryOperator::Greater => LiveBinaryOperator::Greater,
					BinaryOperator::GreaterOrEqual => LiveBinaryOperator::GreaterOrEqual,
					BinaryOperator::And => LiveBinaryOperator::And,
					BinaryOperator::Or => LiveBinaryOperator::Or,
					BinaryOperator::Add => LiveBinaryOperator::Add,
					BinaryOperator::Subtract => LiveBinaryOperator::Subtract,
					BinaryOperator::Multiply => LiveBinaryOperator::Multiply,
					BinaryOperator::Divide => LiveBinaryOperator::Divide,
					BinaryOperator::In => LiveBinaryOperator::In,
					BinaryOperator::Coalesce => LiveBinaryOperator::Coalesce,
					BinaryOperator::Power => LiveBinaryOperator::Power,
					BinaryOperator::Remainder => LiveBinaryOperator::Remainder,
					BinaryOperator::Is => LiveBinaryOperator::Is,
					BinaryOperator::NotIs => LiveBinaryOperator::NotIs,
					BinaryOperator::NotIn => LiveBinaryOperator::NotIn,
					BinaryOperator::AllIn => LiveBinaryOperator::AllIn,
					BinaryOperator::AnyIn => LiveBinaryOperator::AnyIn,
					BinaryOperator::NoneIn => LiveBinaryOperator::NoneIn,
					BinaryOperator::AnyEqual => LiveBinaryOperator::AnyEqual,
					BinaryOperator::AllEqual => LiveBinaryOperator::AllEqual,
					BinaryOperator::Contains => LiveBinaryOperator::Contains,
					BinaryOperator::ContainsNot => LiveBinaryOperator::ContainsNot,
					BinaryOperator::ContainsAny => LiveBinaryOperator::ContainsAny,
					BinaryOperator::ContainsAll => LiveBinaryOperator::ContainsAll,
					BinaryOperator::Elvis => LiveBinaryOperator::Elvis,
					BinaryOperator::Inside => LiveBinaryOperator::Inside,
					BinaryOperator::NotInside => LiveBinaryOperator::NotInside,
					BinaryOperator::AllInside => LiveBinaryOperator::AllInside,
					BinaryOperator::AnyInside => LiveBinaryOperator::AnyInside,
					BinaryOperator::NoneInside => LiveBinaryOperator::NoneInside,
					BinaryOperator::Outside => LiveBinaryOperator::Outside,
					BinaryOperator::Intersects => LiveBinaryOperator::Intersects,
					BinaryOperator::Pipeline => {
						return Err(self.analyzer.unsupported(
							"LIVE binary operation outside the owned recipe",
							expr.span,
						));
					}
				};
				Ok(self
					.expression(left, alias, depth + 1)?
					.binary(op, self.expression(right, alias, depth + 1)?))
			}
			ExprKind::Call {
				callee,
				arguments,
				type_argument,
				distinct,
				argument_names,
			} => {
				let (namespace, name) = builtin_name(callee)
					.ok_or_else(|| self.analyzer.unsupported("dynamic LIVE function", expr.span))?;
				if argument_names.len() != arguments.len() {
					return Err(self.analyzer.unsupported("LIVE call argument names", expr.span));
				}
				self.node(
					depth,
					namespace
						.len()
						.checked_add(name.len())
						.and_then(|bytes| bytes.checked_add(2))
						.ok_or_else(|| {
							FerrosGrynnError::operation_overflow("LIVE function name")
						})?,
				)?;
				let name = if namespace.is_empty() {
					Text::from(name)
				} else {
					Text::from(format!("{namespace}::{name}"))
				};
				if type_argument.is_none()
					&& !distinct
					&& argument_names.iter().all(Option::is_none)
				{
					self.budget.admit(
						arguments
							.len()
							.checked_mul(2 * std::mem::size_of::<LiveExpr>())
							.ok_or_else(|| {
								FerrosGrynnError::operation_overflow("LIVE call arguments")
							})?,
						arguments.len() as u64,
					)?;
					return Ok(LiveExpr::Call {
						name,
						arguments: arguments
							.iter()
							.map(|value| self.expression(value, alias, depth + 1))
							.collect::<FerrosGrynnResult<_>>()?,
					});
				}
				let type_argument = type_argument
					.as_ref()
					.map(|target| -> FerrosGrynnResult<Text> {
						crate::live::admit_cast_type_name(
							target.value.as_bytes(),
							depth + 1,
							&mut |bytes, work| self.budget.admit(bytes, work),
						)?;
						Ok(Text::from(target.value.as_ref()))
					})
					.transpose()?;
				self.budget.admit(
					arguments
						.len()
						.checked_mul(2 * std::mem::size_of::<(Option<Text>, LiveExpr)>())
						.ok_or_else(|| {
							FerrosGrynnError::operation_overflow("LIVE call arguments")
						})?,
					arguments.len() as u64,
				)?;
				let mut values = Vec::with_capacity(arguments.len());
				for (name, value) in argument_names.iter().zip(arguments) {
					let name = name
						.as_ref()
						.map(|name| -> FerrosGrynnResult<Text> {
							self.budget.admit(
								name.value.len().checked_mul(2).ok_or_else(|| {
									FerrosGrynnError::operation_overflow("LIVE argument name")
								})?,
								1,
							)?;
							Ok(Text::from(name.value.as_ref()))
						})
						.transpose()?;
					values.push((name, self.expression(value, alias, depth + 1)?));
				}
				Ok(LiveExpr::CallWithOptions {
					name,
					type_argument,
					distinct: *distinct,
					arguments: values,
				})
			}
			ExprKind::Subquery(select) => {
				Ok(LiveExpr::Select(Box::new(self.select(select, expr.span, depth + 1)?)))
			}
			_ => Err(self
				.analyzer
				.unsupported("LIVE expression outside the owned recipe", expr.span)),
		}
	}

	/// Canonical current-row names preserve ordinary alias/property precedence.
	fn row_field(
		&mut self,
		name: &str,
		alias: Option<(&str, &super::Schema)>,
		depth: usize,
		qualifier: Option<&str>,
	) -> FerrosGrynnResult<LiveExpr> {
		self.node(depth, std::mem::size_of::<Text>() * 2)?;
		self.node(
			depth,
			name.len()
				.checked_mul(2)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE row field"))?,
		)?;
		if let Some((_, schema)) = alias
			&& let Some(index) = qualifier.map_or_else(
				|| schema.index_of(name),
				|qualifier| schema.qualified_index(qualifier, name),
			) {
			crate::gql::live_lowering::admit_type_shape(
				&schema.columns[index].ty,
				0,
				depth + 1,
				&mut |bytes, work| self.budget.admit(bytes, work),
			)?;
		}
		let path = if let Some(qualifier) = qualifier {
			self.node(
				depth,
				qualifier
					.len()
					.checked_mul(2)
					.and_then(|bytes| bytes.checked_add(std::mem::size_of::<Text>() * 2))
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE source qualifier"))?,
			)?;
			LivePath::new(vec![qualifier.into(), name.into()])?
		} else {
			LivePath::field(name)?
		};
		Ok(LiveExpr::Field(path))
	}
}

#[cfg(test)]
/// Regression tests for live behavior.
mod tests {
	use std::collections::{BTreeMap, BTreeSet};

	use super::{Analyzer, Recipe};
	use crate::{
		datastore::{BindingAdmission, ClassField, ClassSchema, Datastore},
		gql::root_catalog::RootCatalog,
		gql_status::{FerrosGrynnError, FerrosGrynnResult},
		graph::AtomKind,
		live::LiveBindingOptions,
		syn::{ParserSettings, StatementKind, parse},
		values::{Object, Type},
	};

	/// Runs the real converter with tighter scratch before ordinary analysis.
	fn convert(store: &Datastore, source: &str, bytes: usize) -> FerrosGrynnResult<()> {
		let root = store.inner.current_root();
		let catalog = RootCatalog(&root);
		let variables = Object::new();
		let analyzer = Analyzer::new(source, &catalog, &variables, &BTreeSet::new());
		let program = parse(source, ParserSettings::default())?;
		let StatementKind::LiveSelect(statement) = &program.statements[0].kind else {
			return Err(FerrosGrynnError::invalid_argument("LIVE recipe fixture missing"));
		};
		let options = LiveBindingOptions::default().with_budgets(bytes, 1_000_000)?;
		let mut recipe = Recipe {
			analyzer: &analyzer,
			captures: BTreeMap::new(),
			operand_slots: BTreeMap::new(),
			budget: BindingAdmission::new(&options),
			frozen_graphs: Vec::new(),
		};
		let _query = recipe.select(&statement.select, program.statements[0].span, 0)?;
		Ok(())
	}

	/// Repeated rich types and long qualifier copies spend the shared cap.
	#[test]
	fn live_gql_recipe_admission_counts_types_aliases_and_record_keys() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let shape = Type::RecordShape(
			(0..8).map(|index| (format!("field_{index:064}").into(), Type::Int)).collect(),
		);
		setup
			.define_class(
				ClassSchema::new("person", AtomKind::Node, [ClassField::new("details", shape)])
					.assert_ok(),
			)
			.assert_ok();
		setup.commit().assert_ok();
		assert!(convert(&store, "LIVE SELECT details FROM person;", 320 * 1024).is_ok());
		let items = vec!["details"; 200].join(", ");
		assert!(convert(&store, &format!("LIVE SELECT {items} FROM person;"), 320 * 1024).is_err());
		let alias = "a".repeat(64 * 1024);
		assert!(
			convert(&store, &format!("LIVE SELECT * FROM person AS `{alias}`;"), 320 * 1024)
				.is_err()
		);
		let large = "a".repeat(128 * 1024);
		assert!(
			convert(&store, &format!("LIVE SELECT * FROM person:'{large}';"), 320 * 1024).is_err()
		);
	}

	/// Type-name parsing is admitted before lowercase/type recursion, including
	/// inactive expressions and the highest supported generic nesting.
	#[test]
	fn live_recipe_cast_type_admission() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		setup
			.define_class(
				ClassSchema::new("person", AtomKind::Node, [ClassField::new("age", Type::Int)])
					.assert_ok(),
			)
			.assert_ok();
		setup.commit().assert_ok();
		for depth in [0, 1, 64] {
			let target = format!("{}int{}", "option<".repeat(depth), ">".repeat(depth));
			convert(
				&store,
				&format!("LIVE SELECT VALUE IF false THEN <{target}>age ELSE 0 END FROM person;"),
				16 * 1024 * 1024,
			)
			.assert_ok();
		}
		let large = "a".repeat(128 * 1024);
		assert!(
			convert(&store, &format!("LIVE SELECT VALUE <{large}>age FROM person;"), 320 * 1024)
				.is_err()
		);
		let target = format!("{}int{}", "option<".repeat(65), ">".repeat(65));
		let error = convert(
			&store,
			&format!("LIVE SELECT VALUE <{target}>age FROM person;"),
			16 * 1024 * 1024,
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("overdeep GQL cast admitted"))
		.assert_ok();
		assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	}

	/// Text preflight rejects oversized first-use PURE bodies without filling
	/// the long-lived session cache, including SINCE/BUFFER expressions.
	#[test]
	fn live_gql_preflight_bounds_first_use_functions_and_options() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		for result in store
			.execute(
				"DEFINE CLASS person TYPE NODE; DEFINE PURE FUNCTION fn::age() -> int { RETURN \
				 18; };",
				&Object::new(),
			)
			.assert_ok()
		{
			result.result.assert_ok();
		}
		let root = store.inner.current_root();
		let catalog = RootCatalog(&root);
		let control = crate::gql::ExecutionControl::new(
			&crate::gql::QueryExecutionOptions {
				barrier_memory_bytes: std::num::NonZeroUsize::new(1024 * 1024)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("fixture compiler budget is zero")
					})
					.assert_ok(),
				..crate::gql::QueryExecutionOptions::default()
			},
			None,
		);
		for source in [
			"LIVE SELECT VALUE fn::age() FROM person;",
			"LIVE SELECT * FROM person BUFFER fn::age();",
		] {
			let program = parse(source, ParserSettings::default()).assert_ok();
			let (logical, _) = super::super::analyze_statement_with_runtime(
				&program.statements[0],
				source,
				&catalog,
				&Object::new(),
				&BTreeSet::new(),
				&control,
			)
			.assert_ok();
			assert_eq!(logical.statements.len(), 1);
			if source.contains("VALUE") {
				assert!(!control.function_cache().has_entries());
			}
		}
		let mut setup = store.begin();
		setup
			.define_function(
				crate::datastore::SchemaFunction::new(
					"fn",
					"large",
					Vec::new(),
					Type::Int,
					crate::datastore::FunctionEffect::Pure,
					false,
					None,
					format!("{{ /* {} */ RETURN 18; }}", "x".repeat(4096)),
				)
				.assert_ok(),
			)
			.assert_ok();
		setup.commit().assert_ok();
		let root = store.inner.current_root();
		let catalog = RootCatalog(&root);
		for source in [
			"LIVE SELECT VALUE fn::large() FROM person;",
			"LIVE SELECT * FROM person BUFFER fn::large();",
		] {
			let program = parse(source, ParserSettings::default()).assert_ok();
			let error = super::super::analyze_statement_with_runtime(
				&program.statements[0],
				source,
				&catalog,
				&Object::new(),
				&BTreeSet::new(),
				&control,
			)
			.err()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("unbounded textual compiler admitted")
			})
			.assert_ok();
			assert!(error.to_string().contains("PURE compiler scratch"), "{error}");
			assert_eq!(control.function_cache().named_dependency_count(), 1);
		}
	}

	/// Separate source declarations retain separate frozen lexical slots.
	#[test]
	fn live_gql_frozen_scopes_keep_source_owners() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		setup.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let left = setup.define_graph("left", crate::graph::GraphKind::Graph).assert_ok();
		let right = setup.define_graph("right", crate::graph::GraphKind::Graph).assert_ok();
		setup.commit().assert_ok();
		let root = store.inner.current_root();
		let catalog = RootCatalog(&root);
		let parameters = Object::from_iter([
			("left", crate::values::Value::from(left)),
			("right", crate::values::Value::from(right)),
		]);
		let source = "SELECT * FROM person IN GRAPH $left; SELECT * FROM person IN GRAPH $right;";
		let analyzer = Analyzer::new(source, &catalog, &parameters, &BTreeSet::new());
		let program = parse(source, ParserSettings::default()).assert_ok();
		let options = LiveBindingOptions::default();
		let mut recipe = Recipe {
			analyzer: &analyzer,
			captures: BTreeMap::new(),
			operand_slots: BTreeMap::new(),
			budget: BindingAdmission::new(&options),
			frozen_graphs: Vec::new(),
		};
		for (slot, statement) in program.statements.iter().enumerate() {
			let StatementKind::Select(select) = &statement.kind else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("source capture fixture missing"));
					test_result.assert_ok();
				};
			};
			let slot = u32::try_from(slot)
				.map_err(|_error| {
					FerrosGrynnError::operation_overflow("source capture fixture slot")
				})
				.assert_ok();
			assert!(matches!(
				recipe.source(&select.source, statement.span, 0, slot).assert_ok(),
				crate::live::LiveSource::Class(_)
			));
			assert!(matches!(
				recipe.source(&select.source, statement.span, 1, slot).assert_ok(),
				crate::live::LiveSource::GraphClassExpr { .. }
			));
		}
		assert_eq!(recipe.frozen_graphs.len(), 2);
		assert_eq!(recipe.frozen_graphs[0].source, 0);
		assert_eq!(recipe.frozen_graphs[1].source, 1);
		let (super::TypedExprKind::Slot(left), super::TypedExprKind::Slot(right)) =
			(&recipe.frozen_graphs[0].graph.kind, &recipe.frozen_graphs[1].graph.kind)
		else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("frozen lexical slots were replaced"));
				test_result.assert_ok();
			};
		};
		assert_ne!(left, right);
	}
}
