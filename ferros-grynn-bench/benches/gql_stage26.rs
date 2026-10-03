//! Stage-26 complete-result and explicit resource-rejection measurements.

use std::{hint::black_box, time::Duration};

use criterion::{Criterion, criterion_group, criterion_main};
use ferros_grynn::{
	gql_status::FerrosGrynnResult,
	graph::{
		EdgeTopology,
		Graph,
		Mutable,
		analytics::{
			self,
			AllShortestOptions,
			AnalyticsOrientation,
			CentralityOptions,
			ClusteringOptions,
			KatzOptions,
			LabelPropagationOptions,
			LouvainOptions,
			PageRankOptions,
			SpectralOptions,
			TriangleOptions,
			WeightSelection,
		},
		tensor::{EndpointTensor, GraphAlgorithmLimits, ProjectionOptions, TensorOptions},
	},
	values::{Number, PropertyMap},
};

/// Explicit preparation controls are reported separately from SDK defaults.
fn limits() -> GraphAlgorithmLimits {
	let mut limits = GraphAlgorithmLimits::default();
	limits.memory_bytes = 12 * 1024 * 1024 * 1024;
	limits.max_work = 1_000_000_000;
	limits
}

/// Stable record IDs keep ordering independent of UUID generation.
fn cycle(rows: usize, weight: Number) -> FerrosGrynnResult<Graph<Mutable>> {
	let mut graph = Graph::<Mutable>::with_capacity(rows, rows);
	let mut nodes = Vec::with_capacity(rows);
	for ordinal in 0..rows {
		nodes.push(graph.add_node_with_id(
			format!("n:{ordinal}").parse()?,
			None,
			PropertyMap::new(),
		)?);
	}
	for ordinal in 0..rows {
		let edge = graph.add_edge_with_id(
			format!("e:{ordinal}").parse()?,
			EdgeTopology::directed([nodes[ordinal]], [nodes[(ordinal + 1) % rows]]),
			None,
			PropertyMap::new(),
		)?;
		graph.set_edge_weight(edge, Some(weight))?;
	}
	Ok(graph)
}

/// A sparse zero-triangle case in both favorable and adverse canonical order.
fn star(rows: usize, hub_last: bool) -> FerrosGrynnResult<Graph<Mutable>> {
	let mut graph = Graph::<Mutable>::with_capacity(rows, rows - 1);
	let hub = graph.add_node_with_id(
		if hub_last {
			"n:z_hub"
		} else {
			"n:hub"
		}
		.parse()?,
		None,
		PropertyMap::new(),
	)?;
	for ordinal in 1..rows {
		let node = graph.add_node_with_id(
			format!("n:v{ordinal:08}").parse()?,
			None,
			PropertyMap::new(),
		)?;
		graph.add_edge_with_id(
			format!("e:{ordinal}").parse()?,
			EdgeTopology::undirected([hub, node]),
			None,
			PropertyMap::new(),
		)?;
	}
	Ok(graph)
}

/// Benchmarks the same complete result, or the same typed rejection, every
/// time.
fn measure<T: std::fmt::Debug>(
	criterion: &mut Criterion,
	name: &str,
	mut run: impl FnMut() -> FerrosGrynnResult<T>,
) {
	let expected = match run() {
		Ok(value) => {
			println!("FG26\t{name}\tcomplete\t{value:?}");
			None
		}
		Err(error) => {
			let code = error.status().code().to_string();
			assert_eq!(code, "FG-54F13", "unexpected preflight failure: {error}");
			println!("FG26\t{name}\tresource_rejected\t{error}");
			Some(code)
		}
	};
	criterion.bench_function(name, |bench| {
		bench.iter(|| match run() {
			Ok(value) => {
				assert!(expected.is_none());
				black_box(value);
			}
			Err(error) => {
				assert_eq!(Some(error.status().code().to_string()), expected);
				black_box(error);
			}
		})
	});
}

/// Reports measured work and admission; these bytes are not process RSS.
fn scalars(criterion: &mut Criterion, graph: &Graph<Mutable>, name: &str) -> FerrosGrynnResult<()> {
	let mut tensor_options = TensorOptions::default();
	tensor_options.limits = limits();
	let tensor = EndpointTensor::from_graph(graph, tensor_options)?;
	let mut projection_options = ProjectionOptions::default();
	projection_options.limits = limits();
	measure(criterion, &format!("{name}/project"), || {
		let projection = tensor.project(projection_options.clone())?;
		Ok((
			projection.atoms().len(),
			projection.binary_adjacency().csr().stored_cells(),
			projection.resources(),
		))
	});
	let projection = tensor.project(projection_options)?;
	let n = projection.atoms().len();
	measure(criterion, &format!("{name}/degree"), || {
		let result = analytics::degree(&projection, limits(), &mut || Ok(()))?;
		assert!(
			result.values.iter().all(|(_, degree)| degree.incoming == 1 && degree.outgoing == 1)
		);
		Ok((result.values.len(), result.admitted_bytes, result.work))
	});
	let mut page_rank = PageRankOptions::default();
	page_rank.limits = limits();
	measure(criterion, &format!("{name}/pagerank"), || {
		let result = analytics::page_rank(&projection, page_rank, &mut || Ok(()))?;
		assert!(result.values.iter().all(|(_, score)| (score - 1.0 / n as f64).abs() < 1.0e-10));
		assert!((result.values.iter().map(|(_, score)| score).sum::<f64>() - 1.0).abs() < 1.0e-8);
		Ok((result.values.len(), result.admitted_bytes, result.work, result.convergence))
	});
	let mut spectral = SpectralOptions::default();
	spectral.limits = limits();
	measure(criterion, &format!("{name}/eigenvector"), || {
		let result = analytics::eigenvector(&projection, spectral, &mut || Ok(()))?;
		assert!(
			result
				.values
				.iter()
				.all(|(_, score)| (score - 1.0 / (n as f64).sqrt()).abs() < 1.0e-10)
		);
		Ok((result.values.len(), result.admitted_bytes, result.work, result.convergence))
	});
	let mut katz = KatzOptions::default();
	katz.spectral.limits = limits();
	measure(criterion, &format!("{name}/katz"), || {
		let result = analytics::katz(&projection, katz, &mut || Ok(()))?;
		assert!(result.values.iter().all(|(_, score)| (score - 2.0).abs() < 1.0e-9));
		Ok((result.values.len(), result.admitted_bytes, result.work, result.convergence))
	});
	let mut triangles = TriangleOptions::default();
	triangles.orientation = AnalyticsOrientation::Directed;
	triangles.limits = limits();
	measure(criterion, &format!("{name}/triangles"), || {
		let result = analytics::triangles(&projection, triangles, &mut || Ok(()))?;
		assert_eq!(result.totals.cycles + result.totals.transitive, 0);
		Ok((result.local.values.len(), result.local.admitted_bytes, result.local.work))
	});
	let mut clustering = ClusteringOptions::default();
	clustering.orientation = AnalyticsOrientation::Directed;
	clustering.limits = limits();
	measure(criterion, &format!("{name}/clustering"), || {
		let result = analytics::clustering(&projection, clustering, &mut || Ok(()))?;
		assert_eq!((result.mean, result.transitivity), (0.0, 0.0));
		Ok((result.local.values.len(), result.local.admitted_bytes, result.local.work))
	});
	let mut lpa = LabelPropagationOptions::default();
	lpa.limits = limits();
	measure(criterion, &format!("{name}/lpa"), || {
		let result = analytics::label_propagation(&projection, lpa, &mut || Ok(()))?;
		assert_eq!(result.assignments.values.len(), n);
		Ok((
			result.assignments.admitted_bytes,
			result.assignments.work,
			result.assignments.convergence,
		))
	});
	let source = tensor
		.domain_atoms()
		.find(|atom| atom.atom().id().to_string() == "n:0")
		.expect("cycle source");
	let target = tensor
		.domain_atoms()
		.find(|atom| atom.atom().id().to_string() == "n:3")
		.expect("cycle target");
	let mut paths = AllShortestOptions::default();
	paths.max_depth = Some(4);
	paths.limits = limits();
	let mut selection = ProjectionOptions::default();
	selection.limits = limits();
	measure(criterion, &format!("{name}/all_shortest"), || {
		let result = tensor.all_shortest_paths(source, target, selection.clone(), paths)?;
		let resources = (result.admitted_bytes(), result.work());
		let complete = result.collect_all()?;
		assert_eq!(complete.len(), 1);
		Ok(resources)
	});
	Ok(())
}

/// Runs large linear fixtures separately from all-source and exact-gain
/// ceilings.
fn benchmark(criterion: &mut Criterion) {
	let rows = std::env::var("FG26_ROWS")
		.ok()
		.map(|value| value.parse::<usize>().expect("FG26_ROWS integer"))
		.unwrap_or(10_000)
		.max(4);
	println!(
		"FG26\tlimits\tpreparation={:?}\tSDK_default={:?}",
		limits(),
		GraphAlgorithmLimits::default()
	);
	let graph = cycle(rows, Number::Int(1)).expect("cycle fixture");
	let name = format!("stage26/cycle/{rows}");
	measure(criterion, &format!("{name}/capture_default"), || {
		EndpointTensor::from_graph(&graph, TensorOptions::default())
			.map(|tensor| (tensor.shape(), tensor.admitted_bytes(), tensor.work()))
	});
	let mut options = TensorOptions::default();
	options.limits = limits();
	measure(criterion, &format!("{name}/capture"), || {
		EndpointTensor::from_graph(&graph, options)
			.map(|tensor| (tensor.shape(), tensor.admitted_bytes(), tensor.work()))
	});
	scalars(criterion, &graph, &name).expect("explicit preparation must complete");
	drop(graph);
	if std::env::var("FG26_SUPERLINEAR").is_ok() {
		for size in [32, 128, 512, 10_000] {
			let graph = cycle(size, Number::Int(1)).expect("small cycle");
			let tensor = EndpointTensor::from_graph(&graph, options).expect("small tensor");
			let mut selection = ProjectionOptions::default();
			selection.limits = limits();
			let projection = tensor.project(selection).expect("small projection");
			measure(criterion, &format!("stage26/centrality/{size}/harmonic"), || {
				analytics::harmonic_closeness(
					&projection,
					CentralityOptions::default(),
					&mut || Ok(()),
				)
				.map(|result| (result.values.len(), result.admitted_bytes, result.work))
			});
			measure(criterion, &format!("stage26/centrality/{size}/brandes"), || {
				analytics::betweenness(&projection, CentralityOptions::default(), &mut || Ok(()))
					.map(|result| (result.values.len(), result.admitted_bytes, result.work))
			});
		}
		for hub_last in [false, true] {
			let graph = star(rows, hub_last).expect("star fixture");
			let tensor = EndpointTensor::from_graph(&graph, options).expect("star tensor");
			let mut selection = ProjectionOptions::default();
			selection.limits = limits();
			let projection = tensor.project(selection).expect("star projection");
			measure(criterion, &format!("stage26/star/{rows}/{hub_last}/triangles"), || {
				analytics::triangles(&projection, TriangleOptions::default(), &mut || Ok(())).map(
					|result| {
						assert_eq!(result.totals.undirected, 0);
						(result.local.admitted_bytes, result.local.work)
					},
				)
			});
			measure(criterion, &format!("stage26/star/{rows}/{hub_last}/clustering"), || {
				analytics::clustering(&projection, ClusteringOptions::default(), &mut || Ok(()))
					.map(|result| {
						assert_eq!(result.mean, 0.0);
						(result.local.admitted_bytes, result.local.work)
					})
			});
		}
		for weight in [Number::Int(1), Number::Float(0.5)] {
			let graph = cycle(128, weight).expect("Louvain cycle");
			let tensor = EndpointTensor::from_graph(&graph, options).expect("Louvain tensor");
			let projection =
				tensor.project(ProjectionOptions::default()).expect("Louvain projection");
			let mut communities = LouvainOptions::default();
			communities.orientation = AnalyticsOrientation::Directed;
			communities.weights = WeightSelection::Stored;
			measure(criterion, &format!("stage26/louvain/{weight:?}"), || {
				analytics::louvain(&projection, communities, &mut || Ok(())).map(|result| {
					(
						result.assignments.values.len(),
						result.assignments.admitted_bytes,
						result.assignments.work,
						result.modularity,
					)
				})
			});
		}
	}
}

criterion_group! {
	name = benches;
	config = Criterion::default().sample_size(10).warm_up_time(Duration::from_millis(200)).measurement_time(Duration::from_secs(1));
	targets = benchmark
}
criterion_main!(benches);
