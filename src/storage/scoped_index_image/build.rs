use std::collections::{BTreeMap, BTreeSet};

use super::{ScopedIndexFile, ScopedIndexImages, ScopedIndexKind};
use crate::{
	datastore::{ClassIndexKind, DatabaseRoot, IndexBinding, IndexEntries, IndexScope},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind},
	storage::{
		graph_index::{
			CountIndexScope,
			CountIndexValue,
			ScalarIndexEntry,
			ScalarIndexMode,
			build_label_index,
			build_scalar_index,
			encode_count_index,
		},
		page::{PageLsn, PageNo},
	},
	values::{Text, storage::DiskAtomRef},
};

/// Bulk-builds all currently visible normalized indexes without publishing
/// their physical files.
pub(in crate::storage) fn build_scoped_index_images(
	database: &DatabaseRoot,
) -> FerrosGrynnResult<ScopedIndexImages> {
	let mut images = ScopedIndexImages::default();
	build_explicit(&database.indexes.base, IndexScope::Base, &mut images.base, |atom| Ok(atom))?;
	for (graph_id, indexes) in &database.indexes.graphs {
		let graph = database
			.graph(*graph_id)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph_id.to_string()))?;
		let files = images.graphs.entry(*graph_id).or_default();
		for (class, atoms) in &indexes.classes {
			let schema = database
				.schema
				.class_by_id(*class)
				.ok_or_else(|| invalid("graph class index references an absent class"))?;
			let addresses = local_addresses(atoms)?;
			files.push(ScopedIndexFile {
				scope: IndexScope::Effective {
					graph: *graph_id,
				},
				relative_path: format!("idx/cls_{}.fgidx", class.get()),
				kind: ScopedIndexKind::Class {
					class: *class,
					kind: schema.kind(),
				},
				root: PageNo::new(0),
				pages: build_label_index(&addresses, schema.kind(), PageLsn::ZERO)?,
			});
		}
		let label_ids = label_ids(indexes.labels.keys().map(|(kind, label)| (*kind, label)))?;
		for ((kind, label), atoms) in &indexes.labels {
			let addresses = local_addresses(atoms)?;
			let label_id = *label_ids
				.get(&(*kind, label.clone()))
				.ok_or_else(|| invalid("label index has no dense label identity"))?;
			files.push(ScopedIndexFile {
				scope: IndexScope::Effective {
					graph: *graph_id,
				},
				relative_path: label_filename(*kind, label_id),
				kind: ScopedIndexKind::Label {
					kind: *kind,
					label_id,
				},
				root: PageNo::new(0),
				pages: build_label_index(&addresses, *kind, PageLsn::ZERO)?,
			});
		}
		build_explicit(
			&indexes.explicit,
			IndexScope::Effective {
				graph: *graph_id,
			},
			files,
			local_address,
		)?;
		if graph.occurrences().len() != indexes.classes.values().map(BTreeSet::len).sum::<usize>() {
			return Err(invalid("graph class indexes do not cover every occurrence exactly once"));
		}
	}
	Ok(images)
}

fn build_explicit<Source, Target>(
	bindings: &BTreeMap<crate::datastore::IndexId, IndexBinding<Source>>,
	scope: IndexScope,
	files: &mut Vec<ScopedIndexFile>,
	mut address: impl FnMut(Source) -> FerrosGrynnResult<Target> + Copy,
) -> FerrosGrynnResult<()>
where
	Source: Copy + Ord,
	Target: crate::storage::graph_index::PrimaryAddress + Ord,
{
	let mut standard = 0_u32;
	let mut unique = 0_u32;
	for binding in bindings.values() {
		let (relative_path, root, pages) = match &binding.entries {
			IndexEntries::Standard(entries) => {
				let entries = scalar_entries(entries, address)?;
				let built = build_scalar_index(&entries, ScalarIndexMode::Standard, PageLsn::ZERO)?;
				let path = scalar_filename(ClassIndexKind::Standard, standard);
				standard = standard
					.checked_add(1)
					.ok_or_else(|| invalid("standard index ordinal overflows u32"))?;
				(path, built.root, built.pages)
			}
			IndexEntries::Unique(entries) => {
				let entries = entries
					.iter()
					.map(|(key, atom)| {
						Ok(ScalarIndexEntry {
							key: key.clone(),
							atoms: vec![address(*atom)?],
						})
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let built = build_scalar_index(&entries, ScalarIndexMode::Unique, PageLsn::ZERO)?;
				let path = scalar_filename(ClassIndexKind::Unique, unique);
				unique = unique
					.checked_add(1)
					.ok_or_else(|| invalid("unique index ordinal overflows u32"))?;
				(path, built.root, built.pages)
			}
			IndexEntries::Count(count) => {
				let count_scope = match scope {
					IndexScope::Base => CountIndexScope::Base,
					IndexScope::Effective {
						graph,
					} => CountIndexScope::Effective(graph),
				};
				(format!("idx/cnt_{}.fgidx", binding.class_id.get()), PageNo::new(0), vec![
					encode_count_index(
						CountIndexValue {
							class_id: binding.class_id.get(),
							scope: count_scope,
							atom_count: *count,
							last_updated_lsn: 0,
							created_at_lsn: 0,
						},
						PageLsn::ZERO,
					)?,
				])
			}
		};
		files.push(ScopedIndexFile {
			scope,
			relative_path,
			kind: ScopedIndexKind::Explicit {
				index: binding.id,
				version: binding.version,
				class: binding.class_id,
				kind: binding.kind,
			},
			root,
			pages,
		});
	}
	Ok(())
}

fn scalar_entries<Source, Target>(
	entries: &BTreeMap<Vec<u8>, BTreeSet<Source>>,
	mut address: impl FnMut(Source) -> FerrosGrynnResult<Target>,
) -> FerrosGrynnResult<Vec<ScalarIndexEntry<Target>>>
where
	Source: Copy + Ord,
	Target: Ord,
{
	entries
		.iter()
		.map(|(key, atoms)| {
			let mut atoms =
				atoms.iter().copied().map(&mut address).collect::<FerrosGrynnResult<Vec<_>>>()?;
			atoms.sort_unstable();
			Ok(ScalarIndexEntry {
				key: key.clone(),
				atoms,
			})
		})
		.collect()
}

fn local_addresses(atoms: &BTreeSet<AtomId>) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let mut result =
		atoms.iter().copied().map(local_address).collect::<FerrosGrynnResult<Vec<_>>>()?;
	result.sort_unstable_by_key(|atom| atom.slot());
	Ok(result)
}

pub(super) fn local_address(atom: AtomId) -> FerrosGrynnResult<DiskAtomRef> {
	DiskAtomRef::from_parts(
		atom.kind().ok_or_else(|| invalid("graph index contains a null atom"))?,
		atom.slot(),
	)
}

pub(super) fn label_ids<'a>(
	labels: impl Iterator<Item = (AtomKind, &'a Text)>,
) -> FerrosGrynnResult<BTreeMap<(AtomKind, Text), u32>> {
	let mut next = BTreeMap::<AtomKind, u32>::new();
	let mut result = BTreeMap::new();
	for (kind, label) in labels {
		let id = next.entry(kind).or_default();
		if result.insert((kind, label.clone()), *id).is_some() {
			return Err(invalid("duplicate graph label index"));
		}
		*id = id.checked_add(1).ok_or_else(|| invalid("label identity overflows u32"))?;
	}
	Ok(result)
}

pub(super) fn scalar_filename(kind: ClassIndexKind, ordinal: u32) -> String {
	let prefix = match kind {
		ClassIndexKind::Standard => "std",
		ClassIndexKind::Unique => "uniq",
		ClassIndexKind::Count => "cnt",
	};
	format!("idx/{prefix}_{ordinal:04}.fgidx")
}

pub(super) fn label_filename(kind: AtomKind, label_id: u32) -> String {
	let kind = match kind {
		AtomKind::Node => "node",
		AtomKind::Edge => "edge",
	};
	format!("idx/lbl_{kind}_{label_id}.fgidx")
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
