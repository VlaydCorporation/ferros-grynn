use std::{
	collections::{BTreeMap, BTreeSet},
	num::NonZeroU16,
	sync::Arc,
};

use super::{
	closure::LanguageClosureState,
	ir::{
		Aggregate,
		Builtin,
		ClosureEffect,
		Column,
		ColumnOrigin,
		EndpointDomain,
		Expand,
		LogicalBlock,
		LogicalBlockStatement,
		LogicalBuildGraph,
		LogicalBuildGraphSource,
		LogicalClosure,
		LogicalClosureBody,
		LogicalDmlData,
		LogicalDmlKind,
		LogicalDmlReturn,
		LogicalDmlReturnItem,
		LogicalDmlSelector,
		LogicalDmlStatement,
		LogicalDmlTarget,
		LogicalDuration,
		LogicalExpect,
		LogicalExplain,
		LogicalFulltextDefinition,
		LogicalGraphContract,
		LogicalGraphDmlStatement,
		LogicalGraphPropertyMutation,
		LogicalGraphReference,
		LogicalHyperPattern,
		LogicalIndexHint,
		LogicalKnnDistance,
		LogicalLetValue,
		LogicalLinearPattern,
		LogicalMatch,
		LogicalMatchContains,
		LogicalMatchPattern,
		LogicalMatchPatternKind,
		LogicalMatchSelector,
		LogicalOrder,
		LogicalProgram,
		LogicalRecordTarget,
		LogicalRelateEdge,
		LogicalRelateEndpoint,
		LogicalRepeat,
		LogicalReturnGraph,
		LogicalSchemaStatement,
		LogicalSelect,
		LogicalSelectAccess,
		LogicalSelectItem,
		LogicalSelectJoin,
		LogicalSelectSource,
		LogicalSelectSourceKind,
		LogicalSelectStart,
		LogicalStatement,
		LogicalTraverse,
		LogicalTraverseSource,
		LogicalTraverseStep,
		LogicalVectorDefinition,
		LogicalWildcardField,
		LogicalWithinPattern,
		MAX_MATCH_PATH_DEPTH,
		MatchBinding,
		MatchOp,
		NamedFunctionKey,
		PathExpand,
		ScanTarget,
		Schema,
		TraverseStepKind,
		TypedExpr,
		TypedExprKind,
		validate_match_max_depth,
	},
};
use crate::{
	datastore::{
		AnalyzerFilter,
		AnalyzerLanguage,
		AnalyzerTokenizer,
		ClassIndexKind,
		ClassIndexOptions,
		ClassIndexState,
		ClassSchemaMode,
		DiskAnnIndexOptions,
		FulltextIndexOptions,
		FunctionEffect,
		FunctionId,
		HnswIndexOptions,
		IndexMaintenanceMode,
		IndexScope,
		LEVEL_PATH_PROPERTY,
		SchemaAnalyzer,
		SchemaFunction,
		SchemaFunctionParameter,
		VectorDistance,
	},
	gql_status::{ExecutionPhase, FerrosGrynnError, FerrosGrynnResult, Location, TextSpan},
	graph::{GraphTopologyConstraint, GraphTopologyConstraints},
	syn::{
		AlterTarget,
		AnalyzerFilterName,
		AnalyzerTokenizerName,
		BasicIndexDefinition,
		BinaryOperator,
		BuildGraphSource,
		BuildGraphStatement,
		ClassKindName,
		ClassSchemaModeName,
		DefineAnalyzerStatement,
		DefineClassStatement,
		DefineFieldStatement,
		DefineFunctionStatement,
		DefineGraphStatement,
		DefineIndexStatement,
		DefineParameterStatement,
		DmlData,
		DmlReturn,
		DmlReturnItem,
		DmlTarget,
		ExpectClause,
		Expr,
		ExprKind,
		FulltextOperator,
		FunctionEffectName,
		FunctionSignatureName,
		GraphContract,
		GraphFamilyName,
		GraphPropertyMutation,
		GraphValidatorName,
		Identifier,
		IndexScopeName,
		InfoTarget,
		KnnDistance,
		KnnSearchArgument,
		LetValue,
		Literal,
		MatchEndpoint,
		MatchEndpointKind,
		MatchHyperPattern,
		MatchLinearPattern,
		MatchMetaScope,
		MatchPathMode,
		MatchPattern,
		MatchSelector,
		MatchStatement,
		MatchWithinPattern,
		ParameterCommentChange,
		Program,
		RecordKey,
		RemoveTarget,
		ReturnGraph,
		SelectAccessHint,
		SelectGroup,
		SelectItem,
		SelectSource,
		SelectSourceKind,
		SelectStart,
		SelectTarget,
		StatementKind,
		TraverseSource,
		TraverseStatement,
		TypeName,
		UnaryOperator,
		UpdateGraphStatement,
		VectorDistanceName,
		VectorElementName,
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
		VectorKind,
	},
};

/// Maximum finite path depth accepted by the MVP analyzer.
const MAX_MATCH_WITHIN_DEPTH: u32 = 7;

pub(super) trait QueryCatalog {
	/// Bounded SDK analysis admits private relation metadata before copying it.
	///
	/// # Errors
	/// Propagates the embedding's metadata memory and work limits.
	fn admit_analysis(&self, _bytes: usize, _work: u64) -> FerrosGrynnResult<()> {
		Ok(())
	}
	/// Returns the lexical type retained by a preceding successful declaration.
	fn lexical_binding_type(&self, _name: &str) -> Option<&Type> {
		None
	}
	/// Reactive provenance belongs to a lexical declaration, never to UUID
	/// shape.
	fn lexical_binding_is_reactive(&self, _name: &str) -> bool {
		false
	}
	fn schema_epoch(&self) -> u64;
	fn schema(&self, class: &str) -> FerrosGrynnResult<Schema>;
	fn class_definition(
		&self,
		class: &str,
	) -> FerrosGrynnResult<Arc<crate::datastore::ClassSchema>>;
	fn schema_mode(&self, class: &str) -> FerrosGrynnResult<ClassSchemaMode>;
	fn class_kind(&self, class: &str) -> FerrosGrynnResult<AtomKind>;
	/// Resolves a case-sensitive named graph.
	fn graph(&self, name: &str) -> FerrosGrynnResult<GraphRef>;
	/// Resolves an optional named graph without turning absence into an error.
	fn graph_if_exists(&self, name: &str) -> FerrosGrynnResult<Option<GraphRef>>;
	fn functions(
		&self,
		namespace: &str,
		name: &str,
	) -> FerrosGrynnResult<Vec<Arc<crate::datastore::SchemaFunction>>>;
	fn function(
		&self,
		id: FunctionId,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaFunction>>>;
	fn analyzer(
		&self,
		name: &str,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaAnalyzer>>>;
	fn analyzer_by_id(
		&self,
		id: crate::datastore::AnalyzerId,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaAnalyzer>>>;
}

pub(super) fn analyze(
	program: &Program,
	source: &str,
	catalog: &dyn QueryCatalog,
	variables: &Object,
) -> FerrosGrynnResult<LogicalProgram> {
	Analyzer::new(source, catalog, variables, &BTreeSet::new()).program(program)
}

/// Reuses normal program analysis with an embedding's bounded function cache.
pub(super) fn analyze_with_runtime(
	program: &Program,
	source: &str,
	catalog: &dyn QueryCatalog,
	variables: &Object,
	function_cache: &LanguageClosureState,
) -> FerrosGrynnResult<LogicalProgram> {
	Analyzer::new(source, catalog, variables, &BTreeSet::new())
		.with_function_cache(function_cache)
		.program(program)
}

/// Types a reactive scalar expression with ordinary SELECT subquery semantics.
pub(super) fn analyze_reactive_expression(
	expression: &Expr,
	catalog: &dyn QueryCatalog,
	variables: &Object,
	function_cache: &LanguageClosureState,
) -> FerrosGrynnResult<LogicalProgram> {
	let analyzer =
		Analyzer::new("", catalog, variables, &BTreeSet::new()).with_function_cache(function_cache);
	let value = analyzer.expr_context(expression, None, false, true, 0)?;
	if typed_expr_contains_aggregate(&value) {
		return Err(FerrosGrynnError::invalid_argument(
			"LIVE LET aggregates require a SELECT subquery",
		));
	}
	Ok(LogicalProgram {
		statements: vec![LogicalStatement::Expression {
			value,
		}],
		initial_slots: analyzer.initial_slots,
		slot_count: analyzer.next_slot as usize,
	})
}

pub(super) fn analyze_statement(
	statement: &crate::syn::Statement,
	source: &str,
	catalog: &dyn QueryCatalog,
	variables: &Object,
) -> FerrosGrynnResult<LogicalProgram> {
	Analyzer::new(source, catalog, variables, &BTreeSet::new()).single_statement(statement)
}

pub(super) fn analyze_statement_with_mutable(
	statement: &crate::syn::Statement,
	source: &str,
	catalog: &dyn QueryCatalog,
	variables: &Object,
	mutable_variables: &BTreeSet<Box<str>>,
) -> FerrosGrynnResult<LogicalProgram> {
	Analyzer::new(source, catalog, variables, mutable_variables).single_statement(statement)
}

pub(super) fn analyze_statement_with_runtime(
	statement: &crate::syn::Statement,
	source: &str,
	catalog: &dyn QueryCatalog,
	variables: &Object,
	mutable_variables: &BTreeSet<Box<str>>,
	control: &super::options::ExecutionControl,
) -> FerrosGrynnResult<(LogicalProgram, Option<LogicalDuration>)> {
	let mut analyzer = Analyzer::new(source, catalog, variables, mutable_variables)
		.with_function_cache(control.function_cache());
	analyzer.live_checks = Some(control.interruption());
	analyzer.live_scratch = analyzer.live_scratch.min(control.barrier_memory_limit());
	let timeout = statement
		.timeout
		.as_ref()
		.map(|duration| analyzer.duration_input(duration, "TIMEOUT"))
		.transpose()?;
	Ok((analyzer.single_statement(statement)?, timeout))
}

/// Types import item deadlines before loop-local declarations alter the frame.
/// The caller admits the sparse timeout vector before entering analysis.
pub(super) fn analyze_import_with_runtime(
	program: &Program,
	source: &str,
	catalog: &dyn QueryCatalog,
	variables: &Object,
	control: &super::options::ExecutionControl,
) -> FerrosGrynnResult<(LogicalProgram, Vec<(usize, LogicalDuration)>)> {
	let mut analyzer = Analyzer::new(source, catalog, variables, &BTreeSet::new())
		.with_function_cache(control.function_cache());
	analyzer.live_checks = Some(control.interruption());
	analyzer.live_scratch = analyzer.live_scratch.min(control.barrier_memory_limit());
	let count = program.statements.iter().filter(|statement| statement.timeout.is_some()).count();
	let mut timeouts = Vec::with_capacity(count);
	for (index, statement) in program.statements.iter().enumerate() {
		control.check()?;
		if let Some(timeout) = &statement.timeout {
			timeouts.push((index, analyzer.duration_input(timeout, "TIMEOUT")?));
		}
	}
	Ok((analyzer.program(program)?, timeouts))
}

struct Analyzer<'a> {
	/// Optional registration-option slot usage; new closure locals lie outside
	/// this mask.
	slot_reads: Option<std::rc::Rc<std::cell::RefCell<Vec<bool>>>>,
	source: &'a str,
	catalog: &'a dyn QueryCatalog,
	slots: BTreeMap<Box<str>, Slot>,
	initial_slots: Vec<(Box<str>, u32, bool)>,
	next_slot: u32,
	scopes: Vec<Vec<(Box<str>, Option<Slot>)>>,
	variables: &'a Object,
	statement_now: DateTime,
	function_stack: Vec<FunctionId>,
	function_overrides: BTreeMap<FunctionId, Arc<SchemaFunction>>,
	function_cache: Option<&'a LanguageClosureState>,
	/// LIVE preflight uses a private compiler without retaining session checks.
	live_checks: Option<super::options::ExecutionChecks>,
	/// Registration analysis respects both the LIVE and caller scratch caps.
	live_scratch: usize,
	current_function: Option<Arc<SchemaFunction>>,
	current_function_has_explicit_result: bool,
}

/// Source-preserving event definition and lifecycle lowering.
mod event;
/// Bounded LIVE syntax recipes share literal/source/type analysis.
mod live;

#[derive(Debug, Clone)]
struct Slot {
	id: u32,
	ty: Type,
	mutable: bool,
	/// The frame holds an identity whose value is resolved in the read
	/// snapshot.
	reactive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Semantic category assigned to a MATCH alias.
enum MatchAliasKind {
	/// An atom endpoint with a fixed domain.
	Endpoint(EndpointDomain),
	/// A public path value.
	Path,
}

const fn endpoint_domain(kind: MatchEndpointKind) -> EndpointDomain {
	match kind {
		MatchEndpointKind::Node => EndpointDomain::Node,
		MatchEndpointKind::Edge => EndpointDomain::Edge,
	}
}

const fn match_meta_required(scope: MatchMetaScope, domain: EndpointDomain) -> bool {
	match scope {
		MatchMetaScope::None => false,
		MatchMetaScope::Meta => true,
		MatchMetaScope::MetaVertex => matches!(domain, EndpointDomain::Node),
		MatchMetaScope::MetaEdge => matches!(domain, EndpointDomain::Edge),
	}
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
	fn new(
		source: &'a str,
		catalog: &'a dyn QueryCatalog,
		variables: &'a Object,
		mutable_variables: &BTreeSet<Box<str>>,
	) -> Self {
		let mut slots = BTreeMap::new();
		let mut initial_slots = Vec::new();
		for (index, (name, value)) in variables.iter().enumerate() {
			let id = index as u32;
			let reactive = catalog.lexical_binding_is_reactive(name.as_str());
			slots.insert(name.as_str().into(), Slot {
				id,
				ty: catalog
					.lexical_binding_type(name.as_str())
					.cloned()
					.unwrap_or_else(|| value.value_type()),
				mutable: !reactive && mutable_variables.contains(name.as_str()),
				reactive,
			});
			initial_slots.push((
				name.as_str().into(),
				id,
				!reactive && mutable_variables.contains(name.as_str()),
			));
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
			function_stack: Vec::new(),
			function_overrides: BTreeMap::new(),
			function_cache: None,
			slot_reads: None,
			live_checks: None,
			live_scratch: crate::live::LiveBindingOptions::default().scratch_bytes(),
			current_function: None,
			current_function_has_explicit_result: true,
		}
	}

	fn with_function_cache(mut self, function_cache: &'a LanguageClosureState) -> Self {
		self.function_cache = Some(function_cache);
		self
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
			StatementKind::Explain(explain) => LogicalStatement::Explain(LogicalExplain {
				mode: explain.mode,
				format: explain.format,
				statement: Box::new(self.statement(&explain.statement, loop_depth)?),
			}),
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
					keep_levelpath: keep_levelpath_enabled(
						statement.keep_levelpath.as_ref(),
						None,
						|message, span| self.semantic(message, span),
					)?,
				})
			}
			StatementKind::Create(statement) => LogicalStatement::Dml(self.dml(
				LogicalDmlKind::Create,
				&statement.targets,
				None,
				None,
				statement.data.as_ref(),
				None,
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
				statement.keep_levelpath.as_ref(),
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
				None,
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
				None,
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
				LogicalStatement::Schema(self.define_class(definition)?)
			}
			StatementKind::DefineField(definition) => {
				LogicalStatement::Schema(self.define_field(definition)?)
			}
			StatementKind::DefineIndex(definition) => {
				LogicalStatement::Schema(self.define_index(definition)?)
			}
			StatementKind::DefineGraphIndex(definition) => {
				LogicalStatement::Schema(self.define_graph_index(definition)?)
			}
			StatementKind::DefineAnalyzer(definition) => {
				LogicalStatement::Schema(self.define_analyzer(definition)?)
			}
			StatementKind::DefineParameter(definition) => {
				LogicalStatement::Schema(self.define_parameter(definition)?)
			}
			StatementKind::DefineFunction(definition) => {
				LogicalStatement::Schema(self.define_function(definition)?)
			}
			StatementKind::DefineEvent(definition) => {
				LogicalStatement::Schema(self.define_event(definition)?)
			}
			StatementKind::Remove(statement) => {
				LogicalStatement::Schema(self.remove(&statement.target)?)
			}
			StatementKind::Alter(statement) => {
				if let AlterTarget::Let {
					name,
					value,
				} = &statement.target
				{
					let binding = self.slots.get(&name.value).ok_or_else(|| {
						self.semantic("ALTER LET requires an existing lexical binding.", name.span)
					})?;
					if !binding.mutable {
						return Err(self.semantic(
							"ALTER LET requires a mutable lexical binding; execute parameters and \
							 captures are immutable.",
							name.span,
						));
					}
					let value = self.expr(value, None)?;
					if !assignment_compatible(&binding.ty, &value.ty) {
						return Err(self.semantic(
							&format!(
								"ALTER LET value has type {}, expected {}.",
								value.ty, binding.ty
							),
							value.span,
						));
					}
					LogicalStatement::SetBinding {
						slot: binding.id,
						expected: binding.ty.clone(),
						value,
					}
				} else {
					LogicalStatement::Schema(self.alter(&statement.target)?)
				}
			}
			StatementKind::Info(statement) => {
				LogicalStatement::Schema(self.info(&statement.target)?)
			}
			StatementKind::ShowChanges(statement) => {
				LogicalStatement::Schema(self.show_changes(statement)?)
			}
			StatementKind::Kill(kill) => {
				LogicalStatement::Schema(LogicalSchemaStatement::CloseLive {
					id: self.expr(
						&Expr {
							kind: ExprKind::Parameter(kill.job.clone()),
							span: kill.job.span,
						},
						None,
					)?,
				})
			}
			StatementKind::AckLive(ack) => {
				LogicalStatement::Schema(LogicalSchemaStatement::AckLive {
					spelling: self.changefeed_spelling([ack.query.span, ack.cursor.span])?,
					id: self.expr(
						&Expr {
							kind: ExprKind::Parameter(ack.query.clone()),
							span: ack.query.span,
						},
						None,
					)?,
					cursor: {
						let cursor = self.expr(&ack.cursor, None)?;
						if typed_expression_effect(&cursor) > ClosureEffect::StableRead {
							return Err(self.semantic(
								"ACK cursor expression must be read-only.",
								ack.cursor.span,
							));
						}
						cursor
					},
				})
			}
			StatementKind::Maintenance(_) => {
				return Err(self.semantic(
					"Maintenance statements must be executed through Datastore or GqlSession.",
					statement.span,
				));
			}
			StatementKind::Import(_) => {
				return Err(self
					.semantic("BEGIN IMPORT must be executed through Datastore.", statement.span));
			}
			StatementKind::BuildGraph(build) => {
				LogicalStatement::BuildGraph(self.build_graph(build, statement.span)?)
			}
			StatementKind::LiveLet(binding) => {
				let plan = self.live_binding(&binding.value)?;
				let slot = self.declare(&binding.name, Type::Any)?;
				if let Some(declaration) = self.slots.get_mut(&binding.name.value) {
					declaration.reactive = true;
				}
				LogicalStatement::Let {
					name: binding.name.value.clone(),
					slot,
					expected: Type::Any,
					value: LogicalLetValue::Live(Box::new(
						LogicalSchemaStatement::DefineLiveBinding(Box::new(plan)),
					)),
				}
			}
			StatementKind::Let(binding) => {
				let (value, value_type) = match &binding.value {
					LetValue::LiveSelect(select) => (
						LogicalLetValue::Live(Box::new(LogicalSchemaStatement::RegisterLive(
							Box::new(self.live_select(select, statement.span)?),
						))),
						Type::Uuid,
					),
					LetValue::Expression(expression) => {
						let expression = self.expr(expression, None)?;
						let value_type = expression.ty.clone();
						(LogicalLetValue::Expression(expression), value_type)
					}
					LetValue::BuildGraph(build) => {
						let build = self.build_graph(build, statement.span)?;
						let family = build.contract.family.unwrap_or(if build.name.is_some() {
							GraphKind::Graph
						} else {
							GraphKind::Subgraph
						});
						let value_type = if build.name.is_some() {
							Type::GraphRef(family)
						} else {
							Type::Graph(family)
						};
						(LogicalLetValue::BuildGraph(build), value_type)
					}
				};
				let expected = if let Some(declared) = &binding.declared_type {
					let declared = self.type_name(declared)?;
					if !assignment_compatible(&declared, &value_type) {
						return Err(self.semantic(
							&format!("LET value has type {value_type}, expected {declared}."),
							statement.span,
						));
					}
					declared
				} else {
					value_type
				};
				let slot = self.declare_mutable(&binding.name, expected.clone())?;
				LogicalStatement::Let {
					name: binding.name.value.clone(),
					slot,
					expected,
					value,
				}
			}
			StatementKind::Return(statement) => LogicalStatement::Return {
				value: self.expr(&statement.value, None)?,
			},
			StatementKind::ReturnGraph(graph) => {
				LogicalStatement::ReturnGraph(self.return_graph(graph, None, statement.span)?)
			}
			StatementKind::Relate(statement) => LogicalStatement::GraphDml(self.relate(statement)?),
			StatementKind::Match(statement) => {
				LogicalStatement::Match(self.match_statement(statement)?)
			}
			StatementKind::Select(select) => {
				LogicalStatement::Select(self.select(select, statement.span)?)
			}
			StatementKind::LiveSelect(select) => {
				LogicalStatement::Schema(LogicalSchemaStatement::RegisterLive(Box::new(
					self.live_select(select, statement.span)?,
				)))
			}
			StatementKind::Traverse(traverse) => {
				LogicalStatement::Traverse(self.traverse(traverse, statement.span)?)
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
			StatementKind::Sleep(duration) => {
				LogicalStatement::Sleep(self.duration_input(duration, "SLEEP")?)
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
			let timeout = statement
				.timeout
				.as_ref()
				.map(|value| self.duration_input(value, "TIMEOUT"))
				.transpose()
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

	/// Keeps lexical and reactive duration parameters in their typed frame.
	fn duration_input(
		&self,
		value: &crate::syn::TimeoutValue,
		subject: &str,
	) -> FerrosGrynnResult<LogicalDuration> {
		match value {
			crate::syn::TimeoutValue::Literal {
				..
			} => crate::gql::options::resolve_duration(value, self.variables, subject)
				.map(LogicalDuration::Fixed),
			crate::syn::TimeoutValue::Parameter(name) => {
				let expression = self.expr(
					&Expr {
						kind: ExprKind::Parameter(name.clone()),
						span: name.span,
					},
					None,
				)?;
				if !assignment_compatible(&Type::Duration, &expression.ty) {
					return Err(self.semantic(
						&format!(
							"{subject} parameter has type {}, expected duration.",
							expression.ty
						),
						name.span,
					));
				}
				Ok(LogicalDuration::Parameter(expression))
			}
		}
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

	/// Static record classes and dynamic references share endpoint-kind
	/// validation.
	fn relate_endpoint(
		&self,
		endpoint: &crate::syn::RelateEndpoint,
		atom: bool,
	) -> FerrosGrynnResult<LogicalRelateEndpoint> {
		let expected = endpoint.kind.map(|kind| match kind {
			crate::syn::MatchEndpointKind::Node => AtomKind::Node,
			crate::syn::MatchEndpointKind::Edge => AtomKind::Edge,
		});
		match &endpoint.target {
			crate::syn::RelateEndpointTarget::Record(target) => {
				let kind = self.catalog.class_kind(target.class().value.as_ref())?;
				if !atom && kind != AtomKind::Node
					|| expected.is_some_and(|expected| expected != kind)
				{
					return Err(self.semantic(
						"RELATE endpoint class does not match its node/edge wrapper.",
						endpoint.span,
					));
				}
				self.record_target(target).map(LogicalRelateEndpoint::Record)
			}
			crate::syn::RelateEndpointTarget::Reference(value) => {
				Ok(LogicalRelateEndpoint::Reference {
					value: self.expr(value, None)?,
					kind: expected,
				})
			}
		}
	}

	fn relate(
		&self,
		statement: &crate::syn::RelateStatement,
	) -> FerrosGrynnResult<LogicalGraphDmlStatement> {
		let graph = statement.graph.as_ref().ok_or_else(|| {
			self.semantic("RELATE requires a graph scope.", statement.source.span)
		})?;
		if self.catalog.class_kind(statement.edge.class().value.as_ref())? != AtomKind::Edge {
			return Err(
				self.semantic("RELATE edge requires an EDGE class.", statement.edge.class().span)
			);
		}
		let source = self.relate_endpoint(&statement.source, statement.atom_endpoints)?;
		let target = self.relate_endpoint(&statement.target, statement.atom_endpoints)?;
		let class = statement.edge.class().value.clone();
		validate_reserved_levelpath_data(statement.data.as_ref(), |message, span| {
			self.semantic(message, span)
		})?;
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
		let keep_levelpath = keep_levelpath_enabled(
			statement.keep_levelpath.as_ref(),
			statement.alias.as_ref(),
			|message, span| self.semantic(message, span),
		)?;
		Ok(LogicalGraphDmlStatement::Relate {
			bridge: statement.bridge.as_ref().map(|clause| clause.alias.value.clone()),
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
			keep_levelpath,
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
		keep_levelpath: Option<&crate::syn::KeepLevelPathClause>,
		returning: Option<&DmlReturn>,
		expect: Option<&ExpectClause>,
		cascade: bool,
	) -> FerrosGrynnResult<LogicalDmlStatement> {
		if keep_levelpath.is_some() && graph.is_none() {
			return Err(self.semantic(
				"KEEP LEVELPATH requires graph-scoped UPDATE.",
				targets[0].class().span,
			));
		}
		let keep_levelpath = keep_levelpath_enabled(keep_levelpath, None, |message, span| {
			self.semantic(message, span)
		})?;
		validate_reserved_levelpath_data(data, |message, span| self.semantic(message, span))?;
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
			keep_levelpath,
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
		let contract = self.graph_contract(&definition.contract);
		let kind = contract.family.unwrap_or(GraphKind::Graph);
		if definition.root.is_some() && kind != GraphKind::RootedTree {
			return Err(self.semantic("ROOT is valid only for ROOTED_TREE.", span));
		}
		Ok(LogicalSchemaStatement::DefineGraph {
			mode: definition.mode,
			name: definition.name.value.clone(),
			kind,
			constraints: contract.validators,
			changefeed: definition
				.changefeed
				.as_ref()
				.map(|clause| self.changefeed_config(clause))
				.transpose()?,
			root: definition.root.as_ref().map(|root| self.expr(root, None)).transpose()?,
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

	fn graph_contract(&self, contract: &GraphContract) -> LogicalGraphContract {
		let family = contract.family.map(|family| match family {
			GraphFamilyName::Graph => GraphKind::Graph,
			GraphFamilyName::Tree => GraphKind::Tree,
			GraphFamilyName::RootedTree => GraphKind::RootedTree,
			GraphFamilyName::Forest => GraphKind::Forest,
			GraphFamilyName::Dag => GraphKind::Dag,
			GraphFamilyName::Hyper => GraphKind::Hypergraph,
			GraphFamilyName::Meta => GraphKind::Metagraph,
		});
		let mut validators = GraphTopologyConstraints::empty();
		for validator in &contract.validators {
			validators.insert(match validator {
				GraphValidatorName::Simple => GraphTopologyConstraint::Simple,
				GraphValidatorName::Bipartite => GraphTopologyConstraint::Bipartite,
				GraphValidatorName::Complete => GraphTopologyConstraint::Complete,
			});
		}
		LogicalGraphContract {
			family,
			validators,
		}
	}

	/// Admits cold SELECT storage before allocating its owned graph selectors.
	fn select_return_graph(
		&self,
		graph: &ReturnGraph,
		schema: &Schema,
		span: TextSpan,
	) -> FerrosGrynnResult<Box<LogicalReturnGraph>> {
		self.catalog.admit_analysis(std::mem::size_of::<LogicalReturnGraph>(), 0)?;
		self.return_graph(graph, Some(schema), span).map(Box::new)
	}

	fn return_graph(
		&self,
		graph: &ReturnGraph,
		schema: Option<&Schema>,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalReturnGraph> {
		let contract = graph.contract.as_ref().map_or(
			LogicalGraphContract {
				family: None,
				validators: GraphTopologyConstraints::empty(),
			},
			|contract| self.graph_contract(contract),
		);
		if graph.root.is_some() && contract.family != Some(GraphKind::RootedTree) {
			return Err(self.semantic("RETURN GRAPH ROOT requires ROOTED_TREE.", span));
		}
		Ok(LogicalReturnGraph {
			bridge: graph.bridge.as_ref().map(|clause| clause.alias.value.clone()),
			contract,
			root: graph.root.as_ref().map(|root| self.expr(root, schema)).transpose()?,
			nodes: self.expr(&graph.nodes, schema)?,
			edges: self.expr(&graph.edges, schema)?,
		})
	}

	fn build_graph(
		&self,
		build: &BuildGraphStatement,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalBuildGraph> {
		let contract = build.contract.as_ref().map_or(
			LogicalGraphContract {
				family: None,
				validators: GraphTopologyConstraints::empty(),
			},
			|contract| self.graph_contract(contract),
		);
		if build.name.is_none() && build.mode != crate::syn::DefinitionMode::Normal {
			return Err(self.semantic(
				"Anonymous BUILD GRAPH does not accept OVERWRITE or IF NOT EXISTS.",
				span,
			));
		}
		let existing = build
			.name
			.as_ref()
			.map(|name| self.catalog.graph_if_exists(&name.value))
			.transpose()?
			.flatten();
		let overwrite_rooted_tree = build.mode == crate::syn::DefinitionMode::Overwrite
			&& contract.family.is_none()
			&& existing.as_ref().is_some_and(|graph| graph.kind() == GraphKind::RootedTree);
		if build.root.is_some()
			&& contract.family != Some(GraphKind::RootedTree)
			&& !overwrite_rooted_tree
		{
			return Err(self.semantic("BUILD GRAPH ROOT requires ROOTED_TREE.", span));
		}
		let (source, schema, direct) = match &build.source {
			BuildGraphSource::Match(statement) => {
				let statement = self.match_statement(statement)?;
				let schema = statement.result.clone();
				(LogicalBuildGraphSource::Match(Box::new(statement)), Some(schema), false)
			}
			BuildGraphSource::Select(statement) => {
				let statement = self.select(statement, span)?;
				let schema = statement.output_schema();
				(LogicalBuildGraphSource::Select(Box::new(statement)), Some(schema), false)
			}
			BuildGraphSource::Traverse(statement) => {
				let statement = self.traverse(statement, span)?;
				let schema = statement.result.clone();
				(LogicalBuildGraphSource::Traverse(Box::new(statement)), Some(schema), false)
			}
			BuildGraphSource::Expression(expression) => {
				(LogicalBuildGraphSource::Expression(self.expr(expression, None)?), None, true)
			}
		};
		if direct && (build.nodes.is_some() || build.edges.is_some()) {
			return Err(
				self.semantic("Direct graph values do not accept NODES or EDGES selectors.", span)
			);
		}
		let schema = schema.as_ref();
		Ok(LogicalBuildGraph {
			bridge: build.bridge.as_ref().map(|clause| clause.alias.value.clone()),
			mode: build.mode,
			contract,
			contract_explicit: build.contract.is_some(),
			name: build.name.as_ref().map(|name| name.value.clone()),
			source,
			root: build.root.as_ref().map(|root| self.expr(root, schema)).transpose()?,
			nodes: build.nodes.as_ref().map(|nodes| self.expr(nodes, schema)).transpose()?,
			edges: build.edges.as_ref().map(|edges| self.expr(edges, schema)).transpose()?,
			limit_nodes: build.limit_nodes,
			limit_edges: build.limit_edges,
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

	/// Validates a global class and its optional fixed-duration capture policy.
	fn define_class(
		&self,
		definition: &DefineClassStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		Ok(LogicalSchemaStatement::DefineClass {
			mode: definition.mode,
			name: definition.name.value.clone(),
			kind: class_kind(definition.kind),
			schema_mode: class_schema_mode(definition.schema_mode),
			changefeed: definition
				.changefeed
				.as_ref()
				.map(|clause| self.changefeed_config(clause))
				.transpose()?,
		})
	}

	/// Validates literal fixed retention using the public runtime duration
	/// policy.
	fn changefeed_config(
		&self,
		clause: &crate::syn::ChangefeedClause,
	) -> FerrosGrynnResult<crate::changefeed::ChangefeedConfig> {
		let duration = clause
			.duration
			.parse::<crate::values::Duration>()
			.map_err(|error| self.semantic(&error.to_string(), clause.span))?;
		crate::changefeed::ChangefeedConfig::new(duration)
			.map(|config| {
				config.with_include_original(clause.include_original).with_full(clause.full)
			})
			.map_err(|error| {
				self.locate(error.with_phase(ExecutionPhase::SemanticAnalysis), clause.span)
			})
	}

	/// Lowers history expressions without requiring a currently live schema
	/// owner.
	fn show_changes(
		&self,
		statement: &crate::syn::ShowChangesStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		use crate::{
			changefeed::ChangefeedSelector,
			gql::ir::LogicalChangefeedTarget,
			syn::ShowChangesTarget,
		};
		let target = match &statement.target {
			ShowChangesTarget::Class(name) => LogicalChangefeedTarget::Named(
				ChangefeedSelector::ClassName(Text::from(name.value.as_ref())),
			),
			ShowChangesTarget::GraphName(name) => LogicalChangefeedTarget::Named(
				ChangefeedSelector::GraphName(Text::from(name.value.as_ref())),
			),
			ShowChangesTarget::Graph(value) => {
				LogicalChangefeedTarget::Graph(self.expr(value, None)?)
			}
			ShowChangesTarget::Changefeed(value) => {
				LogicalChangefeedTarget::Id(self.expr(value, None)?)
			}
		};
		let since = self.expr(&statement.since, None)?;
		if !compatible(&Type::Number, &since.ty)
			&& !matches!(since.ty, Type::DateTime | Type::Bytes)
		{
			return Err(self.semantic(
				"SINCE requires an exact integer stamp, timestamp or cursor bytes.",
				since.span,
			));
		}
		let limit = statement.limit.as_ref().map(|value| self.expr(value, None)).transpose()?;
		if let Some(limit) = &limit
			&& !compatible(&Type::Number, &limit.ty)
		{
			return Err(self.semantic("LIMIT requires a nonnegative integer.", limit.span));
		}
		let target_span = match &target {
			LogicalChangefeedTarget::Graph(value) | LogicalChangefeedTarget::Id(value) => {
				Some(value.span)
			}
			LogicalChangefeedTarget::Named(_) => None,
		};
		let spelling = self.changefeed_spelling(
			[target_span, Some(since.span), limit.as_ref().map(|value| value.span)]
				.into_iter()
				.flatten(),
		)?;
		Ok(LogicalSchemaStatement::ShowChanges {
			target,
			since,
			full: statement.full,
			limit,
			spelling,
		})
	}

	/// Owns bounded operand spelling while spans still refer to this analyzer.
	fn changefeed_spelling(
		&self,
		spans: impl IntoIterator<Item = TextSpan>,
	) -> FerrosGrynnResult<Box<str>> {
		Ok(self.operand_spelling(spans)?.into())
	}

	/// Borrows bounded analyzer-owned operand text before a caller admits its
	/// copy.
	fn operand_spelling(
		&self,
		spans: impl IntoIterator<Item = TextSpan>,
	) -> FerrosGrynnResult<&str> {
		let mut start = usize::MAX;
		let mut end = 0;
		for span in spans {
			let offset = span.offset as usize;
			start = start.min(offset);
			end =
				end.max(offset.checked_add(span.len as usize).ok_or_else(|| {
					FerrosGrynnError::operation_overflow("changefeed operand span")
				})?);
		}
		let spelling = self.source.get(start..end).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("changefeed operand span is outside its definition")
		})?;
		if spelling.len() > 1_000_000 {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed operand spelling",
				spelling.len() as u64,
				1_000_000,
			));
		}
		Ok(spelling)
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

	/// Resolves graph options without importing storage bindings into typed IR.
	fn define_graph_index(
		&self,
		definition: &crate::syn::DefineGraphIndexStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		use crate::{
			datastore::{
				GraphIndex,
				GraphIndexCost,
				GraphIndexDefinition,
				NeighbourhoodAlgorithm,
				PathIndexAlgorithm,
				ReachabilityAlgorithm,
			},
			graph::{TraversalDirection, TraversalSemantics},
			syn::GraphIndexFamilyName,
		};
		let graph = self.catalog.graph(&definition.graph.value)?;
		if definition.family == GraphIndexFamilyName::Tensor {
			if definition.algorithm.is_some()
				|| definition.projection.is_some()
				|| definition.direction.is_some()
				|| !definition.edge_labels.is_empty()
				|| definition.weighted
				|| definition.cost.is_some()
				|| !definition.patterns.is_empty()
			{
				return Err(self.semantic(
					"TENSOR stores complete ATOM incidence; use FORMAT, SCOPE and MAX_BYTES.",
					definition.name.span,
				));
			}
			let scope = match definition
				.cache_scope
				.as_ref()
				.map(|value| value.value.to_ascii_uppercase())
				.as_deref()
				.unwrap_or("LOCAL")
			{
				"LOCAL" => crate::graph::tensor::TensorScope::Local,
				"ROOTED" => crate::graph::tensor::TensorScope::Rooted,
				_ => {
					return Err(self
						.semantic("TENSOR SCOPE must be LOCAL or ROOTED.", definition.name.span));
				}
			};
			let format = match definition
				.cache_format
				.as_ref()
				.map(|value| value.value.to_ascii_uppercase())
				.as_deref()
				.unwrap_or("AUTO")
			{
				"AUTO" => crate::datastore::TensorCacheFormat::Auto,
				"CSR" => crate::datastore::TensorCacheFormat::Csr,
				"DENSE" => crate::datastore::TensorCacheFormat::Dense,
				_ => {
					return Err(self.semantic(
						"TENSOR FORMAT must be AUTO, CSR or DENSE.",
						definition.name.span,
					));
				}
			};
			let mut index = GraphIndex::new_tensor(
				definition.name.value.as_ref(),
				scope,
				format,
				definition.max_bytes.ok_or_else(|| {
					self.semantic("TENSOR requires MAX_BYTES.", definition.name.span)
				})?,
			)?
			.with_maintenance_mode(if definition.deferred {
				IndexMaintenanceMode::Deferred
			} else {
				IndexMaintenanceMode::Immediate
			});
			index.comment = definition.comment.as_ref().map(|comment| comment.as_ref().into());
			index.validate()?;
			return Ok(LogicalSchemaStatement::DefineGraphIndex {
				mode: definition.mode,
				graph: definition.graph.value.clone(),
				index,
				concurrently: definition.concurrently,
			});
		}
		if definition.cache_scope.is_some() || definition.cache_format.is_some() {
			return Err(
				self.semantic("FORMAT and SCOPE require a TENSOR index.", definition.name.span)
			);
		}
		let algorithm =
			definition.algorithm.as_ref().map(|(name, _)| name.value.to_ascii_uppercase());
		let parameter = definition.algorithm.as_ref().and_then(|(_, parameter)| *parameter);
		let narrow = |value: u32| {
			u16::try_from(value).map_err(|_error| {
				self.semantic("Graph algorithm parameter exceeds u16.", definition.name.span)
			})
		};
		let narrow_byte = |value: u32| {
			u8::try_from(value).map_err(|_error| {
				self.semantic("Graph algorithm parameter exceeds u8.", definition.name.span)
			})
		};
		if parameter.is_some()
			&& !matches!(
				algorithm.as_deref(),
				Some("GRAIL" | "FERRARI" | "BFL" | "SKETCH" | "LANDMARK" | "LANDMARK_SSSP")
			) {
			return Err(
				self.semantic("This algorithm does not accept a parameter.", definition.name.span)
			);
		}
		if definition.weighted && definition.cost.is_some() {
			return Err(
				self.semantic("WEIGHTED and COST cannot be combined.", definition.name.span)
			);
		}
		if !matches!(definition.family, GraphIndexFamilyName::Path)
			&& (definition.weighted || definition.cost.is_some() || !definition.patterns.is_empty())
		{
			return Err(
				self.semantic("Costs and PATTERNS require a PATH index.", definition.name.span)
			);
		}
		let options = match definition.family {
			GraphIndexFamilyName::Tensor => {
				return Err(
					self.semantic("TENSOR definition was not resolved.", definition.name.span)
				);
			}
			GraphIndexFamilyName::Reachability => GraphIndexDefinition::Reachability {
				algorithm: match algorithm.as_deref().unwrap_or("AUTO") {
					"AUTO" => ReachabilityAlgorithm::Auto,
					"TREE_COVER" => ReachabilityAlgorithm::TreeCover,
					"GRAIL" => ReachabilityAlgorithm::Grail(narrow_byte(parameter.unwrap_or(2))?),
					"FERRARI" => {
						ReachabilityAlgorithm::Ferrari(narrow_byte(parameter.unwrap_or(4))?)
					}
					"BFL" => ReachabilityAlgorithm::Bfl(narrow(parameter.unwrap_or(160))?),
					"CHAIN_COVER" => ReachabilityAlgorithm::ChainCover,
					"ON_DEMAND" => ReachabilityAlgorithm::OnDemand,
					_ => {
						return Err(
							self.semantic("Unknown reachability algorithm.", definition.name.span)
						);
					}
				},
			},
			GraphIndexFamilyName::Neighbourhood(radius) => GraphIndexDefinition::Neighbourhood {
				radius,
				algorithm: match algorithm.as_deref().unwrap_or("AUTO") {
					"AUTO" => NeighbourhoodAlgorithm::Auto,
					"EXACT" => NeighbourhoodAlgorithm::Exact,
					"SKETCH" => {
						NeighbourhoodAlgorithm::Sketch(narrow_byte(parameter.unwrap_or(12))?)
					}
					"LANDMARK" => {
						NeighbourhoodAlgorithm::Landmark(narrow(parameter.ok_or_else(|| {
							self.semantic("LANDMARK requires a count.", definition.name.span)
						})?)?)
					}
					"ON_DEMAND" => NeighbourhoodAlgorithm::OnDemand,
					_ => {
						return Err(
							self.semantic("Unknown neighbourhood algorithm.", definition.name.span)
						);
					}
				},
			},
			GraphIndexFamilyName::Path => {
				let algorithm = match algorithm.as_deref().unwrap_or("ON_DEMAND") {
					"LANDMARK_SSSP" => {
						PathIndexAlgorithm::LandmarkSssp(narrow(parameter.ok_or_else(|| {
							self.semantic("LANDMARK_SSSP requires a count.", definition.name.span)
						})?)?)
					}
					"PATTERN_CACHE" => PathIndexAlgorithm::PatternCache,
					"ON_DEMAND" => PathIndexAlgorithm::OnDemand,
					_ => {
						return Err(
							self.semantic("Unknown path index algorithm.", definition.name.span)
						);
					}
				};
				let cost = if let Some(function) = &definition.cost {
					if function.namespace.value.as_ref() != "fn" || function.parameters.is_some() {
						return Err(self.semantic(
							"Graph COST requires fn::name without a signature qualifier.",
							function.name.span,
						));
					}
					let mut candidates = self.catalog.functions("fn", &function.name.value)?;
					candidates.retain(|function| {
						(function.parameters().len() == 1
							|| matches!(algorithm, PathIndexAlgorithm::OnDemand)
								&& function.parameters().len() == 2
								&& function.parameters().nth(1).is_some_and(|parameter| {
									parameter.value_type() == &Type::Number
								}))
							&& function.parameters().next().is_some_and(|parameter| {
								matches!(
									parameter.value_type(),
									Type::Any | Type::AtomOccurrence(AtomKind::Edge)
								)
							})
							&& matches!(
								function.result(),
								Type::Int | Type::Float | Type::Decimal | Type::Number
							)
							&& (matches!(algorithm, PathIndexAlgorithm::OnDemand)
								|| function.effect() == FunctionEffect::Pure && function.is_cost())
					});
					if candidates.len() != 1 {
						return Err(self.semantic(
							"Graph COST must resolve uniquely to a numeric edge function; \
							 materialization requires PURE COST.",
							function.name.span,
						));
					}
					let candidate = candidates.pop().ok_or_else(|| {
						self.semantic("Graph COST function is absent.", function.name.span)
					})?;
					GraphIndexCost::Function {
						function: candidate.id().ok_or_else(|| {
							self.semantic("Graph COST function is unpublished.", function.name.span)
						})?,
						version: candidate.version(),
					}
				} else if definition.weighted {
					GraphIndexCost::Weighted
				} else {
					GraphIndexCost::Hop
				};
				GraphIndexDefinition::Path {
					algorithm,
					cost,
					patterns: definition
						.patterns
						.iter()
						.map(|source| source.as_ref().into())
						.collect(),
				}
			}
		};
		let mode = match definition
			.projection
			.as_ref()
			.map(|name| name.value.to_ascii_uppercase())
			.as_deref()
			.unwrap_or("NODE")
		{
			"NODE" => TraversalSemantics::Node,
			"ATOM" => TraversalSemantics::Atom,
			_ => {
				return Err(
					self.semantic("Graph index MODE must be NODE or ATOM.", definition.name.span)
				);
			}
		};
		let direction = match definition
			.direction
			.as_ref()
			.map(|name| name.value.to_ascii_uppercase())
			.as_deref()
			.unwrap_or("OUT")
		{
			"OUT" => TraversalDirection::Forward,
			"IN" => TraversalDirection::Backward,
			"BOTH" => TraversalDirection::Both,
			_ => {
				return Err(self.semantic(
					"Graph index DIRECTION must be OUT, IN, or BOTH.",
					definition.name.span,
				));
			}
		};
		let mut index =
			GraphIndex::new(definition.name.value.as_ref(), options, definition.max_bytes)?
				.with_mode(mode)
				.with_direction(direction)
				.with_edge_labels(definition.edge_labels.iter().map(|label| label.value.as_ref()))?
				.with_maintenance_mode(if definition.deferred {
					IndexMaintenanceMode::Deferred
				} else {
					IndexMaintenanceMode::Immediate
				});
		index.validate_family(graph.kind())?;
		index.comment = definition.comment.as_ref().map(|comment| comment.as_ref().into());
		index.validate()?;
		Ok(LogicalSchemaStatement::DefineGraphIndex {
			mode: definition.mode,
			graph: definition.graph.value.clone(),
			index,
			concurrently: definition.concurrently,
		})
	}

	fn define_index(
		&self,
		definition: &DefineIndexStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		let (kind, fields, options) = match &definition.definition {
			BasicIndexDefinition::Geometry {
				field,
				geometry_type,
			} => {
				let kind = geometry_type
					.as_ref()
					.map(|kind| kind.value.parse::<crate::values::GeometryKind>())
					.transpose()?;
				(
					ClassIndexKind::Geometry,
					vec![field.value.clone()],
					ClassIndexOptions::Geometry(crate::datastore::GeometryIndexOptions::new(kind)),
				)
			}
			BasicIndexDefinition::Standard(fields) => (
				ClassIndexKind::Standard,
				fields.iter().map(|field| field.value.clone()).collect(),
				ClassIndexOptions::Scalar,
			),
			BasicIndexDefinition::Unique(fields) => (
				ClassIndexKind::Unique,
				fields.iter().map(|field| field.value.clone()).collect(),
				ClassIndexOptions::Scalar,
			),
			BasicIndexDefinition::Count => {
				(ClassIndexKind::Count, Vec::new(), ClassIndexOptions::Scalar)
			}
			BasicIndexDefinition::Fulltext {
				field,
				analyzer,
				k1_bits,
				b_bits,
				highlights,
			} => {
				let analyzer_definition =
					self.catalog.analyzer(analyzer.value.as_ref())?.ok_or_else(|| {
						self.semantic(
							&format!("Analyzer '{}' does not exist.", analyzer.value),
							analyzer.span,
						)
					})?;
				let analyzer_id = analyzer_definition
					.id()
					.ok_or_else(|| self.semantic("Analyzer is not published.", analyzer.span))?;
				let options = FulltextIndexOptions::new(
					analyzer_id,
					analyzer_definition.version(),
					f64::from_bits(*k1_bits),
					f64::from_bits(*b_bits),
					*highlights,
				)?;
				(
					ClassIndexKind::Fulltext,
					vec![field.value.clone()],
					ClassIndexOptions::Fulltext(options),
				)
			}
			BasicIndexDefinition::Hnsw {
				field,
				dimension,
				element,
				distance,
				ef_construction,
				m,
			} => {
				let element = vector_element(*element);
				let distance = self.vector_distance(distance, element)?;
				let options = HnswIndexOptions::new(
					element,
					self.nonzero_index_value(*dimension, "HNSW dimension", field.span)?,
					distance,
					self.nonzero_index_value(*m, "HNSW M", field.span)?,
					self.nonzero_index_value(*ef_construction, "HNSW EFC", field.span)?,
				)?;
				(ClassIndexKind::Hnsw, vec![field.value.clone()], ClassIndexOptions::Hnsw(options))
			}
			BasicIndexDefinition::DiskAnn {
				field,
				dimension,
				element,
				distance,
				degree,
				build_list_size,
				alpha_bits,
				hashed_vector,
			} => {
				let element = vector_element(*element);
				let distance = self.vector_distance(distance, element)?;
				let options = DiskAnnIndexOptions::new(
					element,
					self.nonzero_index_value(*dimension, "DiskANN dimension", field.span)?,
					distance,
					self.nonzero_index_value(*degree, "DiskANN degree", field.span)?,
					self.nonzero_index_value(*build_list_size, "DiskANN L_BUILD", field.span)?,
					f32::from_bits(*alpha_bits),
					*hashed_vector,
				)?;
				(
					ClassIndexKind::DiskAnn,
					vec![field.value.clone()],
					ClassIndexOptions::DiskAnn(options),
				)
			}
		};
		let scope = match &definition.scope {
			IndexScopeName::Base => IndexScope::Base,
			IndexScopeName::Effective {
				graph,
			} => IndexScope::Effective {
				graph: self.catalog.graph(&graph.value)?.id(),
			},
		};
		if definition.deferred && kind == ClassIndexKind::Unique {
			return Err(
				self.semantic("DEFER is not supported for Unique indexes.", definition.name.span)
			);
		}
		Ok(LogicalSchemaStatement::DefineIndex {
			mode: definition.mode,
			class: definition.class.value.clone(),
			name: definition.name.value.clone(),
			kind,
			fields,
			options,
			scope,
			maintenance: if definition.deferred {
				IndexMaintenanceMode::Deferred
			} else {
				IndexMaintenanceMode::Immediate
			},
			concurrently: definition.concurrently,
		})
	}

	fn nonzero_index_value(
		&self,
		value: u16,
		label: &str,
		span: TextSpan,
	) -> FerrosGrynnResult<NonZeroU16> {
		NonZeroU16::new(value)
			.ok_or_else(|| self.semantic(&format!("{label} must be positive."), span))
	}

	fn vector_distance(
		&self,
		distance: &VectorDistanceName,
		element: VectorKind,
	) -> FerrosGrynnResult<VectorDistance> {
		Ok(match distance {
			VectorDistanceName::Euclidean => VectorDistance::Euclidean,
			VectorDistanceName::Cosine => VectorDistance::Cosine,
			VectorDistanceName::Manhattan => VectorDistance::Manhattan,
			VectorDistanceName::Minkowski {
				exponent_bits,
			} => VectorDistance::Minkowski(f64::from_bits(*exponent_bits)),
			VectorDistanceName::InnerProduct => VectorDistance::InnerProduct,
			VectorDistanceName::CosineNormalized => VectorDistance::CosineNormalized,
			VectorDistanceName::Function(function) => {
				if function.namespace.value.as_ref() != "fn" || function.parameters.is_some() {
					return Err(self.semantic(
						"Custom distance must name fn::name without an overload signature.",
						function.name.span,
					));
				}
				let vector = Type::Vector(element);
				let mut candidates = self.catalog.functions("fn", function.name.value.as_ref())?;
				candidates.retain(|candidate| {
					candidate.effect() == FunctionEffect::Pure
						&& candidate.is_cost()
						&& candidate.result() == &Type::Float
						&& candidate
							.parameters()
							.map(SchemaFunctionParameter::value_type)
							.eq([&vector, &vector])
				});
				if candidates.len() != 1 {
					return Err(self.semantic(
						"Custom distance must resolve uniquely to PURE COST fn(vector<T>, \
						 vector<T>) -> float.",
						function.name.span,
					));
				}
				let candidate = candidates.pop().ok_or_else(|| {
					self.semantic("Custom distance function does not exist.", function.name.span)
				})?;
				VectorDistance::Custom {
					function: candidate.id().ok_or_else(|| {
						self.semantic(
							"Custom distance function is not published.",
							function.name.span,
						)
					})?,
					version: candidate.version(),
				}
			}
		})
	}

	fn define_parameter(
		&self,
		definition: &DefineParameterStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		Ok(LogicalSchemaStatement::DefineParameter {
			mode: definition.mode,
			name: definition.name.value.clone(),
			value: self.expr(&definition.value, None)?,
			comment: definition.comment.as_deref().map(Text::from),
		})
	}

	fn define_analyzer(
		&self,
		definition: &DefineAnalyzerStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		let tokenizers =
			definition.tokenizers.iter().copied().map(analyzer_tokenizer).collect::<Vec<_>>();
		let filters = definition
			.filters
			.iter()
			.map(|filter| self.analyzer_filter(filter))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut analyzer =
			SchemaAnalyzer::new(definition.name.value.as_ref(), tokenizers, filters)?;
		if let Some(function) = &definition.function {
			let (id, version) = self.resolve_analyzer_function(function)?;
			analyzer = analyzer.with_function(id, version)?;
		}
		if let Some(comment) = definition.comment.as_deref() {
			analyzer = analyzer.with_comment(comment);
		}
		Ok(LogicalSchemaStatement::DefineAnalyzer {
			mode: definition.mode,
			analyzer,
		})
	}

	fn analyzer_filter(&self, filter: &AnalyzerFilterName) -> FerrosGrynnResult<AnalyzerFilter> {
		Ok(match filter {
			AnalyzerFilterName::Nfc => AnalyzerFilter::Nfc,
			AnalyzerFilterName::Nfkc => AnalyzerFilter::Nfkc,
			AnalyzerFilterName::Ascii => AnalyzerFilter::Ascii,
			AnalyzerFilterName::Lowercase => AnalyzerFilter::Lowercase,
			AnalyzerFilterName::Uppercase => AnalyzerFilter::Uppercase,
			AnalyzerFilterName::EdgeNgram {
				min,
				max,
			} => AnalyzerFilter::EdgeNgram {
				min: *min,
				max: *max,
			},
			AnalyzerFilterName::Ngram {
				min,
				max,
			} => AnalyzerFilter::Ngram {
				min: *min,
				max: *max,
			},
			AnalyzerFilterName::Snowball(language) => {
				AnalyzerFilter::Snowball(analyzer_language(language)?)
			}
			AnalyzerFilterName::Mapper(value) => {
				let ExprKind::Object(fields) = &value.kind else {
					return Err(
						self.semantic("Analyzer mapper must be an inline object.", value.span)
					);
				};
				let mut entries = BTreeMap::new();
				for field in fields {
					let replacement = match &field.value.kind {
						ExprKind::Literal(Literal::String(value)) => {
							Some(Text::from(value.as_ref()))
						}
						ExprKind::Literal(Literal::Null) => None,
						_ => {
							return Err(self.semantic(
								"Analyzer mapper values must be strings or null.",
								field.value.span,
							));
						}
					};
					if entries.insert(Text::from(field.name.value.as_ref()), replacement).is_some()
					{
						return Err(
							self.semantic("Analyzer mapper keys must be unique.", field.name.span)
						);
					}
				}
				AnalyzerFilter::Mapper(entries)
			}
		})
	}

	fn resolve_analyzer_function(
		&self,
		function: &FunctionSignatureName,
	) -> FerrosGrynnResult<(FunctionId, crate::datastore::SchemaVersion)> {
		if function.namespace.value.as_ref() != "fn" || function.parameters.is_some() {
			return Err(self.semantic(
				"Analyzer FUNCTION must name fn::name without an overload signature.",
				function.name.span,
			));
		}
		let mut candidates = self.catalog.functions("fn", function.name.value.as_ref())?;
		candidates.retain(|candidate| {
			candidate.effect() == FunctionEffect::Pure
				&& candidate.parameters().len() == 1
				&& candidate
					.parameters()
					.next()
					.is_some_and(|parameter| parameter.value_type() == &Type::String)
				&& candidate.result() == &Type::Array(Box::new(Type::Object))
		});
		if candidates.len() != 1 {
			return Err(self.semantic(
				"Analyzer FUNCTION must resolve uniquely to PURE fn(string) -> array<object>.",
				function.name.span,
			));
		}
		let candidate = candidates.pop().ok_or_else(|| {
			self.semantic("Analyzer FUNCTION does not exist.", function.name.span)
		})?;
		let id = candidate.id().ok_or_else(|| {
			self.semantic("Analyzer FUNCTION is not published.", function.name.span)
		})?;
		Ok((id, candidate.version()))
	}

	fn define_function(
		&self,
		definition: &DefineFunctionStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		let parameters = definition
			.parameters
			.iter()
			.map(|parameter| {
				Ok((parameter.name.value.clone(), self.type_name(&parameter.value_type)?))
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let declared_result =
			definition.result_type.as_ref().map(|result| self.type_name(result)).transpose()?;
		let body = Text::from(self.source_slice(definition.body.span)?);
		let pending = SchemaFunction::new(
			definition.namespace.value.as_ref(),
			definition.name.value.as_ref(),
			parameters
				.iter()
				.map(|(name, value_type)| {
					SchemaFunctionParameter::new(name.as_ref(), value_type.clone())
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?,
			declared_result.clone().unwrap_or(Type::Any),
			function_effect(definition.effect),
			definition.cost,
			definition.max_depth,
			body.clone(),
		)?;
		let closure = crate::syn::ClosureExpr {
			parameters: definition
				.parameters
				.iter()
				.map(|parameter| crate::syn::ClosureParameter {
					name: parameter.name.clone(),
					declared_type: Some(parameter.value_type.clone()),
				})
				.collect(),
			result_type: definition.result_type.clone(),
			body: crate::syn::ClosureBody::Block(definition.body.clone()),
		};
		let analyzer = Self {
			source: self.source,
			catalog: self.catalog,
			slots: BTreeMap::new(),
			initial_slots: Vec::new(),
			next_slot: 0,
			scopes: vec![Vec::new()],
			variables: self.variables,
			statement_now: self.statement_now.clone(),
			function_stack: Vec::new(),
			function_overrides: BTreeMap::new(),
			function_cache: self.function_cache,
			slot_reads: None,
			live_checks: self.live_checks.clone(),
			live_scratch: self.live_scratch,
			current_function: Some(Arc::new(pending)),
			current_function_has_explicit_result: declared_result.is_some(),
		};
		let analyzed = analyzer.closure(&closure, definition.body.span)?;
		let effect = function_effect(definition.effect);
		let allowed = match effect {
			FunctionEffect::Pure => ClosureEffect::Pure,
			FunctionEffect::Stable => ClosureEffect::StableRead,
			FunctionEffect::Impure => ClosureEffect::DataMutation,
		};
		if analyzed.effect > allowed {
			return Err(self.semantic(
				"Function body exceeds its declared effect or performs a schema mutation.",
				definition.body.span,
			));
		}
		if definition.cost
			&& (!matches!(analyzed.result, Type::Int | Type::Float | Type::Decimal | Type::Number)
				|| effect != FunctionEffect::Pure)
		{
			return Err(self.semantic(
				"COST functions must be PURE and return a number.",
				definition.body.span,
			));
		}
		Ok(LogicalSchemaStatement::DefineFunction {
			mode: definition.mode,
			namespace: definition.namespace.value.clone(),
			name: definition.name.value.clone(),
			parameters,
			result: analyzed.result,
			effect,
			cost: definition.cost,
			max_depth: definition.max_depth,
			body,
			comment: definition.comment.as_deref().map(Text::from),
		})
	}

	fn remove(&self, target: &RemoveTarget) -> FerrosGrynnResult<LogicalSchemaStatement> {
		match target {
			RemoveTarget::Event {
				if_exists,
				name,
				owner,
			} => Ok(LogicalSchemaStatement::Event(crate::gql::ir::LogicalEventStatement::Remove {
				if_exists: *if_exists,
				name: name.value.clone(),
				owner: event::event_owner(owner),
			})),
			RemoveTarget::LiveLet(name) => Ok(LogicalSchemaStatement::RemoveLiveBinding {
				id: self.live_binding_identity(name)?,
			}),
			RemoveTarget::Analyzer {
				if_exists,
				name,
			} => Ok(LogicalSchemaStatement::RemoveAnalyzer {
				if_exists: *if_exists,
				name: name.value.clone(),
			}),
			RemoveTarget::Function {
				if_exists,
				signature,
			} => Ok(LogicalSchemaStatement::RemoveFunction {
				if_exists: *if_exists,
				namespace: signature.namespace.value.clone(),
				name: signature.name.value.clone(),
				signature: self.function_signature(signature)?,
			}),
			RemoveTarget::Parameter {
				if_exists,
				name,
			} => Ok(LogicalSchemaStatement::RemoveParameter {
				if_exists: *if_exists,
				name: name.value.clone(),
			}),
			RemoveTarget::Graph {
				if_exists,
				name,
			} => Ok(LogicalSchemaStatement::RemoveGraph {
				if_exists: *if_exists,
				name: name.value.clone(),
			}),
			RemoveTarget::Class {
				if_exists,
				name,
			} => Ok(LogicalSchemaStatement::RemoveClass {
				if_exists: *if_exists,
				name: name.value.clone(),
			}),
			RemoveTarget::Field {
				if_exists,
				name,
				class,
			} => Ok(LogicalSchemaStatement::RemoveField {
				if_exists: *if_exists,
				class: class.value.clone(),
				name: name.value.clone(),
			}),
			RemoveTarget::Index {
				if_exists,
				name,
				class,
			} => Ok(LogicalSchemaStatement::RemoveIndex {
				if_exists: *if_exists,
				class: class.value.clone(),
				name: name.value.clone(),
			}),
			RemoveTarget::GraphIndex {
				if_exists,
				name,
				graph,
			} => Ok(LogicalSchemaStatement::RemoveGraphIndex {
				if_exists: *if_exists,
				graph: graph.value.clone(),
				name: name.value.clone(),
			}),
		}
	}

	fn alter(&self, target: &AlterTarget) -> FerrosGrynnResult<LogicalSchemaStatement> {
		match target {
			AlterTarget::Event {
				if_exists,
				name,
				owner,
				actions,
			} => Ok(LogicalSchemaStatement::Event(crate::gql::ir::LogicalEventStatement::Alter {
				if_exists: *if_exists,
				name: name.value.clone(),
				owner: event::event_owner(owner),
				actions: self.event_changes(actions)?,
			})),
			AlterTarget::LiveLet {
				name,
				value,
			} => Ok(LogicalSchemaStatement::ReplaceLiveBinding {
				id: self.live_binding_identity(name)?,
				plan: Box::new(self.live_binding(value)?),
			}),
			AlterTarget::Let {
				name,
				..
			} => Err(self.semantic(
				"Lexical ALTER LET must lower through its own slot boundary.",
				name.span,
			)),
			AlterTarget::Changefeed {
				graph,
				if_exists,
				name,
				configuration,
			} => Ok(LogicalSchemaStatement::AlterChangefeed {
				graph: *graph,
				if_exists: *if_exists,
				name: name.value.clone(),
				configuration: configuration
					.as_ref()
					.map(|clause| self.changefeed_config(clause))
					.transpose()?,
			}),
			AlterTarget::Analyzer {
				if_exists,
				name,
				function,
				tokenizers,
				filters,
				comment,
			} => Ok(LogicalSchemaStatement::AlterAnalyzer {
				if_exists: *if_exists,
				name: name.value.clone(),
				function: function
					.as_ref()
					.map(|function| {
						function
							.as_ref()
							.map(|function| self.resolve_analyzer_function(function))
							.transpose()
					})
					.transpose()?,
				tokenizers: tokenizers
					.as_ref()
					.map(|tokenizers| tokenizers.iter().copied().map(analyzer_tokenizer).collect()),
				filters: filters
					.as_ref()
					.map(|filters| {
						filters
							.iter()
							.map(|filter| self.analyzer_filter(filter))
							.collect::<FerrosGrynnResult<Vec<_>>>()
					})
					.transpose()?,
				comment: match comment {
					ParameterCommentChange::Keep => None,
					ParameterCommentChange::Set(comment) => {
						Some(Some(Text::from(comment.as_ref())))
					}
					ParameterCommentChange::Drop => Some(None),
				},
			}),
			AlterTarget::Function {
				if_exists,
				signature,
				effect,
				cost,
				max_depth,
				body,
				comment,
			} => {
				let parameter_types = self.function_signature(signature)?;
				let body = body
					.as_ref()
					.map(|body| self.source_slice(body.span).map(Text::from))
					.transpose()?;
				if effect.is_some() || cost.is_some() || max_depth.is_some() || body.is_some() {
					self.validate_altered_function(
						signature.namespace.value.as_ref(),
						signature.name.value.as_ref(),
						parameter_types.as_deref(),
						effect.map(function_effect),
						*cost,
						*max_depth,
						body.as_ref(),
						signature.name.span,
					)?;
				}
				Ok(LogicalSchemaStatement::AlterFunction {
					if_exists: *if_exists,
					namespace: signature.namespace.value.clone(),
					name: signature.name.value.clone(),
					signature: parameter_types,
					effect: effect.map(function_effect),
					cost: *cost,
					max_depth: *max_depth,
					body,
					comment: match comment {
						ParameterCommentChange::Keep => None,
						ParameterCommentChange::Set(comment) => {
							Some(Some(Text::from(comment.as_ref())))
						}
						ParameterCommentChange::Drop => Some(None),
					},
				})
			}
			AlterTarget::Parameter {
				if_exists,
				name,
				value,
				comment,
			} => Ok(LogicalSchemaStatement::AlterParameter {
				if_exists: *if_exists,
				name: name.value.clone(),
				value: value.as_ref().map(|value| self.expr(value, None)).transpose()?,
				comment: match comment {
					ParameterCommentChange::Keep => None,
					ParameterCommentChange::Set(comment) => {
						Some(Some(Text::from(comment.as_ref())))
					}
					ParameterCommentChange::Drop => Some(None),
				},
			}),
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

	#[allow(
		clippy::too_many_arguments,
		reason = "function-call analysis needs the complete typed call context"
	)]
	fn validate_altered_function(
		&self,
		namespace: &str,
		name: &str,
		signature: Option<&[Type]>,
		effect: Option<FunctionEffect>,
		cost: Option<bool>,
		max_depth: Option<Option<u8>>,
		body: Option<&Text>,
		span: TextSpan,
	) -> FerrosGrynnResult<()> {
		let mut candidates = self.catalog.functions(namespace, name)?;
		if let Some(signature) = signature {
			candidates.retain(|function| {
				function.parameters().map(SchemaFunctionParameter::value_type).eq(signature.iter())
			});
		}
		if candidates.len() != 1 {
			return Ok(());
		}
		let Some(existing) = candidates.pop() else {
			return Ok(());
		};
		let parameters = existing
			.parameters()
			.map(|parameter| {
				SchemaFunctionParameter::new(
					parameter.name().clone(),
					parameter.value_type().clone(),
				)
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut replacement = SchemaFunction::new(
			existing.namespace().clone(),
			existing.name().clone(),
			parameters,
			existing.result().clone(),
			effect.unwrap_or(existing.effect()),
			cost.unwrap_or(existing.is_cost()),
			max_depth.unwrap_or(existing.max_depth()),
			body.cloned().unwrap_or_else(|| existing.body().clone()),
		)?;
		replacement.id = existing.id();
		replacement.version = existing.version();
		if replacement.is_cost()
			&& !matches!(
				replacement.result(),
				Type::Int | Type::Float | Type::Decimal | Type::Number
			) {
			return Err(self.semantic("COST functions must return a number.", span));
		}
		compile_named_function(
			self.catalog,
			&replacement,
			&self.function_stack,
			&self.function_overrides,
			self.function_cache,
		)
		.map(|_| ())
		.map_err(|error| self.semantic(&format!("Altered function body is invalid: {error}"), span))
	}

	fn info(&self, target: &InfoTarget) -> FerrosGrynnResult<LogicalSchemaStatement> {
		Ok(match target {
			InfoTarget::Database => LogicalSchemaStatement::InfoDatabase,
			InfoTarget::LiveLet(name) => LogicalSchemaStatement::InfoLiveBinding {
				id: self.live_binding_identity(name)?,
			},
			InfoTarget::Live(parameter) => LogicalSchemaStatement::InfoLive {
				id: self.expr(
					&Expr {
						kind: ExprKind::Parameter(parameter.clone()),
						span: parameter.span,
					},
					None,
				)?,
			},
			InfoTarget::Changefeed(parameter) => LogicalSchemaStatement::InfoChangefeed {
				id: self.expr(
					&Expr {
						kind: ExprKind::Parameter(parameter.clone()),
						span: parameter.span,
					},
					None,
				)?,
				spelling: self.changefeed_spelling([parameter.span])?,
			},
			InfoTarget::Analyzer(name) => LogicalSchemaStatement::InfoAnalyzer {
				name: name.value.clone(),
			},
			InfoTarget::Function(signature) => LogicalSchemaStatement::InfoFunction {
				namespace: signature.namespace.value.clone(),
				name: signature.name.value.clone(),
				signature: self.function_signature(signature)?,
			},
			InfoTarget::Parameter(name) => LogicalSchemaStatement::InfoParameter {
				name: name.value.clone(),
			},
			InfoTarget::Job(job) => {
				return Err(self.semantic(
					"INFO FOR JOB must be executed through Datastore or GqlSession.",
					job.span,
				));
			}
			InfoTarget::Jobs => {
				return Err(FerrosGrynnError::invalid_argument(
					"INFO FOR JOBS must be executed through Datastore or GqlSession.",
				));
			}
			InfoTarget::Class(name) => LogicalSchemaStatement::InfoClass {
				name: name.value.clone(),
			},
			InfoTarget::Graph(name) => LogicalSchemaStatement::InfoGraph {
				name: name.value.clone(),
			},
			InfoTarget::Index {
				name,
				class,
			} => LogicalSchemaStatement::InfoIndex {
				class: class.value.clone(),
				name: name.value.clone(),
			},
			InfoTarget::GraphIndex {
				name,
				graph,
			} => LogicalSchemaStatement::InfoGraphIndex {
				graph: graph.value.clone(),
				name: name.value.clone(),
			},
		})
	}

	fn function_signature(
		&self,
		signature: &FunctionSignatureName,
	) -> FerrosGrynnResult<Option<Vec<Type>>> {
		signature
			.parameters
			.as_ref()
			.map(|parameters| {
				parameters.iter().map(|parameter| self.type_name(parameter)).collect()
			})
			.transpose()
	}

	fn source_slice(&self, span: TextSpan) -> FerrosGrynnResult<&str> {
		let start = span.offset as usize;
		let end = span
			.end()
			.map(|end| end as usize)
			.ok_or_else(|| self.semantic("Function body source range overflows.", span))?;
		self.source
			.get(start..end)
			.ok_or_else(|| self.semantic("Function body source range is invalid.", span))
	}

	fn traverse(
		&self,
		statement: &TraverseStatement,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalTraverse> {
		let source = match &statement.source {
			TraverseSource::Expression(expression) => {
				LogicalTraverseSource::Expression(self.expr(expression, None)?)
			}
			TraverseSource::Match(statement) => {
				let mut source = self.match_statement(statement)?;
				source.projection = source
					.bindings
					.columns
					.iter()
					.enumerate()
					.map(|(column, binding)| TypedExpr {
						kind: TypedExprKind::Column(column),
						ty: binding.ty.clone(),
						span,
					})
					.collect();
				source.result = source.bindings.clone();
				LogicalTraverseSource::Match(Box::new(source))
			}
			TraverseSource::Select(statement) => {
				LogicalTraverseSource::Select(Box::new(self.select(statement, span)?))
			}
		};
		let graph =
			statement.graph.as_ref().map(|graph| self.graph_reference(graph, None)).transpose()?;
		let custom_cost =
			statement.custom_cost.as_ref().map(|name| self.path_cost_function(name)).transpose()?;
		let mut current_type = traverse_source_item_type(&source);
		let mut steps = Vec::with_capacity(statement.steps.len());
		for step in &statement.steps {
			let analyzed = self.traverse_step(&step.expression, step.span, &current_type)?;
			current_type = traverse_step_output_type(&analyzed, current_type);
			steps.push(analyzed);
		}
		if custom_cost.is_some() {
			if statement.constraints.objective.is_some_and(|objective| !objective.is_weighted())
				|| steps.iter().any(|step| {
					matches!(
						step.kind,
						TraverseStepKind::ShortestPath
							| TraverseStepKind::AllShortestPaths
							| TraverseStepKind::BellmanFord
					)
				}) {
				return Err(self.semantic(
					"TRAVERSE WITH COST requires weightedShortestPath/distanceTo or navigation.",
					span,
				));
			}
			if !steps.iter().any(|step| {
				matches!(
					step.kind,
					TraverseStepKind::WeightedShortestPath
						| TraverseStepKind::AllWeightedShortestPaths
						| TraverseStepKind::DistanceTo
						| TraverseStepKind::Out
						| TraverseStepKind::In
						| TraverseStepKind::Both
						| TraverseStepKind::InVertex
						| TraverseStepKind::OutVertex
				)
			}) {
				return Err(self.semantic("TRAVERSE WITH COST has no connector consumer.", span));
			}
		}
		let mut result = Schema {
			columns: vec![Column {
				name: "current".into(),
				qualifier: None,
				ty: Type::Any,
			}],
			origins: BTreeMap::new(),
		};
		let source_schema = match &source {
			LogicalTraverseSource::Expression(_) => None,
			LogicalTraverseSource::Match(source) => Some(&source.result),
			LogicalTraverseSource::Select(source) => Some(&source.result),
		};
		if let Some(source_schema) = source_schema {
			for column in &source_schema.columns {
				push_traverse_result_column(&mut result, column.name.clone(), column.ty.clone());
			}
		}
		for step in &steps {
			if matches!(step.kind, TraverseStepKind::As | TraverseStepKind::SideEffect)
				&& let Some(name) =
					step.arguments.first().and_then(|argument| match &argument.kind {
						TypedExprKind::Constant(value) => value.as_str(),
						_ => None,
					}) {
				push_traverse_result_column(&mut result, name.into(), Type::Any);
			}
		}
		let returning = statement
			.returning
			.as_ref()
			.map(|returning| self.select_expr(&returning.value, Some(&result), false, 0))
			.transpose()?;
		let return_graph = statement
			.return_graph
			.as_ref()
			.map(|graph| self.return_graph(graph, Some(&result), graph.span))
			.transpose()?;
		Ok(LogicalTraverse {
			atom: statement.atom,
			constraints: statement.constraints.clone(),
			source,
			graph,
			custom_cost,
			steps,
			result,
			returning,
			return_graph,
		})
	}

	fn pipeline_expression(
		&self,
		expression: &Expr,
		schema: Option<&Schema>,
		allow_current_field: bool,
		allow_aggregates: bool,
		aggregate_depth: u32,
	) -> FerrosGrynnResult<TypedExpr> {
		if schema.is_some() {
			return Err(
				self.unsupported("traversal pipeline inside row expression", expression.span)
			);
		}
		let mut step_expressions = Vec::new();
		let source = pipeline_parts(expression, &mut step_expressions);
		let source = self.expr_context(
			source,
			schema,
			allow_current_field,
			allow_aggregates,
			aggregate_depth,
		)?;
		let mut current_type = collection_item_type(&source.ty);
		let mut steps = Vec::with_capacity(step_expressions.len());
		for step in step_expressions {
			let analyzed = self.traverse_step(step, step.span, &current_type)?;
			current_type = traverse_step_output_type(&analyzed, current_type);
			steps.push(analyzed);
		}
		Ok(TypedExpr {
			kind: TypedExprKind::Traversal(Box::new(LogicalTraverse {
				atom: false,
				constraints: crate::syn::MatchConstraints::default(),
				source: LogicalTraverseSource::Expression(source),
				graph: None,
				custom_cost: None,
				steps,
				result: Schema {
					columns: vec![Column {
						name: "current".into(),
						qualifier: None,
						ty: Type::Any,
					}],
					origins: BTreeMap::new(),
				},
				returning: None,
				return_graph: None,
			})),
			ty: Type::Array(Box::new(Type::Any)),
			span: expression.span,
		})
	}

	fn traverse_step(
		&self,
		expression: &Expr,
		span: TextSpan,
		current_type: &Type,
	) -> FerrosGrynnResult<LogicalTraverseStep> {
		if let Some(repeat) = parse_repeat_syntax(expression) {
			return self.traverse_repeat(repeat, span);
		}
		let ExprKind::Call {
			callee,
			type_argument,
			distinct,
			argument_names,
			arguments,
		} = &expression.kind
		else {
			return Err(self.semantic("A traversal step must be a `tv::` call.", expression.span));
		};
		if type_argument.is_some() || *distinct || argument_names.iter().any(Option::is_some) {
			return Err(self.semantic(
				"Traversal steps do not accept type arguments, named arguments, or DISTINCT.",
				expression.span,
			));
		}
		let Some((namespace, name)) = builtin_name(callee) else {
			return Err(
				self.semantic("A traversal step must use the `tv::` namespace.", callee.span)
			);
		};
		if !namespace.eq_ignore_ascii_case("tv") {
			return Err(
				self.semantic("A traversal step must use the `tv::` namespace.", callee.span)
			);
		}
		let kind = traverse_step_kind(name).ok_or_else(|| {
			self.semantic(&format!("Unknown traversal step `tv::{name}`."), callee.span)
		})?;
		let expected = traverse_step_arity(kind);
		if !expected.contains(&arguments.len()) {
			return Err(self.semantic(
				&format!(
					"Traversal step `tv::{name}` expects {}..={} argument(s), found {}.",
					expected.start(),
					expected.end(),
					arguments.len()
				),
				expression.span,
			));
		}
		if kind == TraverseStepKind::Match {
			let ExprKind::MatchSubquery(statement) = &arguments[0].kind else {
				return Err(self.semantic(
					"tv::match expects `MATCH ... IN GRAPH ... RETURN ...`.",
					arguments[0].span,
				));
			};
			return Ok(LogicalTraverseStep {
				kind,
				arguments: Vec::new(),
				named_function: None,
				algorithm_input: None,
				output_type: None,
				match_plan: Some(Box::new(self.match_statement(statement)?)),
				repeat: None,
				span,
			});
		}
		if kind == TraverseStepKind::Algorithm {
			return self.traverse_algorithm(arguments, current_type, span);
		}
		let arguments = arguments
			.iter()
			.map(|argument| self.expr(argument, None))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		self.validate_traverse_step_effect(kind, &arguments)?;
		if kind.is_tensor_analytics()
			&& arguments
				.first()
				.is_some_and(|argument| !matches!(argument.ty, Type::Object | Type::Any))
		{
			return Err(self.semantic("Analytical options must be an object.", span));
		}
		Ok(LogicalTraverseStep {
			kind,
			arguments,
			named_function: None,
			algorithm_input: None,
			output_type: None,
			match_plan: None,
			repeat: None,
			span,
		})
	}

	fn traverse_repeat(
		&self,
		repeat: ParsedRepeat,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalTraverseStep> {
		if repeat.arguments.len() != 1 {
			return Err(self.semantic("tv::repeat expects one closure.", span));
		}
		let body = self.expr(&repeat.arguments[0], None)?;
		ensure_traversal_closure(self, &body, false)?;
		let mut times = None;
		let mut until = None;
		let mut emit = false;
		for modifier in repeat.modifiers {
			match modifier.name.to_ascii_lowercase().as_str() {
				"times" if times.is_none() && modifier.arguments.len() == 1 => {
					times = Some(self.expr(&modifier.arguments[0], None)?);
				}
				"until" if until.is_none() && modifier.arguments.len() == 1 => {
					let predicate = self.expr(&modifier.arguments[0], None)?;
					ensure_traversal_closure(self, &predicate, false)?;
					until = Some(predicate);
				}
				"emit" if !emit && modifier.arguments.is_empty() => emit = true,
				_ => {
					return Err(self.semantic(
						"repeat accepts each of `.times(n)`, `.until(closure)`, and `.emit()` at \
						 most once.",
						modifier.span,
					));
				}
			}
		}
		if times.is_none() && until.is_none() {
			return Err(
				self.semantic("tv::repeat requires `.times(n)` or `.until(closure)`.", span)
			);
		}
		Ok(LogicalTraverseStep {
			kind: TraverseStepKind::Repeat,
			arguments: Vec::new(),
			named_function: None,
			algorithm_input: None,
			output_type: None,
			match_plan: None,
			repeat: Some(LogicalRepeat {
				body,
				times,
				until,
				emit,
			}),
			span,
		})
	}

	fn traverse_algorithm(
		&self,
		arguments: &[Expr],
		current_type: &Type,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalTraverseStep> {
		let Some((namespace, name)) = arguments.first().and_then(builtin_name) else {
			return Err(
				self.semantic("tv::algo expects a named `fn::name` as its first argument.", span)
			);
		};
		if !namespace.eq_ignore_ascii_case("fn") {
			return Err(self.semantic(
				"tv::algo accepts only a named function from the `fn::` namespace.",
				arguments[0].span,
			));
		}
		let mut call_arguments = Vec::with_capacity(arguments.len());
		call_arguments.push(TypedExpr {
			kind: TypedExprKind::Constant(Value::None),
			ty: current_type.clone(),
			span,
		});
		call_arguments.extend(
			arguments[1..]
				.iter()
				.map(|argument| self.expr(argument, None))
				.collect::<FerrosGrynnResult<Vec<_>>>()?,
		);
		let argument_names = vec![None; call_arguments.len()];
		let (invocation, output_type) =
			self.named_function(name, &argument_names, call_arguments, span)?;
		let (callee, named_function, mut resolved_arguments) = match invocation {
			TypedExprKind::Invoke {
				callee,
				arguments,
			} => (Some(*callee), None, arguments),
			TypedExprKind::NamedInvoke {
				function,
				arguments,
			} => (None, Some(function), arguments),
			_ => {
				return Err(self.semantic("tv::algo failed to resolve its named function.", span));
			}
		};
		let implicit = resolved_arguments.remove(0);
		let effect = callee
			.as_ref()
			.and_then(|callee| match &callee.kind {
				TypedExprKind::Closure(closure) => {
					Some(closure.named_function.map_or(closure.effect, |function| {
						match function.effect {
							FunctionEffect::Pure => ClosureEffect::Pure,
							FunctionEffect::Stable => ClosureEffect::StableRead,
							FunctionEffect::Impure => ClosureEffect::DataMutation,
						}
					}))
				}
				_ => None,
			})
			.or_else(|| {
				named_function.map(|function| match function.effect {
					FunctionEffect::Pure => ClosureEffect::Pure,
					FunctionEffect::Stable => ClosureEffect::StableRead,
					FunctionEffect::Impure => ClosureEffect::DataMutation,
				})
			});
		if !matches!(effect, Some(ClosureEffect::Pure | ClosureEffect::StableRead)) {
			return Err(FerrosGrynnError::closure_effect_not_allowed(
				format!("{effect:?}"),
				"tv::algo".to_owned(),
			));
		}
		let mut step_arguments = Vec::with_capacity(resolved_arguments.len() + 1);
		if let Some(callee) = callee {
			step_arguments.push(callee);
		}
		step_arguments.extend(resolved_arguments);
		Ok(LogicalTraverseStep {
			kind: TraverseStepKind::Algorithm,
			arguments: step_arguments,
			named_function,
			algorithm_input: Some(implicit.ty),
			output_type: Some(output_type),
			match_plan: None,
			repeat: None,
			span,
		})
	}

	fn validate_traverse_step_effect(
		&self,
		kind: TraverseStepKind,
		arguments: &[TypedExpr],
	) -> FerrosGrynnResult<()> {
		let positions = match kind {
			TraverseStepKind::Where
			| TraverseStepKind::DeduplicateBy
			| TraverseStepKind::Map
			| TraverseStepKind::FlatMap
			| TraverseStepKind::Sack => vec![0],
			TraverseStepKind::NamedSack => vec![1],
			TraverseStepKind::Order => (!arguments.is_empty()).then_some(0).into_iter().collect(),
			TraverseStepKind::Choose => (0..arguments.len()).collect(),
			TraverseStepKind::Branch | TraverseStepKind::Coalesce => (0..arguments.len()).collect(),
			TraverseStepKind::SideEffect => arguments.len().checked_sub(1).into_iter().collect(),
			_ => Vec::new(),
		};
		for position in positions {
			ensure_traversal_closure(
				self,
				&arguments[position],
				kind == TraverseStepKind::SideEffect,
			)?;
		}
		Ok(())
	}

	/// Analyzes a complete MATCH statement.
	fn match_statement(&self, statement: &MatchStatement) -> FerrosGrynnResult<LogicalMatch> {
		let timeout = statement
			.timeout
			.as_ref()
			.map(|timeout| self.duration_input(timeout, "TIMEOUT"))
			.transpose()?;
		let graph = self.match_graph_reference(&statement.source)?;
		let (max_depth, max_depth_binding) = self.match_max_depth(statement.max_depth.as_ref())?;
		let global_cost = statement
			.custom_cost
			.as_ref()
			.map(|function| self.path_cost_function(function))
			.transpose()?;
		if let Some(function) = &statement.custom_cost
			&& !statement.patterns.iter().any(
				|pattern| matches!(pattern, MatchPattern::Linear(pattern) if !pattern.steps.is_empty()),
			) {
			return Err(
				self.semantic("WITH COST requires a linear connector path.", function.name.span)
			);
		}
		let mut aliases = BTreeMap::new();
		let mut bindings = Schema::default();
		let mut patterns = Vec::with_capacity(statement.patterns.len());
		for (original_index, pattern) in statement.patterns.iter().enumerate() {
			patterns.push(match pattern {
				MatchPattern::Linear(pattern) => self.match_pattern(
					pattern,
					original_index,
					max_depth,
					(&statement.constraints, global_cost),
					statement.atom,
					statement.meta_scope,
					&mut aliases,
					&mut bindings,
				)?,
				MatchPattern::Hyper(pattern) => self.match_hyper_pattern(
					pattern,
					original_index,
					statement.atom,
					statement.meta_scope,
					&mut aliases,
					&mut bindings,
				)?,
				MatchPattern::Within(pattern) => self.match_within_pattern(
					pattern,
					original_index,
					statement.atom,
					statement.meta_scope,
					&mut aliases,
					&mut bindings,
				)?,
			});
		}

		let filter = statement
			.filter
			.as_ref()
			.map(|filter| self.expr(filter, Some(&bindings)))
			.transpose()?;
		let return_graph = statement
			.return_graph
			.as_ref()
			.map(|graph| self.return_graph(graph, Some(&bindings), graph.span))
			.transpose()?;
		let mut projection = Vec::with_capacity(statement.projection.len());
		let mut columns = Vec::with_capacity(statement.projection.len().max(1));
		if let Some(graph) = &return_graph {
			columns.push(Column {
				name: "graph".into(),
				qualifier: None,
				ty: Type::Graph(graph.contract.family.unwrap_or(GraphKind::Subgraph)),
			});
		} else {
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
		}
		let result = Schema {
			columns,
			origins: BTreeMap::new(),
		};
		let order = statement
			.order
			.iter()
			.map(|order| {
				let expression = if order.random {
					TypedExpr {
						kind: TypedExprKind::Constant(Value::None),
						ty: Type::Int,
						span: order.expression.span,
					}
				} else if let ExprKind::Name(name) = &order.expression.kind
					&& let Some(index) = result.index_of(name.value.as_ref())
				{
					TypedExpr {
						kind: TypedExprKind::Column(bindings.columns.len() + index),
						ty: result.columns[index].ty.clone(),
						span: order.expression.span,
					}
				} else {
					self.expr(&order.expression, Some(&bindings))?
				};
				Ok(LogicalOrder {
					expression,
					random: order.random,
					collate: order.collate,
					numeric: order.numeric,
					direction: order.direction,
					nulls: order.nulls,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(LogicalMatch {
			graph,
			timeout,
			optional: statement.optional,
			binding_distinct: statement.binding_distinct,
			mode: statement.mode,
			atom: statement.atom,
			meta_scope: statement.meta_scope,
			patterns,
			bindings,
			filter,
			projection,
			return_graph,
			projection_distinct: statement.projection_distinct,
			order,
			max_depth,
			max_depth_binding,
			result,
		})
	}

	fn match_graph_reference(&self, expression: &Expr) -> FerrosGrynnResult<LogicalGraphReference> {
		let graph = match &expression.kind {
			ExprKind::Name(name) => {
				self.catalog.graph(name.value.as_ref()).map(LogicalGraphReference::Static)
			}
			ExprKind::Parameter(_) => {
				self.expr(expression, None).map(LogicalGraphReference::Expression)
			}
			_ => {
				return Err(self.semantic(
					"MATCH IN GRAPH accepts a graph name or GraphRef parameter.",
					expression.span,
				));
			}
		};
		graph.map_err(|error| {
			self.locate(error.with_phase(ExecutionPhase::SemanticAnalysis), expression.span)
		})
	}

	fn match_max_depth(
		&self,
		expression: Option<&Expr>,
	) -> FerrosGrynnResult<(u32, Option<TypedExpr>)> {
		let Some(expression) = expression else {
			return Ok((MAX_MATCH_PATH_DEPTH, None));
		};
		let value = match &expression.kind {
			ExprKind::Literal(literal) => self.literal(literal, expression.span)?,
			ExprKind::Parameter(_) => {
				return Ok((MAX_MATCH_PATH_DEPTH, Some(self.expr(expression, None)?)));
			}
			_ => {
				return Err(self.semantic(
					"MATCH MAXDEPTH must be an integer literal or parameter.",
					expression.span,
				));
			}
		};
		validate_match_max_depth(&value)
			.map(|depth| (depth, None))
			.map_err(|error| self.semantic(&error.to_string(), expression.span))
	}

	/// Lowers one source pattern into linear expansion operations.
	fn match_pattern(
		&self,
		pattern: &MatchLinearPattern,
		original_index: usize,
		max_depth: u32,
		global: (&crate::syn::MatchConstraints, Option<crate::gql::ir::LogicalPathCost>),
		atom: bool,
		meta_scope: MatchMetaScope,
		aliases: &mut BTreeMap<Box<str>, MatchAlias>,
		bindings: &mut Schema,
	) -> FerrosGrynnResult<LogicalMatchPattern> {
		let (global_constraints, global_cost) = global;
		let path_column = pattern
			.path_alias
			.as_ref()
			.map(|alias| {
				self.bind_match_alias(alias, MatchAliasKind::Path, Type::Path, aliases, bindings)
			})
			.transpose()?;
		let start = self.match_endpoint(
			&pattern.start,
			atom,
			match_meta_required(meta_scope, endpoint_domain(pattern.start.kind)),
			aliases,
			bindings,
		)?;
		let mut source = start.clone();
		let mut ops = Vec::with_capacity(pattern.steps.len());
		for step in &pattern.steps {
			let target = self.match_endpoint(
				&step.target,
				atom,
				match_meta_required(meta_scope, endpoint_domain(step.target.kind)),
				aliases,
				bindings,
			)?;
			let edge = self.match_binding(
				&step.edge.selector,
				step.edge.alias.as_ref(),
				step.edge.label.as_ref(),
				step.edge.properties.as_ref(),
				step.edge.predicate.as_ref(),
				match_meta_required(meta_scope, EndpointDomain::Edge),
				EndpointDomain::Edge,
				aliases,
				bindings,
			)?;
			let custom_cost = step
				.edge
				.custom_cost
				.as_ref()
				.map(|function| self.path_cost_function(function))
				.transpose()?
				.or(global_cost);
			let mode = step
				.edge
				.constraints
				.mode
				.or(global_constraints.mode)
				.unwrap_or(MatchPathMode::Trail);
			let objective = step.edge.constraints.objective.or(global_constraints.objective);
			if custom_cost.is_some() && objective.is_some_and(|objective| !objective.is_weighted())
			{
				return Err(self.semantic(
					"COST requires WEIGHTED SHORTEST or path enumeration.",
					step.edge.span,
				));
			}
			let filters = if step.edge.constraints.filters.is_empty() {
				global_constraints.filters.clone()
			} else {
				step.edge.constraints.filters.clone()
			};
			if let Some(length) = step.edge.length {
				if objective == Some(crate::syn::MatchPathObjective::AllWeightedShortest)
					&& mode == MatchPathMode::Walk
					&& length.max.is_none()
				{
					return Err(self.semantic(
						"ALL WEIGHTED SHORTEST WALK requires an explicit finite depth bound.",
						length.span,
					));
				}
				if let Some(alias) = &step.edge.alias {
					return Err(self.semantic(
						"A variable-length MATCH edge cannot have an alias; bind the path instead.",
						alias.span,
					));
				}
				let maximum = length.max.unwrap_or(max_depth);
				if length.min > maximum {
					return Err(
						self.semantic("MATCH path minimum exceeds its maximum.", length.span)
					);
				}
				if maximum > MAX_MATCH_PATH_DEPTH {
					return Err(self.semantic(
						&format!("MATCH path depth exceeds the maximum of {MAX_MATCH_PATH_DEPTH}."),
						length.span,
					));
				}
				ops.push(MatchOp::PathExpand(PathExpand {
					source: source.clone(),
					edge,
					target: target.clone(),
					direction: step.direction,
					min: length.min,
					max: maximum,
					inherits_max_depth: length.max.is_none(),
					mode,
					objective,
					custom_cost,
					filters,
					path_column,
				}));
			} else {
				if step.edge.constraints.mode.is_some() || step.edge.constraints.objective.is_some()
				{
					return Err(self.semantic(
						"Local MATCH path mode and objective require a variable-length segment.",
						step.edge.span,
					));
				}
				ops.push(MatchOp::Expand(Expand {
					source: source.clone(),
					edge,
					target: target.clone(),
					direction: step.direction,
					custom_cost,
					filters,
				}));
			}
			source = target;
		}
		Ok(LogicalMatchPattern {
			kind: LogicalMatchPatternKind::Linear(LogicalLinearPattern {
				path_column,
				start,
				ops,
			}),
			original_index,
		})
	}

	/// Captures the ordinary named PURE COST plan; two-argument functions are
	/// valid exact consumers but cannot bind an additive landmark artifact.
	fn path_cost_function(
		&self,
		name: &FunctionSignatureName,
	) -> FerrosGrynnResult<crate::gql::ir::LogicalPathCost> {
		let span = name.name.span;
		if name.namespace.value.as_ref() != "fn" || name.parameters.is_some() {
			return Err(self.semantic("COST requires fn::name without a signature.", span));
		}
		let mut candidates = self.named_function_candidates(&name.name.value)?;
		candidates.retain(|function| {
			let mut parameters = function.parameters();
			function.effect() == FunctionEffect::Pure
				&& function.is_cost()
				&& matches!(
					function.result(),
					Type::Int | Type::Float | Type::Decimal | Type::Number
				)
				&& matches!(parameters.len(), 1 | 2)
				&& parameters.next().is_some_and(|parameter| {
					matches!(
						parameter.value_type(),
						Type::Any | Type::AtomOccurrence(AtomKind::Edge)
					)
				})
				&& parameters.next().is_none_or(|parameter| parameter.value_type() == &Type::Number)
		});
		if candidates.len() != 1 {
			return Err(self.semantic(
				"COST must resolve uniquely to PURE COST fn(edge [, number]) -> number.",
				span,
			));
		}
		let function = candidates.pop().ok_or_else(|| self.semantic("COST is absent.", span))?;
		let key = NamedFunctionKey {
			id: function.id().ok_or_else(|| self.semantic("COST is unpublished.", span))?,
			version: function.version(),
			schema_epoch: self.catalog.schema_epoch(),
			max_depth: function.max_depth(),
			effect: function.effect(),
		};
		if self.function_stack.contains(&key.id) {
			Self::validate_recursive_cycle(
				self.catalog,
				&self.function_stack,
				&function,
				span,
				self,
			)?;
		} else if let Some(cache) = self.function_cache
			&& cache.cached_named(key).is_none()
		{
			let plan = compile_named_function(
				self.catalog,
				&function,
				&self.function_stack,
				&self.function_overrides,
				self.function_cache,
			)?;
			cache.cache_named_plan(&plan)?;
		}
		Ok(crate::gql::ir::LogicalPathCost {
			function: key,
			stateful: function.parameters().len() == 2,
		})
	}

	/// Lowers one hyperedge endpoint-assignment relation.
	fn match_hyper_pattern(
		&self,
		pattern: &MatchHyperPattern,
		original_index: usize,
		atom: bool,
		meta_scope: MatchMetaScope,
		aliases: &mut BTreeMap<Box<str>, MatchAlias>,
		bindings: &mut Schema,
	) -> FerrosGrynnResult<LogicalMatchPattern> {
		if pattern.edge.kind != MatchEndpointKind::Edge {
			return Err(self.semantic("MATCH HYPER requires an edge position.", pattern.edge.span));
		}
		let edge = self.match_endpoint(
			&pattern.edge,
			true,
			match_meta_required(meta_scope, EndpointDomain::Edge),
			aliases,
			bindings,
		)?;
		let contains = pattern
			.contains
			.iter()
			.map(|contains| {
				let endpoints = contains
					.endpoints
					.iter()
					.map(|endpoint| {
						self.match_endpoint(
							endpoint,
							atom,
							match_meta_required(meta_scope, endpoint_domain(endpoint.kind)),
							aliases,
							bindings,
						)
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				Ok(LogicalMatchContains {
					side: contains.side,
					endpoints,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(LogicalMatchPattern {
			kind: LogicalMatchPatternKind::Hyper(LogicalHyperPattern {
				edge,
				contains,
			}),
			original_index,
		})
	}

	/// Lowers one direct or bounded transitive containment relation.
	fn match_within_pattern(
		&self,
		pattern: &MatchWithinPattern,
		original_index: usize,
		atom: bool,
		meta_scope: MatchMetaScope,
		aliases: &mut BTreeMap<Box<str>, MatchAlias>,
		bindings: &mut Schema,
	) -> FerrosGrynnResult<LogicalMatchPattern> {
		let max = pattern.depth.max.unwrap_or(MAX_MATCH_WITHIN_DEPTH);
		if pattern.depth.min == 0 {
			return Err(self.semantic("MATCH WITHIN depth must start at one.", pattern.depth.span));
		}
		if pattern.depth.min > max {
			return Err(self
				.semantic("MATCH WITHIN minimum depth exceeds its maximum.", pattern.depth.span));
		}
		if max > MAX_MATCH_WITHIN_DEPTH {
			return Err(self.semantic(
				&format!("MATCH WITHIN depth cannot exceed {MAX_MATCH_WITHIN_DEPTH}."),
				pattern.depth.span,
			));
		}
		let child = self.match_endpoint(
			&pattern.child,
			atom,
			match_meta_required(meta_scope, endpoint_domain(pattern.child.kind)),
			aliases,
			bindings,
		)?;
		let owner = self.match_endpoint(&pattern.owner, true, true, aliases, bindings)?;
		Ok(LogicalMatchPattern {
			kind: LogicalMatchPatternKind::Within(LogicalWithinPattern {
				child,
				owner,
				min: pattern.depth.min,
				max,
			}),
			original_index,
		})
	}

	/// Converts one endpoint into a typed binding.
	fn match_endpoint(
		&self,
		endpoint: &MatchEndpoint,
		allow_edge: bool,
		force_meta: bool,
		aliases: &mut BTreeMap<Box<str>, MatchAlias>,
		bindings: &mut Schema,
	) -> FerrosGrynnResult<MatchBinding> {
		let domain = match endpoint.kind {
			MatchEndpointKind::Node => EndpointDomain::Node,
			MatchEndpointKind::Edge if allow_edge => EndpointDomain::Edge,
			MatchEndpointKind::Edge => {
				return Err(self.semantic("An edge endpoint requires MATCH ATOM.", endpoint.span));
			}
		};
		self.match_binding(
			&endpoint.selector,
			endpoint.alias.as_ref(),
			endpoint.label.as_ref(),
			endpoint.properties.as_ref(),
			endpoint.predicate.as_ref(),
			endpoint.meta || force_meta,
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
		properties: Option<&Expr>,
		predicate: Option<&Expr>,
		meta: bool,
		domain: EndpointDomain,
		aliases: &mut BTreeMap<Box<str>, MatchAlias>,
		bindings: &mut Schema,
	) -> FerrosGrynnResult<MatchBinding> {
		let column = alias
			.map(|alias| {
				let ty = Type::AtomOccurrence(match domain {
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
		let properties = match properties {
			None => Vec::new(),
			Some(Expr {
				kind: ExprKind::Object(fields),
				..
			}) => fields
				.iter()
				.map(|field| {
					Ok((field.name.value.clone(), self.expr(&field.value, Some(bindings))?))
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?,
			Some(value) => {
				return Err(self.semantic(
					"An inline MATCH property constraint must be an object.",
					value.span,
				));
			}
		};
		let predicate =
			predicate.map(|predicate| self.expr(predicate, Some(bindings))).transpose()?;
		Ok(MatchBinding {
			domain,
			selector: self.match_selector(selector, domain)?,
			column,
			label: label.map(|label| label.value.clone()),
			properties,
			predicate,
			meta,
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
		let outer = if statement.lexical_id.is_some() {
			None
		} else {
			outer
		};
		if !statement.union_inputs.is_empty() {
			if statement.union_purpose == crate::syn::SelectUnionPurpose::JoinedTypingVariants {
				self.catalog.admit_analysis(
					statement
						.union_inputs
						.len()
						.checked_mul(std::mem::size_of::<LogicalSelect>())
						.ok_or_else(|| {
							FerrosGrynnError::operation_overflow("LIVE JOIN family metadata")
						})?,
					statement.union_inputs.len() as u64,
				)?;
				let inputs = statement
					.union_inputs
					.iter()
					.map(|input| self.select_with_outer(input, span, outer))
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let first = statement
					.union_inputs
					.first()
					.ok_or_else(|| self.semantic("JOIN typing family has no inputs", span))?;
				// Reuse ordinary bounded analysis instead of cloning nested
				// typed trees.
				let mut wrapper = self.select_with_outer(first, span, outer)?;
				for input in &inputs[1..] {
					if statement.value && input.result.columns.len() != wrapper.result.columns.len()
					{
						return Err(self.semantic("JOIN VALUE result shape differs", span));
					}
					for (column, other) in
						wrapper.result.columns.iter_mut().zip(&input.result.columns)
					{
						for ty in [&column.ty, &other.ty] {
							crate::gql::live_lowering::admit_type_shape(
								ty,
								0,
								4,
								&mut |bytes, work| self.catalog.admit_analysis(bytes, work),
							)?;
						}
						column.ty = unify(column.ty.clone(), other.ty.clone());
					}
				}
				wrapper.union_purpose = crate::syn::SelectUnionPurpose::JoinedTypingVariants;
				wrapper.lexical_id = statement.lexical_id;
				wrapper.union_inputs = inputs;
				return Ok(wrapper);
			}
			let inputs = statement
				.union_inputs
				.iter()
				.map(|input| self.select_with_outer(input, span, None))
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let result = inputs
				.first()
				.ok_or_else(|| self.semantic("independent SELECT has no inputs", span))?
				.result
				.clone();
			return Ok(LogicalSelect {
				return_graph: statement
					.return_graph
					.as_deref()
					.map(|graph| self.select_return_graph(graph, &result, span))
					.transpose()?,
				union_inputs: inputs,
				union_purpose: crate::syn::SelectUnionPurpose::IndependentOutputs,
				lexical_id: statement.lexical_id,
				source: LogicalSelectSource {
					kind: LogicalSelectSourceKind::Empty {
						graph: statement
							.source
							.graph
							.as_ref()
							.map(|graph| self.graph_reference(graph, None))
							.transpose()?,
					},
					schema: Schema::default(),
					wildcard: Vec::new(),
					alias: "live".into(),
				},
				joins: Vec::new(),
				access: LogicalSelectAccess::Automatic,
				input_schema: Schema::default(),
				filter: None,
				split: Vec::new(),
				group: None,
				projection: Vec::new(),
				projection_paths: Vec::new(),
				result,
				omit: Vec::new(),
				fetch: Vec::new(),
				fetch_bridges: None,
				order: Vec::new(),
				start: None,
				limit: None,
				value: statement.value,
				only: statement.only,
				tempfiles: false,
				outer_columns: 0,
			});
		}
		if statement.union_purpose != crate::syn::SelectUnionPurpose::JoinedTypingVariants
			&& std::iter::once(&statement.source)
				.chain(statement.joins.iter().map(|join| &join.source))
				.any(|source| matches!(source.kind, SelectSourceKind::AtomUnion { .. }))
		{
			return Err(
				self.semantic("Atom union typing metadata requires an SDK JOIN family.", span)
			);
		}
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
			input_schema.origins.extend(
				source
					.schema
					.origins
					.iter()
					.map(|(column, origin)| (column + offset, origin.clone())),
			);
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
		let access = match &statement.access {
			None => LogicalSelectAccess::Automatic,
			Some(SelectAccessHint::NoIndex(_)) => LogicalSelectAccess::NoIndex,
			Some(SelectAccessHint::Indexes(indexes)) => LogicalSelectAccess::Indexes(
				indexes
					.iter()
					.map(|hint| LogicalIndexHint {
						source: hint.source.as_ref().map(|source| source.value.clone()),
						index: hint.index.value.clone(),
					})
					.collect(),
			),
		};
		let outer_columns = outer.map_or(0, |schema| schema.columns.len());
		if let Some(outer) = outer {
			let offset = input_schema.columns.len();
			input_schema.columns.extend(outer.columns.iter().cloned());
			input_schema.origins.extend(
				outer.origins.iter().map(|(column, origin)| (column + offset, origin.clone())),
			);
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
		let mut result_origins = BTreeMap::new();
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
							let output = columns.len();
							columns.push(Column {
								name: name.clone(),
								qualifier: None,
								ty: input_schema.columns[*index].ty.clone(),
							});
							if let Some(origin) = input_schema.origins.get(index) {
								result_origins.insert(output, origin.clone());
							}
						}
					}
					projection.push(LogicalSelectItem::Wildcard(wildcard.clone()));
				}
				SelectItem::Expr {
					expr,
					..
				}
				| SelectItem::PathExpr {
					expr,
					..
				} => {
					let typed = self.select_expr(expr, Some(&input_schema), true, 0)?;
					contains_aggregate |= typed_expr_contains_aggregate(&typed);
					let alias = match item {
						SelectItem::Expr {
							alias,
							..
						} => alias.as_ref(),
						SelectItem::PathExpr {
							alias,
							..
						} => alias.segments.first(),
						SelectItem::Wildcard(_) => None,
					};
					let name = alias
						.map(|alias| alias.value.clone())
						.or_else(|| select_expression_name(expr))
						.unwrap_or_else(|| format!("value_{index}").into_boxed_str());
					let output = columns.len();
					if let TypedExprKind::Column(column) = &typed.kind
						&& let Some(origin) = input_schema.origins.get(column)
					{
						result_origins.insert(output, origin.clone());
					}
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
					}
					| SelectItem::PathExpr {
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
			origins: result_origins,
		};
		let order = statement
			.order
			.iter()
			.map(|order| {
				let expression = if order.random {
					TypedExpr {
						kind: TypedExprKind::Constant(Value::None),
						ty: Type::Int,
						span: order.expression.span,
					}
				} else if let ExprKind::Name(name) = &order.expression.kind
					&& let Some(index) = result.index_of(name.value.as_ref())
				{
					TypedExpr {
						kind: TypedExprKind::Column(input_schema.columns.len() + index),
						ty: result.columns[index].ty.clone(),
						span: order.expression.span,
					}
				} else {
					if statement.fetch_bridges.is_some() {
						let mut schema = input_schema.clone();
						for column in &mut schema.columns {
							if result.index_of(&column.name).is_some() {
								column.name = "".into();
							}
						}
						let offset = schema.columns.len();
						schema.columns.extend(result.columns.iter().cloned());
						schema.origins.extend(
							result
								.origins
								.iter()
								.map(|(index, origin)| (offset + *index, origin.clone())),
						);
						self.select_expr(&order.expression, Some(&schema), true, 0)?
					} else {
						self.select_expr(&order.expression, Some(&input_schema), true, 0)?
					}
				};
				Ok(LogicalOrder {
					expression,
					random: order.random,
					collate: order.collate,
					numeric: order.numeric,
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
		self.validate_select_search(
			filter.as_ref(),
			&access,
			&joins,
			&split,
			group.as_deref(),
			&projection,
			&order,
			span,
		)?;
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
		let projection_paths = if statement
			.projection
			.iter()
			.any(|item| matches!(item, SelectItem::PathExpr { .. }))
		{
			let mut paths = Vec::new();
			for item in &statement.projection {
				let path = match item {
					SelectItem::PathExpr {
						alias,
						..
					} => {
						if alias.segments.is_empty()
							|| alias.segments.len() > 64
							|| alias
								.segments
								.iter()
								.any(|part| part.value.is_empty() || part.value.contains('\0'))
						{
							return Err(self.semantic(
								"SELECT output path must contain 1..64 nonempty names.",
								alias.span,
							));
						}
						Some(
							alias
								.segments
								.iter()
								.map(|part| part.value.clone())
								.collect::<Vec<_>>(),
						)
					}
					_ => None,
				};
				paths.push(path);
			}
			validate_projection_paths(&projection, &paths)
				.map_err(|reason| self.semantic(reason, span))?;
			paths
		} else {
			Vec::new()
		};
		Ok(LogicalSelect {
			return_graph: statement
				.return_graph
				.as_deref()
				.map(|graph| self.select_return_graph(graph, &result, span))
				.transpose()?,
			union_inputs: Vec::new(),
			union_purpose: crate::syn::SelectUnionPurpose::IndependentOutputs,
			lexical_id: statement.lexical_id,
			source,
			joins,
			access,
			input_schema,
			filter,
			split,
			group,
			projection,
			projection_paths,
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
			fetch_bridges: statement.fetch_bridges.as_ref().map(|clauses| {
				clauses
					.iter()
					.map(|clause| clause.as_ref().map(|clause| clause.alias.value.clone()))
					.collect()
			}),
			order,
			start,
			limit,
			value: statement.value,
			only: statement.only,
			tempfiles: statement.tempfiles,
			outer_columns,
		})
	}

	#[allow(
		clippy::too_many_arguments,
		reason = "search validation must see every SELECT expression context"
	)]
	fn validate_select_search(
		&self,
		filter: Option<&TypedExpr>,
		access: &LogicalSelectAccess,
		joins: &[LogicalSelectJoin],
		split: &[TypedExpr],
		group: Option<&[TypedExpr]>,
		projection: &[LogicalSelectItem],
		order: &[LogicalOrder],
		span: TextSpan,
	) -> FerrosGrynnResult<()> {
		let mut slots = BTreeMap::<u16, bool>::new();
		if let Some(filter) = filter {
			let mut knn_count = 0_usize;
			self.validate_knn_shape(filter, true, &mut knn_count)?;
			visit_typed_expression(filter, &mut |expression| match &expression.kind {
				TypedExprKind::Fulltext {
					op,
					right,
					definitions,
					..
				} => {
					if !typed_expression_is_statement_fixed(right)
						|| typed_expression_effect(right) > ClosureEffect::StableRead
					{
						return Err(self.semantic(
							"Fulltext query must be independent of source rows and may not mutate \
							 data.",
							right.span,
						));
					}
					let definition =
						self.selected_fulltext_definition(definitions, access, expression.span)?;
					if let FulltextOperator::Slot(slot) = op
						&& slots.insert(*slot, definition.highlights).is_some()
					{
						return Err(self.semantic(
							&format!("Fulltext match slot {slot} is duplicated in one statement."),
							expression.span,
						));
					}
					Ok(())
				}
				TypedExprKind::Knn {
					origin,
					k,
					ef,
					distance,
					right,
					definitions,
					..
				} => {
					for (name, value) in [("KNN K", k.as_ref()), ("KNN query", right.as_ref())] {
						if !typed_expression_is_statement_fixed(value)
							|| typed_expression_effect(value) > ClosureEffect::StableRead
						{
							return Err(self.semantic(
								&format!(
									"{name} must be independent of source rows and may not mutate \
									 data."
								),
								value.span,
							));
						}
					}
					if let Some(ef) = ef
						&& (!typed_expression_is_statement_fixed(ef)
							|| typed_expression_effect(ef) > ClosureEffect::StableRead)
					{
						return Err(self.semantic(
							"KNN EF must be independent of source rows and may not mutate data.",
							ef.span,
						));
					}
					let distance_input = match distance {
						LogicalKnnDistance::Minkowski(value) => {
							Some((value, ClosureEffect::StableRead))
						}
						LogicalKnnDistance::Callable(value) => Some((value, ClosureEffect::Pure)),
						_ => None,
					};
					if let Some((value, effect)) = distance_input
						&& (!typed_expression_is_statement_fixed(value)
							|| typed_expression_effect(value) > effect)
					{
						return Err(self.semantic(
							"KNN distance input must be row-independent and respect its effect \
							 contract.",
							value.span,
						));
					}
					self.validate_vector_definitions(
						origin,
						definitions,
						access,
						ef.is_some(),
						!matches!(filter.kind, TypedExprKind::Knn { .. }),
						expression.span,
					)
				}
				TypedExprKind::Builtin {
					function: Builtin::SearchScore(_) | Builtin::SearchHighlight(_),
					..
				} => {
					return Err(self.semantic(
						"search::score and search::highlight are valid only in SELECT projection \
						 or ORDER BY.",
						expression.span,
					));
				}
				_ => Ok(()),
			})?;
		}

		for join in joins {
			if let Some(predicate) = &join.predicate {
				self.reject_search_context(predicate, "JOIN ON")?;
			}
		}
		for expression in split {
			self.reject_search_context(expression, "SPLIT")?;
		}
		if let Some(group) = group {
			for expression in group {
				self.reject_search_context(expression, "GROUP BY")?;
			}
		}
		for item in projection {
			if let LogicalSelectItem::Expression {
				expression,
				..
			} = item
			{
				self.validate_search_result_expression(expression, &slots)?;
			}
		}
		for item in order {
			self.validate_search_result_expression(&item.expression, &slots)?;
		}
		if filter.is_none() && !slots.is_empty() {
			return Err(self.semantic("Fulltext slots require SELECT WHERE.", span));
		}
		Ok(())
	}

	fn validate_knn_shape(
		&self,
		expression: &TypedExpr,
		positive_conjunction: bool,
		count: &mut usize,
	) -> FerrosGrynnResult<()> {
		match &expression.kind {
			TypedExprKind::Knn {
				..
			} => {
				if !positive_conjunction {
					return Err(self.semantic(
						"KNN must be a positive top-level conjunction in SELECT WHERE.",
						expression.span,
					));
				}
				*count += 1;
				if *count > 1 {
					return Err(self.semantic(
						"One SELECT source may contain only one KNN predicate.",
						expression.span,
					));
				}
			}
			TypedExprKind::Binary {
				left,
				op: BinaryOperator::And,
				right,
			} if positive_conjunction => {
				self.validate_knn_shape(left, true, count)?;
				self.validate_knn_shape(right, true, count)?;
			}
			_ if typed_expression_contains_knn(expression) => {
				return Err(self.semantic(
					"KNN must be a positive top-level conjunction in SELECT WHERE.",
					expression.span,
				));
			}
			_ => {}
		}
		Ok(())
	}

	fn selected_fulltext_definition<'definition>(
		&self,
		definitions: &'definition [LogicalFulltextDefinition],
		access: &LogicalSelectAccess,
		span: TextSpan,
	) -> FerrosGrynnResult<&'definition LogicalFulltextDefinition> {
		let forced = match access {
			LogicalSelectAccess::Indexes(hints) => definitions
				.iter()
				.filter(|definition| {
					hints.iter().any(|hint| {
						hint.index.as_ref() == definition.name.as_str()
							&& hint.source.as_deref().is_none_or(|source| {
								source.eq_ignore_ascii_case(definition.source.as_ref())
							})
					})
				})
				.collect::<Vec<_>>(),
			LogicalSelectAccess::Automatic | LogicalSelectAccess::NoIndex => Vec::new(),
		};
		let definition = match forced.as_slice() {
			[definition] => {
				if definition.state != ClassIndexState::Ready {
					return Err(self.semantic(
						&format!("Forced Fulltext index `{}` is not Ready.", definition.name),
						span,
					));
				}
				*definition
			}
			[] => match definitions {
				[definition] => definition,
				_ => {
					return Err(self.semantic(
						"Multiple compatible Fulltext definitions require WITH INDEX selecting \
						 exactly one definition.",
						span,
					));
				}
			},
			_ => {
				return Err(self.semantic(
					"WITH INDEX selects more than one compatible Fulltext definition.",
					span,
				));
			}
		};
		Ok(definition)
	}

	fn validate_vector_definitions(
		&self,
		origin: &ColumnOrigin,
		definitions: &[LogicalVectorDefinition],
		access: &LogicalSelectAccess,
		requires_ann: bool,
		has_residual: bool,
		span: TextSpan,
	) -> FerrosGrynnResult<()> {
		let (forced, explicit_source) = match access {
			LogicalSelectAccess::Indexes(hints) => (
				definitions
					.iter()
					.filter(|definition| {
						hints.iter().any(|hint| {
							hint.index.as_ref() == definition.name.as_str()
								&& hint.source.as_deref().is_none_or(|source| {
									source.eq_ignore_ascii_case(origin.source.as_ref())
								})
						})
					})
					.collect::<Vec<_>>(),
				hints.iter().any(|hint| {
					hint.source
						.as_deref()
						.is_some_and(|source| source.eq_ignore_ascii_case(origin.source.as_ref()))
				}),
			),
			LogicalSelectAccess::Automatic | LogicalSelectAccess::NoIndex => (Vec::new(), false),
		};
		if forced.len() > 1 {
			return Err(
				self.semantic("WITH INDEX selects more than one compatible ANN definition.", span)
			);
		}
		if explicit_source && forced.is_empty() {
			return Err(self.semantic(
				"WITH INDEX does not contain a compatible ANN definition for the KNN source.",
				span,
			));
		}
		if let Some(definition) = forced.first() {
			if definition.state != ClassIndexState::Ready {
				return Err(self.semantic(
					&format!("Forced ANN index `{}` is not Ready.", definition.name),
					span,
				));
			}
			if has_residual {
				return Err(self.semantic(
					"Forced ANN does not support an additional row predicate in stage 23.",
					span,
				));
			}
		}
		if requires_ann {
			if matches!(access, LogicalSelectAccess::NoIndex) {
				return Err(self.semantic("KNN EF is incompatible with WITH NOINDEX.", span));
			}
			let ready = definitions
				.iter()
				.filter(|definition| definition.state == ClassIndexState::Ready)
				.count();
			if ready != 1 {
				return Err(
					self.semantic("KNN EF requires exactly one compatible Ready ANN index.", span)
				);
			}
		}
		Ok(())
	}

	fn reject_search_context(
		&self,
		expression: &TypedExpr,
		context: &str,
	) -> FerrosGrynnResult<()> {
		let mut found = None;
		visit_typed_expression(expression, &mut |expression| {
			if found.is_none() && typed_expression_is_search_related(expression) {
				found = Some(expression.span);
			}
			Ok::<(), FerrosGrynnError>(())
		})?;
		if let Some(span) = found {
			return Err(self.semantic(
				&format!(
					"Fulltext, KNN, score, and highlight expressions are not valid in {context}."
				),
				span,
			));
		}
		Ok(())
	}

	fn validate_search_result_expression(
		&self,
		expression: &TypedExpr,
		slots: &BTreeMap<u16, bool>,
	) -> FerrosGrynnResult<()> {
		visit_typed_expression(expression, &mut |expression| match &expression.kind {
			TypedExprKind::Fulltext {
				..
			}
			| TypedExprKind::Knn {
				..
			} => Err(self.semantic(
				"Fulltext and KNN predicates are valid only in SELECT WHERE.",
				expression.span,
			)),
			TypedExprKind::Builtin {
				function: Builtin::SearchScore(slot),
				..
			} if !slots.contains_key(slot) => Err(self.semantic(
				&format!("search::score references unknown Fulltext slot {slot}."),
				expression.span,
			)),
			TypedExprKind::Builtin {
				function: Builtin::SearchHighlight(slot),
				..
			} => match slots.get(slot) {
				None => Err(self.semantic(
					&format!("search::highlight references unknown Fulltext slot {slot}."),
					expression.span,
				)),
				Some(false) => Err(self.semantic(
					&format!(
						"search::highlight requires a HIGHLIGHTS Fulltext definition for slot \
						 {slot}."
					),
					expression.span,
				)),
				Some(true) => Ok(()),
			},
			_ => Ok(()),
		})
	}

	fn select_source(
		&self,
		source: &SelectSource,
		outer: Option<&Schema>,
	) -> FerrosGrynnResult<LogicalSelectSource> {
		let (kind, mut schema, wildcard, default_alias) = match &source.kind {
			SelectSourceKind::Empty => {
				let schema = super::execution::atom_source_schema(0);
				let wildcard = all_columns_wildcard(&schema);
				(
					LogicalSelectSourceKind::Empty {
						graph: source
							.graph
							.as_ref()
							.map(|graph| self.graph_reference(graph, outer))
							.transpose()?,
					},
					schema,
					wildcard,
					"live".into(),
				)
			}
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
			SelectSourceKind::AtomUnion {
				classes,
				typing,
			} => {
				if classes.windows(2).any(|pair| pair[0].value >= pair[1].value)
					|| typing.is_some_and(|ordinal| ordinal >= classes.len())
				{
					return Err(self.semantic("Atom union class choices are invalid.", source.span));
				}
				self.catalog.admit_analysis(
					classes
						.len()
						.checked_add(5)
						.and_then(|count| {
							count.checked_mul(
								std::mem::size_of::<ScanTarget>()
									+ std::mem::size_of::<Column>()
									+ 128,
							)
						})
						.ok_or_else(|| {
							FerrosGrynnError::operation_overflow("atom union metadata")
						})?,
					classes.len() as u64,
				)?;
				let mut missing = super::execution::atom_source_schema(0);
				for column in &mut missing.columns {
					column.ty = Type::None;
				}
				let mut names = missing
					.columns
					.iter()
					.map(|column| column.name.to_ascii_lowercase())
					.collect::<BTreeSet<_>>();
				let mut selected = None;
				let mut targets = Vec::with_capacity(classes.len());
				for (ordinal, class) in classes.iter().enumerate() {
					let schema = self.catalog.schema(class.value.as_ref())?;
					self.catalog.admit_analysis(
						schema
							.columns
							.len()
							.checked_mul(std::mem::size_of::<Column>() + 128)
							.ok_or_else(|| {
								FerrosGrynnError::operation_overflow("atom union schema")
							})?,
						schema.columns.len() as u64,
					)?;
					for column in &schema.columns {
						self.catalog.admit_analysis(
							column.name.len().checked_mul(2).ok_or_else(|| {
								FerrosGrynnError::operation_overflow("atom union field")
							})?,
							1,
						)?;
						if names.insert(column.name.to_ascii_lowercase()) {
							missing.columns.push(Column {
								name: column.name.clone(),
								qualifier: None,
								ty: Type::None,
							});
						}
					}
					if *typing == Some(ordinal) {
						let wildcard =
							atom_wildcard(&schema, self.catalog.schema_mode(class.value.as_ref())?);
						selected = Some((schema, wildcard));
					}
					self.catalog.admit_analysis(class.value.len(), 1)?;
					targets.push(ScanTarget::Class(class.value.clone()));
				}
				let tag_column = missing.columns.len();
				let (mut schema, wildcard) = selected.unwrap_or_else(|| {
					let wildcard = all_columns_wildcard(&missing);
					(missing, wildcard)
				});
				self.catalog.admit_analysis(
					tag_column
						.checked_add(1)
						.and_then(|count| count.checked_mul(std::mem::size_of::<Column>() + 64))
						.ok_or_else(|| {
							FerrosGrynnError::operation_overflow("atom union padding")
						})?,
					tag_column as u64,
				)?;
				while schema.columns.len() < tag_column {
					schema.columns.push(Column {
						name: format!("\0fg_pad_{}", schema.columns.len()).into(),
						qualifier: None,
						ty: Type::None,
					});
				}
				schema.columns.push(Column {
					name: "\0fg_class".into(),
					qualifier: None,
					ty: Type::Int,
				});
				schema.origins.clear();
				(
					LogicalSelectSourceKind::AtomUnion {
						targets,
						graph: source
							.graph
							.as_ref()
							.map(|graph| self.graph_reference(graph, outer))
							.transpose()?,
						tag_column,
						typing: *typing,
					},
					schema,
					wildcard,
					"live_union".into(),
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
				let schema = select.output_schema();
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
		let scope = match &kind {
			LogicalSelectSourceKind::Atoms {
				graph: None,
				..
			} => Some(Some(IndexScope::Base)),
			LogicalSelectSourceKind::Atoms {
				graph: Some(LogicalGraphReference::Static(graph)),
				..
			} => Some(Some(IndexScope::Effective {
				graph: graph.id(),
			})),
			LogicalSelectSourceKind::Atoms {
				graph: Some(LogicalGraphReference::Expression(_)),
				..
			} => Some(None),
			LogicalSelectSourceKind::Graphs {
				..
			}
			| LogicalSelectSourceKind::Empty {
				..
			}
			| LogicalSelectSourceKind::AtomUnion {
				..
			} => Some(None),
			LogicalSelectSourceKind::Derived(_) => None,
		};
		for origin in schema.origins.values_mut() {
			origin.source = alias.clone();
			if let Some(scope) = scope {
				origin.scope = scope;
			}
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
				if let Some(reads) = &self.slot_reads
					&& let Some(read) = reads.borrow_mut().get_mut(slot.id as usize)
				{
					*read = true;
				}
				(
					if slot.reactive {
						TypedExprKind::LiveSlot(slot.id)
					} else {
						TypedExprKind::Slot(slot.id)
					},
					slot.ty.clone(),
				)
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
					return self.pipeline_expression(
						expr,
						schema,
						allow_current_field,
						allow_aggregates,
						aggregate_depth,
					);
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
			ExprKind::Fulltext {
				left,
				op,
				right,
			} => {
				let left = self.expr_context(
					left,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				if left.ty != Type::String {
					return Err(self.semantic(
						"A Fulltext predicate requires a declared string field on the left.",
						expr.span,
					));
				}
				let origin = column_origin(schema, &left).ok_or_else(|| {
					self.semantic(
						"A Fulltext predicate must reference one declared source field.",
						expr.span,
					)
				})?;
				let right = self.expr_context(
					right,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				let valid_query = right.ty == Type::Any
					|| match op {
						FulltextOperator::Any | FulltextOperator::Slot(_) => {
							right.ty == Type::String
						}
						FulltextOperator::All | FulltextOperator::AnyOf => {
							matches!(&right.ty, Type::Array(item) if **item == Type::String)
						}
					};
				if !valid_query {
					return Err(self.semantic(
						"@@ and @n@ require a string query; @AND@ and @OR@ require array<string>.",
						right.span,
					));
				}
				let definitions = self.fulltext_definitions(origin, expr.span)?;
				(
					TypedExprKind::Fulltext {
						left: Box::new(left),
						op: *op,
						right: Box::new(right),
						definitions,
					},
					Type::Bool,
				)
			}
			ExprKind::Knn {
				left,
				k,
				search,
				right,
			} => {
				let left = self.expr_context(
					left,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				let Type::Vector(element) = &left.ty else {
					return Err(self.semantic(
						"A KNN predicate requires a declared vector field on the left.",
						expr.span,
					));
				};
				let origin = column_origin(schema, &left).ok_or_else(|| {
					self.semantic(
						"A KNN predicate must reference one declared source field.",
						expr.span,
					)
				})?;
				let k = self.expr_context(
					k,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				if !is_integer(&k.ty) {
					return Err(self.semantic("KNN K must be an integer.", k.span));
				}
				let right = self.expr_context(
					right,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				let element = *element;
				if right.ty != Type::Any && right.ty != Type::Vector(element) {
					return Err(self.semantic(
						"KNN field and query vector element types must match.",
						right.span,
					));
				}
				let (distance, ef) = self.knn_distance(
					search.as_ref(),
					element,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
					expr.span,
				)?;
				let definitions = self.vector_definitions(origin, element, &distance, expr.span)?;
				(
					TypedExprKind::Knn {
						left: Box::new(left),
						origin: origin.clone(),
						k: Box::new(k),
						ef,
						distance,
						right: Box::new(right),
						definitions,
					},
					Type::Bool,
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
				let ty = if matches!(base.ty, Type::Matcher) {
					if !matches!(field.value.to_ascii_lowercase().as_str(), "start" | "end") {
						return Err(self.semantic(
							"Matcher exposes only `start` and `end` fields.",
							field.span,
						));
					}
					Type::Option(Box::new(Type::Int))
				} else {
					Type::Any
				};
				(
					TypedExprKind::Field {
						base: Box::new(base),
						field: field.value.clone(),
						null_safe: *null_safe,
					},
					ty,
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
					Type::Vector(kind) => match kind {
						crate::values::VectorKind::F64
						| crate::values::VectorKind::F32
						| crate::values::VectorKind::F16 => Type::Float,
						crate::values::VectorKind::I64
						| crate::values::VectorKind::I32
						| crate::values::VectorKind::I16
						| crate::values::VectorKind::I8
						| crate::values::VectorKind::U8 => Type::Int,
					},
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
				argument_names,
				arguments,
			} => {
				if builtin_name(callee).is_some_and(|(namespace, name)| {
					namespace.eq_ignore_ascii_case("path") && name.eq_ignore_ascii_case("cost")
				}) && arguments.len() == 2
				{
					if *distinct
						|| type_argument.is_some()
						|| argument_names.iter().any(Option::is_some)
					{
						return Err(
							self.semantic("path::cost does not accept call modifiers.", expr.span)
						);
					}
					let cost_expr = &arguments[1];
					let Some(("fn", name)) = builtin_name(cost_expr) else {
						return Err(self.semantic(
							"path::cost requires a named PURE COST fn::name.",
							cost_expr.span,
						));
					};
					let cost = self.path_cost_function(&FunctionSignatureName {
						namespace: Identifier {
							value: "fn".into(),
							span: cost_expr.span,
							quoted: false,
						},
						name: Identifier {
							value: name.into(),
							span: cost_expr.span,
							quoted: false,
						},
						parameters: None,
					})?;
					let path = self.expr_context(
						&arguments[0],
						schema,
						allow_current_field,
						allow_aggregates,
						aggregate_depth,
					)?;
					self.validate_builtin_arguments(
						"path::cost",
						std::slice::from_ref(&path),
						expr.span,
					)?;
					return Ok(TypedExpr {
						kind: TypedExprKind::Builtin {
							function: Builtin::PathCostWith(cost),
							arguments: vec![path],
						},
						ty: Type::Number,
						span: expr.span,
					});
				}
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
					if argument_names.iter().any(Option::is_some) {
						return Err(self.semantic(
							"Aggregate functions do not accept named arguments.",
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
				if let Some((namespace, name)) = builtin_name(callee) {
					let key = format!(
						"{}::{}",
						namespace.to_ascii_lowercase(),
						name.to_ascii_lowercase()
					);
					if matches!(
						key.as_str(),
						"array::push" | "array::pop" | "array::clear" | "set::add" | "set::clear"
					) {
						if *distinct
							|| type_argument.is_some()
							|| argument_names.iter().any(Option::is_some)
						{
							return Err(self.semantic(
								"Mutable collection helpers do not accept DISTINCT, type \
								 arguments, or named arguments.",
								expr.span,
							));
						}
						if schema.is_some() {
							return Err(self.semantic(
								"Mutable collection helpers are not allowed in row-producing \
								 query expressions.",
								expr.span,
							));
						}
						let expected = if matches!(key.as_str(), "array::push" | "set::add") {
							2
						} else {
							1
						};
						if arguments.len() != expected {
							return Err(self.semantic(
								&format!("Function `{key}` expects {expected} arguments."),
								expr.span,
							));
						}
						let Some(Expr {
							kind: ExprKind::Parameter(binding),
							..
						}) = arguments.first()
						else {
							return Err(self.semantic(
								"The first mutable collection argument must be a direct \
								 `$binding`.",
								expr.span,
							));
						};
						let slot =
							self.slots.get(binding.value.as_ref()).cloned().ok_or_else(|| {
								self.semantic(
									&format!("Unknown parameter `${}`.", binding.value),
									binding.span,
								)
							})?;
						if !slot.mutable {
							return Err(self.semantic(
								"Execute parameters, durable parameters, loop bindings, and \
								 closure captures are immutable.",
								binding.span,
							));
						}
						let array_target =
							matches!(key.as_str(), "array::push" | "array::pop" | "array::clear");
						if array_target && !matches!(slot.ty, Type::Array(_) | Type::Any) {
							return Err(self.semantic(
								"Mutable array helper requires an array binding.",
								binding.span,
							));
						}
						if !array_target && !matches!(slot.ty, Type::Set(_) | Type::Any) {
							return Err(self.semantic(
								"Mutable set helper requires a set binding.",
								binding.span,
							));
						}
						let values = arguments[1..]
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
						if let (Some(value), Type::Array(item) | Type::Set(item)) =
							(values.first(), &slot.ty)
							&& !compatible(item, &value.ty)
						{
							return Err(self.semantic(
								&format!("Inserted value has type {}, expected {item}.", value.ty),
								value.span,
							));
						}
						let (function, ty) = match key.as_str() {
							"array::push" => (Builtin::ArrayPush(slot.id), Type::Int),
							"array::pop" => (Builtin::ArrayPop(slot.id), Type::Any),
							"array::clear" => (Builtin::ArrayClear(slot.id), Type::None),
							"set::add" => (Builtin::SetAdd(slot.id), Type::Bool),
							"set::clear" => (Builtin::SetClear(slot.id), Type::None),
							_ => {
								return Err(
									self.semantic("Unknown mutable collection helper.", expr.span)
								);
							}
						};
						return Ok(TypedExpr {
							kind: TypedExprKind::Builtin {
								function,
								arguments: values,
							},
							ty,
							span: expr.span,
						});
					}
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
				if let Some((namespace, name)) = builtin_name(callee) {
					if namespace.eq_ignore_ascii_case("fn") {
						if type_argument.is_some() {
							return Err(self.semantic(
								"Named functions do not accept call-site type arguments.",
								expr.span,
							));
						}
						self.named_function(name, argument_names, arguments, expr.span)?
					} else {
						if argument_names.iter().any(Option::is_some) {
							return Err(self.semantic(
								"Built-in functions do not accept named arguments.",
								expr.span,
							));
						}
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
				} else {
					if argument_names.iter().any(Option::is_some) {
						return Err(self.semantic(
							"Language closures do not accept named arguments.",
							expr.span,
						));
					}
					if type_argument.is_some() {
						return Err(self.semantic(
							"Language closures do not accept call-site type arguments.",
							expr.span,
						));
					}
					let callee = self.expr_context(
						callee,
						schema,
						allow_current_field,
						allow_aggregates,
						aggregate_depth,
					)?;
					let result = match &callee.ty {
						Type::Closure {
							parameters,
							result,
						} => {
							if parameters.len() != arguments.len() {
								return Err(self.semantic(
									&format!(
										"Closure expects {} argument(s), found {}.",
										parameters.len(),
										arguments.len()
									),
									expr.span,
								));
							}
							for (parameter, argument) in parameters.iter().zip(&arguments) {
								if !compatible(parameter, &argument.ty) {
									return Err(self.semantic(
										&format!(
											"Closure argument has type {}, expected {parameter}.",
											argument.ty
										),
										argument.span,
									));
								}
							}
							(**result).clone()
						}
						Type::Any => Type::Any,
						other => {
							return Err(self.semantic(
								&format!("Cannot call a value of type {other}."),
								callee.span,
							));
						}
					};
					(
						TypedExprKind::Invoke {
							callee: Box::new(callee),
							arguments,
						},
						result,
					)
				}
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
			ExprKind::MatchSubquery(_) => {
				return Err(self.semantic(
					"A MATCH query expression is valid only inside tv::match(...).",
					expr.span,
				));
			}
			ExprKind::Closure(closure) => {
				let closure = self.closure(closure, expr.span)?;
				let ty = Type::Closure {
					parameters: closure.parameters.iter().map(|(_, ty)| ty.clone()).collect(),
					result: Box::new(closure.result.clone()),
				};
				(TypedExprKind::Closure(Box::new(closure)), ty)
			}
		};
		Ok(TypedExpr {
			kind,
			ty,
			span: expr.span,
		})
	}

	fn fulltext_definitions(
		&self,
		origin: &ColumnOrigin,
		span: TextSpan,
	) -> FerrosGrynnResult<Vec<LogicalFulltextDefinition>> {
		let class = self.catalog.class_definition(origin.class.as_ref())?;
		let mut definitions = Vec::new();
		for index in class.indexes() {
			let Some(options) = index.fulltext_options() else {
				continue;
			};
			if index.fields().next().is_none_or(|field| field.as_str() != origin.field.as_ref())
				|| !index_scope_matches(index.scope(), origin.scope)
			{
				continue;
			}
			let analyzer = self
				.catalog
				.analyzer_by_id(options.analyzer())?
				.ok_or_else(|| self.semantic("Fulltext analyzer does not exist.", span))?;
			if analyzer.version() != options.analyzer_version() {
				return Err(
					self.semantic("Fulltext index captures a different analyzer version.", span)
				);
			}
			let analyzer_function = analyzer
				.function()
				.map(|(id, version)| {
					let function = self.catalog.function(id)?.ok_or_else(|| {
						self.semantic("Fulltext analyzer function does not exist.", span)
					})?;
					if function.version() != version || function.effect() != FunctionEffect::Pure {
						return Err(self.semantic(
							"Fulltext analyzer requires its exact PURE function version.",
							span,
						));
					}
					let key = NamedFunctionKey {
						id,
						version,
						schema_epoch: self.catalog.schema_epoch(),
						max_depth: function.max_depth(),
						effect: function.effect(),
					};
					if let Some(cache) = self.function_cache
						&& cache.cached_named(key).is_none()
					{
						let closure = compile_named_function(
							self.catalog,
							&function,
							&self.function_stack,
							&self.function_overrides,
							self.function_cache,
						)?;
						cache.cache_named_plan(&closure)?;
					}
					Ok(key)
				})
				.transpose()?;
			definitions.push(LogicalFulltextDefinition {
				class: origin.class.clone(),
				field: origin.field.clone(),
				source: origin.source.clone(),
				scope: origin.scope,
				index: index.id().ok_or_else(|| {
					self.semantic("Published Fulltext index has no IndexId.", span)
				})?,
				name: index.name().clone(),
				state: index.state(),
				analyzer,
				analyzer_function,
				k1: options.k1(),
				b: options.b(),
				highlights: options.highlights(),
			});
		}
		if definitions.is_empty() {
			return Err(self.semantic(
				"A Fulltext predicate requires a compatible Fulltext index definition.",
				span,
			));
		}
		Ok(definitions)
	}

	#[allow(
		clippy::too_many_arguments,
		reason = "KNN distance analysis shares the surrounding expression context"
	)]
	fn knn_distance(
		&self,
		search: Option<&KnnSearchArgument>,
		element: VectorKind,
		schema: Option<&Schema>,
		allow_current_field: bool,
		allow_aggregates: bool,
		aggregate_depth: u32,
		span: TextSpan,
	) -> FerrosGrynnResult<(LogicalKnnDistance, Option<Box<TypedExpr>>)> {
		let Some(search) = search else {
			return Ok((LogicalKnnDistance::Builtin(VectorDistance::Euclidean), None));
		};
		if let KnnSearchArgument::Ef(value) = search {
			let value = self.expr_context(
				value,
				schema,
				allow_current_field,
				allow_aggregates,
				aggregate_depth,
			)?;
			if !is_integer(&value.ty) {
				return Err(self.semantic("KNN EF must be an integer.", value.span));
			}
			return Ok((
				LogicalKnnDistance::Builtin(VectorDistance::Euclidean),
				Some(Box::new(value)),
			));
		}
		let KnnSearchArgument::Distance(distance) = search else {
			unreachable!("KNN search argument was exhausted above")
		};
		let distance = match distance {
			KnnDistance::Euclidean => LogicalKnnDistance::Builtin(VectorDistance::Euclidean),
			KnnDistance::Cosine => LogicalKnnDistance::Builtin(VectorDistance::Cosine),
			KnnDistance::Manhattan => LogicalKnnDistance::Builtin(VectorDistance::Manhattan),
			KnnDistance::InnerProduct => LogicalKnnDistance::Builtin(VectorDistance::InnerProduct),
			KnnDistance::CosineNormalized => {
				LogicalKnnDistance::Builtin(VectorDistance::CosineNormalized)
			}
			KnnDistance::Minkowski(exponent) => {
				let exponent = self.expr_context(
					exponent,
					schema,
					allow_current_field,
					allow_aggregates,
					aggregate_depth,
				)?;
				if !is_number(&exponent.ty) {
					return Err(self.semantic("MINKOWSKI exponent must be numeric.", exponent.span));
				}
				LogicalKnnDistance::Minkowski(Box::new(exponent))
			}
			KnnDistance::Custom(expression) => {
				if let Some((namespace, name)) = builtin_name(expression) {
					if !namespace.eq_ignore_ascii_case("fn") {
						return Err(self.semantic(
							"Custom KNN distance must be a fn:: function or closure.",
							expression.span,
						));
					}
					self.knn_named_distance(name, element, expression.span)?
				} else {
					let callable = self.expr_context(
						expression,
						schema,
						allow_current_field,
						allow_aggregates,
						aggregate_depth,
					)?;
					let expected = Type::Closure {
						parameters: vec![Type::Vector(element), Type::Vector(element)],
						result: Box::new(Type::Float),
					};
					if !compatible(&expected, &callable.ty) {
						return Err(self.semantic(
							"Custom KNN closure must accept two equal vector types and return \
							 float.",
							callable.span,
						));
					}
					LogicalKnnDistance::Callable(Box::new(callable))
				}
			}
		};
		if let LogicalKnnDistance::Callable(callable) = &distance
			&& let TypedExprKind::Closure(closure) = &callable.kind
			&& closure.effect != ClosureEffect::Pure
		{
			return Err(self.semantic("Custom KNN distance closure must be PURE.", span));
		}
		Ok((distance, None))
	}

	fn knn_named_distance(
		&self,
		name: &str,
		element: VectorKind,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalKnnDistance> {
		let expected = Type::Vector(element);
		let mut candidates = self
			.catalog
			.functions("fn", name)?
			.into_iter()
			.filter(|function| {
				function.effect() == FunctionEffect::Pure
					&& function.is_cost()
					&& function.result() == &Type::Float
					&& function.parameters().len() == 2
					&& function.parameters().all(|parameter| parameter.value_type() == &expected)
			})
			.collect::<Vec<_>>();
		if candidates.len() != 1 {
			return Err(self.semantic(
				"Custom KNN distance must resolve uniquely to PURE COST fn(vector<T>, vector<T>) \
				 -> float.",
				span,
			));
		}
		let function = candidates
			.pop()
			.ok_or_else(|| self.semantic("Custom KNN distance function does not exist.", span))?;
		let id = function
			.id()
			.ok_or_else(|| self.semantic("Custom KNN distance function is not published.", span))?;
		let key = NamedFunctionKey {
			id,
			version: function.version(),
			schema_epoch: self.catalog.schema_epoch(),
			max_depth: function.max_depth(),
			effect: function.effect(),
		};
		if let Some(cache) = self.function_cache
			&& cache.cached_named(key).is_none()
		{
			let closure = compile_named_function(
				self.catalog,
				&function,
				&self.function_stack,
				&self.function_overrides,
				self.function_cache,
			)?;
			cache.cache_named_plan(&closure)?;
		}
		Ok(LogicalKnnDistance::Named(key))
	}

	fn vector_definitions(
		&self,
		origin: &ColumnOrigin,
		element: VectorKind,
		distance: &LogicalKnnDistance,
		span: TextSpan,
	) -> FerrosGrynnResult<Vec<LogicalVectorDefinition>> {
		let class = self.catalog.class_definition(origin.class.as_ref())?;
		class
			.indexes()
			.filter_map(|index| {
				let options = match index.kind() {
					ClassIndexKind::Hnsw => index.hnsw_options().map(ClassIndexOptions::Hnsw),
					ClassIndexKind::DiskAnn => {
						index.disk_ann_options().map(ClassIndexOptions::DiskAnn)
					}
					_ => None,
				}?;
				let option_element = match options {
					ClassIndexOptions::Hnsw(options) => options.element(),
					ClassIndexOptions::DiskAnn(options) => options.element(),
					ClassIndexOptions::Scalar
					| ClassIndexOptions::Fulltext(_)
					| ClassIndexOptions::Geometry(_) => return None,
				};
				(index.fields().next().is_some_and(|field| field.as_str() == origin.field.as_ref())
					&& index_scope_matches(index.scope(), origin.scope)
					&& option_element == element
					&& vector_distance_matches(options, distance))
				.then(|| {
					Ok(LogicalVectorDefinition {
						class: origin.class.clone(),
						field: origin.field.clone(),
						source: origin.source.clone(),
						scope: origin.scope,
						index: index.id().ok_or_else(|| {
							self.semantic("Published vector index has no IndexId.", span)
						})?,
						name: index.name().clone(),
						kind: index.kind(),
						state: index.state(),
						options,
					})
				})
			})
			.collect()
	}

	fn closure(
		&self,
		closure: &crate::syn::ClosureExpr,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalClosure> {
		let mut captures = self.slots.values().map(|slot| slot.id).collect::<Vec<_>>();
		captures.sort_unstable();
		captures.dedup();
		let mut slots = self.slots.clone();
		for slot in slots.values_mut() {
			slot.mutable = false;
		}
		let mut analyzer = Self {
			source: self.source,
			catalog: self.catalog,
			slots,
			initial_slots: Vec::new(),
			next_slot: self.next_slot,
			scopes: vec![Vec::new()],
			variables: self.variables,
			statement_now: self.statement_now.clone(),
			function_stack: self.function_stack.clone(),
			function_overrides: self.function_overrides.clone(),
			function_cache: self.function_cache,
			slot_reads: self.slot_reads.clone(),
			live_checks: self.live_checks.clone(),
			live_scratch: self.live_scratch,
			current_function: self.current_function.clone(),
			current_function_has_explicit_result: self.current_function_has_explicit_result,
		};
		let mut parameters = Vec::with_capacity(closure.parameters.len());
		for parameter in &closure.parameters {
			let ty = parameter
				.declared_type
				.as_ref()
				.map(|ty| analyzer.type_name(ty))
				.transpose()?
				.unwrap_or(Type::Any);
			let slot = analyzer.declare_mutable(&parameter.name, ty.clone())?;
			parameters.push((slot, ty));
		}
		let (body, inferred, effect) = match &closure.body {
			crate::syn::ClosureBody::Expression(expression) => {
				let expression = analyzer.expr(expression, None)?;
				let effect = typed_expression_effect(&expression);
				let inferred = expression.ty.clone();
				(LogicalClosureBody::Expression(Box::new(expression)), inferred, effect)
			}
			crate::syn::ClosureBody::Block(block) => {
				let block = analyzer.block(block, 0, None)?.0;
				if let [
					LogicalBlockStatement {
						statement: LogicalStatement::Return {
							value,
						},
						timeout: None,
					},
				] = block.statements.as_slice()
				{
					(
						LogicalClosureBody::Expression(Box::new(value.clone())),
						value.ty.clone(),
						typed_expression_effect(value),
					)
				} else {
					let inferred = logical_block_result_type(&block);
					let effect = logical_block_effect(&block);
					(LogicalClosureBody::Block(block), inferred, effect)
				}
			}
		};
		let result = closure
			.result_type
			.as_ref()
			.map(|ty| analyzer.type_name(ty))
			.transpose()?
			.unwrap_or_else(|| inferred.clone());
		if closure.result_type.is_some() && !compatible(&result, &inferred) {
			return Err(self
				.semantic(&format!("Closure body has type {inferred}, expected {result}."), span));
		}
		Ok(LogicalClosure {
			parameters,
			captures,
			result,
			body,
			effect,
			slot_count: analyzer.next_slot as usize,
			retained_plan_bytes: std::mem::size_of::<LogicalClosure>().saturating_add(
				(span.len as usize).saturating_mul(std::mem::size_of::<TypedExpr>()),
			),
			named_function: None,
		})
	}

	/// Merges catalog definitions with the current function compilation scope.
	fn named_function_candidates(&self, name: &str) -> FerrosGrynnResult<Vec<Arc<SchemaFunction>>> {
		let mut candidates = self.catalog.functions("fn", name)?;
		for override_function in
			self.function_overrides.values().filter(|function| function.name().as_str() == name)
		{
			let signature = override_function
				.parameters()
				.map(SchemaFunctionParameter::value_type)
				.collect::<Vec<_>>();
			candidates.retain(|candidate| {
				!candidate
					.parameters()
					.map(SchemaFunctionParameter::value_type)
					.eq(signature.iter().copied())
			});
			candidates.push(Arc::clone(override_function));
		}
		if let Some(current) =
			self.current_function.as_ref().filter(|function| function.name().as_str() == name)
		{
			let current_signature =
				current.parameters().map(SchemaFunctionParameter::value_type).collect::<Vec<_>>();
			candidates.retain(|candidate| {
				!candidate
					.parameters()
					.map(SchemaFunctionParameter::value_type)
					.eq(current_signature.iter().copied())
			});
			candidates.push(Arc::clone(current));
		}
		Ok(candidates)
	}

	fn named_function(
		&self,
		name: &str,
		argument_names: &[Option<Identifier>],
		arguments: Vec<TypedExpr>,
		span: TextSpan,
	) -> FerrosGrynnResult<(TypedExprKind, Type)> {
		let candidates = self.named_function_candidates(name)?;
		let _signature_admission = admit_function_signatures(
			candidates
				.iter()
				.filter(|function| function.parameters().len() == arguments.len())
				.map(Arc::as_ref),
			self.function_cache,
		)?;
		let mut ranked = candidates
			.into_iter()
			.filter(|function| function.parameters().len() == arguments.len())
			.filter_map(|function| {
				let order = named_argument_order(&function, argument_names)?;
				let ranks = function
					.parameters()
					.zip(&order)
					.map(|(parameter, argument_index)| {
						function_argument_rank(
							parameter.value_type(),
							&arguments[*argument_index].ty,
						)
					})
					.collect::<Option<Vec<_>>>()?;
				let maximum = ranks.iter().copied().max().unwrap_or(0);
				let total = ranks.iter().map(|rank| u32::from(*rank)).sum::<u32>();
				Some(((maximum, total), function, order, ranks))
			})
			.collect::<Vec<_>>();
		ranked.sort_by_key(|(rank, function, _, _)| (*rank, function.id()));
		let Some((best_rank, function, order, ranks)) = ranked.first().cloned() else {
			return Err(self.semantic(
				&format!("No overload of `fn::{name}` accepts these argument types."),
				span,
			));
		};
		if ranked.get(1).is_some_and(|(rank, _, _, _)| *rank == best_rank) {
			return Err(self.semantic(&format!("Call to `fn::{name}` is ambiguous."), span));
		}
		let mut supplied = arguments.into_iter().map(Some).collect::<Vec<_>>();
		let mut arguments = Vec::with_capacity(supplied.len());
		for argument_index in order {
			let Some(argument) = supplied.get_mut(argument_index).and_then(Option::take) else {
				return Err(self.semantic("Invalid named function argument mapping.", span));
			};
			arguments.push(argument);
		}
		let arguments = arguments
			.into_iter()
			.zip(function.parameters())
			.zip(ranks)
			.map(|((argument, parameter), rank)| {
				if matches!(rank, 1 | 3) {
					TypedExpr {
						kind: TypedExprKind::Cast {
							target: parameter.value_type().clone(),
							expr: Box::new(argument),
						},
						ty: parameter.value_type().clone(),
						span,
					}
				} else {
					argument
				}
			})
			.collect::<Vec<_>>();
		let result = function.result().clone();
		let Some(id) = function.id() else {
			if function.max_depth().is_none() {
				return Err(
					self.semantic("Recursive functions require an explicit MAXDEPTH.", span)
				);
			}
			if !self.current_function_has_explicit_result {
				return Err(
					self.semantic("Recursive functions require an explicit return type.", span)
				);
			}
			let effect = match function.effect() {
				FunctionEffect::Pure => ClosureEffect::Pure,
				FunctionEffect::Stable => ClosureEffect::StableRead,
				FunctionEffect::Impure => ClosureEffect::DataMutation,
			};
			let placeholder = LogicalClosure {
				parameters: function
					.parameters()
					.enumerate()
					.map(|(slot, parameter)| (slot as u32, parameter.value_type().clone()))
					.collect(),
				captures: Vec::new(),
				result: result.clone(),
				body: LogicalClosureBody::Expression(Box::new(TypedExpr {
					kind: TypedExprKind::Constant(Value::None),
					ty: result.clone(),
					span,
				})),
				effect,
				slot_count: arguments.len(),
				retained_plan_bytes: 0,
				named_function: None,
			};
			return Ok((
				TypedExprKind::Invoke {
					callee: Box::new(TypedExpr {
						kind: TypedExprKind::Closure(Box::new(placeholder)),
						ty: Type::Closure {
							parameters: function
								.parameters()
								.map(|parameter| parameter.value_type().clone())
								.collect(),
							result: Box::new(result.clone()),
						},
						span,
					}),
					arguments,
				},
				result,
			));
		};
		let key = NamedFunctionKey {
			id,
			version: function.version(),
			schema_epoch: self.catalog.schema_epoch(),
			max_depth: function.max_depth(),
			effect: function.effect(),
		};
		if self.function_stack.contains(&id) {
			Self::validate_recursive_cycle(
				self.catalog,
				&self.function_stack,
				&function,
				span,
				self,
			)?;
			return Ok((
				TypedExprKind::NamedInvoke {
					function: key,
					arguments,
				},
				result,
			));
		}
		let closure = if let Some(cache) = self.function_cache {
			if cache.cached_named(key).is_none() {
				let closure = compile_named_function(
					self.catalog,
					&function,
					&self.function_stack,
					&self.function_overrides,
					self.function_cache,
				)?;
				cache.cache_named_plan(&closure)?;
			}
			return Ok((
				TypedExprKind::NamedInvoke {
					function: key,
					arguments,
				},
				result,
			));
		} else {
			compile_named_function(
				self.catalog,
				&function,
				&self.function_stack,
				&self.function_overrides,
				self.function_cache,
			)?
		};
		Ok((
			TypedExprKind::Invoke {
				callee: Box::new(TypedExpr {
					kind: TypedExprKind::Closure(Box::new(closure)),
					ty: Type::Closure {
						parameters: function
							.parameters()
							.map(|parameter| parameter.value_type().clone())
							.collect(),
						result: Box::new(result.clone()),
					},
					span,
				}),
				arguments,
			},
			result,
		))
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
			"geo::within" => (Builtin::GeoWithin, 2, Type::Bool),
			"geo::intersects" => (Builtin::GeoIntersects, 2, Type::Bool),
			"geo::distance" => (Builtin::GeoDistance, 2, Type::Float),
			"math::floor" => (Builtin::MathFloor, 1, Type::Float),
			"math::ceil" => (Builtin::MathCeil, 1, Type::Float),
			"string::upper" => (Builtin::StringUpper, 1, Type::String),
			"string::lower" => (Builtin::StringLower, 1, Type::String),
			"string::len" => (Builtin::StringLen, 1, Type::Int),
			"string::contains" => (Builtin::StringContains, 2, Type::Bool),
			"string::matches" => (Builtin::StringMatches, 2, Type::Bool),
			"string::slice" => (Builtin::StringSlice, 3, Type::String),
			"regex::matcher" => (Builtin::RegexMatcher, 2, Type::Matcher),
			"regex::next" => (Builtin::RegexNext, 1, Type::Bool),
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
			"graph::inner" => (Builtin::GraphInner, 1, Type::Any),
			"graph::level_path" => (Builtin::GraphLevelPath, 1, Type::Object),
			"path::atoms" => (Builtin::PathAtoms, 1, Type::Array(Box::new(Type::Any))),
			"path::nodes" => {
				(Builtin::PathNodes, 1, Type::Array(Box::new(Type::AtomOccurrence(AtomKind::Node))))
			}
			"path::edges" => {
				(Builtin::PathEdges, 1, Type::Array(Box::new(Type::AtomOccurrence(AtomKind::Edge))))
			}
			"path::length" => (Builtin::PathLength, 1, Type::Int),
			"path::cost" => (Builtin::PathCost, 1, Type::Number),
			"option::is_some" => (Builtin::OptionIsSome, 1, Type::Bool),
			"option::is_none" => (Builtin::OptionIsNone, 1, Type::Bool),
			"::uuid" => (Builtin::Uuid, 0, Type::Uuid),
			"::ulid" => (Builtin::Ulid, 0, Type::Ulid),
			"rand::id" => {
				if arguments.len() > 1 {
					return Err(
						self.semantic("Function `rand::id` expects zero or one argument.", span)
					);
				}
				(Builtin::RandId, arguments.len(), Type::String)
			}
			"search::analyze" => {
				let Some(TypedExpr {
					kind: TypedExprKind::Constant(Value::String(name)),
					..
				}) = arguments.first()
				else {
					return Err(self.semantic(
						"search::analyze requires a constant analyzer name as its first argument.",
						span,
					));
				};
				let analyzer = self.catalog.analyzer(name.as_str())?.ok_or_else(|| {
					self.semantic(&format!("Analyzer `{name}` does not exist."), span)
				})?;
				let function = analyzer
					.function()
					.map(|(id, version)| {
						let function = self.catalog.function(id)?.ok_or_else(|| {
							self.semantic("Analyzer function does not exist.", span)
						})?;
						if function.version() != version {
							return Err(self.semantic(
								"Analyzer function version does not match its durable definition.",
								span,
							));
						}
						let key = NamedFunctionKey {
							id,
							version,
							schema_epoch: self.catalog.schema_epoch(),
							max_depth: function.max_depth(),
							effect: function.effect(),
						};
						if let Some(cache) = self.function_cache
							&& cache.cached_named(key).is_none()
						{
							let closure = compile_named_function(
								self.catalog,
								&function,
								&self.function_stack,
								&self.function_overrides,
								self.function_cache,
							)?;
							cache.cache_named_plan(&closure)?;
						}
						Ok(key)
					})
					.transpose()?;
				(
					Builtin::SearchAnalyze {
						analyzer,
						function,
					},
					2,
					Type::Array(Box::new(Type::String)),
				)
			}
			"search::score" => {
				let slot = search_slot_argument(arguments.first(), span, |message, span| {
					self.semantic(message, span)
				})?;
				(Builtin::SearchScore(slot), 1, Type::Float)
			}
			"search::highlight" => {
				let slot = search_slot_argument(arguments.get(2), span, |message, span| {
					self.semantic(message, span)
				})?;
				(Builtin::SearchHighlight(slot), 3, Type::String)
			}
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
			"geo::within" | "geo::intersects" | "geo::distance" => {
				vec![BuiltinArgument::Geometry, BuiltinArgument::Geometry]
			}
			"string::upper" | "string::lower" | "string::len" => {
				vec![BuiltinArgument::String]
			}
			"string::contains" | "string::split" | "search::analyze" => {
				vec![BuiltinArgument::String, BuiltinArgument::String]
			}
			"search::score" => vec![BuiltinArgument::Integer],
			"search::highlight" => {
				vec![BuiltinArgument::String, BuiltinArgument::String, BuiltinArgument::Integer]
			}
			"string::matches" => vec![BuiltinArgument::String, BuiltinArgument::RegexOrString],
			"string::slice" => {
				vec![BuiltinArgument::String, BuiltinArgument::Integer, BuiltinArgument::Integer]
			}
			"regex::matcher" => vec![BuiltinArgument::String, BuiltinArgument::RegexOrString],
			"regex::next" => vec![BuiltinArgument::Matcher],
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
			"::uuid" | "::ulid" => Vec::new(),
			"rand::id" => {
				arguments.first().map_or_else(Vec::new, |_| vec![BuiltinArgument::Integer])
			}
			"meta::id" | "meta::class" => vec![BuiltinArgument::RecordOrAtom],
			"graph::inner" | "graph::level_path" => vec![BuiltinArgument::AtomOccurrence],
			"path::atoms" | "path::nodes" | "path::edges" | "path::length" | "path::cost" => {
				vec![BuiltinArgument::Path]
			}
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
			BinaryOperator::Inside
			| BinaryOperator::NotInside
			| BinaryOperator::Outside
			| BinaryOperator::Intersects => {
				if [left, right].into_iter().all(|ty| {
					matches!(ty, Type::Geometry | Type::Any | Type::None | Type::Null)
						|| matches!(ty, Type::Option(inner) if matches!(inner.as_ref(), Type::Geometry | Type::Any))
				}) {
					Ok(Type::Bool)
				} else {
					Err(self.semantic("Spatial operators require geometry operands.", span))
				}
			}
			BinaryOperator::AllInside | BinaryOperator::AnyInside | BinaryOperator::NoneInside => {
				let element = match left {
					Type::Array(element) | Type::Set(element) => element.as_ref(),
					Type::Any | Type::None | Type::Null => left,
					_ => {
						return Err(
							self.semantic("Spatial quantifiers require an array or set.", span)
						);
					}
				};
				if [element, right]
					.into_iter()
					.all(|ty| matches!(ty, Type::Geometry | Type::Any | Type::None | Type::Null))
				{
					Ok(Type::Bool)
				} else {
					Err(self.semantic(
						"Spatial quantifiers require geometry elements and a geometry right \
						 operand.",
						span,
					))
				}
			}
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
			Literal::Record {
				class,
				key,
			} => {
				let key = match key {
					RecordKey::Integer(value, _) => {
						value.parse::<i64>().map(RecordIdPart::Int).map_err(|_error| {
							self.semantic("Record integer ID is out of range.", span)
						})?
					}
					RecordKey::String(value, _) => RecordIdPart::String(value.as_ref().into()),
				};
				RecordId::new(class.as_ref(), key)
					.map(|record| Value::Record(Box::new(record)))
					.map_err(|error| {
						self.locate(error.with_phase(ExecutionPhase::SemanticAnalysis), span)
					})
			}
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
			SelectTarget::BoundRecord {
				class,
				parameter,
			} => {
				if self
					.slots
					.get(parameter.value.as_ref())
					.is_none_or(|slot| slot.mutable || slot.reactive)
				{
					return Err(self.semantic(
						"SDK point source requires an immutable literal slot.",
						parameter.span,
					));
				}
				let Some(Value::Record(record)) = self.variables.get(parameter.value.as_ref())
				else {
					return Err(self.semantic(
						"SDK point source requires a captured RecordId.",
						parameter.span,
					));
				};
				if record.class() != class.value.as_ref() {
					return Err(self.semantic(
						"SDK point source class disagrees with its RecordId.",
						class.span,
					));
				}
				Ok(ScanTarget::Record((**record).clone()))
			}
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
		self.type_name_value(name.value.as_ref(), name.span)
	}

	fn type_name_value(&self, value: &str, span: TextSpan) -> FerrosGrynnResult<Type> {
		let lower = value.to_ascii_lowercase();
		let ty = match lower.as_str() {
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
			"geometry" => Type::Geometry,
			"datetime" => Type::DateTime,
			"bytes" => Type::Bytes,
			"array" => Type::Array(Box::new(Type::Any)),
			"set" => Type::Set(Box::new(Type::Any)),
			"range" => Type::Range(Box::new(Type::Any)),
			"object" => Type::Object,
			"record" => Type::Record,
			"error" => Type::Error,
			"matcher" => Type::Matcher,
			_ => {
				let Some((outer, inner)) = lower.split_once('<') else {
					return Err(self.semantic(&format!("Unknown type `{value}`."), span));
				};
				let Some(inner) = inner.strip_suffix('>') else {
					return Err(self.semantic(&format!("Unknown type `{value}`."), span));
				};
				match outer {
					"array" => Type::Array(Box::new(self.type_name_value(inner, span)?)),
					"set" => Type::Set(Box::new(self.type_name_value(inner, span)?)),
					"range" => Type::Range(Box::new(self.type_name_value(inner, span)?)),
					"option" => Type::Option(Box::new(self.type_name_value(inner, span)?)),
					"vector" => Type::Vector(match inner {
						"f64" => crate::values::VectorKind::F64,
						"f32" => crate::values::VectorKind::F32,
						"f16" => crate::values::VectorKind::F16,
						"i64" => crate::values::VectorKind::I64,
						"i32" => crate::values::VectorKind::I32,
						"i16" => crate::values::VectorKind::I16,
						"i8" => crate::values::VectorKind::I8,
						"u8" => crate::values::VectorKind::U8,
						_ => {
							return Err(self.semantic(
								&format!("Unknown vector element type `{inner}`."),
								span,
							));
						}
					}),
					_ => return Err(self.semantic(&format!("Unknown type `{value}`."), span)),
				}
			}
		};
		Ok(ty)
	}

	fn declare(&mut self, name: &crate::syn::Identifier, ty: Type) -> FerrosGrynnResult<u32> {
		self.declare_with_mutability(name, ty, false)
	}

	fn declare_mutable(
		&mut self,
		name: &crate::syn::Identifier,
		ty: Type,
	) -> FerrosGrynnResult<u32> {
		self.declare_with_mutability(name, ty, true)
	}

	fn declare_with_mutability(
		&mut self,
		name: &crate::syn::Identifier,
		ty: Type,
		mutable: bool,
	) -> FerrosGrynnResult<u32> {
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
			mutable,
			reactive: false,
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

	fn validate_recursive_cycle(
		catalog: &dyn QueryCatalog,
		stack: &[FunctionId],
		function: &SchemaFunction,
		span: TextSpan,
		analyzer: &Analyzer<'_>,
	) -> FerrosGrynnResult<()> {
		let id = function
			.id()
			.ok_or_else(|| analyzer.semantic("Published function is missing FunctionId.", span))?;
		let start = stack.iter().position(|candidate| *candidate == id).unwrap_or(0);
		for candidate in &stack[start..] {
			let candidate = if let Some(overridden) = analyzer.function_overrides.get(candidate) {
				Arc::clone(overridden)
			} else if *candidate == id {
				Arc::new(function.clone())
			} else {
				catalog.function(*candidate)?.ok_or_else(|| {
					analyzer.semantic(
						"Recursive function dependency disappeared from the catalog.",
						span,
					)
				})?
			};
			if candidate.max_depth().is_none() {
				return Err(analyzer.semantic(
					"Every function in a recursive cycle requires an explicit MAXDEPTH.",
					span,
				));
			}
		}
		Ok(())
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

fn logical_block_result_type(block: &LogicalBlock) -> Type {
	block
		.statements
		.last()
		.map_or(Type::None, |statement| logical_statement_result_type(&statement.statement))
}

fn logical_statement_result_type(statement: &LogicalStatement) -> Type {
	match statement {
		LogicalStatement::Return {
			value,
		}
		| LogicalStatement::Expression {
			value,
		} => value.ty.clone(),
		LogicalStatement::If {
			branches,
			else_block,
		} => branches
			.iter()
			.map(|(_, block)| logical_block_result_type(block))
			.chain(else_block.iter().map(logical_block_result_type))
			.reduce(unify)
			.unwrap_or(Type::None),
		LogicalStatement::Try {
			body,
			catch,
		} => catch.as_ref().map_or_else(
			|| logical_block_result_type(body),
			|(_, catch)| unify(logical_block_result_type(body), logical_block_result_type(catch)),
		),
		_ => Type::Any,
	}
}

fn logical_block_effect(block: &LogicalBlock) -> ClosureEffect {
	block.statements.iter().fold(ClosureEffect::Pure, |effect, statement| {
		let timeout_effect = match &statement.timeout {
			Some(LogicalDuration::Parameter(value)) => typed_expression_effect(value),
			Some(LogicalDuration::Fixed(_)) | None => ClosureEffect::Pure,
		};
		effect.max(logical_statement_effect(&statement.statement)).max(timeout_effect)
	})
}

pub(in crate::gql) fn logical_statement_effect(statement: &LogicalStatement) -> ClosureEffect {
	match statement {
		LogicalStatement::Explain(explain) => {
			if explain.mode == crate::syn::ExplainMode::Analyze {
				ClosureEffect::SchemaMutation
			} else {
				ClosureEffect::StableRead
			}
		}
		LogicalStatement::Schema(statement) => match statement {
			LogicalSchemaStatement::AckLive {
				id,
				cursor,
				..
			} => ClosureEffect::DataMutation
				.max(typed_expression_effect(id))
				.max(typed_expression_effect(cursor)),
			LogicalSchemaStatement::InfoChangefeed {
				id,
				..
			} => ClosureEffect::StableRead.max(typed_expression_effect(id)),
			LogicalSchemaStatement::ShowChanges {
				target,
				since,
				limit,
				..
			} => {
				let target_effect = match target {
					crate::gql::ir::LogicalChangefeedTarget::Named(_) => ClosureEffect::Pure,
					crate::gql::ir::LogicalChangefeedTarget::Graph(value)
					| crate::gql::ir::LogicalChangefeedTarget::Id(value) => typed_expression_effect(value),
				};
				ClosureEffect::StableRead
					.max(target_effect)
					.max(typed_expression_effect(since))
					.max(limit.as_ref().map_or(ClosureEffect::Pure, typed_expression_effect))
			}
			statement if statement.is_read() => ClosureEffect::StableRead,
			_ => ClosureEffect::SchemaMutation,
		},
		LogicalStatement::Dml(_) | LogicalStatement::GraphDml(_) => ClosureEffect::DataMutation,
		LogicalStatement::BuildGraph(build) => {
			if build.name.is_some() {
				ClosureEffect::DataMutation
			} else {
				ClosureEffect::StableRead
			}
		}
		LogicalStatement::ReturnGraph(_) => ClosureEffect::StableRead,
		LogicalStatement::SetBinding {
			value,
			..
		} => typed_expression_effect(value),
		LogicalStatement::Let {
			value,
			..
		} => match value {
			LogicalLetValue::Live(_) => ClosureEffect::SchemaMutation,
			LogicalLetValue::Expression(value) => typed_expression_effect(value),
			LogicalLetValue::BuildGraph(build) => {
				if build.name.is_some() {
					ClosureEffect::DataMutation
				} else {
					ClosureEffect::StableRead
				}
			}
		},
		LogicalStatement::Return {
			value,
		}
		| LogicalStatement::Expression {
			value,
		}
		| LogicalStatement::Throw {
			value,
		} => typed_expression_effect(value),
		LogicalStatement::Sleep(_) => ClosureEffect::DataMutation,
		LogicalStatement::If {
			branches,
			else_block,
		} => branches
			.iter()
			.fold(ClosureEffect::Pure, |effect, (condition, block)| {
				effect.max(typed_expression_effect(condition)).max(logical_block_effect(block))
			})
			.max(else_block.as_ref().map_or(ClosureEffect::Pure, logical_block_effect)),
		LogicalStatement::For {
			iterable,
			body,
			..
		} => typed_expression_effect(iterable).max(logical_block_effect(body)),
		LogicalStatement::While {
			condition,
			body,
		} => typed_expression_effect(condition).max(logical_block_effect(body)),
		LogicalStatement::Try {
			body,
			catch,
		} => logical_block_effect(body).max(
			catch.as_ref().map_or(ClosureEffect::Pure, |(_, block)| logical_block_effect(block)),
		),
		LogicalStatement::AssertInvariants {
			..
		}
		| LogicalStatement::Match(_)
		| LogicalStatement::Select(_) => ClosureEffect::StableRead,
		LogicalStatement::Traverse(traverse) => {
			let source = match &traverse.source {
				LogicalTraverseSource::Expression(expression) => {
					typed_expression_effect(expression)
				}
				LogicalTraverseSource::Match(_) | LogicalTraverseSource::Select(_) => {
					ClosureEffect::StableRead
				}
			};
			traverse.steps.iter().fold(source, |effect, step| {
				let child = if step.match_plan.is_some() {
					ClosureEffect::StableRead
				} else {
					ClosureEffect::Pure
				};
				let arguments = step.arguments.iter().fold(ClosureEffect::Pure, |effect, value| {
					effect.max(typed_expression_effect(value))
				});
				let repeat = step.repeat.as_ref().map_or(ClosureEffect::Pure, |repeat| {
					typed_expression_effect(&repeat.body)
						.max(
							repeat
								.times
								.as_ref()
								.map_or(ClosureEffect::Pure, typed_expression_effect),
						)
						.max(
							repeat
								.until
								.as_ref()
								.map_or(ClosureEffect::Pure, typed_expression_effect),
						)
				});
				effect.max(child).max(arguments).max(repeat)
			})
		}
		LogicalStatement::Break | LogicalStatement::Continue => ClosureEffect::Pure,
	}
}

fn typed_expression_effect(expression: &TypedExpr) -> ClosureEffect {
	match &expression.kind {
		TypedExprKind::Traversal(traverse) => {
			let source = match &traverse.source {
				LogicalTraverseSource::Expression(expression) => {
					typed_expression_effect(expression)
				}
				LogicalTraverseSource::Match(_) | LogicalTraverseSource::Select(_) => {
					ClosureEffect::StableRead
				}
			};
			traverse.steps.iter().fold(source, |effect, step| {
				let child = if step.match_plan.is_some() {
					ClosureEffect::StableRead
				} else {
					ClosureEffect::Pure
				};
				let arguments = step.arguments.iter().fold(ClosureEffect::Pure, |effect, value| {
					effect.max(typed_expression_effect(value))
				});
				let repeat = step.repeat.as_ref().map_or(ClosureEffect::Pure, |repeat| {
					typed_expression_effect(&repeat.body)
						.max(
							repeat
								.times
								.as_ref()
								.map_or(ClosureEffect::Pure, typed_expression_effect),
						)
						.max(
							repeat
								.until
								.as_ref()
								.map_or(ClosureEffect::Pure, typed_expression_effect),
						)
				});
				effect.max(child).max(arguments).max(repeat)
			})
		}
		TypedExprKind::LiveSlot(_) => ClosureEffect::StableRead,
		TypedExprKind::Subquery {
			..
		} => ClosureEffect::StableRead,
		TypedExprKind::Builtin {
			function: Builtin::GraphInner | Builtin::GraphLevelPath | Builtin::TimeNow(_),
			..
		} => ClosureEffect::StableRead,
		TypedExprKind::Builtin {
			function:
				Builtin::Uuid
				| Builtin::Ulid
				| Builtin::RandId
				| Builtin::RegexMatcher
				| Builtin::RegexNext
				| Builtin::ArrayPush(_)
				| Builtin::ArrayPop(_)
				| Builtin::ArrayClear(_)
				| Builtin::SetAdd(_)
				| Builtin::SetClear(_),
			..
		} => ClosureEffect::DataMutation,
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => values
			.iter()
			.fold(ClosureEffect::Pure, |effect, value| effect.max(typed_expression_effect(value))),
		TypedExprKind::Object(fields) => {
			fields.iter().fold(ClosureEffect::Pure, |effect, (_, value)| {
				effect.max(typed_expression_effect(value))
			})
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => start
			.iter()
			.chain(end.iter())
			.fold(ClosureEffect::Pure, |effect, value| effect.max(typed_expression_effect(value))),
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
		} => typed_expression_effect(expr),
		TypedExprKind::Binary {
			left,
			right,
			..
		}
		| TypedExprKind::Index {
			base: left,
			index: right,
		}
		| TypedExprKind::Filter {
			base: left,
			predicate: right,
		} => typed_expression_effect(left).max(typed_expression_effect(right)),
		TypedExprKind::Fulltext {
			left,
			right,
			..
		} => ClosureEffect::StableRead
			.max(typed_expression_effect(left))
			.max(typed_expression_effect(right)),
		TypedExprKind::Knn {
			left,
			k,
			ef,
			distance,
			right,
			..
		} => {
			let distance = match distance {
				LogicalKnnDistance::Minkowski(value) | LogicalKnnDistance::Callable(value) => {
					typed_expression_effect(value)
				}
				LogicalKnnDistance::Builtin(_) | LogicalKnnDistance::Named(_) => {
					ClosureEffect::Pure
				}
			};
			ClosureEffect::StableRead
				.max(typed_expression_effect(left))
				.max(typed_expression_effect(k))
				.max(ef.as_deref().map_or(ClosureEffect::Pure, typed_expression_effect))
				.max(distance)
				.max(typed_expression_effect(right))
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => typed_expression_effect(condition)
			.max(typed_expression_effect(then_value))
			.max(typed_expression_effect(else_value)),
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => operand
			.iter()
			.chain(else_value.iter())
			.fold(ClosureEffect::Pure, |effect, value| effect.max(typed_expression_effect(value)))
			.max(branches.iter().fold(ClosureEffect::Pure, |effect, (condition, value)| {
				effect.max(typed_expression_effect(condition)).max(typed_expression_effect(value))
			})),
		TypedExprKind::Builtin {
			arguments,
			..
		} => arguments
			.iter()
			.fold(ClosureEffect::Pure, |effect, value| effect.max(typed_expression_effect(value))),
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => arguments
			.iter()
			.fold(ClosureEffect::Pure, |effect, value| effect.max(typed_expression_effect(value)))
			.max(match &callee.kind {
				TypedExprKind::Closure(closure) => closure.effect,
				_ => ClosureEffect::StableRead,
			}),
		TypedExprKind::NamedInvoke {
			function,
			arguments,
		} => arguments
			.iter()
			.fold(ClosureEffect::Pure, |effect, value| effect.max(typed_expression_effect(value)))
			.max(match function.effect {
				FunctionEffect::Pure => ClosureEffect::Pure,
				FunctionEffect::Stable => ClosureEffect::StableRead,
				FunctionEffect::Impure => ClosureEffect::DataMutation,
			}),
		TypedExprKind::Aggregate {
			argument,
			..
		} => argument
			.as_ref()
			.map_or(ClosureEffect::Pure, |argument| typed_expression_effect(argument)),
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_)
		| TypedExprKind::Closure(_) => ClosureEffect::Pure,
	}
}

fn search_slot_argument(
	argument: Option<&TypedExpr>,
	span: TextSpan,
	error: impl Fn(&str, TextSpan) -> FerrosGrynnError,
) -> FerrosGrynnResult<u16> {
	let Some(TypedExpr {
		kind: TypedExprKind::Constant(Value::Number(number)),
		..
	}) = argument
	else {
		return Err(error("Search match slot must be a constant integer.", span));
	};
	let slot = u16::try_from(number.clone())
		.map_err(|_| error("Search match slot must be in 1..=65535.", span))?;
	if slot == 0 {
		return Err(error("Search match slot must be in 1..=65535.", span));
	}
	Ok(slot)
}

#[derive(Clone, Copy)]
enum BuiltinArgument {
	Any,
	Geometry,
	Number,
	Integer,
	String,
	RegexOrString,
	Matcher,
	DateTime,
	Duration,
	Array,
	Set,
	Path,
	RecordOrAtom,
	AtomOccurrence,
}

impl BuiltinArgument {
	fn accepts(self, ty: &Type) -> bool {
		matches!(ty, Type::Any)
			|| match self {
				Self::Any => true,
				Self::Geometry => {
					matches!(ty, Type::Geometry | Type::None | Type::Null)
						|| matches!(ty, Type::Option(inner) if matches!(inner.as_ref(), Type::Geometry | Type::Any))
				}
				Self::Number => is_number(ty),
				Self::Integer => is_integer(ty),
				Self::String => matches!(ty, Type::String),
				Self::RegexOrString => matches!(ty, Type::Regex | Type::String),
				Self::Matcher => matches!(ty, Type::Matcher),
				Self::DateTime => matches!(ty, Type::DateTime),
				Self::Duration => matches!(ty, Type::Duration),
				Self::Array => matches!(ty, Type::Array(_)),
				Self::Set => matches!(ty, Type::Set(_)),
				Self::Path => matches!(ty, Type::Path),
				Self::RecordOrAtom => {
					matches!(ty, Type::Record | Type::Atom(_) | Type::AtomOccurrence(_))
				}
				Self::AtomOccurrence => matches!(ty, Type::AtomOccurrence(_)),
			}
	}

	const fn name(self) -> &'static str {
		match self {
			Self::Any => "any",
			Self::Geometry => "geometry",
			Self::Number => "number",
			Self::Integer => "integer",
			Self::String => "string",
			Self::RegexOrString => "regex or string",
			Self::Matcher => "matcher",
			Self::DateTime => "datetime",
			Self::Duration => "duration",
			Self::Array => "array",
			Self::Set => "set",
			Self::Path => "path",
			Self::RecordOrAtom => "record or atom",
			Self::AtomOccurrence => "atom occurrence",
		}
	}
}

fn keep_levelpath_enabled(
	clause: Option<&crate::syn::KeepLevelPathClause>,
	allowed_alias: Option<&crate::syn::Identifier>,
	error: impl Fn(&str, TextSpan) -> FerrosGrynnError,
) -> FerrosGrynnResult<bool> {
	let Some(clause) = clause else {
		return Ok(false);
	};
	for alias in &clause.aliases {
		if allowed_alias.is_none_or(|allowed| allowed.value != alias.value) {
			return Err(error(
				"KEEP LEVELPATH FOR may name only an occurrence mutated by this statement.",
				alias.span,
			));
		}
	}
	Ok(true)
}

fn validate_reserved_levelpath_data(
	data: Option<&DmlData>,
	error: impl Fn(&str, TextSpan) -> FerrosGrynnError,
) -> FerrosGrynnResult<()> {
	let fields: Box<dyn Iterator<Item = &crate::syn::Identifier> + '_> = match data {
		Some(DmlData::Set(assignments)) => Box::new(assignments.iter().map(|value| &value.field)),
		Some(DmlData::Unset(fields) | DmlData::Reset(fields)) => Box::new(fields.iter()),
		_ => Box::new(std::iter::empty()),
	};
	for field in fields {
		if field.value.eq_ignore_ascii_case(LEVEL_PATH_PROPERTY) {
			return Err(error(
				"`levelpath` is a system-managed graph-local property; use KEEP LEVELPATH.",
				field.span,
			));
		}
	}
	Ok(())
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
		ExprKind::Fulltext {
			left,
			right,
			..
		} => {
			collect_expr_names(left, names);
			collect_expr_names(right, names);
		}
		ExprKind::Knn {
			left,
			k,
			search,
			right,
		} => {
			collect_expr_names(left, names);
			collect_expr_names(k, names);
			if let Some(search) = search {
				match search {
					KnnSearchArgument::Ef(value) => collect_expr_names(value, names),
					KnnSearchArgument::Distance(
						KnnDistance::Minkowski(value) | KnnDistance::Custom(value),
					) => collect_expr_names(value, names),
					KnnSearchArgument::Distance(_) => {}
				}
			}
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
				}
				| crate::syn::SelectItem::PathExpr {
					expr,
					..
				} = item
				{
					collect_expr_names(expr, names);
				}
			}
			if let Some(graph) = &select.return_graph {
				for expr in std::iter::once(&graph.nodes)
					.chain(std::iter::once(&graph.edges))
					.chain(graph.root.iter())
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
		ExprKind::MatchSubquery(_)
		| ExprKind::Closure(_)
		| ExprKind::Literal(_)
		| ExprKind::Parameter(_) => {}
	}
}

fn builtin_name(callee: &Expr) -> Option<(&str, &str)> {
	if let ExprKind::Name(name) = &callee.kind {
		return Some(("", name.value.as_ref()));
	}
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

pub(in crate::gql) fn compile_named_function(
	catalog: &dyn QueryCatalog,
	function: &crate::datastore::SchemaFunction,
	stack: &[FunctionId],
	overrides: &BTreeMap<FunctionId, Arc<SchemaFunction>>,
	function_cache: Option<&LanguageClosureState>,
) -> FerrosGrynnResult<LogicalClosure> {
	let _signature_admission =
		admit_function_signatures(std::iter::once(function), function_cache)?;
	let _admission = function_cache
		.map(|cache| cache.admit_compile(function_compile_source_bytes(function)?))
		.transpose()?;
	let parameters = function
		.parameters()
		.map(|parameter| {
			Ok(format!(
				"$`{}`: {}",
				parameter.name().as_str().replace('`', "``"),
				function_source_type(parameter.value_type())?
			))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?
		.join(", ");
	let source = format!(
		"| {parameters} | -> {} {}",
		function_source_type(function.result())?,
		function.body()
	);
	let program = crate::syn::parse(&source, crate::syn::ParserSettings::default())?;
	let Some(crate::syn::Statement {
		kind: StatementKind::Expression(Expr {
			kind: ExprKind::Closure(closure),
			span,
		}),
		..
	}) = program.statements.first()
	else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"stored function body did not parse as a closure",
		));
	};
	let variables = Object::new();
	let mut analyzer = Analyzer::new(&source, catalog, &variables, &BTreeSet::new());
	analyzer.function_cache = function_cache;
	analyzer.function_overrides.clone_from(overrides);
	analyzer.function_stack.extend_from_slice(stack);
	let id = function.id().ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("published function is missing FunctionId")
	})?;
	analyzer.function_overrides.insert(id, Arc::new(function.clone()));
	analyzer.function_stack.push(id);
	let mut closure = analyzer.closure(closure, *span)?;
	let allowed = match function.effect() {
		FunctionEffect::Pure => ClosureEffect::Pure,
		FunctionEffect::Stable => ClosureEffect::StableRead,
		FunctionEffect::Impure => ClosureEffect::DataMutation,
	};
	if closure.effect > allowed {
		return Err(FerrosGrynnError::invalid_storage_file(
			"stored function body exceeds its declared effect",
		));
	}
	closure.named_function = Some(NamedFunctionKey {
		id,
		version: function.version(),
		schema_epoch: catalog.schema_epoch(),
		max_depth: function.max_depth(),
		effect: function.effect(),
	});
	Ok(closure)
}

/// Bounded embeddings inspect borrowed signature shapes before ranking,
/// recursive rendering and compiler-owned copies; no resolver is duplicated.
fn admit_function_signatures<'function, 'cache>(
	functions: impl Iterator<Item = &'function SchemaFunction>,
	cache: Option<&'cache LanguageClosureState>,
) -> FerrosGrynnResult<Option<super::closure::CompileReservation<'cache>>> {
	let Some(cache) = cache.filter(|cache| cache.is_compiler_bounded()) else {
		return Ok(None);
	};
	let maximum_work = cache.remaining_compile_work();
	let mut extent = 0_usize;
	let mut work_used = 0_u64;
	let mut admit = |bytes: usize, work: u64| -> FerrosGrynnResult<()> {
		cache.check_compile()?;
		extent = extent
			.checked_add(bytes)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("PURE signature extent"))?;
		work_used = work_used
			.checked_add(work)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("PURE compiler work"))?;
		if let Some(limit) = maximum_work
			&& work_used > limit
		{
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"PURE compiler work",
				limit,
			));
		}
		Ok(())
	};
	for function in functions {
		for parameter in function.parameters() {
			admit(
				parameter
					.name()
					.len()
					.checked_mul(2)
					.and_then(|bytes| bytes.checked_add(128))
					.ok_or_else(|| {
						FerrosGrynnError::operation_overflow("PURE parameter name extent")
					})?,
				u64::try_from(parameter.name().len()).ok().and_then(|bytes| bytes.checked_add(1))
					.ok_or_else(|| FerrosGrynnError::operation_overflow("PURE parameter name work"))?,
			)?;
			super::live_lowering::admit_type_shape(parameter.value_type(), 0, 1, &mut admit)?;
		}
		super::live_lowering::admit_type_shape(function.result(), 0, 1, &mut admit)?;
	}
	cache.admit_compile_metadata(extent, work_used).map(Some)
}

/// Counts a conservative source extent without constructing the signature.
fn function_compile_source_bytes(function: &SchemaFunction) -> FerrosGrynnResult<usize> {
	let mut bytes = function.body().len().checked_add(32);
	for parameter in function.parameters() {
		bytes = bytes
			.and_then(|bytes| parameter.name().len().checked_mul(2)?.checked_add(bytes))
			.and_then(|bytes| bytes.checked_add(16))
			.and_then(|bytes| {
				bytes.checked_add(function_type_source_bytes(parameter.value_type()))
			});
	}
	bytes
		.and_then(|bytes| bytes.checked_add(function_type_source_bytes(function.result())))
		.ok_or_else(|| FerrosGrynnError::operation_overflow("function source extent"))
}

/// Parser-supported nested types contain one child and require no traversal
/// heap.
fn function_type_source_bytes(mut value: &Type) -> usize {
	let mut bytes = 32_usize;
	while let Type::Array(inner) | Type::Set(inner) | Type::Range(inner) | Type::Option(inner) =
		value
	{
		bytes = bytes.saturating_add(8);
		value = inner;
	}
	bytes
}

fn function_source_type(value: &Type) -> FerrosGrynnResult<String> {
	let name = match value {
		Type::Any => "any".to_owned(),
		Type::None => "none".to_owned(),
		Type::Null => "null".to_owned(),
		Type::Bool => "bool".to_owned(),
		Type::Number => "number".to_owned(),
		Type::Int => "int".to_owned(),
		Type::Float => "float".to_owned(),
		Type::Decimal => "decimal".to_owned(),
		Type::String => "string".to_owned(),
		Type::Duration => "duration".to_owned(),
		Type::DateTime => "datetime".to_owned(),
		Type::Bytes => "bytes".to_owned(),
		Type::Array(item) => format!("array<{}>", function_source_type(item)?),
		Type::Set(item) => format!("set<{}>", function_source_type(item)?),
		Type::Range(item) => format!("range<{}>", function_source_type(item)?),
		Type::Option(item) => format!("option<{}>", function_source_type(item)?),
		Type::Vector(kind) => format!("vector<{}>", match kind {
			crate::values::VectorKind::F64 => "f64",
			crate::values::VectorKind::F32 => "f32",
			crate::values::VectorKind::F16 => "f16",
			crate::values::VectorKind::I64 => "i64",
			crate::values::VectorKind::I32 => "i32",
			crate::values::VectorKind::I16 => "i16",
			crate::values::VectorKind::I8 => "i8",
			crate::values::VectorKind::U8 => "u8",
		}),
		Type::Object => "object".to_owned(),
		Type::Record => "record".to_owned(),
		Type::Error => "error".to_owned(),
		Type::Matcher => "matcher".to_owned(),
		_ => {
			return Err(FerrosGrynnError::invalid_storage_file(
				"stored function signature contains a type unsupported by the source parser",
			));
		}
	};
	Ok(name)
}

fn function_argument_rank(expected: &Type, actual: &Type) -> Option<u8> {
	if expected == actual {
		return Some(0);
	}
	match (expected, actual) {
		(Type::Decimal, Type::Int) => Some(1),
		(Type::Number, Type::Int | Type::Float | Type::Decimal) => Some(2),
		(Type::Any, _) => Some(2),
		(Type::Array(expected), Type::Array(actual))
		| (Type::Set(expected), Type::Set(actual))
		| (Type::Range(expected), Type::Range(actual)) => function_argument_rank(expected, actual),
		(Type::Float, Type::Int | Type::Decimal) => Some(3),
		(Type::Option(_), Type::None | Type::Null) => Some(0),
		(Type::Option(inner), actual) => function_argument_rank(inner, actual),
		_ => None,
	}
}

fn named_argument_order(
	function: &SchemaFunction,
	argument_names: &[Option<Identifier>],
) -> Option<Vec<usize>> {
	let parameters = function.parameters().collect::<Vec<_>>();
	if parameters.len() != argument_names.len() {
		return None;
	}
	let mut order = vec![usize::MAX; parameters.len()];
	let mut next_positional = 0_usize;
	for (argument_index, argument_name) in argument_names.iter().enumerate() {
		let parameter_index = if let Some(argument_name) = argument_name {
			parameters.iter().position(|parameter| {
				parameter.name().as_str().eq_ignore_ascii_case(argument_name.value.as_ref())
			})?
		} else {
			while next_positional < order.len() && order[next_positional] != usize::MAX {
				next_positional += 1;
			}
			if next_positional == order.len() {
				return None;
			}
			let parameter_index = next_positional;
			next_positional += 1;
			parameter_index
		};
		if order[parameter_index] != usize::MAX {
			return None;
		}
		order[parameter_index] = argument_index;
	}
	order.iter().all(|index| *index != usize::MAX).then_some(order)
}

fn pipeline_parts<'expression>(
	expression: &'expression Expr,
	steps: &mut Vec<&'expression Expr>,
) -> &'expression Expr {
	let ExprKind::Binary {
		left,
		op: BinaryOperator::Pipeline,
		right,
	} = &expression.kind
	else {
		return expression;
	};
	let source = pipeline_parts(left, steps);
	steps.push(right);
	source
}

struct ParsedRepeat {
	arguments: Vec<Expr>,
	modifiers: Vec<ParsedRepeatModifier>,
}

struct ParsedRepeatModifier {
	name: Box<str>,
	arguments: Vec<Expr>,
	span: TextSpan,
}

fn parse_repeat_syntax(expression: &Expr) -> Option<ParsedRepeat> {
	let ExprKind::Call {
		callee,
		type_argument: None,
		distinct: false,
		argument_names,
		arguments,
	} = &expression.kind
	else {
		return None;
	};
	if argument_names.iter().any(Option::is_some) {
		return None;
	}
	if let Some((namespace, name)) = builtin_name(callee)
		&& namespace.eq_ignore_ascii_case("tv")
		&& name.eq_ignore_ascii_case("repeat")
	{
		return Some(ParsedRepeat {
			arguments: arguments.clone(),
			modifiers: Vec::new(),
		});
	}
	let ExprKind::Field {
		base,
		field,
		null_safe: false,
	} = &callee.kind
	else {
		return None;
	};
	let mut repeat = parse_repeat_syntax(base)?;
	repeat.modifiers.push(ParsedRepeatModifier {
		name: field.value.clone(),
		arguments: arguments.clone(),
		span: expression.span,
	});
	Some(repeat)
}

fn ensure_traversal_closure(
	analyzer: &Analyzer<'_>,
	expression: &TypedExpr,
	allow_mutation: bool,
) -> FerrosGrynnResult<()> {
	if !matches!(expression.ty, Type::Closure { .. } | Type::Any) {
		return Err(analyzer.semantic(
			&format!("Traversal closure position received {}.", expression.ty),
			expression.span,
		));
	}
	let TypedExprKind::Closure(closure) = &expression.kind else {
		return Ok(());
	};
	let allowed = match closure.effect {
		ClosureEffect::Pure | ClosureEffect::StableRead => true,
		ClosureEffect::DataMutation => allow_mutation,
		ClosureEffect::SchemaMutation => false,
	};
	if allowed {
		return Ok(());
	}
	Err(FerrosGrynnError::closure_effect_not_allowed(
		format!("{:?}", closure.effect),
		if allow_mutation {
			"tv::side_effect".to_owned()
		} else {
			"read-only traversal step".to_owned()
		},
	))
}

fn push_traverse_result_column(schema: &mut Schema, name: Box<str>, ty: Type) {
	if let Some(column) = schema.columns.iter_mut().find(|column| column.name == name) {
		column.ty = unify(column.ty.clone(), ty);
	} else {
		schema.columns.push(Column {
			name,
			qualifier: None,
			ty,
		});
	}
}

fn traverse_source_item_type(source: &LogicalTraverseSource) -> Type {
	match source {
		LogicalTraverseSource::Expression(expression) => collection_item_type(&expression.ty),
		LogicalTraverseSource::Match(source) => {
			source.result.columns.last().map_or(Type::Any, |column| column.ty.clone())
		}
		LogicalTraverseSource::Select(source) => {
			if source.value && source.result.columns.len() == 1 {
				source.result.columns[0].ty.clone()
			} else {
				Type::Object
			}
		}
	}
}

fn collection_item_type(ty: &Type) -> Type {
	match ty {
		Type::Array(item) | Type::Set(item) | Type::Range(item) => (**item).clone(),
		_ => ty.clone(),
	}
}

fn traverse_step_output_type(step: &LogicalTraverseStep, current: Type) -> Type {
	if step.kind.is_tensor_analytics() {
		return Type::Object;
	}
	if let Some(output) = &step.output_type {
		return collection_item_type(output);
	}
	match step.kind {
		TraverseStepKind::Vertices
		| TraverseStepKind::Out
		| TraverseStepKind::In
		| TraverseStepKind::Both
		| TraverseStepKind::InVertex
		| TraverseStepKind::OutVertex => Type::AtomOccurrence(AtomKind::Node),
		TraverseStepKind::Edges
		| TraverseStepKind::OutEdges
		| TraverseStepKind::InEdges
		| TraverseStepKind::BothEdges
		| TraverseStepKind::Hyper => Type::AtomOccurrence(AtomKind::Edge),
		TraverseStepKind::MetaInner => Type::GraphRef(GraphKind::Graph),
		TraverseStepKind::Map | TraverseStepKind::FlatMap => step
			.arguments
			.first()
			.and_then(|argument| match &argument.ty {
				Type::Closure {
					result,
					..
				} => Some(if step.kind == TraverseStepKind::FlatMap {
					collection_item_type(result)
				} else {
					(**result).clone()
				}),
				_ => None,
			})
			.unwrap_or(Type::Any),
		TraverseStepKind::Unfold => collection_item_type(&current),
		TraverseStepKind::Select | TraverseStepKind::Project | TraverseStepKind::Group => {
			Type::Object
		}
		TraverseStepKind::Path
		| TraverseStepKind::ShortestPath
		| TraverseStepKind::WeightedShortestPath
		| TraverseStepKind::AllShortestPaths
		| TraverseStepKind::AllWeightedShortestPaths => Type::Path,
		TraverseStepKind::Count | TraverseStepKind::KHopEstimate => Type::Int,
		TraverseStepKind::Sum
		| TraverseStepKind::Minimum
		| TraverseStepKind::Maximum
		| TraverseStepKind::Average
		| TraverseStepKind::DistanceTo => Type::Number,
		_ => current,
	}
}

fn traverse_step_kind(name: &str) -> Option<TraverseStepKind> {
	Some(match name.to_ascii_lowercase().as_str() {
		"v" => TraverseStepKind::Vertices,
		"e" => TraverseStepKind::Edges,
		"out" => TraverseStepKind::Out,
		"in" => TraverseStepKind::In,
		"both" => TraverseStepKind::Both,
		"oute" => TraverseStepKind::OutEdges,
		"ine" => TraverseStepKind::InEdges,
		"bothe" => TraverseStepKind::BothEdges,
		"inv" => TraverseStepKind::InVertex,
		"outv" => TraverseStepKind::OutVertex,
		"hyper" => TraverseStepKind::Hyper,
		"meta_inner" => TraverseStepKind::MetaInner,
		"hasattr" => TraverseStepKind::HasAttribute,
		"has" => TraverseStepKind::Has,
		"haslabel" => TraverseStepKind::HasLabel,
		"where" => TraverseStepKind::Where,
		"dedup" => TraverseStepKind::Deduplicate,
		"dedup_by" => TraverseStepKind::DeduplicateBy,
		"simplepath" => TraverseStepKind::SimplePath,
		"cyclicpath" => TraverseStepKind::CyclicPath,
		"map" => TraverseStepKind::Map,
		"flatmap" => TraverseStepKind::FlatMap,
		"algo" => TraverseStepKind::Algorithm,
		"values" => TraverseStepKind::Values,
		"select" => TraverseStepKind::Select,
		"match" => TraverseStepKind::Match,
		"as" => TraverseStepKind::As,
		"project" => TraverseStepKind::Project,
		"unfold" => TraverseStepKind::Unfold,
		"init_sack" => TraverseStepKind::InitializeSack,
		"sack" => TraverseStepKind::Sack,
		"get_sack" => TraverseStepKind::GetSack,
		"init_named_sack" => TraverseStepKind::InitializeNamedSack,
		"named_sack" => TraverseStepKind::NamedSack,
		"get_named_sack" => TraverseStepKind::GetNamedSack,
		"side_effect" => TraverseStepKind::SideEffect,
		"path" => TraverseStepKind::Path,
		"emit" => TraverseStepKind::Emit,
		"limit" => TraverseStepKind::Limit,
		"skip" => TraverseStepKind::Skip,
		"repeat" => TraverseStepKind::Repeat,
		"branch" => TraverseStepKind::Branch,
		"choose" => TraverseStepKind::Choose,
		"coalesce" => TraverseStepKind::Coalesce,
		"order" => TraverseStepKind::Order,
		"group" => TraverseStepKind::Group,
		"fold" => TraverseStepKind::Fold,
		"count" => TraverseStepKind::Count,
		"sum" => TraverseStepKind::Sum,
		"min" => TraverseStepKind::Minimum,
		"max" => TraverseStepKind::Maximum,
		"avg" => TraverseStepKind::Average,
		"khop" => TraverseStepKind::KHop,
		"khopestimate" => TraverseStepKind::KHopEstimate,
		"reachable" => TraverseStepKind::Reachable,
		"shortestpath" => TraverseStepKind::ShortestPath,
		"weightedshortestpath" => TraverseStepKind::WeightedShortestPath,
		"allshortestpaths" => TraverseStepKind::AllShortestPaths,
		"allweightedshortestpaths" => TraverseStepKind::AllWeightedShortestPaths,
		"distanceto" => TraverseStepKind::DistanceTo,
		"bellmanford" => TraverseStepKind::BellmanFord,
		"topologicalsort" => TraverseStepKind::TopologicalSort,
		"stronglyconnectedcomponents" => TraverseStepKind::StronglyConnectedComponents,
		"weaklyconnectedcomponents" => TraverseStepKind::WeaklyConnectedComponents,
		"condensation" => TraverseStepKind::Condensation,
		"minimumspanningforest" => TraverseStepKind::MinimumSpanningForest,
		"findcuts" => TraverseStepKind::Cuts,
		"lca" => TraverseStepKind::LowestCommonAncestor,
		"hascycle" => TraverseStepKind::HasCycle,
		"pagerank" => TraverseStepKind::PageRank,
		"degree" => TraverseStepKind::Degree,
		"harmoniccloseness" => TraverseStepKind::HarmonicCloseness,
		"betweenness" => TraverseStepKind::Betweenness,
		"eigenvector" => TraverseStepKind::Eigenvector,
		"katz" => TraverseStepKind::Katz,
		"triangles" => TraverseStepKind::Triangles,
		"clustering" => TraverseStepKind::Clustering,
		"labelpropagation" => TraverseStepKind::LabelPropagation,
		"louvain" => TraverseStepKind::Louvain,
		_ => return None,
	})
}

const fn traverse_step_arity(kind: TraverseStepKind) -> std::ops::RangeInclusive<usize> {
	match kind {
		TraverseStepKind::AllShortestPaths | TraverseStepKind::AllWeightedShortestPaths => 1..=2,
		TraverseStepKind::PageRank
		| TraverseStepKind::Degree
		| TraverseStepKind::HarmonicCloseness
		| TraverseStepKind::Betweenness
		| TraverseStepKind::Eigenvector
		| TraverseStepKind::Katz
		| TraverseStepKind::Triangles
		| TraverseStepKind::Clustering
		| TraverseStepKind::LabelPropagation
		| TraverseStepKind::Louvain => 0..=1,
		TraverseStepKind::Vertices
		| TraverseStepKind::Edges
		| TraverseStepKind::InVertex
		| TraverseStepKind::OutVertex
		| TraverseStepKind::Hyper
		| TraverseStepKind::MetaInner
		| TraverseStepKind::Deduplicate
		| TraverseStepKind::SimplePath
		| TraverseStepKind::CyclicPath
		| TraverseStepKind::Unfold
		| TraverseStepKind::GetSack
		| TraverseStepKind::Path
		| TraverseStepKind::Emit
		| TraverseStepKind::Fold
		| TraverseStepKind::Count
		| TraverseStepKind::Sum
		| TraverseStepKind::Minimum
		| TraverseStepKind::Maximum
		| TraverseStepKind::Average
		| TraverseStepKind::TopologicalSort
		| TraverseStepKind::StronglyConnectedComponents
		| TraverseStepKind::WeaklyConnectedComponents
		| TraverseStepKind::Condensation
		| TraverseStepKind::MinimumSpanningForest
		| TraverseStepKind::Cuts
		| TraverseStepKind::HasCycle => 0..=0,
		TraverseStepKind::Out
		| TraverseStepKind::In
		| TraverseStepKind::Both
		| TraverseStepKind::OutEdges
		| TraverseStepKind::InEdges
		| TraverseStepKind::BothEdges
		| TraverseStepKind::Order => 0..=1,
		TraverseStepKind::Group => 0..=usize::MAX,
		TraverseStepKind::HasAttribute
		| TraverseStepKind::Where
		| TraverseStepKind::DeduplicateBy
		| TraverseStepKind::Map
		| TraverseStepKind::FlatMap
		| TraverseStepKind::As
		| TraverseStepKind::InitializeSack
		| TraverseStepKind::Sack
		| TraverseStepKind::GetNamedSack
		| TraverseStepKind::Limit
		| TraverseStepKind::Skip
		| TraverseStepKind::KHop
		| TraverseStepKind::KHopEstimate
		| TraverseStepKind::Reachable
		| TraverseStepKind::ShortestPath
		| TraverseStepKind::WeightedShortestPath
		| TraverseStepKind::DistanceTo
		| TraverseStepKind::BellmanFord
		| TraverseStepKind::LowestCommonAncestor => 1..=1,
		TraverseStepKind::Has
		| TraverseStepKind::InitializeNamedSack
		| TraverseStepKind::NamedSack
		| TraverseStepKind::SideEffect => 2..=2,
		TraverseStepKind::Choose => 2..=3,
		TraverseStepKind::HasLabel
		| TraverseStepKind::Values
		| TraverseStepKind::Select
		| TraverseStepKind::Match
		| TraverseStepKind::Project
		| TraverseStepKind::Branch
		| TraverseStepKind::Coalesce => 1..=usize::MAX,
		TraverseStepKind::Algorithm => 1..=usize::MAX,
		TraverseStepKind::Repeat => 1..=1,
	}
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

fn column_origin<'a>(
	schema: Option<&'a Schema>,
	expression: &TypedExpr,
) -> Option<&'a ColumnOrigin> {
	let TypedExprKind::Column(column) = expression.kind else {
		return None;
	};
	schema?.origins.get(&column)
}

fn index_scope_matches(index: IndexScope, source: Option<IndexScope>) -> bool {
	match source {
		Some(source) => index == source,
		None => matches!(index, IndexScope::Effective { .. }),
	}
}

pub(super) fn visit_typed_expression<'expression, Error>(
	expression: &'expression TypedExpr,
	visit: &mut impl FnMut(&'expression TypedExpr) -> Result<(), Error>,
) -> Result<(), Error> {
	visit(expression)?;
	match &expression.kind {
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			for value in values {
				visit_typed_expression(value, visit)?;
			}
		}
		TypedExprKind::Object(fields) => {
			for (_, value) in fields {
				visit_typed_expression(value, visit)?;
			}
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => {
			if let Some(value) = start {
				visit_typed_expression(value, visit)?;
			}
			if let Some(value) = end {
				visit_typed_expression(value, visit)?;
			}
		}
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
		} => visit_typed_expression(expr, visit)?,
		TypedExprKind::Binary {
			left,
			right,
			..
		}
		| TypedExprKind::Index {
			base: left,
			index: right,
		}
		| TypedExprKind::Filter {
			base: left,
			predicate: right,
		}
		| TypedExprKind::Fulltext {
			left,
			right,
			..
		} => {
			visit_typed_expression(left, visit)?;
			visit_typed_expression(right, visit)?;
		}
		TypedExprKind::Knn {
			left,
			k,
			ef,
			distance,
			right,
			..
		} => {
			visit_typed_expression(left, visit)?;
			visit_typed_expression(k, visit)?;
			if let Some(ef) = ef {
				visit_typed_expression(ef, visit)?;
			}
			if let LogicalKnnDistance::Minkowski(value) | LogicalKnnDistance::Callable(value) =
				distance
			{
				visit_typed_expression(value, visit)?;
			}
			visit_typed_expression(right, visit)?;
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			visit_typed_expression(condition, visit)?;
			visit_typed_expression(then_value, visit)?;
			visit_typed_expression(else_value, visit)?;
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			if let Some(operand) = operand {
				visit_typed_expression(operand, visit)?;
			}
			for (condition, value) in branches {
				visit_typed_expression(condition, visit)?;
				visit_typed_expression(value, visit)?;
			}
			if let Some(value) = else_value {
				visit_typed_expression(value, visit)?;
			}
		}
		TypedExprKind::Builtin {
			arguments,
			..
		}
		| TypedExprKind::NamedInvoke {
			arguments,
			..
		} => {
			for argument in arguments {
				visit_typed_expression(argument, visit)?;
			}
		}
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => {
			visit_typed_expression(callee, visit)?;
			for argument in arguments {
				visit_typed_expression(argument, visit)?;
			}
		}
		TypedExprKind::Aggregate {
			argument,
			..
		} => {
			if let Some(argument) = argument {
				visit_typed_expression(argument, visit)?;
			}
		}
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_)
		| TypedExprKind::Closure(_)
		| TypedExprKind::Traversal(_)
		| TypedExprKind::LiveSlot(_)
		| TypedExprKind::Subquery {
			..
		} => {}
	}
	Ok(())
}

fn typed_expression_contains_knn(expression: &TypedExpr) -> bool {
	let mut found = false;
	let result = visit_typed_expression(expression, &mut |expression| {
		found |= matches!(expression.kind, TypedExprKind::Knn { .. });
		Ok::<_, std::convert::Infallible>(())
	});
	match result {
		Ok(()) => found,
		Err(never) => match never {},
	}
}

fn typed_expression_is_statement_fixed(expression: &TypedExpr) -> bool {
	let mut fixed = true;
	let result = visit_typed_expression(expression, &mut |expression| {
		if matches!(
			expression.kind,
			TypedExprKind::Column(_)
				| TypedExprKind::CurrentField(_)
				| TypedExprKind::Aggregate { .. }
				| TypedExprKind::Traversal(_)
				| TypedExprKind::Subquery { .. }
		) {
			fixed = false;
		}
		Ok::<_, std::convert::Infallible>(())
	});
	match result {
		Ok(()) => fixed,
		Err(never) => match never {},
	}
}

fn typed_expression_is_search_related(expression: &TypedExpr) -> bool {
	matches!(
		expression.kind,
		TypedExprKind::Fulltext { .. }
			| TypedExprKind::Knn { .. }
			| TypedExprKind::Builtin {
				function: Builtin::SearchScore(_) | Builtin::SearchHighlight(_),
				..
			}
	)
}

fn vector_distance_matches(options: ClassIndexOptions, distance: &LogicalKnnDistance) -> bool {
	let expected = match distance {
		LogicalKnnDistance::Builtin(distance) => Some(*distance),
		LogicalKnnDistance::Minkowski(exponent) => match &exponent.kind {
			TypedExprKind::Constant(Value::Number(number)) => {
				number.as_f64().map(VectorDistance::Minkowski)
			}
			_ => None,
		},
		LogicalKnnDistance::Named(function) => Some(VectorDistance::Custom {
			function: function.id,
			version: function.version,
		}),
		LogicalKnnDistance::Callable(_) => None,
	};
	let actual = match options {
		ClassIndexOptions::Hnsw(options) => options.distance(),
		ClassIndexOptions::DiskAnn(options) => options.distance(),
		ClassIndexOptions::Scalar
		| ClassIndexOptions::Fulltext(_)
		| ClassIndexOptions::Geometry(_) => return false,
	};
	expected.is_some_and(|expected| expected == actual)
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
		origins: BTreeMap::new(),
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

/// Checks output paths equally for ordinary and zero-domain SELECT plans.
pub(super) fn validate_projection_paths(
	projection: &[LogicalSelectItem],
	paths: &[Option<Vec<Box<str>>>],
) -> Result<(), &'static str> {
	if paths.is_empty() {
		return Ok(());
	}
	if paths.len() != projection.len() {
		return Err("SELECT output paths do not match the projection.");
	}
	let mut occupied = BTreeSet::<&[Box<str>]>::new();
	for (item, path) in projection.iter().zip(paths) {
		let key = match (item, path) {
			(_, Some(path)) => path.as_slice(),
			(
				LogicalSelectItem::Expression {
					name,
					..
				},
				None,
			) => std::slice::from_ref(name),
			_ => return Err("Nested output aliases cannot be combined with wildcard projection."),
		};
		if key.is_empty()
			|| key.len() > 64
			|| key.iter().any(|part| part.is_empty() || part.contains('\0'))
		{
			return Err("SELECT output path must contain 1..64 nonempty names.");
		}
		if occupied.range(..=key).next_back().is_some_and(|previous| key.starts_with(previous))
			|| occupied.range(key..).next().is_some_and(|next| next.starts_with(key))
		{
			return Err("SELECT output paths overlap.");
		}
		occupied.insert(key);
	}
	Ok(())
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
		ExprKind::Fulltext {
			left,
			right,
			..
		} => expr_contains_aggregate(left) || expr_contains_aggregate(right),
		ExprKind::Knn {
			left,
			k,
			search,
			right,
		} => {
			expr_contains_aggregate(left)
				|| expr_contains_aggregate(k)
				|| search.as_ref().is_some_and(|search| match search {
					KnnSearchArgument::Ef(value) => expr_contains_aggregate(value),
					KnnSearchArgument::Distance(
						KnnDistance::Minkowski(value) | KnnDistance::Custom(value),
					) => expr_contains_aggregate(value),
					KnnSearchArgument::Distance(_) => false,
				})
				|| expr_contains_aggregate(right)
		}
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
		| ExprKind::Subquery(_)
		| ExprKind::MatchSubquery(_)
		| ExprKind::Closure(_) => false,
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
		ExprKind::Fulltext {
			left,
			right,
			..
		} => expr_references_row(left) || expr_references_row(right),
		ExprKind::Knn {
			left,
			k,
			search,
			right,
		} => {
			expr_references_row(left)
				|| expr_references_row(k)
				|| search.as_ref().is_some_and(|search| match search {
					KnnSearchArgument::Ef(value) => expr_references_row(value),
					KnnSearchArgument::Distance(
						KnnDistance::Minkowski(value) | KnnDistance::Custom(value),
					) => expr_references_row(value),
					KnnSearchArgument::Distance(_) => false,
				})
				|| expr_references_row(right)
		}
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
		ExprKind::Literal(_)
		| ExprKind::Parameter(_)
		| ExprKind::Subquery(_)
		| ExprKind::MatchSubquery(_)
		| ExprKind::Closure(_) => false,
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
		TypedExprKind::Fulltext {
			left,
			right,
			..
		} => typed_expr_contains_aggregate(left) || typed_expr_contains_aggregate(right),
		TypedExprKind::Knn {
			left,
			k,
			ef,
			distance,
			right,
			..
		} => {
			typed_expr_contains_aggregate(left)
				|| typed_expr_contains_aggregate(k)
				|| ef.as_deref().is_some_and(typed_expr_contains_aggregate)
				|| match distance {
					LogicalKnnDistance::Minkowski(value) | LogicalKnnDistance::Callable(value) => {
						typed_expr_contains_aggregate(value)
					}
					LogicalKnnDistance::Builtin(_) | LogicalKnnDistance::Named(_) => false,
				}
				|| typed_expr_contains_aggregate(right)
		}
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
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => {
			typed_expr_contains_aggregate(callee)
				|| arguments.iter().any(typed_expr_contains_aggregate)
		}
		TypedExprKind::NamedInvoke {
			arguments,
			..
		} => arguments.iter().any(typed_expr_contains_aggregate),
		TypedExprKind::Traversal(_) => false,
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_)
		| TypedExprKind::Closure(_)
		| TypedExprKind::LiveSlot(_)
		| TypedExprKind::Subquery {
			..
		} => false,
	}
}

pub(in crate::gql) fn group_expr_equivalent(left: &TypedExpr, right: &TypedExpr) -> bool {
	match (&left.kind, &right.kind) {
		(TypedExprKind::Constant(left), TypedExprKind::Constant(right)) => left == right,
		(TypedExprKind::Slot(left), TypedExprKind::Slot(right)) => left == right,
		(TypedExprKind::LiveSlot(left), TypedExprKind::LiveSlot(right)) => left == right,
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

const fn function_effect(effect: FunctionEffectName) -> FunctionEffect {
	match effect {
		FunctionEffectName::Pure => FunctionEffect::Pure,
		FunctionEffectName::Stable => FunctionEffect::Stable,
		FunctionEffectName::Impure => FunctionEffect::Impure,
	}
}

const fn vector_element(element: VectorElementName) -> VectorKind {
	match element {
		VectorElementName::F64 => VectorKind::F64,
		VectorElementName::F32 => VectorKind::F32,
		VectorElementName::F16 => VectorKind::F16,
		VectorElementName::I64 => VectorKind::I64,
		VectorElementName::I32 => VectorKind::I32,
		VectorElementName::I16 => VectorKind::I16,
		VectorElementName::I8 => VectorKind::I8,
		VectorElementName::U8 => VectorKind::U8,
	}
}

const fn analyzer_tokenizer(tokenizer: AnalyzerTokenizerName) -> AnalyzerTokenizer {
	match tokenizer {
		AnalyzerTokenizerName::Blank => AnalyzerTokenizer::Blank,
		AnalyzerTokenizerName::Camel => AnalyzerTokenizer::Camel,
		AnalyzerTokenizerName::Class => AnalyzerTokenizer::Class,
		AnalyzerTokenizerName::Punct => AnalyzerTokenizer::Punct,
	}
}

fn analyzer_language(language: &Identifier) -> FerrosGrynnResult<AnalyzerLanguage> {
	match language.value.to_ascii_lowercase().as_str() {
		"arabic" => Ok(AnalyzerLanguage::Arabic),
		"danish" => Ok(AnalyzerLanguage::Danish),
		"dutch" => Ok(AnalyzerLanguage::Dutch),
		"english" => Ok(AnalyzerLanguage::English),
		"finnish" => Ok(AnalyzerLanguage::Finnish),
		"french" => Ok(AnalyzerLanguage::French),
		"german" => Ok(AnalyzerLanguage::German),
		"greek" => Ok(AnalyzerLanguage::Greek),
		"hungarian" => Ok(AnalyzerLanguage::Hungarian),
		"italian" => Ok(AnalyzerLanguage::Italian),
		"norwegian" => Ok(AnalyzerLanguage::Norwegian),
		"portuguese" => Ok(AnalyzerLanguage::Portuguese),
		"romanian" => Ok(AnalyzerLanguage::Romanian),
		"russian" => Ok(AnalyzerLanguage::Russian),
		"spanish" => Ok(AnalyzerLanguage::Spanish),
		"swedish" => Ok(AnalyzerLanguage::Swedish),
		"tamil" => Ok(AnalyzerLanguage::Tamil),
		"turkish" => Ok(AnalyzerLanguage::Turkish),
		_ => Err(FerrosGrynnError::invalid_argument(format!(
			"unsupported Snowball language '{}'",
			language.value
		))),
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
		|| matches!((expected, actual),
			(Type::Array(expected), Type::Array(actual))
			| (Type::Set(expected), Type::Set(actual))
			| (Type::Range(expected), Type::Range(actual))
			if compatible(expected, actual))
		|| matches!(expected, Type::Option(inner) if matches!(actual, Type::None | Type::Null) || compatible(inner, actual))
		|| matches!(expected, Type::Union(types) if types.iter().any(|ty| compatible(ty, actual)))
}

/// Dynamic assignments defer their remaining checks to the lexical validator.
fn assignment_compatible(expected: &Type, actual: &Type) -> bool {
	if compatible(expected, actual) || matches!(actual, Type::Any) {
		return true;
	}
	match (expected, actual) {
		(Type::Array(expected), Type::Array(actual))
		| (Type::Set(expected), Type::Set(actual))
		| (Type::Range(expected), Type::Range(actual))
		| (Type::Option(expected), Type::Option(actual)) => assignment_compatible(expected, actual),
		(Type::Option(inner), actual) => {
			matches!(actual, Type::None | Type::Null) || assignment_compatible(inner, actual)
		}
		(Type::Union(types), actual) => types.iter().any(|ty| assignment_compatible(ty, actual)),
		_ => compatible(expected, actual),
	}
}

/// Widens branch/item result types; nullable widening is idempotent.
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
		return nullable_type(right);
	}
	if matches!(right, Type::None | Type::Null) {
		return nullable_type(left);
	}
	let (left, right) = match (left, right) {
		(Type::Option(left), Type::Option(right)) => return nullable_type(unify(*left, *right)),
		(Type::Option(left), right) => return nullable_type(unify(*left, right)),
		(left, Type::Option(right)) => return nullable_type(unify(left, *right)),
		values => values,
	};
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

/// Reuses one optional wrapper across nullable branch/item widening.
fn nullable_type(value: Type) -> Type {
	match value {
		value @ Type::Option(_) => value,
		value => Type::Option(Box::new(value)),
	}
}
