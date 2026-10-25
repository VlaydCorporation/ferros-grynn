//! Bounded page-at-a-time output for immutable import index generations.

use std::{
	collections::BTreeMap,
	fs,
	path::Path,
	time::{Duration, Instant},
};

use super::{
	ScopedIndexKind,
	StreamedScopedIndexFile,
	StreamedScopedIndexImages,
	build::{explicit_index_paths, label_filename, label_ids, local_address},
};
use crate::{
	datastore::{DatabaseRoot, IndexBinding, IndexEntries, IndexScope},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind},
	storage::{
		direct::DirectFile,
		graph_index::{
			CountIndexScope,
			CountIndexValue,
			ExternalIndexSorter,
			PrimaryAddress,
			SortedIndexRecords,
			StreamedIndex,
			encode_count_index,
			stream_label_index,
			stream_standard_index,
			stream_unique_index,
		},
		page::{AlignedPage, PageLsn, PageNo},
	},
	values::storage::DiskAtomRef,
};

/// Writes one unique B+tree directly into an unreachable generation file.
pub(in crate::storage) fn write_streamed_unique_file<Address: PrimaryAddress>(
	records: &SortedIndexRecords,
	database_root: &Path,
	relative_path: &str,
) -> FerrosGrynnResult<StreamedIndex> {
	write_streamed_file(database_root, relative_path, |file| {
		stream_unique_index::<Address>(records, database_root, PageLsn::ZERO, |page_no, page| {
			write_next(file, page_no, page)
		})
	})
}

/// Streams one engine-owned class or label membership without a full scope
/// build.
///
/// # Errors
/// Propagates address validation, bounded sorting, codec and file-sync errors.
pub(in crate::storage) fn write_streamed_membership_file(
	database_root: &Path,
	relative_path: &str,
	kind: AtomKind,
	atoms: impl IntoIterator<Item = AtomId>,
) -> FerrosGrynnResult<StreamedIndex> {
	let mut sorter = ExternalIndexSorter::new(database_root);
	for atom in atoms {
		let address = local_address(atom)?;
		sorter.push(address.slot().to_be_bytes().to_vec(), address)?;
	}
	let records = sorter.finish()?;
	write_streamed_file(database_root, relative_path, |file| {
		stream_label_index::<DiskAtomRef>(&records, kind, PageLsn::ZERO, |page_no, page| {
			write_next(file, page_no, page)
		})
	})
}

/// Writes all secondary, class, label, and count indexes directly into one
/// unreachable import generation.
pub(in crate::storage) fn write_streamed_scoped_indexes(
	database: &DatabaseRoot,
	database_root: &Path,
	generation: &str,
) -> FerrosGrynnResult<StreamedScopedIndexImages> {
	let mut images = StreamedScopedIndexImages::default();
	let atom_prefix = format!("atoms/{generation}");
	write_explicit(
		&database.indexes.base,
		IndexScope::Base,
		&atom_prefix,
		database_root,
		&mut images.base,
		&mut images.index_timings,
		Ok,
	)?;

	for (graph_id, indexes) in &database.indexes.graphs {
		let graph = database
			.graph(*graph_id)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph_id.to_string()))?;
		let prefix = format!("graphs/{:016x}/{generation}", graph_id.get());
		let files = images.graphs.entry(*graph_id).or_default();
		for (class, atoms) in &indexes.classes {
			let schema = database
				.schema
				.class_by_id(*class)
				.ok_or_else(|| invalid("graph class index references an absent class"))?;
			let path = format!("{prefix}/idx/cls_{}.fgidx", class.get());
			let built = write_streamed_membership_file(
				database_root,
				&path,
				schema.kind(),
				atoms.iter().copied(),
			)?;
			files.push(StreamedScopedIndexFile {
				scope: IndexScope::Effective {
					graph: *graph_id,
				},
				path,
				kind: ScopedIndexKind::Class {
					class: *class,
					kind: schema.kind(),
				},
				root: built.root,
				page_count: built.page_count,
			});
		}

		let label_ids = label_ids(indexes.labels.keys().map(|(kind, label)| (*kind, label)))?;
		for ((kind, label), atoms) in &indexes.labels {
			let label_id = *label_ids
				.get(&(*kind, label.clone()))
				.ok_or_else(|| invalid("label index has no dense label identity"))?;
			let path = format!("{prefix}/{}", label_filename(*kind, label_id));
			let built =
				write_streamed_membership_file(database_root, &path, *kind, atoms.iter().copied())?;
			files.push(StreamedScopedIndexFile {
				scope: IndexScope::Effective {
					graph: *graph_id,
				},
				path,
				kind: ScopedIndexKind::Label {
					kind: *kind,
					label_id,
				},
				root: built.root,
				page_count: built.page_count,
			});
		}

		write_explicit(
			&indexes.explicit,
			IndexScope::Effective {
				graph: *graph_id,
			},
			&prefix,
			database_root,
			files,
			&mut images.index_timings,
			local_address,
		)?;
		if graph.occurrences().len()
			!= indexes.classes.values().map(|atoms| atoms.len()).sum::<usize>()
		{
			return Err(invalid("graph class indexes do not cover every occurrence exactly once"));
		}
	}
	Ok(images)
}

/// Streams all explicit Standard, Unique, and Count bindings for one scope.
fn write_explicit<Source, Address>(
	bindings: &BTreeMap<crate::datastore::IndexId, IndexBinding<Source>>,
	scope: IndexScope,
	prefix: &str,
	database_root: &Path,
	files: &mut Vec<StreamedScopedIndexFile>,
	timings: &mut BTreeMap<crate::datastore::IndexId, Duration>,
	address: impl FnMut(Source) -> FerrosGrynnResult<Address> + Copy,
) -> FerrosGrynnResult<()>
where
	Source: Copy + Ord,
	Address: PrimaryAddress + Ord,
{
	for planned in explicit_index_paths(bindings) {
		let (binding, relative_path) = planned?;
		let started = Instant::now();
		files.push(write_streamed_binding(
			binding,
			scope,
			database_root,
			format!("{prefix}/{relative_path}"),
			address,
		)?);
		if timings.insert(binding.id, started.elapsed()).is_some() {
			return Err(invalid("duplicate IndexId while recording import index timing"));
		}
	}
	Ok(())
}

/// Streams one final binding without traversing unrelated scopes or indexes.
pub(in crate::storage) fn write_streamed_binding<Source, Address>(
	binding: &IndexBinding<Source>,
	scope: IndexScope,
	database_root: &Path,
	path: String,
	mut address: impl FnMut(Source) -> FerrosGrynnResult<Address>,
) -> FerrosGrynnResult<StreamedScopedIndexFile>
where
	Source: Copy + Ord,
	Address: PrimaryAddress + Ord,
{
	if binding.deferred {
		return Err(invalid("deferred physical index reached the generation builder"));
	}
	let built = match &binding.entries {
		IndexEntries::Standard(entries) => {
			let mut sorter = ExternalIndexSorter::new(database_root);
			for (key, atoms) in entries {
				for atom in atoms {
					sorter.push(key.clone(), address(*atom)?)?;
				}
			}
			let records = sorter.finish()?;
			write_streamed_file(database_root, &path, |file| {
				stream_standard_index::<Address>(
					&records,
					database_root,
					PageLsn::ZERO,
					|page_no, page| write_next(file, page_no, page),
				)
			})?
		}
		IndexEntries::Unique(entries) => {
			let mut sorter = ExternalIndexSorter::new(database_root);
			for (key, atom) in entries {
				sorter.push(key.clone(), address(*atom)?)?;
			}
			let records = sorter.finish()?;
			write_streamed_file(database_root, &path, |file| {
				stream_unique_index::<Address>(
					&records,
					database_root,
					PageLsn::ZERO,
					|page_no, page| write_next(file, page_no, page),
				)
			})?
		}
		IndexEntries::Count(count) => {
			let count_scope = match scope {
				IndexScope::Base => CountIndexScope::Base,
				IndexScope::Effective {
					graph,
				} => CountIndexScope::Effective(graph),
			};
			let page = encode_count_index(
				CountIndexValue {
					class_id: binding.class_id.get(),
					scope: count_scope,
					atom_count: *count,
					last_updated_lsn: 0,
					created_at_lsn: 0,
				},
				PageLsn::ZERO,
			)?;
			write_streamed_file(database_root, &path, |file| {
				write_next(file, PageNo::new(0), &page)?;
				Ok(StreamedIndex {
					root: PageNo::new(0),
					page_count: 1,
				})
			})?
		}
	};
	Ok(StreamedScopedIndexFile {
		scope,
		path,
		kind: ScopedIndexKind::Explicit {
			index: binding.id,
			version: binding.version,
			class: binding.class_id,
			kind: binding.kind,
		},
		root: built.root,
		page_count: built.page_count,
	})
}

/// Creates, fills, syncs, and rereads one immutable side-file.
pub(in crate::storage) fn write_streamed_file(
	database_root: &Path,
	relative_path: &str,
	build: impl FnOnce(&DirectFile) -> FerrosGrynnResult<StreamedIndex>,
) -> FerrosGrynnResult<StreamedIndex> {
	let path = database_root.join(relative_path);
	let parent = path.parent().ok_or_else(|| invalid("index file has no parent directory"))?;
	fs::create_dir_all(parent)
		.map_err(|source| io_failure("create streamed index directory", source))?;
	let file = DirectFile::create_new(&path)?;
	let built = build(&file)?;
	if file.page_count() != built.page_count {
		return Err(invalid("streamed index page count disagrees with its file length"));
	}
	file.sync_data()?;
	validate_pages(&file, built.page_count)?;
	Ok(built)
}

/// Writes one page only at the current sequential end of the file.
fn write_next(file: &DirectFile, page_no: PageNo, page: &AlignedPage) -> FerrosGrynnResult<()> {
	if page_no.get() != file.page_count() {
		return Err(invalid("streamed index writer produced non-sequential pages"));
	}
	file.write_page(page_no, page)
}

/// Rereads every completed page through Direct I/O checksum validation.
fn validate_pages(file: &DirectFile, page_count: u64) -> FerrosGrynnResult<()> {
	for raw in 0..page_count {
		let page_no = PageNo::new(raw);
		let mut page = Box::new(AlignedPage::zeroed());
		file.read_page(page_no, &mut page)?;
	}
	Ok(())
}

/// Builds an invalid-storage-file status for a streamed-image invariant.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

/// Wraps a filesystem failure in the project storage status.
fn io_failure(operation: impl Into<String>, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}
