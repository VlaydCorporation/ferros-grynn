use std::collections::BTreeMap;

use super::ir::{
	Column,
	EndpointDomain,
	Expand,
	LogicalDmlData,
	LogicalDmlKind,
	LogicalDmlReturn,
	LogicalDmlReturnItem,
	LogicalDmlSelector,
	LogicalDmlStatement,
	LogicalDmlTarget,
	LogicalExpect,
	LogicalGraphDmlStatement,
	LogicalMatch,
	LogicalMatchPattern,
	LogicalMatchSelector,
	LogicalNode,
	LogicalNodeKind,
	LogicalProgram,
	LogicalRecordTarget,
	LogicalRelateEdge,
	LogicalSchemaStatement,
	LogicalSelect,
	LogicalStatement,
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
		SelectItem,
		SelectTarget,
		StatementKind,
		TypeName,
		UnaryOperator,
	},
	values::{
		AtomKind,
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

struct Analyzer<'a> {
	source: &'a str,
	catalog: &'a dyn QueryCatalog,
	slots: BTreeMap<Box<str>, Slot>,
	initial_slots: Vec<(Box<str>, u32)>,
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
	fn new(source: &'a str, catalog: &'a dyn QueryCatalog, variables: &Object) -> Self {
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
		}
	}

	fn program(mut self, program: &Program) -> FerrosGrynnResult<LogicalProgram> {
		let mut statements = Vec::with_capacity(program.statements.len());
		for statement in &program.statements {
			match &statement.kind {
				StatementKind::Attach(statement) => {
					if self.catalog.class_kind(statement.record.class().value.as_ref())?
						!= AtomKind::Node
					{
						return Err(self.semantic(
							"ATTACH requires a NODE class; edge occurrences are created by RELATE.",
							statement.record.class().span,
						));
					}
					statements.push(LogicalStatement::GraphDml(LogicalGraphDmlStatement::Attach {
						record: self.record_target(&statement.record)?,
						graph: self.catalog.graph(&statement.graph.value)?,
						label: statement
							.label
							.as_ref()
							.map(|label| Text::from(label.value.as_ref())),
					}));
				}
				StatementKind::Create(statement) => {
					statements.push(LogicalStatement::Dml(self.dml(
						LogicalDmlKind::Create,
						&statement.targets,
						None,
						None,
						statement.data.as_ref(),
						None,
						statement.returning.as_ref(),
						statement.expect.as_ref(),
						false,
					)?));
				}
				StatementKind::Update(statement) => {
					statements.push(LogicalStatement::Dml(self.dml(
						LogicalDmlKind::Update,
						&statement.targets,
						statement.graph.as_ref(),
						statement.label.as_ref(),
						statement.data.as_ref(),
						statement.filter.as_ref(),
						statement.returning.as_ref(),
						statement.expect.as_ref(),
						false,
					)?));
				}
				StatementKind::Upsert(statement) => {
					statements.push(LogicalStatement::Dml(self.dml(
						LogicalDmlKind::Upsert,
						&statement.targets,
						None,
						None,
						statement.data.as_ref(),
						statement.filter.as_ref(),
						statement.returning.as_ref(),
						statement.expect.as_ref(),
						false,
					)?));
				}
				StatementKind::Delete(statement) => {
					statements.push(LogicalStatement::Dml(self.dml(
						LogicalDmlKind::Delete,
						&statement.targets,
						None,
						None,
						None,
						statement.filter.as_ref(),
						statement.returning.as_ref(),
						statement.expect.as_ref(),
						statement.cascade,
					)?));
				}
				StatementKind::Detach(statement) => {
					statements.push(LogicalStatement::GraphDml(LogicalGraphDmlStatement::Detach {
						record: self.record_target(&statement.record)?,
						graph: self.catalog.graph(&statement.graph.value)?,
						cascade: statement.cascade,
					}));
				}
				StatementKind::DefineGraph(definition) => {
					statements.push(LogicalStatement::Schema(
						self.define_graph(definition, statement.span)?,
					));
				}
				StatementKind::DefineClass(definition) => {
					statements.push(LogicalStatement::Schema(Self::define_class(definition)));
				}
				StatementKind::DefineField(definition) => {
					statements.push(LogicalStatement::Schema(self.define_field(definition)?));
				}
				StatementKind::DefineIndex(definition) => {
					statements.push(LogicalStatement::Schema(self.define_index(definition)?));
				}
				StatementKind::Remove(statement) => {
					statements.push(LogicalStatement::Schema(Self::remove(&statement.target)));
				}
				StatementKind::Alter(statement) => {
					statements.push(LogicalStatement::Schema(self.alter(&statement.target)?));
				}
				StatementKind::Info(statement) => {
					statements.push(LogicalStatement::Schema(Self::info(&statement.target)));
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
					let slot = self.bind(&statement.name.value, value.ty.clone());
					statements.push(LogicalStatement::Let {
						slot,
						value,
					});
				}
				StatementKind::Return(statement) => {
					statements.push(LogicalStatement::Return {
						value: self.expr(&statement.value, None)?,
					});
				}
				StatementKind::Relate(statement) => {
					statements.push(LogicalStatement::GraphDml(self.relate(statement)?));
				}
				StatementKind::Match(statement) => {
					statements.push(LogicalStatement::Match(self.match_statement(statement)?));
				}
				StatementKind::Select(select) => {
					statements.push(LogicalStatement::Select(self.select(select, statement.span)?));
				}
			}
		}
		Ok(LogicalProgram {
			statements,
			initial_slots: self.initial_slots,
			slot_count: self.slots.len(),
		})
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
			graph: self.catalog.graph(&statement.graph.value)?,
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
		let target = self.scan_target(&statement.target)?;
		let scan_schema = self.catalog.schema(target.class())?;
		let mut plan = LogicalSelect {
			nodes: Vec::new(),
			root: super::ir::NodeId(0),
			graph: statement
				.graph
				.as_ref()
				.map(|graph| self.catalog.graph(&graph.value))
				.transpose()?,
			value: statement.value,
			only: statement.only,
		};
		let mut root = plan.push(LogicalNode {
			kind: LogicalNodeKind::Scan {
				target,
			},
			schema: scan_schema.clone(),
			span,
		});
		if let Some(filter) = &statement.filter {
			let predicate = self.expr(filter, Some(&scan_schema))?;
			root = plan.push(LogicalNode {
				kind: LogicalNodeKind::Filter {
					input: root,
					predicate,
				},
				schema: scan_schema.clone(),
				span: filter.span,
			});
		}

		let mut expressions = Vec::new();
		let mut columns = Vec::new();
		for (index, item) in statement.projection.iter().enumerate() {
			match item {
				SelectItem::Wildcard(item_span) => {
					if statement.value {
						return Err(self.semantic("SELECT VALUE does not accept `*`.", *item_span));
					}
					for (column_index, column) in scan_schema.columns.iter().enumerate() {
						expressions.push(TypedExpr {
							kind: TypedExprKind::Column(column_index),
							ty: column.ty.clone(),
							span: *item_span,
						});
						columns.push(column.clone());
					}
				}
				SelectItem::Expr {
					expr,
					alias,
				} => {
					let typed = self.expr(expr, Some(&scan_schema))?;
					let name: Box<str> = alias
						.as_ref()
						.map(|alias| alias.value.clone())
						.or_else(|| match &expr.kind {
							ExprKind::Name(name) => Some(name.value.clone()),
							_ => None,
						})
						.unwrap_or_else(|| format!("value_{index}").into_boxed_str());
					columns.push(Column {
						name,
						ty: typed.ty.clone(),
					});
					expressions.push(typed);
				}
			}
		}
		let project_schema = Schema {
			columns,
		};
		root = plan.push(LogicalNode {
			kind: LogicalNodeKind::Project {
				input: root,
				expressions,
			},
			schema: project_schema.clone(),
			span,
		});
		if statement.limit.is_some() || statement.start.is_some() {
			let limit = statement.limit.as_ref().map(|expr| self.expr(expr, None)).transpose()?;
			let start = statement.start.as_ref().map(|expr| self.expr(expr, None)).transpose()?;
			for expression in limit.iter().chain(start.iter()) {
				if !is_integer(&expression.ty) {
					return Err(
						self.semantic("LIMIT and START require an integer.", expression.span)
					);
				}
			}
			root = plan.push(LogicalNode {
				kind: LogicalNodeKind::Limit {
					input: root,
					limit,
					start,
				},
				schema: project_schema,
				span,
			});
		}
		plan.root = root;
		Ok(plan)
	}

	fn expr(&self, expr: &Expr, schema: Option<&Schema>) -> FerrosGrynnResult<TypedExpr> {
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
					return Err(
						self.semantic(&format!("Unknown field `{}`.", name.value), name.span)
					);
				};
				let index = schema.index_of(&name.value).ok_or_else(|| {
					self.semantic(&format!("Unknown field `{}`.", name.value), name.span)
				})?;
				(TypedExprKind::Column(index), schema.columns[index].ty.clone())
			}
			ExprKind::Array(values) => {
				let values = values
					.iter()
					.map(|value| self.expr(value, schema))
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let item_type =
					values.iter().map(|value| value.ty.clone()).reduce(unify).unwrap_or(Type::Any);
				(TypedExprKind::Array(values), Type::Array(Box::new(item_type)))
			}
			ExprKind::Object(fields) => {
				let fields = fields
					.iter()
					.map(|field| {
						self.expr(&field.value, schema)
							.map(|value| (field.name.value.clone(), value))
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				(TypedExprKind::Object(fields), Type::Object)
			}
			ExprKind::Unary {
				op,
				expr: inner,
			} => {
				let inner = self.expr(inner, schema)?;
				let ty = match op {
					UnaryOperator::Not => Type::Bool,
					UnaryOperator::Assert => inner.ty.clone(),
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
				let left = self.expr(left, schema)?;
				let right = self.expr(right, schema)?;
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
				let condition = self.expr(condition, schema)?;
				let then_value = self.expr(then_value, schema)?;
				let else_value = self.expr(else_value, schema)?;
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
			ExprKind::Field {
				base,
				field,
				null_safe,
			} => {
				let base = self.expr(base, schema)?;
				(
					TypedExprKind::Field {
						base: Box::new(base),
						field: field.value.clone(),
						null_safe: *null_safe,
					},
					Type::Any,
				)
			}
			ExprKind::Index {
				base,
				index,
			} => {
				let base = self.expr(base, schema)?;
				let ty = match &base.ty {
					Type::Array(item) => (**item).clone(),
					_ => Type::Any,
				};
				(
					TypedExprKind::Index {
						base: Box::new(base),
						index: Box::new(self.expr(index, schema)?),
					},
					ty,
				)
			}
			ExprKind::Call {
				..
			} => return Err(self.unsupported("function calls", expr.span)),
			ExprKind::Cast {
				target,
				expr: inner,
			} => {
				let target = self.type_name(target)?;
				(
					TypedExprKind::Cast {
						target: target.clone(),
						expr: Box::new(self.expr(inner, schema)?),
					},
					target,
				)
			}
		};
		Ok(TypedExpr {
			kind,
			ty,
			span: expr.span,
		})
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
				if is_number(left) && is_number(right) {
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
			"object" => Type::Object,
			"record" => Type::Record,
			_ => return Err(self.semantic(&format!("Unknown type `{}`.", name.value), name.span)),
		};
		Ok(ty)
	}

	fn bind(&mut self, name: &str, ty: Type) -> u32 {
		if let Some(slot) = self.slots.get_mut(name) {
			slot.ty = ty;
			return slot.id;
		}
		let id = self.slots.len() as u32;
		self.slots.insert(name.into(), Slot {
			id,
			ty,
		});
		id
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

fn collect_expr_names(expr: &Expr, names: &mut BTreeMap<Box<str>, TextSpan>) {
	match &expr.kind {
		ExprKind::Name(name) => {
			names.entry(name.value.clone()).or_insert(name.span);
		}
		ExprKind::Array(values) => {
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
		ExprKind::Field {
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
		} => {
			collect_expr_names(callee, names);
			for argument in arguments {
				collect_expr_names(argument, names);
			}
		}
		ExprKind::Literal(_) | ExprKind::Parameter(_) => {}
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
