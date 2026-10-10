//! Shared bounded logical dependencies for runtime and durable validation.

use std::{
	collections::{BTreeMap, BTreeSet},
	mem::size_of,
};

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveBindingId, LiveBindingOptions, LiveExpr, LiveProjection, LiveQuery},
};
/// Admission shared by descriptor traversal and dependency ordering.
pub struct BindingAdmission {
	/// Conservative retained node/container/name bytes.
	used: usize,
	/// Checked caller ceiling before allocation.
	maximum: usize,
	/// Unspent dependency/ordering work.
	remaining: u64,
	/// Original work ceiling for typed diagnostics.
	work_limit: u64,
}

impl BindingAdmission {
	/// Starts a caller-bounded dependency preparation.
	pub(crate) const fn new(options: &LiveBindingOptions) -> Self {
		Self {
			used: 0,
			maximum: options.scratch_bytes(),
			remaining: options.work(),
			work_limit: options.work(),
		}
	}

	/// Accounts borrowed node inspection and retained collections before
	/// growth.
	pub fn admit(&mut self, bytes: usize, work: u64) -> FerrosGrynnResult<()> {
		self.remaining = self.remaining.checked_sub(work).ok_or_else(|| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE dependency work",
				self.work_limit,
			)
		})?;
		let used = self
			.used
			.checked_add(bytes)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE dependency scratch"))?;
		if used > self.maximum {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE dependency scratch",
				self.maximum as u64,
			));
		}
		self.used = used;
		Ok(())
	}

	/// Returns conservative admitted bytes and known charged work without
	/// inspection.
	pub const fn observation(&self) -> (usize, u64) {
		(self.used, self.work_limit - self.remaining)
	}

	/// Shares unspent admission with the existing typed compiler/evaluator.
	pub fn remaining_options(&self) -> FerrosGrynnResult<LiveBindingOptions> {
		if self.remaining == 0 || self.used == self.maximum {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE dependency preparation",
				self.work_limit,
			));
		}
		LiveBindingOptions::default().with_budgets(self.maximum - self.used, self.remaining)
	}
}

/// Finds stable dependencies without substituting values or lexical names.
pub fn expression_dependencies(
	expression: &LiveExpr,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<Vec<LiveBindingId>> {
	let mut dependencies = BTreeSet::new();
	let mut record_names = BTreeSet::new();
	visit_expression(expression, 0, &mut dependencies, &mut record_names, budget)?;
	budget.admit(
		dependencies
			.len()
			.checked_mul(size_of::<LiveBindingId>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE dependency identities"))?,
		0,
	)?;
	Ok(dependencies.into_iter().collect())
}

/// Includes filters/projections of every nested SELECT in the same DAG.
pub fn query_dependencies(
	query: &LiveQuery,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<Vec<LiveBindingId>> {
	let mut dependencies = BTreeSet::new();
	let mut record_names = BTreeSet::new();
	visit_query(query, 0, &mut dependencies, &mut record_names, budget)?;
	budget.admit(
		dependencies
			.len()
			.checked_mul(size_of::<LiveBindingId>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE dependency identities"))?,
		0,
	)?;
	Ok(dependencies.into_iter().collect())
}

/// Borrowed recursive traversal is bounded before descending or retaining IDs.
pub fn query_record_names(
	query: &LiveQuery,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<Vec<crate::values::Text>> {
	let mut dependencies = BTreeSet::new();
	let mut names = BTreeSet::new();
	visit_query(query, 0, &mut dependencies, &mut names, budget)?;
	budget.admit(
		names
			.len()
			.checked_mul(size_of::<crate::values::Text>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE query record names"))?,
		0,
	)?;
	Ok(names.into_iter().collect())
}

/// Borrowed recursive traversal is bounded before descending or retaining IDs.
pub fn expression_record_names(
	expression: &LiveExpr,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<Vec<crate::values::Text>> {
	let mut dependencies = BTreeSet::new();
	let mut record_names = BTreeSet::new();
	visit_expression(expression, 0, &mut dependencies, &mut record_names, budget)?;
	budget.admit(
		record_names
			.len()
			.checked_mul(size_of::<crate::values::Text>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE record source names"))?,
		0,
	)?;
	Ok(record_names.into_iter().collect())
}

/// Borrowed recursive traversal is bounded before descending or retaining IDs.
fn visit_expression(
	expression: &LiveExpr,
	depth: usize,
	dependencies: &mut BTreeSet<LiveBindingId>,
	record_names: &mut BTreeSet<crate::values::Text>,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<()> {
	if depth > 64 {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE dependency depth",
			64,
		));
	}
	budget.admit(0, 1)?;
	match expression {
		LiveExpr::Range {
			start,
			end,
			..
		} => {
			for child in start.iter().chain(end.iter()) {
				visit_expression(child, depth + 1, dependencies, record_names, budget)?;
			}
		}
		LiveExpr::Binding(id) => {
			if !dependencies.contains(id) {
				budget.admit(128, 1)?;
				dependencies.insert(*id);
			}
		}
		LiveExpr::Not(value)
		| LiveExpr::Negate(value)
		| LiveExpr::Positive(value)
		| LiveExpr::Cast {
			value,
			..
		} => {
			visit_expression(value, depth + 1, dependencies, record_names, budget)?;
		}
		LiveExpr::Access {
			base,
			..
		} => visit_expression(base, depth + 1, dependencies, record_names, budget)?,
		LiveExpr::Index {
			base,
			index,
		} => {
			visit_expression(base, depth + 1, dependencies, record_names, budget)?;
			visit_expression(index, depth + 1, dependencies, record_names, budget)?;
		}
		LiveExpr::Binary {
			left,
			right,
			..
		} => {
			visit_expression(left, depth + 1, dependencies, record_names, budget)?;
			visit_expression(right, depth + 1, dependencies, record_names, budget)?;
		}
		LiveExpr::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			for child in [condition, then_value, else_value] {
				visit_expression(child, depth + 1, dependencies, record_names, budget)?;
			}
		}
		LiveExpr::Case {
			operand,
			branches,
			else_value,
		} => {
			for child in operand
				.iter()
				.map(Box::as_ref)
				.chain(branches.iter().flat_map(|(condition, value)| [condition, value]))
				.chain(else_value.iter().map(Box::as_ref))
			{
				visit_expression(child, depth + 1, dependencies, record_names, budget)?;
			}
		}
		LiveExpr::Call {
			arguments,
			..
		}
		| LiveExpr::Array(arguments)
		| LiveExpr::Set(arguments) => {
			for argument in arguments {
				visit_expression(argument, depth + 1, dependencies, record_names, budget)?;
			}
		}
		LiveExpr::CallWithOptions {
			arguments,
			..
		} => {
			for (_, value) in arguments {
				visit_expression(value, depth + 1, dependencies, record_names, budget)?;
			}
		}
		LiveExpr::Object(arguments) => {
			for (_, value) in arguments {
				visit_expression(value, depth + 1, dependencies, record_names, budget)?;
			}
		}
		LiveExpr::Select(query) => {
			visit_query(query, depth + 1, dependencies, record_names, budget)?;
		}
		LiveExpr::Literal(_) | LiveExpr::Field(_) | LiveExpr::Parameter(_) => {}
	}
	Ok(())
}

/// Orders borrowed definitions without cloning expressions or their values.
pub fn binding_order<
	'definition,
	Entries: Iterator<Item = (LiveBindingId, &'definition [LiveBindingId])> + Clone,
>(
	entries: Entries,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<Vec<LiveBindingId>> {
	let mut remaining = BTreeMap::new();
	let mut reverse = BTreeMap::<LiveBindingId, Vec<LiveBindingId>>::new();
	let mut ready = BTreeSet::new();
	for (id, dependencies) in entries.clone() {
		budget.admit(3 * 128 + size_of::<LiveBindingId>(), 1)?;
		if remaining.insert(id, dependencies.len()).is_some() {
			return Err(FerrosGrynnError::invalid_argument("LIVE LET identities must be distinct"));
		}
		if dependencies.is_empty() {
			ready.insert(id);
		}
	}
	for (id, dependencies) in entries {
		for dependency in dependencies {
			budget.admit(128 + 2 * size_of::<LiveBindingId>(), 1)?;
			if !remaining.contains_key(dependency) {
				return Err(FerrosGrynnError::invalid_argument(
					"LIVE LET dependency is absent or removed",
				));
			}
			reverse.entry(*dependency).or_default().push(id);
		}
	}
	let mut order = Vec::new();
	while let Some(id) = ready.pop_first() {
		budget.admit(2 * size_of::<LiveBindingId>(), 1)?;
		order.push(id);
		if let Some(dependents) = reverse.get(&id) {
			for dependent in dependents {
				budget.admit(0, 1)?;
				let count = remaining.get_mut(dependent).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"LIVE dependency ordering lost a definition",
					)
				})?;
				*count = count.checked_sub(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"LIVE dependency ordinals are inconsistent",
					)
				})?;
				if *count == 0 {
					ready.insert(*dependent);
				}
			}
		}
	}
	if order.len() != remaining.len() {
		return Err(FerrosGrynnError::invalid_argument(
			"LIVE LET dependencies must form an acyclic graph",
		));
	}
	Ok(order)
}

/// A query wrapper counts towards the same bounded logical expression depth.
fn visit_query(
	query: &LiveQuery,
	depth: usize,
	dependencies: &mut BTreeSet<LiveBindingId>,
	record_names: &mut BTreeSet<crate::values::Text>,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<()> {
	if depth > 64 {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE dependency depth",
			64,
		));
	}
	budget.admit(0, 1)?;
	for (index, source) in query.sources().enumerate() {
		if index != 0 {
			budget.admit(0, 1)?;
		}
		if let crate::live::LiveSource::Record(record)
		| crate::live::LiveSource::GraphRecord {
			record,
			..
		}
		| crate::live::LiveSource::GraphRecordExpr {
			record,
			..
		} = source
			&& !record_names.contains(record.class())
		{
			budget.admit(
				record.class().len().checked_add(128).ok_or_else(|| {
					FerrosGrynnError::operation_overflow("LIVE record source name")
				})?,
				1,
			)?;
			record_names.insert(record.class().into());
		}
		if let Some(graph) = source.graph_expression() {
			visit_expression(graph, depth + 1, dependencies, record_names, budget)?;
		}
	}
	for join in query.joins() {
		if let Some(predicate) = join.predicate() {
			visit_expression(predicate, depth + 1, dependencies, record_names, budget)?;
		}
	}
	if let Some(filter) = query.filter() {
		visit_expression(filter, depth + 1, dependencies, record_names, budget)?;
	}
	for value in query.split() {
		visit_expression(value, depth + 1, dependencies, record_names, budget)?;
	}
	if let Some(keys) = query.group() {
		for key in keys {
			visit_expression(key, depth + 1, dependencies, record_names, budget)?;
		}
	}
	for order in query.order() {
		match order.key() {
			super::LiveOrderKey::Expression(value) => {
				visit_expression(value, depth + 1, dependencies, record_names, budget)?;
			}
			super::LiveOrderKey::Name(_) => budget.admit(0, 1)?,
		}
	}
	for value in query.limit().into_iter().chain(query.start_offset()) {
		visit_expression(value, depth + 1, dependencies, record_names, budget)?;
	}
	match query.projection() {
		LiveProjection::Records => {}
		LiveProjection::Value(value) => {
			visit_expression(value, depth + 1, dependencies, record_names, budget)?;
		}
		LiveProjection::Fields(fields) => {
			for field in fields {
				visit_expression(&field.expression, depth + 1, dependencies, record_names, budget)?;
			}
		}
	}
	Ok(())
}
