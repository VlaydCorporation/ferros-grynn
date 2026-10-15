//! Complete qualified tensor generations over the shared physical section
//! codecs.

use std::{path::Path, sync::Arc};

use super::{
	DatabaseRoot,
	GraphIndex,
	GraphIndexDefinition,
	TensorCacheFormat,
	TransactionGraphView,
	transaction::capture_tensor,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		EdgeDirection,
		GraphId,
		SnapshotId,
		tensor::{
			CsrMatrix,
			EndpointTensor,
			GraphAlgorithmLimits,
			TensorAtom,
			TensorConnector,
			TensorOptions,
			TensorScope,
			build::PlaneBuilder,
			control::{Control, overflow, resource},
		},
	},
	storage::{
		DerivedSectionKind,
		GraphArtifactIdentity,
		TensorCacheHeader,
		TensorCacheLayout,
		TensorCacheTag,
	},
	values::Number,
};

/// One immutable full ATOM capture; NODE and adjacency remain logical views.
#[derive(Debug)]
pub struct TensorArtifact {
	/// Exact full-page packing.
	pub(crate) layout: TensorCacheLayout,
	/// Complete canonical semantic definition, including scope and byte cap.
	pub(crate) descriptor: Vec<u8>,
	/// Qualified axes, incidence topology, weights and containment bytes.
	metadata: Vec<u8>,
	/// Immutable runtime data with owned public identities.
	pub(crate) tensor: EndpointTensor,
	/// Durable graph incidence revisions, including boundary/containment
	/// bindings.
	pub(crate) dependencies: Vec<(GraphId, u64)>,
	/// Aggregate build admission carried into restoration and publication.
	pub(crate) admitted_work: u64,
	/// Aggregate retained and scratch admission for this capture.
	pub(crate) admitted_bytes: usize,
}

impl TensorArtifact {
	/// Captures incidence and chooses the smaller admitted encoded AUTO format.
	///
	/// # Errors
	/// Returns invalid definitions, snapshot errors, resource caps or
	/// cancellation.
	pub(crate) fn build(
		database: &DatabaseRoot,
		index: &GraphIndex,
		revision: u64,
		limits: GraphAlgorithmLimits,
		sort_root: Option<&Path>,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		Self::build_format(database, index, revision, limits, sort_root, None, check)
	}

	/// Restores the recorded AUTO representation without changing its
	/// descriptor.
	fn build_format(
		database: &DatabaseRoot,
		index: &GraphIndex,
		revision: u64,
		mut limits: GraphAlgorithmLimits,
		sort_root: Option<&Path>,
		recorded: Option<TensorCacheTag>,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		index.validate()?;
		let GraphIndexDefinition::Tensor {
			scope,
			format,
		} = *index.definition()
		else {
			return Err(invalid("tensor builder received another definition family"));
		};
		let format = match (format, recorded) {
			(TensorCacheFormat::Auto, Some(TensorCacheTag::Csr)) => TensorCacheFormat::Csr,
			(TensorCacheFormat::Auto, Some(TensorCacheTag::Dense)) => TensorCacheFormat::Dense,
			(TensorCacheFormat::Csr, Some(TensorCacheTag::Dense))
			| (TensorCacheFormat::Dense, Some(TensorCacheTag::Csr)) => {
				return Err(invalid("tensor recorded format differs from explicit definition"));
			}
			(format, _) => format,
		};
		let reference = index
			.graph()
			.and_then(|id| database.graph(id))
			.ok_or_else(|| invalid("tensor definition graph is absent"))?
			.reference();
		let scratch_bytes = limits.memory_bytes;
		let fixed = database
			.graphs
			.len()
			.checked_mul(128)
			.and_then(|bytes| bytes.checked_add(65_536))
			.ok_or_else(overflow)?;
		let descriptor = index.descriptor_with_limit(
			scratch_bytes
				.checked_sub(fixed)
				.ok_or_else(|| resource("tensor build bytes", scratch_bytes as u64))?,
		)?;
		let fixed = fixed.checked_add(descriptor.len()).ok_or_else(overflow)?;
		limits.memory_bytes = scratch_bytes
			.checked_sub(fixed)
			.filter(|bytes| *bytes > 0)
			.ok_or_else(|| resource("tensor build bytes", scratch_bytes as u64))?;
		// The shallow root copy retains the existing immutable graph/atom
		// storage.
		let root = Arc::new(database.clone());
		let snapshot = SnapshotId::new(database.commit_ts, 0);
		let mut sorted = |rows: usize,
		                  connectors: &[TensorConnector],
		                  source: bool,
		                  control: &mut Control<'_>| {
			sorted_plane(
				sort_root.ok_or_else(|| invalid("tensor sort root is absent"))?,
				rows,
				connectors,
				source,
				control,
			)
		};
		let plane = sort_root.is_some().then_some(&mut sorted as &mut PlaneBuilder<'_>);
		let tensor = capture_tensor(
			reference,
			TensorOptions {
				scope,
				limits,
			},
			&mut |graph| TransactionGraphView::for_root(root.clone(), graph, snapshot),
			&|_| {},
			plane,
			check,
		)?;
		limits.memory_bytes = scratch_bytes;
		let mut control = Control::new(limits, check)?;
		control.admit(fixed.checked_add(tensor.admitted_bytes()).ok_or_else(overflow)?)?;
		control.work(usize::try_from(tensor.work()).map_err(|_cause| overflow())?)?;
		let mut dependencies = control.array(tensor.bindings.len())?;
		for binding in &tensor.bindings {
			control.step()?;
			let graph =
				binding.graph().ok_or_else(|| invalid("cache binding is not catalog-owned"))?.id();
			let revision = database
				.graph(graph)
				.ok_or_else(|| invalid("cache dependency graph is absent"))?
				.tensor_revision;
			dependencies.push((graph, revision));
		}
		let mut metadata_bytes = 0_usize;
		visit_metadata(database, &tensor, &dependencies, &mut |bytes| {
			control.work(bytes.len())?;
			metadata_bytes = metadata_bytes.checked_add(bytes.len()).ok_or_else(overflow)?;
			Ok(())
		})?;
		let mut metadata = control.array(metadata_bytes)?;
		visit_metadata(database, &tensor, &dependencies, &mut |bytes| {
			control.work(bytes.len())?;
			metadata.extend_from_slice(bytes);
			Ok(())
		})?;
		let identity = GraphArtifactIdentity {
			index: index.id().map_or(0, |id| id.get()),
			version: index.version().get(),
			graph: reference.id().get(),
			revision,
			fingerprint: rapidhash::v3::rapidhash_v3(&descriptor),
		};
		let header = TensorCacheHeader {
			identity,
			rows: tensor.atoms.len() as u64,
			connectors: tensor.connectors.len() as u64,
			source_cells: tensor.source.stored_cells() as u64,
			target_cells: tensor.target.stored_cells() as u64,
			metadata_bytes: metadata.len() as u64,
			descriptor_bytes: descriptor.len() as u64,
			max_bytes: index.max_bytes().ok_or_else(|| invalid("tensor MAX_BYTES is absent"))?,
			format: TensorCacheTag::Csr,
			rooted: scope == TensorScope::Rooted,
		};
		let sparse = || TensorCacheLayout::new(header);
		let dense = || {
			TensorCacheLayout::new(TensorCacheHeader {
				format: TensorCacheTag::Dense,
				..header
			})
		};
		let layout = match format {
			TensorCacheFormat::Csr => sparse()?,
			TensorCacheFormat::Dense => dense()?,
			TensorCacheFormat::Auto => match (
				admissible_encoding(sparse(), &control),
				admissible_encoding(dense(), &control),
			) {
				(Ok(csr), Ok(dense)) => {
					if dense.page_count < csr.page_count {
						dense
					} else {
						csr
					}
				}
				(Ok(layout), Err(_)) | (Err(_), Ok(layout)) => layout,
				(Err(error), Err(_)) => return Err(error),
			},
		};
		admit_encoding(&layout, &mut control)?;
		Ok(Self {
			layout,
			descriptor,
			metadata,
			tensor,
			dependencies,
			admitted_work: control.work,
			admitted_bytes: control.bytes,
		})
	}

	/// Restores only after comparing complete logical bytes and dependency
	/// revisions.
	///
	/// # Errors
	/// Rejects stale/malformed captures, full-descriptor mismatch or resource
	/// exhaustion.
	pub(crate) fn restore(
		database: &DatabaseRoot,
		index: &GraphIndex,
		layout: TensorCacheLayout,
		data: [Vec<u8>; 8],
		limits: GraphAlgorithmLimits,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		if TensorCacheLayout::new(layout.header)? != layout {
			return Err(invalid("tensor layout is noncanonical"));
		}
		for (section, bytes) in layout.sections.iter().zip(&data) {
			if section.count.checked_mul(u64::from(section.width)) != Some(bytes.len() as u64) {
				return Err(invalid("tensor section length differs from its extent"));
			}
		}
		let scratch_bytes = limits.memory_bytes;
		let mut control = Control::new(limits, check)?;
		let payload = data
			.iter()
			.try_fold(65_536_usize, |total, bytes| total.checked_add(bytes.capacity()))
			.ok_or_else(overflow)?;
		control.admit(payload)?;
		let membership_work = layout
			.sections
			.iter()
			.skip(2)
			.try_fold(0_u64, |total, section| total.checked_add(section.count))
			.ok_or_else(overflow)?;
		control.work(usize::try_from(membership_work).map_err(|_cause| overflow())?)?;
		layout.validate_membership(&data)?;
		let remaining = scratch_bytes
			.checked_sub(control.bytes)
			.filter(|bytes| *bytes > 0)
			.ok_or_else(|| resource("tensor restore bytes", scratch_bytes as u64))?;
		// shortcut: reopen rescans the exact incidence oracle to validate
		// metadata; decode dictionaries directly only if measured
		// reopen latency requires it.
		let build_limits = GraphAlgorithmLimits {
			memory_bytes: remaining,
			max_work: limits
				.max_work
				.checked_sub(control.work)
				.filter(|work| *work > 0)
				.ok_or_else(|| resource("tensor restore work", limits.max_work))?,
			..limits
		};
		let mut artifact = Self::build_format(
			database,
			index,
			index.build_revision(),
			build_limits,
			None,
			Some(layout.header.format),
			&mut || control.poll(),
		)?;
		control.admit(artifact.admitted_bytes)?;
		control.work(usize::try_from(artifact.admitted_work).map_err(|_cause| overflow())?)?;
		control.work(
			artifact.metadata.len().checked_add(artifact.descriptor.len()).ok_or_else(overflow)?,
		)?;
		if artifact.layout != layout
			|| artifact.metadata != data[0]
			|| artifact.descriptor != data[1]
		{
			return Err(invalid(
				"tensor cache descriptor, dependencies or topology differ from snapshot",
			));
		}
		for (section, bytes) in layout.sections.iter().zip(&data).skip(2) {
			for (ordinal, expected) in bytes.chunks_exact(usize::from(section.width)).enumerate() {
				control.step()?;
				let mut actual = [0_u8; 8];
				artifact.encode_element(
					section.kind,
					ordinal as u64,
					&mut actual[..expected.len()],
				)?;
				if actual[..expected.len()] != *expected {
					return Err(invalid("tensor membership differs from captured topology"));
				}
			}
		}
		artifact.admitted_work = control.work;
		artifact.admitted_bytes = control.bytes;
		Ok(artifact)
	}

	/// Writes one already admitted element into the shared section stream.
	///
	/// # Errors
	/// Rejects forged kinds, ordinals, dimensions or destination widths.
	pub(crate) fn encode_element(
		&self,
		kind: DerivedSectionKind,
		ordinal: u64,
		output: &mut [u8],
	) -> FerrosGrynnResult<()> {
		let section = self
			.layout
			.sections
			.iter()
			.find(|section| section.kind == kind)
			.ok_or_else(|| invalid("tensor section kind is absent"))?;
		if ordinal >= section.count || output.len() != usize::from(section.width) {
			return Err(invalid("tensor element ordinal or width is invalid"));
		}
		let ordinal = usize::try_from(ordinal).map_err(|_cause| overflow())?;
		match kind {
			DerivedSectionKind::DefinitionDescriptor => output[0] = self.descriptor[ordinal],
			DerivedSectionKind::TensorMetadata => output[0] = self.metadata[ordinal],
			DerivedSectionKind::TensorSourceOffsets => output
				.copy_from_slice(&(self.tensor.source.offsets()[ordinal] as u64).to_le_bytes()),
			DerivedSectionKind::TensorTargetOffsets => output
				.copy_from_slice(&(self.tensor.target.offsets()[ordinal] as u64).to_le_bytes()),
			DerivedSectionKind::TensorSourceOrdinals => output.copy_from_slice(
				&(self.tensor.source.column_indices()[ordinal] as u64).to_le_bytes(),
			),
			DerivedSectionKind::TensorTargetOrdinals => output.copy_from_slice(
				&(self.tensor.target.column_indices()[ordinal] as u64).to_le_bytes(),
			),
			DerivedSectionKind::TensorSourceBits | DerivedSectionKind::TensorTargetBits => {
				let plane = if kind == DerivedSectionKind::TensorSourceBits {
					&self.tensor.source
				} else {
					&self.tensor.target
				};
				let (rows, columns) = plane.shape();
				let cells = rows.checked_mul(columns).ok_or_else(overflow)?;
				let mut byte = 0_u8;
				for bit in 0..8 {
					let cell = ordinal
						.checked_mul(8)
						.and_then(|at| at.checked_add(bit))
						.ok_or_else(overflow)?;
					if cell < cells && plane.get(cell / columns, cell % columns)?.is_some() {
						byte |= 1 << bit;
					}
				}
				output[0] = byte;
			}
			_ => return Err(invalid("tensor writer received another artifact section")),
		}
		Ok(())
	}
}

/// Packs membership directly from the existing bounded external-sort stream.
fn sorted_plane(
	root: &Path,
	rows: usize,
	connectors: &[TensorConnector],
	source: bool,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<CsrMatrix<bool>> {
	control.work(connectors.len())?;
	let count = connectors.iter().try_fold(0_usize, |count, connector| {
		connector.plane_endpoints(source).iter().try_fold(count, |count, endpoints| {
			count.checked_add(endpoints.len()).ok_or_else(overflow)
		})
	})?;
	let mut offsets = control.array(rows.checked_add(1).ok_or_else(overflow)?)?;
	let mut columns = control.array(count)?;
	let mut values = control.array(count)?;
	control.sort(count)?;
	let remaining = control.limits.memory_bytes.checked_sub(control.bytes).ok_or_else(overflow)?;
	let run_budget =
		(remaining / 16).min(crate::storage::external_sort_run_budget()).clamp(40, 8 << 20);
	let record_bytes = count.checked_mul(40).ok_or_else(overflow)?;
	let workspace = count
		.checked_mul(128)
		.ok_or_else(overflow)?
		.min(run_budget.checked_mul(3).ok_or_else(overflow)?)
		.max(if count == 0 {
			0
		} else {
			256
		})
		.checked_add(if count != 0 && record_bytes >= run_budget {
			4 << 20
		} else {
			0
		})
		.ok_or_else(overflow)?;
	control.admit(workspace)?;
	let mut sorter = crate::storage::ExternalIndexSorter::with_budget(root, run_budget);
	for (column, connector) in connectors.iter().enumerate() {
		control.step()?;
		for endpoint in connector.plane_endpoints(source).into_iter().flatten() {
			control.step()?;
			if endpoint.row >= rows {
				return Err(invalid("tensor sort endpoint exceeds axis"));
			}
			sorter.push_raw((endpoint.row as u64).to_be_bytes().to_vec(), column as u64)?;
		}
	}
	let sorted = sorter.finish()?;
	let mut previous = None;
	sorted.visit_raw(|key, column| {
		control.step()?;
		let row = usize::try_from(u64::from_be_bytes(
			key.try_into().map_err(|_cause| invalid("tensor sort key width is invalid"))?,
		))
		.map_err(|_cause| overflow())?;
		let column = usize::try_from(column).map_err(|_cause| overflow())?;
		if row >= rows || column >= connectors.len() {
			return Err(invalid("tensor sort ordinal exceeds axis"));
		}
		let pair = (row, column);
		if previous.is_some_and(|prior| prior > pair) {
			return Err(invalid("tensor sort order is invalid"));
		}
		if previous == Some(pair) {
			return Ok(());
		}
		while offsets.len() <= row {
			control.step()?;
			offsets.push(columns.len());
		}
		columns.push(column);
		values.push(true);
		previous = Some(pair);
		Ok(())
	})?;
	while offsets.len() <= rows {
		control.step()?;
		offsets.push(columns.len());
	}
	Ok(CsrMatrix::from_canonical((rows, connectors.len()), offsets, columns, values))
}

/// Charges complete encoding/refinement work before a section stream begins.
fn admit_encoding(layout: &TensorCacheLayout, control: &mut Control<'_>) -> FerrosGrynnResult<()> {
	control.work(encoding_work(layout)?)
}

/// Rejects an AUTO candidate before selection when its encoding exceeds work.
fn admissible_encoding(
	layout: FerrosGrynnResult<TensorCacheLayout>,
	control: &Control<'_>,
) -> FerrosGrynnResult<TensorCacheLayout> {
	let layout = layout?;
	let work = u64::try_from(encoding_work(&layout)?).map_err(|_cause| overflow())?;
	if control.work.checked_add(work).is_none_or(|total| total > control.limits.max_work) {
		return Err(resource("tensor encoding work", control.limits.max_work));
	}
	Ok(layout)
}

/// Computes the checked sparse/dense element-encoding bound without work.
fn encoding_work(layout: &TensorCacheLayout) -> FerrosGrynnResult<usize> {
	let count = if layout.header.format == TensorCacheTag::Dense {
		layout
			.header
			.rows
			.checked_mul(layout.header.connectors)
			.and_then(|cells| cells.checked_mul(128))
	} else {
		layout.sections.iter().try_fold(0_u64, |total, section| total.checked_add(section.count))
	};
	usize::try_from(count.ok_or_else(overflow)?).map_err(|_cause| overflow())
}

/// Streams explicit little-endian logical metadata; no executable IR is
/// serialized.
fn visit_metadata(
	database: &DatabaseRoot,
	tensor: &EndpointTensor,
	dependencies: &[(GraphId, u64)],
	emit: &mut impl FnMut(&[u8]) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	emit(b"FGTCMETA")?;
	for count in [
		tensor.root as u64,
		tensor.atoms.len() as u64,
		tensor.connectors.len() as u64,
		dependencies.len() as u64,
		tensor.containment.len() as u64,
	] {
		emit(&count.to_le_bytes())?;
	}
	for (graph, revision) in dependencies {
		emit(&graph.get().to_le_bytes())?;
		emit(&revision.to_le_bytes())?;
	}
	for (row, atom) in tensor.atoms.iter().enumerate() {
		emit_atom(database, atom, emit)?;
		emit(&[
			u8::from(atom.atom().kind() == crate::graph::AtomKind::Node),
			u8::from(tensor.domain[row]),
			0,
			0,
			0,
			0,
			0,
			0,
		])?;
	}
	for connector in &tensor.connectors {
		emit_atom(database, &connector.atom, emit)?;
		emit(&[
			match connector.direction {
				EdgeDirection::Directed => 0,
				EdgeDirection::Bidirectional => 1,
				EdgeDirection::Undirected => 2,
			},
			0,
			0,
			0,
			0,
			0,
			0,
			0,
		])?;
		let label = connector.label.as_ref();
		let length =
			u32::try_from(label.map_or(0, |label| label.len())).map_err(|_cause| overflow())?;
		emit(&length.to_le_bytes())?;
		emit(&[u8::from(label.is_some()), 0, 0, 0])?;
		if let Some(label) = label {
			emit(label.as_bytes())?;
		}
		let mut weight = [0_u8; 24];
		match connector.weight {
			Number::Int(value) => weight[8..16].copy_from_slice(&value.to_le_bytes()),
			Number::Float(value) => {
				weight[0] = 1;
				weight[8..16].copy_from_slice(&value.to_bits().to_le_bytes());
			}
			Number::Decimal(value) => {
				weight[0] = 2;
				weight[8..].copy_from_slice(&value.serialize());
			}
		}
		emit(&weight)?;
		for endpoints in [&connector.source, &connector.target] {
			emit(&(endpoints.len() as u64).to_le_bytes())?;
			for endpoint in endpoints {
				emit(&(endpoint.row as u64).to_le_bytes())?;
				emit(&(endpoint.owners.len() as u64).to_le_bytes())?;
				for owner in &endpoint.owners {
					emit_atom(database, owner, emit)?;
				}
			}
		}
	}
	for containment in &tensor.containment {
		emit_atom(database, &containment.owner, emit)?;
		emit(
			&containment
				.child
				.graph()
				.ok_or_else(|| invalid("cache child is not catalog-owned"))?
				.id()
				.get()
				.to_le_bytes(),
		)?;
	}
	Ok(())
}

/// Stores a qualified graph/global identity only behind the storage boundary.
fn emit_atom(
	database: &DatabaseRoot,
	atom: &TensorAtom,
	emit: &mut impl FnMut(&[u8]) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let occurrence =
		atom.occurrence().ok_or_else(|| invalid("runtime atom cannot be cached durably"))?;
	let global = database
		.resolve_atom(occurrence.atom().id())
		.ok_or_else(|| invalid("tensor atom is absent"))?;
	emit(&occurrence.graph().id().get().to_le_bytes())?;
	emit(&global.raw().to_le_bytes())
}

/// Typed secondary corruption at cache capture/restore boundaries.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
