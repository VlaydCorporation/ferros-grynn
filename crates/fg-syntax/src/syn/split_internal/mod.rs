//! Owner-side constructors for internal SDK lowering.

#![allow(clippy::wildcard_imports, reason = "Constructors share the owned AST vocabulary")]

use fg_gql_status::TextSpan;

use super::*;

/// Characterizes the immediate-child traversal contract.
#[cfg(test)]
mod tests;

/// Tests direct value children in their original traversal order.
/// Callable names and nested query or closure scopes are not traversed.
pub fn any_value_child<P: FnMut(&Expr) -> bool>(expression: &Expr, mut predicate: P) -> bool {
	match &expression.kind {
		ExprKind::Call {
			arguments,
			..
		}
		| ExprKind::Array(arguments)
		| ExprKind::Set(arguments) => arguments.iter().any(&mut predicate),
		ExprKind::Object(fields) => fields.iter().any(|field| predicate(&field.value)),
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
		} => predicate(expr),
		ExprKind::Binary {
			left,
			right,
			..
		}
		| ExprKind::Fulltext {
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
		} => predicate(left) || predicate(right),
		ExprKind::Knn {
			left,
			k,
			search,
			right,
		} => {
			let search_expression = search.as_ref().and_then(|search| match search {
				KnnSearchArgument::Ef(value) => Some(value.as_ref()),
				KnnSearchArgument::Distance(distance) => match distance {
					KnnDistance::Minkowski(value) | KnnDistance::Custom(value) => {
						Some(value.as_ref())
					}
					KnnDistance::Euclidean
					| KnnDistance::Cosine
					| KnnDistance::Manhattan
					| KnnDistance::InnerProduct
					| KnnDistance::CosineNormalized => None,
				},
			});
			predicate(left)
				|| predicate(k)
				|| search_expression.is_some_and(&mut predicate)
				|| predicate(right)
		}
		ExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => predicate(condition) || predicate(then_value) || predicate(else_value),
		ExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			operand.iter().any(|value| predicate(value))
				|| branches
					.iter()
					.any(|(condition, value)| predicate(condition) || predicate(value))
				|| else_value.iter().any(|value| predicate(value))
		}
		ExprKind::Range {
			start,
			end,
			..
		} => start.iter().chain(end.iter()).any(|value| predicate(value)),
		ExprKind::Literal(_)
		| ExprKind::Parameter(_)
		| ExprKind::Name(_)
		| ExprKind::Subquery(_)
		| ExprKind::MatchSubquery(_)
		| ExprKind::Closure(_) => false,
	}
}

/// Returns whether the mode tracks unique edges and unique atoms.
pub const fn match_mode_uniqueness(mode: MatchMode) -> (bool, bool) {
	match mode {
		MatchMode::RepeatableElements => (false, false),
		MatchMode::DifferentEdges => (true, false),
		MatchMode::DifferentAtoms => (false, true),
	}
}

/// Maps an event owner while keeping name allocation in the selected branch.
#[inline]
pub fn map_event_owner<R, C: FnOnce(&Identifier) -> R, G: FnOnce(&Identifier) -> R>(
	owner: &EventOwnerName,
	class: C,
	graph: G,
) -> R {
	match owner {
		EventOwnerName::Class(name) => class(name),
		EventOwnerName::Graph(name) => graph(name),
	}
}

/// Selects only the requested incidence branch, without eagerly allocating
/// other results.
#[inline]
pub fn with_match_direction<
	C,
	R,
	F: FnOnce(C) -> R,
	B: FnOnce(C) -> R,
	U: FnOnce(C) -> R,
	D: FnOnce(C) -> R,
>(
	direction: MatchDirection,
	context: C,
	forward: F,
	backward: B,
	undirected: U,
	bidirectional: D,
) -> R {
	match direction {
		MatchDirection::Forward => forward(context),
		MatchDirection::Backward => backward(context),
		MatchDirection::Undirected => undirected(context),
		MatchDirection::Bidirectional => bidirectional(context),
	}
}

/// Evaluates the requested meta requirement using caller-owned domain
/// predicates.
pub const fn match_meta_requirement(scope: MatchMetaScope, node: bool, edge: bool) -> bool {
	match scope {
		MatchMetaScope::None => false,
		MatchMetaScope::Meta => true,
		MatchMetaScope::MetaVertex => node,
		MatchMetaScope::MetaEdge => edge,
	}
}

/// Returns whether fulltext matching requires every token.
pub const fn fulltext_requires_all(operator: &FulltextOperator) -> bool {
	match operator {
		FulltextOperator::All => true,
		FulltextOperator::Any | FulltextOperator::Slot(_) | FulltextOperator::AnyOf => false,
	}
}

/// Constructs an internal Block from already evaluated fields.
pub fn new_block(statements: Vec<Statement>, span: TextSpan) -> Block {
	Block {
		statements,
		span,
	}
}

/// Constructs an internal BuildGraphStatement from already evaluated fields.
#[allow(
	clippy::too_many_arguments,
	reason = "One argument per existing AST field preserves the constructor contract"
)]
pub fn new_build_graph_statement(
	bridge: Option<Box<BridgeClause>>,
	mode: DefinitionMode,
	contract: Option<GraphContract>,
	name: Option<Identifier>,
	source: BuildGraphSource,
	root: Option<Expr>,
	nodes: Option<Expr>,
	edges: Option<Expr>,
	limit_nodes: Option<u64>,
	limit_edges: Option<u64>,
	timeout: Option<TimeoutValue>,
) -> BuildGraphStatement {
	BuildGraphStatement {
		bridge,
		mode,
		contract,
		name,
		source,
		root,
		nodes,
		edges,
		limit_nodes,
		limit_edges,
		timeout,
	}
}

/// Constructs an internal ClosureExpr from already evaluated fields.
pub fn new_closure_expr(
	parameters: Vec<ClosureParameter>,
	result_type: Option<TypeName>,
	body: ClosureBody,
) -> ClosureExpr {
	ClosureExpr {
		parameters,
		result_type,
		body,
	}
}

/// Constructs an internal ClosureParameter from already evaluated fields.
pub fn new_closure_parameter(
	name: Identifier,
	declared_type: Option<TypeName>,
) -> ClosureParameter {
	ClosureParameter {
		name,
		declared_type,
	}
}

/// Constructs an internal Expr from already evaluated fields.
pub fn new_expr(kind: ExprKind, span: TextSpan) -> Expr {
	Expr {
		kind,
		span,
	}
}

/// Constructs an internal FunctionSignatureName from already evaluated fields.
pub fn new_function_signature_name(
	namespace: Identifier,
	name: Identifier,
	parameters: Option<Vec<TypeName>>,
) -> FunctionSignatureName {
	FunctionSignatureName {
		namespace,
		name,
		parameters,
	}
}

/// Constructs an internal GraphContract from already evaluated fields.
pub fn new_graph_contract(
	family: Option<GraphFamilyName>,
	validators: Vec<GraphValidatorName>,
	span: TextSpan,
) -> GraphContract {
	GraphContract {
		family,
		validators,
		span,
	}
}

/// Constructs an internal Identifier from already evaluated fields.
pub fn new_identifier(value: Box<str>, quoted: bool, span: TextSpan) -> Identifier {
	Identifier {
		value,
		quoted,
		span,
	}
}

/// Constructs an internal LetStatement from already evaluated fields.
pub fn new_let_statement(
	name: Identifier,
	declared_type: Option<TypeName>,
	value: LetValue,
) -> LetStatement {
	LetStatement {
		name,
		declared_type,
		value,
	}
}

/// Constructs an internal ObjectField from already evaluated fields.
pub fn new_object_field(name: Identifier, value: Expr, span: TextSpan) -> ObjectField {
	ObjectField {
		name,
		value,
		span,
	}
}

/// Constructs an internal Program from already evaluated fields.
pub fn new_program(statements: Vec<Statement>) -> Program {
	Program {
		statements,
	}
}

/// Constructs an internal ReturnGraph from already evaluated fields.
pub fn new_return_graph(
	bridge: Option<Box<BridgeClause>>,
	contract: Option<GraphContract>,
	root: Option<Expr>,
	nodes: Expr,
	edges: Expr,
	span: TextSpan,
) -> ReturnGraph {
	ReturnGraph {
		bridge,
		contract,
		root,
		nodes,
		edges,
		span,
	}
}

/// Constructs an internal ReturnStatement from already evaluated fields.
pub fn new_return_statement(value: Expr, alias: Option<Identifier>) -> ReturnStatement {
	ReturnStatement {
		value,
		alias,
	}
}

/// Constructs an internal SelectJoin from already evaluated fields.
pub fn new_select_join(
	kind: SelectJoinKind,
	source: SelectSource,
	predicate: Option<Expr>,
	span: TextSpan,
) -> SelectJoin {
	SelectJoin {
		kind,
		source,
		predicate,
		span,
	}
}

/// Constructs an internal SelectOrder from already evaluated fields.
pub fn new_select_order(
	expression: Expr,
	random: bool,
	collate: bool,
	numeric: bool,
	direction: SelectOrderDirection,
	nulls: SelectNulls,
) -> SelectOrder {
	SelectOrder {
		expression,
		random,
		collate,
		numeric,
		direction,
		nulls,
	}
}

/// Constructs an internal SelectPath from already evaluated fields.
pub fn new_select_path(segments: Vec<Identifier>, span: TextSpan) -> SelectPath {
	SelectPath {
		segments,
		span,
	}
}

/// Constructs an internal SelectSource from already evaluated fields.
pub fn new_select_source(
	kind: SelectSourceKind,
	graph: Option<Expr>,
	alias: Option<Identifier>,
	span: TextSpan,
) -> SelectSource {
	SelectSource {
		kind,
		graph,
		alias,
		span,
	}
}

/// Constructs an internal SelectStatement from already evaluated fields.
#[allow(
	clippy::too_many_arguments,
	reason = "One argument per existing AST field preserves the constructor contract"
)]
pub fn new_select_statement(
	union_inputs: Vec<SelectStatement>,
	union_purpose: SelectUnionPurpose,
	lexical_id: Option<u32>,
	only: bool,
	value: bool,
	projection: Vec<SelectItem>,
	source: SelectSource,
	joins: Vec<SelectJoin>,
	access: Option<SelectAccessHint>,
	filter: Option<Expr>,
	split: Vec<Expr>,
	group: Option<SelectGroup>,
	omit: Vec<SelectPath>,
	fetch: Vec<SelectPath>,
	fetch_bridges: Option<Box<[Option<BridgeClause>]>>,
	order: Vec<SelectOrder>,
	limit: Option<Expr>,
	start: Option<SelectStart>,
	tempfiles: bool,
	return_graph: Option<Box<ReturnGraph>>,
) -> SelectStatement {
	SelectStatement {
		union_inputs,
		union_purpose,
		lexical_id,
		only,
		value,
		projection,
		source,
		joins,
		access,
		filter,
		split,
		group,
		omit,
		fetch,
		fetch_bridges,
		order,
		limit,
		start,
		tempfiles,
		return_graph,
	}
}

/// Constructs an internal Statement from already evaluated fields.
pub fn new_statement(
	kind: StatementKind,
	span: TextSpan,
	timeout: Option<TimeoutValue>,
) -> Statement {
	Statement {
		kind,
		span,
		timeout,
	}
}

/// Constructs an internal TypeName from already evaluated fields.
pub fn new_type_name(value: Box<str>, span: TextSpan) -> TypeName {
	TypeName {
		value,
		span,
	}
}

/// Internal SDK union classification.
pub use super::stmt::SelectUnionPurpose;

/// Examines real tokens rather than quoted names, strings or comments.
///
/// # Errors
/// Returns the unchanged lexer diagnostics.
pub fn has_bridge_clause(source: &str) -> FerrosGrynnResult<bool> {
	super::has_bridge_clause(source)
}

/// Private syntax metadata used by semantic analysis and SDK lowering.
pub trait SelectMetadata: Sized {
	/// Returns the existing owned union inputs without cloning them.
	fn union_inputs(&self) -> &Vec<Self>;
	/// Replaces already prepared union inputs.
	fn set_union_inputs(&mut self, inputs: Vec<Self>);
	/// Returns the union purpose.
	fn union_purpose(&self) -> SelectUnionPurpose;
	/// Sets the union purpose.
	fn set_union_purpose(&mut self, purpose: SelectUnionPurpose);
	/// Returns the lexical origin.
	fn lexical_id(&self) -> Option<u32>;
	/// Sets the lexical origin.
	fn set_lexical_id(&mut self, id: Option<u32>);
	/// Wraps already admitted independent inputs without cloning.
	fn independent_union(inputs: Vec<Self>, lexical_id: u32) -> Self;
}

impl SelectMetadata for SelectStatement {
	fn union_inputs(&self) -> &Vec<Self> {
		&self.union_inputs
	}

	fn set_union_inputs(&mut self, inputs: Vec<Self>) {
		self.union_inputs = inputs;
	}

	fn union_purpose(&self) -> SelectUnionPurpose {
		self.union_purpose
	}

	fn set_union_purpose(&mut self, purpose: SelectUnionPurpose) {
		self.union_purpose = purpose;
	}

	fn lexical_id(&self) -> Option<u32> {
		self.lexical_id
	}

	fn set_lexical_id(&mut self, id: Option<u32>) {
		self.lexical_id = id;
	}

	fn independent_union(inputs: Vec<Self>, lexical_id: u32) -> Self {
		Self::independent_union(inputs, lexical_id)
	}
}

/// Internal DML target inspection used by analysis.
pub trait DmlTargetInternal {
	/// Returns the parsed class identifier.
	fn class(&self) -> &Identifier;
	/// Returns whether the target is one exact record.
	fn is_exact(&self) -> bool;
}
impl DmlTargetInternal for DmlTarget {
	fn class(&self) -> &Identifier {
		self.class()
	}

	fn is_exact(&self) -> bool {
		self.is_exact()
	}
}

/// Internal diagnostic location of a parsed record key.
pub trait RecordKeyInternal {
	/// Returns the original UTF-8 byte span.
	fn span(&self) -> TextSpan;
}
impl RecordKeyInternal for RecordKey {
	fn span(&self) -> TextSpan {
		self.span()
	}
}

/// Exhaustive owner-side dispatch for pure cross-domain enum mappings.
pub trait EnumChoice {
	/// One value for each existing variant, in declaration order.
	type Choices<T>;
	/// Selects an already evaluated value without allocation or indexing.
	fn choose<T>(&self, choices: Self::Choices<T>) -> T;
}
impl<E: EnumChoice + ?Sized> EnumChoice for &E {
	type Choices<T> = E::Choices<T>;

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		E::choose(*self, choices)
	}
}
impl EnumChoice for AnalyzerTokenizerName {
	type Choices<T> = [T; 4];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2, variant_3] = choices;
		match self {
			Self::Blank => variant_0,
			Self::Camel => variant_1,
			Self::Class => variant_2,
			Self::Punct => variant_3,
		}
	}
}
impl EnumChoice for ClassKindName {
	type Choices<T> = [T; 2];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1] = choices;
		match self {
			Self::Node => variant_0,
			Self::Edge => variant_1,
		}
	}
}
impl EnumChoice for ClassSchemaModeName {
	type Choices<T> = [T; 2];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1] = choices;
		match self {
			Self::Schemaless => variant_0,
			Self::Schemafull => variant_1,
		}
	}
}
impl EnumChoice for ExecutionModeName {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::BestEffort => variant_0,
			Self::Deterministic => variant_1,
			Self::Fast => variant_2,
		}
	}
}
impl EnumChoice for ExplainMode {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::Logical => variant_0,
			Self::Full => variant_1,
			Self::Analyze => variant_2,
		}
	}
}
impl EnumChoice for FunctionEffectName {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::Pure => variant_0,
			Self::Stable => variant_1,
			Self::Impure => variant_2,
		}
	}
}
impl EnumChoice for GraphFamilyName {
	type Choices<T> = [T; 7];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2, variant_3, variant_4, variant_5, variant_6] = choices;
		match self {
			Self::Graph => variant_0,
			Self::Tree => variant_1,
			Self::RootedTree => variant_2,
			Self::Forest => variant_3,
			Self::Dag => variant_4,
			Self::Hyper => variant_5,
			Self::Meta => variant_6,
		}
	}
}
impl EnumChoice for GraphValidatorName {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::Simple => variant_0,
			Self::Bipartite => variant_1,
			Self::Complete => variant_2,
		}
	}
}
impl EnumChoice for LiveOutputName {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::Full => variant_0,
			Self::Diff => variant_1,
			Self::Patch => variant_2,
		}
	}
}
impl EnumChoice for LiveOverflowName {
	type Choices<T> = [T; 3];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2] = choices;
		match self {
			Self::Gap => variant_0,
			Self::Disconnect => variant_1,
			Self::Block => variant_2,
		}
	}
}
impl EnumChoice for MatchEndpointKind {
	type Choices<T> = [T; 2];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1] = choices;
		match self {
			Self::Node => variant_0,
			Self::Edge => variant_1,
		}
	}
}
impl EnumChoice for MatchPathMode {
	type Choices<T> = [T; 4];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1, variant_2, variant_3] = choices;
		match self {
			Self::Walk => variant_0,
			Self::Trail => variant_1,
			Self::Acyclic => variant_2,
			Self::Simple => variant_3,
		}
	}
}
impl EnumChoice for SelectNulls {
	type Choices<T> = [T; 2];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1] = choices;
		match self {
			Self::First => variant_0,
			Self::Last => variant_1,
		}
	}
}
impl EnumChoice for SelectOrderDirection {
	type Choices<T> = [T; 2];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [variant_0, variant_1] = choices;
		match self {
			Self::Ascending => variant_0,
			Self::Descending => variant_1,
		}
	}
}
impl EnumChoice for VectorElementName {
	type Choices<T> = [T; 8];

	fn choose<T>(&self, choices: Self::Choices<T>) -> T {
		let [
			variant_0,
			variant_1,
			variant_2,
			variant_3,
			variant_4,
			variant_5,
			variant_6,
			variant_7,
		] = choices;
		match self {
			Self::F64 => variant_0,
			Self::F32 => variant_1,
			Self::F16 => variant_2,
			Self::I64 => variant_3,
			Self::I32 => variant_4,
			Self::I16 => variant_5,
			Self::I8 => variant_6,
			Self::U8 => variant_7,
		}
	}
}

use std::collections::BTreeMap;

/// Collects expr names.
pub fn collect_expr_names(expr: &Expr, names: &mut BTreeMap<Box<str>, TextSpan>) {
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

/// Returns the source span of an optional import graph declaration.
pub fn import_graph_span(graph: Option<&ImportGraphRef>) -> TextSpan {
	match graph {
		Some(ImportGraphRef::Name(name) | ImportGraphRef::Parameter(name)) => name.span,
		None => TextSpan::new(0, 0),
	}
}

/// Public classification of the statement responsible for one query result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryStatementKind {
	/// A EXPLAIN statement or control operation.
	Explain,
	/// A BEGIN statement or control operation.
	Begin,
	/// A COMMIT statement or control operation.
	Commit,
	/// A CANCEL statement or control operation.
	Cancel,
	/// A EXPRESSION statement or control operation.
	Expression,
	/// A IF statement or control operation.
	If,
	/// A FOR statement or control operation.
	For,
	/// A WHILE statement or control operation.
	While,
	/// A TRY statement or control operation.
	Try,
	/// A THROW statement or control operation.
	Throw,
	/// A SLEEP statement or control operation.
	Sleep,
	/// A ASSERT statement or control operation.
	Assert,
	/// A BUILD GRAPH statement or control operation.
	BuildGraph,
	/// A CREATE statement or control operation.
	Create,
	/// A UPDATE statement or control operation.
	Update,
	/// A UPSERT statement or control operation.
	Upsert,
	/// A DELETE statement or control operation.
	Delete,
	/// A ATTACH statement or control operation.
	Attach,
	/// A DETACH statement or control operation.
	Detach,
	/// A RELATE statement or control operation.
	Relate,
	/// A DEFINE statement or control operation.
	Define,
	/// A REMOVE statement or control operation.
	Remove,
	/// A ALTER statement or control operation.
	Alter,
	/// A INFO statement or control operation.
	Info,
	/// Whole-commit durable history, including explicit gaps.
	ShowChanges,
	/// Transactional LIVE query registration.
	Live,
	/// Transactional confirmation of an issued LIVE delivery.
	Ack,
	/// A MAINTENANCE statement or control operation.
	Maintenance,
	/// A KILL statement or control operation.
	Kill,
	/// A LET statement or control operation.
	Let,
	/// A MATCH statement or control operation.
	Match,
	/// A RETURN statement or control operation.
	Return,
	/// A RETURN GRAPH statement or control operation.
	ReturnGraph,
	/// A SELECT statement or control operation.
	Select,
	/// A TRAVERSE statement or control operation.
	Traverse,
	/// A IMPORT statement or control operation.
	Import,
}

/// Returns the statement kind result.
pub const fn statement_kind(statement: &StatementKind) -> QueryStatementKind {
	match statement {
		StatementKind::Explain(_) => QueryStatementKind::Explain,
		StatementKind::Begin(_) => QueryStatementKind::Begin,
		StatementKind::Commit(_) => QueryStatementKind::Commit,
		StatementKind::Cancel => QueryStatementKind::Cancel,
		StatementKind::Expression(_) | StatementKind::Break | StatementKind::Continue => {
			QueryStatementKind::Expression
		}
		StatementKind::If(_) => QueryStatementKind::If,
		StatementKind::For(_) => QueryStatementKind::For,
		StatementKind::While(_) => QueryStatementKind::While,
		StatementKind::Try(_) => QueryStatementKind::Try,
		StatementKind::Throw(_) => QueryStatementKind::Throw,
		StatementKind::Sleep(_) => QueryStatementKind::Sleep,
		StatementKind::AssertInvariants(_) => QueryStatementKind::Assert,
		StatementKind::BuildGraph(_) => QueryStatementKind::BuildGraph,
		StatementKind::Attach(_) => QueryStatementKind::Attach,
		StatementKind::Create(_) => QueryStatementKind::Create,
		StatementKind::Update(_) => QueryStatementKind::Update,
		StatementKind::UpdateGraph(_) => QueryStatementKind::Update,
		StatementKind::Upsert(_) => QueryStatementKind::Upsert,
		StatementKind::Delete(_) => QueryStatementKind::Delete,
		StatementKind::Detach(_) => QueryStatementKind::Detach,
		StatementKind::DefineGraph(_)
		| StatementKind::DefineClass(_)
		| StatementKind::DefineField(_)
		| StatementKind::DefineIndex(_)
		| StatementKind::DefineGraphIndex(_)
		| StatementKind::DefineAnalyzer(_)
		| StatementKind::DefineFunction(_)
		| StatementKind::DefineEvent(_)
		| StatementKind::DefineParameter(_) => QueryStatementKind::Define,
		StatementKind::Remove(_) => QueryStatementKind::Remove,
		StatementKind::Alter(_) => QueryStatementKind::Alter,
		StatementKind::Info(_) => QueryStatementKind::Info,
		StatementKind::ShowChanges(_) => QueryStatementKind::ShowChanges,
		StatementKind::Maintenance(_) => QueryStatementKind::Maintenance,
		StatementKind::Kill(_) => QueryStatementKind::Kill,
		StatementKind::Import(_) => QueryStatementKind::Import,
		StatementKind::Let(_) => QueryStatementKind::Let,
		StatementKind::LiveSelect(_) | StatementKind::LiveLet(_) => QueryStatementKind::Live,
		StatementKind::AckLive(_) => QueryStatementKind::Ack,
		StatementKind::Match(_) => QueryStatementKind::Match,
		StatementKind::Relate(_) => QueryStatementKind::Relate,
		StatementKind::Return(_) => QueryStatementKind::Return,
		StatementKind::ReturnGraph(_) => QueryStatementKind::ReturnGraph,
		StatementKind::Select(_) => QueryStatementKind::Select,
		StatementKind::Traverse(_) => QueryStatementKind::Traverse,
	}
}
