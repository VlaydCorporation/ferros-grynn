//! Analyzer variants become one raw JOIN relation before relational reduction.

use super::*;

/// Collapses only the private SDK typing family; ordinary unions stay intact.
pub(super) fn collapse<'plan, Read: QueryRead>(
	read: &Read,
	wrapper: &'plan LogicalSelect,
	slots: &[Value],
) -> FerrosGrynnResult<Cow<'plan, LogicalSelect>> {
	if wrapper.union_purpose != crate::syn::SelectUnionPurpose::JoinedTypingVariants
		|| wrapper.union_inputs.is_empty()
	{
		return Ok(Cow::Borrowed(wrapper));
	}
	let fields = read.bounded_fields().ok_or_else(invalid)?;
	let variants = &wrapper.union_inputs;
	let first = variants.first().ok_or_else(invalid)?;
	let count = variants.len();
	let width = first.input_schema.columns.len();
	fields.work(count as u64)?;
	if variants.iter().any(|plan| {
		plan.input_schema.columns.len() != width || plan.joins.len() != first.joins.len()
	}) {
		return Err(invalid());
	}
	let sources = first.joins.len().checked_add(1).ok_or_else(invalid)?;
	let cells = sources.checked_mul(count).ok_or_else(invalid)?;
	fields.admit(
		cells
			.checked_mul(std::mem::size_of::<Option<usize>>() + std::mem::size_of::<usize>())
			.ok_or_else(invalid)?,
	)?;
	fields.admit(
		count
			.checked_mul(
				std::mem::size_of::<Vec<Option<usize>>>()
					+ std::mem::size_of::<&TypedExpr>()
					+ std::mem::size_of::<&[TypedExpr]>(),
			)
			.ok_or_else(invalid)?,
	)?;
	let mut columns = Vec::with_capacity(sources);
	let mut labels = (0..count).map(|_| Vec::with_capacity(sources)).collect::<Vec<_>>();
	let mut offset = 0_usize;
	for (source_index, source) in std::iter::once(&first.source)
		.chain(first.joins.iter().map(|join| &join.source))
		.enumerate()
	{
		if let LogicalSelectSourceKind::AtomUnion {
			tag_column,
			..
		} = &source.kind
		{
			columns.push(offset.checked_add(*tag_column).ok_or_else(invalid)?);
			for (label, variant) in labels.iter_mut().zip(variants) {
				let source = if source_index == 0 {
					&variant.source
				} else {
					&variant.joins[source_index - 1].source
				};
				let LogicalSelectSourceKind::AtomUnion {
					typing,
					..
				} = &source.kind
				else {
					return Err(invalid());
				};
				label.push(*typing);
			}
		}
		offset = offset.checked_add(source.schema.columns.len()).ok_or_else(invalid)?;
	}
	if columns.is_empty() {
		return Err(invalid());
	}
	let selector = aggregate_union::RowSelector::Joined {
		columns: &columns,
		labels: &labels,
	};
	admission::select_copy(read, first, 0)?;
	let mut plan = first.clone();
	plan.lexical_id = wrapper.lexical_id;
	plan.outer_columns = wrapper.outer_columns;
	plan.union_purpose = crate::syn::SelectUnionPurpose::IndependentOutputs;
	// The root adapter may have frozen computed scopes on the wrapper only.
	for (target, source) in std::iter::once(&mut plan.source)
		.chain(plan.joins.iter_mut().map(|join| &mut join.source))
		.zip(std::iter::once(&wrapper.source).chain(wrapper.joins.iter().map(|join| &join.source)))
	{
		if let Some(reference) = source.kind.graph_scope() {
			admission::graph_copy(read, reference, 0)?;
			*target.kind.graph_scope_mut().ok_or_else(invalid)? = reference.clone();
		}
	}
	admission::schema_copy(read, &wrapper.result)?;
	plan.result = wrapper.result.clone();
	for index in 0..plan.joins.len() {
		if plan.joins[index].predicate.is_none() {
			continue;
		}
		let prefix_end = plan
			.source
			.schema
			.columns
			.len()
			.checked_add(plan.joins[..=index].iter().try_fold(0_usize, |sum, join| {
				sum.checked_add(join.source.schema.columns.len()).ok_or_else(invalid)
			})?)
			.ok_or_else(invalid)?;
		let prefix = columns.partition_point(|column| *column < prefix_end);
		let expressions = expression_variants(read, variants, |variant| {
			variant.joins[index].predicate.as_ref().ok_or_else(invalid)
		})?;
		if prefix == 0 {
			admission::expression_copy(read, expressions[0], 0)?;
			plan.joins[index].predicate = Some(expressions[0].clone());
		} else {
			fields.admit(
				count
					.checked_mul(prefix)
					.and_then(|cells| cells.checked_mul(std::mem::size_of::<Option<usize>>()))
					.and_then(|bytes| {
						count
							.checked_mul(std::mem::size_of::<Vec<Option<usize>>>())
							.and_then(|metadata| bytes.checked_add(metadata))
					})
					.ok_or_else(invalid)?,
			)?;
			let prefix_labels =
				labels.iter().map(|label| label[..prefix].to_vec()).collect::<Vec<_>>();
			plan.joins[index].predicate = Some(aggregate_union::row_expression_selected(
				read,
				&expressions,
				aggregate_union::RowSelector::Joined {
					columns: &columns[..prefix],
					labels: &prefix_labels,
				},
			)?);
		}
	}
	if first.filter.is_some() {
		let expressions = expression_variants(read, variants, |variant| {
			variant.filter.as_ref().ok_or_else(invalid)
		})?;
		plan.filter = Some(aggregate_union::row_expression_selected(read, &expressions, selector)?);
	}
	for index in 0..plan.split.len() {
		let expressions = expression_variants(read, variants, |variant| {
			variant.split.get(index).ok_or_else(invalid)
		})?;
		plan.split[index] = aggregate_union::row_expression_selected(read, &expressions, selector)?;
	}
	fields.admit(count.checked_mul(std::mem::size_of::<&[TypedExpr]>()).ok_or_else(invalid)?)?;
	let groups =
		variants.iter().map(|variant| variant.group.as_deref().unwrap_or(&[])).collect::<Vec<_>>();
	if let Some(group) = &mut plan.group {
		for (index, key) in group.iter_mut().enumerate() {
			let expressions = expression_variants(read, variants, |variant| {
				variant.group.as_deref().and_then(|group| group.get(index)).ok_or_else(invalid)
			})?;
			*key = aggregate_union::row_expression_selected(read, &expressions, selector)?;
		}
	}
	for (index, item) in plan.projection.iter_mut().enumerate() {
		match item {
			LogicalSelectItem::Expression {
				expression,
				..
			} => {
				let expressions = expression_variants(read, variants, |variant| {
					match variant.projection.get(index) {
						Some(LogicalSelectItem::Expression {
							expression,
							..
						}) => Ok(expression),
						_ => Err(invalid()),
					}
				})?;
				*expression = if first.group.is_some() {
					aggregate_union::merge_expression_selected(
						read,
						&expressions,
						selector,
						slots,
						&groups,
					)?
				} else {
					aggregate_union::row_expression_selected(read, &expressions, selector)?
				};
			}
			LogicalSelectItem::Wildcard(_) => {
				fields.admit(
					count
						.checked_mul(
							std::mem::size_of::<TypedExpr>()
								+ std::mem::size_of::<&TypedExpr>()
								+ std::mem::size_of::<Vec<LogicalWildcardField>>(),
						)
						.ok_or_else(invalid)?,
				)?;
				let ordinals = (0..count)
					.map(|ordinal| TypedExpr {
						kind: TypedExprKind::Constant(Value::from(ordinal)),
						ty: Type::Int,
						span: crate::gql_status::TextSpan::new(0, 0),
					})
					.collect::<Vec<_>>();
				let ordinal_refs = ordinals.iter().collect::<Vec<_>>();
				let ordinal =
					aggregate_union::row_expression_selected(read, &ordinal_refs, selector)?;
				let choices = variants
					.iter()
					.map(|variant| match variant.projection.get(index) {
						Some(LogicalSelectItem::Wildcard(fields)) => {
							admission::wildcard_copy(read, fields)?;
							Ok(fields.clone())
						}
						_ => Err(invalid()),
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				*item = LogicalSelectItem::WildcardVariants {
					selector: ordinal,
					choices,
				};
			}
			LogicalSelectItem::WildcardVariants {
				..
			} => return Err(invalid()),
		}
	}
	for (index, order) in plan.order.iter_mut().enumerate() {
		let expressions = expression_variants(read, variants, |variant| {
			variant.order.get(index).map(|order| &order.expression).ok_or_else(invalid)
		})?;
		order.expression = if let TypedExprKind::Column(column) = expressions[0].kind
			&& column >= width
			&& expressions.iter().all(
				|expression| matches!(expression.kind, TypedExprKind::Column(other) if other == column),
			) {
			admission::expression_copy(read, expressions[0], 0)?;
			expressions[0].clone()
		} else if first.group.is_some() {
			aggregate_union::merge_expression_selected(
				read,
				&expressions,
				selector,
				slots,
				&groups,
			)?
		} else {
			aggregate_union::row_expression_selected(read, &expressions, selector)?
		};
	}
	Ok(Cow::Owned(plan))
}

/// Charges each borrowed clause vector before allocation.
fn expression_variants<'plan, Read: QueryRead>(
	read: &Read,
	variants: &'plan [LogicalSelect],
	select: impl Fn(&'plan LogicalSelect) -> FerrosGrynnResult<&'plan TypedExpr>,
) -> FerrosGrynnResult<Vec<&'plan TypedExpr>> {
	let fields = read.bounded_fields().ok_or_else(invalid)?;
	fields.work(variants.len() as u64)?;
	fields.admit(
		variants.len().checked_mul(std::mem::size_of::<&TypedExpr>()).ok_or_else(invalid)?,
	)?;
	variants.iter().map(select).collect()
}

/// Malformed private family metadata never becomes a partial relational result.
fn invalid() -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument("LIVE JOIN typing family is inconsistent")
}
