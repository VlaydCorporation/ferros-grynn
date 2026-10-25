//! Qualified analytical barriers over the public tensor kernels.

use std::collections::BTreeSet;

use fg_runtime::__internal::{RecordIdInternal as _, ValueInternal as _};

use super::*;
use crate::graph::{
	Graph,
	Mutable,
	analytics::{
		self,
		AnalyticsOrientation,
		AnalyticsResult,
		ConvergenceMetadata,
		WeightSelection,
	},
	tensor::{
		EndpointTensor,
		GraphAlgorithmLimits,
		ProjectionOptions,
		TensorAtom,
		TensorOptions,
		TensorScope,
		control::{Control, invalid, overflow, resource},
	},
};

/// Validated common projection and algorithm controls for one closed `tv` call.
struct AnalyticsOptions {
	/// Requested qualified connector scope.
	scope: TensorScope,
	/// NODE/ATOM follows the traversal statement.
	projection: ProjectionOptions,
	/// Numerical strength selection.
	weights: WeightSelection,
	/// Structural/community orientation.
	orientation: AnalyticsOrientation,
	/// Complete resolved parameters exposed in the result.
	parameters: Object,
}

impl AnalyticsOptions {
	/// Rejects unknown/inapplicable fields before capturing any incidence.
	fn new(kind: TraverseStepKind, value: Option<&Value>, atom: bool) -> FerrosGrynnResult<Self> {
		let empty = Object::new();
		let object = match value {
			None => &empty,
			Some(Value::Object(object)) => object,
			Some(_) => return Err(invalid("analytical options must be an object")),
		};
		for key in object.keys() {
			let allowed = match key.as_str() {
				"scope" | "direction" | "labels" => true,
				"weights" => {
					kind != TraverseStepKind::Triangles && kind != TraverseStepKind::Degree
				}
				"orientation" => matches!(
					kind,
					TraverseStepKind::Triangles
						| TraverseStepKind::Clustering
						| TraverseStepKind::Louvain
				),
				"damping" => kind == TraverseStepKind::PageRank,
				"tolerance" | "max_iterations" => matches!(
					kind,
					TraverseStepKind::PageRank
						| TraverseStepKind::Eigenvector
						| TraverseStepKind::Katz
				),
				"normalized" => matches!(
					kind,
					TraverseStepKind::HarmonicCloseness | TraverseStepKind::Betweenness
				),
				"alpha" | "beta" => kind == TraverseStepKind::Katz,
				"votes" => kind == TraverseStepKind::LabelPropagation,
				"max_sweeps" => {
					matches!(kind, TraverseStepKind::LabelPropagation | TraverseStepKind::Louvain)
				}
				"max_levels" | "resolution" => kind == TraverseStepKind::Louvain,
				_ => false,
			};
			if !allowed {
				return Err(invalid("unknown or inapplicable analytical option"));
			}
		}
		let scope = match text_option(object, "scope", "LOCAL")? {
			"LOCAL" => TensorScope::Local,
			"ROOTED" => TensorScope::Rooted,
			_ => return Err(invalid("analytical scope must be LOCAL or ROOTED")),
		};
		let mut projection = {
			let split_field_0 = if atom {
				TraversalSemantics::Atom
			} else {
				TraversalSemantics::Node
			};
			let split_field_1 = direction_option(object, "direction", TraversalDirection::Forward)?;
			let mut split_options: ProjectionOptions = ProjectionOptions::default();
			split_options.mode = split_field_0;
			split_options.direction = split_field_1;
			split_options
		};
		if let Some(labels) = object.get("labels") {
			let Value::Array(labels) = labels else {
				return Err(invalid("labels must be an array"));
			};
			for label in labels.iter() {
				let label =
					label.as_str().ok_or_else(|| invalid("connector labels must be strings"))?;
				projection.labels.insert(label.into());
			}
		}
		let weights = match text_option(object, "weights", "UNWEIGHTED")? {
			"UNWEIGHTED" => WeightSelection::Unweighted,
			"STORED" => WeightSelection::Stored,
			_ => return Err(invalid("weights must be UNWEIGHTED or STORED")),
		};
		let orientation = match text_option(object, "orientation", "UNDIRECTED")? {
			"UNDIRECTED" => AnalyticsOrientation::Undirected,
			"DIRECTED" => AnalyticsOrientation::Directed,
			_ => return Err(invalid("orientation must be UNDIRECTED or DIRECTED")),
		};
		Ok(Self {
			scope,
			projection,
			weights,
			orientation,
			parameters: object.clone(),
		})
	}

	/// Resolves stopping controls with the same SDK defaults.
	fn iteration(&mut self) -> FerrosGrynnResult<analytics::IterationOptions> {
		let tolerance = float_option(&self.parameters, "tolerance", 1.0e-10)?;
		let max_iterations = usize_option(&self.parameters, "max_iterations", 1000)?;
		self.parameters.insert("tolerance", Value::from(tolerance));
		self.parameters.insert("max_iterations", Value::from(max_iterations as u64));
		Ok({
			let split_field_0 = tolerance;
			let split_field_1 = max_iterations;
			let mut split_options: analytics::IterationOptions =
				analytics::IterationOptions::default();
			split_options.tolerance = split_field_0;
			split_options.max_iterations = split_field_1;
			split_options
		})
	}
}

impl<Read: QueryRead> TraversalMachine<'_, Read> {
	/// Deduplicates qualified vertices, then invokes the unchanged Rust kernel.
	#[allow(
		clippy::mutable_key_type,
		reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash \
		          identities"
	)]
	pub(super) fn execute_tensor_analytics(
		&self,
		kind: TraverseStepKind,
		step: &PreparedStep,
		input: Vec<Traverser>,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let limits = self.control.graph_algorithm_limits();
		if limits.memory_bytes == 0 {
			return Err(resource("analytical memory", 0));
		}
		if limits.max_work == 0 {
			return Err(resource("analytical work", 0));
		}
		let mut check = || self.control.check();
		let mut control = Control::new(limits, &mut check)?;
		let mut memory = self.control.barrier_account();
		if !memory.reserve(limits.memory_bytes) {
			return Err(resource("analytical scratch", limits.memory_bytes as u64));
		}
		// Failed invocations retain their conservative work admission, so a
		// catchable numeric error cannot reset statement-wide gas.
		self.control.record_graph_work(limits.max_work)?;
		let argument_bytes = step.arguments.iter().try_fold(0_usize, |bytes, value| {
			bytes.checked_add(value.estimated_heap_bytes()).ok_or_else(overflow)
		})?;
		control.work(argument_bytes)?;
		control.admit(
			argument_bytes
				.checked_mul(4)
				.and_then(|bytes| bytes.checked_add(16_384))
				.ok_or_else(overflow)?,
		)?;
		let mut options = AnalyticsOptions::new(kind, step.arguments.first(), self.atom_semantics)?;
		let mut members = BTreeSet::new();
		let mut graph = self.graph.clone();
		for traverser in &input {
			control.step()?;
			let value = algorithms::occurrence(&traverser.current)?;
			// Validate origin and membership using the same snapshot reader.
			let view = self.read.graph(self.snapshot, value.graph())?;
			if view.atom_id(value.atom())?.is_none() {
				return Err(invalid("analytical stream occurrence is absent"));
			}
			if graph.is_none() {
				graph = Some(value.graph().clone());
			}
			control.admit(
				value
					.atom()
					.id()
					.estimated_heap_bytes()
					.checked_add(value.graph().name().len())
					.and_then(|bytes| bytes.checked_add(256))
					.ok_or_else(overflow)?,
			)?;
			let member = TensorAtom::Occurrence(value.clone());
			members.insert(member);
		}
		let tensor_options = {
			let split_field_0 = options.scope;
			let split_field_1 = remaining(&control)?;
			let mut split_options: TensorOptions = TensorOptions::default();
			split_options.scope = split_field_0;
			split_options.limits = split_field_1;
			split_options
		};
		let tensor = if let Some(graph) = &graph {
			let (tensor, binding) = self.read.endpoint_tensor_selected(
				self.snapshot,
				graph,
				tensor_options,
				step.tensor_cache.as_ref(),
				&mut || control.poll(),
			)?;
			self.control.record_tensor_capture(&tensor, binding.as_ref());
			tensor
		} else {
			control.admit(4096)?;
			EndpointTensor::from_graph_with_check(
				&Graph::<Mutable>::new(),
				{
					let split_field_0 = remaining(&control)?;
					let mut split_options: TensorOptions = tensor_options;
					split_options.limits = split_field_0;
					split_options
				},
				&mut || control.poll(),
			)?
		};
		control.admit(tensor.admitted_bytes())?;
		control.work(usize::try_from(tensor.work()).map_err(|_cause| overflow())?)?;
		options.projection.limits = remaining(&control)?;
		options.projection.weights = if kind == TraverseStepKind::Degree {
			WeightSelection::Stored
		} else {
			options.weights
		};
		let projection =
			tensor.project_induced_with_check(options.projection.clone(), &members, &mut || {
				control.poll()
			})?;
		control.admit(projection.resources().0)?;
		control.work(usize::try_from(projection.resources().1).map_err(|_cause| overflow())?)?;
		let kernel_limits = remaining(&control)?;
		let weights = options.weights;
		let orientation = options.orientation;
		if !matches!(kind, TraverseStepKind::Degree | TraverseStepKind::Triangles) {
			options.parameters.insert(
				"weights",
				Value::from(if weights == WeightSelection::Stored {
					"STORED"
				} else {
					"UNWEIGHTED"
				}),
			);
		}
		if matches!(
			kind,
			TraverseStepKind::Triangles | TraverseStepKind::Clustering | TraverseStepKind::Louvain
		) {
			options.parameters.insert(
				"orientation",
				Value::from(if orientation == AnalyticsOrientation::Directed {
					"DIRECTED"
				} else {
					"UNDIRECTED"
				}),
			);
		}
		let mut extra = Object::new();
		let (values, convergence) = match kind {
			TraverseStepKind::Degree => convert(
				analytics::degree(&projection, kernel_limits, &mut || control.poll())?,
				degree_value,
				&mut control,
			)?,
			TraverseStepKind::PageRank => {
				let damping = float_option(&options.parameters, "damping", 0.85)?;
				options.parameters.insert("damping", Value::from(damping));
				let iteration = options.iteration()?;
				convert(
					analytics::page_rank(
						&projection,
						{
							let split_field_0 = damping;
							let split_field_1 = weights;
							let split_field_2 = iteration;
							let split_field_3 = kernel_limits;
							let mut split_options: analytics::PageRankOptions =
								analytics::PageRankOptions::default();
							split_options.damping = split_field_0;
							split_options.weights = split_field_1;
							split_options.iteration = split_field_2;
							split_options.limits = split_field_3;
							split_options
						},
						&mut || control.poll(),
					)?,
					Value::from,
					&mut control,
				)?
			}
			TraverseStepKind::HarmonicCloseness | TraverseStepKind::Betweenness => {
				let normalized = bool_option(&options.parameters, "normalized", true)?;
				options.parameters.insert("normalized", Value::from(normalized));
				let parameters = {
					let split_field_0 = weights;
					let split_field_1 = normalized;
					let split_field_2 = kernel_limits;
					let mut split_options: analytics::CentralityOptions =
						analytics::CentralityOptions::default();
					split_options.weights = split_field_0;
					split_options.normalized = split_field_1;
					split_options.limits = split_field_2;
					split_options
				};
				let result = if kind == TraverseStepKind::HarmonicCloseness {
					analytics::harmonic_closeness(&projection, parameters, &mut || control.poll())?
				} else {
					analytics::betweenness(&projection, parameters, &mut || control.poll())?
				};
				convert(result, Value::from, &mut control)?
			}
			TraverseStepKind::Eigenvector | TraverseStepKind::Katz => {
				let iteration = options.iteration()?;
				let spectral = {
					let split_field_0 = weights;
					let split_field_1 = iteration;
					let split_field_2 = kernel_limits;
					let mut split_options: analytics::SpectralOptions =
						analytics::SpectralOptions::default();
					split_options.weights = split_field_0;
					split_options.iteration = split_field_1;
					split_options.limits = split_field_2;
					split_options
				};
				if kind == TraverseStepKind::Eigenvector {
					let result =
						analytics::eigenvector(&projection, spectral, &mut || control.poll())?;
					convert(result, Value::from, &mut control)?
				} else {
					let alpha = options
						.parameters
						.get("alpha")
						.map(|_| float_option(&options.parameters, "alpha", 0.0))
						.transpose()?;
					let beta = float_option(&options.parameters, "beta", 1.0)?;
					options.parameters.insert("beta", Value::from(beta));
					options.parameters.insert("alpha", alpha.map_or(Value::None, Value::from));
					let result = analytics::katz(
						&projection,
						{
							let split_field_0 = alpha;
							let split_field_1 = beta;
							let split_field_2 = spectral;
							let mut split_options: analytics::KatzOptions =
								analytics::KatzOptions::default();
							split_options.alpha = split_field_0;
							split_options.beta = split_field_1;
							split_options.spectral = split_field_2;
							split_options
						},
						&mut || control.poll(),
					)?;
					options
						.parameters
						.insert("alpha", result.parameters.alpha.map_or(Value::None, Value::from));
					convert(result, Value::from, &mut control)?
				}
			}
			TraverseStepKind::Triangles => {
				let result = analytics::triangles(
					&projection,
					{
						let split_field_0 = orientation;
						let split_field_1 = kernel_limits;
						let mut split_options: analytics::TriangleOptions =
							analytics::TriangleOptions::default();
						split_options.orientation = split_field_0;
						split_options.limits = split_field_1;
						split_options
					},
					&mut || control.poll(),
				)?;
				extra.insert("totals", triangle_value(result.totals));
				convert(result.local, triangle_value, &mut control)?
			}
			TraverseStepKind::Clustering => {
				let result = analytics::clustering(
					&projection,
					{
						let split_field_0 = orientation;
						let split_field_1 = weights;
						let split_field_2 = kernel_limits;
						let mut split_options: analytics::ClusteringOptions =
							analytics::ClusteringOptions::default();
						split_options.orientation = split_field_0;
						split_options.weights = split_field_1;
						split_options.limits = split_field_2;
						split_options
					},
					&mut || control.poll(),
				)?;
				extra.insert("mean", Value::from(result.mean));
				extra.insert("transitivity", Value::from(result.transitivity));
				convert(result.local, Value::from, &mut control)?
			}
			TraverseStepKind::LabelPropagation | TraverseStepKind::Louvain => {
				let default_sweeps = if kind == TraverseStepKind::Louvain {
					100
				} else {
					1000
				};
				let max_sweeps = usize_option(&options.parameters, "max_sweeps", default_sweeps)?;
				options.parameters.insert("max_sweeps", Value::from(max_sweeps as u64));
				if kind == TraverseStepKind::LabelPropagation {
					let votes =
						direction_option(&options.parameters, "votes", TraversalDirection::Both)?;
					options.parameters.insert("votes", Value::from(direction_name(votes)));
					let result = analytics::label_propagation(
						&projection,
						{
							let split_field_0 = votes;
							let split_field_1 = weights;
							let split_field_2 = max_sweeps;
							let split_field_3 = kernel_limits;
							let mut split_options: analytics::LabelPropagationOptions =
								analytics::LabelPropagationOptions::default();
							split_options.votes = split_field_0;
							split_options.weights = split_field_1;
							split_options.max_sweeps = split_field_2;
							split_options.limits = split_field_3;
							split_options
						},
						&mut || control.poll(),
					)?;
					extra.insert("levels", Value::from(result.levels as u64));
					convert(result.assignments, atom_value, &mut control)?
				} else {
					let resolution = float_option(&options.parameters, "resolution", 1.0)?;
					let max_levels = usize_option(&options.parameters, "max_levels", 64)?;
					options.parameters.insert("resolution", Value::from(resolution));
					options.parameters.insert("max_levels", Value::from(max_levels as u64));
					let result = analytics::louvain(
						&projection,
						{
							let split_field_0 = orientation;
							let split_field_1 = weights;
							let split_field_2 = resolution;
							let split_field_3 = max_sweeps;
							let split_field_4 = max_levels;
							let split_field_5 = kernel_limits;
							let mut split_options: analytics::LouvainOptions =
								analytics::LouvainOptions::default();
							split_options.orientation = split_field_0;
							split_options.weights = split_field_1;
							split_options.resolution = split_field_2;
							split_options.max_sweeps = split_field_3;
							split_options.max_levels = split_field_4;
							split_options.limits = split_field_5;
							split_options
						},
						&mut || control.poll(),
					)?;
					extra.insert("levels", Value::from(result.levels as u64));
					extra.insert("modularity", result.modularity.map_or(Value::None, Value::from));
					convert(result.assignments, atom_value, &mut control)?
				}
			}
			_ => return Err(invalid("non-analytical kind reached tensor barrier")),
		};
		extra.insert("values", Value::Array(values.into()));
		extra.insert(
			"scope",
			Value::from(if options.scope == TensorScope::Rooted {
				"ROOTED"
			} else {
				"LOCAL"
			}),
		);
		extra.insert(
			"mode",
			Value::from(if self.atom_semantics {
				"ATOM"
			} else {
				"NODE"
			}),
		);
		extra.insert("direction", Value::from(direction_name(options.projection.direction)));
		extra.insert("parameters", Value::Object(options.parameters));
		extra.insert(
			"iterations",
			convergence.map_or(Value::None, |value| Value::from(value.iterations as u64)),
		);
		extra.insert(
			"residual",
			convergence.map_or(Value::None, |value| Value::from(value.residual)),
		);
		extra.insert("work", Value::from(control.work));
		self.control
			.record_tensor_iterations(convergence.map_or(0, |value| value.iterations as u64));
		self.control.release_graph_work(limits.max_work - control.work);
		Ok(vec![Traverser::new(Value::Object(extra))])
	}
}

/// Keeps every phase within the caller's original aggregate admission.
fn remaining(control: &Control<'_>) -> FerrosGrynnResult<GraphAlgorithmLimits> {
	let memory_bytes = control
		.limits
		.memory_bytes
		.checked_sub(control.bytes)
		.filter(|value| *value > 0)
		.ok_or_else(|| resource("analytical bytes", control.limits.memory_bytes as u64))?;
	let max_work = control
		.limits
		.max_work
		.checked_sub(control.work)
		.filter(|value| *value > 0)
		.ok_or_else(|| resource("analytical work", control.limits.max_work))?;
	Ok({
		let split_field_0 = memory_bytes;
		let split_field_1 = max_work;
		let mut split_options: GraphAlgorithmLimits = control.limits;
		split_options.memory_bytes = split_field_0;
		split_options.max_work = split_field_1;
		split_options
	})
}

/// Admits result rows before converting owned identities and values.
fn convert<T, P>(
	result: AnalyticsResult<T, P>,
	value: impl Fn(T) -> Value,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<(Vec<Value>, Option<ConvergenceMetadata>)> {
	control.admit(result.admitted_bytes)?;
	control.work(usize::try_from(result.work).map_err(|_cause| overflow())?)?;
	control.admit(result.values.len().checked_mul(2048).ok_or_else(overflow)?)?;
	let mut values = control.array(result.values.len())?;
	for (atom, score) in result.values {
		control.step()?;
		values.push(Value::Object(Object::from_iter([
			("atom", atom_value(atom)),
			("value", value(score)),
		])));
	}
	Ok((values, result.convergence))
}

/// Snapshot adapters always produce qualified public occurrences.
fn atom_value(atom: TensorAtom) -> Value {
	fg_runtime::__internal::tensor_atom_into_value(atom)
}

/// Serializes exact motif counters without floating conversion.
fn triangle_value(value: analytics::TriangleCounts) -> Value {
	Value::Object(Object::from_iter([
		("undirected", Value::from(value.undirected)),
		("cycles", Value::from(value.cycles)),
		("transitive", Value::from(value.transitive)),
	]))
}

/// Degree and strength retain their distinct numerical contracts.
fn degree_value(value: analytics::DegreeValues) -> Value {
	Value::Object(Object::from_iter([
		("incoming", Value::from(value.incoming)),
		("outgoing", Value::from(value.outgoing)),
		("total", Value::from(value.total)),
		("incoming_strength", Value::Number(value.incoming_strength)),
		("outgoing_strength", Value::Number(value.outgoing_strength)),
		("total_strength", Value::Number(value.total_strength)),
		("normalized_incoming", Value::from(value.normalized_incoming)),
		("normalized_outgoing", Value::from(value.normalized_outgoing)),
		("normalized_total", Value::from(value.normalized_total)),
	]))
}

/// Borrowed text avoids allocating options before their admission.
fn text_option<'a>(
	options: &'a Object,
	name: &str,
	default: &'a str,
) -> FerrosGrynnResult<&'a str> {
	options.get(name).map_or(Ok(default), |value| {
		value.as_str().ok_or_else(|| invalid("analytical option must be a string"))
	})
}

/// Exact direction vocabulary shared by projection and community votes.
fn direction_option(
	options: &Object,
	name: &str,
	default: TraversalDirection,
) -> FerrosGrynnResult<TraversalDirection> {
	match text_option(options, name, direction_name(default))? {
		"OUT" => Ok(TraversalDirection::Forward),
		"IN" => Ok(TraversalDirection::Backward),
		"BOTH" => Ok(TraversalDirection::Both),
		_ => Err(invalid("direction must be OUT, IN or BOTH")),
	}
}

/// Stable public direction names.
fn direction_name(direction: TraversalDirection) -> &'static str {
	fg_runtime::__internal::EnumChoice::choose(&(direction), ["OUT", "IN", "BOTH"])
}

/// Validates finite iterative parameters independently of Number distances.
fn float_option(options: &Object, name: &str, default: f64) -> FerrosGrynnResult<f64> {
	match options.get(name) {
		None => Ok(default),
		Some(Value::Number(value)) => value
			.as_f64()
			.filter(|value| value.is_finite())
			.ok_or_else(|| invalid("analytical number must be finite")),
		Some(_) => Err(invalid("analytical option must be numeric")),
	}
}

/// Iteration/sweep bounds accept positive integers only.
fn usize_option(options: &Object, name: &str, default: usize) -> FerrosGrynnResult<usize> {
	match options.get(name) {
		None => Ok(default),
		Some(Value::Number(Number::Int(value))) => usize::try_from(*value)
			.ok()
			.filter(|value| *value > 0)
			.ok_or_else(|| invalid("analytical limit must be a positive integer")),
		Some(_) => Err(invalid("analytical limit must be a positive integer")),
	}
}

/// Normalization is explicit and never coerced from another value kind.
fn bool_option(options: &Object, name: &str, default: bool) -> FerrosGrynnResult<bool> {
	match options.get(name) {
		None => Ok(default),
		Some(Value::Bool(value)) => Ok(*value),
		Some(_) => Err(invalid("analytical option must be boolean")),
	}
}
