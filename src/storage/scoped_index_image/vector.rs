//! Vector side-file inputs derived from one immutable database root.

use std::{
	collections::{BTreeMap, BTreeSet},
	path::Path,
	time::Instant,
};

use rapidhash::v3::rapidhash_v3;

use super::{
	ScopedIndexFile,
	ScopedIndexImages,
	ScopedIndexKind,
	StreamedScopedIndexFile,
	StreamedScopedIndexImages,
	local_address,
	stream::write_streamed_file,
};
use crate::{
	datastore::{
		ClassId,
		ClassIndexKind,
		ClassIndexOptions,
		ClassIndexState,
		DatabaseRoot,
		IndexId,
		IndexScope,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::vector_index::{
		VectorBuildSpec,
		VectorDocument,
		build_vector_graph_with_distance,
		build_vector_index_with_distance,
		write_vector_index_stream,
	},
	values::{PropertyKeyId, Value, Vector, storage::DiskAtomRef},
};

/// Vector source state.
struct VectorSource {
	/// Spec associated with this vector source.
	spec: VectorBuildSpec,
	/// Kind associated with this vector source.
	kind: ClassIndexKind,
	/// Relative path associated with this vector source.
	relative_path: String,
}

/// Builds every query-visible HNSW/DiskANN definition into buffered pages.
pub(in crate::storage) fn build_vector_index_images(
	database: &DatabaseRoot,
	skipped: &BTreeSet<IndexId>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<ScopedIndexImages> {
	let mut images = ScopedIndexImages::default();
	for source in vector_sources(database, skipped)? {
		let scope = source.spec.scope;
		let kind = explicit_kind(&source);
		let built = build_vector_index_with_distance(
			source.spec,
			vector_documents(database, &source)?,
			distance_function,
		)?;
		let file = ScopedIndexFile {
			scope,
			relative_path: source.relative_path,
			kind,
			root: built.root,
			pages: built.pages,
		};
		push_buffered(&mut images, scope, file);
	}
	Ok(images)
}

/// Writes every query-visible vector base directly into an unreachable
/// generation without retaining encoded pages in memory.
pub(in crate::storage) fn write_streamed_vector_index_images(
	database: &DatabaseRoot,
	skipped: &BTreeSet<IndexId>,
	database_root: &Path,
	generation: &str,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<StreamedScopedIndexImages> {
	let mut images = StreamedScopedIndexImages::default();
	for source in vector_sources(database, skipped)? {
		let started = Instant::now();
		let scope = source.spec.scope;
		let kind = explicit_kind(&source);
		let prefix = match source.spec.scope {
			IndexScope::Base => format!("atoms/{generation}"),
			IndexScope::Effective {
				graph,
			} => format!("graphs/{:016x}/{generation}", graph.get()),
		};
		let path = format!("{prefix}/{}", source.relative_path);
		let (entrypoint, nodes) = build_vector_graph_with_distance(
			source.spec,
			vector_documents(database, &source)?,
			distance_function,
		)?;
		let built = write_streamed_file(database_root, &path, |file| {
			write_vector_index_stream(file, source.spec, entrypoint, &nodes)
		})?;
		let file = StreamedScopedIndexFile {
			scope,
			path,
			kind,
			root: built.root,
			page_count: built.page_count,
		};
		push_streamed(&mut images, scope, file);
		if images.index_timings.insert(source.spec.index, started.elapsed()).is_some() {
			return Err(invalid("duplicate vector IndexId while recording build timing"));
		}
	}
	Ok(images)
}

/// Returns the vector sources result.
fn vector_sources(
	database: &DatabaseRoot,
	skipped: &BTreeSet<IndexId>,
) -> FerrosGrynnResult<Vec<VectorSource>> {
	let mut sources = Vec::new();
	let mut ordinals = BTreeMap::<(IndexScope, u8), u32>::new();
	for class in database.schema.classes() {
		let class_id = class.id().ok_or_else(|| invalid("vector class has no ClassId"))?;
		for index in class.indexes().filter(|index| {
			matches!(index.kind(), ClassIndexKind::Hnsw | ClassIndexKind::DiskAnn)
				&& index.state() == ClassIndexState::Ready
		}) {
			let id = index.id().ok_or_else(|| invalid("vector definition has no IndexId"))?;
			if skipped.contains(&id) {
				continue;
			}
			let options = match index.kind() {
				ClassIndexKind::Hnsw => ClassIndexOptions::Hnsw(
					index
						.hnsw_options()
						.ok_or_else(|| invalid("HNSW definition has no kind-specific options"))?,
				),
				ClassIndexKind::DiskAnn => {
					ClassIndexOptions::DiskAnn(index.disk_ann_options().ok_or_else(|| {
						invalid("DiskANN definition has no kind-specific options")
					})?)
				}
				_ => continue,
			};
			let field_name =
				index.fields().next().ok_or_else(|| invalid("vector definition has no field"))?;
			let field = class
				.fields()
				.find(|field| field.name() == field_name)
				.ok_or_else(|| invalid("vector definition field is absent from its class"))?;
			let key = field.key.ok_or_else(|| invalid("vector field has no PropertyKeyId"))?;
			let spec = VectorBuildSpec {
				index: id,
				index_version: index.version(),
				scope: index.scope(),
				class: class_id,
				field: key,
				options,
				build_revision: index.build_revision(),
				build_seed: build_seed(
					id,
					index.version(),
					database.commit_ts,
					index.build_revision(),
				),
			};
			let kind_tag = u8::from(index.kind() != ClassIndexKind::Hnsw);
			let ordinal = ordinals.entry((index.scope(), kind_tag)).or_default();
			let current = *ordinal;
			*ordinal =
				ordinal.checked_add(1).ok_or_else(|| invalid("vector file ordinal exceeds u32"))?;
			let prefix = if index.kind() == ClassIndexKind::Hnsw {
				"hnsw"
			} else {
				"diskann"
			};
			sources.push(VectorSource {
				spec,
				kind: index.kind(),
				relative_path: format!("idx/{prefix}_{current:04}.fgidx"),
			});
		}
	}
	Ok(sources)
}

/// Returns the vector documents result.
fn vector_documents<'a>(
	database: &'a DatabaseRoot,
	source: &VectorSource,
) -> FerrosGrynnResult<Vec<VectorDocument<&'a Vector>>> {
	match source.spec.scope {
		IndexScope::Base => base_documents(database, source.spec.class, source.spec.field),
		IndexScope::Effective {
			graph,
		} => effective_documents(database, graph, source.spec.class, source.spec.field),
	}
}

/// Returns the explicit kind result.
fn explicit_kind(source: &VectorSource) -> ScopedIndexKind {
	ScopedIndexKind::Explicit {
		index: source.spec.index,
		version: source.spec.index_version,
		class: source.spec.class,
		kind: source.kind,
	}
}

/// Pushes buffered.
fn push_buffered(images: &mut ScopedIndexImages, scope: IndexScope, file: ScopedIndexFile) {
	match scope {
		IndexScope::Base => images.base.push(file),
		IndexScope::Effective {
			graph,
		} => images.graphs.entry(graph).or_default().push(file),
	}
}

/// Pushes streamed.
fn push_streamed(
	images: &mut StreamedScopedIndexImages,
	scope: IndexScope,
	file: StreamedScopedIndexFile,
) {
	match scope {
		IndexScope::Base => images.base.push(file),
		IndexScope::Effective {
			graph,
		} => images.graphs.entry(graph).or_default().push(file),
	}
}

/// Returns the base documents result.
fn base_documents(
	database: &DatabaseRoot,
	class: ClassId,
	key: PropertyKeyId,
) -> FerrosGrynnResult<Vec<VectorDocument<&Vector>>> {
	database
		.atoms
		.atoms()
		.filter(|atom| atom.class_id() == class)
		.filter_map(|atom| match atom.properties().get(key) {
			None | Some(Value::Null) => None,
			Some(Value::Vector(vector)) => {
				Some(DiskAtomRef::from_parts(atom.kind(), atom.id().slot()).map(|atom| {
					VectorDocument {
						atom,
						vector,
					}
				}))
			}
			Some(_) => Some(Err(invalid("vector base field contains a non-vector value"))),
		})
		.collect()
}

/// Returns the effective documents result.
fn effective_documents(
	database: &DatabaseRoot,
	graph: crate::graph::GraphId,
	class: ClassId,
	key: PropertyKeyId,
) -> FerrosGrynnResult<Vec<VectorDocument<&Vector>>> {
	let graph_root = database
		.graph(graph)
		.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
	graph_root
		.occurrences()
		.filter_map(|occurrence| {
			let atom = match database.atom(occurrence.global_id()) {
				Some(atom) if atom.class_id() == class => atom,
				Some(_) => return None,
				None => {
					return Some(Err(invalid("vector occurrence references an absent base atom")));
				}
			};
			match occurrence.effective_value(atom.properties().get(key), key) {
				None | Some(Value::Null) => None,
				Some(Value::Vector(vector)) => {
					Some(local_address(occurrence.local_id()).map(|atom| VectorDocument {
						atom,
						vector,
					}))
				}
				Some(_) => Some(Err(invalid("vector effective field contains a non-vector value"))),
			}
		})
		.collect()
}

/// Builds seed.
fn build_seed(
	index: crate::datastore::IndexId,
	version: crate::datastore::SchemaVersion,
	source_revision: u64,
	build_revision: u64,
) -> u64 {
	let mut bytes = Vec::with_capacity(24);
	bytes.extend_from_slice(&index.get().to_le_bytes());
	bytes.extend_from_slice(&version.get().to_le_bytes());
	bytes.extend_from_slice(&source_revision.to_le_bytes());
	bytes.extend_from_slice(&build_revision.to_le_bytes());
	rapidhash_v3(&bytes)
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
