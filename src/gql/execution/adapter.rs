#![allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing imports during stage 1"
)]

use super::*;

/// Parses, analyzes, and executes query text within an existing transaction.
///
/// # Errors
///
/// Returns syntax, semantic, storage, or execution errors produced by the
/// query.
pub fn execute_transaction(
	transaction: &Transaction,
	source: &str,
	variables: &Object,
	settings: ParserSettings,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let snapshot = transaction.query_snapshot()?;
	execute_text(
		&TransactionRead {
			transaction,
			snapshot,
		},
		source,
		variables,
		settings,
	)
}

pub(super) struct TransactionRead<'a> {
	pub(super) transaction: &'a Transaction,
	pub(super) snapshot: QuerySnapshot,
}

impl QueryCatalog for TransactionRead<'_> {
	fn schema(&self, table: &str) -> FerrosGrynnResult<Schema> {
		self.transaction.query_table(&self.snapshot, table).map(|schema| query_schema(&schema))
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
		table: &str,
		predicate: Option<&TypedExpr>,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		let mut constraints = Vec::new();
		if let Some(predicate) = predicate {
			collect_scalar_constraints(predicate, slots, &mut constraints);
		}
		let evaluator: Arc<dyn PredicateEvaluator> = Arc::new(QueryPredicate {
			predicate: predicate.cloned(),
			slots: slots.to_vec(),
			source: source.to_owned(),
		});

		let rows = self.transaction.query_row_cursor(&self.snapshot, table, &constraints)?;
		let predicate_graph = self.transaction.register_streaming_query_predicate(
			&self.snapshot,
			table,
			evaluator.clone(),
		)?;
		Ok(Box::new(StorageCursor {
			rows,
			transaction: self.transaction,
			snapshot: &self.snapshot,
			predicate: predicate_graph.map(|graph| (graph, evaluator)),
		}))
	}

	fn get(&self, _snapshot: Snapshot, record: &RecordId) -> FerrosGrynnResult<Option<Row>> {
		let row = self.transaction.query_point(&self.snapshot, record)?;
		self.transaction.register_query_point(
			&self.snapshot,
			record.table(),
			record,
			row.as_ref(),
		)?;
		Ok(row.map(|row| Row::new(row.values)))
	}

	fn graph(
		&self,
		_snapshot: Snapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Self::Graph<'_>> {
		self.transaction.query_graph(&self.snapshot, reference)
	}

	fn property_dictionary(&self, _snapshot: Snapshot) -> Arc<PropertyDictionary> {
		self.snapshot.property_dictionary()
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

fn query_schema(schema: &TableSchema) -> Schema {
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

struct QueryPredicate {
	predicate: Option<TypedExpr>,
	slots: Vec<Value>,
	source: String,
}

impl PredicateEvaluator for QueryPredicate {
	fn matches(&self, row: &QueryStorageRow) -> FerrosGrynnResult<bool> {
		self.predicate.as_ref().map_or(Ok(true), |predicate| {
			eval(predicate, Some(&Row::new(row.values.clone())), &self.slots, &self.source, None)
				.map(|value| value.is_truthy())
		})
	}
}

/// Streaming adapter that keeps predicate registration alive after an early
/// LIMIT or cursor drop.
struct StorageCursor<'a> {
	rows: QueryStorageCursor<'a>,
	transaction: &'a Transaction,
	snapshot: &'a QuerySnapshot,
	predicate: Option<(RecordId, Arc<dyn PredicateEvaluator>)>,
}

impl RowCursor for StorageCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		let Some(row) = self.rows.next_row()? else {
			return Ok(None);
		};
		if let Some((graph, evaluator)) = &self.predicate {
			self.transaction.register_streamed_query_row(
				self.snapshot,
				graph,
				&row,
				evaluator.as_ref(),
			)?;
		}
		Ok(Some(Row::new(row.values)))
	}
}
