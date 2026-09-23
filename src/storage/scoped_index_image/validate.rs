use super::{ScopedIndexFile, ScopedIndexImages, ScopedIndexKind};
use crate::{
	datastore::{ClassIndexKind, DatabaseRoot, IndexEntries, IndexScope},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		graph_index::{
			CountIndexScope,
			ScalarIndexMode,
			decode_count_index,
			scan_scalar_index,
			visit_label_index_pages,
		},
		page::{AlignedPage, PageNo},
	},
	values::storage::DiskAtomRef,
};

/// Reopens every generated page chain and checks it against the immutable
/// source root.
pub(in crate::storage) fn validate_scoped_index_images(
	images: &ScopedIndexImages,
	database: &DatabaseRoot,
) -> FerrosGrynnResult<()> {
	for file in &images.base {
		validate_explicit(file, database)?;
	}
	for (graph, files) in &images.graphs {
		for file in files {
			match file.kind {
				ScopedIndexKind::Class {
					class,
					kind,
				} => {
					let expected = database.indexes.graph_class(*graph, class);
					let mut actual = Vec::new();
					visit_label_index_pages::<DiskAtomRef>(reader(file), kind, |atom| {
						actual.push(atom);
						Ok(())
					})?;
					let expected = expected
						.into_iter()
						.map(|atom| DiskAtomRef::from_parts(kind, atom.slot()))
						.collect::<FerrosGrynnResult<Vec<_>>>()?;
					if actual != expected {
						return Err(invalid("class index disagrees with graph occurrences"));
					}
				}
				ScopedIndexKind::Label {
					kind,
					..
				} => {
					visit_label_index_pages::<DiskAtomRef>(reader(file), kind, |_atom| Ok(()))?;
				}
				ScopedIndexKind::Explicit {
					..
				} => validate_explicit(file, database)?,
			}
		}
	}
	Ok(())
}

fn validate_explicit(file: &ScopedIndexFile, database: &DatabaseRoot) -> FerrosGrynnResult<()> {
	let ScopedIndexKind::Explicit {
		index,
		class,
		kind,
		..
	} = file.kind
	else {
		return Err(invalid("non-explicit index reached explicit validator"));
	};
	match file.scope {
		IndexScope::Base => {
			let binding = database
				.indexes
				.base_index(index)
				.ok_or_else(|| invalid("base index image has no logical binding"))?;
			validate_global_entries(file, kind, class, &binding.entries)
		}
		IndexScope::Effective {
			graph,
		} => {
			let binding = database
				.indexes
				.graph_index(graph, index)
				.ok_or_else(|| invalid("effective index image has no logical binding"))?;
			validate_local_entries(file, kind, class, graph, &binding.entries)
		}
	}
}

fn validate_global_entries(
	file: &ScopedIndexFile,
	kind: ClassIndexKind,
	class: crate::datastore::ClassId,
	expected: &IndexEntries<crate::datastore::GlobalAtomId>,
) -> FerrosGrynnResult<()> {
	match (kind, expected) {
		(ClassIndexKind::Standard, IndexEntries::Standard(expected)) => {
			let actual = scan_scalar_index(reader(file), file.root, ScalarIndexMode::Standard)?;
			let expected = expected
				.iter()
				.map(|(key, atoms)| crate::storage::graph_index::ScalarIndexEntry {
					key: key.clone(),
					atoms: atoms.iter().copied().collect(),
				})
				.collect::<Vec<_>>();
			ensure_equal(actual, expected, "Base Standard")
		}
		(ClassIndexKind::Unique, IndexEntries::Unique(expected)) => {
			let actual = scan_scalar_index(reader(file), file.root, ScalarIndexMode::Unique)?;
			let expected = expected
				.iter()
				.map(|(key, atom)| crate::storage::graph_index::ScalarIndexEntry {
					key: key.clone(),
					atoms: vec![*atom],
				})
				.collect::<Vec<_>>();
			ensure_equal(actual, expected, "Base Unique")
		}
		(ClassIndexKind::Count, IndexEntries::Count(expected)) => {
			validate_count(file, class, CountIndexScope::Base, *expected)
		}
		_ => Err(invalid("Base index kind disagrees with its logical entries")),
	}
}

fn validate_local_entries(
	file: &ScopedIndexFile,
	kind: ClassIndexKind,
	class: crate::datastore::ClassId,
	graph: crate::graph::GraphId,
	expected: &IndexEntries<crate::graph::AtomId>,
) -> FerrosGrynnResult<()> {
	match (kind, expected) {
		(ClassIndexKind::Standard, IndexEntries::Standard(expected)) => {
			let actual: Vec<crate::storage::graph_index::ScalarIndexEntry<DiskAtomRef>> =
				scan_scalar_index(reader(file), file.root, ScalarIndexMode::Standard)?;
			let expected = local_entries(expected)?;
			ensure_equal(actual, expected, "Effective Standard")
		}
		(ClassIndexKind::Unique, IndexEntries::Unique(expected)) => {
			let actual: Vec<crate::storage::graph_index::ScalarIndexEntry<DiskAtomRef>> =
				scan_scalar_index(reader(file), file.root, ScalarIndexMode::Unique)?;
			let expected = expected
				.iter()
				.map(|(key, atom)| {
					Ok(crate::storage::graph_index::ScalarIndexEntry {
						key: key.clone(),
						atoms: vec![DiskAtomRef::from_parts(
							atom.kind()
								.ok_or_else(|| invalid("effective index contains a null atom"))?,
							atom.slot(),
						)?],
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			ensure_equal(actual, expected, "Effective Unique")
		}
		(ClassIndexKind::Count, IndexEntries::Count(expected)) => {
			validate_count(file, class, CountIndexScope::Effective(graph), *expected)
		}
		_ => Err(invalid("Effective index kind disagrees with its logical entries")),
	}
}

fn local_entries(
	entries: &std::collections::BTreeMap<Vec<u8>, std::collections::BTreeSet<crate::graph::AtomId>>,
) -> FerrosGrynnResult<Vec<crate::storage::graph_index::ScalarIndexEntry<DiskAtomRef>>> {
	entries
		.iter()
		.map(|(key, atoms)| {
			let mut atoms = atoms
				.iter()
				.map(|atom| {
					DiskAtomRef::from_parts(
						atom.kind()
							.ok_or_else(|| invalid("effective index contains a null atom"))?,
						atom.slot(),
					)
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			atoms.sort_unstable();
			Ok(crate::storage::graph_index::ScalarIndexEntry {
				key: key.clone(),
				atoms,
			})
		})
		.collect()
}

fn validate_count(
	file: &ScopedIndexFile,
	class: crate::datastore::ClassId,
	scope: CountIndexScope,
	expected: u64,
) -> FerrosGrynnResult<()> {
	if file.pages.len() != 1 || file.root != PageNo::new(0) {
		return Err(invalid("Count index is not exactly one page"));
	}
	let value = decode_count_index(&file.pages[0])?;
	if value.class_id != class.get() || value.scope != scope || value.atom_count != expected {
		return Err(invalid("Count index value disagrees with its logical binding"));
	}
	Ok(())
}

fn reader(
	file: &ScopedIndexFile,
) -> impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()> + '_ {
	move |page_no, output| {
		let index = usize::try_from(page_no.get())
			.map_err(|_error| invalid("index page number exceeds usize"))?;
		let page = file.pages.get(index).ok_or_else(|| invalid("index page is absent"))?;
		output.as_bytes_mut().copy_from_slice(page.as_bytes());
		Ok(())
	}
}

fn ensure_equal<T: PartialEq>(actual: T, expected: T, name: &str) -> FerrosGrynnResult<()> {
	if actual != expected {
		return Err(invalid(format!("{name} image disagrees with logical index")));
	}
	Ok(())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
