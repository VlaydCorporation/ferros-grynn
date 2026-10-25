//! Logical query descriptors shared by Rust registration and GQL lowering.

use super::LiveBindingId;
use crate::{
	datastore::ClassId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{GraphRef, Object, RecordId, Text, Value},
};

/// Checked field path; each segment is literal, including dots inside a name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LivePath(
	/// Checked field path; each segment is literal, including dots inside a
	/// name.
	Vec<Text>,
);

impl LivePath {
	/// Checks a nonempty path with at most 64 nonempty segments.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for empty, overdeep or NUL names.
	pub fn new(segments: Vec<Text>) -> FerrosGrynnResult<Self> {
		if segments.is_empty()
			|| segments.len() > 64
			|| segments.iter().any(|name| name.is_empty() || name.as_str().contains('\0'))
		{
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE field path must contain 1..64 nonempty names without NUL",
			));
		}
		Ok(Self(segments))
	}

	/// Creates a single field path without interpreting dots as separators.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for an empty or NUL name.
	pub fn field<IntoText: Into<Text>>(name: IntoText) -> FerrosGrynnResult<Self> {
		Self::new(vec![name.into()])
	}

	/// Returns the checked ordered field names.
	pub fn segments(&self) -> &[Text] {
		&self.0
	}
}

/// Source descriptor; containment does not implicitly expand a graph scope.
///
/// ```
/// # use fg_engine as ferros_grynn;
/// # fn scoped(graph: ferros_grynn::graph::GraphRef) -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
/// use ferros_grynn::live::{LiveQuery, LiveSource};
/// let query = LiveQuery::new(LiveSource::GraphRecord {
///     graph,
///     record: "person:alice".parse()?,
/// });
/// assert!(matches!(query.source(), LiveSource::GraphRecord { .. }));
/// # Ok(())
/// # }
/// ```
///
/// A reactive scope follows the binding identity. An unchanged graph-name
/// value retains its resolved graph identity through renames; removing that
/// graph faults the subscription even if its name is reused.
///
/// ```rust,standalone_crate
/// # use fg_engine as ferros_grynn;
/// # fn registered() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
/// use ferros_grynn::{
///     datastore::{ClassSchema, Datastore},
///     graph::{AtomKind, GraphKind},
///     live::{LiveBindingOptions, LiveExpr, LiveOptions, LiveQuery, LiveSource},
///     values::{Object, PropertyMap, RecordId},
/// };
/// let store = Datastore::in_memory();
/// let mut tx = store.begin();
/// let class = tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
/// let graph = tx.define_graph("people", GraphKind::Graph)?;
/// let record: RecordId = "person:alice".parse()?;
/// tx.create_atom(record.clone(), PropertyMap::new())?;
/// tx.attach_node(graph.id(), &record, None, None)?;
/// let scope = tx.define_live_binding(
///     LiveExpr::Literal(graph.into()),
///     Object::new(),
///     LiveBindingOptions::default(),
/// )?;
/// tx.commit()?;
/// let mut class_subscription = store.subscribe_live(
///     LiveQuery::new(LiveSource::GraphClassExpr {
///         graph: Box::new(LiveExpr::Binding(scope)),
///         class,
///     }),
///     LiveOptions::default(),
/// )?;
/// let mut record_subscription = store.subscribe_live(
///     LiveQuery::new(LiveSource::GraphRecordExpr {
///         graph: Box::new(LiveExpr::Binding(scope)),
///         record,
///     }),
///     LiveOptions::default(),
/// )?;
/// let mut graph_subscription = store.subscribe_live(
///     LiveQuery::new(LiveSource::GraphExpr(Box::new(LiveExpr::Binding(scope)))),
///     LiveOptions::default(),
/// )?;
/// class_subscription.close()?;
/// record_subscription.close()?;
/// graph_subscription.close()?;
/// # Ok(())
/// # }
/// # registered()?;
/// # Ok::<(), ferros_grynn::gql_status::FerrosGrynnError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveSource {
	/// All database-global records of one class identity.
	Class(
		/// All database-global records of one class identity.
		ClassId,
	),
	/// One global record, including a dependency on its absence.
	Record(
		/// One global record, including a dependency on its absence.
		RecordId,
	),
	/// All qualified occurrences of one graph.
	Graph(
		/// All qualified occurrences of one graph.
		GraphRef,
	),
	/// Occurrences of one class in one graph, with effective properties.
	GraphClass {
		/// Selected graph identity and origin.
		graph: GraphRef,
		/// Immutable class identity.
		class: ClassId,
	},
	/// One qualified occurrence, including a dependency on absent membership.
	GraphRecord {
		/// Selected graph identity, independent of later renames.
		graph: GraphRef,
		/// Exact base record identity; properties are graph-effective.
		record: RecordId,
	},
	/// Qualified class source whose graph is evaluated against each commit
	/// root.
	GraphClassExpr {
		/// Read-only graph scope, yielding a GraphRef or graph name.
		graph: Box<LiveExpr>,
		/// Immutable class identity, independent of graph scope changes.
		class: ClassId,
	},
	/// Exact base record in a graph selected by a read-only scope expression.
	GraphRecordExpr {
		/// Read-only graph scope; Binding tracks a LIVE LET identity.
		graph: Box<LiveExpr>,
		/// Exact base identity; results retain the resolved graph occurrence.
		record: RecordId,
	},
	/// All qualified occurrences in a graph selected once by a read-only
	/// expression. Includes every class in the snapshot, without recursively
	/// expanding containment. The expression must yield a GraphRef or name.
	GraphExpr(
		/// All qualified occurrences in a graph selected once by a read-only
		/// expression. Includes every class in the snapshot, without
		/// recursively expanding containment. The expression must yield a
		/// GraphRef or name.
		Box<LiveExpr>,
	),
}

impl LiveSource {
	/// Borrows the scope recipe without resolving names or reactive values.
	pub(crate) fn graph_expression(&self) -> Option<&LiveExpr> {
		match self {
			Self::GraphExpr(graph)
			| Self::GraphClassExpr {
				graph,
				..
			}
			| Self::GraphRecordExpr {
				graph,
				..
			} => Some(graph),
			_ => None,
		}
	}
}

/// Ordinary relational JOIN behavior, independent of physical JOIN ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LiveJoinKind {
	/// Retains matching pairs.
	Inner,
	/// Retains matching pairs and one unmatched right slot when none match.
	Left,
	/// Retains every pair without an ON predicate.
	Cross,
}

/// Owned JOIN source and logical qualifier; identities follow declaration
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct LiveJoin {
	/// One source recipe, independently scoped from FROM.
	pub(crate) source: LiveSource,
	/// Checked source qualifier.
	pub(crate) alias: Text,
	/// Relational matching behavior.
	pub(crate) kind: LiveJoinKind,
	/// INNER/LEFT predicate; CROSS has no predicate.
	pub(crate) predicate: Option<LiveExpr>,
}

impl LiveJoin {
	/// Constructs one checked source recipe; types and aliases bind at
	/// registration.
	///
	/// # Errors
	/// Rejects an empty/NUL alias, missing INNER/LEFT ON, or a CROSS ON.
	pub fn new<IntoText: Into<Text>>(
		source: LiveSource,
		alias: IntoText,
		kind: LiveJoinKind,
		predicate: Option<LiveExpr>,
	) -> FerrosGrynnResult<Self> {
		let alias = alias.into();
		LiveQuery::validate_alias(alias.as_str())?;
		if matches!(kind, LiveJoinKind::Cross) == predicate.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE INNER/LEFT require ON and CROSS forbids ON",
			));
		}
		Ok(Self {
			source,
			alias,
			kind,
			predicate,
		})
	}

	/// Borrows the independently owned source recipe.
	pub const fn source(&self) -> &LiveSource {
		&self.source
	}

	/// Borrows the logical source qualifier.
	pub const fn alias(&self) -> &Text {
		&self.alias
	}

	/// Returns relational JOIN behavior.
	pub const fn kind(&self) -> LiveJoinKind {
		self.kind
	}

	/// Borrows ON; absent only for CROSS.
	pub const fn predicate(&self) -> Option<&LiveExpr> {
		self.predicate.as_ref()
	}
}

/// Logical binary operation, lowered through the ordinary expression analyzer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LiveBinaryOperator {
	/// Exact runtime equality.
	Equal,
	/// Exact runtime inequality.
	NotEqual,
	/// Ordered comparison.
	Less,
	/// Inclusive ordered comparison.
	LessOrEqual,
	/// Ordered comparison.
	Greater,
	/// Inclusive ordered comparison.
	GreaterOrEqual,
	/// Short-circuit boolean conjunction.
	And,
	/// Short-circuit boolean disjunction.
	Or,
	/// Existing numeric or collection addition semantics.
	Add,
	/// Existing numeric subtraction semantics.
	Subtract,
	/// Existing numeric multiplication semantics.
	Multiply,
	/// Existing numeric division semantics.
	Divide,
	/// Existing membership predicate.
	In,
	/// Existing nullish coalescing semantics.
	Coalesce,
	/// Existing numeric exponentiation semantics.
	Power,
	/// Existing numeric remainder semantics.
	Remainder,
	/// Exact runtime equality, matching IS.
	Is,
	/// Exact runtime inequality, matching NOT IS.
	NotIs,
	/// Negated membership predicate.
	NotIn,
	/// Every left member belongs to the right operand.
	AllIn,
	/// At least one left member belongs to the right operand.
	AnyIn,
	/// No left member belongs to the right operand.
	NoneIn,
	/// At least one left member equals the right operand.
	AnyEqual,
	/// Every left member equals the right operand.
	AllEqual,
	/// The left operand contains the right value.
	Contains,
	/// The left operand does not contain the right value.
	ContainsNot,
	/// The left operand contains at least one right member.
	ContainsAny,
	/// The left operand contains every right member.
	ContainsAll,
	/// Short-circuit truthy coalescing semantics.
	Elvis,
	/// Exact within predicate; boundary contact is excluded.
	Inside,
	/// Negated within predicate with ordinary sentinel semantics.
	NotInside,
	/// Every left geometry is within the right geometry.
	AllInside,
	/// At least one left geometry is within the right geometry.
	AnyInside,
	/// No left geometry is within the right geometry.
	NoneInside,
	/// Ordinary outside predicate.
	Outside,
	/// Exact intersection predicate including boundary contact.
	Intersects,
}

/// Owned logical expression; named calls undergo normal effect/version
/// analysis.
///
/// ```
/// # use fg_engine as ferros_grynn;
/// use ferros_grynn::live::LiveExpr;
/// let recipe = LiveExpr::Object(vec![(
/// 	"values".into(),
/// 	LiveExpr::Array(vec![
/// 		LiveExpr::Negate(Box::new(LiveExpr::Literal(5.into()))),
/// 		LiveExpr::Positive(Box::new(LiveExpr::Parameter("fixed".into()))),
/// 	]),
/// )]);
/// assert!(matches!(recipe, LiveExpr::Object(_)));
/// let first = LiveExpr::Index {
/// 	base: Box::new(LiveExpr::Access {
/// 		base: Box::new(recipe),
/// 		field: "values".into(),
/// 		null_safe: false,
/// 	}),
/// 	index: Box::new(LiveExpr::Literal(0.into())),
/// };
/// assert!(matches!(first, LiveExpr::Index { .. }));
/// let selected = LiveExpr::Conditional {
/// 	condition: Box::new(LiveExpr::Literal(true.into())),
/// 	then_value: Box::new(first),
/// 	else_value: Box::new(LiveExpr::Literal(0.into())),
/// };
/// assert!(matches!(selected, LiveExpr::Conditional { .. }));
/// let unique = LiveExpr::Set(vec![LiveExpr::Parameter("fixed".into())]);
/// assert!(matches!(unique, LiveExpr::Set(_)));
/// let selection = LiveExpr::Case {
/// 	operand: None,
/// 	branches: vec![(LiveExpr::Literal(true.into()), unique)],
/// 	else_value: None,
/// };
/// assert!(matches!(selection, LiveExpr::Case { .. }));
/// let integer = LiveExpr::Cast {
/// 	target: "int".into(),
/// 	value: Box::new(LiveExpr::Literal("9007199254740993".into())),
/// };
/// assert!(matches!(integer, LiveExpr::Cast { .. }));
/// let interval = LiveExpr::Range {
/// 	start: Some(Box::new(LiveExpr::Parameter("fixed".into()))),
/// 	end: None,
/// 	inclusive_end: false,
/// };
/// assert!(matches!(interval, LiveExpr::Range { .. }));
/// let typed = LiveExpr::CallWithOptions {
/// 	name: "type::is".into(),
/// 	type_argument: Some("int".into()),
/// 	distinct: false,
/// 	arguments: vec![(None, LiveExpr::Parameter("fixed".into()))],
/// };
/// assert!(matches!(typed, LiveExpr::CallWithOptions { .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[allow(
	clippy::tabs_in_doc_comments,
	reason = "nightly rustfmt uses required hard tabs in Rustdoc examples"
)]
pub enum LiveExpr {
	/// Frozen literal. Runtime-only values are rejected during registration.
	Literal(
		/// Frozen literal. Runtime-only values are rejected during
		/// registration.
		Value,
	),
	/// Read a source row path. A class prefix qualifies only when no declared
	/// property has that name; declared nested properties retain precedence.
	Field(
		/// Read a source row path. A class prefix qualifies only when no
		/// declared property has that name; declared nested properties retain
		/// precedence.
		LivePath,
	),
	/// Ordinary LET or execute parameter, frozen by value at registration.
	Parameter(
		/// Ordinary LET or execute parameter, frozen by value at registration.
		Text,
	),
	/// Reactive binding identity, independent of lexical spelling.
	Binding(
		/// Reactive binding identity, independent of lexical spelling.
		LiveBindingId,
	),
	/// Boolean negation using the shared evaluator.
	Not(
		/// Boolean negation using the shared evaluator.
		Box<Self>,
	),
	/// Shared analyzer/evaluator binary operation.
	Binary {
		/// Left operand.
		left: Box<Self>,
		/// Operation preserving short-circuit behavior.
		op: LiveBinaryOperator,
		/// Right operand.
		right: Box<Self>,
	},
	/// Function call; nondeterministic and IMPURE calls are rejected.
	Call {
		/// Qualified builtin or published PURE function name.
		name: Text,
		/// Ordered arguments.
		arguments: Vec<Self>,
	},
	/// Call modifiers resolved by the ordinary type/signature/effect analyzer.
	CallWithOptions {
		/// Qualified builtin or published PURE function name.
		name: Text,
		/// Optional symbolic type argument, independent of storage types.
		type_argument: Option<Text>,
		/// DISTINCT is valid only for an ordinary SELECT aggregate.
		distinct: bool,
		/// Optional names and values in their original evaluation order.
		arguments: Vec<(Option<Text>, Self)>,
	},
	/// Read-only SELECT expression, including data-dependent LIVE LET values.
	Select(
		/// Read-only SELECT expression, including data-dependent LIVE LET
		/// values.
		Box<LiveQuery>,
	),
	/// Ordered array construction using the shared evaluator.
	Array(
		/// Ordered array construction using the shared evaluator.
		Vec<Self>,
	),
	/// Set construction with ordinary Value equality and canonical ordering.
	/// Every input is captured and evaluated before duplicate elimination.
	Set(
		/// Set construction with ordinary Value equality and canonical
		/// ordering. Every input is captured and evaluated before duplicate
		/// elimination.
		Vec<Self>,
	),
	/// Ordered object construction; duplicate keys follow ordinary SELECT
	/// rules.
	Object(
		/// Ordered object construction; duplicate keys follow ordinary SELECT
		/// rules.
		Vec<(Text, Self)>,
	),
	/// Numeric negation using exact runtime Number semantics.
	Negate(
		/// Numeric negation using exact runtime Number semantics.
		Box<Self>,
	),
	/// Numeric unary plus, including ordinary type validation.
	Positive(
		/// Numeric unary plus, including ordinary type validation.
		Box<Self>,
	),
	/// Literal field access on an expression, including qualified records.
	Access {
		/// Object, record or qualified occurrence expression.
		base: Box<Self>,
		/// One literal field name, without splitting dots.
		field: Text,
		/// Return NONE for a nullish base using ordinary SELECT rules.
		null_safe: bool,
	},
	/// Checked indexed access using ordinary array/object/vector semantics.
	Index {
		/// Indexed collection expression.
		base: Box<Self>,
		/// Index expression with ordinary runtime type validation.
		index: Box<Self>,
	},
	/// Ordinary IF truthiness; only the selected branch is evaluated.
	/// All branches are validated and captured during registration.
	Conditional {
		/// Condition using ordinary runtime truthiness.
		condition: Box<Self>,
		/// Value selected by a truthy condition.
		then_value: Box<Self>,
		/// Value selected otherwise.
		else_value: Box<Self>,
	},
	/// Ordinary searched/simple CASE, selecting the first matching branch.
	/// Every child is validated and captured at registration.
	Case {
		/// Optional simple-CASE operand, evaluated once per result row.
		operand: Option<Box<Self>>,
		/// Nonempty ordered condition/value pairs, checked at registration.
		branches: Vec<(Self, Self)>,
		/// Optional fallback; no match without ELSE returns NONE.
		else_value: Option<Box<Self>>,
	},
	/// Ordinary explicit conversion, validated through the shared analyzer.
	Cast {
		/// Symbolic type name, such as `int`, `option<int>` or `vector<f64>`.
		/// Names and generic nesting are checked before ordinary type parsing.
		target: Text,
		/// Value to convert, including frozen or reactive inputs.
		value: Box<Self>,
	},
	/// Ordinary range construction without enumerating its members.
	Range {
		/// Optional included lower bound; absence leaves the range open.
		start: Option<Box<Self>>,
		/// Optional upper bound; absence leaves the range open.
		end: Option<Box<Self>>,
		/// Include a present upper bound instead of excluding it.
		inclusive_end: bool,
	},
}

/// Admits symbolic type parsing and inferred copies before scanning or
/// allocating. Semantic type resolution stays in the ordinary analyzer.
///
/// # Errors
///
/// Returns typed size/work, malformed nesting or depth-limit errors.
pub fn admit_cast_type_name<'name, Admission>(
	raw: &'name [u8],
	copies: usize,
	admit: &mut Admission,
) -> FerrosGrynnResult<&'name str>
where
	Admission: FnMut(usize, u64) -> FerrosGrynnResult<()>,
{
	admit(raw.len(), raw.len() as u64)?;
	let name = std::str::from_utf8(raw)
		.map_err(|_error| FerrosGrynnError::invalid_argument("LIVE cast type is not UTF-8"))?;
	if name.is_empty() {
		return Err(FerrosGrynnError::invalid_argument("LIVE cast type name is empty"));
	}
	let mut depth = 0_usize;
	let mut maximum = 0_usize;
	for byte in name.bytes() {
		match byte {
			b'<' => {
				depth += 1;
				if depth > 64 {
					return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
						"LIVE cast type depth",
						64,
					));
				}
				maximum = maximum.max(depth);
			}
			b'>' => {
				depth = depth.checked_sub(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE cast type nesting is malformed")
				})?;
			}
			_ => {}
		}
	}
	if depth != 0 {
		return Err(FerrosGrynnError::invalid_argument("LIVE cast type nesting is malformed"));
	}
	let scale = maximum
		.checked_add(8)
		.and_then(|value| value.checked_mul(copies))
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE cast type copies"))?;
	let bytes = name
		.len()
		.checked_add(std::mem::size_of::<crate::values::Type>())
		.and_then(|bytes| bytes.checked_mul(scale))
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE cast type size"))?;
	let work = name
		.len()
		.checked_mul(maximum + 1)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE cast type work"))?;
	admit(bytes, work as u64)?;
	Ok(name)
}

impl LiveExpr {
	/// Creates a field expression without requiring GQL text.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for an invalid field name.
	pub fn field<IntoText: Into<Text>>(name: IntoText) -> FerrosGrynnResult<Self> {
		Ok(Self::Field(LivePath::field(name)?))
	}

	/// Composes a logical binary operation; type checking occurs at
	/// registration.
	#[must_use]
	pub fn binary(self, op: LiveBinaryOperator, right: Self) -> Self {
		Self::Binary {
			left: Box::new(self),
			op,
			right: Box::new(right),
		}
	}

	/// Coerces the result to a boolean through ordinary double negation.
	///
	/// ```
	/// # use fg_engine as ferros_grynn;
	/// use ferros_grynn::live::LiveExpr;
	/// let truthy = LiveExpr::Literal(1.into()).truthy();
	/// assert!(matches!(truthy, LiveExpr::Not(_)));
	/// ```
	#[must_use]
	pub fn truthy(self) -> Self {
		Self::Not(Box::new(Self::Not(Box::new(self))))
	}
}

/// One projected expression with an optional checked output path.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LiveField {
	/// Owned logical expression.
	pub expression: LiveExpr,
	/// Optional nested alias, interpreted as a path.
	pub alias: Option<LivePath>,
}

impl LiveField {
	/// Builds a projected field without a renamed output path.
	pub const fn new(expression: LiveExpr) -> Self {
		Self {
			expression,
			alias: None,
		}
	}

	/// Replaces the checked nested output alias.
	#[must_use]
	pub fn with_alias(mut self, alias: LivePath) -> Self {
		self.alias = Some(alias);
		self
	}
}

/// Logical projection; output formatting does not change exact internal images.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveProjection {
	/// Complete SELECT records.
	#[default]
	Records,
	/// Ordered projected expressions.
	Fields(
		/// Ordered projected expressions.
		Vec<LiveField>,
	),
	/// SELECT VALUE result, including scalar results.
	Value(
		/// SELECT VALUE result, including scalar results.
		LiveExpr,
	),
}

/// Formatting of result changes, independent of the logical projection.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LiveOutputMode {
	/// Exact complete before/after projected results.
	#[default]
	Full,
	/// Exact changed fields of projected object results.
	Diff,
	/// Ordered add/remove/replace operations with JSON Pointer paths.
	Patch,
}

/// Independent Rust builder for a read-only reactive SELECT descriptor.
///
/// ```
/// # use fg_engine as ferros_grynn;
/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
/// use ferros_grynn::live::{LiveBinaryOperator, LiveExpr, LiveQuery, LiveSource};
/// let query = LiveQuery::new(LiveSource::Record("person:alice".parse()?))
/// 	.with_filter(
/// 		LiveExpr::field("age")?
/// 			.binary(LiveBinaryOperator::GreaterOrEqual, LiveExpr::Literal(18.into())),
/// 	)
/// 	.with_fetch(vec![ferros_grynn::live::LivePath::field("manager")?]);
/// assert_eq!(query.fetch().len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
#[allow(
	clippy::tabs_in_doc_comments,
	reason = "nightly rustfmt uses required hard tabs in Rustdoc examples"
)]
pub struct LiveQuery {
	/// Optional owned graph terminal, separate from row delivery presentation.
	pub(crate) graph_result: Option<super::LiveGraphResult>,
	/// Source identity, resolved without reparsing text.
	pub(crate) source: LiveSource,
	/// Optional logical qualifier, independent of the source identity.
	pub(crate) alias: Option<Text>,
	/// Independently scoped sources in declaration order.
	pub(crate) joins: Vec<LiveJoin>,
	/// WHERE expression evaluated against both commit images.
	pub(crate) filter: Option<LiveExpr>,
	/// Ordered bound fields expanded before grouping and projection.
	pub(crate) split: Vec<LiveExpr>,
	/// None is row-local; an empty key list means GROUP ALL.
	pub(crate) group: Option<Vec<LiveExpr>>,
	/// Ordered comparison keys controlling result window membership.
	pub(crate) order: Vec<super::LiveOrder>,
	/// Exact nonnegative integral output count, evaluated per invocation.
	pub(crate) limit: Option<LiveExpr>,
	/// Exact nonnegative integral offset, evaluated per invocation.
	pub(crate) start_offset: Option<LiveExpr>,
	/// Enforces ordinary SELECT ONLY cardinality after the complete window.
	pub(crate) only: bool,
	/// Logical SELECT projection.
	pub(crate) projection: LiveProjection,
	/// Ordered literal output paths removed before FETCH.
	pub(crate) omit: Vec<LivePath>,
	/// External record dependencies resolved by ordinary FETCH.
	pub(crate) fetch: Vec<LivePath>,
	/// Owned ordinary parameter inputs, frozen during successful registration.
	pub(crate) parameters: Object,
	/// Delivery presentation of exact result changes.
	pub(crate) output: LiveOutputMode,
}

impl LiveQuery {
	/// Builds a complete-record query with no filter, OMIT, FETCH or
	/// parameters.
	pub fn new(source: LiveSource) -> Self {
		Self {
			graph_result: None,
			source,
			alias: None,
			joins: Vec::new(),
			filter: None,
			split: Vec::new(),
			group: None,
			order: Vec::new(),
			limit: None,
			start_offset: None,
			only: false,
			projection: LiveProjection::Records,
			omit: Vec::new(),
			fetch: Vec::new(),
			parameters: Object::new(),
			output: LiveOutputMode::Full,
		}
	}

	/// Sets the source qualifier without changing result identities.
	/// Qualified paths use separate segments, such as `["p", "age"]`.
	///
	/// # Errors
	/// Returns an argument error for an empty name or a name containing NUL.
	///
	/// ```
	/// # use fg_engine as ferros_grynn;
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::live::{LiveQuery, LiveSource};
	/// let source = LiveSource::Record("person:alice".parse()?);
	/// let query = LiveQuery::new(source).with_alias("p")?;
	/// assert_eq!(query.alias().map(|name| name.as_str()), Some("p"));
	/// # Ok(())
	/// # }
	/// ```
	pub fn with_alias<IntoText: Into<Text>>(mut self, alias: IntoText) -> FerrosGrynnResult<Self> {
		let alias = alias.into();
		Self::validate_alias(alias.as_str())?;
		self.alias = Some(alias);
		Ok(self)
	}

	/// Shares source-name validation with the canonical descriptor codec.
	pub(crate) fn validate_alias(alias: &str) -> FerrosGrynnResult<()> {
		if alias.is_empty() || alias.contains('\0') {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE source alias must be nonempty and contain no NUL",
			));
		}
		Ok(())
	}

	/// Replaces JOIN declarations without copying their owned recipes.
	/// Duplicate or ambiguous qualifiers are rejected by ordinary registration.
	///
	/// ```
	/// # use fg_engine as ferros_grynn;
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::live::{LiveJoin, LiveJoinKind, LiveQuery, LiveSource};
	/// let source = LiveSource::Record("person:alice".parse()?);
	/// let right = LiveSource::Record("person:bob".parse()?);
	/// let join = LiveJoin::new(right, "r", LiveJoinKind::Cross, None)?;
	/// let query = LiveQuery::new(source).with_joins(vec![join]);
	/// assert_eq!(query.joins().len(), 1);
	/// # Ok(())
	/// # }
	/// ```
	pub fn with_joins(mut self, joins: Vec<LiveJoin>) -> Self {
		self.joins = joins;
		self
	}

	/// Replaces the WHERE expression, retaining the source identity.
	pub fn with_filter(mut self, expression: LiveExpr) -> Self {
		self.filter = Some(expression);
		self
	}

	/// Replaces sequential SPLIT fields. Each operand must bind to a source
	/// column or nested field; ordinary analysis/evaluation rejects unbound
	/// operands and non-array/non-set values. NONE/NULL produce no rows.
	/// Duplicate array members retain distinct Composite identity positions.
	/// `LiveExpr::field` keeps literal dots; nested paths use `LivePath`
	/// segments. An empty replacement clears SPLIT.
	///
	/// ```
	/// # use fg_engine as ferros_grynn;
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::live::{LiveExpr, LiveQuery, LiveSource};
	/// let source = LiveSource::Record("person:alice".parse()?);
	/// let query = LiveQuery::new(source).with_split(vec![LiveExpr::field("tags")?]);
	/// assert_eq!(query.split().len(), 1);
	/// assert!(query.with_split(Vec::new()).split().is_empty());
	/// # Ok(())
	/// # }
	/// ```
	pub fn with_split(mut self, expressions: Vec<LiveExpr>) -> Self {
		self.split = expressions;
		self
	}

	/// Requires at most one result after ORDER, START and LIMIT. An empty
	/// domain emits no synthetic NONE identity; the row/group identity is
	/// unchanged. Registration rejects multiple rows. An active violation
	/// suspends the subscription with a fault while source DML commits.
	///
	/// ```
	/// # use fg_engine as ferros_grynn;
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::live::{LiveQuery, LiveSource};
	/// let query = LiveQuery::new(LiveSource::Record("person:alice".parse()?)).with_only(true);
	/// assert!(query.only());
	/// # Ok(())
	/// # }
	/// ```
	pub fn with_only(mut self, only: bool) -> Self {
		self.only = only;
		self
	}

	/// Groups the complete source domain by ordered exact key values.
	/// Result identities contain these keys, independently of projection.
	///
	/// # Errors
	/// Returns an argument error for an empty list; use
	/// [`Self::with_group_all`]. Expression types and effects are checked
	/// during registration.
	///
	/// ```
	/// # use fg_engine as ferros_grynn;
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::live::{LiveExpr, LiveProjection, LiveQuery, LiveSource};
	/// let query = LiveQuery::new(LiveSource::Record("person:alice".parse()?));
	/// let projection = LiveProjection::Value(LiveExpr::field("age")?);
	/// let query = query.with_projection(projection);
	/// let query = query.with_group_by(vec![LiveExpr::field("age")?])?;
	/// assert_eq!(query.group().map(<[_]>::len), Some(1));
	/// # Ok(())
	/// # }
	/// ```
	pub fn with_group_by(mut self, keys: Vec<LiveExpr>) -> FerrosGrynnResult<Self> {
		if keys.is_empty() {
			return Err(FerrosGrynnError::invalid_argument("LIVE GROUP BY requires a key"));
		}
		self.group = Some(keys);
		Ok(self)
	}

	/// Selects one global group, including an empty source domain.
	pub fn with_group_all(mut self) -> Self {
		self.group = Some(Vec::new());
		self
	}

	/// Replaces deterministic ORDER keys. ORDER controls window membership;
	/// deliveries retain canonical identity order. Reordering the same
	/// identities with unchanged payloads does not create result changes.
	///
	/// ```
	/// # use fg_engine as ferros_grynn;
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::live::{LiveExpr, LiveOrder, LiveOrderDirection, LiveQuery, LiveSource};
	/// let order = LiveOrder::by_name("age")?.with_direction(LiveOrderDirection::Descending);
	/// let query = LiveQuery::new(LiveSource::Record("person:alice".parse()?))
	/// 	.with_order(vec![order])
	/// 	.with_limit(LiveExpr::Literal(10_i64.into()))
	/// 	.with_start_offset(LiveExpr::Literal(2_i64.into()));
	/// assert_eq!(query.order().len(), 1);
	/// assert!(query.limit().is_some() && query.start_offset().is_some());
	/// # Ok(())
	/// # }
	/// ```
	#[allow(
		clippy::tabs_in_doc_comments,
		reason = "nightly rustfmt uses required hard tabs in Rustdoc examples"
	)]
	pub fn with_order(mut self, order: Vec<super::LiveOrder>) -> Self {
		self.order = order;
		self
	}

	/// Sets the output count. Registration/evaluation require an exact,
	/// nonnegative integral Number representable as a platform ordinal.
	/// Ordinary SELECT static type checks also apply.
	/// Ordinary parameters freeze; reactive bindings update this expression.
	pub fn with_limit(mut self, expression: LiveExpr) -> Self {
		self.limit = Some(expression);
		self
	}

	/// Sets the number of output rows skipped before LIMIT. Uses the same
	/// exact validation and parameter rules as [`Self::with_limit`].
	pub fn with_start_offset(mut self, expression: LiveExpr) -> Self {
		self.start_offset = Some(expression);
		self
	}

	/// Replaces logical projection without changing dependency semantics.
	pub fn with_projection(mut self, projection: LiveProjection) -> Self {
		self.projection = projection;
		self
	}

	/// Adds a terminal graph projection using ordinary SELECT result aliases.
	#[must_use = "the updated LIVE query must be submitted"]
	pub fn with_graph_result(mut self, result: super::LiveGraphResult) -> Self {
		self.graph_result = Some(result);
		self
	}

	/// Borrows the logical graph recipe without materializing its sources.
	pub const fn graph_result(&self) -> Option<&super::LiveGraphResult> {
		self.graph_result.as_ref()
	}

	/// Removes output paths after projection and before FETCH.
	/// Result identities are retained even when `id` is omitted.
	/// Missing paths and paths through non-object values leave output
	/// unchanged.
	///
	/// ```
	/// # use fg_engine as ferros_grynn;
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::live::{LivePath, LiveQuery, LiveSource};
	/// let query = LiveQuery::new(LiveSource::Record("person:alice".parse()?));
	/// let paths = vec![LivePath::field("id")?, LivePath::field("secret")?];
	/// let query = query.with_omit(paths);
	/// assert_eq!(query.omit().len(), 2);
	/// # Ok(())
	/// # }
	/// ```
	pub fn with_omit(mut self, paths: Vec<LivePath>) -> Self {
		self.omit = paths;
		self
	}

	/// Selects external FETCH paths.
	pub fn with_fetch(mut self, paths: Vec<LivePath>) -> Self {
		self.fetch = paths;
		self
	}

	/// Supplies owned ordinary parameters, captured by value at registration.
	pub fn with_parameters(mut self, parameters: Object) -> Self {
		self.parameters = parameters;
		self
	}

	/// Changes delivery formatting, retaining exact internal before/after.
	pub fn with_output(mut self, mode: LiveOutputMode) -> Self {
		self.output = mode;
		self
	}

	/// Returns the logical source.
	pub const fn source(&self) -> &LiveSource {
		&self.source
	}

	/// Borrows the explicit source qualifier.
	pub const fn alias(&self) -> Option<&Text> {
		self.alias.as_ref()
	}

	/// Borrows JOIN declarations in logical source order.
	pub fn joins(&self) -> &[LiveJoin] {
		&self.joins
	}

	/// Visits every independently scoped source without allocating a
	/// collection.
	pub(crate) fn sources(&self) -> impl Iterator<Item = &LiveSource> {
		std::iter::once(&self.source).chain(self.joins.iter().map(|join| &join.source))
	}

	/// Visits sources in place while preserving the other query fields.
	pub(crate) fn sources_mut(&mut self) -> impl Iterator<Item = &mut LiveSource> {
		std::iter::once(&mut self.source).chain(self.joins.iter_mut().map(|join| &mut join.source))
	}

	/// Returns the WHERE expression.
	pub const fn filter(&self) -> Option<&LiveExpr> {
		self.filter.as_ref()
	}

	/// Returns ordered grouping keys; `Some(&[])` denotes GROUP ALL.
	pub fn group(&self) -> Option<&[LiveExpr]> {
		self.group.as_deref()
	}

	/// Borrows the ordered comparison keys controlling window membership.
	pub fn order(&self) -> &[super::LiveOrder] {
		&self.order
	}

	/// Borrows the optional exact output count expression.
	pub const fn limit(&self) -> Option<&LiveExpr> {
		self.limit.as_ref()
	}

	/// Borrows the optional exact output offset expression.
	pub const fn start_offset(&self) -> Option<&LiveExpr> {
		self.start_offset.as_ref()
	}

	/// Returns the logical projection.
	pub const fn projection(&self) -> &LiveProjection {
		&self.projection
	}

	/// Borrows sequential SPLIT operands in their declaration order.
	pub fn split(&self) -> &[LiveExpr] {
		&self.split
	}

	/// Returns whether the complete result must contain at most one row.
	pub const fn only(&self) -> bool {
		self.only
	}

	/// Returns ordered literal output paths removed before FETCH.
	pub fn omit(&self) -> &[LivePath] {
		&self.omit
	}

	/// Returns the external FETCH paths.
	pub fn fetch(&self) -> &[LivePath] {
		&self.fetch
	}

	/// Returns the owned frozen parameter inputs.
	pub const fn parameters(&self) -> &Object {
		&self.parameters
	}

	/// Returns delivery formatting.
	pub const fn output(&self) -> LiveOutputMode {
		self.output
	}
}
