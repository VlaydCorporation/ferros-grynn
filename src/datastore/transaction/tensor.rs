//! One-snapshot adapter for qualified runtime tensors.

use std::collections::{BTreeSet, VecDeque};

use super::{QuerySnapshot, Transaction, TransactionGraphView};
use crate::{
	datastore::{
		ClassIndexState,
		ConflictKey,
		GraphIndex,
		GraphIndexDefinition,
		IndexId,
		QueryTensorCache,
		TensorArtifact,
		TensorCacheFormat,
	},
	gql_status::FerrosGrynnResult,
	graph::{
		AtomId,
		AtomKind,
		EdgeEndpoint,
		EdgeRead,
		GraphRead,
		GraphRef,
		SnapshotId,
		nested_graph_reference,
		tensor::{
			EndpointTensor,
			TensorAtom,
			TensorOptions,
			TensorScope,
			build::{InputConnector, InputEndpoint, PlaneBuilder, TensorBuilder},
			control::{Control, invalid, overflow, resource},
		},
	},
	values::Number,
};

impl Transaction {
	/// Captures an owned tensor from one MVCC snapshot, with qualified axes.
	/// LOCAL includes boundary endpoints; ROOTED scans each child graph once.
	///
	/// # Errors
	/// Returns inaccessible graph, malformed incidence or admission errors.
	pub fn endpoint_tensor(
		&mut self,
		graph: &GraphRef,
		options: TensorOptions,
	) -> FerrosGrynnResult<EndpointTensor> {
		self.endpoint_tensor_with_check(graph, options, &mut || Ok(()))
	}

	/// Captures qualified tensor data with cooperative cancellation checks.
	///
	/// # Errors
	/// Propagates snapshot/read/check errors and resource exhaustion.
	pub fn endpoint_tensor_with_check(
		&mut self,
		graph: &GraphRef,
		options: TensorOptions,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<EndpointTensor> {
		let snapshot = self.query_snapshot()?;
		self.endpoint_tensor_at(&snapshot, graph, options, check)
	}

	/// Reads a required Ready compatible tensor cache from this exact snapshot.
	///
	/// # Errors
	/// Rejects a missing, stale or incompatible definition before returning
	/// data, and propagates resource, snapshot and cancellation failures.
	pub fn endpoint_tensor_using_index(
		&mut self,
		graph: &GraphRef,
		options: TensorOptions,
		index: IndexId,
	) -> FerrosGrynnResult<EndpointTensor> {
		let snapshot = self.query_snapshot()?;
		let mut check = || Ok(());
		let mut control = Control::new(options.limits, &mut check)?;
		self.cached_tensor_at(&snapshot, graph, options, Some(index), &mut control)?
			.ok_or_else(|| invalid("required tensor cache is unavailable or incompatible"))
	}

	/// Captures tensor data under a caller's already selected immutable root.
	pub(crate) fn endpoint_tensor_at(
		&self,
		snapshot: &QuerySnapshot,
		graph: &GraphRef,
		options: TensorOptions,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<EndpointTensor> {
		self.endpoint_tensor_selected_at(snapshot, graph, options, None, check)
			.map(|(tensor, _binding)| tensor)
	}

	/// Validates the planned generation and reports actual cache/fallback use.
	pub(crate) fn endpoint_tensor_selected_at(
		&self,
		snapshot: &QuerySnapshot,
		graph: &GraphRef,
		options: TensorOptions,
		selected: Option<&QueryTensorCache>,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<(EndpointTensor, Option<QueryTensorCache>)> {
		let mut control = Control::new(options.limits, check)?;
		let bindings = self.tensor_cache_bindings(
			snapshot,
			graph,
			Some(options.scope),
			selected.map(|binding| binding.index),
			&mut control,
		)?;
		let binding = if let Some(selected) = selected {
			bindings
				.into_iter()
				.find(|binding| binding == selected && binding.scope == options.scope)
		} else {
			bindings
				.into_iter()
				.filter(|binding| binding.scope == options.scope)
				.min_by_key(|binding| (binding.bytes, binding.index))
		};
		if let Some(binding) = binding
			&& let Some(tensor) =
				self.cached_tensor_at(snapshot, graph, options, Some(binding.index), &mut control)?
		{
			return Ok((tensor, Some(binding)));
		}
		let limits = crate::graph::tensor::GraphAlgorithmLimits {
			memory_bytes: options.limits.memory_bytes.checked_sub(control.bytes).ok_or_else(
				|| resource("tensor lookup bytes", options.limits.memory_bytes as u64),
			)?,
			max_work: options
				.limits
				.max_work
				.checked_sub(control.work)
				.ok_or_else(|| resource("tensor lookup work", options.limits.max_work))?,
			..options.limits
		};
		let mut tensor = capture_tensor(
			graph,
			TensorOptions {
				limits,
				..options
			},
			&mut |reference| self.query_graph(snapshot, reference),
			&|key| self.record_read(key),
			None,
			&mut || control.poll(),
		)?;
		control.admit(tensor.admitted_bytes())?;
		control.work(usize::try_from(tensor.work()).map_err(|_cause| overflow())?)?;
		tensor.options = options;
		tensor.admitted_bytes = control.bytes;
		tensor.work = control.work;
		Ok((tensor, None))
	}

	/// Projects demanded Ready metadata through the same full-byte validator.
	pub(crate) fn query_tensor_caches(
		&self,
		snapshot: &QuerySnapshot,
		graph: &GraphRef,
	) -> FerrosGrynnResult<Vec<QueryTensorCache>> {
		let mut check = || Ok(());
		let mut control =
			Control::new(crate::graph::tensor::GraphAlgorithmLimits::default(), &mut check)?;
		self.tensor_cache_bindings(snapshot, graph, None, None, &mut control)
	}

	/// Keeps metadata admission separate from immutable artifact payloads.
	fn tensor_cache_bindings(
		&self,
		snapshot: &QuerySnapshot,
		graph: &GraphRef,
		requested_scope: Option<TensorScope>,
		selected: Option<IndexId>,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<Vec<QueryTensorCache>> {
		self.ensure_live()?;
		self.query_graph(snapshot, graph)?;
		self.record_read(ConflictKey::GraphDefinition(graph.id()));
		let schema = snapshot
			.root
			.schema
			.graph(graph.id())
			.ok_or_else(|| invalid("tensor graph schema is absent"))?;
		let mut bindings = Vec::new();
		for index in schema.indexes() {
			control.step()?;
			let GraphIndexDefinition::Tensor {
				scope,
				..
			} = index.definition()
			else {
				continue;
			};
			if requested_scope.is_some_and(|requested| requested != *scope)
				|| selected.is_some_and(|selected| index.id() != Some(selected))
			{
				continue;
			}
			let Some(artifact) = self.ready_tensor_artifact(snapshot, index, *scope, control)?
			else {
				continue;
			};
			let header = artifact.layout.header;
			control.admit(2 * size_of::<QueryTensorCache>())?;
			bindings.try_reserve_exact(1).map_err(|cause| {
				resource("tensor cache bindings", control.limits.memory_bytes as u64)
					.with_technical_source(cause)
			})?;
			control.admit(
				artifact
					.dependencies
					.len()
					.checked_mul(2 * size_of::<(crate::graph::GraphId, u64)>())
					.and_then(|bytes| bytes.checked_add(128))
					.ok_or_else(overflow)?,
			)?;
			bindings.push(QueryTensorCache {
				graph: graph.id(),
				index: index.id().ok_or_else(|| invalid("tensor index has no identity"))?,
				version: index.version(),
				revision: index.build_revision(),
				fingerprint: header.identity.fingerprint,
				scope: *scope,
				format: match header.format {
					crate::storage::TensorCacheTag::Csr => TensorCacheFormat::Csr,
					crate::storage::TensorCacheTag::Dense => TensorCacheFormat::Dense,
				},
				rows: header.rows,
				connectors: header.connectors,
				source_cells: header.source_cells,
				target_cells: header.target_cells,
				bytes: artifact
					.layout
					.page_count
					.checked_mul(crate::storage::PAGE_SIZE as u64)
					.ok_or_else(overflow)?,
				dependencies: artifact.dependencies.clone().into(),
			});
		}
		Ok(bindings)
	}

	/// Checks catalog state, durable revisions and complete descriptor bytes.
	fn ready_tensor_artifact<'s>(
		&self,
		snapshot: &'s QuerySnapshot,
		index: &GraphIndex,
		scope: TensorScope,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<Option<&'s TensorArtifact>> {
		if index.state() != ClassIndexState::Ready
			|| !matches!(index.definition(), GraphIndexDefinition::Tensor { scope: indexed, .. } if *indexed == scope)
		{
			return Ok(None);
		}
		let Some(artifact) = index.id().and_then(|id| snapshot.root.tensor_artifacts.get(&id))
		else {
			return Ok(None);
		};
		control.work(artifact.dependencies.len())?;
		if artifact.layout.header.identity.version != index.version().get()
			|| artifact.layout.header.identity.revision != index.build_revision()
			|| artifact.dependencies.iter().any(|(graph, revision)| {
				snapshot.root.graph(*graph).is_none_or(|root| root.tensor_revision != *revision)
			}) {
			return Ok(None);
		}
		control.admit(artifact.descriptor.len())?;
		control.work(artifact.descriptor.len())?;
		if index.descriptor_with_limit(artifact.descriptor.len())? != artifact.descriptor {
			return Ok(None);
		}
		for binding in &artifact.tensor.bindings {
			control.step()?;
			let reference = binding
				.graph
				.as_ref()
				.ok_or_else(|| invalid("cached tensor binding lacks catalog identity"))?;
			let graph = snapshot
				.root
				.graph(reference.id())
				.ok_or_else(|| invalid("cached graph is absent"))?;
			if graph.reference() != reference
				|| self.query_graph(snapshot, reference)?.identity() != binding.origin
			{
				return Err(invalid("cached tensor origin differs from snapshot"));
			}
		}
		Ok(Some(artifact))
	}

	/// Selects one compatible generation and retains the capture route's SSI
	/// keys.
	fn cached_tensor_at(
		&self,
		snapshot: &QuerySnapshot,
		graph: &GraphRef,
		options: TensorOptions,
		forced: Option<IndexId>,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<Option<EndpointTensor>> {
		self.query_graph(snapshot, graph)?;
		self.record_read(ConflictKey::GraphDefinition(graph.id()));
		let schema = snapshot
			.root
			.schema
			.graph(graph.id())
			.ok_or_else(|| invalid("tensor graph schema is absent"))?;
		for index in schema.indexes() {
			control.step()?;
			if forced.is_some_and(|id| index.id() != Some(id)) {
				continue;
			}
			let Some(artifact) =
				self.ready_tensor_artifact(snapshot, index, options.scope, control)?
			else {
				continue;
			};
			control.admit(artifact.tensor.admitted_bytes())?;
			control.work(artifact.tensor.admitted_bytes())?;
			// ponytail: the owned SDK result clones the sparse capture; share
			// its payload only if measured warm-read allocations
			// require it.
			let mut tensor = artifact.tensor.clone();
			let current_snapshot = SnapshotId::new(snapshot.root.commit_ts, self.id);
			for binding in &mut tensor.bindings {
				control.step()?;
				let reference = binding
					.graph
					.as_ref()
					.ok_or_else(|| invalid("cached tensor binding lacks catalog identity"))?;
				binding.snapshot = current_snapshot;
				self.record_reads([
					ConflictKey::GraphDefinition(reference.id()),
					ConflictKey::GraphKindRange(reference.id(), AtomKind::Node),
					ConflictKey::GraphKindRange(reference.id(), AtomKind::Edge),
				]);
			}
			for containment in &mut tensor.containment {
				containment.child.snapshot = current_snapshot;
			}
			for atom in &tensor.atoms {
				control.step()?;
				let occurrence = atom
					.occurrence()
					.ok_or_else(|| invalid("cached tensor atom is not qualified"))?;
				let graph = occurrence.graph().id();
				let global = snapshot
					.root
					.resolve_atom(occurrence.atom().id())
					.ok_or_else(|| invalid("cached atom is absent"))?;
				let local = snapshot
					.root
					.graph(graph)
					.and_then(|graph| graph.local_id(global))
					.ok_or_else(|| invalid("cached occurrence is absent"))?;
				self.record_reads([
					ConflictKey::BaseAtom(global),
					ConflictKey::GraphMembership(graph, global),
					ConflictKey::GraphOverlay(graph, global),
					ConflictKey::GraphIncidence(graph, local),
				]);
			}
			tensor.options = options;
			tensor.admitted_bytes = control.bytes;
			tensor.work = control.work;
			return Ok(Some(tensor));
		}
		Ok(None)
	}
}
#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::{
			ClassSchema,
			ConflictKey,
			Datastore,
			GraphIndex,
			GraphIndexDefinition,
			TensorArtifact,
			TensorCacheFormat,
		},
		graph::{AtomKind, EdgeTopology, GraphKind},
		values::{PropertyMap, RecordId},
	};

	#[test]
	fn tensor_auto_chooses_a_work_admitted_format_and_external_sort_matches_capture() {
		use crate::graph::tensor::GraphAlgorithmLimits;
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("n", AtomKind::Node, []).unwrap()).unwrap();
		seed.define_class(ClassSchema::new("e", AtomKind::Edge, []).unwrap()).unwrap();
		let graph = seed.define_graph("sort", GraphKind::Hypergraph).unwrap();
		let mut nodes = Vec::new();
		for ordinal in 0..100 {
			let record: RecordId = format!("n:{ordinal}").parse().unwrap();
			seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
			nodes.push(seed.attach_node(graph.id(), &record, None, None).unwrap());
		}
		for ordinal in 0..100 {
			let record: RecordId = format!("e:{ordinal}").parse().unwrap();
			seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
			seed.attach_edge(
				graph.id(),
				&record,
				None,
				EdgeTopology::directed(nodes[..10].iter().copied(), nodes[10..20].iter().copied()),
				None,
				None,
			)
			.unwrap();
		}
		let id = seed
			.define_graph_index(
				"sort",
				GraphIndex::new_tensor(
					"auto",
					TensorScope::Local,
					TensorCacheFormat::Auto,
					1 << 20,
				)
				.unwrap(),
			)
			.unwrap();
		for ordinal in 0..3 {
			seed.define_graph_index(
				"sort",
				GraphIndex::new_tensor(
					format!("extra{ordinal}"),
					TensorScope::Local,
					TensorCacheFormat::Auto,
					1 << 20,
				)
				.unwrap(),
			)
			.unwrap();
		}
		seed.commit().unwrap();
		let mut read = store.begin();
		let snapshot = read.query_snapshot().unwrap();
		let mut index = read.graph_index("sort", "auto").unwrap().unwrap();
		index.build_revision = snapshot.root.commit_ts + 1;
		let limits = GraphAlgorithmLimits {
			memory_bytes: 32 << 20,
			max_work: 1_000_000,
			..Default::default()
		};
		let memory = TensorArtifact::build(
			&snapshot.root,
			&index,
			index.build_revision(),
			limits,
			None,
			&mut || Ok(()),
		)
		.unwrap();
		assert_eq!(memory.layout.header.format, crate::storage::TensorCacheTag::Csr);
		let mut data: [Vec<u8>; 8] = std::array::from_fn(|section| {
			vec![
				0;
				memory.layout.sections[section].count as usize
					* usize::from(memory.layout.sections[section].width)
			]
		});
		for (section, bytes) in memory.layout.sections.iter().zip(&mut data) {
			for (ordinal, output) in bytes.chunks_exact_mut(usize::from(section.width)).enumerate()
			{
				memory.encode_element(section.kind, ordinal as u64, output).unwrap();
			}
		}
		let restored = TensorArtifact::restore(
			&snapshot.root,
			&index,
			memory.layout.clone(),
			data,
			GraphAlgorithmLimits::default(),
			&mut || Ok(()),
		)
		.unwrap();
		assert_eq!(restored.layout, memory.layout);
		assert_eq!(restored.tensor.source, memory.tensor.source);
		index.definition = GraphIndexDefinition::Tensor {
			scope: TensorScope::Local,
			format: TensorCacheFormat::Dense,
		};
		assert!(
			TensorArtifact::build(
				&snapshot.root,
				&index,
				index.build_revision(),
				limits,
				None,
				&mut || Ok(())
			)
			.is_err()
		);
		index.definition = GraphIndexDefinition::Tensor {
			scope: TensorScope::Local,
			format: TensorCacheFormat::Auto,
		};
		let root = store.inner.query_scratch_root().join("tensor-sort");
		let sorted = crate::storage::with_external_sort_control(
			std::num::NonZeroUsize::new(512).unwrap(),
			std::num::NonZeroUsize::MIN,
			std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
			|| {
				TensorArtifact::build(
					&snapshot.root,
					&index,
					index.build_revision(),
					limits,
					Some(&root),
					&mut || Ok(()),
				)
			},
		)
		.unwrap();
		assert!(crate::storage::external_sort_metrics().runs > 0);
		assert_eq!(sorted.tensor.source, memory.tensor.source);
		assert_eq!(sorted.tensor.target, memory.tensor.target);
		assert!(root.join("tmp/index-sort").read_dir().unwrap().next().is_none());
		let options = TensorOptions {
			limits: GraphAlgorithmLimits {
				max_work: 1,
				..limits
			},
			..Default::default()
		};
		let error = read
			.endpoint_tensor_using_index(&graph, options, IndexId::new(id.get() + 100).unwrap())
			.unwrap_err();
		assert!(error.to_string().contains("work"), "{error}");
	}

	#[test]
	fn tensor_artifact_restores_both_formats_and_checks_full_metadata_revisions_and_caps() {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("n", AtomKind::Node, []).unwrap()).unwrap();
		seed.define_class(ClassSchema::new("e", AtomKind::Edge, []).unwrap()).unwrap();
		let graph = seed.define_graph("cache", GraphKind::Graph).unwrap();
		let records = ["n:a", "n:b", "e:ab"].map(|id| id.parse::<RecordId>().unwrap());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
		}
		let a = seed.attach_node(graph.id(), &records[0], None, None).unwrap();
		let b = seed.attach_node(graph.id(), &records[1], None, None).unwrap();
		let edge = seed
			.attach_edge(
				graph.id(),
				&records[2],
				None,
				EdgeTopology::bidirectional([a], [b]),
				Some(Number::Int(-3)),
				None,
			)
			.unwrap();
		let id = seed
			.define_graph_index(
				"cache",
				GraphIndex::new_tensor(
					"tensor",
					TensorScope::Local,
					TensorCacheFormat::Auto,
					1 << 20,
				)
				.unwrap(),
			)
			.unwrap();
		seed.commit().unwrap();
		let mut read = store.begin();
		let snapshot = read.query_snapshot().unwrap();
		let mut index = snapshot
			.root
			.schema
			.graph(graph.id())
			.unwrap()
			.indexes()
			.find(|index| index.id() == Some(id))
			.unwrap()
			.clone();
		index.build_revision = snapshot.root.commit_ts + 1;
		let limits = crate::graph::tensor::GraphAlgorithmLimits {
			memory_bytes: 16 << 20,
			..Default::default()
		};
		for format in [TensorCacheFormat::Csr, TensorCacheFormat::Dense, TensorCacheFormat::Auto] {
			index.definition = GraphIndexDefinition::Tensor {
				scope: TensorScope::Local,
				format,
			};
			let artifact = TensorArtifact::build(
				&snapshot.root,
				&index,
				index.build_revision(),
				limits,
				None,
				&mut || Ok(()),
			)
			.unwrap();
			let mut data: [Vec<u8>; 8] = std::array::from_fn(|section| {
				vec![
					0;
					artifact.layout.sections[section].count as usize
						* usize::from(artifact.layout.sections[section].width)
				]
			});
			for (section, bytes) in artifact.layout.sections.iter().zip(&mut data) {
				for (ordinal, output) in
					bytes.chunks_exact_mut(usize::from(section.width)).enumerate()
				{
					artifact.encode_element(section.kind, ordinal as u64, output).unwrap();
				}
			}
			let restored = TensorArtifact::restore(
				&snapshot.root,
				&index,
				artifact.layout.clone(),
				data.clone(),
				limits,
				&mut || Ok(()),
			)
			.unwrap();
			assert_eq!(restored.tensor.atoms(), artifact.tensor.atoms());
			assert_eq!(restored.tensor.connectors(), artifact.tensor.connectors());
			assert_eq!(restored.tensor.source, artifact.tensor.source);
			assert_eq!(restored.tensor.target, artifact.tensor.target);
			assert!(restored.admitted_work > artifact.admitted_work);
			let mut cancelled = 0;
			assert!(
				TensorArtifact::restore(
					&snapshot.root,
					&index,
					artifact.layout.clone(),
					data.clone(),
					limits,
					&mut || {
						cancelled += 1;
						Err(invalid("cancelled restore"))
					}
				)
				.is_err()
			);
			assert_eq!(cancelled, 1);
			assert!(
				TensorArtifact::restore(
					&snapshot.root,
					&index,
					artifact.layout.clone(),
					data.clone(),
					crate::graph::tensor::GraphAlgorithmLimits {
						max_work: artifact.admitted_work,
						..limits
					},
					&mut || Ok(())
				)
				.is_err()
			);
			let mut retained = data.clone();
			retained[0].reserve(limits.memory_bytes);
			assert!(
				TensorArtifact::restore(
					&snapshot.root,
					&index,
					artifact.layout.clone(),
					retained,
					limits,
					&mut || Ok(())
				)
				.is_err()
			);
			let mut corrupted = data.clone();
			corrupted[0][0] ^= 1;
			assert!(
				TensorArtifact::restore(
					&snapshot.root,
					&index,
					artifact.layout.clone(),
					corrupted,
					limits,
					&mut || Ok(())
				)
				.is_err()
			);
			let mut changed = snapshot.root.as_ref().clone();
			changed
				.mark_version(ConflictKey::GraphIncidence(graph.id(), edge), changed.commit_ts + 1);
			assert!(
				TensorArtifact::restore(
					&changed,
					&index,
					artifact.layout.clone(),
					data,
					limits,
					&mut || Ok(())
				)
				.is_err()
			);
			assert!(
				TensorArtifact::build(
					&snapshot.root,
					&index,
					index.build_revision(),
					crate::graph::tensor::GraphAlgorithmLimits {
						memory_bytes: 100,
						..limits
					},
					None,
					&mut || Ok(())
				)
				.is_err()
			);
		}
		index.max_bytes = Some(16_383);
		assert!(
			TensorArtifact::build(
				&snapshot.root,
				&index,
				index.build_revision(),
				limits,
				None,
				&mut || Ok(())
			)
			.is_err()
		);
	}
}

/// Captures qualified incidence through the common transactional/root reader.
pub fn capture_tensor<'view>(
	graph: &GraphRef,
	options: TensorOptions,
	view: &mut impl FnMut(&GraphRef) -> FerrosGrynnResult<TransactionGraphView<'view>>,
	read: &dyn Fn(ConflictKey),
	plane: Option<&mut PlaneBuilder<'_>>,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<EndpointTensor> {
	let mut builder = TensorBuilder::new(options, check)?;
	let mut pending = VecDeque::new();
	let mut visited = BTreeSet::new();
	builder.control.admit(512)?;
	pending.push_back(graph.clone());
	while let Some(reference) = pending.pop_front() {
		builder.control.step()?;
		if !visited.insert(reference.id()) {
			continue;
		}
		builder.control.admit(128)?;
		read(ConflictKey::GraphDefinition(reference.id()));
		let graph_view = view(&reference)?;
		builder.bind(&graph_view)?;
		for id in graph_view.node_ids()?.chain(graph_view.edge_ids()?) {
			builder.atom(&graph_view, id?, true)?;
		}
		for id in graph_view.edge_ids()? {
			let id = id?;
			let atom = builder.atom(&graph_view, id, true)?;
			let edge = graph_view.edge(id)?.ok_or_else(|| invalid("tensor connector is absent"))?;
			let topology = edge.topology()?;
			let mut source = builder.endpoint_buffer(topology.source().len())?;
			for endpoint in topology.source() {
				source.push(tensor_endpoint(&reference, endpoint, &mut builder, view, read)?);
			}
			let mut target = builder.endpoint_buffer(topology.target().len())?;
			for endpoint in topology.target() {
				target.push(tensor_endpoint(&reference, endpoint, &mut builder, view, read)?);
			}
			let label = edge.label_text()?;
			builder.control.admit(label.as_ref().map_or(0, |value| value.len()))?;
			builder.connector(InputConnector {
				atom,
				direction: topology.direction(),
				source,
				target,
				label: label.map(std::borrow::Cow::into_owned),
				weight: edge.weight().unwrap_or(Number::Int(1)),
			})?;
		}
		{
			for id in graph_view.node_ids()?.chain(graph_view.edge_ids()?) {
				let id = id?;
				builder.control.step()?;
				if let Some(child) = nested_graph_reference(&graph_view, id)? {
					read(ConflictKey::GraphDefinition(child.id()));
					let owner = builder.atom(&graph_view, id, true)?;
					let child_view = view(&child)?;
					builder.containment(owner, &child_view)?;
					if options.scope != TensorScope::Rooted {
						continue;
					}
					builder.control.admit(512)?;
					pending.try_reserve(1).map_err(|cause| {
						resource("tensor scope queue", options.limits.memory_bytes as u64)
							.with_technical_source(cause)
					})?;
					pending.push_back(child);
				}
			}
		}
	}
	match plane {
		Some(plane) => builder.finish_with_planes(plane),
		None => builder.finish(),
	}
}

/// Resolves every qualified owner using the same captured root provider.
fn tensor_endpoint<'view>(
	origin: &GraphRef,
	endpoint: &EdgeEndpoint,
	builder: &mut TensorBuilder<'_>,
	view: &mut impl FnMut(&GraphRef) -> FerrosGrynnResult<TransactionGraphView<'view>>,
	read: &dyn Fn(ConflictKey),
) -> FerrosGrynnResult<InputEndpoint> {
	let (scope, terminal): (&[AtomId], AtomId) = match endpoint {
		EdgeEndpoint::Local(id) => (&[], *id),
		EdgeEndpoint::Nested(value) => (value.scope(), value.atom()),
	};
	builder.control.admit(512)?;
	let mut reference = origin.clone();
	let mut owners = builder.control.array::<TensorAtom>(scope.len())?;
	for owner in scope {
		builder.control.step()?;
		read(ConflictKey::GraphDefinition(reference.id()));
		let graph_view = view(&reference)?;
		builder.bind(&graph_view)?;
		let atom = builder.atom(&graph_view, *owner, false)?;
		let child = nested_graph_reference(&graph_view, *owner)?
			.ok_or_else(|| invalid("tensor endpoint owner has no child graph"))?;
		read(ConflictKey::GraphDefinition(child.id()));
		let child_view = view(&child)?;
		builder.control.admit(atom.heap_bytes().checked_add(512).ok_or_else(overflow)?)?;
		builder.containment(atom.clone(), &child_view)?;
		owners.push(atom);
		reference = child;
	}
	read(ConflictKey::GraphDefinition(reference.id()));
	let graph_view = view(&reference)?;
	builder.bind(&graph_view)?;
	Ok(InputEndpoint {
		atom: builder.atom(&graph_view, terminal, false)?,
		owners,
	})
}
