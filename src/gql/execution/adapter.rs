#![allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing imports during stage 1"
)]

use super::*;
use crate::datastore::{ClassSchema, ClassSchemaMode};

/// Parses, analyzes, and executes query text within an existing transaction.
///
/// # Errors
///
/// Returns syntax, semantic, storage, or execution errors produced by the
/// query.
pub fn execute_transaction(
	transaction: &mut Transaction,
	source: &str,
	variables: &Object,
	settings: ParserSettings,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let program = crate::gql::parse(source, settings)?;
	execute_parsed_transaction(transaction, source, &program, variables)
}

/// Analyzes and executes an already parsed program within a transaction.
///
/// # Errors
///
/// Returns semantic, storage, or execution errors produced while analyzing or
/// executing the program.
pub fn execute_parsed_transaction(
	transaction: &mut Transaction,
	source: &str,
	program: &Program,
	variables: &Object,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let snapshot = transaction.query_snapshot()?;
	let mut read = TransactionRead {
		transaction,
		snapshot,
	};
	execute_program(&mut read, source, program, variables)
}

pub(super) struct TransactionRead<'a> {
	pub(super) transaction: &'a mut Transaction,
	pub(super) snapshot: QuerySnapshot,
}

impl TransactionRead<'_> {
	fn refresh_snapshot(&mut self) -> FerrosGrynnResult<()> {
		self.snapshot = self.transaction.query_snapshot()?;
		Ok(())
	}
}

impl QueryCatalog for TransactionRead<'_> {
	fn schema(&self, class: &str) -> FerrosGrynnResult<Schema> {
		self.transaction.query_class(&self.snapshot, class).map(|schema| query_schema(&schema))
	}

	fn schema_mode(&self, class: &str) -> FerrosGrynnResult<ClassSchemaMode> {
		self.transaction.query_class(&self.snapshot, class).map(|schema| schema.schema_mode())
	}

	fn class_kind(&self, class: &str) -> FerrosGrynnResult<AtomKind> {
		self.transaction.query_class(&self.snapshot, class).map(|schema| schema.kind())
	}

	fn graph(&self, name: &str) -> FerrosGrynnResult<GraphRef> {
		self.transaction.query_named_graph(&self.snapshot, name)
	}
}

impl QueryRead for TransactionRead<'_> {
	type Graph<'a>
		= TransactionGraphView<'a>
	where
		Self: 'a;

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot> {
		Ok(Snapshot(self.snapshot.commit_ts()))
	}

	fn catalog(&self, _snapshot: Snapshot) -> &dyn QueryCatalog {
		self
	}

	fn scan<'a>(
		&'a self,
		_snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		predicate: Option<&TypedExpr>,
		slots: &[Value],
		_source: &str,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		let mut constraints = Vec::new();
		if let Some(predicate) = predicate {
			collect_scalar_constraints(predicate, slots, &mut constraints);
		}
		let rows = self.transaction.query_row_cursor(&self.snapshot, class, &constraints, graph)?;
		Ok(Box::new(StorageCursor {
			rows,
		}))
	}

	fn get(
		&self,
		_snapshot: Snapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<Option<Row>> {
		let row = self.transaction.query_point(&self.snapshot, record, graph)?;
		Ok(row.map(|row| Row::new(row.values)))
	}

	fn match_candidates(
		&self,
		_snapshot: Snapshot,
		graph: &GraphRef,
		binding: &ResolvedMatchBinding,
	) -> FerrosGrynnResult<Vec<AtomId>> {
		let (class, record) = match &binding.selector {
			ResolvedMatchSelector::Any => (None, None),
			ResolvedMatchSelector::Class(class) => (Some(class.as_ref()), None),
			ResolvedMatchSelector::Record(record) => (None, Some(record)),
		};
		self.transaction.query_match_candidates(
			&self.snapshot,
			graph,
			binding.domain.atom_kind(),
			class,
			record,
			binding.label.as_deref(),
		)
	}

	fn record_field(
		&self,
		_snapshot: Snapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
		field: &str,
	) -> FerrosGrynnResult<Value> {
		self.transaction.query_record_field(&self.snapshot, record, graph, field)
	}

	fn graph(
		&self,
		_snapshot: Snapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Self::Graph<'_>> {
		self.transaction.query_graph(&self.snapshot, reference)
	}

	fn property_dictionary(&self, _snapshot: Snapshot) -> Arc<PropertyDictionary> {
		self.transaction.query_property_dictionary()
	}

	fn execute_schema(&mut self, statement: &LogicalSchemaStatement) -> FerrosGrynnResult<Value> {
		let value = execute_schema(self.transaction, &self.snapshot, statement)?;
		self.refresh_snapshot()?;
		Ok(value)
	}

	fn execute_dml(
		&mut self,
		statement: &LogicalDmlStatement,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<Value> {
		let snapshot = self.snapshot.clone();
		let value = self.transaction.atomic_mutation(|transaction| {
			execute_dml(transaction, &snapshot, statement, slots, source)
		})?;
		self.refresh_snapshot()?;
		Ok(value)
	}

	fn execute_graph_dml(
		&mut self,
		statement: &LogicalGraphDmlStatement,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<Value> {
		let snapshot = self.snapshot.clone();
		let value = self.transaction.atomic_mutation(|transaction| {
			execute_graph_dml(transaction, &snapshot, statement, slots, source)
		})?;
		self.refresh_snapshot()?;
		Ok(value)
	}
}

fn collect_scalar_constraints(
	expr: &TypedExpr,
	slots: &[Value],
	constraints: &mut Vec<ScalarIndexConstraint>,
) {
	let TypedExprKind::Binary {
		left,
		op,
		right,
	} = &expr.kind
	else {
		return;
	};
	if matches!(op, BinaryOperator::And) {
		collect_scalar_constraints(left, slots, constraints);
		collect_scalar_constraints(right, slots, constraints);
		return;
	}
	let comparison = match op {
		BinaryOperator::Equal | BinaryOperator::Is => ScalarComparison::Equal,
		BinaryOperator::Less => ScalarComparison::Less,
		BinaryOperator::LessOrEqual => ScalarComparison::LessOrEqual,
		BinaryOperator::Greater => ScalarComparison::Greater,
		BinaryOperator::GreaterOrEqual => ScalarComparison::GreaterOrEqual,
		_ => return,
	};
	if let (TypedExprKind::Column(column), Some(value)) =
		(&left.kind, scalar_constraint_value(right, slots))
	{
		constraints.push(ScalarIndexConstraint {
			column: *column,
			comparison,
			value,
		});
	} else if let (Some(value), TypedExprKind::Column(column)) =
		(scalar_constraint_value(left, slots), &right.kind)
	{
		constraints.push(ScalarIndexConstraint {
			column: *column,
			comparison: match comparison {
				ScalarComparison::Equal => ScalarComparison::Equal,
				ScalarComparison::Less => ScalarComparison::Greater,
				ScalarComparison::LessOrEqual => ScalarComparison::GreaterOrEqual,
				ScalarComparison::Greater => ScalarComparison::Less,
				ScalarComparison::GreaterOrEqual => ScalarComparison::LessOrEqual,
			},
			value,
		});
	}
}

fn scalar_constraint_value(expr: &TypedExpr, slots: &[Value]) -> Option<Value> {
	match &expr.kind {
		TypedExprKind::Constant(value) => Some(value.clone()),
		TypedExprKind::Slot(slot) => slots.get(usize::try_from(*slot).ok()?).cloned(),
		_ => None,
	}
}

fn query_schema(schema: &ClassSchema) -> Schema {
	let mut columns = Vec::with_capacity(schema.fields().len() + 1);
	columns.push(Column {
		name: "id".into(),
		ty: Type::Record,
	});
	columns.extend(schema.fields().map(|field| Column {
		name: field.name().as_str().into(),
		ty: field.value_type().clone(),
	}));
	Schema {
		columns,
	}
}

/// Streaming adapter that keeps predicate registration alive after an early
/// LIMIT or cursor drop.
struct StorageCursor<'a> {
	rows: QueryStorageCursor<'a>,
}

impl RowCursor for StorageCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		let Some(row) = self.rows.next_row()? else {
			return Ok(None);
		};
		Ok(Some(Row::new(row.values)))
	}
}
