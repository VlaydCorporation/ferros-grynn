use std::{collections::BTreeMap, sync::Arc, time::Duration};

use crate::{
	datastore::{
		ClassIndexKind,
		ClassIndexOptions,
		ClassIndexState,
		ClassSchemaMode,
		FieldUpdate,
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
/// Node id state.
pub(super) struct NodeId(
	/// Numeric identity reserved for a node in the logical query
	/// representation.
	pub(super) u32,
);

#[derive(Debug, Clone)]
/// Logical program state.
pub(super) struct LogicalProgram {
	/// Statements associated with this logical program.
	pub(super) statements: Vec<LogicalStatement>,
	/// Initial slots associated with this logical program.
	pub(super) initial_slots: Vec<(Box<str>, u32, bool)>,
	/// Slot count associated with this logical program.
	pub(super) slot_count: usize,
}

#[derive(Debug, Clone)]
/// Logical statement alternatives.
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
	/// Explain case of logical statement.
	Explain(
		/// Explain case of logical statement.
		LogicalExplain,
	),
	/// Schema case of logical statement.
	Schema(
		/// Schema case of logical statement.
		LogicalSchemaStatement,
	),
	/// Dml case of logical statement.
	Dml(
		/// Dml case of logical statement.
		LogicalDmlStatement,
	),
	/// Graph dml case of logical statement.
	GraphDml(
		/// Graph dml case of logical statement.
		LogicalGraphDmlStatement,
	),
	/// Let case of logical statement.
	Let {
		/// Name associated with this logical statement.
		name: Box<str>,
		/// Slot associated with this logical statement.
		slot: u32,
		/// Declared type, or the inferred initializer type when unannotated.
		expected: Type,
		/// Value associated with this logical statement.
		value: LogicalLetValue,
	},
	/// Return case of logical statement.
	Return {
		/// Carries the analyzed endpoint expression through ordinary execution.
		value: TypedExpr,
	},
	/// Return graph case of logical statement.
	ReturnGraph(
		/// Return graph case of logical statement.
		LogicalReturnGraph,
	),
	/// Expression case of logical statement.
	Expression {
		/// Carries the analyzed endpoint expression through ordinary execution.
		value: TypedExpr,
	},
	/// If case of logical statement.
	If {
		/// Branches associated with this logical statement.
		branches: Vec<(TypedExpr, LogicalBlock)>,
		/// Else block associated with this logical statement.
		else_block: Option<LogicalBlock>,
	},
	/// For case of logical statement.
	For {
		/// Slot associated with this logical statement.
		slot: u32,
		/// Iterable associated with this logical statement.
		iterable: TypedExpr,
		/// Body associated with this logical statement.
		body: LogicalBlock,
	},
	/// While case of logical statement.
	While {
		/// Condition associated with this logical statement.
		condition: TypedExpr,
		/// Body associated with this logical statement.
		body: LogicalBlock,
	},
	/// Break case of logical statement.
	Break,
	/// Continue case of logical statement.
	Continue,
	/// Try case of logical statement.
	Try {
		/// Body associated with this logical statement.
		body: LogicalBlock,
		/// Catch associated with this logical statement.
		catch: Option<(u32, LogicalBlock)>,
	},
	/// Throw case of logical statement.
	Throw {
		/// Carries the analyzed endpoint expression through ordinary execution.
		value: TypedExpr,
	},
	/// Sleep case of logical statement.
	Sleep(
		/// Sleep case of logical statement.
		LogicalDuration,
	),
	/// Assert invariants case of logical statement.
	AssertInvariants {
		/// Graph associated with this logical statement.
		graph: Option<GraphRef>,
		/// Report associated with this logical statement.
		report: usize,
	},
	/// Build graph case of logical statement.
	BuildGraph(
		/// Build graph case of logical statement.
		LogicalBuildGraph,
	),
	/// Executes a read-only graph pattern match.
	Match(
		/// Executes a read-only graph pattern match.
		LogicalMatch,
	),
	/// Select case of logical statement.
	Select(
		/// Select case of logical statement.
		LogicalSelect,
	),
	/// Executes a streaming traversal pipeline.
	Traverse(
		/// Executes a streaming traversal pipeline.
		LogicalTraverse,
	),
}

#[derive(Debug, Clone)]
/// Logical let value alternatives.
pub(super) enum LogicalLetValue {
	/// Expression case of logical let value.
	Expression(
		/// Expression case of logical let value.
		TypedExpr,
	),
	/// Cold graph construction owned by this lexical binding.
	BuildGraph(
		/// Cold graph construction owned by this lexical binding.
		Box<LogicalBuildGraph>,
	),
	/// Checked catalog command used only by LET ... = LIVE SELECT.
	Live(
		/// Checked catalog command used only by LET ... = LIVE SELECT.
		Box<LogicalSchemaStatement>,
	),
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
/// Logical explain state.
pub(super) struct LogicalExplain {
	/// Mode associated with this logical explain.
	pub(super) mode: crate::syn::ExplainMode,
	/// Format associated with this logical explain.
	pub(super) format: crate::syn::ExplainFormat,
	/// Statement associated with this logical explain.
	pub(super) statement: Box<LogicalStatement>,
}

#[derive(Debug, Clone)]
/// Logical block state.
pub(super) struct LogicalBlock {
	/// Statements associated with this logical block.
	pub(super) statements: Vec<LogicalBlockStatement>,
}

#[derive(Debug, Clone)]
/// Logical block statement state.
pub(super) struct LogicalBlockStatement {
	/// Statement associated with this logical block statement.
	pub(super) statement: LogicalStatement,
	/// Timeout associated with this logical block statement.
	pub(super) timeout: Option<LogicalDuration>,
}

/// A duration literal is fixed; a parameter retains its lexical provenance.
#[derive(Debug, Clone)]
pub(super) enum LogicalDuration {
	/// Literal duration validated by the existing fixed-duration parser.
	Fixed(
		/// Literal duration validated by the existing fixed-duration parser.
		Duration,
	),
	/// Checked Slot/LiveSlot with invocation-time value and source location.
	Parameter(
		/// Checked Slot/LiveSlot with invocation-time value and source
		/// location.
		TypedExpr,
	),
}

/// One analyzed graph-membership or graph-incidence mutation.
#[derive(Clone)]
pub(super) enum LogicalGraphDmlStatement {
	/// Attach case of logical graph dml statement.
	Attach {
		/// Record associated with this logical graph dml statement.
		record: LogicalRecordTarget,
		/// Graph associated with this logical graph dml statement.
		graph: GraphRef,
		/// Label associated with this logical graph dml statement.
		label: Option<Text>,
		/// Keep levelpath associated with this logical graph dml statement.
		keep_levelpath: bool,
	},
	/// Detach case of logical graph dml statement.
	Detach {
		/// Record associated with this logical graph dml statement.
		record: LogicalRecordTarget,
		/// Graph associated with this logical graph dml statement.
		graph: GraphRef,
		/// Cascade associated with this logical graph dml statement.
		cascade: bool,
	},
	/// Cold typed relation mutation.
	Relate(
		/// Cold typed relation mutation.
		Box<LogicalRelate>,
	),
}

/// Complete typed payload retained by one RELATE mutation.
#[derive(Debug, Clone)]
pub(super) struct LogicalRelate {
	/// Optional runtime adapter for post-commit relation mirroring.
	pub(super) bridge: Option<Box<str>>,
	/// Atom endpoints associated with this logical graph dml statement.
	pub(super) atom_endpoints: bool,
	/// Retains the analyzed source endpoint identity for ordinary RELATE.
	pub(super) source: LogicalRelateEndpoint,
	/// Edge associated with this logical graph dml statement.
	pub(super) edge: LogicalRelateEdge,
	/// Retains the analyzed target endpoint identity for ordinary RELATE.
	pub(super) target: LogicalRelateEndpoint,
	/// Graph associated with this logical graph dml statement.
	pub(super) graph: GraphRef,
	/// Label associated with this logical graph dml statement.
	pub(super) label: Option<Text>,
	/// Direction associated with this logical graph dml statement.
	pub(super) direction: MatchDirection,
	/// Weight associated with this logical graph dml statement.
	pub(super) weight: Option<TypedExpr>,
	/// Keep levelpath associated with this logical graph dml statement.
	pub(super) keep_levelpath: bool,
}

impl std::fmt::Debug for LogicalGraphDmlStatement {
	/// Preserves the original variant names and field order in diagnostics.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Attach {
				record,
				graph,
				label,
				keep_levelpath,
			} => f
				.debug_struct("Attach")
				.field("record", record)
				.field("graph", graph)
				.field("label", label)
				.field("keep_levelpath", keep_levelpath)
				.finish(),
			Self::Detach {
				record,
				graph,
				cascade,
			} => f
				.debug_struct("Detach")
				.field("record", record)
				.field("graph", graph)
				.field("cascade", cascade)
				.finish(),
			Self::Relate(relate) => f
				.debug_struct("Relate")
				.field("bridge", &relate.bridge)
				.field("atom_endpoints", &relate.atom_endpoints)
				.field("source", &relate.source)
				.field("edge", &relate.edge)
				.field("target", &relate.target)
				.field("graph", &relate.graph)
				.field("label", &relate.label)
				.field("direction", &relate.direction)
				.field("weight", &relate.weight)
				.field("keep_levelpath", &relate.keep_levelpath)
				.finish(),
		}
	}
}

impl LogicalGraphDmlStatement {
	/// Returns the statement kind result.
	pub(super) const fn statement_kind(&self) -> crate::gql::QueryStatementKind {
		match self {
			Self::Attach {
				..
			} => crate::gql::QueryStatementKind::Attach,
			Self::Detach {
				..
			} => crate::gql::QueryStatementKind::Detach,
			Self::Relate(_) => crate::gql::QueryStatementKind::Relate,
		}
	}
}

/// Exact class-qualified record target used by graph DML.
#[derive(Debug, Clone)]
pub(super) struct LogicalRecordTarget {
	/// Class associated with this logical record target.
	pub(super) class: Box<str>,
	/// Key associated with this logical record target.
	pub(super) key: TypedExpr,
}

/// Typed RELATE endpoint selection has no ATTACH/DETACH target responsibility.
#[derive(Debug, Clone)]
pub(super) enum LogicalRelateEndpoint {
	/// An exact class-qualified record with ordinary key evaluation.
	Record(
		/// An exact class-qualified record with ordinary key evaluation.
		LogicalRecordTarget,
	),
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
	/// Class associated with this logical relate edge.
	pub(super) class: Box<str>,
	/// Key associated with this logical relate edge.
	pub(super) key: Option<TypedExpr>,
	/// Schema associated with this logical relate edge.
	pub(super) schema: Schema,
	/// Data associated with this logical relate edge.
	pub(super) data: Option<LogicalDmlData>,
	/// Returning associated with this logical relate edge.
	pub(super) returning: LogicalDmlReturn,
	/// Expect associated with this logical relate edge.
	pub(super) expect: Option<LogicalExpect>,
}

/// One analyzed data-mutation statement.
#[derive(Debug, Clone)]
pub(super) struct LogicalDmlStatement {
	/// Kind associated with this logical dml statement.
	pub(super) kind: LogicalDmlKind,
	/// Graph associated with this logical dml statement.
	pub(super) graph: Option<GraphRef>,
	/// Label mutation: no change, explicit removal, or replacement.
	pub(super) label: Option<FieldUpdate<crate::values::Text>>,
	/// Cascade associated with this logical dml statement.
	pub(super) cascade: bool,
	/// Keep levelpath associated with this logical dml statement.
	pub(super) keep_levelpath: bool,
	/// Targets associated with this logical dml statement.
	pub(super) targets: Vec<LogicalDmlTarget>,
}

/// Mutation operation shared by every target in a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LogicalDmlKind {
	/// Create case of logical dml kind.
	Create,
	/// Update case of logical dml kind.
	Update,
	/// Upsert case of logical dml kind.
	Upsert,
	/// Delete case of logical dml kind.
	Delete,
}

impl LogicalDmlKind {
	/// Returns the statement kind result.
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
	/// Class associated with this logical dml target.
	pub(super) class: Box<str>,
	/// Selector associated with this logical dml target.
	pub(super) selector: LogicalDmlSelector,
	/// Schema associated with this logical dml target.
	pub(super) schema: Schema,
	/// Data associated with this logical dml target.
	pub(super) data: Option<LogicalDmlData>,
	/// Filter associated with this logical dml target.
	pub(super) filter: Option<TypedExpr>,
	/// Returning associated with this logical dml target.
	pub(super) returning: LogicalDmlReturn,
	/// Expect associated with this logical dml target.
	pub(super) expect: Option<LogicalExpect>,
}

/// Records selected or generated for one DML target.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlSelector {
	/// Class case of logical dml selector.
	Class,
	/// Record case of logical dml selector.
	Record(
		/// Record case of logical dml selector.
		TypedExpr,
	),
	/// Range case of logical dml selector.
	Range(
		/// Range case of logical dml selector.
		u32,
	),
}

/// An analyzed record mutation mode.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlData {
	/// Content case of logical dml data.
	Content(
		/// Content case of logical dml data.
		TypedExpr,
	),
	/// Merge case of logical dml data.
	Merge(
		/// Merge case of logical dml data.
		TypedExpr,
	),
	/// Patch case of logical dml data.
	Patch(
		/// Patch case of logical dml data.
		TypedExpr,
	),
	/// Replace case of logical dml data.
	Replace(
		/// Replace case of logical dml data.
		TypedExpr,
	),
	/// Set case of logical dml data.
	Set(
		/// Set case of logical dml data.
		Vec<(Box<str>, TypedExpr)>,
	),
	/// Unset case of logical dml data.
	Unset(
		/// Unset case of logical dml data.
		Vec<Box<str>>,
	),
	/// Reset case of logical dml data.
	Reset(
		/// Reset case of logical dml data.
		Vec<Box<str>>,
	),
}

/// An analyzed DML result projection.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlReturn {
	/// None case of logical dml return.
	None,
	/// Before case of logical dml return.
	Before,
	/// After case of logical dml return.
	After,
	/// Diff case of logical dml return.
	Diff,
	/// Ordered object fields; None expands the selected object's wildcard.
	Fields(
		/// Ordered object fields; None expands the selected object's wildcard.
		Vec<Option<LogicalDmlReturnField>>,
	),
	/// Value case of logical dml return.
	Value(
		/// Value case of logical dml return.
		TypedExpr,
	),
}

/// One named object-return field; an absent field represents a wildcard.
#[derive(Debug, Clone)]
pub(super) struct LogicalDmlReturnField {
	/// Output name chosen by the analyzer before expression analysis.
	pub(super) name: Box<str>,
	/// Original typed expression, retained inline without another allocation.
	pub(super) value: TypedExpr,
}

/// A pre-image requirement checked before the first mutation.
#[derive(Debug, Clone)]
pub(super) enum LogicalExpect {
	/// Exists case of logical expect.
	Exists(
		/// Exists case of logical expect.
		bool,
	),
	/// Predicate case of logical expect.
	Predicate(
		/// Predicate case of logical expect.
		TypedExpr,
	),
}

/// Logical source owner resolved by the ordinary transaction catalog adapter.
#[derive(Debug, Clone)]
pub(super) enum LogicalEventOwner {
	/// Global class namespace, resolved at command execution.
	Class(
		/// Global class namespace, resolved at command execution.
		Box<str>,
	),
	/// Graph namespace, resolved at command execution.
	Graph(
		/// Graph namespace, resolved at command execution.
		Box<str>,
	),
}

/// One ordered source/policy replacement within ALTER EVENT.
#[derive(Debug, Clone)]
pub(super) enum LogicalEventChange {
	/// Checked synchronous or asynchronous policy replacement.
	Execution(
		/// Checked synchronous or asynchronous policy replacement.
		crate::datastore::EventExecution,
	),
	/// Owned read-only predicate source replacement.
	When(
		/// Owned read-only predicate source replacement.
		Text,
	),
	/// Owned expression/block handler source replacement.
	Then(
		/// Owned expression/block handler source replacement.
		Text,
	),
	/// Optional durable comment replacement or removal.
	Comment(
		/// Optional durable comment replacement or removal.
		Option<Text>,
	),
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
	Event(
		/// Event DDL retains source recipes, never executable handler IR.
		LogicalEventStatement,
	),
	/// Defines a reactive identity through the ordinary transaction API.
	DefineLiveBinding(
		/// Defines a reactive identity through the ordinary transaction API.
		Box<LogicalLiveBinding>,
	),
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
	RegisterLive(
		/// Registers a logical LIVE incarnation through the ordinary
		/// transaction.
		Box<LogicalLiveSelect>,
	),
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
	/// Define graph case of logical schema statement.
	DefineGraph {
		/// Mode associated with this logical schema statement.
		mode: DefinitionMode,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Kind associated with this logical schema statement.
		kind: GraphKind,
		/// Constraints associated with this logical schema statement.
		constraints: GraphTopologyConstraints,
		/// Root associated with this logical schema statement.
		root: Option<TypedExpr>,
		/// Content associated with this logical schema statement.
		content: Option<TypedExpr>,
		/// Final validated capture policy for the defined graph source.
		changefeed: Option<crate::changefeed::ChangefeedConfig>,
	},
	/// Update graph case of logical schema statement.
	UpdateGraph {
		/// Graph associated with this logical schema statement.
		graph: GraphRef,
		/// Mutation associated with this logical schema statement.
		mutation: LogicalGraphPropertyMutation,
		/// Returning associated with this logical schema statement.
		returning: LogicalDmlReturn,
	},
	/// Define class case of logical schema statement.
	DefineClass {
		/// Mode associated with this logical schema statement.
		mode: DefinitionMode,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Kind associated with this logical schema statement.
		kind: crate::values::AtomKind,
		/// Schema mode associated with this logical schema statement.
		schema_mode: ClassSchemaMode,
		/// Final validated capture policy for the defined global class source.
		changefeed: Option<crate::changefeed::ChangefeedConfig>,
	},
	/// Define field case of logical schema statement.
	DefineField {
		/// Mode associated with this logical schema statement.
		mode: DefinitionMode,
		/// Class associated with this logical schema statement.
		class: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Value type associated with this logical schema statement.
		value_type: Type,
	},
	/// Define index case of logical schema statement.
	DefineIndex {
		/// Mode associated with this logical schema statement.
		mode: DefinitionMode,
		/// Class associated with this logical schema statement.
		class: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Kind associated with this logical schema statement.
		kind: ClassIndexKind,
		/// Fields associated with this logical schema statement.
		fields: Vec<Box<str>>,
		/// Options associated with this logical schema statement.
		options: ClassIndexOptions,
		/// Scope associated with this logical schema statement.
		scope: IndexScope,
		/// Maintenance associated with this logical schema statement.
		maintenance: crate::datastore::IndexMaintenanceMode,
		/// Concurrently associated with this logical schema statement.
		concurrently: bool,
	},
	/// Graph-owned definition resolved to public catalog types.
	DefineGraphIndex {
		/// Mode associated with this logical schema statement.
		mode: DefinitionMode,
		/// Graph associated with this logical schema statement.
		graph: Box<str>,
		/// Index associated with this logical schema statement.
		index: crate::datastore::GraphIndex,
		/// Concurrently associated with this logical schema statement.
		concurrently: bool,
	},
	/// Define analyzer case of logical schema statement.
	DefineAnalyzer {
		/// Mode associated with this logical schema statement.
		mode: DefinitionMode,
		/// Analyzer associated with this logical schema statement.
		analyzer: crate::datastore::SchemaAnalyzer,
	},
	/// Define parameter case of logical schema statement.
	DefineParameter {
		/// Mode associated with this logical schema statement.
		mode: DefinitionMode,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Carries the analyzed endpoint expression through ordinary execution.
		value: TypedExpr,
		/// Comment associated with this logical schema statement.
		comment: Option<Text>,
	},
	/// Define function case of logical schema statement.
	DefineFunction {
		/// Mode associated with this logical schema statement.
		mode: DefinitionMode,
		/// Namespace associated with this logical schema statement.
		namespace: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Parameters associated with this logical schema statement.
		parameters: Vec<(Box<str>, Type)>,
		/// Result associated with this logical schema statement.
		result: Type,
		/// Effect associated with this logical schema statement.
		effect: FunctionEffect,
		/// Cost associated with this logical schema statement.
		cost: bool,
		/// Max depth associated with this logical schema statement.
		max_depth: Option<u8>,
		/// Body associated with this logical schema statement.
		body: Text,
		/// Comment associated with this logical schema statement.
		comment: Option<Text>,
	},
	/// Remove graph case of logical schema statement.
	RemoveGraph {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Remove class case of logical schema statement.
	RemoveClass {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Remove field case of logical schema statement.
	RemoveField {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Class associated with this logical schema statement.
		class: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Remove index case of logical schema statement.
	RemoveIndex {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Class associated with this logical schema statement.
		class: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Removes a graph-owned user definition.
	RemoveGraphIndex {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Graph associated with this logical schema statement.
		graph: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Remove parameter case of logical schema statement.
	RemoveParameter {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Remove function case of logical schema statement.
	RemoveFunction {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Namespace associated with this logical schema statement.
		namespace: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Signature associated with this logical schema statement.
		signature: Option<Vec<Type>>,
	},
	/// Remove analyzer case of logical schema statement.
	RemoveAnalyzer {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Alter class mode case of logical schema statement.
	AlterClassMode {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Mode associated with this logical schema statement.
		mode: ClassSchemaMode,
	},
	/// Changes only the capture policy of an existing catalog owner.
	AlterChangefeed {
		/// Selects a graph owner instead of a global class owner.
		graph: bool,
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Name associated with this logical schema statement.
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
	/// Alter field type case of logical schema statement.
	AlterFieldType {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Class associated with this logical schema statement.
		class: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Value type associated with this logical schema statement.
		value_type: Type,
	},
	/// Prepare index removal case of logical schema statement.
	PrepareIndexRemoval {
		/// Class associated with this logical schema statement.
		class: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Alter parameter case of logical schema statement.
	AlterParameter {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Value associated with this logical schema statement.
		value: Option<TypedExpr>,
		/// Omission keeps the comment; `Clear` removes it and `Set` replaces
		/// it.
		comment: Option<FieldUpdate<Text>>,
	},
	/// Alter function case of logical schema statement.
	AlterFunction {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Namespace associated with this logical schema statement.
		namespace: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Signature associated with this logical schema statement.
		signature: Option<Vec<Type>>,
		/// Effect associated with this logical schema statement.
		effect: Option<FunctionEffect>,
		/// Cost associated with this logical schema statement.
		cost: Option<bool>,
		/// Omission keeps MAXDEPTH; `Clear` removes it and `Set` replaces it.
		max_depth: Option<FieldUpdate<u8>>,
		/// Body associated with this logical schema statement.
		body: Option<Text>,
		/// Omission keeps the comment; `Clear` removes it and `Set` replaces
		/// it.
		comment: Option<FieldUpdate<Text>>,
	},
	/// Alter analyzer case of logical schema statement.
	AlterAnalyzer {
		/// If exists associated with this logical schema statement.
		if_exists: bool,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Omission keeps the mapper; `Clear` removes it and `Set` selects
		/// another.
		function:
			Option<FieldUpdate<(crate::datastore::FunctionId, crate::datastore::SchemaVersion)>>,
		/// Tokenizers associated with this logical schema statement.
		tokenizers: Option<Vec<crate::datastore::AnalyzerTokenizer>>,
		/// Filters associated with this logical schema statement.
		filters: Option<Vec<crate::datastore::AnalyzerFilter>>,
		/// Omission keeps the comment; `Clear` removes it and `Set` replaces
		/// it.
		comment: Option<FieldUpdate<Text>>,
	},
	/// Info database case of logical schema statement.
	InfoDatabase,
	/// Info analyzer case of logical schema statement.
	InfoAnalyzer {
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Info class case of logical schema statement.
	InfoClass {
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Read-only graph metadata and its source-owned event definitions.
	InfoGraph {
		/// Case-sensitive graph name in the statement snapshot.
		name: Box<str>,
	},
	/// Info index case of logical schema statement.
	InfoIndex {
		/// Class associated with this logical schema statement.
		class: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Observes a graph-owned definition.
	InfoGraphIndex {
		/// Graph associated with this logical schema statement.
		graph: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Info parameter case of logical schema statement.
	InfoParameter {
		/// Name associated with this logical schema statement.
		name: Box<str>,
	},
	/// Info function case of logical schema statement.
	InfoFunction {
		/// Namespace associated with this logical schema statement.
		namespace: Box<str>,
		/// Name associated with this logical schema statement.
		name: Box<str>,
		/// Signature associated with this logical schema statement.
		signature: Option<Vec<Type>>,
	},
}

/// One catalog-owned graph-property mutation.
#[derive(Debug, Clone)]
pub(super) enum LogicalGraphPropertyMutation {
	/// Set case of logical graph property mutation.
	Set(
		/// Set case of logical graph property mutation.
		Vec<(Vec<Box<str>>, TypedExpr)>,
	),
	/// Unset case of logical graph property mutation.
	Unset(
		/// Unset case of logical graph property mutation.
		Vec<Vec<Box<str>>>,
	),
}

/// History binding independent of current schema-name resolution.
#[derive(Debug, Clone)]
pub(super) enum LogicalChangefeedTarget {
	/// Active-name-first lookup through the retained feed catalog.
	Named(
		/// Active-name-first lookup through the retained feed catalog.
		crate::changefeed::ChangefeedSelector,
	),
	/// A graph reference or graph-name value evaluated by the ordinary
	/// evaluator.
	Graph(
		/// A graph reference or graph-name value evaluated by the ordinary
		/// evaluator.
		TypedExpr,
	),
	/// A runtime UUID value evaluated by the ordinary evaluator.
	Id(
		/// A runtime UUID value evaluated by the ordinary evaluator.
		TypedExpr,
	),
}

impl LogicalSchemaStatement {
	/// Distinguishes history/catalog reads from atomic schema mutations.
	pub(super) const fn is_read(&self) -> bool {
		matches!(
			self.statement_kind(),
			crate::gql::QueryStatementKind::Info | crate::gql::QueryStatementKind::ShowChanges
		)
	}

	/// Returns the statement kind result.
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
	/// Family associated with this logical graph contract.
	pub(super) family: Option<GraphKind>,
	/// Validators associated with this logical graph contract.
	pub(super) validators: GraphTopologyConstraints,
}

/// Typed selectors used by an owned graph result.
#[derive(Debug, Clone)]
pub(super) struct LogicalReturnGraph {
	/// Optional adapter resolving external relation recipes into local atoms.
	pub(super) bridge: Option<Box<str>>,
	/// Contract associated with this logical return graph.
	pub(super) contract: LogicalGraphContract,
	/// Root associated with this logical return graph.
	pub(super) root: Option<TypedExpr>,
	/// Nodes associated with this logical return graph.
	pub(super) nodes: TypedExpr,
	/// Edges associated with this logical return graph.
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
	/// Match case of logical build graph source.
	Match(
		/// Match case of logical build graph source.
		Box<LogicalMatch>,
	),
	/// Select case of logical build graph source.
	Select(
		/// Select case of logical build graph source.
		Box<LogicalSelect>,
	),
	/// Traverse case of logical build graph source.
	Traverse(
		/// Traverse case of logical build graph source.
		Box<LogicalTraverse>,
	),
	/// Expression case of logical build graph source.
	Expression(
		/// Expression case of logical build graph source.
		TypedExpr,
	),
}

/// One anonymous materialization or named atomic graph installation.
#[derive(Debug, Clone)]
pub(super) struct LogicalBuildGraph {
	/// Optional adapter resolving external relation recipes before assembly.
	pub(super) bridge: Option<Box<str>>,
	/// Mode associated with this logical build graph.
	pub(super) mode: DefinitionMode,
	/// Contract associated with this logical build graph.
	pub(super) contract: LogicalGraphContract,
	/// Contract explicit associated with this logical build graph.
	pub(super) contract_explicit: bool,
	/// Name associated with this logical build graph.
	pub(super) name: Option<Box<str>>,
	/// Source associated with this logical build graph.
	pub(super) source: LogicalBuildGraphSource,
	/// Root associated with this logical build graph.
	pub(super) root: Option<TypedExpr>,
	/// Nodes associated with this logical build graph.
	pub(super) nodes: Option<TypedExpr>,
	/// Edges associated with this logical build graph.
	pub(super) edges: Option<TypedExpr>,
	/// Limit nodes associated with this logical build graph.
	pub(super) limit_nodes: Option<u64>,
	/// Limit edges associated with this logical build graph.
	pub(super) limit_edges: Option<u64>,
}

#[derive(Clone)]
/// An analyzed MATCH statement with a snapshot-bound catalog graph.
pub(super) struct LogicalMatch {
	/// Fixed layout delta copied with this subtree, including nested owners.
	pub(super) boxed_layout_bytes: usize,
	/// Static identity or lexical GraphRef operand resolved before planning.
	pub(super) graph: LogicalGraphReference,
	/// Invocation deadline retained for MATCH embedded in another statement.
	pub(super) timeout: Option<LogicalDuration>,
	/// Whether an unmatched input row survives with new aliases set to none.
	pub(super) optional: bool,
	/// Whether complete binding tuples are deduplicated before projection.
	pub(super) distinctness: crate::syn::MatchDistinctness,
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
	pub(super) return_graph: Option<Box<LogicalReturnGraph>>,
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

impl std::fmt::Debug for LogicalMatch {
	/// Formats the semantic plan and its retained allocation count for
	/// diagnostics.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("LogicalMatch")
			.field("boxed_layout_bytes", &self.boxed_layout_bytes)
			.field("graph", &self.graph)
			.field("timeout", &self.timeout)
			.field("optional", &self.optional)
			.field("distinctness", &self.distinctness)
			.field("mode", &self.mode)
			.field("atom", &self.atom)
			.field("meta_scope", &self.meta_scope)
			.field("patterns", &self.patterns)
			.field("bindings", &self.bindings)
			.field("filter", &self.filter)
			.field("projection", &self.projection)
			.field("return_graph", &self.return_graph)
			.field("order", &self.order)
			.field("max_depth", &self.max_depth)
			.field("max_depth_binding", &self.max_depth_binding)
			.field("result", &self.result)
			.finish()
	}
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
	Linear(
		/// StandardPath or AtomWalk chain.
		LogicalLinearPattern,
	),
	/// Hyperedge endpoint assignment.
	Hyper(
		/// Hyperedge endpoint assignment.
		LogicalHyperPattern,
	),
	/// Direct or transitive graph containment join.
	Within(
		/// Direct or transitive graph containment join.
		LogicalWithinPattern,
	),
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
	/// Edge associated with this logical hyper pattern.
	pub(super) edge: MatchBinding,
	/// Contains associated with this logical hyper pattern.
	pub(super) contains: Vec<LogicalMatchContains>,
}

#[derive(Debug, Clone)]
/// One typed HEAD, TAIL, or unqualified endpoint group.
pub(super) struct LogicalMatchContains {
	/// Side associated with this logical match contains.
	pub(super) side: crate::syn::MatchHyperSide,
	/// Endpoints associated with this logical match contains.
	pub(super) endpoints: Vec<MatchBinding>,
}

#[derive(Debug, Clone)]
/// One direct or bounded transitive containment relation.
pub(super) struct LogicalWithinPattern {
	/// Child associated with this logical within pattern.
	pub(super) child: MatchBinding,
	/// Owner associated with this logical within pattern.
	pub(super) owner: Box<MatchBinding>,
	/// Min associated with this logical within pattern.
	pub(super) min: u32,
	/// Max associated with this logical within pattern.
	pub(super) max: u32,
}

#[derive(Debug, Clone)]
/// A linear MATCH expansion operation.
pub(super) enum MatchOp {
	/// Expands through exactly one connector edge.
	Expand(
		/// Expands through exactly one connector edge.
		Expand,
	),
	/// Expands through a finite TRAIL path.
	PathExpand(
		/// Expands through a finite TRAIL path.
		PathExpand,
	),
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
	/// Returns the atom kind result.
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
	Class(
		/// Any atom of one global class.
		Box<str>,
	),
	/// One exact global record identity.
	Record(
		/// One exact global record identity.
		LogicalRecordTarget,
	),
}

#[derive(Debug, Clone)]
/// Logical select state.
pub(super) struct LogicalSelect {
	/// Cold graph selectors stay outside ordinary SELECT storage.
	pub(super) return_graph: Option<Box<LogicalReturnGraph>>,
	/// Independent SDK source plans share ordinary SELECT evaluation.
	pub(super) union_inputs: Vec<Self>,
	/// A private typing family becomes one JOIN input before relational work.
	pub(super) union_purpose: crate::syn::SelectUnionPurpose,
	/// Logical lexical origin proves identical independent subqueries.
	pub(super) lexical_id: Option<u32>,
	/// Source associated with this logical select.
	pub(super) source: LogicalSelectSource,
	/// Joins associated with this logical select.
	pub(super) joins: Vec<LogicalSelectJoin>,
	/// Access associated with this logical select.
	pub(super) access: LogicalSelectAccess,
	/// Input schema associated with this logical select.
	pub(super) input_schema: Schema,
	/// Filter associated with this logical select.
	pub(super) filter: Option<TypedExpr>,
	/// Split associated with this logical select.
	pub(super) split: Vec<TypedExpr>,
	/// Group associated with this logical select.
	pub(super) group: Option<Vec<TypedExpr>>,
	/// Projection associated with this logical select.
	pub(super) projection: Vec<LogicalSelectItem>,
	/// Optional SDK output paths, parallel to projection; empty for ordinary
	/// aliases.
	pub(super) projection_paths: Vec<Option<Vec<Box<str>>>>,
	/// Result associated with this logical select.
	pub(super) result: Schema,
	/// Omit associated with this logical select.
	pub(super) omit: Vec<Vec<Box<str>>>,
	/// Fetch associated with this logical select.
	pub(super) fetch: Vec<Vec<Box<str>>>,
	/// Optional adapter aliases parallel to the ordinary FETCH paths.
	pub(super) fetch_bridges: Option<Box<[Option<Box<str>>]>>,
	/// Order associated with this logical select.
	pub(super) order: Vec<LogicalOrder>,
	/// Start associated with this logical select.
	pub(super) start: Option<LogicalSelectStart>,
	/// Limit associated with this logical select.
	pub(super) limit: Option<TypedExpr>,
	/// Value associated with this logical select.
	pub(super) value: bool,
	/// Only associated with this logical select.
	pub(super) only: bool,
	/// Tempfiles associated with this logical select.
	pub(super) tempfiles: bool,
	/// Outer columns associated with this logical select.
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
	Expression(
		/// A collection, atom, occurrence, graph, or range expression.
		TypedExpr,
	),
	/// Existing MATCH execution used as a correlated traversal source.
	Match(
		/// Existing MATCH execution used as a correlated traversal source.
		Box<LogicalMatch>,
	),
	/// Existing SELECT execution used as a traversal source.
	Select(
		/// Existing SELECT execution used as a traversal source.
		Box<LogicalSelect>,
	),
}

/// One statically known `tv::` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraverseStepKind {
	/// Vertices case of traverse step kind.
	Vertices,
	/// Edges case of traverse step kind.
	Edges,
	/// Out case of traverse step kind.
	Out,
	/// In case of traverse step kind.
	In,
	/// Both case of traverse step kind.
	Both,
	/// Out edges case of traverse step kind.
	OutEdges,
	/// In edges case of traverse step kind.
	InEdges,
	/// Both edges case of traverse step kind.
	BothEdges,
	/// In vertex case of traverse step kind.
	InVertex,
	/// Out vertex case of traverse step kind.
	OutVertex,
	/// Hyper case of traverse step kind.
	Hyper,
	/// Meta inner case of traverse step kind.
	MetaInner,
	/// Has attribute case of traverse step kind.
	HasAttribute,
	/// Has case of traverse step kind.
	Has,
	/// Has label case of traverse step kind.
	HasLabel,
	/// Where case of traverse step kind.
	Where,
	/// Deduplicate case of traverse step kind.
	Deduplicate,
	/// Deduplicate by case of traverse step kind.
	DeduplicateBy,
	/// Simple path case of traverse step kind.
	SimplePath,
	/// Cyclic path case of traverse step kind.
	CyclicPath,
	/// Map case of traverse step kind.
	Map,
	/// Flat map case of traverse step kind.
	FlatMap,
	/// Algorithm case of traverse step kind.
	Algorithm,
	/// Values case of traverse step kind.
	Values,
	/// Select case of traverse step kind.
	Select,
	/// Match case of traverse step kind.
	Match,
	/// As case of traverse step kind.
	As,
	/// Project case of traverse step kind.
	Project,
	/// Unfold case of traverse step kind.
	Unfold,
	/// Initialize sack case of traverse step kind.
	InitializeSack,
	/// Sack case of traverse step kind.
	Sack,
	/// Get sack case of traverse step kind.
	GetSack,
	/// Initialize named sack case of traverse step kind.
	InitializeNamedSack,
	/// Named sack case of traverse step kind.
	NamedSack,
	/// Get named sack case of traverse step kind.
	GetNamedSack,
	/// Side effect case of traverse step kind.
	SideEffect,
	/// Path case of traverse step kind.
	Path,
	/// Emit case of traverse step kind.
	Emit,
	/// Limit case of traverse step kind.
	Limit,
	/// Skip case of traverse step kind.
	Skip,
	/// Repeat case of traverse step kind.
	Repeat,
	/// Branch case of traverse step kind.
	Branch,
	/// Choose case of traverse step kind.
	Choose,
	/// Coalesce case of traverse step kind.
	Coalesce,
	/// Order case of traverse step kind.
	Order,
	/// Group case of traverse step kind.
	Group,
	/// Fold case of traverse step kind.
	Fold,
	/// Count case of traverse step kind.
	Count,
	/// Sum case of traverse step kind.
	Sum,
	/// Minimum case of traverse step kind.
	Minimum,
	/// Maximum case of traverse step kind.
	Maximum,
	/// Average case of traverse step kind.
	Average,
	/// K hop case of traverse step kind.
	KHop,
	/// Reachable case of traverse step kind.
	Reachable,
	/// Shortest path case of traverse step kind.
	ShortestPath,
	/// Weighted shortest path case of traverse step kind.
	WeightedShortestPath,
	/// Distance to case of traverse step kind.
	DistanceTo,
	/// Bellman ford case of traverse step kind.
	BellmanFord,
	/// Topological sort case of traverse step kind.
	TopologicalSort,
	/// Strongly connected components case of traverse step kind.
	StronglyConnectedComponents,
	/// Weakly connected components case of traverse step kind.
	WeaklyConnectedComponents,
	/// Condensation case of traverse step kind.
	Condensation,
	/// Minimum spanning forest case of traverse step kind.
	MinimumSpanningForest,
	/// Cuts case of traverse step kind.
	Cuts,
	/// Lowest common ancestor case of traverse step kind.
	LowestCommonAncestor,
	/// Has cycle case of traverse step kind.
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

	/// Returns whether this value is barrier.
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
	/// Kind associated with this logical traverse step.
	pub(super) kind: TraverseStepKind,
	/// Arguments associated with this logical traverse step.
	pub(super) arguments: Vec<TypedExpr>,
	/// Recursive `tv::algo` target resolved through the named-function cache.
	pub(super) named_function: Option<NamedFunctionKey>,
	/// Target type of the implicit first `tv::algo` argument.
	pub(super) algorithm_input: Option<Type>,
	/// Statically resolved type produced by `tv::algo`.
	pub(super) output_type: Option<Type>,
	/// Match plan associated with this logical traverse step.
	pub(super) match_plan: Option<Box<LogicalMatch>>,
	/// Repeat associated with this logical traverse step.
	pub(super) repeat: Option<LogicalRepeat>,
	/// Source byte range used to locate diagnostics.
	pub(super) span: TextSpan,
}

/// Normalized modifiers of `tv::repeat(body)`.
#[derive(Debug, Clone)]
pub(super) struct LogicalRepeat {
	/// Body associated with this logical repeat.
	pub(super) body: TypedExpr,
	/// Times associated with this logical repeat.
	pub(super) times: Option<TypedExpr>,
	/// Until associated with this logical repeat.
	pub(super) until: Option<TypedExpr>,
	/// Emit associated with this logical repeat.
	pub(super) emit: bool,
}

/// One complete streaming traversal statement.
#[derive(Clone)]
pub(super) struct LogicalTraverse {
	/// Fixed layout delta copied with this subtree, including nested owners.
	pub(super) boxed_layout_bytes: usize,
	/// Atom associated with this logical traverse.
	pub(super) atom: bool,
	/// Constraints associated with this logical traverse.
	pub(super) constraints: crate::syn::MatchConstraints,
	/// Source associated with this logical traverse.
	pub(super) source: LogicalTraverseSource,
	/// Graph associated with this logical traverse.
	pub(super) graph: Option<LogicalGraphReference>,
	/// Shared static contribution used by navigation and weighted path steps.
	pub(super) custom_cost: Option<LogicalPathCost>,
	/// Steps associated with this logical traverse.
	pub(super) steps: Vec<LogicalTraverseStep>,
	/// Columns available to the final RETURN expression in runtime row order.
	pub(super) result: Schema,
	/// Returning associated with this logical traverse.
	pub(super) returning: Option<TypedExpr>,
	/// Return graph associated with this logical traverse.
	pub(super) return_graph: Option<LogicalReturnGraph>,
}

impl std::fmt::Debug for LogicalTraverse {
	/// Formats the semantic plan and its retained allocation count for
	/// diagnostics.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("LogicalTraverse")
			.field("boxed_layout_bytes", &self.boxed_layout_bytes)
			.field("atom", &self.atom)
			.field("constraints", &self.constraints)
			.field("source", &self.source)
			.field("graph", &self.graph)
			.field("custom_cost", &self.custom_cost)
			.field("steps", &self.steps)
			.field("result", &self.result)
			.field("returning", &self.returning)
			.field("return_graph", &self.return_graph)
			.finish()
	}
}

#[derive(Debug, Clone, Default)]
/// Logical select access alternatives.
pub(super) enum LogicalSelectAccess {
	#[default]
	/// Automatic case of logical select access.
	Automatic,
	/// No index case of logical select access.
	NoIndex,
	/// Indexes case of logical select access.
	Indexes(
		/// Indexes case of logical select access.
		Vec<LogicalIndexHint>,
	),
}

#[derive(Debug, Clone)]
/// Logical index hint state.
pub(super) struct LogicalIndexHint {
	/// Source associated with this logical index hint.
	pub(super) source: Option<Box<str>>,
	/// Index associated with this logical index hint.
	pub(super) index: Box<str>,
}

#[derive(Debug, Clone)]
/// Logical select source state.
pub(super) struct LogicalSelectSource {
	/// Kind associated with this logical select source.
	pub(super) kind: LogicalSelectSourceKind,
	/// Schema associated with this logical select source.
	pub(super) schema: Schema,
	/// Wildcard associated with this logical select source.
	pub(super) wildcard: Vec<LogicalWildcardField>,
	/// Alias associated with this logical select source.
	pub(super) alias: Box<str>,
}

#[derive(Debug, Clone)]
/// Logical select source kind alternatives.
pub(super) enum LogicalSelectSourceKind {
	/// A zero-row SDK relation still validates its optional graph scope.
	Empty {
		/// Graph source identity is checked even when there are no classes.
		graph: Option<LogicalGraphReference>,
	},
	/// Atoms case of logical select source kind.
	Atoms {
		/// Target associated with this logical select source kind.
		target: ScanTarget,
		/// Graph associated with this logical select source kind.
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
	/// Graphs case of logical select source kind.
	Graphs {
		/// References associated with this logical select source kind.
		references: Option<Vec<LogicalGraphReference>>,
	},
	/// Derived case of logical select source kind.
	Derived(
		/// Derived case of logical select source kind.
		Box<LogicalSelect>,
	),
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
/// Logical graph reference alternatives.
pub(super) enum LogicalGraphReference {
	/// Static case of logical graph reference.
	Static(
		/// Static case of logical graph reference.
		GraphRef,
	),
	/// Expression case of logical graph reference.
	Expression(
		/// Expression case of logical graph reference.
		TypedExpr,
	),
}

#[derive(Debug, Clone)]
/// Logical select join state.
pub(super) struct LogicalSelectJoin {
	/// Kind associated with this logical select join.
	pub(super) kind: crate::syn::SelectJoinKind,
	/// Source associated with this logical select join.
	pub(super) source: LogicalSelectSource,
	/// Predicate associated with this logical select join.
	pub(super) predicate: Option<TypedExpr>,
}

#[derive(Debug, Clone)]
/// Logical select item alternatives.
pub(super) enum LogicalSelectItem {
	/// Wildcard case of logical select item.
	Wildcard(
		/// Wildcard case of logical select item.
		Vec<LogicalWildcardField>,
	),
	/// Uses the ordinary wildcard projector for one class typing state.
	WildcardVariants {
		/// Lazy class tuple CASE returning a checked field-list ordinal.
		selector: TypedExpr,
		/// Original ordinary wildcard lists, in analyzer variant order.
		choices: Vec<Vec<LogicalWildcardField>>,
	},
	/// Expression case of logical select item.
	Expression {
		/// Name associated with this logical select item.
		name: Box<str>,
		/// Expression associated with this logical select item.
		expression: TypedExpr,
	},
}

#[derive(Debug, Clone)]
/// Logical wildcard field alternatives.
pub(super) enum LogicalWildcardField {
	/// Column case of logical wildcard field.
	Column {
		/// Name associated with this logical wildcard field.
		name: Box<str>,
		/// Index associated with this logical wildcard field.
		index: usize,
	},
	/// Properties case of logical wildcard field.
	Properties {
		/// Index associated with this logical wildcard field.
		index: usize,
	},
}

#[derive(Debug, Clone)]
/// Logical order state.
pub(super) struct LogicalOrder {
	/// Expression associated with this logical order.
	pub(super) expression: TypedExpr,
	/// Random associated with this logical order.
	pub(super) random: bool,
	/// Collate associated with this logical order.
	pub(super) collate: bool,
	/// Numeric associated with this logical order.
	pub(super) numeric: bool,
	/// Direction associated with this logical order.
	pub(super) direction: crate::syn::SelectOrderDirection,
	/// Nulls associated with this logical order.
	pub(super) nulls: crate::syn::SelectNulls,
}

#[derive(Debug, Clone)]
/// Logical select start alternatives.
pub(super) enum LogicalSelectStart {
	/// Offset case of logical select start.
	Offset(
		/// Offset case of logical select start.
		TypedExpr,
	),
	/// After case of logical select start.
	After {
		/// Cursor associated with this logical select start.
		cursor: TypedExpr,
		/// Read committed associated with this logical select start.
		read_committed: bool,
	},
}

#[derive(Debug, Clone)]
/// Scan target alternatives.
pub(super) enum ScanTarget {
	/// Class case of scan target.
	Class(
		/// Class case of scan target.
		Box<str>,
	),
	/// Record case of scan target.
	Record(
		/// Record case of scan target.
		RecordId,
	),
}

impl ScanTarget {
	/// Returns the class result.
	pub(super) fn class(&self) -> &str {
		match self {
			Self::Class(class) => class,
			Self::Record(record) => record.class(),
		}
	}
}

#[derive(Debug, Clone, Default)]
/// Schema state.
pub(super) struct Schema {
	/// Columns associated with this schema.
	pub(super) columns: Vec<Column>,
	/// Source origins retained with this state.
	pub(super) origins: BTreeMap<usize, ColumnOrigin>,
}

impl Schema {
	/// Returns the index of result.
	pub(super) fn index_of(&self, name: &str) -> Option<usize> {
		self.columns.iter().position(|column| {
			!column.name.starts_with('\0') && column.name.eq_ignore_ascii_case(name)
		})
	}

	/// Returns the matching indices result.
	pub(super) fn matching_indices<'a>(
		&'a self,
		name: &'a str,
	) -> impl Iterator<Item = usize> + 'a {
		self.columns.iter().enumerate().filter_map(move |(index, column)| {
			(!column.name.starts_with('\0') && column.name.eq_ignore_ascii_case(name))
				.then_some(index)
		})
	}

	/// Returns the qualified index result.
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
	/// Source associated with this column origin.
	pub(super) source: Box<str>,
	/// Class associated with this column origin.
	pub(super) class: Box<str>,
	/// Field associated with this column origin.
	pub(super) field: Box<str>,
	/// `None` denotes a dynamic graph scope whose identity is resolved at
	/// statement execution.
	pub(super) scope: Option<IndexScope>,
}

#[derive(Debug, Clone)]
/// Column state.
pub(super) struct Column {
	/// Name associated with this column.
	pub(super) name: Box<str>,
	/// Qualifier associated with this column.
	pub(super) qualifier: Option<Box<str>>,
	/// Ty associated with this column.
	pub(super) ty: Type,
}

#[derive(Debug, Clone)]
/// Typed expr state.
pub(super) struct TypedExpr {
	/// Kind associated with this typed expr.
	pub(super) kind: TypedExprKind,
	/// Ty associated with this typed expr.
	pub(super) ty: Type,
	/// Source byte range used to locate diagnostics.
	pub(super) span: TextSpan,
}

/// Statically classified side effects of one language closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ClosureEffect {
	/// Pure case of closure effect.
	Pure,
	/// Stable read case of closure effect.
	StableRead,
	/// Data mutation case of closure effect.
	DataMutation,
	/// Schema mutation case of closure effect.
	SchemaMutation,
}

/// Typed closure body executed by the session-owned closure registry.
#[derive(Debug, Clone)]
pub(super) enum LogicalClosureBody {
	/// Expression case of logical closure body.
	Expression(
		/// Expression case of logical closure body.
		Box<TypedExpr>,
	),
	/// Block case of logical closure body.
	Block(
		/// Block case of logical closure body.
		LogicalBlock,
	),
}

/// Session-scoped closure with explicit capture and invocation slots.
#[derive(Clone)]
pub(super) struct LogicalClosure {
	/// Fixed layout delta copied with this subtree, including nested owners.
	pub(super) boxed_layout_bytes: usize,
	/// Parameters associated with this logical closure.
	pub(super) parameters: Vec<(u32, Type)>,
	/// Captures associated with this logical closure.
	pub(super) captures: Vec<u32>,
	/// Result associated with this logical closure.
	pub(super) result: Type,
	/// Body associated with this logical closure.
	pub(super) body: LogicalClosureBody,
	/// Effect associated with this logical closure.
	pub(super) effect: ClosureEffect,
	/// Slot count associated with this logical closure.
	pub(super) slot_count: usize,
	/// Conservative retained size of the typed closure plan.
	pub(super) retained_plan_bytes: usize,
	/// Named function associated with this logical closure.
	pub(super) named_function: Option<NamedFunctionKey>,
}

impl std::fmt::Debug for LogicalClosure {
	/// Formats the semantic plan and its retained allocation count for
	/// diagnostics.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("LogicalClosure")
			.field("boxed_layout_bytes", &self.boxed_layout_bytes)
			.field("parameters", &self.parameters)
			.field("captures", &self.captures)
			.field("result", &self.result)
			.field("body", &self.body)
			.field("effect", &self.effect)
			.field("slot_count", &self.slot_count)
			.field("retained_plan_bytes", &self.retained_plan_bytes)
			.field("named_function", &self.named_function)
			.finish()
	}
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
	/// Identifier used to locate this value within its owning subsystem.
	pub(super) id: FunctionId,
	/// Version associated with this named function key.
	pub(super) version: SchemaVersion,
	/// Schema epoch associated with this named function key.
	pub(super) schema_epoch: u64,
	/// Max depth associated with this named function key.
	pub(super) max_depth: Option<u8>,
	/// Effect associated with this named function key.
	pub(super) effect: FunctionEffect,
}

#[derive(Debug, Clone)]
/// Typed expr kind alternatives.
pub(super) enum TypedExprKind {
	/// Constant case of typed expr kind.
	Constant(
		/// Constant case of typed expr kind.
		Value,
	),
	/// Slot case of typed expr kind.
	Slot(
		/// Slot case of typed expr kind.
		u32,
	),
	/// Column case of typed expr kind.
	Column(
		/// Column case of typed expr kind.
		usize,
	),
	/// Array case of typed expr kind.
	Array(
		/// Array case of typed expr kind.
		Vec<TypedExpr>,
	),
	/// Set case of typed expr kind.
	Set(
		/// Set case of typed expr kind.
		Vec<TypedExpr>,
	),
	/// Object case of typed expr kind.
	Object(
		/// Object case of typed expr kind.
		Vec<(Box<str>, TypedExpr)>,
	),
	/// Range case of typed expr kind.
	Range {
		/// Start associated with this typed expr kind.
		start: Option<Box<TypedExpr>>,
		/// End associated with this typed expr kind.
		end: Option<Box<TypedExpr>>,
		/// Inclusive end associated with this typed expr kind.
		inclusive_end: bool,
	},
	/// Unary case of typed expr kind.
	Unary {
		/// Op associated with this typed expr kind.
		op: crate::syn::UnaryOperator,
		/// Expr associated with this typed expr kind.
		expr: Box<TypedExpr>,
	},
	/// Binary case of typed expr kind.
	Binary {
		/// Left associated with this typed expr kind.
		left: Box<TypedExpr>,
		/// Op associated with this typed expr kind.
		op: crate::syn::BinaryOperator,
		/// Right associated with this typed expr kind.
		right: Box<TypedExpr>,
	},
	/// Fulltext case of typed expr kind.
	Fulltext {
		/// Left associated with this typed expr kind.
		left: Box<TypedExpr>,
		/// Op associated with this typed expr kind.
		op: crate::syn::FulltextOperator,
		/// Right associated with this typed expr kind.
		right: Box<TypedExpr>,
		/// Definitions associated with this typed expr kind.
		definitions: Vec<LogicalFulltextDefinition>,
	},
	/// Knn case of typed expr kind.
	Knn {
		/// Left associated with this typed expr kind.
		left: Box<TypedExpr>,
		/// Origin associated with this typed expr kind.
		origin: ColumnOrigin,
		/// K associated with this typed expr kind.
		k: Box<TypedExpr>,
		/// Ef associated with this typed expr kind.
		ef: Option<Box<TypedExpr>>,
		/// Distance associated with this typed expr kind.
		distance: LogicalKnnDistance,
		/// Right associated with this typed expr kind.
		right: Box<TypedExpr>,
		/// Definitions associated with this typed expr kind.
		definitions: Vec<LogicalVectorDefinition>,
	},
	/// Conditional case of typed expr kind.
	Conditional {
		/// Condition associated with this typed expr kind.
		condition: Box<TypedExpr>,
		/// Then value associated with this typed expr kind.
		then_value: Box<TypedExpr>,
		/// Else value associated with this typed expr kind.
		else_value: Box<TypedExpr>,
	},
	/// Case case of typed expr kind.
	Case {
		/// Operand associated with this typed expr kind.
		operand: Option<Box<TypedExpr>>,
		/// Branches associated with this typed expr kind.
		branches: Vec<(TypedExpr, TypedExpr)>,
		/// Else value associated with this typed expr kind.
		else_value: Option<Box<TypedExpr>>,
	},
	/// Field case of typed expr kind.
	Field {
		/// Base associated with this typed expr kind.
		base: Box<TypedExpr>,
		/// Field associated with this typed expr kind.
		field: Box<str>,
		/// Null safe associated with this typed expr kind.
		null_safe: bool,
	},
	/// Current field case of typed expr kind.
	CurrentField(
		/// Current field case of typed expr kind.
		Box<str>,
	),
	/// Destructure case of typed expr kind.
	Destructure {
		/// Base associated with this typed expr kind.
		base: Box<TypedExpr>,
		/// Fields associated with this typed expr kind.
		fields: Vec<Box<str>>,
	},
	/// Index case of typed expr kind.
	Index {
		/// Base associated with this typed expr kind.
		base: Box<TypedExpr>,
		/// Index associated with this typed expr kind.
		index: Box<TypedExpr>,
	},
	/// Filter case of typed expr kind.
	Filter {
		/// Base associated with this typed expr kind.
		base: Box<TypedExpr>,
		/// Predicate associated with this typed expr kind.
		predicate: Box<TypedExpr>,
	},
	/// Builtin case of typed expr kind.
	Builtin {
		/// Function associated with this typed expr kind.
		function: Builtin,
		/// Arguments associated with this typed expr kind.
		arguments: Vec<TypedExpr>,
	},
	/// Closure case of typed expr kind.
	Closure(
		/// Closure case of typed expr kind.
		Box<LogicalClosure>,
	),
	/// Invoke case of typed expr kind.
	Invoke {
		/// Callee associated with this typed expr kind.
		callee: Box<TypedExpr>,
		/// Arguments associated with this typed expr kind.
		arguments: Vec<TypedExpr>,
	},
	/// Named invoke case of typed expr kind.
	NamedInvoke {
		/// Function associated with this typed expr kind.
		function: NamedFunctionKey,
		/// Arguments associated with this typed expr kind.
		arguments: Vec<TypedExpr>,
	},
	/// A generic expression pipeline lowered to the traversal VM.
	Traversal(
		/// A generic expression pipeline lowered to the traversal VM.
		Box<LogicalTraverse>,
	),
	/// Aggregate case of typed expr kind.
	Aggregate {
		/// Function associated with this typed expr kind.
		function: Aggregate,
		/// Argument associated with this typed expr kind.
		argument: Option<Box<TypedExpr>>,
		/// Distinct associated with this typed expr kind.
		distinct: bool,
	},
	/// Subquery case of typed expr kind.
	Subquery {
		/// Select associated with this typed expr kind.
		select: Box<LogicalSelect>,
		/// Collection associated with this typed expr kind.
		collection: bool,
	},
	/// Cast case of typed expr kind.
	Cast {
		/// Target associated with this typed expr kind.
		target: Type,
		/// Expr associated with this typed expr kind.
		expr: Box<TypedExpr>,
	},
	/// Reads the value of a lexical reactive identity from this exact snapshot.
	LiveSlot(
		/// Reads the value of a lexical reactive identity from this exact
		/// snapshot.
		u32,
	),
}

/// Snapshot-exact Fulltext definition selected during semantic analysis.
#[derive(Debug, Clone)]
pub(super) struct LogicalFulltextDefinition {
	/// Class associated with this logical fulltext definition.
	pub(super) class: Box<str>,
	/// Field associated with this logical fulltext definition.
	pub(super) field: Box<str>,
	/// Source associated with this logical fulltext definition.
	pub(super) source: Box<str>,
	/// Scope associated with this logical fulltext definition.
	pub(super) scope: Option<IndexScope>,
	/// Index associated with this logical fulltext definition.
	pub(super) index: IndexId,
	/// Name associated with this logical fulltext definition.
	pub(super) name: Text,
	/// State associated with this logical fulltext definition.
	pub(super) state: ClassIndexState,
	/// Analyzer associated with this logical fulltext definition.
	pub(super) analyzer: Arc<SchemaAnalyzer>,
	/// Analyzer function associated with this logical fulltext definition.
	pub(super) analyzer_function: Option<NamedFunctionKey>,
	/// K1 associated with this logical fulltext definition.
	pub(super) k1: f64,
	/// B associated with this logical fulltext definition.
	pub(super) b: f64,
	/// Highlights associated with this logical fulltext definition.
	pub(super) highlights: bool,
}

/// Exact KNN distance or a typed callable evaluated by the query runtime.
#[derive(Debug, Clone)]
pub(super) enum LogicalKnnDistance {
	/// Builtin case of logical knn distance.
	Builtin(
		/// Builtin case of logical knn distance.
		VectorDistance,
	),
	/// Minkowski case of logical knn distance.
	Minkowski(
		/// Minkowski case of logical knn distance.
		Box<TypedExpr>,
	),
	/// Named case of logical knn distance.
	Named(
		/// Named case of logical knn distance.
		NamedFunctionKey,
	),
	/// Callable case of logical knn distance.
	Callable(
		/// Callable case of logical knn distance.
		Box<TypedExpr>,
	),
}

/// One compatible durable ANN definition visible to planning.
#[derive(Debug, Clone)]
pub(super) struct LogicalVectorDefinition {
	/// Class associated with this logical vector definition.
	pub(super) class: Box<str>,
	/// Field associated with this logical vector definition.
	pub(super) field: Box<str>,
	/// Source associated with this logical vector definition.
	pub(super) source: Box<str>,
	/// Scope associated with this logical vector definition.
	pub(super) scope: Option<IndexScope>,
	/// Index associated with this logical vector definition.
	pub(super) index: IndexId,
	/// Name associated with this logical vector definition.
	pub(super) name: Text,
	/// Kind associated with this logical vector definition.
	pub(super) kind: ClassIndexKind,
	/// State associated with this logical vector definition.
	pub(super) state: ClassIndexState,
	/// Options associated with this logical vector definition.
	pub(super) options: ClassIndexOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Aggregate alternatives.
pub(super) enum Aggregate {
	/// Count case of aggregate.
	Count,
	/// Sum case of aggregate.
	Sum,
	/// Average case of aggregate.
	Average,
	/// Minimum case of aggregate.
	Minimum,
	/// Maximum case of aggregate.
	Maximum,
	/// Array group case of aggregate.
	ArrayGroup,
	/// Variance population case of aggregate.
	VariancePopulation,
	/// Standard deviation population case of aggregate.
	StandardDeviationPopulation,
	/// Variance sample case of aggregate.
	VarianceSample,
	/// Standard deviation sample case of aggregate.
	StandardDeviationSample,
}

/// Statically resolved built-ins in the executable Stage 12 profile.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Builtin {
	/// Built-in `MathAbs` operation in the query language.
	MathAbs,
	/// Built-in `MathSqrt` operation in the query language.
	MathSqrt,
	/// Built-in `MathPow` operation in the query language.
	MathPow,
	/// Built-in `MathFloor` operation in the query language.
	MathFloor,
	/// Built-in `MathCeil` operation in the query language.
	MathCeil,
	/// Built-in `StringUpper` operation in the query language.
	StringUpper,
	/// Built-in `StringLower` operation in the query language.
	StringLower,
	/// Built-in `StringLen` operation in the query language.
	StringLen,
	/// Built-in `StringContains` operation in the query language.
	StringContains,
	/// Built-in `StringMatches` operation in the query language.
	StringMatches,
	/// Built-in `StringSlice` operation in the query language.
	StringSlice,
	/// Built-in `RegexMatcher` operation in the query language.
	RegexMatcher,
	/// Built-in `RegexNext` operation in the query language.
	RegexNext,
	/// Built-in `StringSplit` operation in the query language.
	StringSplit,
	/// Built-in `StringConcat` operation in the query language.
	StringConcat,
	/// Built-in `TimeNow` operation in the query language.
	TimeNow(
		/// Built-in `TimeNow` operation in the query language.
		DateTime,
	),
	/// Built-in `TimeFormat` operation in the query language.
	TimeFormat,
	/// Built-in `TimeAdd` operation in the query language.
	TimeAdd,
	/// Built-in `TimeDiff` operation in the query language.
	TimeDiff,
	/// Built-in `ArrayLen` operation in the query language.
	ArrayLen,
	/// Built-in `ArrayConcat` operation in the query language.
	ArrayConcat,
	/// Built-in `ArrayDistinct` operation in the query language.
	ArrayDistinct,
	/// Built-in `ArrayFirst` operation in the query language.
	ArrayFirst,
	/// Built-in `ArrayFlatten` operation in the query language.
	ArrayFlatten,
	/// Built-in `ArrayPush` operation in the query language.
	ArrayPush(
		/// Built-in `ArrayPush` operation in the query language.
		u32,
	),
	/// Built-in `ArrayPop` operation in the query language.
	ArrayPop(
		/// Built-in `ArrayPop` operation in the query language.
		u32,
	),
	/// Built-in `ArrayClear` operation in the query language.
	ArrayClear(
		/// Built-in `ArrayClear` operation in the query language.
		u32,
	),
	/// Built-in `SetLen` operation in the query language.
	SetLen,
	/// Built-in `SetUnion` operation in the query language.
	SetUnion,
	/// Built-in `SetIntersect` operation in the query language.
	SetIntersect,
	/// Built-in `SetDiff` operation in the query language.
	SetDiff,
	/// Built-in `SetHas` operation in the query language.
	SetHas,
	/// Built-in `SetAdd` operation in the query language.
	SetAdd(
		/// Built-in `SetAdd` operation in the query language.
		u32,
	),
	/// Built-in `SetClear` operation in the query language.
	SetClear(
		/// Built-in `SetClear` operation in the query language.
		u32,
	),
	/// Built-in `TypeIs` operation in the query language.
	TypeIs(
		/// Built-in `TypeIs` operation in the query language.
		Type,
	),
	/// Built-in `TypeTo` operation in the query language.
	TypeTo(
		/// Built-in `TypeTo` operation in the query language.
		Type,
	),
	/// Built-in `MetaId` operation in the query language.
	MetaId,
	/// Built-in `MetaClass` operation in the query language.
	MetaClass,
	/// Built-in `MetaType` operation in the query language.
	MetaType,
	/// Built-in `GraphInner` operation in the query language.
	GraphInner,
	/// Built-in `GraphLevelPath` operation in the query language.
	GraphLevelPath,
	/// Built-in `PathAtoms` operation in the query language.
	PathAtoms,
	/// Built-in `PathNodes` operation in the query language.
	PathNodes,
	/// Built-in `PathEdges` operation in the query language.
	PathEdges,
	/// Built-in `PathLength` operation in the query language.
	PathLength,
	/// Built-in `PathCost` operation in the query language.
	PathCost,
	/// Built-in `OptionIsSome` operation in the query language.
	OptionIsSome,
	/// Built-in `OptionIsNone` operation in the query language.
	OptionIsNone,
	/// Built-in `Uuid` operation in the query language.
	Uuid,
	/// Built-in `Ulid` operation in the query language.
	Ulid,
	/// Built-in `RandId` operation in the query language.
	RandId,
	/// Built-in `SearchAnalyze` operation in the query language.
	SearchAnalyze {
		/// Analyzer associated with this builtin.
		analyzer: Arc<SchemaAnalyzer>,
		/// Function associated with this builtin.
		function: Option<NamedFunctionKey>,
	},
	/// Built-in `SearchScore` operation in the query language.
	SearchScore(
		/// Built-in `SearchScore` operation in the query language.
		u16,
	),
	/// Built-in `SearchHighlight` operation in the query language.
	SearchHighlight(
		/// Built-in `SearchHighlight` operation in the query language.
		u16,
	),
	/// Built-in `GeoWithin` operation in the query language.
	GeoWithin,
	/// Built-in `GeoIntersects` operation in the query language.
	GeoIntersects,
	/// Built-in `GeoDistance` operation in the query language.
	GeoDistance,
	/// Streaming recomputation through a captured versioned PURE COST.
	PathCostWith(
		/// Streaming recomputation through a captured versioned PURE COST.
		LogicalPathCost,
	),
}
