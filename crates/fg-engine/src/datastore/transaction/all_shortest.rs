//! Direct snapshot/COST adapter without parsing a query string.

use fg_runtime::__internal::{RecordIdInternal as _, TensorPathInternal as _};

use super::Transaction;
use crate::{
	datastore::{ConflictKey, GraphIndexCost},
	gql::PureFunctionEvaluator,
	gql_status::FerrosGrynnResult,
	graph::{
		AtomOccurrenceRef,
		GraphRef,
		Path,
		analytics::{AllShortestOptions, PathCost},
		tensor::{
			ProjectionOptions,
			TensorAtom,
			TensorOptions,
			control::{Control, invalid, resource},
		},
	},
	values::Number,
};

impl Transaction {
	/// Creates the existing bounded evaluator without exposing database roots.
	///
	/// # Errors
	/// Rejects insufficient compiler/runtime scratch.
	pub(crate) fn tensor_cost_evaluator_at<'a>(
		&self,
		snapshot: &'a crate::datastore::QuerySnapshot,
		bytes: usize,
	) -> FerrosGrynnResult<PureFunctionEvaluator<'a>> {
		self.ensure_live()?;
		PureFunctionEvaluator::for_builder(&snapshot.root, bytes, || Ok(()))
	}

	/// Resolves one named PURE COST overload and captures its durable version.
	///
	/// # Errors
	/// Rejects missing/ambiguous overloads, invalid signatures or transaction
	/// state.
	pub fn named_path_cost(
		&mut self,
		namespace: &str,
		name: &str,
		stateful: bool,
	) -> FerrosGrynnResult<GraphIndexCost> {
		let functions = self.functions_named(namespace, name)?;
		let mut candidates = functions.iter().filter(|function| {
			PureFunctionEvaluator::valid_path_cost(
				function,
				Some(if stateful {
					2
				} else {
					1
				}),
			)
		});
		let function =
			candidates.next().ok_or_else(|| invalid("named PURE COST overload is absent"))?;
		if candidates.next().is_some() {
			return Err(invalid("named PURE COST overload is ambiguous"));
		}
		Ok(GraphIndexCost::Function {
			function: function.id().ok_or_else(|| invalid("COST definition is unpublished"))?,
			version: function.version(),
		})
	}

	/// Returns every minimal admissible qualified Path from one MVCC read root.
	/// Named functions reuse published PURE typed IR, including prefix COST.
	///
	/// # Errors
	/// Rejects incompatible endpoints/functions, numeric/resource failures or
	/// expiry.
	#[allow(
		clippy::too_many_arguments,
		reason = "Snapshot scope, projection, constraints and versioned COST remain explicit SDK \
		          inputs"
	)]
	pub fn all_shortest_paths(
		&mut self,
		graph: &GraphRef,
		source: &AtomOccurrenceRef,
		target: &AtomOccurrenceRef,
		mut tensor: TensorOptions,
		mut projection: ProjectionOptions,
		mut options: AllShortestOptions,
		cost: GraphIndexCost,
	) -> FerrosGrynnResult<Vec<Path>> {
		let snapshot = self.query_snapshot()?;
		let mut check = || Ok(());
		let mut control = Control::new(options.limits, &mut check)?;
		// Tensor, evaluator, exact search and public Path buffers share one
		// cap.
		tensor.limits = options.limits;
		tensor.limits.memory_bytes = options.limits.memory_bytes - control.bytes;
		let tensor = self.endpoint_tensor_at(&snapshot, graph, tensor, &mut || Ok(()))?;
		control.admit(tensor.admitted_bytes())?;
		control.work(
			usize::try_from(tensor.work())
				.map_err(|_cause| invalid("tensor work exceeds host extent"))?,
		)?;
		let evaluator = if let GraphIndexCost::Function {
			function,
			version,
		} = cost
		{
			let definition = snapshot
				.root
				.schema
				.functions
				.get(&function)
				.ok_or_else(|| invalid("COST function is absent"))?;
			if definition.version() != version
				|| !PureFunctionEvaluator::valid_path_cost(definition, None)
			{
				return Err(invalid("all-shortest requires the captured PURE COST version"));
			}
			let scratch = (options.limits.memory_bytes - control.bytes) / 4;
			control.admit(scratch)?;
			let evaluator = PureFunctionEvaluator::for_builder(&snapshot.root, scratch, || Ok(()))?;
			for (dependency, _version) in evaluator.dependencies(function, version)? {
				self.record_read(ConflictKey::SchemaFunction(dependency));
			}
			options.cost = PathCost::Custom;
			Some(evaluator)
		} else {
			options.cost = if cost == GraphIndexCost::Hop {
				PathCost::Hop
			} else {
				PathCost::Stored
			};
			None
		};
		control.admit(
			source
				.atom()
				.id()
				.estimated_heap_bytes()
				.checked_add(target.atom().id().estimated_heap_bytes())
				.and_then(|bytes| bytes.checked_add(512))
				.ok_or_else(|| invalid("all-shortest endpoint size overflows"))?,
		)?;
		options.limits.memory_bytes -= control.bytes;
		options.limits.max_work = options
			.limits
			.max_work
			.checked_sub(control.work)
			.filter(|work| *work > 0)
			.ok_or_else(|| resource("all-shortest snapshot work", options.limits.max_work))?;
		projection.limits = options.limits;
		let results = tensor.all_shortest_paths_with_cost(
			&TensorAtom::Occurrence(source.clone()),
			&TensorAtom::Occurrence(target.clone()),
			projection,
			options,
			&mut |context| {
				let GraphIndexCost::Function {
					function,
					version,
				} = cost
				else {
					return Ok(Number::Int(1));
				};
				let edge = context
					.connector
					.occurrence()
					.ok_or_else(|| invalid("snapshot COST connector has no occurrence"))?;
				evaluator
					.as_ref()
					.ok_or_else(|| invalid("snapshot COST evaluator is absent"))?
					.path_contribution(
						function,
						version,
						edge,
						context.prefix,
						&|key| self.record_read(key),
						&|work| context.charge_work(work),
					)
			},
			&mut || Ok(()),
		)?;
		control.admit(results.admitted_bytes())?;
		control.work(
			usize::try_from(results.work())
				.map_err(|_cause| invalid("all-shortest work exceeds host extent"))?,
		)?;
		let logical = results.collect_all()?;
		let mut paths = control.array(logical.len())?;
		for path in logical {
			paths.push(path.materialize_with_control(
				&mut |_origin| Err(invalid("snapshot path contains a standalone runtime identity")),
				&mut control,
			)?);
		}
		Ok(paths)
	}
}
