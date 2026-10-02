//! Geometry side-files built from one immutable BASE/EFFECTIVE root.

use std::{
	collections::{BTreeMap, BTreeSet},
	path::Path,
	time::Instant,
};

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
	datastore::{ClassIndexKind, ClassIndexState, DatabaseRoot, IndexId, IndexScope},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		geometry_index::{GeometryBuildSpec, GeometryEntry, stream_geometry_index},
		page::AlignedPage,
	},
	values::{Value, storage::DiskAtomRef},
};

/// One declared field and its safe scope-local filename.
struct GeometrySource {
	/// Definition and snapshot metadata for the builder.
	spec: GeometryBuildSpec,
	/// Safe scope-local physical filename.
	relative_path: String,
}

/// Writes query-visible bases directly into the unpublished COW generation.
pub(in crate::storage) fn write_streamed_geometry_index_images(
	database: &DatabaseRoot,
	skipped: &BTreeSet<IndexId>,
	database_root: &Path,
	generation: &str,
) -> FerrosGrynnResult<StreamedScopedIndexImages> {
	let mut images = StreamedScopedIndexImages::default();
	for source in sources(database, skipped)? {
		let started = Instant::now();
		let prefix = match source.spec.scope {
			IndexScope::Base => format!("atoms/{generation}"),
			IndexScope::Effective {
				graph,
			} => format!("graphs/{:016x}/{generation}", graph.get()),
		};
		let path = format!("{prefix}/{}", source.relative_path);
		let built = write_streamed_file(database_root, &path, |file| {
			stream_geometry_index(
				source.spec,
				database_root,
				|consumer| visit(database, source.spec, consumer),
				|page_no, page| file.write_page(page_no, page),
			)
		})?;
		let file = StreamedScopedIndexFile {
			scope: source.spec.scope,
			path,
			kind: explicit_kind(source.spec),
			root: built.root,
			page_count: built.page_count,
		};
		match source.spec.scope {
			IndexScope::Base => images.base.push(file),
			IndexScope::Effective {
				graph,
			} => images.graphs.entry(graph).or_default().push(file),
		}
		if images.index_timings.insert(source.spec.index, started.elapsed()).is_some() {
			return Err(invalid("duplicate geometry IndexId"));
		}
	}
	Ok(images)
}

/// Buffered generation control; production publication uses the streamed
/// writer.
pub(in crate::storage) fn build_geometry_index_images(
	database: &DatabaseRoot,
	skipped: &BTreeSet<IndexId>,
) -> FerrosGrynnResult<ScopedIndexImages> {
	let mut images = ScopedIndexImages::default();
	for source in sources(database, skipped)? {
		let mut nodes = 0_u64;
		visit(database, source.spec, &mut |_| {
			nodes = nodes.checked_add(1).ok_or_else(|| invalid("geometry row count overflows"))?;
			Ok(())
		})?;
		let mut maximum_pages = 1_u64;
		while nodes > 0 {
			nodes = nodes.div_ceil(387);
			maximum_pages = maximum_pages
				.checked_add(nodes)
				.ok_or_else(|| invalid("geometry page count overflow"))?;
			if nodes == 1 {
				break;
			}
		}
		let count = usize::try_from(maximum_pages)
			.map_err(|_error| invalid("geometry page count exceeds usize"))?;
		let mut pages = Vec::new();
		pages.try_reserve_exact(count).map_err(|cause| {
			invalid("geometry page allocation failed").with_technical_source(cause)
		})?;
		pages.resize_with(count, AlignedPage::zeroed);
		let built = stream_geometry_index(
			source.spec,
			&std::env::temp_dir(),
			|consumer| visit(database, source.spec, consumer),
			|page_no, page| {
				let output = pages
					.get_mut(
						usize::try_from(page_no.get())
							.map_err(|_error| invalid("geometry page exceeds usize"))?,
					)
					.ok_or_else(|| invalid("geometry page count exceeds its bound"))?;
				output.as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			},
		)?;
		pages.truncate(
			usize::try_from(built.page_count)
				.map_err(|_error| invalid("geometry page count exceeds usize"))?,
		);
		let file = ScopedIndexFile {
			scope: source.spec.scope,
			relative_path: source.relative_path,
			kind: explicit_kind(source.spec),
			root: built.root,
			pages,
		};
		match source.spec.scope {
			IndexScope::Base => images.base.push(file),
			IndexScope::Effective {
				graph,
			} => images.graphs.entry(graph).or_default().push(file),
		}
	}
	Ok(images)
}

/// Resolves versioned field identities and assigns scope-local ordinals.
fn sources(
	database: &DatabaseRoot,
	skipped: &BTreeSet<IndexId>,
) -> FerrosGrynnResult<Vec<GeometrySource>> {
	let mut sources = Vec::new();
	let mut ordinals = BTreeMap::<IndexScope, u32>::new();
	for class in database.schema.classes() {
		for index in class.indexes().filter(|index| {
			index.kind() == ClassIndexKind::Geometry && index.state() == ClassIndexState::Ready
		}) {
			let id = index.id().ok_or_else(|| invalid("geometry definition is unpublished"))?;
			if skipped.contains(&id) {
				continue;
			}
			let name =
				index.fields().next().ok_or_else(|| invalid("geometry definition has no field"))?;
			let field = class
				.fields()
				.find(|field| field.name() == name)
				.and_then(|field| field.key)
				.ok_or_else(|| invalid("geometry field has no property identity"))?;
			let ordinal = ordinals.entry(index.scope()).or_default();
			let relative_path = format!("idx/geo_{ordinal:04}.fgidx");
			*ordinal =
				ordinal.checked_add(1).ok_or_else(|| invalid("geometry ordinal overflow"))?;
			sources.push(GeometrySource {
				relative_path,
				spec: GeometryBuildSpec {
					index: id,
					index_version: index.version(),
					class: class.id().ok_or_else(|| invalid("geometry class is unpublished"))?,
					field,
					scope: index.scope(),
					options: index
						.geometry_options()
						.ok_or_else(|| invalid("geometry options are absent"))?,
					build_revision: index.build_revision(),
				},
			});
		}
	}
	Ok(sources)
}

/// Emits one exact MBR per nonempty geometry without retaining source rows.
fn visit(
	database: &DatabaseRoot,
	spec: GeometryBuildSpec,
	consumer: &mut dyn FnMut(GeometryEntry) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let mut emit = |address: DiskAtomRef, value: Option<&Value>| match value {
		None | Some(Value::None | Value::Null) => Ok(()),
		Some(Value::Geometry(geometry)) => {
			spec.options.validate(geometry)?;
			if let Some(mbr) = geometry.mbr()? {
				consumer(GeometryEntry {
					mbr,
					identity: address.raw(),
				})?;
			}
			Ok(())
		}
		Some(_) => Err(FerrosGrynnError::invalid_argument(
			"geometry index field contains a non-geometry value",
		)),
	};
	match spec.scope {
		IndexScope::Base => {
			for atom in database.atoms.atoms().filter(|atom| atom.class_id() == spec.class) {
				emit(
					DiskAtomRef::from_parts(atom.kind(), atom.id().slot())?,
					atom.properties().get(spec.field),
				)?;
			}
		}
		IndexScope::Effective {
			graph,
		} => {
			let root = database.graph(graph).ok_or_else(|| invalid("geometry graph is absent"))?;
			for occurrence in root.occurrences() {
				let atom = database
					.atom(occurrence.global_id())
					.ok_or_else(|| invalid("geometry occurrence has no base atom"))?;
				if atom.class_id() == spec.class {
					emit(
						local_address(occurrence.local_id())?,
						occurrence.effective_value(atom.properties().get(spec.field), spec.field),
					)?;
				}
			}
		}
	}
	Ok(())
}

/// Keeps geometry bindings in the existing explicit-index registry.
fn explicit_kind(spec: GeometryBuildSpec) -> ScopedIndexKind {
	ScopedIndexKind::Explicit {
		index: spec.index,
		version: spec.index_version,
		class: spec.class,
		kind: ClassIndexKind::Geometry,
	}
}
/// Redacted generation-build error.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
