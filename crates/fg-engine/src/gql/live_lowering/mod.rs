//! Rust logical builders lower to the ordinary SELECT analyzer without text.

use fg_runtime::__internal::{RecordIdInternal as _, TypeInternal as _, ValueInternal as _};
use fg_syntax::__internal::SelectMetadata as _;

use crate::{
	datastore::{DatabaseRoot, Transaction},
	gql_status::{FerrosGrynnError, FerrosGrynnResult, TextSpan},
	live::{
		LiveBinaryOperator,
		LiveExpr,
		LiveNullsOrder,
		LiveOptions,
		LiveOrder,
		LiveOrderDirection,
		LiveOrderKey,
		LivePath,
		LiveProjection,
		LiveQuery,
		LiveSource,
	},
	syn::{
		BinaryOperator,
		Expr,
		ExprKind,
		Identifier,
		ObjectField,
		SelectGroup,
		SelectItem,
		SelectJoin,
		SelectJoinKind,
		SelectNulls,
		SelectOrder,
		SelectOrderDirection,
		SelectPath,
		SelectSource,
		SelectSourceKind,
		SelectStart,
		SelectStatement,
		SelectTarget,
		StatementKind,
		UnaryOperator,
	},
	values::{Object, Text, Type, Value},
};

/// One declared JOIN relation and its analyzer-only class state.
type JoinInput<'query> = (
	Vec<Text>,
	Option<SourceGraph<'query>>,
	Option<&'query crate::values::RecordId>,
	Option<usize>,
);

/// Query result collection retains identities and bounded read dependencies.
mod query;
pub use query::{QueryEvaluation, evaluate_query, evaluate_query_controlled};

/// Borrows row-relative segments using the ordinary source qualifier rules.
/// Callers admit schema inspection before calling this allocation-free helper.
pub fn source_field_segments<'path>(
	path: &'path LivePath,
	class: Option<&crate::datastore::ClassSchema>,
	alias: Option<&Text>,
) -> &'path [Text] {
	let fields = path.segments();
	if fields.len() > 1
		&& (alias.is_some_and(|alias| alias.as_str().eq_ignore_ascii_case(fields[0].as_str()))
			|| class.is_some_and(|class| {
				class.name().as_str().eq_ignore_ascii_case(fields[0].as_str())
					&& !class
						.fields()
						.any(|field| field.name().as_str().eq_ignore_ascii_case(fields[0].as_str()))
			})) {
		&fields[1..]
	} else {
		fields
	}
}

/// Validates SDK recipes through the existing typed query/effect analysis.
pub fn validate_registration_controlled(
	tx: &mut Transaction,
	root: &DatabaseRoot,
	query: &LiveQuery,
	options: &LiveOptions,
	parent: Option<&super::ExecutionControl>,
) -> FerrosGrynnResult<()> {
	let bindings = |id| {
		root.live_catalog()
			.bindings
			.get(&id)
			.filter(|entry| {
				matches!(
					entry.state,
					crate::live::LiveState::Active | crate::live::LiveState::Pending
				)
			})
			.map(|entry| entry.value.as_ref())
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"LIVE LET dependency is absent, removed or paused",
				)
			})
	};
	let mut lowering = Lowering {
		parent: parent.map(|control| control.child(None)),
		root,
		current_schema: None,
		parameters: Object::new(),
		next_literal: 0,
		captures: Vec::new(),
		account: super::options::BarrierAccount::new(options.scratch_bytes()),
		maximum_scratch: options.scratch_bytes(),
		admitted_scratch: 0,
		expression_depth: 0,
		remaining_work: options.work(),
		bindings: &bindings,
		record_sources: std::collections::BTreeMap::new(),
		class_namespace: false,
		current_alias: None,
		current_logical_alias: None,
		multi_source: false,
		join_schemas: Vec::new(),
		next_scope: 0,
		independent_queries: std::collections::BTreeMap::new(),
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
	let cache = super::closure::LanguageClosureState::bounded_with_work(
		lowering.maximum_scratch - lowering.admitted_scratch,
		lowering.remaining_work,
	)
	.with_compile_checks(parent.map(super::ExecutionControl::interruption));
	let (program, _stamp) = super::execution::analyze_parsed_transaction(
		tx,
		"",
		&{
			let split_field_0 = statements;
			crate::syn::split_internal::new_program(split_field_0)
		},
		&lowering.parameters,
		Some(&cache),
	)?;
	if let Some(parent) = parent {
		parent.check()?;
	}
	for statement in &program.statements {
		super::optimizer::validate_deterministic_statement(statement).map_err(|error| {
			FerrosGrynnError::query_feature_not_supported(
				"non-PURE or nondeterministic LIVE expression",
			)
			.with_cause(error)
		})?;
	}
	Ok(())
}

/// One registration's synthetic literal slots never replace named captures.
struct Lowering<'root, 'query> {
	/// Registration-only interruption; never retained in the durable recipe.
	parent: Option<super::ExecutionControl>,
	/// Exact statement metadata used for source resolution.
	root: &'root DatabaseRoot,
	/// Borrowed metadata for repeated field-type admission in this lexical row.
	current_schema: Option<std::sync::Arc<crate::datastore::ClassSchema>>,
	/// Frozen owned values supplied to the ordinary analyzer.
	parameters: Object,
	/// Collision-free synthetic slot sequence within this recipe.
	next_literal: usize,
	/// Nested SELECT captures resolve from the innermost lexical scope.
	captures: Vec<&'query Object>,
	/// Existing query admission accounts retained syntax and owned literal
	/// slots.
	account: super::options::BarrierAccount,
	/// Caller-selected aggregate byte ceiling.
	maximum_scratch: usize,
	/// Retained syntax/schema admission leaves bounded room for function
	/// compilation.
	admitted_scratch: usize,
	/// Ancestor expressions can copy a child's inferred type again.
	expression_depth: usize,
	/// Unspent logical/source/schema work, shared with named compilation.
	remaining_work: u64,
	/// Stable reactive values from this exact candidate catalog.
	bindings: &'root dyn Fn(crate::live::LiveBindingId) -> FerrosGrynnResult<&'root Value>,
	/// Record source spellings retain the concrete owner seen at registration.
	record_sources: std::collections::BTreeMap<Text, crate::datastore::ClassId>,
	/// Graph-wide sources depend on the class set, including an empty set.
	class_namespace: bool,
	/// Synthetic aliases distinguish current fields from correlated outer rows.
	current_alias: Option<Box<str>>,
	/// Public qualifier never replaces the unique synthetic capture owner.
	current_logical_alias: Option<&'query Text>,
	/// JOIN fields bind through public qualifiers and ordinary ambiguity rules.
	multi_source: bool,
	/// Borrowed immutable source shapes for conservative typed-copy admission.
	join_schemas: Vec<std::sync::Arc<crate::datastore::ClassSchema>>,
	/// Checked lexical source sequence within this recipe.
	next_scope: usize,
	/// Borrowed recipe addresses are used only for lookup; IDs follow traversal
	/// order.
	independent_queries: std::collections::BTreeMap<*const LiveQuery, u32>,
}

/// Static scopes keep their refreshed identity; expression scopes borrow
/// recipes.
enum SourceGraph<'query> {
	/// One admitted current reference, shared by graph-wide class scans.
	Reference(
		/// One admitted current reference, shared by graph-wide class scans.
		crate::graph::GraphRef,
	),
	/// Lowered through the same compiler as WHERE and projection.
	Expression(
		/// Lowered through the same compiler as WHERE and projection.
		&'query LiveExpr,
	),
}

impl<'query> Lowering<'_, 'query> {
	/// Checks work before traversing another logical node/source.
	fn work(&mut self, amount: u64) -> FerrosGrynnResult<()> {
		if let Some(parent) = &self.parent {
			parent.check()?;
		}
		self.remaining_work = self.remaining_work.checked_sub(amount).ok_or_else(|| {
			FerrosGrynnError::traversal_resource_limit_exceeded("LIVE registration work", amount)
		})?;
		Ok(())
	}

	/// Uses the existing SELECT barrier account before retaining compiler
	/// input.
	fn admit(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		if let Some(parent) = &self.parent {
			parent.check()?;
		}
		if !self.account.reserve(bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE registration scratch",
				self.maximum_scratch as u64,
			));
		}
		self.admitted_scratch += bytes;
		Ok(())
	}

	/// Checks retained collection storage before its allocation.
	fn admit_array<T>(&mut self, count: usize) -> FerrosGrynnResult<()> {
		let bytes = count
			.checked_mul(2 * std::mem::size_of::<T>())
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE syntax size overflows"))?;
		self.admit(bytes)
	}

	/// Accounts copied UTF-8 names and both syntax/analysis path nodes.
	fn admit_path(&mut self, path: &LivePath) -> FerrosGrynnResult<()> {
		self.admit_array::<Expr>(path.segments().len())?;
		self.admit_array::<Identifier>(path.segments().len())?;
		for segment in path.segments() {
			self.admit_name(segment.as_str())?;
		}
		Ok(())
	}

	/// Names are copied by syntax construction and typed analysis.
	fn admit_name(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.admit(
			name.len().checked_mul(2).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("LIVE identifier size overflows")
			})?,
		)
	}

	/// Ordinary SELECT analysis copies input, output and wildcard schemas.
	fn admit_schema(&mut self, schema: &crate::datastore::ClassSchema) -> FerrosGrynnResult<()> {
		admit_schema_shape(schema, &mut |bytes, work| {
			self.work(work)?;
			self.admit(bytes)
		})
	}

	/// Admits user qualifiers copied into columns, origins and planner schemas.
	fn admit_source_alias(
		&mut self,
		alias: &str,
		schema: &crate::datastore::ClassSchema,
	) -> FerrosGrynnResult<()> {
		let bytes = schema
			.fields()
			.len()
			.checked_add(5)
			.and_then(|count| count.checked_mul(alias.len()))
			.and_then(|bytes| bytes.checked_mul(16))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE source alias copies"))?;
		self.admit(bytes)
	}

	/// Each typed field/output expression copies its possibly rich source type.
	fn admit_field_type(&mut self, path: &LivePath) -> FerrosGrynnResult<()> {
		if let Some(schema) = self.current_schema.clone() {
			self.admit_source_field_type(path, &schema)?;
		}
		for index in 0..self.join_schemas.len() {
			let schema = self.join_schemas[index].clone();
			self.admit_source_field_type(path, &schema)?;
		}
		Ok(())
	}

	/// Accounts one immutable schema before ordinary field type inference.
	fn admit_source_field_type(
		&mut self,
		path: &LivePath,
		schema: &crate::datastore::ClassSchema,
	) -> FerrosGrynnResult<()> {
		for field in schema.fields() {
			self.work(1)?;
			// Match the normal analyzer's case-insensitive column rules. The
			// first two segments cover both field and qualifier.field syntax;
			// conservative admission also covers an ambiguous qualifier.
			if path
				.segments()
				.iter()
				.take(2)
				.any(|segment| field.name().as_str().eq_ignore_ascii_case(segment.as_str()))
			{
				self.admit_expression_type(field.value_type())?;
			}
		}
		Ok(())
	}

	/// Charges direct type inference and possible ancestor propagation.
	fn admit_expression_type(&mut self, ty: &Type) -> FerrosGrynnResult<()> {
		self.admit_type_copies(ty, 0, self.expression_depth + 1)
	}

	/// Reserves each borrowed rich shape before compiler-owned copies exist.
	fn admit_type_copies(
		&mut self,
		ty: &Type,
		depth: usize,
		ancestors: usize,
	) -> FerrosGrynnResult<()> {
		admit_type_shape(ty, depth, ancestors, &mut |bytes, work| {
			self.work(work)?;
			self.admit(bytes)
		})
	}

	/// Qualified occurrence sources use ordinary class scans in the graph
	/// scope.
	fn selects(&mut self, query: &'query LiveQuery) -> FerrosGrynnResult<Vec<SelectStatement>> {
		self.captures.push(query.parameters());
		let previous_schema = self.current_schema.clone();
		let previous_alias = self.current_alias.take();
		let previous_logical_alias = self.current_logical_alias.take();
		let previous_multi_source = std::mem::replace(&mut self.multi_source, false);
		let previous_join_schemas = std::mem::take(&mut self.join_schemas);
		let result = self.selects_inner(query);
		self.current_schema = previous_schema;
		self.current_alias = previous_alias;
		self.current_logical_alias = previous_logical_alias;
		self.multi_source = previous_multi_source;
		self.join_schemas = previous_join_schemas;
		self.captures.pop();
		result
	}

	/// Resolves admitted source choices without lowering row expressions.
	fn source_choices(
		&mut self,
		source: &'query LiveSource,
	) -> FerrosGrynnResult<(
		Vec<Text>,
		Option<SourceGraph<'query>>,
		Option<&'query crate::values::RecordId>,
	)> {
		let class_count = if matches!(source, LiveSource::Graph(_) | LiveSource::GraphExpr(_)) {
			self.class_namespace = true;
			self.root.schema.classes().len()
		} else {
			1
		};
		self.work(class_count as u64)?;
		self.admit(
			class_count
				.max(1)
				.checked_mul(2 * std::mem::size_of::<SelectStatement>())
				.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE syntax size overflows"))?,
		)?;
		if matches!(source, LiveSource::Graph(_) | LiveSource::GraphExpr(_)) {
			let names = self
				.root
				.schema
				.classes()
				.try_fold(0_usize, |bytes, class| bytes.checked_add(class.name().len()))
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE source names size overflows")
				})?;
			self.admit(names)?;
		}
		Ok(match source {
			LiveSource::Class(class) => (vec![self.class_name(*class)?], None, None),
			LiveSource::Record(record)
			| LiveSource::GraphRecord {
				record,
				..
			}
			| LiveSource::GraphRecordExpr {
				record,
				..
			} => {
				self.admit_name(record.class())?;
				let owner = self
					.root
					.schema
					.class(record.class())
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("LIVE record source schema is missing")
					})?
					.id()
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"LIVE record source has no class identity",
						)
					})?;
				if !self.record_sources.contains_key(record.class()) {
					self.admit(128 + record.class().len())?;
					self.record_sources.insert(record.class().into(), owner);
				}
				let graph = match source {
					LiveSource::GraphRecord {
						graph,
						..
					} => Some(SourceGraph::Reference(self.current_graph(graph)?)),
					LiveSource::GraphRecordExpr {
						graph,
						..
					} => Some(SourceGraph::Expression(graph)),
					_ => None,
				};
				(vec![Text::from(record.class())], graph, Some(record))
			}
			LiveSource::GraphClass {
				graph,
				class,
			} => (
				vec![self.class_name(*class)?],
				Some(SourceGraph::Reference(self.current_graph(graph)?)),
				None,
			),
			LiveSource::GraphClassExpr {
				graph,
				class,
			} => (vec![self.class_name(*class)?], Some(SourceGraph::Expression(graph)), None),
			LiveSource::Graph(graph) => (
				self.root.schema.classes().map(|class| class.name().clone()).collect(),
				Some(SourceGraph::Reference(self.current_graph(graph)?)),
				None,
			),
			LiveSource::GraphExpr(graph) => (
				self.root.schema.classes().map(|class| class.name().clone()).collect(),
				Some(SourceGraph::Expression(graph)),
				None,
			),
		})
	}

	/// Builds one source in the original query evaluation order.
	fn atom_source(
		&mut self,
		class: Option<&Text>,
		graph: Option<&SourceGraph<'query>>,
		record: Option<&crate::values::RecordId>,
		alias: Option<&str>,
	) -> FerrosGrynnResult<SelectSource> {
		let kind = if let Some(record) = record {
			let class = class.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("LIVE point source has no class")
			})?;
			self.admit(
				record
					.estimated_heap_bytes()
					.checked_add(std::mem::size_of_val(record))
					.ok_or_else(|| {
						FerrosGrynnError::operation_overflow("LIVE point source literal")
					})?,
			)?;
			let literal = self.literal(&Value::Record(Box::new(record.clone())))?;
			let ExprKind::Parameter(parameter) = literal.kind else {
				return Err(FerrosGrynnError::invalid_storage_file(
					"LIVE point literal has no immutable slot",
				));
			};
			SelectSourceKind::Atoms(SelectTarget::BoundRecord {
				class: identifier(class.as_str()),
				parameter,
			})
		} else {
			class.map_or(SelectSourceKind::Empty, |class| {
				SelectSourceKind::Atoms(SelectTarget::Class(identifier(class.as_str())))
			})
		};
		Ok({
			let split_field_0 = kind;
			let split_field_1 = graph
				.map(|graph| match graph {
					SourceGraph::Reference(graph) => {
						self.literal(&Value::GraphRef(Box::new(graph.clone())))
					}
					SourceGraph::Expression(expression) => self.expression(expression),
				})
				.transpose()?;
			let split_field_2 = alias.or(self.current_alias.as_deref()).map(identifier);
			let split_field_3 = span();
			crate::syn::split_internal::new_select_source(
				split_field_0,
				split_field_1,
				split_field_2,
				split_field_3,
			)
		})
	}

	/// Lowers one lexical scope and restores the parent on every outcome.
	fn selects_inner(
		&mut self,
		query: &'query LiveQuery,
	) -> FerrosGrynnResult<Vec<SelectStatement>> {
		self.multi_source = !query.joins().is_empty();
		self.admit(128)?;
		self.current_alias = Some(format!("__fg_live_source_{}", self.next_scope).into_boxed_str());
		self.current_logical_alias = query.alias();
		if let Some(alias) = self.current_logical_alias {
			self.admit_name(alias.as_str())?;
		}
		self.next_scope = self
			.next_scope
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE lexical source scopes"))?;
		let (classes, graph, record) = self.source_choices(query.source())?;
		self.admit_array::<JoinInput<'query>>(query.joins().len())?;
		let mut join_inputs = Vec::with_capacity(query.joins().len());
		let mut variant_count = 1_usize;
		let mut family = false;
		for join in query.joins() {
			self.admit_name(join.alias().as_str())?;
			let (mut classes, graph, record) = self.source_choices(join.source())?;
			classes.sort();
			let polymorphic = classes.len() != 1;
			family |= polymorphic;
			let choices = classes
				.len()
				.max(1)
				.checked_add(usize::from(
					polymorphic
						&& !classes.is_empty()
						&& join.kind() == crate::live::LiveJoinKind::Left,
				))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE JOIN typing choices"))?;
			variant_count = variant_count
				.checked_mul(choices)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE JOIN typing product"))?;
			for class in &classes {
				let schema = self.root.schema.class(class.as_str()).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE JOIN schema missing")
				})?;
				self.admit_source_alias(join.alias().as_str(), &schema)?;
				self.admit_schema(&schema)?;
				self.admit_array::<std::sync::Arc<crate::datastore::ClassSchema>>(1)?;
				self.join_schemas.push(schema);
			}
			join_inputs.push((classes, graph, record, Some(0)));
		}
		let mut statements = Vec::new();
		let classes = if classes.is_empty() {
			vec![None]
		} else {
			classes.into_iter().map(Some).collect()
		};
		for class in classes {
			self.current_schema = None;
			if let Some(class) = &class {
				let schema = self.root.schema.class(class.as_str()).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE source schema is missing")
				})?;
				self.admit_schema(&schema)?;
				if self.multi_source
					&& let Some(alias) = query.alias()
				{
					self.admit_source_alias(alias.as_str(), &schema)?;
				}
				self.current_schema = Some(schema);
			}
			if !family {
				statements.push(self.select_statement(
					query,
					class.as_ref(),
					graph.as_ref(),
					record,
					&join_inputs,
				)?);
				continue;
			}
			self.work(u64::try_from(variant_count).map_err(|_error| {
				FerrosGrynnError::operation_overflow("LIVE JOIN variant work")
			})?)?;
			self.admit_array::<SelectStatement>(variant_count)?;
			let mut variants = Vec::with_capacity(variant_count);
			for ordinal in 0..variant_count {
				let mut remaining = ordinal;
				for (join, (classes, _, _, typing)) in query.joins().iter().zip(&mut join_inputs) {
					let choices = classes.len().max(1)
						+ usize::from(
							classes.len() > 1 && join.kind() == crate::live::LiveJoinKind::Left,
						);
					let choice = remaining % choices;
					remaining /= choices;
					*typing = (choice < classes.len()).then_some(choice);
				}
				let mut variant = self.select_statement(
					query,
					class.as_ref(),
					graph.as_ref(),
					record,
					&join_inputs,
				)?;
				variant.set_union_purpose(crate::syn::SelectUnionPurpose::JoinedTypingVariants);
				variants.push(variant);
			}
			// Rebuild the wrapper through the same admitted factory.
			let mut wrapper =
				self.select_statement(query, class.as_ref(), graph.as_ref(), record, &join_inputs)?;
			wrapper.set_union_purpose(crate::syn::SelectUnionPurpose::JoinedTypingVariants);
			wrapper.set_union_inputs(variants);
			statements.push(wrapper);
		}
		Ok(statements)
	}

	/// Builds one typed SELECT in the original evaluation order.
	fn graph_expression(&mut self, expression: &'query LiveExpr) -> FerrosGrynnResult<Expr> {
		let previous_schema = self.current_schema.take();
		let previous_logical_alias = self.current_logical_alias.take();
		let previous_multi_source = std::mem::replace(&mut self.multi_source, true);
		let result = self.expression(expression);
		self.current_schema = previous_schema;
		self.current_logical_alias = previous_logical_alias;
		self.multi_source = previous_multi_source;
		result
	}

	/// Graph selectors use result aliases while parameter names retain their
	/// lexical owner.
	fn graph_terminal(
		&mut self,
		graph: &'query crate::live::LiveGraphResult,
	) -> FerrosGrynnResult<crate::syn::ReturnGraph> {
		use crate::{
			graph::GraphKind,
			syn::{GraphFamilyName, GraphValidatorName, ReturnGraph},
		};
		self.admit(2 * std::mem::size_of::<ReturnGraph>())?;
		self.admit_array::<GraphValidatorName>(graph.validators().iter().count())?;
		let family = match graph.family() {
			GraphKind::Graph => Some(GraphFamilyName::Graph),
			GraphKind::Tree => Some(GraphFamilyName::Tree),
			GraphKind::RootedTree => Some(GraphFamilyName::RootedTree),
			GraphKind::Forest => Some(GraphFamilyName::Forest),
			GraphKind::Dag => Some(GraphFamilyName::Dag),
			GraphKind::Hypergraph => Some(GraphFamilyName::Hyper),
			GraphKind::Metagraph => Some(GraphFamilyName::Meta),
			GraphKind::Subgraph => None,

			_ => {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"Unsupported runtime variant at the workspace boundary.",
				));
			}
		};
		Ok({
			let split_field_0 = None;
			let split_field_1 = Some({
				let split_field_0 = family;
				let split_field_1 = graph
					.validators()
					.iter()
					.map(|validator| {
						fg_runtime::__internal::EnumChoice::choose(&(validator), [
							GraphValidatorName::Simple,
							GraphValidatorName::Bipartite,
							GraphValidatorName::Complete,
						])
					})
					.collect();
				let split_field_2 = span();
				crate::syn::split_internal::new_graph_contract(
					split_field_0,
					split_field_1,
					split_field_2,
				)
			});
			let split_field_2 = self.graph_expression(graph.nodes())?;
			let split_field_3 = self.graph_expression(graph.edges())?;
			let split_field_4 = graph.root().map(|root| self.graph_expression(root)).transpose()?;
			let split_field_5 = span();
			crate::syn::split_internal::new_return_graph(
				split_field_0,
				split_field_1,
				split_field_4,
				split_field_2,
				split_field_3,
				split_field_5,
			)
		})
	}

	/// Builds one typed SELECT in the original evaluation order.
	fn select_statement(
		&mut self,
		query: &'query LiveQuery,
		class: Option<&Text>,
		graph: Option<&SourceGraph<'query>>,
		record: Option<&'query crate::values::RecordId>,
		join_inputs: &[JoinInput<'query>],
	) -> FerrosGrynnResult<SelectStatement> {
		let fields = match query.projection() {
			LiveProjection::Fields(fields) => fields.len(),
			_ => 1,
		};
		self.admit_array::<SelectJoin>(query.joins().len())?;
		for (join, (classes, _, _, _)) in query.joins().iter().zip(join_inputs) {
			for class in classes {
				let schema = self.root.schema.class(class.as_str()).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE JOIN schema missing")
				})?;
				self.admit_source_alias(join.alias().as_str(), &schema)?;
				self.admit_schema(&schema)?;
			}
		}
		self.admit_array::<SelectItem>(fields)?;
		self.admit_array::<SelectPath>(query.omit().len())?;
		self.admit_array::<SelectPath>(query.fetch().len())?;
		self.admit_array::<SelectOrder>(query.order().len())?;
		let filter = query.filter().map(|expression| self.expression(expression)).transpose()?;
		let projection = match query.projection() {
			LiveProjection::Records => vec![SelectItem::Wildcard(span())],
			LiveProjection::Value(expression) => vec![SelectItem::Expr {
				expr: self.expression(expression)?,
				alias: None,
			}],
			LiveProjection::Fields(fields) => fields
				.iter()
				.map(|field| {
					let expr = self.expression(&field.expression)?;
					Ok(if let Some(alias) = &field.alias {
						SelectItem::PathExpr {
							expr,
							alias: self.path(alias)?,
						}
					} else {
						SelectItem::Expr {
							expr,
							alias: None,
						}
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?,
		};
		self.admit_array::<Expr>(query.split().len())?;
		let split = query
			.split()
			.iter()
			.map(|value| self.expression(value))
			.collect::<FerrosGrynnResult<_>>()?;
		Ok({
			let split_field_0 = None;
			let split_field_1 = query
				.graph_result()
				.map(|graph| self.graph_terminal(graph).map(Box::new))
				.transpose()?;
			let split_field_2 = match query.group() {
				None => None,
				Some([]) => Some(SelectGroup::All(span())),
				Some(keys) => {
					self.admit_array::<Expr>(keys.len())?;
					Some(SelectGroup::By(
						keys.iter()
							.map(|key| self.expression(key))
							.collect::<FerrosGrynnResult<_>>()?,
					))
				}
			};
			let split_field_3 = Vec::new();
			let split_field_4 = crate::syn::SelectUnionPurpose::IndependentOutputs;
			let split_field_5 = None;
			let split_field_6 = query.only();
			let split_field_7 = matches!(query.projection(), LiveProjection::Value(_));
			let split_field_8 = projection;
			let split_field_9 = self.atom_source(
				class,
				graph,
				record,
				if self.multi_source {
					query.alias().map(Text::as_str).or(class.map(Text::as_str))
				} else {
					None
				},
			)?;
			let split_field_10 = query
				.joins()
				.iter()
				.zip(join_inputs)
				.map(|(join, (classes, graph, record, typing))| {
					Ok({
						let split_field_0 = match join.kind() {
							crate::live::LiveJoinKind::Inner => SelectJoinKind::Inner,
							crate::live::LiveJoinKind::Left => SelectJoinKind::Left,
							crate::live::LiveJoinKind::Cross => SelectJoinKind::Cross,
						};
						let split_field_1 = if classes.len() == 1 {
							self.atom_source(
								classes.first(),
								graph.as_ref(),
								*record,
								Some(join.alias().as_str()),
							)?
						} else {
							self.admit_array::<Identifier>(classes.len())?;
							for class in classes {
								self.admit_name(class.as_str())?;
							}
							let mut source = self.atom_source(
								None,
								graph.as_ref(),
								None,
								Some(join.alias().as_str()),
							)?;
							source.kind = SelectSourceKind::AtomUnion {
								classes: classes
									.iter()
									.map(|class| identifier(class.as_str()))
									.collect(),
								typing: *typing,
							};
							source
						};
						let split_field_2 =
							join.predicate().map(|value| self.expression(value)).transpose()?;
						let split_field_3 = span();
						crate::syn::split_internal::new_select_join(
							split_field_0,
							split_field_1,
							split_field_2,
							split_field_3,
						)
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let split_field_11 = None;
			let split_field_12 = filter;
			let split_field_13 = split;
			let split_field_14 = query
				.omit()
				.iter()
				.map(|path| self.path(path))
				.collect::<FerrosGrynnResult<_>>()?;
			let split_field_15 = query
				.fetch()
				.iter()
				.map(|path| self.path(path))
				.collect::<FerrosGrynnResult<_>>()?;
			let split_field_16 = query
				.order()
				.iter()
				.map(|key| self.order(key))
				.collect::<FerrosGrynnResult<_>>()?;
			let split_field_17 = query.limit().map(|value| self.expression(value)).transpose()?;
			let split_field_18 = query
				.start_offset()
				.map(|value| self.expression(value).map(SelectStart::Offset))
				.transpose()?;
			let split_field_19 = false;
			crate::syn::split_internal::new_select_statement(
				split_field_3,
				split_field_4,
				split_field_5,
				split_field_6,
				split_field_7,
				split_field_8,
				split_field_9,
				split_field_10,
				split_field_11,
				split_field_12,
				split_field_13,
				split_field_2,
				split_field_14,
				split_field_15,
				split_field_0,
				split_field_16,
				split_field_17,
				split_field_18,
				split_field_19,
				split_field_1,
			)
		})
	}

	/// Bare names preserve projected alias resolution in the ordinary analyzer.
	fn order(&mut self, order: &'query LiveOrder) -> FerrosGrynnResult<SelectOrder> {
		self.work(1)?;
		let expression = match order.key() {
			LiveOrderKey::Expression(value) => self.expression(value)?,
			LiveOrderKey::Name(name) => {
				self.admit_name(name.as_str())?;
				{
					let split_field_0 = ExprKind::Name(identifier(name.as_str()));
					let split_field_1 = span();
					crate::syn::split_internal::new_expr(split_field_0, split_field_1)
				}
			}
		};
		Ok({
			let split_field_0 = expression;
			let split_field_1 = false;
			let split_field_2 = order.collation();
			let split_field_3 = order.numeric();
			let split_field_4 = match order.direction() {
				LiveOrderDirection::Ascending => SelectOrderDirection::Ascending,
				LiveOrderDirection::Descending => SelectOrderDirection::Descending,
			};
			let split_field_5 = match order.nulls() {
				LiveNullsOrder::First => SelectNulls::First,
				LiveNullsOrder::Last => SelectNulls::Last,
			};
			crate::syn::split_internal::new_select_order(
				split_field_0,
				split_field_1,
				split_field_2,
				split_field_3,
				split_field_4,
				split_field_5,
			)
		})
	}

	/// Resolves identity to its current spelling without rebinding another
	/// class.
	fn class_name(&mut self, class: crate::datastore::ClassId) -> FerrosGrynnResult<Text> {
		let schema =
			self.root.schema.class_by_id(class).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("LIVE class identity is missing")
			})?;
		self.admit_name(schema.name().as_str())?;
		Ok(schema.name().clone())
	}

	/// Source renames preserve the captured GraphId.
	fn current_graph(
		&mut self,
		graph: &crate::graph::GraphRef,
	) -> FerrosGrynnResult<crate::graph::GraphRef> {
		self.work(1)?;
		let current =
			self.root.schema.graph(graph.id()).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("LIVE graph source was removed")
			})?;
		self.admit(
			current.reference().name().len() + std::mem::size_of::<crate::graph::GraphRef>(),
		)?;
		Ok(current.reference().clone())
	}

	/// Output paths validate admission before copying literal segments.
	fn path(&mut self, path: &LivePath) -> FerrosGrynnResult<SelectPath> {
		self.work(path.segments().len() as u64)?;
		self.admit_path(path)?;
		Ok({
			let split_field_0 =
				path.segments().iter().map(|field| identifier(field.as_str())).collect();
			let split_field_1 = span();
			crate::syn::split_internal::new_select_path(split_field_0, split_field_1)
		})
	}

	/// Literals use owned parameter values, preserving every Number
	/// representation.
	fn literal(&mut self, value: &Value) -> FerrosGrynnResult<Expr> {
		let comparison_work =
			self.parameters.len().checked_mul(value.estimated_heap_bytes() / 64 + 1).ok_or_else(
				|| FerrosGrynnError::operation_overflow("LIVE literal comparison work"),
			)?;
		self.work(u64::try_from(comparison_work).map_err(|_error| {
			FerrosGrynnError::operation_overflow("LIVE literal comparison work")
		})?)?;
		self.admit(
			value
				.estimated_heap_bytes()
				.checked_add(128 + std::mem::size_of::<Value>())
				.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE literal size overflows"))?,
		)?;
		if let Some((slot, _)) = self.parameters.iter().find(|(name, existing)| {
			name.as_str().starts_with("__fg_live_literal_") && same_literal(existing, value)
		}) {
			return Ok({
				let split_field_0 = ExprKind::Parameter(identifier(slot.as_str()));
				let split_field_1 = span();
				crate::syn::split_internal::new_expr(split_field_0, split_field_1)
			});
		}
		loop {
			let slot = format!("__fg_live_literal_{}", self.next_literal);
			self.next_literal += 1;
			if self.parameters.get(slot.as_str()).is_none() {
				self.parameters.insert(slot.as_str(), value.clone());
				return Ok({
					let split_field_0 = ExprKind::Parameter(identifier(&slot));
					let split_field_1 = span();
					crate::syn::split_internal::new_expr(split_field_0, split_field_1)
				});
			}
		}
	}

	/// Keeps lexical and reactive identities separate even when values agree.
	fn captured_literal(&mut self, value: &Value, slot: &str) -> FerrosGrynnResult<Expr> {
		self.work(1)?;
		self.admit_name(slot)?;
		if self.parameters.get(slot).is_none() {
			self.admit(
				value
					.estimated_heap_bytes()
					.checked_add(128 + std::mem::size_of::<Value>())
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE capture slot"))?,
			)?;
			self.parameters.insert(slot, value.clone());
		}
		Ok({
			let split_field_0 = ExprKind::Parameter(identifier(slot));
			let split_field_1 = span();
			crate::syn::split_internal::new_expr(split_field_0, split_field_1)
		})
	}

	/// Restores depth after translating the complete expression tree.
	fn expression(&mut self, expression: &'query LiveExpr) -> FerrosGrynnResult<Expr> {
		if self.expression_depth >= 64 {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE expression depth",
				64,
			));
		}
		self.expression_depth += 1;
		let result = self.expression_inner(expression);
		self.expression_depth -= 1;
		result
	}

	/// Qualifies implicit/current fields without exposing synthetic SDK names.
	fn source_field(&mut self, field: &str) -> FerrosGrynnResult<Expr> {
		self.admit(2 * std::mem::size_of::<Expr>() + 128)?;
		self.admit_name(field)?;
		if self.multi_source {
			Ok(name(field))
		} else if let Some(alias) = &self.current_alias {
			let bytes = alias.len();
			self.admit(
				bytes
					.checked_mul(4)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE source alias"))?,
			)?;
			let alias = self
				.current_alias
				.as_deref()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE source alias was lost"))?;
			Ok({
				let split_field_0 = ExprKind::Field {
					base: Box::new(name(alias)),
					field: identifier(field),
					null_safe: false,
				};
				let split_field_1 = span();
				crate::syn::split_internal::new_expr(split_field_0, split_field_1)
			})
		} else {
			Ok(name(field))
		}
	}

	/// Restores ancestor depth through success, early returns and errors.
	fn expression_inner(&mut self, expression: &'query LiveExpr) -> FerrosGrynnResult<Expr> {
		self.work(1)?;
		self.admit(2 * std::mem::size_of::<Expr>())?;
		let kind = match expression {
			LiveExpr::Range {
				start,
				end,
				inclusive_end,
			} => ExprKind::Range {
				start: start
					.as_deref()
					.map(|value| self.expression(value).map(Box::new))
					.transpose()?,
				end: end
					.as_deref()
					.map(|value| self.expression(value).map(Box::new))
					.transpose()?,
				inclusive_end: *inclusive_end,
			},
			LiveExpr::Cast {
				target,
				value,
			} => {
				crate::live::admit_cast_type_name(
					target.as_str().as_bytes(),
					self.expression_depth + 1,
					&mut |bytes, work| {
						self.work(work)?;
						self.admit(bytes)
					},
				)?;
				ExprKind::Cast {
					target: {
						let split_field_0 = target.as_str().into();
						let split_field_1 = span();
						crate::syn::split_internal::new_type_name(split_field_0, split_field_1)
					},
					expr: Box::new(self.expression(value)?),
				}
			}
			LiveExpr::Literal(value) => return self.literal(value),
			LiveExpr::Field(path) => {
				self.admit_path(path)?;
				self.admit_field_type(path)?;
				let segments = if self.multi_source {
					path.segments()
				} else {
					source_field_segments(
						path,
						self.current_schema.as_deref(),
						self.current_logical_alias,
					)
				};
				let mut fields = segments.iter();
				let first = fields
					.next()
					.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE path is empty"))?;
				let mut expression = self.source_field(first.as_str())?;
				for field in fields {
					expression = {
						let split_field_0 = ExprKind::Field {
							base: Box::new(expression),
							field: identifier(field.as_str()),
							null_safe: false,
						};
						let split_field_1 = span();
						crate::syn::split_internal::new_expr(split_field_0, split_field_1)
					};
				}
				return Ok(expression);
			}
			LiveExpr::Parameter(parameter) => {
				let value = self
					.captures
					.iter()
					.rev()
					.find_map(|captures| captures.get(parameter.as_str()))
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument(format!(
							"LIVE parameter `${parameter}` has no captured value"
						))
					})?;
				self.admit_name(parameter.as_str())?;
				let alias_bytes = self.current_alias.as_deref().map_or(7, str::len);
				self.admit(
					alias_bytes
						.checked_mul(2)
						.and_then(|bytes| bytes.checked_add(64))
						.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE capture name"))?,
				)?;
				let slot = format!(
					"__fg_live_parameter_{}_{}",
					self.current_alias.as_deref().unwrap_or("binding"),
					parameter
				);
				return self.captured_literal(value, &slot);
			}
			LiveExpr::Binding(id) => {
				self.admit(128)?;
				let slot = format!("__fg_live_binding_{}", id.as_uuid());
				return self.captured_literal((self.bindings)(*id)?, &slot);
			}
			LiveExpr::Access {
				base,
				field,
				null_safe,
			} => {
				self.admit_name(field.as_str())?;
				ExprKind::Field {
					base: Box::new(self.expression(base)?),
					field: identifier(field.as_str()),
					null_safe: *null_safe,
				}
			}
			LiveExpr::Index {
				base,
				index,
			} => ExprKind::Index {
				base: Box::new(self.expression(base)?),
				index: Box::new(self.expression(index)?),
			},
			LiveExpr::Array(values) | LiveExpr::Set(values) => {
				self.work(values.len() as u64)?;
				self.admit_array::<Expr>(values.len())?;
				let values = values
					.iter()
					.map(|value| self.expression(value))
					.collect::<FerrosGrynnResult<_>>()?;
				if matches!(expression, LiveExpr::Set(_)) {
					ExprKind::Set(values)
				} else {
					ExprKind::Array(values)
				}
			}
			LiveExpr::Object(fields) => {
				self.work(fields.len() as u64)?;
				self.admit_array::<ObjectField>(fields.len())?;
				let mut output = Vec::with_capacity(fields.len());
				for (key, value) in fields {
					self.admit_name(key.as_str())?;
					output.push({
						let split_field_0 = identifier(key.as_str());
						let split_field_1 = self.expression(value)?;
						let split_field_2 = span();
						crate::syn::split_internal::new_object_field(
							split_field_0,
							split_field_1,
							split_field_2,
						)
					});
				}
				ExprKind::Object(output)
			}
			LiveExpr::Negate(expression) => ExprKind::Unary {
				op: UnaryOperator::Negate,
				expr: Box::new(self.expression(expression)?),
			},
			LiveExpr::Positive(expression) => ExprKind::Unary {
				op: UnaryOperator::Positive,
				expr: Box::new(self.expression(expression)?),
			},
			LiveExpr::Not(expression) => ExprKind::Unary {
				op: UnaryOperator::Not,
				expr: Box::new(self.expression(expression)?),
			},
			LiveExpr::Binary {
				left,
				op,
				right,
			} => ExprKind::Binary {
				left: Box::new(self.expression(left)?),
				op: operator(*op),
				right: Box::new(self.expression(right)?),
			},
			LiveExpr::Conditional {
				condition,
				then_value,
				else_value,
			} => ExprKind::Conditional {
				condition: Box::new(self.expression(condition)?),
				then_value: Box::new(self.expression(then_value)?),
				else_value: Box::new(self.expression(else_value)?),
			},
			LiveExpr::Case {
				operand,
				branches,
				else_value,
			} => {
				if branches.is_empty() {
					return Err(FerrosGrynnError::invalid_argument("LIVE CASE requires a branch"));
				}
				let count = branches
					.len()
					.checked_mul(2)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE CASE branches"))?;
				self.work(count as u64)?;
				self.admit_array::<(Expr, Expr)>(branches.len())?;
				ExprKind::Case {
					operand: operand
						.as_deref()
						.map(|value| self.expression(value).map(Box::new))
						.transpose()?,
					branches: branches
						.iter()
						.map(|(condition, value)| {
							Ok((self.expression(condition)?, self.expression(value)?))
						})
						.collect::<FerrosGrynnResult<_>>()?,
					else_value: else_value
						.as_deref()
						.map(|value| self.expression(value).map(Box::new))
						.transpose()?,
				}
			}
			LiveExpr::Call {
				name: function,
				arguments,
			} => self.call(function, None, false, arguments.iter().map(|value| (None, value)))?,
			LiveExpr::CallWithOptions {
				name,
				type_argument,
				distinct,
				arguments,
			} => self.call(
				name,
				type_argument.as_ref(),
				*distinct,
				arguments.iter().map(|(name, value)| (name.as_ref(), value)),
			)?,
			LiveExpr::Select(query) => {
				let capture_owner = self.next_scope;
				let mut selects = self.selects(query)?;
				let address = std::ptr::from_ref(query.as_ref());
				let id = if let Some(id) = self.independent_queries.get(&address) {
					*id
				} else {
					self.admit(128)?;
					let id = u32::try_from(self.independent_queries.len()).map_err(|_error| {
						FerrosGrynnError::operation_overflow("LIVE independent subqueries")
					})?;
					self.independent_queries.insert(address, id);
					id
				};
				let select = if selects.len() == 1 {
					let mut select = selects.remove(0);
					select.set_lexical_id(Some(id));
					select
				} else {
					self.admit(2 * std::mem::size_of::<SelectStatement>())?;
					let mut select = SelectStatement::independent_union(selects, id);
					select.only = query.only();
					if let LiveSource::GraphExpr(graph) = query.source() {
						// Public SQL aliases are independent of the unique
						// owner of this nested query's parameter frame.
						self.admit(128)?;
						let alias = format!("__fg_live_source_{capture_owner}").into_boxed_str();
						let previous_alias = self.current_alias.replace(alias);
						self.captures.push(query.parameters());
						let scope = self.expression(graph);
						self.captures.pop();
						self.current_alias = previous_alias;
						select.source.graph = Some(scope?);
					}
					select
				};
				ExprKind::Subquery(Box::new(select))
			}
		};
		Ok({
			let split_field_0 = kind;
			let split_field_1 = span();
			crate::syn::split_internal::new_expr(split_field_0, split_field_1)
		})
	}

	/// Preserves supplied call order until ordinary signature mapping and
	/// admits names/types before constructing compiler-owned metadata.
	fn call<Arguments>(
		&mut self,
		function: &Text,
		type_argument: Option<&Text>,
		distinct: bool,
		arguments: Arguments,
	) -> FerrosGrynnResult<ExprKind>
	where
		Arguments: ExactSizeIterator<Item = (Option<&'query Text>, &'query LiveExpr)>,
	{
		self.admit_name(function.as_str())?;
		if let Some((namespace, name)) = function.as_str().split_once("::") {
			for definition in self.root.schema.functions() {
				self.work(1)?;
				if definition.namespace().as_str().eq_ignore_ascii_case(namespace)
					&& definition.name().as_str().eq_ignore_ascii_case(name)
				{
					self.admit_expression_type(definition.result())?;
				}
			}
		}
		let type_argument = type_argument
			.map(|target| -> FerrosGrynnResult<crate::syn::TypeName> {
				crate::live::admit_cast_type_name(
					target.as_str().as_bytes(),
					self.expression_depth + 1,
					&mut |bytes, work| {
						self.work(work)?;
						self.admit(bytes)
					},
				)?;
				Ok({
					let split_field_0 = target.as_str().into();
					let split_field_1 = span();
					crate::syn::split_internal::new_type_name(split_field_0, split_field_1)
				})
			})
			.transpose()?;
		self.admit_array::<Expr>(arguments.len())?;
		self.admit_array::<Option<Identifier>>(arguments.len())?;
		self.admit_array::<Expr>(2)?;
		let callee = if let Some((namespace, function)) = function.as_str().split_once("::") {
			{
				let split_field_0 = ExprKind::Field {
					base: Box::new(name(namespace)),
					field: identifier(function),
					null_safe: false,
				};
				let split_field_1 = span();
				crate::syn::split_internal::new_expr(split_field_0, split_field_1)
			}
		} else {
			name(function.as_str())
		};
		let mut argument_names = Vec::with_capacity(arguments.len());
		let mut values = Vec::with_capacity(arguments.len());
		let mut named_tail = false;
		for (name, value) in arguments {
			if name.is_none() && named_tail {
				return Err(FerrosGrynnError::invalid_argument(
					"Positional arguments cannot follow named arguments.",
				));
			}
			named_tail |= name.is_some();
			let name = name
				.map(|name| -> FerrosGrynnResult<Identifier> {
					self.admit_name(name.as_str())?;
					Ok(identifier(name.as_str()))
				})
				.transpose()?;
			argument_names.push(name);
			values.push(self.expression(value)?);
		}
		Ok(ExprKind::Call {
			callee: Box::new(callee),
			type_argument,
			distinct,
			argument_names,
			arguments: values,
		})
	}
}

/// Slot reuse preserves observable representation, including nested graph
/// descriptors whose identity equality intentionally omits their spelling.
fn same_literal(left: &Value, right: &Value) -> bool {
	match (left, right) {
		(Value::GraphRef(left), Value::GraphRef(right)) => {
			left.id() == right.id()
				&& left.name() == right.name()
				&& left.kind() == right.kind()
				&& left.storage_mode() == right.storage_mode()
		}
		(Value::Array(left), Value::Array(right)) => {
			left.len() == right.len()
				&& left.iter().zip(right).all(|(left, right)| same_literal(left, right))
		}
		(Value::Set(left), Value::Set(right)) => {
			left.len() == right.len()
				&& left.iter().zip(right).all(|(left, right)| same_literal(left, right))
		}
		(Value::Object(left), Value::Object(right)) => {
			left.len() == right.len()
				&& left.iter().zip(right).all(|((left_key, left), (right_key, right))| {
					left_key == right_key && same_literal(left, right)
				})
		}
		(Value::Range(left), Value::Range(right)) => {
			same_literal_bound(left.start(), right.start())
				&& same_literal_bound(left.end(), right.end())
		}
		_ => crate::values::runtime::same_value(left, right),
	}
}

/// Range markers and qualified endpoints both participate in slot identity.
fn same_literal_bound(left: std::ops::Bound<&Value>, right: std::ops::Bound<&Value>) -> bool {
	match (left, right) {
		(std::ops::Bound::Unbounded, std::ops::Bound::Unbounded) => true,
		(std::ops::Bound::Included(left), std::ops::Bound::Included(right))
		| (std::ops::Bound::Excluded(left), std::ops::Bound::Excluded(right)) => {
			same_literal(left, right)
		}
		_ => false,
	}
}

/// Owned value and concrete transitive function versions from one evaluation.
pub struct BindingEvaluation {
	/// Exact result from the shared typed evaluator.
	pub(crate) value: Value,
	/// Sorted distinct versions compiled from the same candidate root.
	pub(crate) functions: Vec<(crate::datastore::FunctionId, crate::datastore::SchemaVersion)>,
	/// Concrete owners of nested record sources; class recreation never
	/// rebinds.
	pub(crate) record_sources: Vec<(Text, crate::datastore::ClassId)>,
	/// Work already consumed by lowering, compilation, reads and nested PURE
	/// calls.
	pub(crate) work_used: u64,
}

/// Runs a logical reactive expression through the ordinary typed SELECT
/// evaluator.
pub fn evaluate_binding<'root>(
	root: &'root std::sync::Arc<DatabaseRoot>,
	expression: &LiveExpr,
	parameters: &Object,
	bindings: &'root dyn Fn(crate::live::LiveBindingId) -> FerrosGrynnResult<&'root Value>,
	options: &crate::live::LiveBindingOptions,
	scratch_root: std::path::PathBuf,
	observe: &dyn Fn(crate::datastore::ConflictKey),
) -> FerrosGrynnResult<BindingEvaluation> {
	evaluate_binding_controlled(
		root,
		expression,
		parameters,
		bindings,
		options,
		scratch_root,
		observe,
		None,
	)
}

/// Registration inherits interruption while durable replay uses its own bounds.
#[allow(
	clippy::too_many_arguments,
	reason = "one fixed root binds recipe, inputs, budgets and caller cancellation"
)]
pub fn evaluate_binding_controlled<'root>(
	root: &'root std::sync::Arc<DatabaseRoot>,
	expression: &LiveExpr,
	parameters: &Object,
	bindings: &'root dyn Fn(crate::live::LiveBindingId) -> FerrosGrynnResult<&'root Value>,
	options: &crate::live::LiveBindingOptions,
	scratch_root: std::path::PathBuf,
	observe: &dyn Fn(crate::datastore::ConflictKey),
	parent: Option<&super::ExecutionControl>,
) -> FerrosGrynnResult<BindingEvaluation> {
	use std::cell::Cell;
	let mut lowering = Lowering {
		parent: parent.map(|control| control.child(None)),
		root,
		current_schema: None,
		parameters: Object::new(),
		next_literal: 0,
		captures: vec![parameters],
		account: super::options::BarrierAccount::new(options.scratch_bytes()),
		maximum_scratch: options.scratch_bytes(),
		admitted_scratch: 0,
		expression_depth: 0,
		remaining_work: options.work(),
		bindings,
		record_sources: std::collections::BTreeMap::new(),
		class_namespace: false,
		current_alias: None,
		current_logical_alias: None,
		multi_source: false,
		join_schemas: Vec::new(),
		next_scope: 0,
		independent_queries: std::collections::BTreeMap::new(),
	};
	let expression = lowering.expression(expression)?;
	let (cache, control) = evaluation_runtime(&lowering, options.work())?;
	let remaining_work = Cell::new(lowering.remaining_work);
	let work = |amount: u64| -> FerrosGrynnResult<()> {
		remaining_work.set(remaining_work.get().checked_sub(amount).ok_or_else(|| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE evaluation work",
				options.work(),
			)
		})?);
		Ok(())
	};
	let read = super::execution::RootRead::new(root, &control, observe, &work, scratch_root);
	if lowering.class_namespace {
		observe(crate::datastore::ConflictKey::ClassCatalogRange);
	}
	let program = super::analysis::analyze_reactive_expression(
		&expression,
		&read,
		&lowering.parameters,
		&cache,
	)?;
	let compile_work = cache
		.remaining_compile_work()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE compiler work is not bounded"))?;
	work(lowering.remaining_work - compile_work)?;
	let Some(super::ir::LogicalStatement::Expression {
		value,
	}) = program.statements.first()
	else {
		return Err(FerrosGrynnError::invalid_argument("LIVE LET typed expression is missing"));
	};
	super::optimizer::validate_deterministic_statement(&program.statements[0])?;
	let fields = read.fields();
	fields.admit(
		program
			.slot_count
			.checked_mul(std::mem::size_of::<Value>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE slots"))?,
	)?;
	for value in lowering.parameters.values() {
		fields.admit_value(value)?;
	}
	let slots = super::execution::initialize_slots(&program, &lowering.parameters);
	let value = super::execution::evaluate_root_expression(&read, value, &slots, &control)?;
	let count = cache.named_dependency_count();
	fields.work(count as u64)?;
	fields.admit(
		count
			.checked_mul(192)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE function dependencies"))?,
	)?;
	let functions = cache.named_dependencies().into_iter().collect();
	fields.admit(
		lowering
			.record_sources
			.len()
			.checked_mul(std::mem::size_of::<(Text, crate::datastore::ClassId)>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE source owners"))?,
	)?;
	let record_sources = lowering.record_sources.into_iter().collect();
	Ok(BindingEvaluation {
		value,
		functions,
		record_sources,
		work_used: options.work() - remaining_work.get(),
	})
}

/// Binding and query consumers share bounded compiler/runtime allocation.
fn evaluation_runtime(
	lowering: &Lowering<'_, '_>,
	work: u64,
) -> FerrosGrynnResult<(
	std::sync::Arc<super::closure::LanguageClosureState>,
	super::options::ExecutionControl,
)> {
	let remaining = lowering
		.maximum_scratch
		.checked_sub(lowering.admitted_scratch)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE compiler scratch"))?;
	let compiler = remaining / 2;
	let runtime = std::num::NonZeroUsize::new(
		(remaining - compiler).min(super::options::DEFAULT_BARRIER_MEMORY_BYTES),
	)
	.ok_or_else(|| {
		FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE runtime scratch",
			lowering.maximum_scratch as u64,
		)
	})?;
	let cache = std::sync::Arc::new(
		super::closure::LanguageClosureState::bounded_with_work(compiler, lowering.remaining_work)
			.with_compile_checks(
				lowering.parent.as_ref().map(super::ExecutionControl::interruption),
			),
	);
	let execution = super::QueryExecutionOptions {
		barrier_memory_bytes: runtime,
		max_loop_iterations: work,
		max_path_expansions: work,
		..super::QueryExecutionOptions::default()
	};
	let control =
		super::options::ExecutionControl::with_runtime(&execution, None, None, cache.clone());
	let control = if let Some(parent) = &lowering.parent {
		control.inherit_checks(parent)
	} else {
		control
	};
	Ok((cache, control))
}

/// Admits borrowed rich types for both typed lowering and root planner
/// metadata.
pub(super) fn admit_schema_shape(
	schema: &crate::datastore::ClassSchema,
	admit: &mut impl FnMut(usize, u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	// Textual conversion and SDK lowering reserve ordinary schema copies.
	admit(
		schema
			.name()
			.len()
			.checked_mul(16)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE schema source name"))?,
		schema.fields().len() as u64,
	)?;
	let count = schema
		.fields()
		.len()
		.checked_add(5)
		.and_then(|count| count.checked_mul(4))
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE schema size"))?;
	let width = 2
		* (std::mem::size_of::<super::ir::Column>()
			+ std::mem::size_of::<super::ir::ColumnOrigin>()
			+ std::mem::size_of::<super::ir::LogicalWildcardField>());
	admit(
		count
			.checked_mul(width)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE schema columns"))?,
		0,
	)?;
	for field in schema.fields() {
		let names = field
			.name()
			.len()
			.checked_mul(16)
			.and_then(|bytes| bytes.checked_add(schema.name().len().checked_mul(16)?))
			.and_then(|bytes| bytes.checked_add(512))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE schema names"))?;
		admit(names, 0)?;
		admit_type_shape(field.value_type(), 0, 1, admit)?;
	}
	Ok(())
}

/// Admits borrowed rich types for typed lowering and root planner metadata.
pub(super) fn admit_type_shape(
	ty: &Type,
	depth: usize,
	copies: usize,
	admit: &mut impl FnMut(usize, u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	ty.try_visit(depth, &mut |ty, depth| {
		if depth > 64 {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE schema type depth",
				64,
			));
		}
		let scale = copies.checked_mul(8).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("LIVE inferred type size overflows")
		})?;
		admit(
			scale.checked_mul(std::mem::size_of::<Type>()).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("LIVE inferred type size overflows")
			})?,
			copies as u64,
		)?;
		if let Type::RecordShape(fields) = ty {
			for (name, _) in fields {
				admit(
					name.len().checked_mul(scale).ok_or_else(|| {
						FerrosGrynnError::invalid_argument("LIVE type names overflow")
					})?,
					0,
				)?;
			}
		}
		Ok(())
	})
}

/// Synthetic syntax carries no source text offsets.
const fn span() -> TextSpan {
	TextSpan::new(0, 0)
}
/// Quoted identifiers preserve literal field/class spelling.
fn identifier(value: &str) -> Identifier {
	{
		let split_field_0 = value.into();
		let split_field_1 = true;
		let split_field_2 = span();
		crate::syn::split_internal::new_identifier(split_field_0, split_field_1, split_field_2)
	}
}
/// One unqualified field or callable syntax node.
fn name(value: &str) -> Expr {
	{
		let split_field_0 = ExprKind::Name(identifier(value));
		let split_field_1 = span();
		crate::syn::split_internal::new_expr(split_field_0, split_field_1)
	}
}
/// Explicit operation mapping does not depend on enum discriminants.
const fn operator(operator: LiveBinaryOperator) -> BinaryOperator {
	match operator {
		LiveBinaryOperator::Equal => BinaryOperator::Equal,
		LiveBinaryOperator::NotEqual => BinaryOperator::NotEqual,
		LiveBinaryOperator::Less => BinaryOperator::Less,
		LiveBinaryOperator::LessOrEqual => BinaryOperator::LessOrEqual,
		LiveBinaryOperator::Greater => BinaryOperator::Greater,
		LiveBinaryOperator::GreaterOrEqual => BinaryOperator::GreaterOrEqual,
		LiveBinaryOperator::And => BinaryOperator::And,
		LiveBinaryOperator::Or => BinaryOperator::Or,
		LiveBinaryOperator::Add => BinaryOperator::Add,
		LiveBinaryOperator::Subtract => BinaryOperator::Subtract,
		LiveBinaryOperator::Multiply => BinaryOperator::Multiply,
		LiveBinaryOperator::Divide => BinaryOperator::Divide,
		LiveBinaryOperator::In => BinaryOperator::In,
		LiveBinaryOperator::Coalesce => BinaryOperator::Coalesce,
		LiveBinaryOperator::Power => BinaryOperator::Power,
		LiveBinaryOperator::Remainder => BinaryOperator::Remainder,
		LiveBinaryOperator::Is => BinaryOperator::Is,
		LiveBinaryOperator::NotIs => BinaryOperator::NotIs,
		LiveBinaryOperator::NotIn => BinaryOperator::NotIn,
		LiveBinaryOperator::AllIn => BinaryOperator::AllIn,
		LiveBinaryOperator::AnyIn => BinaryOperator::AnyIn,
		LiveBinaryOperator::NoneIn => BinaryOperator::NoneIn,
		LiveBinaryOperator::AnyEqual => BinaryOperator::AnyEqual,
		LiveBinaryOperator::AllEqual => BinaryOperator::AllEqual,
		LiveBinaryOperator::Contains => BinaryOperator::Contains,
		LiveBinaryOperator::ContainsNot => BinaryOperator::ContainsNot,
		LiveBinaryOperator::ContainsAny => BinaryOperator::ContainsAny,
		LiveBinaryOperator::ContainsAll => BinaryOperator::ContainsAll,
		LiveBinaryOperator::Elvis => BinaryOperator::Elvis,
		LiveBinaryOperator::Inside => BinaryOperator::Inside,
		LiveBinaryOperator::NotInside => BinaryOperator::NotInside,
		LiveBinaryOperator::AllInside => BinaryOperator::AllInside,
		LiveBinaryOperator::AnyInside => BinaryOperator::AnyInside,
		LiveBinaryOperator::NoneInside => BinaryOperator::NoneInside,
		LiveBinaryOperator::Outside => BinaryOperator::Outside,
		LiveBinaryOperator::Intersects => BinaryOperator::Intersects,
	}
}
