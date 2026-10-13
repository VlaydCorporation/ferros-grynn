use std::{collections::BTreeMap, sync::Arc, time::Duration};

use crate::{
	datastore::{
		ClassIndexKind,
		ClassIndexOptions,
		ClassIndexState,
		ClassSchemaMode,
		FunctionEffect,
		FunctionId,
		IndexId,
		IndexScope,
		SchemaAnalyzer,
		SchemaVersion,
		VectorDistance,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult, TextSpan},
	graph::GraphTopologyConstraints,
	syn::{DefinitionMode, MatchDirection},
	values::{DateTime, GraphKind, GraphRef, RecordId, Text, Type, Value},
};

/// Hard ceiling shared by MATCH analysis and invocation-time validation.
pub(super) const MAX_MATCH_PATH_DEPTH: u32 = 30;

/// Validates an exact, non-negative MATCH clause depth before row pull.
pub(super) fn validate_match_max_depth(value: &Value) -> FerrosGrynnResult<u32> {
	let depth = value
		.as_number()
		.and_then(|number| i64::try_from(number).ok())
		.and_then(|number| u32::try_from(number).ok())
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("MATCH MAXDEPTH must be a non-negative integer.")
		})?;
	if depth > MAX_MATCH_PATH_DEPTH {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"MATCH MAXDEPTH cannot exceed {MAX_MATCH_PATH_DEPTH}."
		)));
	}
	Ok(depth)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct NodeId(pub(super) u32);

#[derive(Debug, Clone)]
pub(super) struct LogicalProgram {
	pub(super) statements: Vec<LogicalStatement>,
	pub(super) initial_slots: Vec<(Box<str>, u32, bool)>,
	pub(super) slot_count: usize,
}

#[derive(Debug, Clone)]
pub(super) enum LogicalStatement {
	/// Replaces a mutable lexical value while preserving frozen LIVE captures.
	SetBinding {
		/// Checked nearest lexical slot.
		slot: u32,
		/// Runtime contract retained even when the replacement has dynamic
		/// type.
		expected: Type,
		/// Typed replacement retaining the binding's value type.
		value: TypedExpr,
	},
	Explain(LogicalExplain),
	Schema(LogicalSchemaStatement),
	Dml(LogicalDmlStatement),
	GraphDml(LogicalGraphDmlStatement),
	Let {
		name: Box<str>,
		slot: u32,
		/// Declared type, or the inferred initializer type when unannotated.
		expected: Type,
		value: LogicalLetValue,
	},
	Return {
		/// Carries the analyzed endpoint expression through ordinary execution.
		value: TypedExpr,
	},
	ReturnGraph(LogicalReturnGraph),
	Expression {
		/// Carries the analyzed endpoint expression through ordinary execution.
		value: TypedExpr,
	},
	If {
		branches: Vec<(TypedExpr, LogicalBlock)>,
		else_block: Option<LogicalBlock>,
	},
	For {
		slot: u32,
		iterable: TypedExpr,
		body: LogicalBlock,
	},
	While {
		condition: TypedExpr,
		body: LogicalBlock,
	},
	Break,
	Continue,
	Try {
		body: LogicalBlock,
		catch: Option<(u32, LogicalBlock)>,
	},
	Throw {
		/// Carries the analyzed endpoint expression through ordinary execution.
		value: TypedExpr,
	},
	Sleep(LogicalDuration),
	AssertInvariants {
		graph: Option<GraphRef>,
		report: usize,
	},
	BuildGraph(LogicalBuildGraph),
	/// Executes a read-only graph pattern match.
	Match(LogicalMatch),
	Select(LogicalSelect),
	/// Executes a streaming traversal pipeline.
	Traverse(LogicalTraverse),
}

#[derive(Debug, Clone)]
pub(super) enum LogicalLetValue {
	Expression(TypedExpr),
	BuildGraph(LogicalBuildGraph),
	/// Checked catalog command used only by LET ... = LIVE SELECT.
	Live(Box<LogicalSchemaStatement>),
}

/// Analyzed logical recipe with captures resolved at execution, never during
/// analysis of an enclosing block.
#[derive(Debug, Clone)]
pub(super) struct LogicalLiveSelect {
	/// Conservative retained recipe allowance admitted before runtime clone.
	pub(super) retained_bytes: usize,
	/// Owned recipe; no executable typed plan is serialized in its descriptor.
	pub(super) query: crate::live::LiveQuery,
	/// Ordinary graph parameters belong to their logical source declaration.
	/// Reactive scopes retain their expression in the owned query recipe.
	pub(super) frozen_graphs: Vec<LogicalLiveSourceCapture>,
	/// Lexical names and exact frame slots read at registration.
	pub(super) captures: Vec<LogicalLiveCapture>,
	/// Runtime exclusive stamp or inclusive timestamp.
	pub(super) since: Option<TypedExpr>,
	/// Runtime checked event ceiling.
	pub(super) buffer: Option<TypedExpr>,
	/// Explicit validated queue behavior.
	pub(super) overflow: crate::live::LiveOverflowPolicy,
	/// Analyzer-owned registration-only operands; executable IR is never
	/// serialized.
	pub(super) operand_spelling: Box<str>,
	/// Actual lexical reads made by registration-only expressions.
	pub(super) operand_slots: Vec<LogicalLiveCapture>,
}

/// One registration-only scope keeps its source owner independently of aliases.
#[derive(Debug, Clone)]
pub(super) struct LogicalLiveSourceCapture {
	/// Declaration ordinal: zero is FROM; positive ordinals are JOIN sources.
	pub(super) source: u32,
	/// Ordinary scope expression evaluated once at registration.
	pub(super) graph: TypedExpr,
}

/// One lexical capture retains provenance independently of its UUID value.
#[derive(Debug, Clone)]
pub(super) struct LogicalLiveCapture {
	/// Recoverable parameter spelling within the logical recipe.
	pub(super) name: Box<str>,
	/// Exact frame position chosen by lexical resolution.
	pub(super) slot: u32,
	/// Tracks a reactive identity rather than freezing an ordinary value.
	pub(super) reactive: bool,
}

/// Checked expression recipe and its registration-time lexical captures.
#[derive(Debug, Clone)]
pub(super) struct LogicalLiveBinding {
	/// Admission for the recipe before its owned runtime copy.
	pub(super) retained_bytes: usize,
	/// Recoverable expression evaluated by the common pipeline.
	pub(super) expression: crate::live::LiveExpr,
	/// Frozen values and independent reactive identities.
	pub(super) captures: Vec<LogicalLiveCapture>,
}

#[derive(Debug, Clone)]
pub(super) struct LogicalExplain {
	pub(super) mode: crate::syn::ExplainMode,
	pub(super) format: crate::syn::ExplainFormat,
	pub(super) statement: Box<LogicalStatement>,
}

#[derive(Debug, Clone)]
pub(super) struct LogicalBlock {
	pub(super) statements: Vec<LogicalBlockStatement>,
}

#[derive(Debug, Clone)]
pub(super) struct LogicalBlockStatement {
	pub(super) statement: LogicalStatement,
	pub(super) timeout: Option<LogicalDuration>,
}

/// A duration literal is fixed; a parameter retains its lexical provenance.
#[derive(Debug, Clone)]
pub(super) enum LogicalDuration {
	/// Literal duration validated by the existing fixed-duration parser.
	Fixed(Duration),
	/// Checked Slot/LiveSlot with invocation-time value and source location.
	Parameter(TypedExpr),
}

/// One analyzed graph-membership or graph-incidence mutation.
#[derive(Debug, Clone)]
pub(super) enum LogicalGraphDmlStatement {
	Attach {
		record: LogicalRecordTarget,
		graph: GraphRef,
		label: Option<Text>,
		keep_levelpath: bool,
	},
	Detach {
		record: LogicalRecordTarget,
		graph: GraphRef,
		cascade: bool,
	},
	Relate {
		/// Optional runtime adapter for post-commit relation mirroring.
		bridge: Option<Box<str>>,
		atom_endpoints: bool,
		/// Retains the analyzed source endpoint identity for ordinary RELATE.
		source: LogicalRelateEndpoint,
		edge: LogicalRelateEdge,
		/// Retains the analyzed target endpoint identity for ordinary RELATE.
		target: LogicalRelateEndpoint,
		graph: GraphRef,
		label: Option<Text>,
		direction: MatchDirection,
		weight: Option<TypedExpr>,
		keep_levelpath: bool,
	},
}

impl LogicalGraphDmlStatement {
	pub(super) const fn statement_kind(&self) -> crate::gql::QueryStatementKind {
		match self {
			Self::Attach {
				..
			} => crate::gql::QueryStatementKind::Attach,
			Self::Detach {
				..
			} => crate::gql::QueryStatementKind::Detach,
			Self::Relate {
				..
			} => crate::gql::QueryStatementKind::Relate,
		}
	}
}

/// Exact class-qualified record target used by graph DML.
#[derive(Debug, Clone)]
pub(super) struct LogicalRecordTarget {
	pub(super) class: Box<str>,
	pub(super) key: TypedExpr,
}

/// Typed RELATE endpoint selection has no ATTACH/DETACH target responsibility.
#[derive(Debug, Clone)]
pub(super) enum LogicalRelateEndpoint {
	/// An exact class-qualified record with ordinary key evaluation.
	Record(LogicalRecordTarget),
	/// Runtime reference with an optional explicit AtomWalk kind constraint.
	Reference {
		/// Carries the analyzed endpoint expression through ordinary execution.
		value: TypedExpr,
		/// Retains an explicit node or edge kind supplied by the reference
		/// idiom.
		kind: Option<crate::graph::AtomKind>,
	},
}

impl LogicalRelateEndpoint {
	/// Shared effect validation observes the evaluated endpoint operand.
	pub(super) const fn expression(&self) -> &TypedExpr {
		match self {
			Self::Record(record) => &record.key,
			Self::Reference {
				value,
				..
			} => value,
		}
	}
}

/// Edge atom and graph-local payload prepared for RELATE.
#[derive(Debug, Clone)]
pub(super) struct LogicalRelateEdge {
	pub(super) class: Box<str>,
	pub(super) key: Option<TypedExpr>,
	pub(super) schema: Schema,
	pub(super) data: Option<LogicalDmlData>,
	pub(super) returning: LogicalDmlReturn,
	pub(super) expect: Option<LogicalExpect>,
}

/// One analyzed data-mutation statement.
#[derive(Debug, Clone)]
pub(super) struct LogicalDmlStatement {
	pub(super) kind: LogicalDmlKind,
	pub(super) graph: Option<GraphRef>,
	/// Label mutation: no change, explicit removal, or replacement.
	pub(super) label: Option<Option<crate::values::Text>>,
	pub(super) cascade: bool,
	pub(super) keep_levelpath: bool,
	pub(super) targets: Vec<LogicalDmlTarget>,
}

/// Mutation operation shared by every target in a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LogicalDmlKind {
	Create,
	Update,
	Upsert,
	Delete,
}

impl LogicalDmlKind {
	pub(super) const fn statement_kind(self) -> crate::gql::QueryStatementKind {
		match self {
			Self::Create => crate::gql::QueryStatementKind::Create,
			Self::Update => crate::gql::QueryStatementKind::Update,
			Self::Upsert => crate::gql::QueryStatementKind::Upsert,
			Self::Delete => crate::gql::QueryStatementKind::Delete,
		}
	}
}

/// One class-specific mutation plan. Expressions are typed against this
/// target because a statement may name classes with different schemas.
#[derive(Debug, Clone)]
pub(super) struct LogicalDmlTarget {
	pub(super) class: Box<str>,
	pub(super) selector: LogicalDmlSelector,
	pub(super) schema: Schema,
	pub(super) data: Option<LogicalDmlData>,
	pub(super) filter: Option<TypedExpr>,
	pub(super) returning: LogicalDmlReturn,
	pub(super) expect: Option<LogicalExpect>,
}

/// Records selected or generated for one DML target.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlSelector {
	Class,
	Record(TypedExpr),
	Range(u32),
}

/// An analyzed record mutation mode.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlData {
	Content(TypedExpr),
	Merge(TypedExpr),
	Patch(TypedExpr),
	Replace(TypedExpr),
	Set(Vec<(Box<str>, TypedExpr)>),
	Unset(Vec<Box<str>>),
	Reset(Vec<Box<str>>),
}

/// An analyzed DML result projection.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlReturn {
	None,
	Before,
	After,
	Diff,
	Fields(Vec<LogicalDmlReturnItem>),
	Value(TypedExpr),
}

/// One object-shaped return item.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlReturnItem {
	Wildcard,
	Expr {
		name: Box<str>,
		/// Carries the analyzed endpoint expression through ordinary execution.
		value: TypedExpr,
	},
}

/// A pre-image requirement checked before the first mutation.
#[derive(Debug, Clone)]
pub(super) enum LogicalExpect {
	Exists(bool),
	Predicate(TypedExpr),
}

/// Logical source owner resolved by the ordinary transaction catalog adapter.
#[derive(Debug, Clone)]
pub(super) enum LogicalEventOwner {
	/// Global class namespace, resolved at command execution.
	Class(Box<str>),
	/// Graph namespace, resolved at command execution.
	Graph(Box<str>),
}

/// One ordered source/policy replacement within ALTER EVENT.
#[derive(Debug, Clone)]
pub(super) enum LogicalEventChange {
	/// Checked synchronous or asynchronous policy replacement.
	Execution(crate::datastore::EventExecution),
	/// Owned read-only predicate source replacement.
	When(Text),
	/// Owned expression/block handler source replacement.
	Then(Text),
	/// Optional durable comment replacement or removal.
	Comment(Option<Text>),
}

/// Source-only lifecycle commands; handler typed IR is never stored here.
#[derive(Debug, Clone)]
pub(super) enum LogicalEventStatement {
	/// Registers or overwrites one source-owned definition.
	Define {
		/// Duplicate-definition policy.
		mode: DefinitionMode,
		/// Name within the stable source owner.
		name: Box<str>,
		/// Logical owner resolved to ClassId or GraphId.
		owner: LogicalEventOwner,
		/// Checked execution, retry and depth policy.
		execution: crate::datastore::EventExecution,
		/// Owned predicate source.
		when: Text,
		/// Owned handler source.
		then: Text,
	},
	/// Removes a definition without cancelling frozen accepted invocations.
	Remove {
		/// Suppresses absence of the definition or its owner.
		if_exists: bool,
		/// Source-owned event name.
		name: Box<str>,
		/// Logical class or graph source.
		owner: LogicalEventOwner,
	},
	/// Applies ordered changes, validating the complete final definition.
	Alter {
		/// Suppresses absence of the definition or its owner.
		if_exists: bool,
		/// Source-owned event name.
		name: Box<str>,
		/// Logical class or graph source.
		owner: LogicalEventOwner,
		/// Source/policy changes in declaration order.
		actions: Vec<LogicalEventChange>,
	},
}

impl LogicalEventStatement {
	/// Classifies each event lifecycle command for shared schema execution.
	pub(super) const fn statement_kind(&self) -> crate::gql::QueryStatementKind {
		match self {
			Self::Define {
				..
			} => crate::gql::QueryStatementKind::Define,
			Self::Remove {
				..
			} => crate::gql::QueryStatementKind::Remove,
			Self::Alter {
				..
			} => crate::gql::QueryStatementKind::Alter,
		}
	}
}

/// Typed core schema command executed by the datastore adapter.
#[derive(Debug, Clone)]
pub(super) enum LogicalSchemaStatement {
	/// Event DDL retains source recipes, never executable handler IR.
	Event(LogicalEventStatement),
	/// Defines a reactive identity through the ordinary transaction API.
	DefineLiveBinding(Box<LogicalLiveBinding>),
	/// Replaces a recipe without changing the identity seen by dependents.
	ReplaceLiveBinding {
		/// Raw lexical identity selected independently of value resolution.
		id: TypedExpr,
		/// Recoverable replacement and current capture positions.
		plan: Box<LogicalLiveBinding>,
	},
	/// Reads retained binding metadata and its exact latest value.
	InfoLiveBinding {
		/// Raw lexical identity of the retained definition.
		id: TypedExpr,
	},
	/// Removes a binding after the common dependency check.
	RemoveLiveBinding {
		/// Raw lexical identity checked against active dependents.
		id: TypedExpr,
	},
	/// Registers a logical LIVE incarnation through the ordinary transaction.
	RegisterLive(Box<LogicalLiveSelect>),
	/// Reads one incarnation through the same snapshot metadata API as Rust.
	InfoLive {
		/// Typed UUID operand, independent of the lexical variable spelling.
		id: TypedExpr,
	},
	/// Stops capture transactionally, without discarding retained history.
	CloseLive {
		/// Typed UUID operand, resolved before staging the close.
		id: TypedExpr,
	},
	/// Confirms only a token issued by a retained session consumer.
	AckLive {
		/// Typed UUID identity operand.
		id: TypedExpr,
		/// Typed fixed Bytes cursor operand.
		cursor: TypedExpr,
		/// Definition-owned operand text for deferred, side-effect-free
		/// planning.
		spelling: Box<str>,
	},
	DefineGraph {
		mode: DefinitionMode,
		name: Box<str>,
		kind: GraphKind,
		constraints: GraphTopologyConstraints,
		root: Option<TypedExpr>,
		content: Option<TypedExpr>,
		/// Final validated capture policy for the defined graph source.
		changefeed: Option<crate::changefeed::ChangefeedConfig>,
	},
	UpdateGraph {
		graph: GraphRef,
		mutation: LogicalGraphPropertyMutation,
		returning: LogicalDmlReturn,
	},
	DefineClass {
		mode: DefinitionMode,
		name: Box<str>,
		kind: crate::values::AtomKind,
		schema_mode: ClassSchemaMode,
		/// Final validated capture policy for the defined global class source.
		changefeed: Option<crate::changefeed::ChangefeedConfig>,
	},
	DefineField {
		mode: DefinitionMode,
		class: Box<str>,
		name: Box<str>,
		value_type: Type,
	},
	DefineIndex {
		mode: DefinitionMode,
		class: Box<str>,
		name: Box<str>,
		kind: ClassIndexKind,
		fields: Vec<Box<str>>,
		options: ClassIndexOptions,
		scope: IndexScope,
		maintenance: crate::datastore::IndexMaintenanceMode,
		concurrently: bool,
	},
	/// Graph-owned definition resolved to public catalog types.
	DefineGraphIndex {
		mode: DefinitionMode,
		graph: Box<str>,
		index: crate::datastore::GraphIndex,
		concurrently: bool,
	},
	DefineAnalyzer {
		mode: DefinitionMode,
		analyzer: crate::datastore::SchemaAnalyzer,
	},
	DefineParameter {
		mode: DefinitionMode,
		name: Box<str>,
		/// Carries the analyzed endpoint expression through ordinary execution.
		value: TypedExpr,
		comment: Option<Text>,
	},
	DefineFunction {
		mode: DefinitionMode,
		namespace: Box<str>,
		name: Box<str>,
		parameters: Vec<(Box<str>, Type)>,
		result: Type,
		effect: FunctionEffect,
		cost: bool,
		max_depth: Option<u8>,
		body: Text,
		comment: Option<Text>,
	},
	RemoveGraph {
		if_exists: bool,
		name: Box<str>,
	},
	RemoveClass {
		if_exists: bool,
		name: Box<str>,
	},
	RemoveField {
		if_exists: bool,
		class: Box<str>,
		name: Box<str>,
	},
	RemoveIndex {
		if_exists: bool,
		class: Box<str>,
		name: Box<str>,
	},
	/// Removes a graph-owned user definition.
	RemoveGraphIndex {
		if_exists: bool,
		graph: Box<str>,
		name: Box<str>,
	},
	RemoveParameter {
		if_exists: bool,
		name: Box<str>,
	},
	RemoveFunction {
		if_exists: bool,
		namespace: Box<str>,
		name: Box<str>,
		signature: Option<Vec<Type>>,
	},
	RemoveAnalyzer {
		if_exists: bool,
		name: Box<str>,
	},
	AlterClassMode {
		if_exists: bool,
		name: Box<str>,
		mode: ClassSchemaMode,
	},
	/// Changes only the capture policy of an existing catalog owner.
	AlterChangefeed {
		/// Selects a graph owner instead of a global class owner.
		graph: bool,
		if_exists: bool,
		name: Box<str>,
		/// A validated policy, or None for DROP CHANGEFEED.
		configuration: Option<crate::changefeed::ChangefeedConfig>,
	},
	/// Reads metadata by the runtime retained feed UUID.
	InfoChangefeed {
		/// Runtime feed identity.
		id: TypedExpr,
		/// Bounded source spelling owned by the analyzer, independent of
		/// callers.
		spelling: Box<str>,
	},
	/// Reads whole commits through the public changefeed reader.
	ShowChanges {
		/// Runtime or retained-name source binding.
		target: LogicalChangefeedTarget,
		/// Exact exclusive stamp, inclusive time, or opaque cursor expression.
		since: TypedExpr,
		/// Requires historical FULL coverage before any pull.
		full: bool,
		/// Whole-commit/GAP element limit.
		limit: Option<TypedExpr>,
		/// Bounded operand source owned by the analyzer, independent of
		/// callers.
		spelling: Box<str>,
	},
	AlterFieldType {
		if_exists: bool,
		class: Box<str>,
		name: Box<str>,
		value_type: Type,
	},
	PrepareIndexRemoval {
		class: Box<str>,
		name: Box<str>,
	},
	AlterParameter {
		if_exists: bool,
		name: Box<str>,
		value: Option<TypedExpr>,
		comment: Option<Option<Text>>,
	},
	AlterFunction {
		if_exists: bool,
		namespace: Box<str>,
		name: Box<str>,
		signature: Option<Vec<Type>>,
		effect: Option<FunctionEffect>,
		cost: Option<bool>,
		max_depth: Option<Option<u8>>,
		body: Option<Text>,
		comment: Option<Option<Text>>,
	},
	AlterAnalyzer {
		if_exists: bool,
		name: Box<str>,
		function: Option<Option<(crate::datastore::FunctionId, crate::datastore::SchemaVersion)>>,
		tokenizers: Option<Vec<crate::datastore::AnalyzerTokenizer>>,
		filters: Option<Vec<crate::datastore::AnalyzerFilter>>,
		comment: Option<Option<Text>>,
	},
	InfoDatabase,
	InfoAnalyzer {
		name: Box<str>,
	},
	InfoClass {
		name: Box<str>,
	},
	/// Read-only graph metadata and its source-owned event definitions.
	InfoGraph {
		/// Case-sensitive graph name in the statement snapshot.
		name: Box<str>,
	},
	InfoIndex {
		class: Box<str>,
		name: Box<str>,
	},
	/// Observes a graph-owned definition.
	InfoGraphIndex {
		graph: Box<str>,
		name: Box<str>,
	},
	InfoParameter {
		name: Box<str>,
	},
	InfoFunction {
		namespace: Box<str>,
		name: Box<str>,
		signature: Option<Vec<Type>>,
	},
}

/// One catalog-owned graph-property mutation.
#[derive(Debug, Clone)]
pub(super) enum LogicalGraphPropertyMutation {
	Set(Vec<(Vec<Box<str>>, TypedExpr)>),
	Unset(Vec<Vec<Box<str>>>),
}

/// History binding independent of current schema-name resolution.
#[derive(Debug, Clone)]
pub(super) enum LogicalChangefeedTarget {
	/// Active-name-first lookup through the retained feed catalog.
	Named(crate::changefeed::ChangefeedSelector),
	/// A graph reference or graph-name value evaluated by the ordinary
	/// evaluator.
	Graph(TypedExpr),
	/// A runtime UUID value evaluated by the ordinary evaluator.
	Id(TypedExpr),
}

impl LogicalSchemaStatement {
	/// Distinguishes history/catalog reads from atomic schema mutations.
	pub(super) const fn is_read(&self) -> bool {
		matches!(
			self.statement_kind(),
			crate::gql::QueryStatementKind::Info | crate::gql::QueryStatementKind::ShowChanges
		)
	}

	pub(super) const fn statement_kind(&self) -> crate::gql::QueryStatementKind {
		match self {
			Self::Event(statement) => statement.statement_kind(),
			Self::RegisterLive(_) | Self::DefineLiveBinding(_) => {
				crate::gql::QueryStatementKind::Live
			}
			Self::AckLive {
				..
			} => crate::gql::QueryStatementKind::Ack,
			Self::CloseLive {
				..
			} => crate::gql::QueryStatementKind::Kill,
			Self::DefineGraph {
				..
			}
			| Self::DefineClass {
				..
			}
			| Self::DefineField {
				..
			}
			| Self::DefineIndex {
				..
			}
			| Self::DefineGraphIndex {
				..
			}
			| Self::DefineAnalyzer {
				..
			}
			| Self::DefineParameter {
				..
			}
			| Self::DefineFunction {
				..
			} => crate::gql::QueryStatementKind::Define,
			Self::UpdateGraph {
				..
			} => crate::gql::QueryStatementKind::Update,
			Self::RemoveLiveBinding {
				..
			}
			| Self::RemoveGraph {
				..
			}
			| Self::RemoveClass {
				..
			}
			| Self::RemoveField {
				..
			}
			| Self::RemoveIndex {
				..
			}
			| Self::RemoveGraphIndex {
				..
			}
			| Self::RemoveParameter {
				..
			}
			| Self::RemoveFunction {
				..
			}
			| Self::RemoveAnalyzer {
				..
			} => crate::gql::QueryStatementKind::Remove,
			Self::ReplaceLiveBinding {
				..
			}
			| Self::AlterClassMode {
				..
			}
			| Self::AlterChangefeed {
				..
			}
			| Self::AlterFieldType {
				..
			}
			| Self::PrepareIndexRemoval {
				..
			}
			| Self::AlterParameter {
				..
			}
			| Self::AlterFunction {
				..
			}
			| Self::AlterAnalyzer {
				..
			} => crate::gql::QueryStatementKind::Alter,
			Self::ShowChanges {
				..
			} => crate::gql::QueryStatementKind::ShowChanges,
			Self::InfoChangefeed {
				..
			}
			| Self::InfoLive {
				..
			}
			| Self::InfoLiveBinding {
				..
			}
			| Self::InfoDatabase
			| Self::InfoAnalyzer {
				..
			}
			| Self::InfoClass {
				..
			}
			| Self::InfoGraph {
				..
			}
			| Self::InfoIndex {
				..
			}
			| Self::InfoGraphIndex {
				..
			}
			| Self::InfoParameter {
				..
			}
			| Self::InfoFunction {
				..
			} => crate::gql::QueryStatementKind::Info,
		}
	}
}

/// Normalized graph family and orthogonal topology validators.
#[derive(Debug, Clone, Copy)]
pub(super) struct LogicalGraphContract {
	pub(super) family: Option<GraphKind>,
	pub(super) validators: GraphTopologyConstraints,
}

/// Typed selectors used by an owned graph result.
#[derive(Debug, Clone)]
pub(super) struct LogicalReturnGraph {
	/// Optional adapter resolving external relation recipes into local atoms.
	pub(super) bridge: Option<Box<str>>,
	pub(super) contract: LogicalGraphContract,
	pub(super) root: Option<TypedExpr>,
	pub(super) nodes: TypedExpr,
	pub(super) edges: TypedExpr,
}

impl LogicalReturnGraph {
	/// Traverses selector children in their evaluation order.
	pub(super) fn expressions(&self) -> impl Iterator<Item = &TypedExpr> {
		std::iter::once(&self.nodes).chain(std::iter::once(&self.edges)).chain(self.root.iter())
	}
}

/// Typed source consumed by `BUILD GRAPH`.
#[derive(Debug, Clone)]
pub(super) enum LogicalBuildGraphSource {
	Match(Box<LogicalMatch>),
	Select(Box<LogicalSelect>),
	Traverse(Box<LogicalTraverse>),
	Expression(TypedExpr),
}

/// One anonymous materialization or named atomic graph installation.
#[derive(Debug, Clone)]
pub(super) struct LogicalBuildGraph {
	/// Optional adapter resolving external relation recipes before assembly.
	pub(super) bridge: Option<Box<str>>,
	pub(super) mode: DefinitionMode,
	pub(super) contract: LogicalGraphContract,
	pub(super) contract_explicit: bool,
	pub(super) name: Option<Box<str>>,
	pub(super) source: LogicalBuildGraphSource,
	pub(super) root: Option<TypedExpr>,
	pub(super) nodes: Option<TypedExpr>,
	pub(super) edges: Option<TypedExpr>,
	pub(super) limit_nodes: Option<u64>,
	pub(super) limit_edges: Option<u64>,
}

#[derive(Debug, Clone)]
/// An analyzed MATCH statement with a snapshot-bound catalog graph.
pub(super) struct LogicalMatch {
	/// Static identity or lexical GraphRef operand resolved before planning.
	pub(super) graph: LogicalGraphReference,
	/// Invocation deadline retained for MATCH embedded in another statement.
	pub(super) timeout: Option<LogicalDuration>,
	/// Whether an unmatched input row survives with new aliases set to none.
	pub(super) optional: bool,
	/// Whether complete binding tuples are deduplicated before projection.
	pub(super) binding_distinct: bool,
	/// Clause-wide atom uniqueness semantics.
	pub(super) mode: crate::syn::MatchMode,
	/// Whether linear endpoint positions use AtomWalk semantics.
	pub(super) atom: bool,
	/// Clause-wide meta requirement retained for planning and EXPLAIN.
	pub(super) meta_scope: crate::syn::MatchMetaScope,
	/// Analyzed patterns in their source order.
	pub(super) patterns: Vec<LogicalMatchPattern>,
	/// Columns bound by aliases in all patterns.
	pub(super) bindings: Schema,
	/// Optional row predicate.
	pub(super) filter: Option<TypedExpr>,
	/// Expressions evaluated for each surviving row.
	pub(super) projection: Vec<TypedExpr>,
	/// Terminal graph assembly, mutually exclusive with ordinary projection.
	pub(super) return_graph: Option<LogicalReturnGraph>,
	/// Whether projected values are deduplicated.
	pub(super) projection_distinct: bool,
	/// Ordering applied after projection.
	pub(super) order: Vec<LogicalOrder>,
	/// Fixed clause maximum, or the checked upper bound for a deferred operand.
	pub(super) max_depth: u32,
	/// Lexical operand resolved against the invocation snapshot before
	/// planning.
	pub(super) max_depth_binding: Option<TypedExpr>,
	/// Public result-column schema.
	pub(super) result: Schema,
}

#[derive(Debug, Clone)]
/// One linear MATCH pattern before runtime reordering.
pub(super) struct LogicalMatchPattern {
	/// Concrete graph relation executed for this source pattern.
	pub(super) kind: LogicalMatchPatternKind,
	/// Stable source-order tie breaker for the optimizer.
	pub(super) original_index: usize,
}

#[derive(Debug, Clone)]
/// One typed graph relation in a MATCH clause.
pub(super) enum LogicalMatchPatternKind {
	/// StandardPath or AtomWalk chain.
	Linear(LogicalLinearPattern),
	/// Hyperedge endpoint assignment.
	Hyper(LogicalHyperPattern),
	/// Direct or transitive graph containment join.
	Within(LogicalWithinPattern),
}

#[derive(Debug, Clone)]
/// One linear StandardPath or AtomWalk chain.
pub(super) struct LogicalLinearPattern {
	/// Optional column receiving the complete linear path.
	pub(super) path_column: Option<usize>,
	/// Initial endpoint binding.
	pub(super) start: MatchBinding,
	/// Linear expansion operations.
	pub(super) ops: Vec<MatchOp>,
}

#[derive(Debug, Clone)]
/// One hyperedge and its typed endpoint groups.
pub(super) struct LogicalHyperPattern {
	pub(super) edge: MatchBinding,
	pub(super) contains: Vec<LogicalMatchContains>,
}

#[derive(Debug, Clone)]
/// One typed HEAD, TAIL, or unqualified endpoint group.
pub(super) struct LogicalMatchContains {
	pub(super) side: crate::syn::MatchHyperSide,
	pub(super) endpoints: Vec<MatchBinding>,
}

#[derive(Debug, Clone)]
/// One direct or bounded transitive containment relation.
pub(super) struct LogicalWithinPattern {
	pub(super) child: MatchBinding,
	pub(super) owner: MatchBinding,
	pub(super) min: u32,
	pub(super) max: u32,
}

#[derive(Debug, Clone)]
/// A linear MATCH expansion operation.
pub(super) enum MatchOp {
	/// Expands through exactly one connector edge.
	Expand(Expand),
	/// Expands through a finite TRAIL path.
	PathExpand(PathExpand),
}

#[derive(Debug, Clone)]
/// A fixed-length connector expansion.
pub(super) struct Expand {
	/// Source endpoint binding.
	pub(super) source: MatchBinding,
	/// Connector-edge binding.
	pub(super) edge: MatchBinding,
	/// Target endpoint binding.
	pub(super) target: MatchBinding,
	/// Required traversal direction.
	pub(super) direction: crate::syn::MatchDirection,
	/// Captured PURE contribution; this one-hop segment starts at zero.
	pub(super) custom_cost: Option<LogicalPathCost>,
	/// Candidate-edge filters inherited from the selected constraint scope.
	pub(super) filters: Vec<crate::syn::MatchPathFilter>,
}

#[derive(Debug, Clone)]
/// A bounded variable-length TRAIL expansion.
pub(super) struct PathExpand {
	/// Source endpoint binding.
	pub(super) source: MatchBinding,
	/// Connector-edge constraints applied to every hop.
	pub(super) edge: MatchBinding,
	/// Target endpoint binding.
	pub(super) target: MatchBinding,
	/// Required traversal direction.
	pub(super) direction: crate::syn::MatchDirection,
	/// Inclusive minimum hop count.
	pub(super) min: u32,
	/// Inclusive maximum hop count.
	pub(super) max: u32,
	/// Whether an open range inherits the invocation's clause maximum.
	pub(super) inherits_max_depth: bool,
	/// Per-path uniqueness mode after local/global override.
	pub(super) mode: crate::syn::MatchPathMode,
	/// Optional shortest-path objective after local/global override.
	pub(super) objective: Option<crate::syn::MatchPathObjective>,
	/// Captured PURE COST identity and additive/stateful invocation contract.
	pub(super) custom_cost: Option<LogicalPathCost>,
	/// Candidate-edge filters after local/global override.
	pub(super) filters: Vec<crate::syn::MatchPathFilter>,
	/// Optional column receiving the public path value.
	pub(super) path_column: Option<usize>,
}

impl PathExpand {
	/// Resolves only open ranges; explicit segment bounds remain independent.
	pub(super) const fn maximum(&self, clause_depth: u32) -> u32 {
		if self.inherits_max_depth {
			clause_depth
		} else {
			self.max
		}
	}
}

/// One versioned path contribution; stateful functions receive the current
/// segment prefix and remain exact traversal consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct LogicalPathCost {
	/// Published identity shared with the ordinary named-function cache.
	pub(super) function: NamedFunctionKey,
	/// Whether a second numeric prefix argument is required.
	pub(super) stateful: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Runtime domain accepted by a MATCH endpoint.
pub(super) enum EndpointDomain {
	/// Node atoms only.
	Node,
	/// Edge atoms only.
	Edge,
}

impl EndpointDomain {
	pub(super) const fn atom_kind(self) -> crate::values::AtomKind {
		match self {
			Self::Node => crate::values::AtomKind::Node,
			Self::Edge => crate::values::AtomKind::Edge,
		}
	}
}

#[derive(Debug, Clone)]
/// Alias and label constraints for one MATCH atom.
pub(super) struct MatchBinding {
	/// Required atom domain.
	pub(super) domain: EndpointDomain,
	/// Global class or exact-record selector.
	pub(super) selector: LogicalMatchSelector,
	/// Optional binding-column index.
	pub(super) column: Option<usize>,
	/// Optional graph-local label predicate.
	pub(super) label: Option<Box<str>>,
	/// Inline effective-property equality predicates.
	pub(super) properties: Vec<(Box<str>, TypedExpr)>,
	/// Inline predicate evaluated after this position is bound.
	pub(super) predicate: Option<TypedExpr>,
	/// Whether this position requires the corresponding meta binding.
	pub(super) meta: bool,
}

#[derive(Debug, Clone)]
/// Global selector resolved before graph-local label filtering.
pub(super) enum LogicalMatchSelector {
	/// Any atom in the endpoint domain.
	Any,
	/// Any atom of one global class.
	Class(Box<str>),
	/// One exact global record identity.
	Record(LogicalRecordTarget),
}

#[derive(Debug, Clone)]
pub(super) struct LogicalSelect {
	/// Cold graph selectors stay outside ordinary SELECT storage.
	pub(super) return_graph: Option<Box<LogicalReturnGraph>>,
	/// Independent SDK source plans share ordinary SELECT evaluation.
	pub(super) union_inputs: Vec<Self>,
	/// A private typing family becomes one JOIN input before relational work.
	pub(super) union_purpose: crate::syn::SelectUnionPurpose,
	/// Logical lexical origin proves identical independent subqueries.
	pub(super) lexical_id: Option<u32>,
	pub(super) source: LogicalSelectSource,
	pub(super) joins: Vec<LogicalSelectJoin>,
	pub(super) access: LogicalSelectAccess,
	pub(super) input_schema: Schema,
	pub(super) filter: Option<TypedExpr>,
	pub(super) split: Vec<TypedExpr>,
	pub(super) group: Option<Vec<TypedExpr>>,
	pub(super) projection: Vec<LogicalSelectItem>,
	/// Optional SDK output paths, parallel to projection; empty for ordinary
	/// aliases.
	pub(super) projection_paths: Vec<Option<Vec<Box<str>>>>,
	pub(super) result: Schema,
	pub(super) omit: Vec<Vec<Box<str>>>,
	pub(super) fetch: Vec<Vec<Box<str>>>,
	/// Optional adapter aliases parallel to the ordinary FETCH paths.
	pub(super) fetch_bridges: Option<Box<[Option<Box<str>>]>>,
	pub(super) order: Vec<LogicalOrder>,
	pub(super) start: Option<LogicalSelectStart>,
	pub(super) limit: Option<TypedExpr>,
	pub(super) value: bool,
	pub(super) only: bool,
	pub(super) tempfiles: bool,
	pub(super) outer_columns: usize,
}

impl LogicalSelect {
	/// Terminal results expose one graph column; selectors retain the projected
	/// schema.
	pub(super) fn output_schema(&self) -> Schema {
		self.return_graph.as_ref().map_or_else(
			|| self.result.clone(),
			|graph| Schema {
				columns: vec![Column {
					name: "graph".into(),
					qualifier: None,
					ty: Type::Graph(graph.contract.family.unwrap_or(GraphKind::Subgraph)),
				}],
				origins: std::collections::BTreeMap::new(),
			},
		)
	}
}

/// Typed source consumed by a traversal pipeline.
#[derive(Debug, Clone)]
pub(super) enum LogicalTraverseSource {
	/// A collection, atom, occurrence, graph, or range expression.
	Expression(TypedExpr),
	/// Existing MATCH execution used as a correlated traversal source.
	Match(Box<LogicalMatch>),
	/// Existing SELECT execution used as a traversal source.
	Select(Box<LogicalSelect>),
}

/// One statically known `tv::` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TraverseStepKind {
	Vertices,
	Edges,
	Out,
	In,
	Both,
	OutEdges,
	InEdges,
	BothEdges,
	InVertex,
	OutVertex,
	Hyper,
	MetaInner,
	HasAttribute,
	Has,
	HasLabel,
	Where,
	Deduplicate,
	DeduplicateBy,
	SimplePath,
	CyclicPath,
	Map,
	FlatMap,
	Algorithm,
	Values,
	Select,
	Match,
	As,
	Project,
	Unfold,
	InitializeSack,
	Sack,
	GetSack,
	InitializeNamedSack,
	NamedSack,
	GetNamedSack,
	SideEffect,
	Path,
	Emit,
	Limit,
	Skip,
	Repeat,
	Branch,
	Choose,
	Coalesce,
	Order,
	Group,
	Fold,
	Count,
	Sum,
	Minimum,
	Maximum,
	Average,
	KHop,
	Reachable,
	ShortestPath,
	WeightedShortestPath,
	DistanceTo,
	BellmanFord,
	TopologicalSort,
	StronglyConnectedComponents,
	WeaklyConnectedComponents,
	Condensation,
	MinimumSpanningForest,
	Cuts,
	LowestCommonAncestor,
	HasCycle,
	/// Explicit approximate distinct count within the given hop radius.
	KHopEstimate,
	/// Normalized sparse PageRank over induced qualified occurrences.
	PageRank,
	/// Exact in/out/total degree and stored connector strength.
	Degree,
	/// Reciprocal finite distances with unreachable occurrences contributing
	/// zero.
	HarmonicCloseness,
	/// Full-source Brandes centrality with parallel shortest multiplicity.
	Betweenness,
	/// Shifted sparse power iteration with original eigenproblem residual.
	Eigenvector,
	/// Contractive Katz iteration with an admitted norm bound.
	Katz,
	/// Exact binary undirected and directed triangle counts.
	Triangles,
	/// Local, mean and global clustering coefficients.
	Clustering,
	/// Canonical sequential weighted or unweighted community voting.
	LabelPropagation,
	/// Positive-gain local moves and deterministic community aggregation.
	Louvain,
	/// Complete minimal hop paths for each qualified source-target seed.
	AllShortestPaths,
	/// Complete minimal stored or PURE COST paths using exact Number ordering.
	AllWeightedShortestPaths,
}

impl TraverseStepKind {
	/// Identifies the qualified graph-wide tensor consumers.
	pub(super) const fn is_tensor_analytics(self) -> bool {
		matches!(
			self,
			Self::PageRank
				| Self::Degree
				| Self::HarmonicCloseness
				| Self::Betweenness
				| Self::Eigenvector
				| Self::Katz
				| Self::Triangles
				| Self::Clustering
				| Self::LabelPropagation
				| Self::Louvain
		)
	}

	pub(super) const fn is_barrier(self) -> bool {
		self.is_tensor_analytics()
			|| matches!(
				self,
				Self::Deduplicate
					| Self::DeduplicateBy
					| Self::Order
					| Self::Group
					| Self::Fold
					| Self::Count
					| Self::Sum
					| Self::Minimum
					| Self::Maximum
					| Self::Average
					| Self::KHop
					| Self::KHopEstimate
					| Self::Reachable
					| Self::ShortestPath
					| Self::WeightedShortestPath
					| Self::AllShortestPaths
					| Self::AllWeightedShortestPaths
					| Self::DistanceTo
					| Self::BellmanFord
					| Self::TopologicalSort
					| Self::StronglyConnectedComponents
					| Self::WeaklyConnectedComponents
					| Self::Condensation
					| Self::MinimumSpanningForest
					| Self::Cuts
					| Self::LowestCommonAncestor
					| Self::HasCycle
			)
	}
}

/// One analyzed traversal step in source order.
#[derive(Debug, Clone)]
pub(super) struct LogicalTraverseStep {
	pub(super) kind: TraverseStepKind,
	pub(super) arguments: Vec<TypedExpr>,
	/// Recursive `tv::algo` target resolved through the named-function cache.
	pub(super) named_function: Option<NamedFunctionKey>,
	/// Target type of the implicit first `tv::algo` argument.
	pub(super) algorithm_input: Option<Type>,
	/// Statically resolved type produced by `tv::algo`.
	pub(super) output_type: Option<Type>,
	pub(super) match_plan: Option<Box<LogicalMatch>>,
	pub(super) repeat: Option<LogicalRepeat>,
	pub(super) span: TextSpan,
}

/// Normalized modifiers of `tv::repeat(body)`.
#[derive(Debug, Clone)]
pub(super) struct LogicalRepeat {
	pub(super) body: TypedExpr,
	pub(super) times: Option<TypedExpr>,
	pub(super) until: Option<TypedExpr>,
	pub(super) emit: bool,
}

/// One complete streaming traversal statement.
#[derive(Debug, Clone)]
pub(super) struct LogicalTraverse {
	pub(super) atom: bool,
	pub(super) constraints: crate::syn::MatchConstraints,
	pub(super) source: LogicalTraverseSource,
	pub(super) graph: Option<LogicalGraphReference>,
	/// Shared static contribution used by navigation and weighted path steps.
	pub(super) custom_cost: Option<LogicalPathCost>,
	pub(super) steps: Vec<LogicalTraverseStep>,
	/// Columns available to the final RETURN expression in runtime row order.
	pub(super) result: Schema,
	pub(super) returning: Option<TypedExpr>,
	pub(super) return_graph: Option<LogicalReturnGraph>,
}

#[derive(Debug, Clone, Default)]
pub(super) enum LogicalSelectAccess {
	#[default]
	Automatic,
	NoIndex,
	Indexes(Vec<LogicalIndexHint>),
}

#[derive(Debug, Clone)]
pub(super) struct LogicalIndexHint {
	pub(super) source: Option<Box<str>>,
	pub(super) index: Box<str>,
}

#[derive(Debug, Clone)]
pub(super) struct LogicalSelectSource {
	pub(super) kind: LogicalSelectSourceKind,
	pub(super) schema: Schema,
	pub(super) wildcard: Vec<LogicalWildcardField>,
	pub(super) alias: Box<str>,
}

#[derive(Debug, Clone)]
pub(super) enum LogicalSelectSourceKind {
	/// A zero-row SDK relation still validates its optional graph scope.
	Empty {
		/// Graph source identity is checked even when there are no classes.
		graph: Option<LogicalGraphReference>,
	},
	Atoms {
		target: ScanTarget,
		graph: Option<LogicalGraphReference>,
	},
	/// One qualified raw relation retains class-local cells and a hidden tag.
	AtomUnion {
		/// Canonical distinct class scans; point sources retain ordinary Atoms.
		targets: Vec<ScanTarget>,
		/// One once-resolved scope shared by all inputs.
		graph: Option<LogicalGraphReference>,
		/// Last cell of the padded source layout, excluded from names/output.
		tag_column: usize,
		/// Analyzer-only state; None types an unmatched LEFT slot.
		typing: Option<usize>,
	},
	Graphs {
		references: Option<Vec<LogicalGraphReference>>,
	},
	Derived(Box<LogicalSelect>),
}

impl LogicalSelectSourceKind {
	/// Borrows an occurrence scope, including a zero-row union wrapper.
	pub(super) fn graph_scope(&self) -> Option<&LogicalGraphReference> {
		match self {
			Self::Empty {
				graph,
			}
			| Self::Atoms {
				graph,
				..
			}
			| Self::AtomUnion {
				graph,
				..
			} => graph.as_ref(),
			Self::Graphs {
				..
			}
			| Self::Derived(_) => None,
		}
	}

	/// Replaces an admitted occurrence scope without changing metadata sources.
	pub(super) fn graph_scope_mut(&mut self) -> Option<&mut LogicalGraphReference> {
		match self {
			Self::Empty {
				graph,
			}
			| Self::Atoms {
				graph,
				..
			}
			| Self::AtomUnion {
				graph,
				..
			} => graph.as_mut(),
			Self::Graphs {
				..
			}
			| Self::Derived(_) => None,
		}
	}
}

#[derive(Debug, Clone)]
pub(super) enum LogicalGraphReference {
	Static(GraphRef),
	Expression(TypedExpr),
}

#[derive(Debug, Clone)]
pub(super) struct LogicalSelectJoin {
	pub(super) kind: crate::syn::SelectJoinKind,
	pub(super) source: LogicalSelectSource,
	pub(super) predicate: Option<TypedExpr>,
}

#[derive(Debug, Clone)]
pub(super) enum LogicalSelectItem {
	Wildcard(Vec<LogicalWildcardField>),
	/// Uses the ordinary wildcard projector for one class typing state.
	WildcardVariants {
		/// Lazy class tuple CASE returning a checked field-list ordinal.
		selector: TypedExpr,
		/// Original ordinary wildcard lists, in analyzer variant order.
		choices: Vec<Vec<LogicalWildcardField>>,
	},
	Expression {
		name: Box<str>,
		expression: TypedExpr,
	},
}

#[derive(Debug, Clone)]
pub(super) enum LogicalWildcardField {
	Column {
		name: Box<str>,
		index: usize,
	},
	Properties {
		index: usize,
	},
}

#[derive(Debug, Clone)]
pub(super) struct LogicalOrder {
	pub(super) expression: TypedExpr,
	pub(super) random: bool,
	pub(super) collate: bool,
	pub(super) numeric: bool,
	pub(super) direction: crate::syn::SelectOrderDirection,
	pub(super) nulls: crate::syn::SelectNulls,
}

#[derive(Debug, Clone)]
pub(super) enum LogicalSelectStart {
	Offset(TypedExpr),
	After {
		cursor: TypedExpr,
		read_committed: bool,
	},
}

#[derive(Debug, Clone)]
pub(super) enum ScanTarget {
	Class(Box<str>),
	Record(RecordId),
}

impl ScanTarget {
	pub(super) fn class(&self) -> &str {
		match self {
			Self::Class(class) => class,
			Self::Record(record) => record.class(),
		}
	}
}

#[derive(Debug, Clone, Default)]
pub(super) struct Schema {
	pub(super) columns: Vec<Column>,
	pub(super) origins: BTreeMap<usize, ColumnOrigin>,
}

impl Schema {
	pub(super) fn index_of(&self, name: &str) -> Option<usize> {
		self.columns.iter().position(|column| {
			!column.name.starts_with('\0') && column.name.eq_ignore_ascii_case(name)
		})
	}

	pub(super) fn matching_indices<'a>(
		&'a self,
		name: &'a str,
	) -> impl Iterator<Item = usize> + 'a {
		self.columns.iter().enumerate().filter_map(move |(index, column)| {
			(!column.name.starts_with('\0') && column.name.eq_ignore_ascii_case(name))
				.then_some(index)
		})
	}

	pub(super) fn qualified_index(&self, qualifier: &str, name: &str) -> Option<usize> {
		self.columns.iter().position(|column| {
			!column.name.starts_with('\0')
				&& column
					.qualifier
					.as_deref()
					.is_some_and(|value| value.eq_ignore_ascii_case(qualifier))
				&& column.name.eq_ignore_ascii_case(name)
		})
	}
}

/// Durable field identity retained while relational aliases and joins reshape
/// the visible row schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ColumnOrigin {
	pub(super) source: Box<str>,
	pub(super) class: Box<str>,
	pub(super) field: Box<str>,
	/// `None` denotes a dynamic graph scope whose identity is resolved at
	/// statement execution.
	pub(super) scope: Option<IndexScope>,
}

#[derive(Debug, Clone)]
pub(super) struct Column {
	pub(super) name: Box<str>,
	pub(super) qualifier: Option<Box<str>>,
	pub(super) ty: Type,
}

#[derive(Debug, Clone)]
pub(super) struct TypedExpr {
	pub(super) kind: TypedExprKind,
	pub(super) ty: Type,
	pub(super) span: TextSpan,
}

/// Statically classified side effects of one language closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ClosureEffect {
	Pure,
	StableRead,
	DataMutation,
	SchemaMutation,
}

/// Typed closure body executed by the session-owned closure registry.
#[derive(Debug, Clone)]
pub(super) enum LogicalClosureBody {
	Expression(Box<TypedExpr>),
	Block(LogicalBlock),
}

/// Session-scoped closure with explicit capture and invocation slots.
#[derive(Debug, Clone)]
pub(super) struct LogicalClosure {
	pub(super) parameters: Vec<(u32, Type)>,
	pub(super) captures: Vec<u32>,
	pub(super) result: Type,
	pub(super) body: LogicalClosureBody,
	pub(super) effect: ClosureEffect,
	pub(super) slot_count: usize,
	/// Conservative retained size of the typed closure plan.
	pub(super) retained_plan_bytes: usize,
	pub(super) named_function: Option<NamedFunctionKey>,
}

impl LogicalClosure {
	/// Named invocation preserves the published declaration, including
	/// callbacks.
	pub(in crate::gql) const fn invocation_effect(&self) -> ClosureEffect {
		match self.named_function {
			Some(function) => match function.effect {
				FunctionEffect::Pure => ClosureEffect::Pure,
				FunctionEffect::Stable => ClosureEffect::StableRead,
				FunctionEffect::Impure => ClosureEffect::DataMutation,
			},
			None => self.effect,
		}
	}
}

/// Session-cache identity and recursion contract of one compiled named UDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct NamedFunctionKey {
	pub(super) id: FunctionId,
	pub(super) version: SchemaVersion,
	pub(super) schema_epoch: u64,
	pub(super) max_depth: Option<u8>,
	pub(super) effect: FunctionEffect,
}

#[derive(Debug, Clone)]
pub(super) enum TypedExprKind {
	Constant(Value),
	Slot(u32),
	Column(usize),
	Array(Vec<TypedExpr>),
	Set(Vec<TypedExpr>),
	Object(Vec<(Box<str>, TypedExpr)>),
	Range {
		start: Option<Box<TypedExpr>>,
		end: Option<Box<TypedExpr>>,
		inclusive_end: bool,
	},
	Unary {
		op: crate::syn::UnaryOperator,
		expr: Box<TypedExpr>,
	},
	Binary {
		left: Box<TypedExpr>,
		op: crate::syn::BinaryOperator,
		right: Box<TypedExpr>,
	},
	Fulltext {
		left: Box<TypedExpr>,
		op: crate::syn::FulltextOperator,
		right: Box<TypedExpr>,
		definitions: Vec<LogicalFulltextDefinition>,
	},
	Knn {
		left: Box<TypedExpr>,
		origin: ColumnOrigin,
		k: Box<TypedExpr>,
		ef: Option<Box<TypedExpr>>,
		distance: LogicalKnnDistance,
		right: Box<TypedExpr>,
		definitions: Vec<LogicalVectorDefinition>,
	},
	Conditional {
		condition: Box<TypedExpr>,
		then_value: Box<TypedExpr>,
		else_value: Box<TypedExpr>,
	},
	Case {
		operand: Option<Box<TypedExpr>>,
		branches: Vec<(TypedExpr, TypedExpr)>,
		else_value: Option<Box<TypedExpr>>,
	},
	Field {
		base: Box<TypedExpr>,
		field: Box<str>,
		null_safe: bool,
	},
	CurrentField(Box<str>),
	Destructure {
		base: Box<TypedExpr>,
		fields: Vec<Box<str>>,
	},
	Index {
		base: Box<TypedExpr>,
		index: Box<TypedExpr>,
	},
	Filter {
		base: Box<TypedExpr>,
		predicate: Box<TypedExpr>,
	},
	Builtin {
		function: Builtin,
		arguments: Vec<TypedExpr>,
	},
	Closure(Box<LogicalClosure>),
	Invoke {
		callee: Box<TypedExpr>,
		arguments: Vec<TypedExpr>,
	},
	NamedInvoke {
		function: NamedFunctionKey,
		arguments: Vec<TypedExpr>,
	},
	/// A generic expression pipeline lowered to the traversal VM.
	Traversal(Box<LogicalTraverse>),
	Aggregate {
		function: Aggregate,
		argument: Option<Box<TypedExpr>>,
		distinct: bool,
	},
	Subquery {
		select: Box<LogicalSelect>,
		collection: bool,
	},
	Cast {
		target: Type,
		expr: Box<TypedExpr>,
	},
	/// Reads the value of a lexical reactive identity from this exact snapshot.
	LiveSlot(u32),
}

/// Snapshot-exact Fulltext definition selected during semantic analysis.
#[derive(Debug, Clone)]
pub(super) struct LogicalFulltextDefinition {
	pub(super) class: Box<str>,
	pub(super) field: Box<str>,
	pub(super) source: Box<str>,
	pub(super) scope: Option<IndexScope>,
	pub(super) index: IndexId,
	pub(super) name: Text,
	pub(super) state: ClassIndexState,
	pub(super) analyzer: Arc<SchemaAnalyzer>,
	pub(super) analyzer_function: Option<NamedFunctionKey>,
	pub(super) k1: f64,
	pub(super) b: f64,
	pub(super) highlights: bool,
}

/// Exact KNN distance or a typed callable evaluated by the query runtime.
#[derive(Debug, Clone)]
pub(super) enum LogicalKnnDistance {
	Builtin(VectorDistance),
	Minkowski(Box<TypedExpr>),
	Named(NamedFunctionKey),
	Callable(Box<TypedExpr>),
}

/// One compatible durable ANN definition visible to planning.
#[derive(Debug, Clone)]
pub(super) struct LogicalVectorDefinition {
	pub(super) class: Box<str>,
	pub(super) field: Box<str>,
	pub(super) source: Box<str>,
	pub(super) scope: Option<IndexScope>,
	pub(super) index: IndexId,
	pub(super) name: Text,
	pub(super) kind: ClassIndexKind,
	pub(super) state: ClassIndexState,
	pub(super) options: ClassIndexOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Aggregate {
	Count,
	Sum,
	Average,
	Minimum,
	Maximum,
	ArrayGroup,
	VariancePopulation,
	StandardDeviationPopulation,
	VarianceSample,
	StandardDeviationSample,
}

/// Statically resolved built-ins in the executable Stage 12 profile.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Builtin {
	MathAbs,
	MathSqrt,
	MathPow,
	MathFloor,
	MathCeil,
	StringUpper,
	StringLower,
	StringLen,
	StringContains,
	StringMatches,
	StringSlice,
	RegexMatcher,
	RegexNext,
	StringSplit,
	StringConcat,
	TimeNow(DateTime),
	TimeFormat,
	TimeAdd,
	TimeDiff,
	ArrayLen,
	ArrayConcat,
	ArrayDistinct,
	ArrayFirst,
	ArrayFlatten,
	ArrayPush(u32),
	ArrayPop(u32),
	ArrayClear(u32),
	SetLen,
	SetUnion,
	SetIntersect,
	SetDiff,
	SetHas,
	SetAdd(u32),
	SetClear(u32),
	TypeIs(Type),
	TypeTo(Type),
	MetaId,
	MetaClass,
	MetaType,
	GraphInner,
	GraphLevelPath,
	PathAtoms,
	PathNodes,
	PathEdges,
	PathLength,
	PathCost,
	OptionIsSome,
	OptionIsNone,
	Uuid,
	Ulid,
	RandId,
	SearchAnalyze {
		analyzer: Arc<SchemaAnalyzer>,
		function: Option<NamedFunctionKey>,
	},
	SearchScore(u16),
	SearchHighlight(u16),
	GeoWithin,
	GeoIntersects,
	GeoDistance,
	/// Streaming recomputation through a captured versioned PURE COST.
	PathCostWith(LogicalPathCost),
}
