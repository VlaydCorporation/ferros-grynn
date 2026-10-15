#![allow(
	clippy::wildcard_imports,
	reason = "schema statements share the parser helpers owned by their parent module"
)]

use super::*;

/// Conflict behavior shared by core `DEFINE` statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DefinitionMode {
	/// Use ordinary definition behavior when a name already exists.
	Normal,
	/// Keep an existing definition with the same name.
	IfNotExists,
	/// Replace the existing named definition.
	Overwrite,
}

/// Parsed atom kind before semantic lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassKindName {
	/// Declare a class containing node atoms.
	Node,
	/// Declare a class containing edge atoms.
	Edge,
}

/// Parsed class schema-validation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassSchemaModeName {
	/// Permit properties outside the explicitly declared field set.
	Schemaless,
	/// Apply the declared class schema to accepted properties.
	Schemafull,
}

/// Core `DEFINE GRAPH` syntax.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DefineGraphStatement {
	/// Execution or declaration policy selected by this clause.
	pub mode: DefinitionMode,
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Requested graph family and structural validators.
	pub contract: GraphContract,
	/// Optional expression selecting the root of the produced graph.
	pub root: Option<Expr>,
	/// Optional expression supplying initial graph content.
	pub content: Option<Expr>,
	/// Optional durable capture policy.
	pub changefeed: Option<ChangefeedClause>,
}

/// Core `DEFINE CLASS` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DefineClassStatement {
	/// Execution or declaration policy selected by this clause.
	pub mode: DefinitionMode,
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Selected operation or representation discriminator.
	pub kind: ClassKindName,
	/// Class property-validation policy selected by the schema declaration.
	pub schema_mode: ClassSchemaModeName,
	/// Optional durable capture policy.
	pub changefeed: Option<ChangefeedClause>,
}

/// Core `DEFINE FIELD` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DefineFieldStatement {
	/// Execution or declaration policy selected by this clause.
	pub mode: DefinitionMode,
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Class selected by the schema declaration, query target or fixture.
	pub class: Identifier,
	/// Declared value type retained for semantic validation.
	pub value_type: Option<TypeName>,
}

/// Vector element type written in an ANN index definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VectorElementName {
	/// 64-bit floating-point vector elements.
	F64,
	/// 32-bit floating-point vector elements.
	F32,
	/// 16-bit floating-point vector elements.
	F16,
	/// 64-bit signed integer vector elements.
	I64,
	/// 32-bit signed integer vector elements.
	I32,
	/// 16-bit signed integer vector elements.
	I16,
	/// 8-bit signed integer vector elements.
	I8,
	/// 8-bit unsigned integer vector elements.
	U8,
}

/// Distance written in a durable ANN index definition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VectorDistanceName {
	/// Euclidean distance between numeric vectors.
	Euclidean,
	/// Cosine distance between numeric vectors.
	Cosine,
	/// L1 distance between numeric vectors.
	Manhattan,
	/// Minkowski distance with an explicit exponent.
	Minkowski {
		/// Floating-point bit representation of the Minkowski exponent.
		exponent_bits: u64,
	},
	/// Inner-product distance for vector search.
	InnerProduct,
	/// Cosine-distance form for normalized vectors.
	CosineNormalized,
	/// User-defined function supplying vector distance.
	Function(
		/// User-defined function supplying vector distance.
		FunctionSignatureName,
	),
}

/// Class-index kind and kind-specific syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BasicIndexDefinition {
	/// Ordinary scalar index definition.
	Standard(
		/// Ordinary scalar index definition.
		Vec<Identifier>,
	),
	/// Scalar index enforcing key uniqueness.
	Unique(
		/// Scalar index enforcing key uniqueness.
		Vec<Identifier>,
	),
	/// Index maintaining aggregate counts.
	Count,
	/// Spatial index definition for geometry values.
	Geometry {
		/// Property name addressed by this expression or schema declaration.
		field: Identifier,
		/// Optional geometry-kind restriction of the spatial index.
		geometry_type: Option<Identifier>,
	},
	/// Analyzed full-text index definition.
	Fulltext {
		/// Property name addressed by this expression or schema declaration.
		field: Identifier,
		/// Named analyzer used to tokenize indexed full-text values.
		analyzer: Identifier,
		/// Floating-point bit representation of the full-text k1 parameter.
		k1_bits: u64,
		/// Floating-point bit representation of the full-text b parameter.
		b_bits: u64,
		/// Whether the index retains data required for highlighted matches.
		highlights: bool,
	},
	/// HNSW approximate nearest-neighbor index definition.
	Hnsw {
		/// Property name addressed by this expression or schema declaration.
		field: Identifier,
		/// Number of scalar coordinates in each indexed vector.
		dimension: u16,
		/// Scalar element type selected for indexed vectors.
		element: VectorElementName,
		/// Distance contract selected for vector comparisons.
		distance: VectorDistanceName,
		/// Candidate-list bound used while constructing the HNSW index.
		ef_construction: u16,
		/// Configured connectivity parameter of the HNSW index.
		m: u16,
	},
	/// DiskANN approximate nearest-neighbor index definition.
	DiskAnn {
		/// Property name addressed by this expression or schema declaration.
		field: Identifier,
		/// Number of scalar coordinates in each indexed vector.
		dimension: u16,
		/// Scalar element type selected for indexed vectors.
		element: VectorElementName,
		/// Distance contract selected for vector comparisons.
		distance: VectorDistanceName,
		/// Maximum graph degree configured for the DiskANN index.
		degree: u16,
		/// Candidate-list size configured for the DiskANN build.
		build_list_size: u16,
		/// Floating-point bit representation of the DiskANN pruning parameter.
		alpha_bits: u32,
		/// Whether the vector-index definition requests hashed vectors.
		hashed_vector: bool,
	},
}

/// Explicit logical scope of a class index.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IndexScopeName {
	/// Index values from base class properties.
	Base,
	/// Index effective values after applying the graph scope.
	Effective {
		/// Graph materialization or physical graph files retained by the
		/// operation.
		graph: Identifier,
	},
}

/// Core `DEFINE INDEX` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DefineIndexStatement {
	/// Execution or declaration policy selected by this clause.
	pub mode: DefinitionMode,
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Class selected by the schema declaration, query target or fixture.
	pub class: Identifier,
	/// Selected scalar, search or vector index specification.
	pub definition: BasicIndexDefinition,
	/// Base or graph-effective scope of the index.
	pub scope: IndexScopeName,
	/// Whether the operation requests background concurrent execution.
	pub concurrently: bool,
	/// Whether initial index population is deferred.
	pub deferred: bool,
}

/// Parsed graph-owned index family and optional radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphIndexFamilyName {
	/// Shortest-path or bounded pattern acceleration.
	Path,
	/// Unbounded reachability of the selected projection.
	Reachability,
	/// Maximum neighbourhood radius.
	Neighbourhood(
		/// Maximum neighbourhood radius.
		u32,
	),
	/// Complete ATOM incidence cache with NODE exposed as a view.
	Tensor,
}

/// Graph-owned `DEFINE INDEX` syntax before catalog resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DefineGraphIndexStatement {
	/// Conflict behavior.
	pub mode: DefinitionMode,
	/// Graph-local index name.
	pub name: Identifier,
	/// Named graph owner.
	pub graph: Identifier,
	/// Kind and optional maximum radius.
	pub family: GraphIndexFamilyName,
	/// Optional algorithm and numeric parameter.
	pub algorithm: Option<(Identifier, Option<u32>)>,
	/// Tensor physical representation: AUTO, CSR or DENSE.
	pub cache_format: Option<Identifier>,
	/// Qualified tensor scope: LOCAL or ROOTED.
	pub cache_scope: Option<Identifier>,
	/// Optional NODE/ATOM projection; NODE is the default.
	pub projection: Option<Identifier>,
	/// Optional OUT/IN/BOTH direction; OUT is the default.
	pub direction: Option<Identifier>,
	/// Exact connector label filter.
	pub edge_labels: Vec<Identifier>,
	/// Built-in weighted path cost.
	pub weighted: bool,
	/// Versioned custom path cost name.
	pub cost: Option<FunctionSignatureName>,
	/// Parameter-free pattern source strings.
	pub patterns: Vec<Box<str>>,
	/// Pages and metadata budget.
	pub max_bytes: Option<u64>,
	/// Optional durable comment.
	pub comment: Option<Box<str>>,
	/// Background initial build.
	pub concurrently: bool,
	/// Deferred maintenance scheduling.
	pub deferred: bool,
}

/// Durable database-parameter definition.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DefineParameterStatement {
	/// Execution or declaration policy selected by this clause.
	pub mode: DefinitionMode,
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Payload retained for the operation or its expected result.
	pub value: Expr,
	/// User-supplied description retained in the schema declaration.
	pub comment: Option<Box<str>>,
}

/// Built-in tokenizer name in a durable analyzer definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnalyzerTokenizerName {
	/// Split text on whitespace.
	Blank,
	/// Split text at camel-case boundaries.
	Camel,
	/// Tokenize according to character-class boundaries.
	Class,
	/// Split text using punctuation boundaries.
	Punct,
}

/// Ordered filter in a durable analyzer definition.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AnalyzerFilterName {
	/// Apply canonical Unicode composition.
	Nfc,
	/// Apply compatibility Unicode composition.
	Nfkc,
	/// Apply the analyzer ASCII-normalization filter.
	Ascii,
	/// Convert tokens to lowercase.
	Lowercase,
	/// Convert tokens to uppercase.
	Uppercase,
	/// Produce edge n-grams within the configured size range.
	EdgeNgram {
		/// Inclusive lower bound accepted by the pattern or filter.
		min: u16,
		/// Upper bound accepted by the pattern or filter, when specified.
		max: u16,
	},
	/// Produce n-grams within the configured size range.
	Ngram {
		/// Inclusive lower bound accepted by the pattern or filter.
		min: u16,
		/// Upper bound accepted by the pattern or filter, when specified.
		max: u16,
	},
	/// Apply the selected Snowball language stemmer.
	Snowball(
		/// Apply the selected Snowball language stemmer.
		Identifier,
	),
	/// Apply a configured token-mapping resource.
	Mapper(
		/// Apply a configured token-mapping resource.
		Expr,
	),
}

/// Durable text-analyzer definition.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DefineAnalyzerStatement {
	/// Execution or declaration policy selected by this clause.
	pub mode: DefinitionMode,
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Optional named function selected by the analyzer declaration or
	/// alteration.
	pub function: Option<FunctionSignatureName>,
	/// Tokenizer sequence selected by the analyzer declaration.
	pub tokenizers: Vec<AnalyzerTokenizerName>,
	/// Filters retained in the order required by the selected declaration.
	pub filters: Vec<AnalyzerFilterName>,
	/// User-supplied description retained in the schema declaration.
	pub comment: Option<Box<str>>,
}

/// Parsed effect annotation of a named function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FunctionEffectName {
	/// Function declaration promises pure evaluation.
	Pure,
	/// Function declaration promises stability within its supported evaluation
	/// scope.
	Stable,
	/// Function declaration permits observable side effects.
	Impure,
}

/// One named argument in a function definition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FunctionParameterDefinition {
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Declared value type retained for semantic validation.
	pub value_type: TypeName,
}

/// Qualified function name with an optional exact overload signature.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FunctionSignatureName {
	/// Namespace qualifying the function name.
	pub namespace: Identifier,
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Declared parameters or bound values supplied to this operation.
	pub parameters: Option<Vec<TypeName>>,
}

/// Durable named-function definition.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DefineFunctionStatement {
	/// Execution or declaration policy selected by this clause.
	pub mode: DefinitionMode,
	/// Declared purity or observable-effect category of the function.
	pub effect: FunctionEffectName,
	/// Whether the function is declared usable as a path-cost function.
	pub cost: bool,
	/// Namespace qualifying the function name.
	pub namespace: Identifier,
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Declared parameters or bound values supplied to this operation.
	pub parameters: Vec<FunctionParameterDefinition>,
	/// Optional declared result type of the function.
	pub result_type: Option<TypeName>,
	/// Maximum nested depth accepted by the operation or declaration.
	pub max_depth: Option<u8>,
	/// Statement block executed when the declared function is called.
	pub body: Block,
	/// User-supplied description retained in the schema declaration.
	pub comment: Option<Box<str>>,
}

/// Optional durable comment update for `ALTER PARAM`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParameterCommentChange {
	/// Leave the existing parameter comment unchanged.
	Keep,
	/// Replace the parameter comment with supplied text.
	Set(
		/// Replace the parameter comment with supplied text.
		Box<str>,
	),
	/// Remove the existing parameter comment.
	Drop,
}

/// Core schema object accepted by `REMOVE`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoveTarget {
	/// Removes one event definition without cancelling accepted jobs.
	Event {
		/// Suppresses missing-definition and missing-owner diagnostics.
		if_exists: bool,
		/// Name within the stable source owner.
		name: Identifier,
		/// Explicit class or graph namespace.
		owner: EventOwnerName,
	},
	/// Removes a reactive binding after checking its dependents.
	LiveLet(
		/// Removes a reactive binding after checking its dependents.
		Identifier,
	),
	/// Removes a graph-owned user definition.
	GraphIndex {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
		/// Graph materialization or physical graph files retained by the
		/// operation.
		graph: Identifier,
	},
	/// Analyzer definition selected to be removed.
	Analyzer {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
	},
	/// Function definition and signature selected to be removed.
	Function {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Qualified function name and optional parameter-type signature.
		signature: FunctionSignatureName,
	},
	/// Stored parameter definition selected to be removed.
	Parameter {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
	},
	/// Named graph selected to be removed.
	Graph {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
	},
	/// Class definition selected to be removed.
	Class {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
	},
	/// Class field declaration selected to be removed.
	Field {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
		/// Class selected by the schema declaration, query target or fixture.
		class: Identifier,
	},
	/// Named index definition selected to be removed.
	Index {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
		/// Class selected by the schema declaration, query target or fixture.
		class: Identifier,
	},
}

/// Core `REMOVE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemoveStatement {
	/// Target endpoint or schema object selected by the operation.
	pub target: RemoveTarget,
}

/// Supported `ALTER` operation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AlterTarget {
	/// Applies ordered changes to a source-owned event definition.
	Event {
		/// Suppresses missing-definition and missing-owner diagnostics.
		if_exists: bool,
		/// Name within the stable source owner.
		name: Identifier,
		/// Explicit class or graph namespace.
		owner: EventOwnerName,
		/// Source and policy changes in declaration order.
		actions: Vec<EventAlterAction>,
	},
	/// Replaces a reactive expression while retaining the binding identity.
	LiveLet {
		/// Existing reactive identity, independent of its current value.
		name: Identifier,
		/// Replacement expression; a literal discards previous dependencies.
		value: Expr,
	},
	/// Replaces a mutable lexical LET value without changing LIVE captures.
	Let {
		/// Existing nearest lexical binding, excluding execute parameters.
		name: Identifier,
		/// Complete replacement value, evaluated by the normal pipeline.
		value: Expr,
	},
	/// Configures or stops capture for one existing class or graph.
	Changefeed {
		/// Whether the owner is GRAPH rather than CLASS.
		graph: bool,
		/// Suppresses missing-owner errors.
		if_exists: bool,
		/// Catalog owner name.
		name: Identifier,
		/// None represents DROP CHANGEFEED.
		configuration: Option<ChangefeedClause>,
	},
	/// Analyzer definition selected to be altered.
	Analyzer {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
		/// Optional named function selected by the analyzer declaration or
		/// alteration.
		function: Option<Option<FunctionSignatureName>>,
		/// Tokenizer sequence selected by the analyzer declaration.
		tokenizers: Option<Vec<AnalyzerTokenizerName>>,
		/// Filters retained in the order required by the selected declaration.
		filters: Option<Vec<AnalyzerFilterName>>,
		/// User-supplied description retained in the schema declaration.
		comment: ParameterCommentChange,
	},
	/// Function definition and signature selected to be altered.
	Function {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Qualified function name and optional parameter-type signature.
		signature: FunctionSignatureName,
		/// Declared purity or observable-effect category of the function.
		effect: Option<FunctionEffectName>,
		/// Whether the function is declared usable as a path-cost function.
		cost: Option<bool>,
		/// Maximum nested depth accepted by the operation or declaration.
		max_depth: Option<Option<u8>>,
		/// Optional replacement function body supplied by an ALTER FUNCTION
		/// statement.
		body: Option<Block>,
		/// User-supplied description retained in the schema declaration.
		comment: ParameterCommentChange,
	},
	/// Stored parameter definition selected to be altered.
	Parameter {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
		/// Payload retained for the operation or its expected result.
		value: Option<Expr>,
		/// User-supplied description retained in the schema declaration.
		comment: ParameterCommentChange,
	},
	/// Class definition selected to be altered.
	Class {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
		/// Class property-validation policy selected by the schema declaration.
		schema_mode: ClassSchemaModeName,
	},
	/// Class field declaration selected to be altered.
	Field {
		/// Whether an absent target is accepted instead of reported as an
		/// error.
		if_exists: bool,
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
		/// Class selected by the schema declaration, query target or fixture.
		class: Identifier,
		/// Declared value type retained for semantic validation.
		value_type: Option<TypeName>,
	},
	/// Index prepared for removal selected to be altered.
	IndexPrepareRemove {
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
		/// Class selected by the schema declaration, query target or fixture.
		class: Identifier,
	},
}

/// Core `ALTER` statement.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AlterStatement {
	/// Target endpoint or schema object selected by the operation.
	pub target: AlterTarget,
}

/// Metadata scope accepted by `INFO FOR`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InfoTarget {
	/// Reads a reactive identity and its definition/value revisions.
	LiveLet(
		/// Reads a reactive identity and its definition/value revisions.
		Identifier,
	),
	/// Runtime UUID parameter containing a retained LIVE query incarnation.
	Live(
		/// Runtime UUID parameter containing a retained LIVE query incarnation.
		Identifier,
	),
	/// Runtime parameter containing the retained feed UUID.
	Changefeed(
		/// Runtime parameter containing the retained feed UUID.
		Identifier,
	),
	/// Reads a graph-owned user or engine index definition.
	GraphIndex {
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
		/// Graph materialization or physical graph files retained by the
		/// operation.
		graph: Identifier,
	},
	/// Database metadata selected to be inspected.
	Database,
	/// Analyzer definition selected to be inspected.
	Analyzer(
		/// Analyzer definition selected to be inspected.
		Identifier,
	),
	/// Function definition and signature selected to be inspected.
	Function(
		/// Function definition and signature selected to be inspected.
		FunctionSignatureName,
	),
	/// Stored parameter definition selected to be inspected.
	Parameter(
		/// Stored parameter definition selected to be inspected.
		Identifier,
	),
	/// One maintenance job selected to be inspected.
	Job(
		/// One maintenance job selected to be inspected.
		Identifier,
	),
	/// Maintenance job collection selected to be inspected.
	Jobs,
	/// Class definition selected to be inspected.
	Class(
		/// Class definition selected to be inspected.
		Identifier,
	),
	/// Reads graph schema metadata, including source-owned events.
	Graph(
		/// Reads graph schema metadata, including source-owned events.
		Identifier,
	),
	/// Named index definition selected to be inspected.
	Index {
		/// Declared name used when resolving or displaying this element.
		name: Identifier,
		/// Class selected by the schema declaration, query target or fixture.
		class: Identifier,
	},
}

/// Core `INFO FOR` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct InfoStatement {
	/// Target endpoint or schema object selected by the operation.
	pub target: InfoTarget,
}

impl Parser<'_> {
	/// Dispatches the DEFINE family before parsing each definition's own
	/// clauses.
	pub(in crate::syn) async fn definition_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<StatementKind> {
		Ok(if self.consume_keyword("EVENT").is_some() {
			StatementKind::DefineEvent(
				context.run(|context| self.define_event_statement(context)).await?,
			)
		} else if self.at_function_definition() {
			StatementKind::DefineFunction(
				context.run(|context| self.define_function_statement(context)).await?,
			)
		} else if self.consume_keyword(keywords::ANALYZER).is_some() {
			StatementKind::DefineAnalyzer(
				context.run(|context| self.define_analyzer_statement(context)).await?,
			)
		} else if self.consume_keyword(keywords::PARAM).is_some() {
			StatementKind::DefineParameter(
				context.run(|context| self.define_parameter_statement(context)).await?,
			)
		} else if self.consume_keyword(keywords::GRAPH).is_some() {
			StatementKind::DefineGraph(
				context.run(|context| self.define_graph_statement(context)).await?,
			)
		} else if self.consume_keyword(keywords::CLASS).is_some() {
			StatementKind::DefineClass(self.define_class_statement()?)
		} else if self.consume_keyword(keywords::FIELD).is_some() {
			StatementKind::DefineField(self.define_field_statement()?)
		} else if self.consume_keyword(keywords::INDEX).is_some() {
			self.define_index_statement()?
		} else {
			let feature = self.current_word().unwrap_or("DEFINE target");
			return Err(self.unsupported(feature, self.current().span));
		})
	}

	/// Parses the define function statement syntax.
	pub(in crate::syn) async fn define_function_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DefineFunctionStatement> {
		let effect = self.function_effect().unwrap_or(FunctionEffectName::Impure);
		let cost = self.consume_keyword(keywords::COST).is_some();
		self.expect_keyword(keywords::FUNCTION, "Expected `FUNCTION` after function modifiers.")?;
		let mode = self.definition_mode()?;
		let (namespace, name) = self.qualified_function_name()?;
		let open = self.expect_symbol(Symbol::LeftParen, "Expected `(` after function name.")?;
		let mut parameters = Vec::new();
		if !self.at_symbol(Symbol::RightParen) {
			loop {
				let name = match self.current().kind {
					TokenKind::Parameter(_) => self.parameter()?,
					_ => self.identifier()?,
				};
				self.expect_symbol(Symbol::Colon, "Expected `:` after function parameter.")?;
				parameters.push(FunctionParameterDefinition {
					name,
					value_type: self.type_name()?,
				});
				if self.consume_symbol(Symbol::Comma).is_none() {
					break;
				}
			}
		}
		self.close(Symbol::RightParen, open.span, "`)`")?;
		let result_type = if self.consume_symbol(Symbol::ArrowRight).is_some() {
			Some(self.type_name()?)
		} else {
			None
		};
		let max_depth = if self.consume_keyword(keywords::MAXDEPTH).is_some() {
			Some(self.function_max_depth()?)
		} else {
			None
		};
		let body = context.run(|context| self.block(context)).await?;
		let comment = if self.consume_keyword(keywords::COMMENT).is_some() {
			Some(self.string_value("Expected a string after `COMMENT`.")?)
		} else {
			None
		};
		self.expect_statement_end("DEFINE FUNCTION")?;
		Ok(DefineFunctionStatement {
			mode,
			effect,
			cost,
			namespace,
			name,
			parameters,
			result_type,
			max_depth,
			body,
			comment,
		})
	}

	/// Parses the define parameter statement syntax.
	pub(in crate::syn) async fn define_parameter_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DefineParameterStatement> {
		let mode = self.definition_mode()?;
		let name = self.parameter()?;
		self.expect_keyword(keywords::VALUE, "Expected `VALUE` after parameter name.")?;
		let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		let comment = if self.consume_keyword(keywords::COMMENT).is_some() {
			Some(self.string_value("Expected a string after `COMMENT`.")?)
		} else {
			None
		};
		self.expect_statement_end("DEFINE PARAM")?;
		Ok(DefineParameterStatement {
			mode,
			name,
			value,
			comment,
		})
	}

	/// Parses the define analyzer statement syntax.
	pub(in crate::syn) async fn define_analyzer_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DefineAnalyzerStatement> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		let mut function = None;
		let mut tokenizers = None;
		let mut filters = None;
		let mut comment = None;
		while !self.at_statement_end() {
			if self.consume_keyword(keywords::FUNCTION).is_some() {
				if function.is_some() {
					return Err(self.syntax(
						"DEFINE ANALYZER FUNCTION may appear only once.",
						self.previous_span(),
					));
				}
				function = Some(self.analyzer_function_name()?);
			} else if self.consume_keyword(keywords::TOKENIZERS).is_some() {
				if tokenizers.is_some() {
					return Err(self.syntax(
						"DEFINE ANALYZER TOKENIZERS may appear only once.",
						self.previous_span(),
					));
				}
				tokenizers = Some(self.analyzer_tokenizers()?);
			} else if self.consume_keyword(keywords::FILTERS).is_some() {
				if filters.is_some() {
					return Err(self.syntax(
						"DEFINE ANALYZER FILTERS may appear only once.",
						self.previous_span(),
					));
				}
				filters = Some(context.run(|context| self.analyzer_filters(context)).await?);
			} else if self.consume_keyword(keywords::COMMENT).is_some() {
				if comment.is_some() {
					return Err(self.syntax(
						"DEFINE ANALYZER COMMENT may appear only once.",
						self.previous_span(),
					));
				}
				comment = Some(self.string_value("Expected a string after `COMMENT`.")?);
			} else {
				return Err(self.syntax("Unexpected DEFINE ANALYZER clause.", self.current().span));
			}
		}
		if function.is_some() && tokenizers.as_ref().is_some_and(|values| !values.is_empty()) {
			return Err(
				self.syntax("ANALYZER FUNCTION and TOKENIZERS are mutually exclusive.", name.span)
			);
		}
		Ok(DefineAnalyzerStatement {
			mode,
			name,
			function,
			tokenizers: tokenizers.unwrap_or_else(Vec::new),
			filters: filters.unwrap_or_else(Vec::new),
			comment,
		})
	}

	/// Parses the analyzer function name syntax.
	fn analyzer_function_name(&mut self) -> FerrosGrynnResult<FunctionSignatureName> {
		let (namespace, name) = self.qualified_function_name()?;
		Ok(FunctionSignatureName {
			namespace,
			name,
			parameters: None,
		})
	}

	/// Parses the analyzer tokenizers syntax.
	fn analyzer_tokenizers(&mut self) -> FerrosGrynnResult<Vec<AnalyzerTokenizerName>> {
		let mut tokenizers = Vec::new();
		loop {
			let token = self.identifier()?;
			let tokenizer = match token.value.to_ascii_lowercase().as_str() {
				"blank" => AnalyzerTokenizerName::Blank,
				"camel" => AnalyzerTokenizerName::Camel,
				"class" => AnalyzerTokenizerName::Class,
				"punct" => AnalyzerTokenizerName::Punct,
				_ => return Err(self.syntax("Unknown analyzer tokenizer.", token.span)),
			};
			if tokenizers.contains(&tokenizer) {
				return Err(self.syntax("Analyzer tokenizer is repeated.", token.span));
			}
			tokenizers.push(tokenizer);
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		Ok(tokenizers)
	}

	/// Parses the analyzer filters syntax.
	async fn analyzer_filters(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Vec<AnalyzerFilterName>> {
		let mut filters = Vec::new();
		loop {
			let name = self.identifier()?;
			let lower = name.value.to_ascii_lowercase();
			let filter = match lower.as_str() {
				"nfc" => AnalyzerFilterName::Nfc,
				"nfkc" => AnalyzerFilterName::Nfkc,
				"ascii" => AnalyzerFilterName::Ascii,
				"lowercase" => AnalyzerFilterName::Lowercase,
				"uppercase" => AnalyzerFilterName::Uppercase,
				"edgengram" | "ngram" => {
					let open = self.expect_symbol(
						Symbol::LeftParen,
						"Expected `(` after analyzer n-gram filter.",
					)?;
					let min = self.analyzer_u16("n-gram minimum")?;
					self.expect_symbol(Symbol::Comma, "Expected `,` between n-gram bounds.")?;
					let max = self.analyzer_u16("n-gram maximum")?;
					self.close(Symbol::RightParen, open.span, "`)`")?;
					if lower == "edgengram" {
						AnalyzerFilterName::EdgeNgram {
							min,
							max,
						}
					} else {
						AnalyzerFilterName::Ngram {
							min,
							max,
						}
					}
				}
				"snowball" => {
					let open =
						self.expect_symbol(Symbol::LeftParen, "Expected `(` after `snowball`.")?;
					let language = self.identifier()?;
					self.close(Symbol::RightParen, open.span, "`)`")?;
					AnalyzerFilterName::Snowball(language)
				}
				"mapper" => {
					let open =
						self.expect_symbol(Symbol::LeftParen, "Expected `(` after `mapper`.")?;
					let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
					self.close(Symbol::RightParen, open.span, "`)`")?;
					AnalyzerFilterName::Mapper(value)
				}
				_ => return Err(self.syntax("Unknown analyzer filter.", name.span)),
			};
			filters.push(filter);
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		Ok(filters)
	}

	/// Parses the analyzer u16 syntax.
	fn analyzer_u16(&mut self, field: &str) -> FerrosGrynnResult<u16> {
		let token = self.tokens.next();
		let TokenKind::Number(value) = token.kind else {
			return Err(self.syntax(&format!("Expected an unsigned {field}."), token.span));
		};
		if value.contains(['.', 'e', 'E', '+', '-']) {
			return Err(self.syntax(&format!("Expected an unsigned {field}."), token.span));
		}
		value.replace('_', "").parse::<u16>().map_err(|_error| {
			self.syntax(&format!("Analyzer {field} is out of range."), token.span)
		})
	}

	/// Parses the define graph statement syntax.
	pub(in crate::syn) async fn define_graph_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DefineGraphStatement> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		let contract = if self.consume_keyword(keywords::TYPE).is_some() {
			let start = self.current().span;
			let contract = self.graph_contract_until(None, start)?;
			if contract.family.is_none() && contract.validators.is_empty() {
				return Err(self.syntax(
					"Expected a graph family or validator after `TYPE`.",
					self.current().span,
				));
			}
			contract
		} else {
			GraphContract {
				family: None,
				validators: Vec::new(),
				span: name.span,
			}
		};
		let mut root = None;
		let mut content = None;
		let mut changefeed = None;
		loop {
			if self.consume_keyword(keywords::ROOT).is_some() {
				if root.is_some() {
					return Err(self
						.syntax("DEFINE GRAPH ROOT may appear only once.", self.previous_span()));
				}
				root = Some(context.run(|context| self.expression(context, 0, 1, 0)).await?);
			} else if self.consume_keyword(keywords::CONTENT).is_some() {
				if content.is_some() {
					return Err(self.syntax(
						"DEFINE GRAPH CONTENT may appear only once.",
						self.previous_span(),
					));
				}
				content = Some(context.run(|context| self.expression(context, 0, 1, 0)).await?);
			} else if self.consume_keyword(keywords::CHANGEFEED).is_some() {
				if changefeed.is_some() {
					return Err(
						self.syntax("CHANGEFEED may appear only once.", self.previous_span())
					);
				}
				changefeed = Some(self.changefeed_clause()?);
			} else {
				break;
			}
		}
		if let Some(feature) = [keywords::ROOTS, keywords::VALIDATE, keywords::COMMENT]
			.iter()
			.copied()
			.find(|keyword| self.at_keyword(keyword))
		{
			return Err(self.unsupported(feature, self.current().span));
		}
		self.expect_statement_end("DEFINE GRAPH")?;
		Ok(DefineGraphStatement {
			mode,
			name,
			contract,
			root,
			content,
			changefeed,
		})
	}

	/// Parses the define class statement syntax.
	pub(in crate::syn) fn define_class_statement(
		&mut self,
	) -> FerrosGrynnResult<DefineClassStatement> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		self.expect_keyword(
			keywords::TYPE,
			"Expected `TYPE NODE` or `TYPE EDGE` after class name.",
		)?;
		let kind = if self.consume_keyword(keywords::NODE).is_some() {
			ClassKindName::Node
		} else if self.consume_keyword(keywords::EDGE).is_some() {
			ClassKindName::Edge
		} else {
			return Err(self.syntax("Expected `NODE` or `EDGE` after `TYPE`.", self.current().span));
		};
		let mut schema_mode = ClassSchemaModeName::Schemaless;
		let mut changefeed = None;
		while !self.at_statement_end() {
			if self.consume_keyword(keywords::SCHEMALESS).is_some() {
				schema_mode = ClassSchemaModeName::Schemaless;
			} else if self.consume_keyword(keywords::SCHEMAFULL).is_some() {
				schema_mode = ClassSchemaModeName::Schemafull;
			} else if self.consume_keyword(keywords::CHANGEFEED).is_some() {
				if changefeed.is_some() {
					return Err(
						self.syntax("CHANGEFEED may appear only once.", self.previous_span())
					);
				}
				changefeed = Some(self.changefeed_clause()?);
			} else if let Some(feature) = [keywords::DROP, keywords::AS, keywords::COMMENT]
				.iter()
				.copied()
				.find(|keyword| self.at_keyword(keyword))
			{
				return Err(self.unsupported(feature, self.current().span));
			} else {
				return Err(self.syntax("Unexpected DEFINE CLASS clause.", self.current().span));
			}
		}
		Ok(DefineClassStatement {
			mode,
			name,
			kind,
			schema_mode,
			changefeed,
		})
	}

	/// Parses the define field statement syntax.
	pub(in crate::syn) fn define_field_statement(
		&mut self,
	) -> FerrosGrynnResult<DefineFieldStatement> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		self.expect_keyword(keywords::ON, "Expected `ON` after field name.")?;
		self.expect_keyword(keywords::CLASS, "Expected `CLASS` after `ON`.")?;
		let class = self.identifier()?;
		let value_type = if self.consume_keyword(keywords::TYPE).is_some() {
			Some(self.type_name()?)
		} else {
			None
		};
		if !self.at_statement_end() {
			let feature = self.current_word().unwrap_or("DEFINE FIELD clause");
			return Err(self.unsupported(feature, self.current().span));
		}
		Ok(DefineFieldStatement {
			mode,
			name,
			class,
			value_type,
		})
	}

	/// Parses the define index statement syntax.
	pub(in crate::syn) fn define_index_statement(&mut self) -> FerrosGrynnResult<StatementKind> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		self.expect_keyword(keywords::ON, "Expected `ON` after index name.")?;
		if !self.at_keyword(keywords::CLASS) {
			self.consume_keyword(keywords::GRAPH);
			return self
				.define_graph_index_statement(mode, name)
				.map(StatementKind::DefineGraphIndex);
		}
		self.expect_keyword(keywords::CLASS, "Expected `CLASS` after `ON`.")?;
		let class = self.identifier()?;
		let definition = if self.consume_keyword(keywords::COUNT).is_some() {
			BasicIndexDefinition::Count
		} else if self.consume_keyword(keywords::FIELDS).is_some()
			|| self.consume_keyword(keywords::COLUMNS).is_some()
		{
			let mut fields = vec![self.identifier()?];
			while self.consume_symbol(Symbol::Comma).is_some() {
				fields.push(self.identifier()?);
			}
			if self.consume_keyword(keywords::UNIQUE).is_some() {
				BasicIndexDefinition::Unique(fields)
			} else if self.consume_keyword(keywords::FULLTEXT).is_some() {
				let field = self.single_specialized_index_field(fields)?;
				self.expect_keyword(keywords::ANALYZER, "Expected `ANALYZER` after `FULLTEXT`.")?;
				let analyzer = self.identifier()?;
				let (k1, b) = if self.consume_keyword(keywords::BM25).is_some() {
					if let Some(open) = self.consume_symbol(Symbol::LeftParen) {
						let k1 = self.index_f64("BM25 k1")?;
						self.expect_symbol(Symbol::Comma, "Expected `,` between BM25 parameters.")?;
						let b = self.index_f64("BM25 b")?;
						self.close(Symbol::RightParen, open.span, "`)`")?;
						(k1, b)
					} else {
						(1.2, 0.75)
					}
				} else {
					(1.2, 0.75)
				};
				BasicIndexDefinition::Fulltext {
					field,
					analyzer,
					k1_bits: k1.to_bits(),
					b_bits: b.to_bits(),
					highlights: self.consume_keyword(keywords::HIGHLIGHTS).is_some(),
				}
			} else if self.consume_keyword(keywords::HNSW).is_some() {
				let field = self.single_specialized_index_field(fields)?;
				self.expect_keyword(keywords::DIMENSION, "Expected `DIMENSION` after `HNSW`.")?;
				let dimension = self.index_positive_u16("HNSW dimension")?;
				let element = if self.consume_keyword(keywords::TYPE).is_some() {
					self.vector_element_name()?
				} else {
					VectorElementName::F64
				};
				let distance = if self.consume_keyword(keywords::DIST).is_some() {
					self.vector_distance_name()?
				} else {
					VectorDistanceName::Euclidean
				};
				let ef_construction = if self.consume_keyword(keywords::EFC).is_some() {
					self.index_positive_u16("HNSW EFC")?
				} else {
					150
				};
				let m = if self.consume_keyword(keywords::M).is_some() {
					self.index_positive_u16("HNSW M")?
				} else {
					12
				};
				BasicIndexDefinition::Hnsw {
					field,
					dimension,
					element,
					distance,
					ef_construction,
					m,
				}
			} else if self.consume_keyword(keywords::DISKANN).is_some() {
				let field = self.single_specialized_index_field(fields)?;
				self.expect_keyword(keywords::DIMENSION, "Expected `DIMENSION` after `DISKANN`.")?;
				let dimension = self.index_positive_u16("DiskANN dimension")?;
				let element = if self.consume_keyword(keywords::TYPE).is_some() {
					self.vector_element_name()?
				} else {
					VectorElementName::F32
				};
				let distance = if self.consume_keyword(keywords::DIST).is_some() {
					self.vector_distance_name()?
				} else {
					VectorDistanceName::Euclidean
				};
				let degree = if self.consume_keyword(keywords::DEGREE).is_some() {
					self.index_positive_u16("DiskANN degree")?
				} else {
					64
				};
				let build_list_size = if self.consume_keyword(keywords::L_BUILD).is_some() {
					self.index_positive_u16("DiskANN L_BUILD")?
				} else {
					100
				};
				let alpha = if self.consume_keyword(keywords::ALPHA).is_some() {
					self.index_f32("DiskANN alpha")?
				} else {
					1.2
				};
				BasicIndexDefinition::DiskAnn {
					field,
					dimension,
					element,
					distance,
					degree,
					build_list_size,
					alpha_bits: alpha.to_bits(),
					hashed_vector: self.consume_keyword(keywords::HASHED_VECTOR).is_some(),
				}
			} else if self.consume_keyword(keywords::GEOMETRY).is_some() {
				BasicIndexDefinition::Geometry {
					field: self.single_specialized_index_field(fields)?,
					geometry_type: if self.consume_keyword(keywords::TYPE).is_some() {
						Some(self.identifier()?)
					} else {
						None
					},
				}
			} else {
				BasicIndexDefinition::Standard(fields)
			}
		} else {
			return Err(
				self.syntax("Expected `COUNT`, `FIELDS`, or `COLUMNS`.", self.current().span)
			);
		};
		self.expect_keyword(
			keywords::SCOPE,
			"Expected explicit `SCOPE BASE` or `SCOPE EFFECTIVE IN GRAPH`.",
		)?;
		let scope = if self.consume_keyword(keywords::BASE).is_some() {
			IndexScopeName::Base
		} else if self.consume_keyword(keywords::EFFECTIVE).is_some() {
			self.expect_keyword(keywords::IN, "Expected `IN GRAPH` after `SCOPE EFFECTIVE`.")?;
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `SCOPE EFFECTIVE IN`.")?;
			IndexScopeName::Effective {
				graph: self.identifier()?,
			}
		} else {
			return Err(
				self.syntax("Expected `BASE` or `EFFECTIVE` after `SCOPE`.", self.current().span)
			);
		};
		let mut concurrently = false;
		let mut deferred = false;
		while !self.at_statement_end() {
			if self.consume_keyword(keywords::CONCURRENTLY).is_some() {
				if concurrently {
					return Err(
						self.syntax("CONCURRENTLY may appear only once.", self.previous_span())
					);
				}
				concurrently = true;
			} else if self.consume_keyword(keywords::DEFER).is_some() {
				if deferred {
					return Err(self.syntax("DEFER may appear only once.", self.previous_span()));
				}
				deferred = true;
			} else {
				let feature = self.current_word().unwrap_or("DEFINE INDEX clause");
				return Err(self.unsupported(feature, self.current().span));
			}
		}
		Ok(StatementKind::DefineIndex(DefineIndexStatement {
			mode,
			name,
			class,
			definition,
			scope,
			concurrently,
			deferred,
		}))
	}

	/// Parses graph clauses without resolving schema or algorithm semantics.
	fn define_graph_index_statement(
		&mut self,
		mode: DefinitionMode,
		name: Identifier,
	) -> FerrosGrynnResult<DefineGraphIndexStatement> {
		let graph = self.identifier()?;
		let family = if self.consume_keyword("PATH").is_some() {
			GraphIndexFamilyName::Path
		} else if self.consume_keyword("REACHABILITY").is_some() {
			GraphIndexFamilyName::Reachability
		} else if self.consume_keyword("NEIGHBOURHOOD").is_some() {
			let open = self.expect_symbol(
				Symbol::LeftParen,
				"Expected neighbourhood radius in parentheses.",
			)?;
			let radius = self.graph_index_u32("neighbourhood radius")?;
			self.close(Symbol::RightParen, open.span, "`)`")?;
			GraphIndexFamilyName::Neighbourhood(radius)
		} else if self.consume_keyword("TENSOR").is_some() {
			GraphIndexFamilyName::Tensor
		} else {
			return Err(self.syntax(
				"Expected PATH, REACHABILITY, NEIGHBOURHOOD, or TENSOR.",
				self.current().span,
			));
		};
		let mut definition = DefineGraphIndexStatement {
			mode,
			name,
			graph,
			family,
			algorithm: None,
			cache_format: None,
			cache_scope: None,
			projection: None,
			direction: None,
			edge_labels: Vec::new(),
			weighted: false,
			cost: None,
			patterns: Vec::new(),
			max_bytes: None,
			comment: None,
			concurrently: false,
			deferred: false,
		};
		let mut clauses = std::collections::BTreeSet::new();
		while !self.at_statement_end() {
			let clause = self
				.current_word()
				.ok_or_else(|| self.syntax("Expected a graph index clause.", self.current().span))?
				.to_ascii_uppercase();
			if !clauses.insert(clause.clone()) {
				return Err(self.syntax("Duplicate graph index clause.", self.current().span));
			}
			match clause.as_str() {
				"FORMAT" => {
					self.tokens.next();
					definition.cache_format = Some(self.identifier()?);
				}
				"SCOPE" => {
					self.tokens.next();
					definition.cache_scope = Some(self.identifier()?);
				}
				"ALGORITHM" => {
					self.tokens.next();
					let algorithm = self.identifier()?;
					let parameter = if let Some(open) = self.consume_symbol(Symbol::LeftParen) {
						let value = self.graph_index_u32("algorithm parameter")?;
						self.close(Symbol::RightParen, open.span, "`)`")?;
						Some(value)
					} else {
						None
					};
					definition.algorithm = Some((algorithm, parameter));
				}
				"MODE" => {
					self.tokens.next();
					definition.projection = Some(self.identifier()?);
				}
				"DIRECTION" => {
					self.tokens.next();
					definition.direction = Some(self.identifier()?);
				}
				"EDGES" => {
					self.tokens.next();
					definition.edge_labels.push(self.identifier()?);
					while self.consume_symbol(Symbol::Comma).is_some() {
						if definition.edge_labels.len() == 1024 {
							return Err(
								self.syntax("Too many graph index labels.", self.current().span)
							);
						}
						definition.edge_labels.push(self.identifier()?);
					}
				}
				"WEIGHTED" => {
					self.tokens.next();
					definition.weighted = true;
				}
				"COST" => {
					self.tokens.next();
					definition.cost = Some(self.function_signature_name()?);
				}
				"PATTERNS" => {
					self.tokens.next();
					let open = self.expect_symbol(
						Symbol::LeftBracket,
						"Expected a list of pattern source strings.",
					)?;
					let mut bytes = 0_usize;
					if !self.at_symbol(Symbol::RightBracket) {
						loop {
							let pattern = self.string_value("Expected a pattern source string.")?;
							bytes = bytes.checked_add(pattern.len()).ok_or_else(|| {
								self.syntax("Pattern source size overflows.", self.previous_span())
							})?;
							if definition.patterns.len() == 64 || bytes > 1024 * 1024 {
								return Err(self.syntax(
									"Pattern list exceeds its bound.",
									self.previous_span(),
								));
							}
							definition.patterns.push(pattern);
							if self.consume_symbol(Symbol::Comma).is_none() {
								break;
							}
						}
					}
					self.close(Symbol::RightBracket, open.span, "`]`")?;
				}
				"COMMENT" => {
					self.tokens.next();
					definition.comment = Some(self.string_value("Expected a comment string.")?);
				}
				"CONCURRENTLY" => {
					self.tokens.next();
					definition.concurrently = true;
				}
				"DEFER" => {
					self.tokens.next();
					definition.deferred = true;
				}
				"WITH" => {
					self.tokens.next();
					let open = self
						.expect_symbol(Symbol::LeftParen, "Expected WITH (MAX_BYTES = bytes).")?;
					self.expect_keyword("MAX_BYTES", "Expected MAX_BYTES.")?;
					self.expect_symbol(Symbol::Equal, "Expected `=` after MAX_BYTES.")?;
					definition.max_bytes = Some(self.maintenance_positive_integer("MAX_BYTES")?);
					self.close(Symbol::RightParen, open.span, "`)`")?;
				}
				_ => return Err(self.syntax("Unexpected graph index clause.", self.current().span)),
			}
		}
		Ok(definition)
	}

	/// Reuses checked maintenance integers and narrows graph parameters
	/// explicitly.
	fn graph_index_u32(&mut self, field: &str) -> FerrosGrynnResult<u32> {
		u32::try_from(self.maintenance_positive_integer(field)?).map_err(|_error| {
			self.syntax("Graph index parameter exceeds u32.", self.previous_span())
		})
	}

	/// Parses the single specialized index field syntax.
	fn single_specialized_index_field(
		&self,
		mut fields: Vec<Identifier>,
	) -> FerrosGrynnResult<Identifier> {
		if fields.len() != 1 {
			return Err(self.syntax(
				"Fulltext, HNSW, and DiskANN indexes require exactly one field.",
				self.previous_span(),
			));
		}
		fields.pop().ok_or_else(|| {
			self.syntax("A specialized index field is missing.", self.previous_span())
		})
	}

	/// Parses the index positive u16 syntax.
	fn index_positive_u16(&mut self, label: &str) -> FerrosGrynnResult<u16> {
		let token = self.tokens.next();
		let TokenKind::Number(value) = token.kind else {
			return Err(self.syntax(&format!("Expected positive {label}."), token.span));
		};
		let value = value.replace('_', "").parse::<u16>().map_err(|_error| {
			self.syntax(&format!("Expected positive {label} in u16 range."), token.span)
		})?;
		if value == 0 {
			return Err(self.syntax(&format!("Expected positive {label}."), token.span));
		}
		Ok(value)
	}

	/// Parses the index f64 syntax.
	fn index_f64(&mut self, label: &str) -> FerrosGrynnResult<f64> {
		let token = self.tokens.next();
		let TokenKind::Number(value) = token.kind else {
			return Err(self.syntax(&format!("Expected numeric {label}."), token.span));
		};
		value
			.replace('_', "")
			.parse::<f64>()
			.map_err(|_error| self.syntax(&format!("Expected finite numeric {label}."), token.span))
	}

	/// Parses the index f32 syntax.
	fn index_f32(&mut self, label: &str) -> FerrosGrynnResult<f32> {
		let token = self.tokens.next();
		let TokenKind::Number(value) = token.kind else {
			return Err(self.syntax(&format!("Expected numeric {label}."), token.span));
		};
		value
			.replace('_', "")
			.parse::<f32>()
			.map_err(|_error| self.syntax(&format!("Expected finite numeric {label}."), token.span))
	}

	/// Parses the vector element name syntax.
	fn vector_element_name(&mut self) -> FerrosGrynnResult<VectorElementName> {
		let token = self.tokens.next();
		let TokenKind::Word {
			value,
			quoted: false,
		} = token.kind
		else {
			return Err(self.syntax("Expected a vector element type.", token.span));
		};
		if value.eq_ignore_ascii_case(keywords::F64) {
			Ok(VectorElementName::F64)
		} else if value.eq_ignore_ascii_case(keywords::F32) {
			Ok(VectorElementName::F32)
		} else if value.eq_ignore_ascii_case(keywords::F16) {
			Ok(VectorElementName::F16)
		} else if value.eq_ignore_ascii_case(keywords::I64) {
			Ok(VectorElementName::I64)
		} else if value.eq_ignore_ascii_case(keywords::I32) {
			Ok(VectorElementName::I32)
		} else if value.eq_ignore_ascii_case(keywords::I16) {
			Ok(VectorElementName::I16)
		} else if value.eq_ignore_ascii_case(keywords::I8) {
			Ok(VectorElementName::I8)
		} else if value.eq_ignore_ascii_case(keywords::U8) {
			Ok(VectorElementName::U8)
		} else {
			Err(self.syntax("Unknown vector element type.", token.span))
		}
	}

	/// Parses the vector distance name syntax.
	fn vector_distance_name(&mut self) -> FerrosGrynnResult<VectorDistanceName> {
		if self.consume_keyword(keywords::EUCLIDEAN).is_some() {
			Ok(VectorDistanceName::Euclidean)
		} else if self.consume_keyword(keywords::COSINE).is_some() {
			Ok(VectorDistanceName::Cosine)
		} else if self.consume_keyword(keywords::MANHATTAN).is_some() {
			Ok(VectorDistanceName::Manhattan)
		} else if self.consume_keyword(keywords::MINKOWSKI).is_some() {
			let open = self.expect_symbol(Symbol::LeftParen, "Expected `(` after `MINKOWSKI`.")?;
			let exponent = self.index_f64("Minkowski exponent")?;
			self.close(Symbol::RightParen, open.span, "`)`")?;
			Ok(VectorDistanceName::Minkowski {
				exponent_bits: exponent.to_bits(),
			})
		} else if self.consume_keyword(keywords::INNER_PRODUCT).is_some() {
			Ok(VectorDistanceName::InnerProduct)
		} else if self.consume_keyword(keywords::COSINE_NORMALIZED).is_some() {
			Ok(VectorDistanceName::CosineNormalized)
		} else {
			let function = self.function_signature_name()?;
			if function.parameters.is_some() {
				return Err(self.syntax(
					"A custom distance names fn::name without an overload signature.",
					function.name.span,
				));
			}
			Ok(VectorDistanceName::Function(function))
		}
	}

	/// Removes statement.
	pub(in crate::syn) fn remove_statement(&mut self) -> FerrosGrynnResult<RemoveStatement> {
		let target = if self.consume_keyword("EVENT").is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			RemoveTarget::Event {
				if_exists,
				name,
				owner: self.event_owner()?,
			}
		} else if self.consume_keyword(keywords::LIVE).is_some() {
			self.expect_keyword(keywords::LET, "Expected LET after REMOVE LIVE.")?;
			RemoveTarget::LiveLet(self.parameter()?)
		} else if self.consume_keyword(keywords::ANALYZER).is_some() {
			let if_exists = self.if_exists()?;
			RemoveTarget::Analyzer {
				if_exists,
				name: self.identifier()?,
			}
		} else if self.consume_keyword(keywords::FUNCTION).is_some() {
			let if_exists = self.if_exists()?;
			RemoveTarget::Function {
				if_exists,
				signature: self.function_signature_name()?,
			}
		} else if self.consume_keyword(keywords::PARAM).is_some() {
			let if_exists = self.if_exists()?;
			RemoveTarget::Parameter {
				if_exists,
				name: self.parameter()?,
			}
		} else if self.consume_keyword(keywords::GRAPH).is_some() {
			let if_exists = self.if_exists()?;
			RemoveTarget::Graph {
				if_exists,
				name: self.identifier()?,
			}
		} else if self.consume_keyword(keywords::CLASS).is_some() {
			let if_exists = self.if_exists()?;
			RemoveTarget::Class {
				if_exists,
				name: self.identifier()?,
			}
		} else if self.consume_keyword(keywords::FIELD).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			let class = self.on_class()?;
			RemoveTarget::Field {
				if_exists,
				name,
				class,
			}
		} else if self.consume_keyword(keywords::INDEX).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			match self.maintenance_index_owner()? {
				MaintenanceIndexOwner::Class(class) => RemoveTarget::Index {
					if_exists,
					name,
					class,
				},
				MaintenanceIndexOwner::Graph(graph) => RemoveTarget::GraphIndex {
					if_exists,
					name,
					graph,
				},
			}
		} else {
			let feature = self.current_word().unwrap_or("REMOVE target");
			return Err(self.unsupported(feature, self.current().span));
		};
		self.expect_statement_end("REMOVE")?;
		Ok(RemoveStatement {
			target,
		})
	}

	/// Parses the alter statement syntax.
	pub(in crate::syn) async fn alter_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<AlterStatement> {
		let live = self.consume_keyword(keywords::LIVE).is_some();
		if live {
			self.expect_keyword(keywords::LET, "Expected LET after ALTER LIVE.")?;
		}
		if live || self.consume_keyword(keywords::LET).is_some() {
			let name = self.parameter()?;
			self.expect_keyword(keywords::VALUE, "Expected VALUE after ALTER LET binding.")?;
			let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
			self.expect_statement_end("ALTER LET")?;
			return Ok(AlterStatement {
				target: if live {
					AlterTarget::LiveLet {
						name,
						value,
					}
				} else {
					AlterTarget::Let {
						name,
						value,
					}
				},
			});
		}
		context.run(|context| self.alter_schema_statement(context)).await
	}

	/// Schema definitions retain their existing parser independently of lexical
	/// updates.
	async fn alter_schema_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<AlterStatement> {
		let target = if self.consume_keyword("EVENT").is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			let owner = self.event_owner()?;
			let actions = context.run(|context| self.event_alter_actions(context)).await?;
			AlterTarget::Event {
				if_exists,
				name,
				owner,
				actions,
			}
		} else if self.consume_keyword(keywords::ANALYZER).is_some() {
			self.alter_analyzer_target(context).await?
		} else if self.consume_keyword(keywords::FUNCTION).is_some() {
			let if_exists = self.if_exists()?;
			let signature = self.function_signature_name()?;
			let effect = self.function_effect();
			let cost = if self.consume_keyword(keywords::COST).is_some() {
				Some(true)
			} else if self.at_keyword(keywords::DROP) && self.peek_keyword(1, keywords::COST) {
				self.tokens.next();
				self.expect_keyword(keywords::COST, "Expected `COST` after `DROP`.")?;
				Some(false)
			} else {
				None
			};
			let max_depth = if self.consume_keyword(keywords::MAXDEPTH).is_some() {
				Some(Some(self.function_max_depth()?))
			} else if self.at_keyword(keywords::DROP) && self.peek_keyword(1, keywords::MAXDEPTH) {
				self.tokens.next();
				self.expect_keyword(keywords::MAXDEPTH, "Expected `MAXDEPTH` after `DROP`.")?;
				Some(None)
			} else {
				None
			};
			let body = if self.at_symbol(Symbol::LeftBrace) {
				Some(context.run(|context| self.block(context)).await?)
			} else {
				None
			};
			let comment = if self.consume_keyword(keywords::COMMENT).is_some() {
				ParameterCommentChange::Set(
					self.string_value("Expected a string after `COMMENT`.")?,
				)
			} else if self.at_keyword(keywords::DROP) && self.peek_keyword(1, keywords::COMMENT) {
				self.tokens.next();
				self.expect_keyword(keywords::COMMENT, "Expected `COMMENT` after `DROP`.")?;
				ParameterCommentChange::Drop
			} else {
				ParameterCommentChange::Keep
			};
			if effect.is_none()
				&& cost.is_none()
				&& max_depth.is_none()
				&& body.is_none()
				&& comment == ParameterCommentChange::Keep
			{
				return Err(self.syntax(
					"ALTER FUNCTION requires an effect, COST, MAXDEPTH, body, or COMMENT change.",
					self.current().span,
				));
			}
			AlterTarget::Function {
				if_exists,
				signature,
				effect,
				cost,
				max_depth,
				body,
				comment,
			}
		} else if self.consume_keyword(keywords::PARAM).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.parameter()?;
			let mut value = None;
			let mut comment = ParameterCommentChange::Keep;
			while !self.at_statement_end() {
				if self.consume_keyword(keywords::VALUE).is_some() {
					if value.is_some() {
						return Err(self.syntax(
							"ALTER PARAM VALUE may appear only once.",
							self.previous_span(),
						));
					}
					value = Some(context.run(|context| self.expression(context, 0, 1, 0)).await?);
				} else if self.consume_keyword(keywords::COMMENT).is_some() {
					if comment != ParameterCommentChange::Keep {
						return Err(self.syntax(
							"ALTER PARAM COMMENT may appear only once.",
							self.previous_span(),
						));
					}
					comment = ParameterCommentChange::Set(
						self.string_value("Expected a string after `COMMENT`.")?,
					);
				} else if self.consume_keyword(keywords::DROP).is_some() {
					self.expect_keyword(keywords::COMMENT, "Expected `COMMENT` after `DROP`.")?;
					if comment != ParameterCommentChange::Keep {
						return Err(self.syntax(
							"ALTER PARAM COMMENT may appear only once.",
							self.previous_span(),
						));
					}
					comment = ParameterCommentChange::Drop;
				} else {
					return Err(self.syntax("Unexpected ALTER PARAM clause.", self.current().span));
				}
			}
			if value.is_none() && comment == ParameterCommentChange::Keep {
				return Err(self.syntax(
					"ALTER PARAM requires VALUE, COMMENT, or DROP COMMENT.",
					self.current().span,
				));
			}
			AlterTarget::Parameter {
				if_exists,
				name,
				value,
				comment,
			}
		} else if self.consume_keyword(keywords::CLASS).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			if self.at_keyword(keywords::CHANGEFEED) || self.at_keyword(keywords::DROP) {
				let target = self.alter_changefeed(false, if_exists, name)?;
				self.expect_statement_end("ALTER CLASS")?;
				return Ok(AlterStatement {
					target,
				});
			}
			let schema_mode = if self.consume_keyword(keywords::SCHEMAFULL).is_some() {
				ClassSchemaModeName::Schemafull
			} else if self.consume_keyword(keywords::SCHEMALESS).is_some() {
				ClassSchemaModeName::Schemaless
			} else {
				let feature = self.current_word().unwrap_or("ALTER CLASS action");
				return Err(self.unsupported(feature, self.current().span));
			};
			AlterTarget::Class {
				if_exists,
				name,
				schema_mode,
			}
		} else if self.consume_keyword(keywords::FIELD).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			let class = self.on_class()?;
			let value_type = if self.consume_keyword(keywords::TYPE).is_some() {
				Some(self.type_name()?)
			} else if self.consume_keyword(keywords::DROP).is_some() {
				self.expect_keyword(keywords::TYPE, "Expected `TYPE` after `DROP`.")?;
				None
			} else {
				let feature = self.current_word().unwrap_or("ALTER FIELD action");
				return Err(self.unsupported(feature, self.current().span));
			};
			AlterTarget::Field {
				if_exists,
				name,
				class,
				value_type,
			}
		} else if self.consume_keyword(keywords::INDEX).is_some() {
			let name = self.identifier()?;
			let class = self.on_class()?;
			self.expect_keyword(keywords::PREPARE, "Expected `PREPARE REMOVE` for ALTER INDEX.")?;
			self.expect_keyword(keywords::REMOVE, "Expected `REMOVE` after `PREPARE`.")?;
			AlterTarget::IndexPrepareRemove {
				name,
				class,
			}
		} else if self.consume_keyword(keywords::GRAPH).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			self.alter_changefeed(true, if_exists, name)?
		} else {
			let feature = self.current_word().unwrap_or("ALTER target");
			return Err(self.unsupported(feature, self.current().span));
		};
		self.expect_statement_end("ALTER")?;
		Ok(AlterStatement {
			target,
		})
	}

	/// Parses one ALTER ANALYZER target after ANALYZER was consumed.
	///
	/// # Errors
	/// Returns the existing clause, duplicate-change or incompatible-option
	/// error.
	async fn alter_analyzer_target(&mut self, context: &mut Stk) -> FerrosGrynnResult<AlterTarget> {
		let if_exists = self.if_exists()?;
		let name = self.identifier()?;
		let mut function = None;
		let mut tokenizers = None;
		let mut filters = None;
		let mut comment = ParameterCommentChange::Keep;
		while !self.at_statement_end() {
			if self.consume_keyword(keywords::FUNCTION).is_some() {
				if function.is_some() {
					return Err(self.syntax(
						"ALTER ANALYZER FUNCTION may appear only once.",
						self.previous_span(),
					));
				}
				function = Some(Some(self.analyzer_function_name()?));
			} else if self.consume_keyword(keywords::TOKENIZERS).is_some() {
				if tokenizers.is_some() {
					return Err(self.syntax(
						"ALTER ANALYZER TOKENIZERS may appear only once.",
						self.previous_span(),
					));
				}
				tokenizers = Some(self.analyzer_tokenizers()?);
			} else if self.consume_keyword(keywords::FILTERS).is_some() {
				if filters.is_some() {
					return Err(self.syntax(
						"ALTER ANALYZER FILTERS may appear only once.",
						self.previous_span(),
					));
				}
				filters = Some(context.run(|context| self.analyzer_filters(context)).await?);
			} else if self.consume_keyword(keywords::COMMENT).is_some() {
				if comment != ParameterCommentChange::Keep {
					return Err(self.syntax(
						"ALTER ANALYZER COMMENT may appear only once.",
						self.previous_span(),
					));
				}
				comment = ParameterCommentChange::Set(
					self.string_value("Expected a string after `COMMENT`.")?,
				);
			} else if self.consume_keyword(keywords::DROP).is_some() {
				if self.consume_keyword(keywords::FUNCTION).is_some() {
					if function.is_some() {
						return Err(self.syntax(
							"ALTER ANALYZER FUNCTION may appear only once.",
							self.previous_span(),
						));
					}
					function = Some(None);
				} else if self.consume_keyword(keywords::TOKENIZERS).is_some() {
					if tokenizers.is_some() {
						return Err(self.syntax(
							"ALTER ANALYZER TOKENIZERS may appear only once.",
							self.previous_span(),
						));
					}
					tokenizers = Some(Vec::new());
				} else if self.consume_keyword(keywords::FILTERS).is_some() {
					if filters.is_some() {
						return Err(self.syntax(
							"ALTER ANALYZER FILTERS may appear only once.",
							self.previous_span(),
						));
					}
					filters = Some(Vec::new());
				} else if self.consume_keyword(keywords::COMMENT).is_some() {
					if comment != ParameterCommentChange::Keep {
						return Err(self.syntax(
							"ALTER ANALYZER COMMENT may appear only once.",
							self.previous_span(),
						));
					}
					comment = ParameterCommentChange::Drop;
				} else {
					return Err(self.syntax(
						"Expected FUNCTION, TOKENIZERS, FILTERS, or COMMENT after DROP.",
						self.current().span,
					));
				}
			} else {
				return Err(self.syntax("Unexpected ALTER ANALYZER clause.", self.current().span));
			}
		}
		if function.is_none()
			&& tokenizers.is_none()
			&& filters.is_none()
			&& comment == ParameterCommentChange::Keep
		{
			return Err(
				self.syntax("ALTER ANALYZER requires at least one change.", self.current().span)
			);
		}
		if function.as_ref().is_some_and(Option::is_some)
			&& tokenizers.as_ref().is_some_and(|values| !values.is_empty())
		{
			return Err(
				self.syntax("ANALYZER FUNCTION and TOKENIZERS are mutually exclusive.", name.span)
			);
		}
		Ok(AlterTarget::Analyzer {
			if_exists,
			name,
			function,
			tokenizers,
			filters,
			comment,
		})
	}

	/// Parses the info statement syntax.
	pub(in crate::syn) fn info_statement(&mut self) -> FerrosGrynnResult<InfoStatement> {
		self.expect_keyword(keywords::FOR, "Expected `FOR` after `INFO`.")?;
		let target = if self.consume_keyword(keywords::DB).is_some() {
			InfoTarget::Database
		} else if self.consume_keyword(keywords::CHANGEFEED).is_some() {
			InfoTarget::Changefeed(self.parameter()?)
		} else if self.consume_keyword(keywords::LIVE).is_some() {
			if self.consume_keyword(keywords::LET).is_some() {
				InfoTarget::LiveLet(self.parameter()?)
			} else {
				InfoTarget::Live(self.parameter()?)
			}
		} else if self.consume_keyword(keywords::ANALYZER).is_some() {
			InfoTarget::Analyzer(self.identifier()?)
		} else if self.consume_keyword(keywords::FUNCTION).is_some() {
			InfoTarget::Function(self.function_signature_name()?)
		} else if self.consume_keyword(keywords::PARAM).is_some() {
			InfoTarget::Parameter(self.parameter()?)
		} else if self.consume_keyword(keywords::JOB).is_some() {
			InfoTarget::Job(self.parameter()?)
		} else if self.consume_keyword(keywords::JOBS).is_some() {
			InfoTarget::Jobs
		} else if self.consume_keyword(keywords::CLASS).is_some() {
			InfoTarget::Class(self.identifier()?)
		} else if self.consume_keyword(keywords::GRAPH).is_some() {
			InfoTarget::Graph(self.identifier()?)
		} else if self.consume_keyword(keywords::INDEX).is_some() {
			let name = self.identifier()?;
			match self.maintenance_index_owner()? {
				MaintenanceIndexOwner::Class(class) => InfoTarget::Index {
					name,
					class,
				},
				MaintenanceIndexOwner::Graph(graph) => InfoTarget::GraphIndex {
					name,
					graph,
				},
			}
		} else {
			let feature = self.current_word().unwrap_or("INFO target");
			return Err(self.unsupported(feature, self.current().span));
		};
		self.expect_statement_end("INFO")?;
		Ok(InfoStatement {
			target,
		})
	}

	/// Parses the definition mode syntax.
	pub(in crate::syn) fn definition_mode(&mut self) -> FerrosGrynnResult<DefinitionMode> {
		if self.consume_keyword(keywords::OVERWRITE).is_some() {
			return Ok(DefinitionMode::Overwrite);
		}
		if self.consume_keyword(keywords::IF).is_some() {
			self.expect_keyword(keywords::NOT, "Expected `NOT EXISTS` after `IF`.")?;
			self.expect_keyword(keywords::EXISTS, "Expected `EXISTS` after `IF NOT`.")?;
			return Ok(DefinitionMode::IfNotExists);
		}
		Ok(DefinitionMode::Normal)
	}

	/// Parses the if exists syntax.
	pub(in crate::syn) fn if_exists(&mut self) -> FerrosGrynnResult<bool> {
		if self.consume_keyword(keywords::IF).is_none() {
			return Ok(false);
		}
		self.expect_keyword(keywords::EXISTS, "Expected `EXISTS` after `IF`.")?;
		Ok(true)
	}

	/// Parses the on class syntax.
	pub(in crate::syn) fn on_class(&mut self) -> FerrosGrynnResult<Identifier> {
		self.expect_keyword(keywords::ON, "Expected `ON CLASS` before class name.")?;
		self.expect_keyword(keywords::CLASS, "Expected `CLASS` after `ON`.")?;
		self.identifier()
	}

	/// Returns whether the at statement end condition holds.
	pub(in crate::syn) fn at_statement_end(&self) -> bool {
		self.at_eof() || self.at_symbol(Symbol::Semicolon)
	}

	/// Parses the expect statement end syntax.
	pub(in crate::syn) fn expect_statement_end(&self, statement: &str) -> FerrosGrynnResult<()> {
		if self.at_statement_end() {
			Ok(())
		} else {
			Err(self.syntax(&format!("Unexpected {statement} clause."), self.current().span))
		}
	}

	/// Parses the current word syntax.
	pub(in crate::syn) fn current_word(&self) -> Option<&str> {
		match &self.current().kind {
			TokenKind::Word {
				value,
				..
			} => Some(value),
			_ => None,
		}
	}

	/// Reads the shared quoted-string clause used by schema and EVENT
	/// statements.
	pub(super) fn string_value(&mut self, message: &str) -> FerrosGrynnResult<Box<str>> {
		let token = self.tokens.next();
		match token.kind {
			TokenKind::String(value) => Ok(value),
			_ => Err(self.syntax(message, token.span)),
		}
	}

	/// Returns whether the at function definition condition holds.
	pub(in crate::syn) fn at_function_definition(&self) -> bool {
		[keywords::PURE, keywords::STABLE, keywords::IMPURE, keywords::COST, keywords::FUNCTION]
			.iter()
			.any(|keyword| self.at_keyword(keyword))
	}

	/// Parses the function effect syntax.
	fn function_effect(&mut self) -> Option<FunctionEffectName> {
		if self.consume_keyword(keywords::PURE).is_some() {
			Some(FunctionEffectName::Pure)
		} else if self.consume_keyword(keywords::STABLE).is_some() {
			Some(FunctionEffectName::Stable)
		} else if self.consume_keyword(keywords::IMPURE).is_some() {
			Some(FunctionEffectName::Impure)
		} else {
			None
		}
	}

	/// Parses the qualified function name syntax.
	fn qualified_function_name(&mut self) -> FerrosGrynnResult<(Identifier, Identifier)> {
		let namespace = self.identifier()?;
		self.expect_symbol(Symbol::DoubleColon, "Expected `::` in a function name.")?;
		let name = self.identifier()?;
		Ok((namespace, name))
	}

	/// Parses the function signature name syntax.
	pub(in crate::syn) fn function_signature_name(
		&mut self,
	) -> FerrosGrynnResult<FunctionSignatureName> {
		let (namespace, name) = self.qualified_function_name()?;
		let parameters = if let Some(open) = self.consume_symbol(Symbol::LeftParen) {
			let mut parameters = Vec::new();
			if !self.at_symbol(Symbol::RightParen) {
				loop {
					parameters.push(self.type_name()?);
					if self.consume_symbol(Symbol::Comma).is_none() {
						break;
					}
				}
			}
			self.close(Symbol::RightParen, open.span, "`)`")?;
			Some(parameters)
		} else {
			None
		};
		Ok(FunctionSignatureName {
			namespace,
			name,
			parameters,
		})
	}

	/// Parses the function max depth syntax.
	fn function_max_depth(&mut self) -> FerrosGrynnResult<u8> {
		let token = self.tokens.next();
		let TokenKind::Number(value) = token.kind else {
			return Err(self.syntax("Expected MAXDEPTH in 1..=64.", token.span));
		};
		let depth = value
			.replace('_', "")
			.parse::<u8>()
			.map_err(|_error| self.syntax("Expected MAXDEPTH in 1..=64.", token.span))?;
		if !(1..=64).contains(&depth) {
			return Err(self.syntax("Expected MAXDEPTH in 1..=64.", token.span));
		}
		Ok(depth)
	}
}
