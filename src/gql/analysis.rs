use std::collections::BTreeMap;

use super::ir::{
	Aggregate,
	Builtin,
	Column,
	EndpointDomain,
	Expand,
	LogicalBlock,
	LogicalBlockStatement,
	LogicalDmlData,
	LogicalDmlKind,
	LogicalDmlReturn,
	LogicalDmlReturnItem,
	LogicalDmlSelector,
	LogicalDmlStatement,
	LogicalDmlTarget,
	LogicalExpect,
	LogicalGraphDmlStatement,
	LogicalGraphPropertyMutation,
	LogicalGraphReference,
	LogicalMatch,
	LogicalMatchPattern,
	LogicalMatchSelector,
	LogicalOrder,
	LogicalProgram,
	LogicalRecordTarget,
	LogicalRelateEdge,
	LogicalSchemaStatement,
	LogicalSelect,
	LogicalSelectItem,
	LogicalSelectJoin,
	LogicalSelectSource,
	LogicalSelectSourceKind,
	LogicalSelectStart,
	LogicalStatement,
	LogicalWildcardField,
	MatchBinding,
	MatchOp,
	PathExpand,
	ScanTarget,
	Schema,
	TypedExpr,
	TypedExprKind,
};
use crate::{
	datastore::{ClassIndexKind, ClassSchemaMode, IndexScope},
	gql_status::{ExecutionPhase, FerrosGrynnError, FerrosGrynnResult, Location, TextSpan},
	syn::{
		AlterTarget,
		BasicIndexDefinition,
		BinaryOperator,
		ClassKindName,
		ClassSchemaModeName,
		DefineClassStatement,
		DefineFieldStatement,
		DefineGraphStatement,
		DefineIndexStatement,
		DmlData,
		DmlReturn,
		DmlReturnItem,
		DmlTarget,
		ExpectClause,
		Expr,
		ExprKind,
		GraphPropertyMutation,
		IndexScopeName,
		InfoTarget,
		Literal,
		MatchEndpoint,
		MatchEndpointKind,
		MatchPattern,
		MatchSelector,
		MatchStatement,
		Program,
		RecordKey,
		RemoveTarget,
		SelectGroup,
		SelectItem,
		SelectSource,
		SelectSourceKind,
		SelectStart,
		SelectTarget,
		StatementKind,
		TypeName,
		UnaryOperator,
		UpdateGraphStatement,
	},
	values::{
		AtomKind,
		DateTime,
		GraphKind,
		GraphRef,
		Number,
		Object,
		RecordId,
		RecordIdPart,
		Text,
		Type,
		Value,
	},
};

/// Maximum finite path depth accepted by the MVP analyzer.
const MAX_MATCH_PATH_DEPTH: u32 = 30;

pub(super) trait QueryCatalog {
	fn schema(&self, class: &str) -> FerrosGrynnResult<Schema>;
	fn schema_mode(&self, class: &str) -> FerrosGrynnResult<ClassSchemaMode>;
	fn class_kind(&self, class: &str) -> FerrosGrynnResult<AtomKind>;
	/// Resolves a case-sensitive named graph.
	fn graph(&self, name: &str) -> FerrosGrynnResult<GraphRef>;
}

pub(super) fn analyze(
	program: &Program,
	source: &str,
	catalog: &dyn QueryCatalog,
	variables: &Object,
) -> FerrosGrynnResult<LogicalProgram> {
	Analyzer::new(source, catalog, variables).program(program)
}

pub(super) fn analyze_statement(
	statement: &crate::syn::Statement,
	source: &str,
	catalog: &dyn QueryCatalog,
	variables: &Object,
) -> FerrosGrynnResult<LogicalProgram> {
	Analyzer::new(source, catalog, variables).single_statement(statement)
}

struct Analyzer<'a> {
	source: &'a str,
	catalog: &'a dyn QueryCatalog,
	slots: BTreeMap<Box<str>, Slot>,
	initial_slots: Vec<(Box<str>, u32)>,
	next_slot: u32,
	scopes: Vec<Vec<(Box<str>, Option<Slot>)>>,
	variables: &'a Object,
	statement_now: DateTime,
}

#[derive(Debug, Clone)]
struct Slot {
	id: u32,
	ty: Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Semantic category assigned to a MATCH alias.
enum MatchAliasKind {
	/// An atom endpoint with a fixed domain.
	Endpoint(EndpointDomain),
	/// A public path value.
	Path,
}

#[derive(Debug, Clone, Copy)]
/// Existing MATCH alias metadata used for equi-joins.
struct MatchAlias {
	/// Binding-column index.
	column: usize,
	/// Alias semantic category.
	kind: MatchAliasKind,
}

impl<'a> Analyzer<'a> {
	fn new(source: &'a str, catalog: &'a dyn QueryCatalog, variables: &'a Object) -> Self {
		let mut slots = BTreeMap::new();
		let mut initial_slots = Vec::new();
		for (index, (name, value)) in variables.iter().enumerate() {
			let id = index as u32;
			slots.insert(name.as_str().into(), Slot {
				id,
				ty: value.value_type(),
			});
			initial_slots.push((name.as_str().into(), id));
		}
		Self {
			source,
			catalog,
			slots,
			initial_slots,
			next_slot: variables.len() as u32,
			scopes: vec![Vec::new()],
			variables,
			statement_now: DateTime::from(jiff::Zoned::now()),
		}
	}

	fn program(mut self, program: &Program) -> FerrosGrynnResult<LogicalProgram> {
		let mut statements = Vec::with_capacity(program.statements.len());
		for statement in &program.statements {
			self.statement_now = DateTime::from(jiff::Zoned::now());
			statements.push(self.statement(statement, 0)?);
		}
		Ok(LogicalProgram {
			statements,
			initial_slots: self.initial_slots,
			slot_count: self.next_slot as usize,
		})
	}

	fn single_statement(
		mut self,
		statement: &crate::syn::Statement,
	) -> FerrosGrynnResult<LogicalProgram> {
		self.statement_now = DateTime::from(jiff::Zoned::now());
		let statement = self.statement(statement, 0)?;
		Ok(LogicalProgram {
			statements: vec![statement],
			initial_slots: self.initial_slots,
			slot_count: self.next_slot as usize,
		})
	}

	fn statement(
		&mut self,
		statement: &crate::syn::Statement,
		loop_depth: u32,
	) -> FerrosGrynnResult<LogicalStatement> {
		Ok(match &statement.kind {
			StatementKind::Begin(_) | StatementKind::Commit(_) | StatementKind::Cancel => {
				return Err(self.semantic(
					"Transaction statements must be executed through Datastore or GqlSession.",
					statement.span,
				));
			}
			StatementKind::Attach(statement) => {
				let graph = statement.graph.as_ref().ok_or_else(|| {
					self.semantic("ATTACH requires a graph scope.", statement.record.class().span)
				})?;
				if self.catalog.class_kind(statement.record.class().value.as_ref())?
					!= AtomKind::Node
				{
					return Err(self.semantic(
						"ATTACH requires a NODE class; edge occurrences are created by RELATE.",
						statement.record.class().span,
					));
				}
				LogicalStatement::GraphDml(LogicalGraphDmlStatement::Attach {
					record: self.record_target(&statement.record)?,
					graph: self.catalog.graph(&graph.value)?,
					label: statement.label.as_ref().map(|label| Text::from(label.value.as_ref())),
				})
			}
			StatementKind::Create(statement) => LogicalStatement::Dml(self.dml(
				LogicalDmlKind::Create,
				&statement.targets,
				None,
				None,
				statement.data.as_ref(),
				None,
				statement.returning.as_ref(),
				statement.expect.as_ref(),
				false,
			)?),
			StatementKind::Update(statement) => LogicalStatement::Dml(self.dml(
				LogicalDmlKind::Update,
				&statement.targets,
				statement.graph.as_ref(),
				statement.label.as_ref(),
				statement.data.as_ref(),
				statement.filter.as_ref(),
				statement.returning.as_ref(),
				statement.expect.as_ref(),
				false,
			)?),
			StatementKind::UpdateGraph(statement) => {
				LogicalStatement::Schema(self.update_graph(statement)?)
			}
			StatementKind::Upsert(statement) => LogicalStatement::Dml(self.dml(
				LogicalDmlKind::Upsert,
				&statement.targets,
				None,
				None,
				statement.data.as_ref(),
				statement.filter.as_ref(),
				statement.returning.as_ref(),
				statement.expect.as_ref(),
				false,
			)?),
			StatementKind::Delete(statement) => LogicalStatement::Dml(self.dml(
				LogicalDmlKind::Delete,
				&statement.targets,
				None,
				None,
				None,
				statement.filter.as_ref(),
				statement.returning.as_ref(),
				statement.expect.as_ref(),
				statement.cascade,
			)?),
			StatementKind::Detach(statement) => {
				let graph = statement.graph.as_ref().ok_or_else(|| {
					self.semantic("DETACH requires a graph scope.", statement.record.class().span)
				})?;
				LogicalStatement::GraphDml(LogicalGraphDmlStatement::Detach {
					record: self.record_target(&statement.record)?,
					graph: self.catalog.graph(&graph.value)?,
					cascade: statement.cascade,
				})
			}
			StatementKind::DefineGraph(definition) => {
				LogicalStatement::Schema(self.define_graph(definition, statement.span)?)
			}
			StatementKind::DefineClass(definition) => {
				LogicalStatement::Schema(Self::define_class(definition))
			}
			StatementKind::DefineField(definition) => {
				LogicalStatement::Schema(self.define_field(definition)?)
			}
			StatementKind::DefineIndex(definition) => {
				LogicalStatement::Schema(self.define_index(definition)?)
			}
			StatementKind::Remove(statement) => {
				LogicalStatement::Schema(Self::remove(&statement.target))
			}
			StatementKind::Alter(statement) => {
				LogicalStatement::Schema(self.alter(&statement.target)?)
			}
			StatementKind::Info(statement) => {
				LogicalStatement::Schema(Self::info(&statement.target))
			}
			StatementKind::Import(_) => {
				return Err(self
					.semantic("BEGIN IMPORT must be executed through Datastore.", statement.span));
			}
			StatementKind::Let(statement) => {
				let value = self.expr(&statement.value, None)?;
				if let Some(declared) = &statement.declared_type {
					let declared = self.type_name(declared)?;
					if !compatible(&declared, &value.ty) {
						return Err(self.semantic(
							&format!("LET value has type {}, expected {declared}.", value.ty),
							statement.value.span,
						));
					}
				}
				let slot = self.declare(&statement.name, value.ty.clone())?;
				LogicalStatement::Let {
					name: statement.name.value.clone(),
					slot,
					value,
				}
			}
			StatementKind::Return(statement) => LogicalStatement::Return {
				value: self.expr(&statement.value, None)?,
			},
			StatementKind::Relate(statement) => LogicalStatement::GraphDml(self.relate(statement)?),
			StatementKind::Match(statement) => {
				LogicalStatement::Match(self.match_statement(statement)?)
			}
			StatementKind::Select(select) => {
				LogicalStatement::Select(self.select(select, statement.span)?)
			}
			StatementKind::Expression(value) => LogicalStatement::Expression {
				value: self.expr(value, None)?,
			},
			StatementKind::If(statement) => LogicalStatement::If {
				branches: statement
					.branches
					.iter()
					.map(|(condition, block)| {
						Ok((self.expr(condition, None)?, self.block(block, loop_depth, None)?.0))
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?,
				else_block: statement
					.else_block
					.as_ref()
					.map(|block| self.block(block, loop_depth, None).map(|value| value.0))
					.transpose()?,
			},
			StatementKind::For(statement) => {
				let iterable = self.expr(&statement.iterable, None)?;
				let item_type = match &iterable.ty {
					Type::Array(item) | Type::Set(item) | Type::Range(item) => (**item).clone(),
					_ => Type::Any,
				};
				let (body, slot) = self.block(
					&statement.body,
					loop_depth + 1,
					Some((&statement.binding, item_type)),
				)?;
				LogicalStatement::For {
					slot: slot.ok_or_else(|| {
						self.semantic("FOR binding is missing.", statement.binding.span)
					})?,
					iterable,
					body,
				}
			}
			StatementKind::While(statement) => LogicalStatement::While {
				condition: self.expr(&statement.condition, None)?,
				body: self.block(&statement.body, loop_depth + 1, None)?.0,
			},
			StatementKind::Break if loop_depth > 0 => LogicalStatement::Break,
			StatementKind::Continue if loop_depth > 0 => LogicalStatement::Continue,
			StatementKind::Break | StatementKind::Continue => {
				return Err(
					self.semantic("BREAK and CONTINUE require an enclosing loop.", statement.span)
				);
			}
			StatementKind::Try(statement) => {
				let body = self.block(&statement.body, loop_depth, None)?.0;
				let catch = statement
					.catch
					.as_ref()
					.map(|catch| {
						let (body, slot) = self.block(
							&catch.body,
							loop_depth,
							Some((&catch.binding, Type::Error)),
						)?;
						Ok::<_, FerrosGrynnError>((
							slot.ok_or_else(|| {
								self.semantic("CATCH binding is missing.", catch.binding.span)
							})?,
							body,
						))
					})
					.transpose()?;
				LogicalStatement::Try {
					body,
					catch,
				}
			}
			StatementKind::Throw(value) => {
				let value = self.expr(value, None)?;
				if !compatible(&Type::String, &value.ty) {
					return Err(self.semantic("THROW requires a string value.", value.span));
				}
				LogicalStatement::Throw {
					value,
				}
			}
			StatementKind::AssertInvariants(assertion) => {
				let report = assertion.report.unwrap_or(1_000) as usize;
				if report > 1_000 {
					return Err(self
						.semantic("ASSERT INVARIANTS REPORT cannot exceed 1000.", statement.span));
				}
				LogicalStatement::AssertInvariants {
					graph: assertion
						.graph
						.as_ref()
						.map(|graph| self.catalog.graph(&graph.value))
						.transpose()?,
					report,
				}
			}
		})
	}

	fn block(
		&mut self,
		block: &crate::syn::Block,
		loop_depth: u32,
		binding: Option<(&crate::syn::Identifier, Type)>,
	) -> FerrosGrynnResult<(LogicalBlock, Option<u32>)> {
		self.scopes.push(Vec::new());
		let slot = binding.map(|(binding, ty)| self.declare(binding, ty)).transpose()?;
		let mut statements = Vec::with_capacity(block.statements.len());
		for statement in &block.statements {
			let timeout = crate::gql::resolve_timeout(statement.timeout.as_ref(), self.variables)
				.map_err(|error| {
				self.locate(error.with_phase(ExecutionPhase::SemanticAnalysis), statement.span)
			})?;
			statements.push(LogicalBlockStatement {
				statement: self.statement(statement, loop_depth)?,
				timeout,
			});
		}
		self.pop_scope();
		Ok((
			LogicalBlock {
				statements,
			},
			slot,
		))
	}

	fn record_target(&self, target: &DmlTarget) -> FerrosGrynnResult<LogicalRecordTarget> {
		let DmlTarget::Record {
			class,
			key,
			..
		} = target
		else {
			return Err(self.semantic(
				"Graph DML requires an exact class-qualified RecordId.",
				target.class().span,
			));
		};
		Ok(LogicalRecordTarget {
			class: class.value.clone(),
			key: self.expr(key, None)?,
		})
	}

	fn relate(
		&self,
		statement: &crate::syn::RelateStatement,
	) -> FerrosGrynnResult<LogicalGraphDmlStatement> {
		let graph = statement.graph.as_ref().ok_or_else(|| {
			self.semantic("RELATE requires a graph scope.", statement.source.class().span)
		})?;
		for endpoint in [&statement.source, &statement.target] {
			let kind = self.catalog.class_kind(endpoint.class().value.as_ref())?;
			if !statement.atom_endpoints && kind != AtomKind::Node {
				return Err(self.semantic(
					"RELATE endpoints require NODE classes unless `RELATE ATOM` is used.",
					endpoint.class().span,
				));
			}
		}
		if self.catalog.class_kind(statement.edge.class().value.as_ref())? != AtomKind::Edge {
			return Err(
				self.semantic("RELATE edge requires an EDGE class.", statement.edge.class().span)
			);
		}
		let source = self.record_target(&statement.source)?;
		let target = self.record_target(&statement.target)?;
		let class = statement.edge.class().value.clone();
		let mut schema = self.catalog.schema(&class)?;
		let mode = self.catalog.schema_mode(&class)?;
		self.extend_dml_schema(
			&mut schema,
			mode,
			statement.data.as_ref(),
			None,
			statement.returning.as_ref(),
			statement.expect.as_ref(),
		)?;
		let key = match &statement.edge {
			DmlTarget::Class(_) => None,
			DmlTarget::Record {
				key,
				..
			} => Some(self.expr(key, None)?),
			DmlTarget::Range {
				..
			} => {
				return Err(self.semantic(
					"RELATE edge does not accept a record generator.",
					statement.edge.class().span,
				));
			}
		};
		let weight = statement.weight.as_ref().map(|weight| self.expr(weight, None)).transpose()?;
		if let Some(weight) = &weight
			&& !compatible(&Type::Number, &weight.ty)
		{
			return Err(self.semantic("RELATE WEIGHTED requires a number.", weight.span));
		}
		Ok(LogicalGraphDmlStatement::Relate {
			atom_endpoints: statement.atom_endpoints,
			source,
			edge: LogicalRelateEdge {
				class,
				key,
				schema: schema.clone(),
				data: statement
					.data
					.as_ref()
					.map(|data| self.dml_data(data, &schema))
					.transpose()?,
				returning: self.dml_return(
					statement.returning.as_ref(),
					LogicalDmlKind::Create,
					&schema,
				)?,
				expect: statement
					.expect
					.as_ref()
					.map(|expect| self.expect(expect, &schema))
					.transpose()?,
			},
			target,
			graph: self.catalog.graph(&graph.value)?,
			label: statement.label.as_ref().map(|label| Text::from(label.value.as_ref())),
			direction: statement.direction,
			weight,
		})
	}

	fn dml(
		&self,
		kind: LogicalDmlKind,
		targets: &[DmlTarget],
		graph: Option<&crate::syn::Identifier>,
		label: Option<&Option<crate::syn::Identifier>>,
		data: Option<&DmlData>,
		filter: Option<&Expr>,
		returning: Option<&DmlReturn>,
		expect: Option<&ExpectClause>,
		cascade: bool,
	) -> FerrosGrynnResult<LogicalDmlStatement> {
		let mut plans = Vec::with_capacity(targets.len());
		for target in targets {
			let class = target.class().value.clone();
			let mut schema = self.catalog.schema(&class)?;
			let mode = self.catalog.schema_mode(&class)?;
			self.extend_dml_schema(&mut schema, mode, data, filter, returning, expect)?;
			let selector = match target {
				DmlTarget::Class(_) => LogicalDmlSelector::Class,
				DmlTarget::Record {
					key,
					..
				} => LogicalDmlSelector::Record(self.expr(key, None)?),
				DmlTarget::Range {
					count,
					..
				} => LogicalDmlSelector::Range(*count),
			};
			let data = data.map(|data| self.dml_data(data, &schema)).transpose()?;
			let filter = filter.map(|expr| self.expr(expr, Some(&schema))).transpose()?;
			let returning = self.dml_return(returning, kind, &schema)?;
			let expect = expect.map(|expect| self.expect(expect, &schema)).transpose()?;
			plans.push(LogicalDmlTarget {
				class,
				selector,
				schema,
				data,
				filter,
				returning,
				expect,
			});
		}
		Ok(LogicalDmlStatement {
			kind,
			graph: graph.map(|graph| self.catalog.graph(&graph.value)).transpose()?,
			label: label.map(|label| label.as_ref().map(|label| Text::from(label.value.as_ref()))),
			cascade,
			targets: plans,
		})
	}

	fn extend_dml_schema(
		&self,
		schema: &mut Schema,
		mode: ClassSchemaMode,
		data: Option<&DmlData>,
		filter: Option<&Expr>,
		returning: Option<&DmlReturn>,
		expect: Option<&ExpectClause>,
	) -> FerrosGrynnResult<()> {
		let mut names = BTreeMap::<Box<str>, TextSpan>::new();
		if let Some(data) = data {
			match data {
				DmlData::Content(value)
				| DmlData::Merge(value)
				| DmlData::Patch(value)
				| DmlData::Replace(value) => collect_expr_names(value, &mut names),
				DmlData::Set(assignments) => {
					for assignment in assignments {
						names
							.entry(assignment.field.value.clone())
							.or_insert(assignment.field.span);
						collect_expr_names(&assignment.value, &mut names);
					}
				}
				DmlData::Unset(fields) | DmlData::Reset(fields) => {
					for field in fields {
						names.entry(field.value.clone()).or_insert(field.span);
					}
				}
			}
		}
		if let Some(filter) = filter {
			collect_expr_names(filter, &mut names);
		}
		if let Some(returning) = returning {
			match returning {
				DmlReturn::Fields(items) => {
					for item in items {
						if let DmlReturnItem::Expr(expr) = item {
							collect_expr_names(expr, &mut names);
						}
					}
				}
				DmlReturn::Value(value) => collect_expr_names(value, &mut names),
				DmlReturn::None | DmlReturn::Before | DmlReturn::After | DmlReturn::Diff => {}
			}
		}
		if let Some(ExpectClause::Predicate(predicate)) = expect {
			collect_expr_names(predicate, &mut names);
		}
		for (name, span) in names {
			if schema.index_of(&name).is_some() {
				continue;
			}
			if mode == ClassSchemaMode::Schemafull {
				return Err(self.semantic(&format!("Unknown field `{name}`."), span));
			}
			schema.columns.push(Column {
				name,
				qualifier: None,
				ty: Type::Any,
			});
		}
		Ok(())
	}

	fn dml_data(&self, data: &DmlData, schema: &Schema) -> FerrosGrynnResult<LogicalDmlData> {
		let object = |value: &Expr| -> FerrosGrynnResult<TypedExpr> {
			let value = self.expr(value, Some(schema))?;
			if !compatible(&Type::Object, &value.ty) {
				return Err(self.semantic("DML object mode requires an object value.", value.span));
			}
			Ok(value)
		};
		match data {
			DmlData::Content(value) => object(value).map(LogicalDmlData::Content),
			DmlData::Merge(value) => object(value).map(LogicalDmlData::Merge),
			DmlData::Patch(value) => object(value).map(LogicalDmlData::Patch),
			DmlData::Replace(value) => object(value).map(LogicalDmlData::Replace),
			DmlData::Set(assignments) => assignments
				.iter()
				.map(|assignment| {
					if assignment.field.value.eq_ignore_ascii_case("id") {
						return Err(self.semantic(
							"The implicit `id` field is read-only.",
							assignment.field.span,
						));
					}
					let value = self.expr(&assignment.value, Some(schema))?;
					if let Some(index) = schema.index_of(&assignment.field.value)
						&& !compatible(&schema.columns[index].ty, &value.ty)
					{
						return Err(self.semantic(
							&format!(
								"Field `{}` expects {}, found {}.",
								assignment.field.value, schema.columns[index].ty, value.ty
							),
							assignment.value.span,
						));
					}
					Ok((assignment.field.value.clone(), value))
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()
				.map(LogicalDmlData::Set),
			DmlData::Unset(fields) => {
				if let Some(field) =
					fields.iter().find(|field| field.value.eq_ignore_ascii_case("id"))
				{
					return Err(self.semantic("The implicit `id` field is read-only.", field.span));
				}
				Ok(LogicalDmlData::Unset(fields.iter().map(|field| field.value.clone()).collect()))
			}
			DmlData::Reset(fields) => {
				if let Some(field) =
					fields.iter().find(|field| field.value.eq_ignore_ascii_case("id"))
				{
					return Err(self.semantic("The implicit `id` field is read-only.", field.span));
				}
				Ok(LogicalDmlData::Reset(fields.iter().map(|field| field.value.clone()).collect()))
			}
		}
	}

	fn dml_return(
		&self,
		returning: Option<&DmlReturn>,
		kind: LogicalDmlKind,
		schema: &Schema,
	) -> FerrosGrynnResult<LogicalDmlReturn> {
		let Some(returning) = returning else {
			return Ok(if kind == LogicalDmlKind::Delete {
				LogicalDmlReturn::Before
			} else {
				LogicalDmlReturn::After
			});
		};
		match returning {
			DmlReturn::None => Ok(LogicalDmlReturn::None),
			DmlReturn::Before => Ok(LogicalDmlReturn::Before),
			DmlReturn::After => Ok(LogicalDmlReturn::After),
			DmlReturn::Diff => Ok(LogicalDmlReturn::Diff),
			DmlReturn::Value(value) => self.expr(value, Some(schema)).map(LogicalDmlReturn::Value),
			DmlReturn::Fields(items) => items
				.iter()
				.enumerate()
				.map(|(index, item)| match item {
					DmlReturnItem::Wildcard(_) => Ok(LogicalDmlReturnItem::Wildcard),
					DmlReturnItem::Expr(expr) => {
						let name = match &expr.kind {
							ExprKind::Name(name) => name.value.clone(),
							_ => format!("value_{index}").into_boxed_str(),
						};
						Ok(LogicalDmlReturnItem::Expr {
							name,
							value: self.expr(expr, Some(schema))?,
						})
					}
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()
				.map(LogicalDmlReturn::Fields),
		}
	}

	fn expect(&self, expect: &ExpectClause, schema: &Schema) -> FerrosGrynnResult<LogicalExpect> {
		match expect {
			ExpectClause::Exists {
				expected,
				..
			} => Ok(LogicalExpect::Exists(*expected)),
			ExpectClause::Predicate(predicate) => {
				self.expr(predicate, Some(schema)).map(LogicalExpect::Predicate)
			}
		}
	}

	fn define_graph(
		&self,
		definition: &DefineGraphStatement,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		let kind = match definition.constraints.as_slice() {
			[] => GraphKind::Graph,
			[constraint] if constraint.value.eq_ignore_ascii_case("DAG") => GraphKind::Dag,
			[constraint] if constraint.value.eq_ignore_ascii_case("HYPER") => GraphKind::Hypergraph,
			[constraint] if constraint.value.eq_ignore_ascii_case("META") => GraphKind::Metagraph,
			_ => {
				let constraints = definition
					.constraints
					.iter()
					.map(|constraint| constraint.value.as_ref())
					.collect::<Vec<_>>()
					.join(" ");
				return Err(self.unsupported(&format!("DEFINE GRAPH TYPE {constraints}"), span));
			}
		};
		Ok(LogicalSchemaStatement::DefineGraph {
			mode: definition.mode,
			name: definition.name.value.clone(),
			kind,
			content: definition
				.content
				.as_ref()
				.map(|content| {
					let content = self.expr(content, None)?;
					if !compatible(&Type::Object, &content.ty) {
						return Err(self.semantic(
							"DEFINE GRAPH CONTENT requires an object value.",
							content.span,
						));
					}
					Ok(content)
				})
				.transpose()?,
		})
	}

	fn update_graph(
		&self,
		statement: &UpdateGraphStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		let mutation = match &statement.mutation {
			GraphPropertyMutation::Set(assignments) => LogicalGraphPropertyMutation::Set(
				assignments
					.iter()
					.map(|assignment| {
						Ok((
							assignment
								.path
								.fields
								.iter()
								.map(|field| field.value.clone())
								.collect(),
							self.expr(&assignment.value, None)?,
						))
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
			GraphPropertyMutation::Unset(paths) => LogicalGraphPropertyMutation::Unset(
				paths
					.iter()
					.map(|path| path.fields.iter().map(|field| field.value.clone()).collect())
					.collect(),
			),
		};
		let returning = match statement.returning.as_ref().unwrap_or(&DmlReturn::None) {
			DmlReturn::None => LogicalDmlReturn::None,
			DmlReturn::Before => LogicalDmlReturn::Before,
			DmlReturn::After => LogicalDmlReturn::After,
			DmlReturn::Diff => LogicalDmlReturn::Diff,
			DmlReturn::Fields(_) | DmlReturn::Value(_) => {
				return Err(self.semantic(
					"UPDATE GRAPH supports only RETURN NONE, BEFORE, AFTER, or DIFF.",
					statement.graph.span,
				));
			}
		};
		Ok(LogicalSchemaStatement::UpdateGraph {
			graph: self.catalog.graph(&statement.graph.value)?,
			mutation,
			returning,
		})
	}

	fn define_class(definition: &DefineClassStatement) -> LogicalSchemaStatement {
		LogicalSchemaStatement::DefineClass {
			mode: definition.mode,
			name: definition.name.value.clone(),
			kind: class_kind(definition.kind),
			schema_mode: class_schema_mode(definition.schema_mode),
		}
	}

	fn define_field(
		&self,
		definition: &DefineFieldStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		Ok(LogicalSchemaStatement::DefineField {
			mode: definition.mode,
			class: definition.class.value.clone(),
			name: definition.name.value.clone(),
			value_type: definition
				.value_type
				.as_ref()
				.map_or(Ok(Type::Any), |name| self.type_name(name))?,
		})
	}

	fn define_index(
		&self,
		definition: &DefineIndexStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		let (kind, fields) = match &definition.definition {
			BasicIndexDefinition::Standard(fields) => {
				(ClassIndexKind::Standard, fields.iter().map(|field| field.value.clone()).collect())
			}
			BasicIndexDefinition::Unique(fields) => {
				(ClassIndexKind::Unique, fields.iter().map(|field| field.value.clone()).collect())
			}
			BasicIndexDefinition::Count => (ClassIndexKind::Count, Vec::new()),
		};
		let scope = match &definition.scope {
			IndexScopeName::Base => IndexScope::Base,
			IndexScopeName::Effective {
				graph,
			} => IndexScope::Effective {
				graph: self.catalog.graph(&graph.value)?.id(),
			},
		};
		Ok(LogicalSchemaStatement::DefineIndex {
			mode: definition.mode,
			class: definition.class.value.clone(),
			name: definition.name.value.clone(),
			kind,
			fields,
			scope,
		})
	}

	fn remove(target: &RemoveTarget) -> LogicalSchemaStatement {
		match target {
			RemoveTarget::Graph {
				if_exists,
				name,
			} => LogicalSchemaStatement::RemoveGraph {
				if_exists: *if_exists,
				name: name.value.clone(),
			},
			RemoveTarget::Class {
				if_exists,
				name,
			} => LogicalSchemaStatement::RemoveClass {
				if_exists: *if_exists,
				name: name.value.clone(),
			},
			RemoveTarget::Field {
				if_exists,
				name,
				class,
			} => LogicalSchemaStatement::RemoveField {
				if_exists: *if_exists,
				class: class.value.clone(),
				name: name.value.clone(),
			},
			RemoveTarget::Index {
				if_exists,
				name,
				class,
			} => LogicalSchemaStatement::RemoveIndex {
				if_exists: *if_exists,
				class: class.value.clone(),
				name: name.value.clone(),
			},
		}
	}

	fn alter(&self, target: &AlterTarget) -> FerrosGrynnResult<LogicalSchemaStatement> {
		match target {
			AlterTarget::Class {
				if_exists,
				name,
				schema_mode,
			} => Ok(LogicalSchemaStatement::AlterClassMode {
				if_exists: *if_exists,
				name: name.value.clone(),
				mode: class_schema_mode(*schema_mode),
			}),
			AlterTarget::Field {
				if_exists,
				name,
				class,
				value_type,
			} => Ok(LogicalSchemaStatement::AlterFieldType {
				if_exists: *if_exists,
				class: class.value.clone(),
				name: name.value.clone(),
				value_type: value_type
					.as_ref()
					.map_or(Ok(Type::Any), |name| self.type_name(name))?,
			}),
			AlterTarget::IndexPrepareRemove {
				name,
				class,
			} => Ok(LogicalSchemaStatement::PrepareIndexRemoval {
				class: class.value.clone(),
				name: name.value.clone(),
			}),
		}
	}

	fn info(target: &InfoTarget) -> LogicalSchemaStatement {
		match target {
			InfoTarget::Database => LogicalSchemaStatement::InfoDatabase,
			InfoTarget::Class(name) => LogicalSchemaStatement::InfoClass {
				name: name.value.clone(),
			},
			InfoTarget::Index {
				name,
				class,
			} => LogicalSchemaStatement::InfoIndex {
				class: class.value.clone(),
				name: name.value.clone(),
			},
		}
	}

	/// Analyzes a complete MATCH statement.
	fn match_statement(&self, statement: &MatchStatement) -> FerrosGrynnResult<LogicalMatch> {
		let graph = self.catalog.graph(&statement.source.value).map_err(|error| {
			self.locate(error.with_phase(ExecutionPhase::SemanticAnalysis), statement.source.span)
		})?;
		let mut aliases = BTreeMap::new();
		let mut bindings = Schema::default();
		let mut patterns = Vec::with_capacity(statement.patterns.len());
		for (original_index, pattern) in statement.patterns.iter().enumerate() {
			patterns.push(self.match_pattern(
				pattern,
				original_index,
				&mut aliases,
				&mut bindings,
			)?);
		}

		let filter = statement
			.filter
			.as_ref()
			.map(|filter| self.expr(filter, Some(&bindings)))
			.transpose()?;
		let mut projection = Vec::with_capacity(statement.projection.len());
		let mut columns = Vec::with_capacity(statement.projection.len());
		for (index, item) in statement.projection.iter().enumerate() {
			let typed = self.expr(&item.expr, Some(&bindings))?;
			let name = item
				.alias
				.as_ref()
				.map(|alias| alias.value.clone())
				.or_else(|| match &item.expr.kind {
					ExprKind::Name(name) => Some(name.value.clone()),
					_ => None,
				})
				.unwrap_or_else(|| format!("value_{index}").into_boxed_str());
			columns.push(Column {
				name,
				qualifier: None,
				ty: typed.ty.clone(),
			});
			projection.push(typed);
		}
		Ok(LogicalMatch {
			graph,
			patterns,
			bindings,
			filter,
			projection,
			result: Schema {
				columns,
			},
		})
	}

	/// Lowers one source pattern into linear expansion operations.
	fn match_pattern(
		&self,
		pattern: &MatchPattern,
		original_index: usize,
		aliases: &mut BTreeMap<Box<str>, MatchAlias>,
		bindings: &mut Schema,
	) -> FerrosGrynnResult<LogicalMatchPattern> {
		let path_column = pattern
			.path_alias
			.as_ref()
			.map(|alias| {
				self.bind_match_alias(alias, MatchAliasKind::Path, Type::Path, aliases, bindings)
			})
			.transpose()?;
		if path_column.is_some()
			&& (pattern.steps.len() != 1 || pattern.steps[0].edge.length.is_none())
		{
			return Err(self.semantic(
				"The MVP path alias requires exactly one variable-length step.",
				pattern.span,
			));
		}
		let start = self.match_endpoint(&pattern.start, aliases, bindings)?;
		let mut source = start.clone();
		let mut ops = Vec::with_capacity(pattern.steps.len());
		for step in &pattern.steps {
			let target = self.match_endpoint(&step.target, aliases, bindings)?;
			let edge = self.match_binding(
				&step.edge.selector,
				step.edge.alias.as_ref(),
				step.edge.label.as_ref(),
				EndpointDomain::Edge,
				aliases,
				bindings,
			)?;
			if let Some(length) = step.edge.length {
				if let Some(alias) = &step.edge.alias {
					return Err(self.semantic(
						"A variable-length MATCH edge cannot have an alias; bind the path instead.",
						alias.span,
					));
				}
				if length.min > length.max {
					return Err(
						self.semantic("MATCH path minimum exceeds its maximum.", length.span)
					);
				}
				if length.max > MAX_MATCH_PATH_DEPTH {
					return Err(self.semantic(
						&format!(
							"MATCH path depth exceeds the MVP maximum of {MAX_MATCH_PATH_DEPTH}."
						),
						length.span,
					));
				}
				ops.push(MatchOp::PathExpand(PathExpand {
					source: source.clone(),
					edge,
					target: target.clone(),
					direction: step.direction,
					min: length.min,
					max: length.max,
					path_column,
				}));
			} else {
				ops.push(MatchOp::Expand(Expand {
					source: source.clone(),
					edge,
					target: target.clone(),
					direction: step.direction,
				}));
			}
			source = target;
		}
		Ok(LogicalMatchPattern {
			start,
			ops,
			original_index,
		})
	}

	/// Converts one endpoint into a typed binding.
	fn match_endpoint(
		&self,
		endpoint: &MatchEndpoint,
		aliases: &mut BTreeMap<Box<str>, MatchAlias>,
		bindings: &mut Schema,
	) -> FerrosGrynnResult<MatchBinding> {
		let domain = match endpoint.kind {
			MatchEndpointKind::Node => EndpointDomain::Node,
			MatchEndpointKind::Edge => EndpointDomain::Edge,
		};
		self.match_binding(
			&endpoint.selector,
			endpoint.alias.as_ref(),
			endpoint.label.as_ref(),
			domain,
			aliases,
			bindings,
		)
	}

	/// Builds a binding and registers its alias when present.
	fn match_binding(
		&self,
		selector: &MatchSelector,
		alias: Option<&crate::syn::Identifier>,
		label: Option<&crate::syn::Identifier>,
		domain: EndpointDomain,
		aliases: &mut BTreeMap<Box<str>, MatchAlias>,
		bindings: &mut Schema,
	) -> FerrosGrynnResult<MatchBinding> {
		let column = alias
			.map(|alias| {
				let ty = Type::Atom(match domain {
					EndpointDomain::Node => AtomKind::Node,
					EndpointDomain::Edge => AtomKind::Edge,
				});
				self.bind_match_alias(
					alias,
					MatchAliasKind::Endpoint(domain),
					ty,
					aliases,
					bindings,
				)
			})
			.transpose()?;
		Ok(MatchBinding {
			domain,
			selector: self.match_selector(selector, domain)?,
			column,
			label: label.map(|label| label.value.clone()),
		})
	}

	/// Resolves one MATCH selector and validates its class against the
	/// delimiter domain.
	fn match_selector(
		&self,
		selector: &MatchSelector,
		domain: EndpointDomain,
	) -> FerrosGrynnResult<LogicalMatchSelector> {
		let (class, span) = match selector {
			MatchSelector::Any(_) => return Ok(LogicalMatchSelector::Any),
			MatchSelector::Class(class)
			| MatchSelector::Record {
				class,
				..
			} => (class, class.span),
		};
		let expected = match domain {
			EndpointDomain::Node => AtomKind::Node,
			EndpointDomain::Edge => AtomKind::Edge,
		};
		if self.catalog.class_kind(class.value.as_ref())? != expected {
			return Err(self.semantic(
				match domain {
					EndpointDomain::Node => "A node pattern requires a NODE class.",
					EndpointDomain::Edge => "An edge pattern requires an EDGE class.",
				},
				span,
			));
		}
		Ok(match selector {
			MatchSelector::Any(_) => LogicalMatchSelector::Any,
			MatchSelector::Class(class) => LogicalMatchSelector::Class(class.value.clone()),
			MatchSelector::Record {
				class,
				key,
				..
			} => LogicalMatchSelector::Record(LogicalRecordTarget {
				class: class.value.clone(),
				key: self.expr(key, None)?,
			}),
		})
	}

	/// Reuses or creates one alias column while enforcing its kind.
	fn bind_match_alias(
		&self,
		alias: &crate::syn::Identifier,
		kind: MatchAliasKind,
		ty: Type,
		aliases: &mut BTreeMap<Box<str>, MatchAlias>,
		bindings: &mut Schema,
	) -> FerrosGrynnResult<usize> {
		let key: Box<str> = if alias.quoted {
			alias.value.clone()
		} else {
			alias.value.to_ascii_lowercase().into_boxed_str()
		};
		if let Some(existing) = aliases.get(&key) {
			if existing.kind != kind {
				return Err(self.semantic(
					&format!("MATCH alias `{}` is reused with a conflicting kind.", alias.value),
					alias.span,
				));
			}
			return Ok(existing.column);
		}
		let column = bindings.columns.len();
		bindings.columns.push(Column {
			name: alias.value.clone(),
			qualifier: None,
			ty,
		});
		aliases.insert(key, MatchAlias {
			column,
			kind,
		});
		Ok(column)
	}

	fn select(
		&self,
		statement: &crate::syn::SelectStatement,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalSelect> {
		self.select_with_outer(statement, span, None)
	}

	fn select_with_outer(
		&self,
		statement: &crate::syn::SelectStatement,
		span: TextSpan,
		outer: Option<&Schema>,
	) -> FerrosGrynnResult<LogicalSelect> {
		let source = self.select_source(&statement.source, outer)?;
		let mut input_schema = source.schema.clone();
		let mut wildcard = source.wildcard.clone();
		let mut aliases = BTreeMap::new();
		aliases.insert(source.alias.to_ascii_lowercase(), statement.source.span);
		let mut joins = Vec::with_capacity(statement.joins.len());
		for join in &statement.joins {
			let source = self.select_source(&join.source, outer)?;
			let alias = source.alias.to_ascii_lowercase();
			if aliases.insert(alias.clone(), join.source.span).is_some() {
				return Err(self.semantic(
					&format!("SELECT source alias `{}` is duplicated.", source.alias),
					join.source.span,
				));
			}
			let offset = input_schema.columns.len();
			wildcard.extend(source.wildcard.iter().map(|field| offset_wildcard(field, offset)));
			input_schema.columns.extend(source.schema.columns.iter().cloned());
			let predicate = join
				.predicate
				.as_ref()
				.map(|predicate| self.select_expr(predicate, Some(&input_schema), false, 0))
				.transpose()?;
			joins.push(LogicalSelectJoin {
				kind: join.kind,
				source,
				predicate,
			});
		}
		let outer_columns = outer.map_or(0, |schema| schema.columns.len());
		if let Some(outer) = outer {
			input_schema.columns.extend(outer.columns.iter().cloned());
		}
		let filter = statement
			.filter
			.as_ref()
			.map(|filter| self.select_expr(filter, Some(&input_schema), false, 0))
			.transpose()?;
		let split = statement
			.split
			.iter()
			.map(|value| self.select_expr(value, Some(&input_schema), false, 0))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let group = match &statement.group {
			None => None,
			Some(SelectGroup::All(_)) => Some(Vec::new()),
			Some(SelectGroup::By(values)) => Some(
				values
					.iter()
					.map(|value| self.select_expr(value, Some(&input_schema), false, 0))
					.collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
		};
		let mut projection = Vec::with_capacity(statement.projection.len());
		let mut columns = Vec::new();
		let mut contains_aggregate = false;
		for (index, item) in statement.projection.iter().enumerate() {
			match item {
				SelectItem::Wildcard(item_span) => {
					if statement.value {
						return Err(self.semantic("SELECT VALUE does not accept `*`.", *item_span));
					}
					for field in &wildcard {
						if let LogicalWildcardField::Column {
							name,
							index,
						} = field
						{
							columns.push(Column {
								name: name.clone(),
								qualifier: None,
								ty: input_schema.columns[*index].ty.clone(),
							});
						}
					}
					projection.push(LogicalSelectItem::Wildcard(wildcard.clone()));
				}
				SelectItem::Expr {
					expr,
					alias,
				} => {
					let typed = self.select_expr(expr, Some(&input_schema), true, 0)?;
					contains_aggregate |= typed_expr_contains_aggregate(&typed);
					let name = alias
						.as_ref()
						.map(|alias| alias.value.clone())
						.or_else(|| select_expression_name(expr))
						.unwrap_or_else(|| format!("value_{index}").into_boxed_str());
					columns.push(Column {
						name: name.clone(),
						qualifier: None,
						ty: typed.ty.clone(),
					});
					projection.push(LogicalSelectItem::Expression {
						name,
						expression: typed,
					});
				}
			}
		}
		contains_aggregate |=
			statement.order.iter().any(|order| expr_contains_aggregate(&order.expression));
		if (group.is_some() || contains_aggregate)
			&& statement.projection.iter().any(|item| matches!(item, SelectItem::Wildcard(_)))
		{
			return Err(self.semantic("Grouped SELECT does not accept `*`.", span));
		}
		if group.is_some() || contains_aggregate {
			let group_expressions = group.as_deref().unwrap_or_default();
			for (item, logical) in statement.projection.iter().zip(&projection) {
				let (
					SelectItem::Expr {
						expr,
						..
					},
					LogicalSelectItem::Expression {
						expression,
						..
					},
				) = (item, logical)
				else {
					continue;
				};
				if !typed_expr_contains_aggregate(expression)
					&& expr_references_row(expr)
					&& !group_expressions
						.iter()
						.any(|group| group_expr_equivalent(group, expression))
				{
					return Err(self.semantic(
						"A projected non-aggregate expression must appear in GROUP BY.",
						expr.span,
					));
				}
			}
		}
		let group = if contains_aggregate && group.is_none() {
			Some(Vec::new())
		} else {
			group
		};
		let result = Schema {
			columns,
		};
		let order = statement
			.order
			.iter()
			.map(|order| {
				let expression = if let ExprKind::Name(name) = &order.expression.kind
					&& let Some(index) = result.index_of(name.value.as_ref())
				{
					TypedExpr {
						kind: TypedExprKind::Column(input_schema.columns.len() + index),
						ty: result.columns[index].ty.clone(),
						span: order.expression.span,
					}
				} else {
					self.select_expr(&order.expression, Some(&input_schema), true, 0)?
				};
				Ok(LogicalOrder {
					expression,
					direction: order.direction,
					nulls: order.nulls,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		if let Some(group_expressions) = &group {
			for (syntax, logical) in statement.order.iter().zip(&order) {
				let projection_alias = matches!(
					&syntax.expression.kind,
					ExprKind::Name(name) if result.index_of(name.value.as_ref()).is_some()
				);
				if !projection_alias
					&& !typed_expr_contains_aggregate(&logical.expression)
					&& expr_references_row(&syntax.expression)
					&& !group_expressions
						.iter()
						.any(|group| group_expr_equivalent(group, &logical.expression))
				{
					return Err(self.semantic(
						"A non-aggregate ORDER BY expression must appear in GROUP BY.",
						syntax.expression.span,
					));
				}
			}
		}
		let start = match &statement.start {
			None => None,
			Some(SelectStart::Offset(value)) => {
				let value = self.expr(value, None)?;
				if !is_integer(&value.ty) {
					return Err(self.semantic("START AT requires an integer.", value.span));
				}
				Some(LogicalSelectStart::Offset(value))
			}
			Some(SelectStart::After {
				cursor,
				read_committed,
			}) => Some(LogicalSelectStart::After {
				cursor: self.expr(cursor, None)?,
				read_committed: *read_committed,
			}),
		};
		let limit = statement.limit.as_ref().map(|value| self.expr(value, None)).transpose()?;
		if let Some(limit) = &limit
			&& !is_integer(&limit.ty)
		{
			return Err(self.semantic("LIMIT requires an integer.", limit.span));
		}
		Ok(LogicalSelect {
			source,
			joins,
			input_schema,
			filter,
			split,
			group,
			projection,
			result,
			omit: statement
				.omit
				.iter()
				.map(|path| path.segments.iter().map(|part| part.value.clone()).collect())
				.collect(),
			fetch: statement
				.fetch
				.iter()
				.map(|path| path.segments.iter().map(|part| part.value.clone()).collect())
				.collect(),
			order,
			start,
			limit,
			value: statement.value,
			only: statement.only,
			tempfiles: statement.tempfiles,
			outer_columns,
		})
	}

	fn select_source(
		&self,
		source: &SelectSource,
		outer: Option<&Schema>,
	) -> FerrosGrynnResult<LogicalSelectSource> {
		let (kind, mut schema, wildcard, default_alias) = match &source.kind {
			SelectSourceKind::Atoms(target) => {
				let target = self.scan_target(target)?;
				let schema = self.catalog.schema(target.class())?;
				let mode = self.catalog.schema_mode(target.class())?;
				let wildcard = atom_wildcard(&schema, mode);
				let alias: Box<str> = target.class().into();
				(
					LogicalSelectSourceKind::Atoms {
						target,
						graph: source
							.graph
							.as_ref()
							.map(|graph| self.graph_reference(graph, outer))
							.transpose()?,
					},
					schema,
					wildcard,
					alias,
				)
			}
			SelectSourceKind::Graph(reference) => {
				let reference = self.graph_reference(reference, outer)?;
				let alias = match &reference {
					LogicalGraphReference::Static(reference) => reference.name().as_str().into(),
					LogicalGraphReference::Expression(_) => "graph".into(),
				};
				let schema = graph_source_schema();
				(
					LogicalSelectSourceKind::Graphs {
						references: Some(vec![reference]),
					},
					schema.clone(),
					all_columns_wildcard(&schema),
					alias,
				)
			}
			SelectSourceKind::Graphs(references) => {
				let references = references
					.as_ref()
					.map(|references| {
						references
							.iter()
							.map(|reference| self.graph_reference(reference, outer))
							.collect::<FerrosGrynnResult<Vec<_>>>()
					})
					.transpose()?;
				let schema = graph_source_schema();
				(
					LogicalSelectSourceKind::Graphs {
						references,
					},
					schema.clone(),
					all_columns_wildcard(&schema),
					"graphs".into(),
				)
			}
			SelectSourceKind::Derived(select) => {
				let select = self.select_with_outer(select, source.span, None)?;
				let schema = select.result.clone();
				(
					LogicalSelectSourceKind::Derived(Box::new(select)),
					schema.clone(),
					all_columns_wildcard(&schema),
					source
						.alias
						.as_ref()
						.ok_or_else(|| {
							self.semantic("Derived source requires an alias.", source.span)
						})?
						.value
						.clone(),
				)
			}
		};
		let alias = source.alias.as_ref().map_or(default_alias, |alias| alias.value.clone());
		for column in &mut schema.columns {
			column.qualifier = Some(alias.clone());
		}
		Ok(LogicalSelectSource {
			kind,
			schema,
			wildcard,
			alias,
		})
	}

	fn graph_reference(
		&self,
		expression: &Expr,
		schema: Option<&Schema>,
	) -> FerrosGrynnResult<LogicalGraphReference> {
		if let ExprKind::Name(name) = &expression.kind
			&& schema.is_none_or(|schema| schema.index_of(name.value.as_ref()).is_none())
		{
			return self.catalog.graph(name.value.as_ref()).map(LogicalGraphReference::Static);
		}
		Ok(LogicalGraphReference::Expression(self.select_expr(expression, schema, false, 0)?))
	}

	fn expr(&self, expr: &Expr, schema: Option<&Schema>) -> FerrosGrynnResult<TypedExpr> {
		self.expr_context(expr, schema, false, false, 0)
	}

	fn expr_with_current(
		&self,
		expr: &Expr,
		schema: Option<&Schema>,
		allow_current_field: bool,
	) -> FerrosGrynnResult<TypedExpr> {
		self.expr_context(expr, schema, allow_current_field, false, 0)
	}

	fn select_expr(
		&self,
		expr: &Expr,
		schema: Option<&Schema>,
		allow_aggregates: bool,
		aggregate_depth: u32,
	) -> FerrosGrynnResult<TypedExpr> {
		self.expr_context(expr, schema, false, allow_aggregates, aggregate_depth)
	}

	fn expr_context(
		&self,
		expr: &Expr,
		schema: Option<&Schema>,
		allow_current_field: bool,
		allow_aggregates: bool,
		aggregate_depth: u32,
	) -> FerrosGrynnResult<TypedExpr> {
		let (kind, ty) = match &expr.kind {
			ExprKind::Literal(literal) => {
				let value = self.literal(literal, expr.span)?;
				let ty = value.value_type();
				(TypedExprKind::Constant(value), ty)
			}
			ExprKind::Parameter(name) => {
				let slot = self.slots.get(name.value.as_ref()).ok_or_else(|| {
					self.semantic(&format!("Unknown parameter `${}`.", name.value), name.span)
				})?;
				(TypedExprKind::Slot(slot.id), slot.ty.clone())
			}
			ExprKind::Name(name) => {
				let Some(schema) = schema else {
					if allow_current_field {
						return Ok(TypedExpr {
							kind: TypedExprKind::CurrentField(name.value.clone()),
							ty: Type::Any,
							span: expr.span,
						});
					}
					return Err(
						self.semantic(&format!("Unknown field `{}`.", name.value), name.span)
					);
				};
				let mut indices = schema.matching_indices(name.value.as_ref());
				if let Some(index) = indices.next() {
					if indices.next().is_some() {
						return Err(self.semantic(
							&format!(
								"Field `{}` is ambiguous; qualify it with a source alias.",
								name.value
							),
							name.span,
						));
					}
					(TypedExprKind::Column(index), schema.columns[index].ty.clone())
				} else if allow_current_field {
					(TypedExprKind::CurrentField(name.value.clone()), Type::Any)
				} else {
					return Err(
						self.semantic(&format!("Unknown field `{}`.", name.value), name.span)
					);
				}
			}
			ExprKind::Array(values) => {
				let values = values
					.iter()
					.map(|value| {
						self.expr_context(
							value,
							schema,
							allow_current_field,
							allow_aggregates,
							aggregate_depth,
						)
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let item_type =
					values.iter().map(|value| value.ty.clone()).reduce(unify).unwrap_or(Type::Any);
				(TypedExprKind::Array(values), Type::Array(Box::new(item_type)))
			}
			ExprKind::Set(values) => {
				let values = values
					.iter()
					.map(|value| {
						self.expr_context(
							value,
							schema,
							allow_current_field,
							allow_aggregates,
							aggregate_depth,
						)
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let item_type =
					values.iter().map(|value| value.ty.clone()).reduce(unify).unwrap_or(Type::Any);
				(TypedExprKind::Set(values), Type::Set(Box::new(item_type)))
			}
			ExprKind::Object(fields) => {
				let fields = fields
					.iter()
					.map(|field| {
						self.expr_context(
							&field.value,
							schema,
							allow_current_field,
							allow_aggregates,
							aggregate_depth,
						)
						.map(|value| (field.name.value.clone(), value))
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				(TypedExprKind::Object(fields), Type::Object)
			}
			ExprKind::Unary {
				op,
				expr: inner,
			} => {
				let inner = self.expr_context(
					inner,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				let ty = match op {
					UnaryOperator::Not => Type::Bool,
					UnaryOperator::Truthy => Type::Bool,
					UnaryOperator::Negate | UnaryOperator::Positive if is_number(&inner.ty) => {
						inner.ty.clone()
					}
					UnaryOperator::Negate | UnaryOperator::Positive => {
						return Err(
							self.semantic("Unary numeric operator requires a number.", expr.span)
						);
					}
				};
				(
					TypedExprKind::Unary {
						op: *op,
						expr: Box::new(inner),
					},
					ty,
				)
			}
			ExprKind::Binary {
				left,
				op,
				right,
			} => {
				if matches!(op, BinaryOperator::Pipeline) {
					return Err(self.unsupported("expression pipeline", expr.span));
				}
				let left = self.expr_context(
					left,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				let right = if matches!(
					op,
					BinaryOperator::In
						| BinaryOperator::NotIn
						| BinaryOperator::AllIn
						| BinaryOperator::AnyIn
						| BinaryOperator::NoneIn
				) && let ExprKind::Subquery(select) = &right.kind
				{
					if !select.value {
						return Err(self.semantic(
							"A collection subquery requires SELECT VALUE with one projected \
							 column.",
							right.span,
						));
					}
					TypedExpr {
						kind: TypedExprKind::Subquery {
							select: Box::new(self.select_with_outer(select, right.span, schema)?),
							collection: true,
						},
						ty: Type::Array(Box::new(Type::Any)),
						span: right.span,
					}
				} else {
					self.expr_context(
						right,
						schema,
						allow_current_field,
						allow_aggregates,
						aggregate_depth,
					)?
				};
				let ty = self.binary_type(*op, &left.ty, &right.ty, expr.span)?;
				(
					TypedExprKind::Binary {
						left: Box::new(left),
						op: *op,
						right: Box::new(right),
					},
					ty,
				)
			}
			ExprKind::Conditional {
				condition,
				then_value,
				else_value,
			} => {
				let condition = self.expr_context(
					condition,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				let then_value = self.expr_context(
					then_value,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				let else_value = self.expr_context(
					else_value,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				let ty = unify(then_value.ty.clone(), else_value.ty.clone());
				(
					TypedExprKind::Conditional {
						condition: Box::new(condition),
						then_value: Box::new(then_value),
						else_value: Box::new(else_value),
					},
					ty,
				)
			}
			ExprKind::Case {
				operand,
				branches,
				else_value,
			} => {
				let operand = operand
					.as_ref()
					.map(|value| {
						self.expr_context(
							value,
							schema,
							allow_current_field,
							allow_aggregates,
							aggregate_depth,
						)
					})
					.transpose()?
					.map(Box::new);
				let branches = branches
					.iter()
					.map(|(condition, value)| {
						Ok((
							self.expr_context(
								condition,
								schema,
								allow_current_field,
								allow_aggregates,
								aggregate_depth,
							)?,
							self.expr_context(
								value,
								schema,
								allow_current_field,
								allow_aggregates,
								aggregate_depth,
							)?,
						))
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let else_value = else_value
					.as_ref()
					.map(|value| {
						self.expr_context(
							value,
							schema,
							allow_current_field,
							allow_aggregates,
							aggregate_depth,
						)
					})
					.transpose()?
					.map(Box::new);
				let ty = branches
					.iter()
					.map(|(_, value)| value.ty.clone())
					.chain(else_value.iter().map(|value| value.ty.clone()))
					.reduce(unify)
					.unwrap_or(Type::None);
				(
					TypedExprKind::Case {
						operand,
						branches,
						else_value,
					},
					ty,
				)
			}
			ExprKind::Field {
				base,
				field,
				null_safe,
			} => {
				if let ExprKind::Name(qualifier) = &base.kind
					&& let Some(schema) = schema
					&& let Some(index) =
						schema.qualified_index(qualifier.value.as_ref(), field.value.as_ref())
				{
					return Ok(TypedExpr {
						kind: TypedExprKind::Column(index),
						ty: schema.columns[index].ty.clone(),
						span: expr.span,
					});
				}
				let base = self.expr_context(
					base,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				(
					TypedExprKind::Field {
						base: Box::new(base),
						field: field.value.clone(),
						null_safe: *null_safe,
					},
					Type::Any,
				)
			}
			ExprKind::Destructure {
				base,
				fields,
			} => (
				TypedExprKind::Destructure {
					base: Box::new(self.expr_context(
						base,
						schema,
						allow_current_field,
						allow_aggregates,
						aggregate_depth,
					)?),
					fields: fields.iter().map(|field| field.value.clone()).collect(),
				},
				Type::Object,
			),
			ExprKind::Index {
				base,
				index,
			} => {
				let base = self.expr_context(
					base,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				let ty = match &base.ty {
					Type::Array(item) => (**item).clone(),
					_ => Type::Any,
				};
				(
					TypedExprKind::Index {
						base: Box::new(base),
						index: Box::new(self.expr_context(
							index,
							schema,
							allow_current_field,
							allow_aggregates,
							aggregate_depth,
						)?),
					},
					ty,
				)
			}
			ExprKind::Filter {
				base,
				predicate,
			} => {
				let base = self.expr_context(
					base,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				let ty = base.ty.clone();
				(
					TypedExprKind::Filter {
						base: Box::new(base),
						predicate: Box::new(self.expr_context(
							predicate,
							schema,
							true,
							allow_aggregates,
							aggregate_depth,
						)?),
					},
					ty,
				)
			}
			ExprKind::Call {
				callee,
				type_argument,
				distinct,
				arguments,
			} => {
				if let Some(function) = aggregate_name(callee) {
					if !allow_aggregates {
						return Err(self.semantic(
							"Aggregate functions are valid only in a SELECT projection.",
							expr.span,
						));
					}
					if aggregate_depth > 0 {
						return Err(
							self.semantic("Nested aggregate functions are not allowed.", expr.span)
						);
					}
					if type_argument.is_some() {
						return Err(self.semantic(
							"Aggregate functions do not accept a type argument.",
							expr.span,
						));
					}
					let expected = if function == Aggregate::Count {
						0..=1
					} else {
						1..=1
					};
					if !expected.contains(&arguments.len()) {
						return Err(self.semantic(
							&format!("Aggregate function expects {} argument(s).", expected.end()),
							expr.span,
						));
					}
					if *distinct && arguments.is_empty() {
						return Err(
							self.semantic("count(DISTINCT ...) requires an expression.", expr.span)
						);
					}
					let argument = arguments
						.first()
						.map(|argument| {
							self.expr_context(
								argument,
								schema,
								allow_current_field,
								true,
								aggregate_depth + 1,
							)
						})
						.transpose()?
						.map(Box::new);
					let ty = aggregate_type(function, argument.as_deref());
					return Ok(TypedExpr {
						kind: TypedExprKind::Aggregate {
							function,
							argument,
							distinct: *distinct,
						},
						ty,
						span: expr.span,
					});
				}
				if *distinct {
					return Err(self
						.semantic("DISTINCT is valid only inside a SELECT aggregate.", expr.span));
				}
				let arguments = arguments
					.iter()
					.map(|argument| {
						self.expr_context(
							argument,
							schema,
							allow_current_field,
							allow_aggregates,
							aggregate_depth,
						)
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let (function, ty) =
					self.builtin(callee, type_argument.as_ref(), &arguments, expr.span)?;
				(
					TypedExprKind::Builtin {
						function,
						arguments,
					},
					ty,
				)
			}
			ExprKind::Cast {
				target,
				expr: inner,
			} => {
				let target = self.type_name(target)?;
				(
					TypedExprKind::Cast {
						target: target.clone(),
						expr: Box::new(self.expr_context(
							inner,
							schema,
							allow_current_field,
							allow_aggregates,
							aggregate_depth,
						)?),
					},
					target,
				)
			}
			ExprKind::Range {
				start,
				end,
				inclusive_end,
			} => {
				let start = start
					.as_ref()
					.map(|value| {
						self.expr_context(
							value,
							schema,
							allow_current_field,
							allow_aggregates,
							aggregate_depth,
						)
					})
					.transpose()?
					.map(Box::new);
				let end = end
					.as_ref()
					.map(|value| {
						self.expr_context(
							value,
							schema,
							allow_current_field,
							allow_aggregates,
							aggregate_depth,
						)
					})
					.transpose()?
					.map(Box::new);
				let item_type = match (&start, &end) {
					(Some(start), Some(end)) => unify(start.ty.clone(), end.ty.clone()),
					(Some(value), None) | (None, Some(value)) => value.ty.clone(),
					(None, None) => Type::Any,
				};
				(
					TypedExprKind::Range {
						start,
						end,
						inclusive_end: *inclusive_end,
					},
					Type::Range(Box::new(item_type)),
				)
			}
			ExprKind::Subquery(select) if allow_aggregates => (
				TypedExprKind::Subquery {
					select: Box::new(self.select_with_outer(select, expr.span, schema)?),
					collection: false,
				},
				Type::Any,
			),
			ExprKind::Subquery(_) => {
				return Err(self.semantic(
					"A SELECT subquery is valid only inside a SELECT statement.",
					expr.span,
				));
			}
		};
		Ok(TypedExpr {
			kind,
			ty,
			span: expr.span,
		})
	}

	fn builtin(
		&self,
		callee: &Expr,
		type_argument: Option<&TypeName>,
		arguments: &[TypedExpr],
		span: TextSpan,
	) -> FerrosGrynnResult<(Builtin, Type)> {
		let Some((namespace, name)) = builtin_name(callee) else {
			return Err(self.semantic("Built-in functions require a namespace.", callee.span));
		};
		let key = format!("{}::{}", namespace.to_ascii_lowercase(), name.to_ascii_lowercase());
		let (function, arity, result) = match key.as_str() {
			"math::abs" => {
				(Builtin::MathAbs, 1, arguments.first().map_or(Type::Any, |v| v.ty.clone()))
			}
			"math::sqrt" => (Builtin::MathSqrt, 1, Type::Float),
			"math::pow" => (Builtin::MathPow, 2, Type::Number),
			"math::floor" => (Builtin::MathFloor, 1, Type::Float),
			"math::ceil" => (Builtin::MathCeil, 1, Type::Float),
			"string::upper" => (Builtin::StringUpper, 1, Type::String),
			"string::lower" => (Builtin::StringLower, 1, Type::String),
			"string::len" => (Builtin::StringLen, 1, Type::Int),
			"string::contains" => (Builtin::StringContains, 2, Type::Bool),
			"string::split" => (Builtin::StringSplit, 2, Type::Array(Box::new(Type::String))),
			"string::concat" => (Builtin::StringConcat, arguments.len(), Type::String),
			"time::now" => (Builtin::TimeNow(self.statement_now.clone()), 0, Type::DateTime),
			"time::format" => (Builtin::TimeFormat, 2, Type::String),
			"time::add" => (Builtin::TimeAdd, 2, Type::DateTime),
			"time::diff" => (Builtin::TimeDiff, 2, Type::Duration),
			"array::len" => (Builtin::ArrayLen, 1, Type::Int),
			"array::concat" => (Builtin::ArrayConcat, 2, Type::Array(Box::new(Type::Any))),
			"array::distinct" => {
				(Builtin::ArrayDistinct, 1, arguments.first().map_or(Type::Any, |v| v.ty.clone()))
			}
			"array::first" => (Builtin::ArrayFirst, 1, Type::Any),
			"array::flatten" => (Builtin::ArrayFlatten, 1, Type::Array(Box::new(Type::Any))),
			"set::len" => (Builtin::SetLen, 1, Type::Int),
			"set::union" => (Builtin::SetUnion, 2, Type::Set(Box::new(Type::Any))),
			"set::intersect" => (Builtin::SetIntersect, 2, Type::Set(Box::new(Type::Any))),
			"set::diff" => (Builtin::SetDiff, 2, Type::Set(Box::new(Type::Any))),
			"set::has" => (Builtin::SetHas, 2, Type::Bool),
			"meta::id" => (Builtin::MetaId, 1, Type::Any),
			"meta::class" => (Builtin::MetaClass, 1, Type::String),
			"meta::type" => (Builtin::MetaType, 1, Type::String),
			"option::is_some" => (Builtin::OptionIsSome, 1, Type::Bool),
			"option::is_none" => (Builtin::OptionIsNone, 1, Type::Bool),
			"type::is" => {
				let ty = type_argument
					.ok_or_else(|| self.semantic("type::is requires a type argument.", span))?;
				(Builtin::TypeIs(self.type_name(ty)?), 1, Type::Bool)
			}
			"type::to" => {
				let ty = type_argument
					.ok_or_else(|| self.semantic("type::to requires a type argument.", span))?;
				let ty = self.type_name(ty)?;
				(Builtin::TypeTo(ty.clone()), 1, ty)
			}
			_ => return Err(self.unsupported(&format!("function `{key}`"), span)),
		};
		if !matches!(key.as_str(), "type::is" | "type::to") && type_argument.is_some() {
			return Err(self.semantic("This built-in does not accept a type argument.", span));
		}
		if arguments.len() != arity {
			return Err(self.semantic(
				&format!("Function `{key}` expects {arity} arguments, found {}.", arguments.len()),
				span,
			));
		}
		self.validate_builtin_arguments(&key, arguments, span)?;
		if key == "math::pow"
			&& matches!(
				(arguments[0].ty.clone(), arguments[1].ty.clone()),
				(Type::Float, Type::Decimal) | (Type::Decimal, Type::Float)
			) {
			return Err(
				self.semantic("Float and decimal arithmetic requires an explicit cast.", span)
			);
		}
		Ok((function, result))
	}

	fn validate_builtin_arguments(
		&self,
		key: &str,
		arguments: &[TypedExpr],
		span: TextSpan,
	) -> FerrosGrynnResult<()> {
		let expected = match key {
			"math::abs" | "math::sqrt" | "math::floor" | "math::ceil" => {
				vec![BuiltinArgument::Number]
			}
			"math::pow" => vec![BuiltinArgument::Number, BuiltinArgument::Number],
			"string::upper" | "string::lower" | "string::len" => {
				vec![BuiltinArgument::String]
			}
			"string::contains" | "string::split" => {
				vec![BuiltinArgument::String, BuiltinArgument::String]
			}
			"string::concat" => vec![BuiltinArgument::String; arguments.len()],
			"time::now" => Vec::new(),
			"time::format" => vec![BuiltinArgument::DateTime, BuiltinArgument::String],
			"time::add" => vec![BuiltinArgument::DateTime, BuiltinArgument::Duration],
			"time::diff" => vec![BuiltinArgument::DateTime, BuiltinArgument::DateTime],
			"array::len" | "array::distinct" | "array::first" | "array::flatten" => {
				vec![BuiltinArgument::Array]
			}
			"array::concat" => vec![BuiltinArgument::Array, BuiltinArgument::Array],
			"set::len" => vec![BuiltinArgument::Set],
			"set::union" | "set::intersect" | "set::diff" => {
				vec![BuiltinArgument::Set, BuiltinArgument::Set]
			}
			"set::has" => vec![BuiltinArgument::Set, BuiltinArgument::Any],
			"meta::id" | "meta::class" => vec![BuiltinArgument::RecordOrAtom],
			"meta::type" | "option::is_some" | "option::is_none" | "type::is" | "type::to" => {
				vec![BuiltinArgument::Any]
			}
			_ => return Ok(()),
		};
		for (index, (argument, expected)) in arguments.iter().zip(expected).enumerate() {
			if !expected.accepts(&argument.ty) {
				return Err(self.semantic(
					&format!(
						"Function `{key}` argument {} expects {}, found {}.",
						index + 1,
						expected.name(),
						argument.ty
					),
					span,
				));
			}
		}
		Ok(())
	}

	fn binary_type(
		&self,
		op: BinaryOperator,
		left: &Type,
		right: &Type,
		span: TextSpan,
	) -> FerrosGrynnResult<Type> {
		match op {
			BinaryOperator::Power
			| BinaryOperator::Multiply
			| BinaryOperator::Divide
			| BinaryOperator::Remainder
			| BinaryOperator::Add
			| BinaryOperator::Subtract => {
				if matches!(
					(left, right),
					(Type::Float, Type::Decimal) | (Type::Decimal, Type::Float)
				) {
					Err(self
						.semantic("Float and decimal arithmetic requires an explicit cast.", span))
				} else if is_number(left) && is_number(right) {
					Ok(unify(left.clone(), right.clone()))
				} else {
					Err(self.semantic("Arithmetic operators require numbers.", span))
				}
			}
			BinaryOperator::Less
			| BinaryOperator::LessOrEqual
			| BinaryOperator::Greater
			| BinaryOperator::GreaterOrEqual
			| BinaryOperator::Equal
			| BinaryOperator::NotEqual
			| BinaryOperator::Is
			| BinaryOperator::NotIs
			| BinaryOperator::In
			| BinaryOperator::NotIn
			| BinaryOperator::AllIn
			| BinaryOperator::AnyIn
			| BinaryOperator::NoneIn
			| BinaryOperator::AnyEqual
			| BinaryOperator::AllEqual
			| BinaryOperator::Contains
			| BinaryOperator::ContainsNot
			| BinaryOperator::ContainsAny
			| BinaryOperator::ContainsAll
			| BinaryOperator::And
			| BinaryOperator::Or => Ok(Type::Bool),
			BinaryOperator::Coalesce | BinaryOperator::Elvis => {
				Ok(unify(left.clone(), right.clone()))
			}
			BinaryOperator::Pipeline => Err(self.unsupported("expression pipeline", span)),
		}
	}

	fn literal(&self, literal: &Literal, span: TextSpan) -> FerrosGrynnResult<Value> {
		match literal {
			Literal::None => Ok(Value::None),
			Literal::Null => Ok(Value::Null),
			Literal::Bool(value) => Ok(Value::Bool(*value)),
			Literal::String(value) => Ok(Value::from(value.as_ref())),
			Literal::Number(value) => {
				value.replace('_', "").parse::<Number>().map(Value::Number).map_err(|error| {
					self.locate(error.with_phase(ExecutionPhase::SemanticAnalysis), span)
				})
			}
		}
	}

	fn scan_target(&self, target: &SelectTarget) -> FerrosGrynnResult<ScanTarget> {
		match target {
			SelectTarget::Class(class) => Ok(ScanTarget::Class(class.value.clone())),
			SelectTarget::Record {
				class,
				key,
				span,
			} => {
				let key = match key {
					RecordKey::Integer(value, _) => {
						value.parse::<i64>().map(RecordIdPart::Int).map_err(|_err| {
							self.semantic("Record integer ID is out of range.", *span)
						})?
					}
					RecordKey::String(value, _) => RecordIdPart::String(value.as_ref().into()),
				};
				RecordId::new(class.value.as_ref(), key).map(ScanTarget::Record).map_err(|error| {
					self.locate(error.with_phase(ExecutionPhase::SemanticAnalysis), *span)
				})
			}
		}
	}

	fn type_name(&self, name: &TypeName) -> FerrosGrynnResult<Type> {
		let ty = match name.value.to_ascii_lowercase().as_str() {
			"any" => Type::Any,
			"none" => Type::None,
			"null" => Type::Null,
			"bool" => Type::Bool,
			"number" => Type::Number,
			"int" => Type::Int,
			"float" => Type::Float,
			"decimal" => Type::Decimal,
			"string" => Type::String,
			"duration" => Type::Duration,
			"datetime" => Type::DateTime,
			"bytes" => Type::Bytes,
			"array" => Type::Array(Box::new(Type::Any)),
			"set" => Type::Set(Box::new(Type::Any)),
			"range" => Type::Range(Box::new(Type::Any)),
			"object" => Type::Object,
			"record" => Type::Record,
			"error" => Type::Error,
			_ => return Err(self.semantic(&format!("Unknown type `{}`.", name.value), name.span)),
		};
		Ok(ty)
	}

	fn declare(&mut self, name: &crate::syn::Identifier, ty: Type) -> FerrosGrynnResult<u32> {
		let current = self
			.scopes
			.last()
			.ok_or_else(|| self.semantic("Lexical scope is unavailable.", name.span))?;
		if name.value.as_ref() != "_" && current.iter().any(|(declared, _)| declared == &name.value)
		{
			return Err(self.semantic(
				&format!("Binding `${}` is already declared in this scope.", name.value),
				name.span,
			));
		}
		let id = self.next_slot;
		self.next_slot = self
			.next_slot
			.checked_add(1)
			.ok_or_else(|| self.semantic("The statement declares too many bindings.", name.span))?;
		if name.value.as_ref() == "_" {
			return Ok(id);
		}
		let previous = self.slots.insert(name.value.clone(), Slot {
			id,
			ty,
		});
		if let Some(scope) = self.scopes.last_mut() {
			scope.push((name.value.clone(), previous));
		}
		Ok(id)
	}

	fn pop_scope(&mut self) {
		let Some(scope) = self.scopes.pop() else {
			return;
		};
		for (name, previous) in scope.into_iter().rev() {
			if let Some(previous) = previous {
				self.slots.insert(name, previous);
			} else {
				self.slots.remove(&name);
			}
		}
	}

	fn semantic(&self, message: &str, span: TextSpan) -> FerrosGrynnError {
		self.locate(
			FerrosGrynnError::invalid_argument(message)
				.with_phase(ExecutionPhase::SemanticAnalysis),
			span,
		)
	}

	fn unsupported(&self, feature: &str, span: TextSpan) -> FerrosGrynnError {
		self.locate(
			FerrosGrynnError::query_feature_not_supported(feature)
				.with_phase(ExecutionPhase::SemanticAnalysis),
			span,
		)
	}

	fn locate(&self, error: FerrosGrynnError, span: TextSpan) -> FerrosGrynnError {
		Location::from_source(None, self.source, span)
			.map_or(error.clone(), |location| error.with_location(location))
	}
}

#[derive(Clone, Copy)]
enum BuiltinArgument {
	Any,
	Number,
	String,
	DateTime,
	Duration,
	Array,
	Set,
	RecordOrAtom,
}

impl BuiltinArgument {
	const fn accepts(self, ty: &Type) -> bool {
		matches!(ty, Type::Any)
			|| match self {
				Self::Any => true,
				Self::Number => is_number(ty),
				Self::String => matches!(ty, Type::String),
				Self::DateTime => matches!(ty, Type::DateTime),
				Self::Duration => matches!(ty, Type::Duration),
				Self::Array => matches!(ty, Type::Array(_)),
				Self::Set => matches!(ty, Type::Set(_)),
				Self::RecordOrAtom => matches!(ty, Type::Record | Type::Atom(_)),
			}
	}

	const fn name(self) -> &'static str {
		match self {
			Self::Any => "any",
			Self::Number => "number",
			Self::String => "string",
			Self::DateTime => "datetime",
			Self::Duration => "duration",
			Self::Array => "array",
			Self::Set => "set",
			Self::RecordOrAtom => "record or atom",
		}
	}
}

fn collect_expr_names(expr: &Expr, names: &mut BTreeMap<Box<str>, TextSpan>) {
	match &expr.kind {
		ExprKind::Name(name) => {
			names.entry(name.value.clone()).or_insert(name.span);
		}
		ExprKind::Array(values) | ExprKind::Set(values) => {
			for value in values {
				collect_expr_names(value, names);
			}
		}
		ExprKind::Object(fields) => {
			for field in fields {
				collect_expr_names(&field.value, names);
			}
		}
		ExprKind::Unary {
			expr,
			..
		}
		| ExprKind::Cast {
			expr,
			..
		} => collect_expr_names(expr, names),
		ExprKind::Binary {
			left,
			right,
			..
		} => {
			collect_expr_names(left, names);
			collect_expr_names(right, names);
		}
		ExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			collect_expr_names(condition, names);
			collect_expr_names(then_value, names);
			collect_expr_names(else_value, names);
		}
		ExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			if let Some(operand) = operand {
				collect_expr_names(operand, names);
			}
			for (condition, value) in branches {
				collect_expr_names(condition, names);
				collect_expr_names(value, names);
			}
			if let Some(else_value) = else_value {
				collect_expr_names(else_value, names);
			}
		}
		ExprKind::Field {
			base,
			..
		}
		| ExprKind::Destructure {
			base,
			..
		} => collect_expr_names(base, names),
		ExprKind::Index {
			base,
			index,
		} => {
			collect_expr_names(base, names);
			collect_expr_names(index, names);
		}
		ExprKind::Call {
			callee,
			arguments,
			..
		} => {
			collect_expr_names(callee, names);
			for argument in arguments {
				collect_expr_names(argument, names);
			}
		}
		ExprKind::Subquery(select) => {
			for item in &select.projection {
				if let crate::syn::SelectItem::Expr {
					expr,
					..
				} = item
				{
					collect_expr_names(expr, names);
				}
			}
		}
		ExprKind::Range {
			start,
			end,
			..
		} => {
			if let Some(start) = start {
				collect_expr_names(start, names);
			}
			if let Some(end) = end {
				collect_expr_names(end, names);
			}
		}
		ExprKind::Filter {
			base,
			predicate,
		} => {
			collect_expr_names(base, names);
			collect_expr_names(predicate, names);
		}
		ExprKind::Literal(_) | ExprKind::Parameter(_) => {}
	}
}

fn builtin_name(callee: &Expr) -> Option<(&str, &str)> {
	let ExprKind::Field {
		base,
		field,
		null_safe: false,
	} = &callee.kind
	else {
		return None;
	};
	let ExprKind::Name(namespace) = &base.kind else {
		return None;
	};
	Some((namespace.value.as_ref(), field.value.as_ref()))
}

fn aggregate_name(callee: &Expr) -> Option<Aggregate> {
	if let ExprKind::Name(name) = &callee.kind {
		return match name.value.to_ascii_lowercase().as_str() {
			"count" => Some(Aggregate::Count),
			"sum" => Some(Aggregate::Sum),
			"avg" => Some(Aggregate::Average),
			"min" => Some(Aggregate::Minimum),
			"max" => Some(Aggregate::Maximum),
			_ => None,
		};
	}
	let (namespace, name) = builtin_name(callee)?;
	match (namespace.to_ascii_lowercase().as_str(), name.to_ascii_lowercase().as_str()) {
		("array", "group") => Some(Aggregate::ArrayGroup),
		("math", "variance_pop") => Some(Aggregate::VariancePopulation),
		("math", "stddev_pop") => Some(Aggregate::StandardDeviationPopulation),
		("math", "variance_samp") => Some(Aggregate::VarianceSample),
		("math", "stddev_samp") => Some(Aggregate::StandardDeviationSample),
		_ => None,
	}
}

fn aggregate_type(function: Aggregate, argument: Option<&TypedExpr>) -> Type {
	match function {
		Aggregate::Count => Type::Int,
		Aggregate::Sum => argument.map_or(Type::Number, |value| value.ty.clone()),
		Aggregate::Average
		| Aggregate::VariancePopulation
		| Aggregate::StandardDeviationPopulation
		| Aggregate::VarianceSample
		| Aggregate::StandardDeviationSample => Type::Float,
		Aggregate::Minimum | Aggregate::Maximum => {
			argument.map_or(Type::Any, |value| value.ty.clone())
		}
		Aggregate::ArrayGroup => Type::Array(Box::new(Type::Any)),
	}
}

fn graph_source_schema() -> Schema {
	Schema {
		columns: [
			("id", Type::Any),
			("name", Type::String),
			("kind", Type::String),
			("storage_mode", Type::String),
			("revision", Type::Int),
			("node_count", Type::Int),
			("edge_count", Type::Int),
			("properties", Type::Object),
		]
		.into_iter()
		.map(|(name, ty)| Column {
			name: name.into(),
			qualifier: None,
			ty,
		})
		.collect(),
	}
}

fn all_columns_wildcard(schema: &Schema) -> Vec<LogicalWildcardField> {
	schema
		.columns
		.iter()
		.enumerate()
		.map(|(index, column)| LogicalWildcardField::Column {
			name: column.name.clone(),
			index,
		})
		.collect()
}

fn atom_wildcard(schema: &Schema, mode: ClassSchemaMode) -> Vec<LogicalWildcardField> {
	let mut fields = Vec::new();
	if let Some(index) = schema.index_of("id") {
		fields.push(LogicalWildcardField::Column {
			name: "id".into(),
			index,
		});
	}
	match mode {
		ClassSchemaMode::Schemafull => {
			fields.extend(schema.columns.iter().enumerate().filter_map(|(index, column)| {
				(!matches!(column.name.as_ref(), "id" | "class" | "kind" | "label" | "properties"))
					.then(|| LogicalWildcardField::Column {
						name: column.name.clone(),
						index,
					})
			}));
		}
		ClassSchemaMode::Schemaless => {
			if let Some(index) = schema.index_of("properties") {
				fields.push(LogicalWildcardField::Properties {
					index,
				});
			}
		}
	}
	fields
}

fn offset_wildcard(field: &LogicalWildcardField, offset: usize) -> LogicalWildcardField {
	match field {
		LogicalWildcardField::Column {
			name,
			index,
		} => LogicalWildcardField::Column {
			name: name.clone(),
			index: index + offset,
		},
		LogicalWildcardField::Properties {
			index,
		} => LogicalWildcardField::Properties {
			index: index + offset,
		},
	}
}

fn select_expression_name(expression: &Expr) -> Option<Box<str>> {
	match &expression.kind {
		ExprKind::Name(name) => Some(name.value.clone()),
		ExprKind::Field {
			field,
			..
		} => Some(field.value.clone()),
		_ => None,
	}
}

fn expr_contains_aggregate(expression: &Expr) -> bool {
	match &expression.kind {
		ExprKind::Call {
			callee,
			arguments,
			..
		} => aggregate_name(callee).is_some() || arguments.iter().any(expr_contains_aggregate),
		ExprKind::Array(values) | ExprKind::Set(values) => {
			values.iter().any(expr_contains_aggregate)
		}
		ExprKind::Object(fields) => {
			fields.iter().any(|field| expr_contains_aggregate(&field.value))
		}
		ExprKind::Unary {
			expr,
			..
		}
		| ExprKind::Field {
			base: expr,
			..
		}
		| ExprKind::Destructure {
			base: expr,
			..
		}
		| ExprKind::Cast {
			expr,
			..
		} => expr_contains_aggregate(expr),
		ExprKind::Binary {
			left,
			right,
			..
		}
		| ExprKind::Index {
			base: left,
			index: right,
		}
		| ExprKind::Filter {
			base: left,
			predicate: right,
		} => expr_contains_aggregate(left) || expr_contains_aggregate(right),
		ExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			expr_contains_aggregate(condition)
				|| expr_contains_aggregate(then_value)
				|| expr_contains_aggregate(else_value)
		}
		ExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			operand.iter().any(|value| expr_contains_aggregate(value))
				|| branches.iter().any(|(condition, value)| {
					expr_contains_aggregate(condition) || expr_contains_aggregate(value)
				})
				|| else_value.iter().any(|value| expr_contains_aggregate(value))
		}
		ExprKind::Range {
			start,
			end,
			..
		} => start.iter().chain(end.iter()).any(|value| expr_contains_aggregate(value)),
		ExprKind::Literal(_)
		| ExprKind::Parameter(_)
		| ExprKind::Name(_)
		| ExprKind::Subquery(_) => false,
	}
}

fn expr_references_row(expression: &Expr) -> bool {
	match &expression.kind {
		ExprKind::Name(_) => true,
		ExprKind::Call {
			arguments,
			..
		} => arguments.iter().any(expr_references_row),
		ExprKind::Array(values) | ExprKind::Set(values) => values.iter().any(expr_references_row),
		ExprKind::Object(fields) => fields.iter().any(|field| expr_references_row(&field.value)),
		ExprKind::Unary {
			expr,
			..
		}
		| ExprKind::Field {
			base: expr,
			..
		}
		| ExprKind::Destructure {
			base: expr,
			..
		}
		| ExprKind::Cast {
			expr,
			..
		} => expr_references_row(expr),
		ExprKind::Binary {
			left,
			right,
			..
		}
		| ExprKind::Index {
			base: left,
			index: right,
		}
		| ExprKind::Filter {
			base: left,
			predicate: right,
		} => expr_references_row(left) || expr_references_row(right),
		ExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			expr_references_row(condition)
				|| expr_references_row(then_value)
				|| expr_references_row(else_value)
		}
		ExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			operand.iter().any(|value| expr_references_row(value))
				|| branches.iter().any(|(condition, value)| {
					expr_references_row(condition) || expr_references_row(value)
				})
				|| else_value.iter().any(|value| expr_references_row(value))
		}
		ExprKind::Range {
			start,
			end,
			..
		} => start.iter().chain(end.iter()).any(|value| expr_references_row(value)),
		ExprKind::Literal(_) | ExprKind::Parameter(_) | ExprKind::Subquery(_) => false,
	}
}

fn typed_expr_contains_aggregate(expression: &TypedExpr) -> bool {
	match &expression.kind {
		TypedExprKind::Aggregate {
			..
		} => true,
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			values.iter().any(typed_expr_contains_aggregate)
		}
		TypedExprKind::Object(fields) => {
			fields.iter().any(|(_, value)| typed_expr_contains_aggregate(value))
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => start.iter().chain(end.iter()).any(|value| typed_expr_contains_aggregate(value)),
		TypedExprKind::Unary {
			expr,
			..
		}
		| TypedExprKind::Field {
			base: expr,
			..
		}
		| TypedExprKind::Destructure {
			base: expr,
			..
		}
		| TypedExprKind::Cast {
			expr,
			..
		} => typed_expr_contains_aggregate(expr),
		TypedExprKind::Binary {
			left,
			right,
			..
		} => typed_expr_contains_aggregate(left) || typed_expr_contains_aggregate(right),
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			typed_expr_contains_aggregate(condition)
				|| typed_expr_contains_aggregate(then_value)
				|| typed_expr_contains_aggregate(else_value)
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			operand.iter().any(|value| typed_expr_contains_aggregate(value))
				|| branches.iter().any(|(condition, value)| {
					typed_expr_contains_aggregate(condition) || typed_expr_contains_aggregate(value)
				})
				|| else_value.iter().any(|value| typed_expr_contains_aggregate(value))
		}
		TypedExprKind::Index {
			base,
			index,
		} => typed_expr_contains_aggregate(base) || typed_expr_contains_aggregate(index),
		TypedExprKind::Filter {
			base,
			predicate,
		} => typed_expr_contains_aggregate(base) || typed_expr_contains_aggregate(predicate),
		TypedExprKind::Builtin {
			arguments,
			..
		} => arguments.iter().any(typed_expr_contains_aggregate),
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_)
		| TypedExprKind::Subquery {
			..
		} => false,
	}
}

fn group_expr_equivalent(left: &TypedExpr, right: &TypedExpr) -> bool {
	match (&left.kind, &right.kind) {
		(TypedExprKind::Constant(left), TypedExprKind::Constant(right)) => left == right,
		(TypedExprKind::Slot(left), TypedExprKind::Slot(right)) => left == right,
		(TypedExprKind::Column(left), TypedExprKind::Column(right)) => left == right,
		(TypedExprKind::Array(left), TypedExprKind::Array(right))
		| (TypedExprKind::Set(left), TypedExprKind::Set(right)) => {
			left.len() == right.len()
				&& left.iter().zip(right).all(|(left, right)| group_expr_equivalent(left, right))
		}
		(TypedExprKind::Object(left), TypedExprKind::Object(right)) => {
			left.len() == right.len()
				&& left.iter().zip(right).all(|((left_name, left), (right_name, right))| {
					left_name == right_name && group_expr_equivalent(left, right)
				})
		}
		(
			TypedExprKind::Range {
				start: left_start,
				end: left_end,
				inclusive_end: left_inclusive,
			},
			TypedExprKind::Range {
				start: right_start,
				end: right_end,
				inclusive_end: right_inclusive,
			},
		) => {
			left_inclusive == right_inclusive
				&& optional_group_expr_equivalent(left_start.as_deref(), right_start.as_deref())
				&& optional_group_expr_equivalent(left_end.as_deref(), right_end.as_deref())
		}
		(
			TypedExprKind::Unary {
				op: left_op,
				expr: left,
			},
			TypedExprKind::Unary {
				op: right_op,
				expr: right,
			},
		) => left_op == right_op && group_expr_equivalent(left, right),
		(
			TypedExprKind::Binary {
				left: left_a,
				op: left_op,
				right: left_b,
			},
			TypedExprKind::Binary {
				left: right_a,
				op: right_op,
				right: right_b,
			},
		) => {
			left_op == right_op
				&& group_expr_equivalent(left_a, right_a)
				&& group_expr_equivalent(left_b, right_b)
		}
		(
			TypedExprKind::Field {
				base: left,
				field: left_field,
				null_safe: left_safe,
			},
			TypedExprKind::Field {
				base: right,
				field: right_field,
				null_safe: right_safe,
			},
		) => {
			left_field == right_field
				&& left_safe == right_safe
				&& group_expr_equivalent(left, right)
		}
		(TypedExprKind::CurrentField(left), TypedExprKind::CurrentField(right)) => left == right,
		(
			TypedExprKind::Destructure {
				base: left,
				fields: left_fields,
			},
			TypedExprKind::Destructure {
				base: right,
				fields: right_fields,
			},
		) => left_fields == right_fields && group_expr_equivalent(left, right),
		(
			TypedExprKind::Index {
				base: left_base,
				index: left_index,
			},
			TypedExprKind::Index {
				base: right_base,
				index: right_index,
			},
		) => {
			group_expr_equivalent(left_base, right_base)
				&& group_expr_equivalent(left_index, right_index)
		}
		(
			TypedExprKind::Builtin {
				function: left_function,
				arguments: left_arguments,
			},
			TypedExprKind::Builtin {
				function: right_function,
				arguments: right_arguments,
			},
		) => {
			left_function == right_function
				&& left_arguments.len() == right_arguments.len()
				&& left_arguments
					.iter()
					.zip(right_arguments)
					.all(|(left, right)| group_expr_equivalent(left, right))
		}
		(
			TypedExprKind::Cast {
				target: left_target,
				expr: left,
			},
			TypedExprKind::Cast {
				target: right_target,
				expr: right,
			},
		) => left_target == right_target && group_expr_equivalent(left, right),
		_ => false,
	}
}

fn optional_group_expr_equivalent(left: Option<&TypedExpr>, right: Option<&TypedExpr>) -> bool {
	match (left, right) {
		(Some(left), Some(right)) => group_expr_equivalent(left, right),
		(None, None) => true,
		(Some(_), None) | (None, Some(_)) => false,
	}
}

const fn class_kind(kind: ClassKindName) -> AtomKind {
	match kind {
		ClassKindName::Node => AtomKind::Node,
		ClassKindName::Edge => AtomKind::Edge,
	}
}

const fn class_schema_mode(mode: ClassSchemaModeName) -> ClassSchemaMode {
	match mode {
		ClassSchemaModeName::Schemaless => ClassSchemaMode::Schemaless,
		ClassSchemaModeName::Schemafull => ClassSchemaMode::Schemafull,
	}
}

const fn is_number(ty: &Type) -> bool {
	matches!(ty, Type::Any | Type::Number | Type::Int | Type::Float | Type::Decimal)
}

const fn is_integer(ty: &Type) -> bool {
	matches!(ty, Type::Any | Type::Number | Type::Int)
}

pub(super) fn compatible(expected: &Type, actual: &Type) -> bool {
	expected == actual
		|| matches!(expected, Type::Any)
		|| matches!(expected, Type::Number) && is_number(actual)
		|| matches!(expected, Type::Option(inner) if matches!(actual, Type::None | Type::Null) || compatible(inner, actual))
		|| matches!(expected, Type::Union(types) if types.iter().any(|ty| compatible(ty, actual)))
}

fn unify(left: Type, right: Type) -> Type {
	if left == right {
		return left;
	}
	if matches!(left, Type::Any) || matches!(right, Type::Any) {
		return Type::Any;
	}
	if is_number(&left) && is_number(&right) {
		return Type::Number;
	}
	if matches!(left, Type::None | Type::Null) {
		return Type::Option(Box::new(right));
	}
	if matches!(right, Type::None | Type::Null) {
		return Type::Option(Box::new(left));
	}
	let mut values = match left {
		Type::Union(values) => values,
		value => vec![value],
	};
	match right {
		Type::Union(right) => values.extend(right),
		value => values.push(value),
	}
	values.sort();
	values.dedup();
	Type::Union(values)
}
