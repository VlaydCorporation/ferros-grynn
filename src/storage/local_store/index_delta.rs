//! Reuse of immutable Fulltext/vector bases across ordinary row commits.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	LocalStore,
	physical_index::{PhysicalIndexDelta, PhysicalIndexEntries},
};
use crate::{
	datastore::{
		AnalyzedToken,
		ClassId,
		ClassIndexKind,
		ClassIndexState,
		DatabaseRoot,
		IndexId,
		IndexMaintenanceMode,
		IndexScope,
		analyze_builtin,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		catalog::{FileCatalog, FileCatalogEntry, PhysicalIndexBinding},
		fulltext_index::DecodedFulltextIndex,
		fulltext_index_delta::{FulltextDeltaRecord, FulltextDeltaSpec, build_fulltext_delta},
		page::AlignedPage,
		scoped_index_image::local_address,
		vector_index::{VectorDeltaRecord, VectorDeltaSpec, build_vector_delta},
	},
	values::{PropertyKeyId, RecordId, Value, storage::DiskAtomRef},
};

/// Physical files that can remain reachable while the new root is published.
#[derive(Default)]
pub(super) struct IndexReusePlan {
	pub(super) indexes: BTreeMap<IndexId, RetainedIndex>,
}

impl IndexReusePlan {
	pub(super) fn skipped_builds(&self) -> BTreeSet<IndexId> {
		self.indexes.keys().copied().collect()
	}
}

/// One immutable base and its exact current replacement generation.
pub(super) struct RetainedIndex {
	pub(super) kind: ClassIndexKind,
	pub(super) scope: IndexScope,
	pub(super) base: PhysicalIndexBinding,
	pub(super) base_entry: FileCatalogEntry,
	pub(super) delta: RetainedDelta,
}

/// Delta publication selected by the reuse planner.
pub(super) enum RetainedDelta {
	None,
	Existing {
		binding: PhysicalIndexBinding,
		entry: FileCatalogEntry,
	},
	Replacement(Vec<AlignedPage>),
}

impl LocalStore {
	/// Builds exact replacement records against the currently published base.
	/// Any unavailable or incompatible physical input simply selects the normal
	/// full builder for that index.
	pub(super) fn prepare_index_reuse(
		&self,
		previous: &DatabaseRoot,
		next: &DatabaseRoot,
		changed_records: &BTreeSet<RecordId>,
		analyze_function: &mut dyn FnMut(
			&crate::datastore::SchemaAnalyzer,
			&str,
		) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
	) -> FerrosGrynnResult<IndexReusePlan> {
		let Some(snapshot) = self.capture_query_snapshot(previous.commit_ts)? else {
			return Ok(IndexReusePlan::default());
		};
		let (catalog, files) = {
			let state = self.state.lock();
			(state._database_catalog.clone(), state.file_catalog.clone())
		};
		let mut plan = IndexReusePlan::default();
		for class in next.schema.classes() {
			let Some(class_id) = class.id() else {
				continue;
			};
			let Some(old_class) = catalog.classes.iter().find(|entry| entry.id == class_id) else {
				continue;
			};
			for index in class.indexes() {
				let Some(index_id) = index.id() else {
					continue;
				};
				let Some(old) = old_class.indexes.iter().find(|entry| entry.id == index_id) else {
					continue;
				};
				let fields = index
					.fields()
					.map(|name| {
						class
							.fields()
							.find(|field| field.name() == name)
							.and_then(|field| field.key)
							.ok_or_else(|| {
								FerrosGrynnError::invalid_storage_file(
									"specialized index field has no durable property key",
								)
							})
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				if !matches!(
					index.kind(),
					ClassIndexKind::Fulltext | ClassIndexKind::Hnsw | ClassIndexKind::DiskAnn
				) || index.state() != ClassIndexState::Ready
					|| index.maintenance_mode() != IndexMaintenanceMode::Immediate
					|| old.version != index.version()
					|| old.kind != index.kind()
					|| old.state != ClassIndexState::Ready
					|| old.maintenance != IndexMaintenanceMode::Immediate
					|| old.build_revision != index.build_revision()
					|| old.scope != index.scope()
					|| old.fields != fields
					|| old.options != index.options
				{
					continue;
				}
				let Some(binding) = old.physical else {
					continue;
				};
				let Some(base_entry) = file_entry(&files, old.scope, binding)?.cloned() else {
					continue;
				};
				let Ok(Some(decoded)) = self.physical_index(&snapshot, index_id) else {
					continue;
				};
				let delta = match (&decoded.entries, &decoded.delta) {
					(PhysicalIndexEntries::Fulltext(base), delta) => prepare_fulltext_delta(
						previous,
						next,
						changed_records,
						base,
						delta,
						analyze_function,
					)?,
					(PhysicalIndexEntries::Hnsw(base), delta) => prepare_vector_delta(
						previous,
						next,
						changed_records,
						base.spec,
						base.posting_count,
						delta,
					)?,
					(PhysicalIndexEntries::Vector(base), delta) => {
						let population = base.nodes.iter().try_fold(0_usize, |count, node| {
							count
								.checked_add(node.postings.len())
								.ok_or_else(|| invalid("vector posting count exceeds usize"))
						})?;
						prepare_vector_delta(
							previous,
							next,
							changed_records,
							base.spec,
							population,
							delta,
						)?
					}
					_ => None,
				};
				let Some(delta) = delta else {
					continue;
				};
				let delta = match delta {
					PreparedDelta::Unchanged => match old.delta {
						Some(delta) => {
							let Some(entry) = file_entry(&files, old.scope, delta)?.cloned() else {
								continue;
							};
							RetainedDelta::Existing {
								binding: delta,
								entry,
							}
						}
						None => RetainedDelta::None,
					},
					PreparedDelta::Replacement(pages) => RetainedDelta::Replacement(pages),
				};
				plan.indexes.insert(index_id, RetainedIndex {
					kind: index.kind(),
					scope: index.scope(),
					base: binding,
					base_entry,
					delta,
				});
			}
		}
		Ok(plan)
	}
}

enum PreparedDelta {
	Unchanged,
	Replacement(Vec<AlignedPage>),
}

fn prepare_fulltext_delta(
	previous: &DatabaseRoot,
	next: &DatabaseRoot,
	changed: &BTreeSet<RecordId>,
	base: &DecodedFulltextIndex,
	delta: &Option<PhysicalIndexDelta>,
	analyze_function: &mut dyn FnMut(
		&crate::datastore::SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
) -> FerrosGrynnResult<Option<PreparedDelta>> {
	let analyzer = next
		.schema
		.analyzer_by_id(base.spec.analyzer)
		.filter(|analyzer| analyzer.version() == base.spec.analyzer_version);
	let Some(analyzer) = analyzer else {
		return Ok(None);
	};
	let mut records = match delta {
		Some(PhysicalIndexDelta::Fulltext(delta)) => delta
			.records
			.iter()
			.cloned()
			.map(|record| (record.atom(), record))
			.collect::<BTreeMap<_, _>>(),
		None => BTreeMap::new(),
		Some(_) => return Ok(None),
	};
	let mut touched = false;
	for record in changed {
		let old = fulltext_row(
			previous,
			record,
			base.spec.class,
			base.spec.scope,
			base.spec.field,
			analyzer.as_ref(),
			analyze_function,
		)?;
		let new = fulltext_row(
			next,
			record,
			base.spec.class,
			base.spec.scope,
			base.spec.field,
			analyzer.as_ref(),
			analyze_function,
		)?;
		if old == new {
			continue;
		}
		if let Some((atom, Some(_tokens))) = old {
			records.insert(atom, FulltextDeltaRecord::Delete {
				atom,
			});
			touched = true;
		}
		if let Some((atom, Some(tokens))) = new {
			records.insert(atom, FulltextDeltaRecord::Upsert {
				atom,
				tokens,
			});
			touched = true;
		}
	}
	if !touched {
		return Ok(Some(PreparedDelta::Unchanged));
	}
	if records.len() > rebuild_threshold(base.documents.len()) {
		return Ok(None);
	}
	let pages = build_fulltext_delta(
		FulltextDeltaSpec {
			base: base.spec,
			revision: next.commit_ts,
		},
		records.into_values(),
	)?;
	Ok(Some(PreparedDelta::Replacement(pages)))
}

fn prepare_vector_delta(
	previous: &DatabaseRoot,
	next: &DatabaseRoot,
	changed: &BTreeSet<RecordId>,
	spec: crate::storage::vector_index::VectorBuildSpec,
	population: usize,
	delta: &Option<PhysicalIndexDelta>,
) -> FerrosGrynnResult<Option<PreparedDelta>> {
	let mut records = match delta {
		Some(PhysicalIndexDelta::Vector(delta)) => delta
			.records
			.iter()
			.cloned()
			.map(|record| (record.atom(), record))
			.collect::<BTreeMap<_, _>>(),
		None => BTreeMap::new(),
		Some(_) => return Ok(None),
	};
	let mut touched = false;
	for record in changed {
		let old = vector_row(previous, record, spec.class, spec.scope, spec.field)?;
		let new = vector_row(next, record, spec.class, spec.scope, spec.field)?;
		if old == new {
			continue;
		}
		if let Some((atom, Some(_vector))) = old {
			records.insert(atom, VectorDeltaRecord::Delete {
				atom,
			});
			touched = true;
		}
		if let Some((atom, Some(vector))) = new {
			records.insert(atom, VectorDeltaRecord::Upsert {
				atom,
				vector,
			});
			touched = true;
		}
	}
	if !touched {
		return Ok(Some(PreparedDelta::Unchanged));
	}
	if records.len() > rebuild_threshold(population) {
		return Ok(None);
	}
	let pages = build_vector_delta(
		VectorDeltaSpec {
			base: spec,
			revision: next.commit_ts,
		},
		records.into_values().collect(),
	)?;
	Ok(Some(PreparedDelta::Replacement(pages)))
}

fn fulltext_row(
	root: &DatabaseRoot,
	record: &RecordId,
	class: ClassId,
	scope: IndexScope,
	key: PropertyKeyId,
	analyzer: &crate::datastore::SchemaAnalyzer,
	analyze_function: &mut dyn FnMut(
		&crate::datastore::SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
) -> FerrosGrynnResult<Option<(DiskAtomRef, Option<Vec<crate::datastore::AnalyzedToken>>)>> {
	let Some((atom, value)) = scoped_value(root, record, class, scope, key)? else {
		return Ok(None);
	};
	let tokens = match value {
		None | Some(Value::Null) => None,
		Some(Value::String(value)) => Some(if analyzer.function().is_some() {
			analyze_function(analyzer, value.as_str())?
		} else {
			analyze_builtin(analyzer, value.as_str())?
		}),
		Some(_) => return Err(invalid("Fulltext delta field contains a non-string value")),
	};
	Ok(Some((atom, tokens)))
}

fn vector_row(
	root: &DatabaseRoot,
	record: &RecordId,
	class: ClassId,
	scope: IndexScope,
	key: PropertyKeyId,
) -> FerrosGrynnResult<Option<(DiskAtomRef, Option<crate::values::Vector>)>> {
	let Some((atom, value)) = scoped_value(root, record, class, scope, key)? else {
		return Ok(None);
	};
	let vector = match value {
		None | Some(Value::Null) => None,
		Some(Value::Vector(value)) => Some(value.clone()),
		Some(_) => return Err(invalid("vector delta field contains a non-vector value")),
	};
	Ok(Some((atom, vector)))
}

fn scoped_value<'a>(
	root: &'a DatabaseRoot,
	record: &RecordId,
	class: ClassId,
	scope: IndexScope,
	key: PropertyKeyId,
) -> FerrosGrynnResult<Option<(DiskAtomRef, Option<&'a Value>)>> {
	let Some(global) = root.resolve_atom(record) else {
		return Ok(None);
	};
	let atom = root
		.atom(global)
		.ok_or_else(|| invalid("primary identity resolves an absent base atom"))?;
	if atom.class_id() != class {
		return Ok(None);
	}
	match scope {
		IndexScope::Base => Ok(Some((
			DiskAtomRef::from_parts(atom.kind(), atom.id().slot())?,
			atom.properties().get(key),
		))),
		IndexScope::Effective {
			graph,
		} => {
			let Some(local) = root.membership(graph, global) else {
				return Ok(None);
			};
			let occurrence = root
				.occurrence(graph, local)
				.ok_or_else(|| invalid("graph membership resolves an absent occurrence"))?;
			Ok(Some((
				local_address(local)?,
				occurrence.effective_value(atom.properties().get(key), key),
			)))
		}
	}
}

fn file_entry(
	files: &FileCatalog,
	scope: IndexScope,
	binding: PhysicalIndexBinding,
) -> FerrosGrynnResult<Option<&FileCatalogEntry>> {
	let space = match scope {
		IndexScope::Base => crate::datastore::DataSpaceId::Database,
		IndexScope::Effective {
			graph,
		} => crate::datastore::DataSpaceId::Graph(graph),
	};
	Ok(files.files.iter().find(|entry| entry.space_id == space && entry.file_id == binding.file_id))
}

fn rebuild_threshold(population: usize) -> usize {
	(population / 1_000).clamp(1, 4_096)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
