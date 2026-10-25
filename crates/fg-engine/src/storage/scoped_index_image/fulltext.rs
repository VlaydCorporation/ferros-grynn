//! Fulltext side-file inputs derived from one immutable database root.

use std::{collections::BTreeSet, path::Path, sync::Arc, time::Instant};

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
		AnalyzedToken,
		ClassId,
		ClassIndexKind,
		ClassIndexState,
		DatabaseRoot,
		IndexId,
		IndexScope,
		SchemaAnalyzer,
		analyze_builtin,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
	storage::fulltext_index::{
		FulltextBuildSpec,
		FulltextDocument,
		build_fulltext_index,
		write_fulltext_index_stream,
	},
	values::{PropertyKeyId, Value, storage::DiskAtomRef},
};

/// Fulltext source state.
struct FulltextSource {
	/// Spec associated with this fulltext source.
	spec: FulltextBuildSpec,
	/// Analyzer associated with this fulltext source.
	analyzer: Arc<SchemaAnalyzer>,
	/// Relative path associated with this fulltext source.
	relative_path: String,
}

/// Builds every query-visible Fulltext definition into buffered page images.
pub(in crate::storage) fn build_fulltext_index_images(
	database: &DatabaseRoot,
	skipped: &BTreeSet<IndexId>,
	analyze_function: &mut dyn FnMut(
		&SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
) -> FerrosGrynnResult<ScopedIndexImages> {
	let mut images = ScopedIndexImages::default();
	for source in fulltext_sources(database, skipped)? {
		let mut documents = Vec::new();
		visit_documents(
			database,
			source.spec,
			source.analyzer.as_ref(),
			analyze_function,
			&mut |document| {
				documents.push(document);
				Ok(())
			},
		)?;
		let built = build_fulltext_index(source.spec, documents)?;
		let file = ScopedIndexFile {
			scope: source.spec.scope,
			relative_path: source.relative_path,
			kind: explicit_kind(source.spec),
			root: built.root,
			pages: built.pages,
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

/// Builds every query-visible Fulltext definition directly into an
/// unreachable generation using one bounded external-sort budget.
pub(in crate::storage) fn write_streamed_fulltext_index_images(
	database: &DatabaseRoot,
	skipped: &BTreeSet<IndexId>,
	database_root: &Path,
	generation: &str,
	analyze_function: &mut dyn FnMut(
		&SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
) -> FerrosGrynnResult<StreamedScopedIndexImages> {
	let mut images = StreamedScopedIndexImages::default();
	for source in fulltext_sources(database, skipped)? {
		let started = Instant::now();
		let prefix = match source.spec.scope {
			IndexScope::Base => format!("atoms/{generation}"),
			IndexScope::Effective {
				graph,
			} => format!("graphs/{:016x}/{generation}", graph.get()),
		};
		let path = format!("{prefix}/{}", source.relative_path);
		let built = write_streamed_file(database_root, &path, |file| {
			write_fulltext_index_stream(source.spec, database_root, file, |consumer| {
				visit_documents(
					database,
					source.spec,
					source.analyzer.as_ref(),
					analyze_function,
					consumer,
				)
			})
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
			return Err(invalid("duplicate Fulltext IndexId while recording build timing"));
		}
	}
	Ok(images)
}

/// Returns the fulltext sources result.
fn fulltext_sources(
	database: &DatabaseRoot,
	skipped: &BTreeSet<IndexId>,
) -> FerrosGrynnResult<Vec<FulltextSource>> {
	let mut sources = Vec::new();
	let mut base_ordinal = 0_u32;
	let mut graph_ordinals = std::collections::BTreeMap::new();
	for class in database.schema.classes() {
		let class_id = class.id().ok_or_else(|| invalid("Fulltext class has no ClassId"))?;
		for index in class.indexes().filter(|index| {
			index.kind() == ClassIndexKind::Fulltext && index.state() == ClassIndexState::Ready
		}) {
			let id = index.id().ok_or_else(|| invalid("Fulltext definition has no IndexId"))?;
			if skipped.contains(&id) {
				continue;
			}
			let options = index
				.fulltext_options()
				.ok_or_else(|| invalid("Fulltext definition has no kind-specific options"))?;
			let analyzer = database
				.schema
				.analyzer_by_id(options.analyzer())
				.ok_or_else(|| invalid("Fulltext definition references an absent analyzer"))?;
			if analyzer.version() != options.analyzer_version() {
				return Err(invalid("Fulltext definition analyzer version is stale"));
			}
			let field_name =
				index.fields().next().ok_or_else(|| invalid("Fulltext definition has no field"))?;
			let field = class
				.fields()
				.find(|field| field.name() == field_name)
				.ok_or_else(|| invalid("Fulltext definition field is absent from its class"))?;
			let field = field.key.ok_or_else(|| invalid("Fulltext field has no PropertyKeyId"))?;
			let ordinal = match index.scope() {
				IndexScope::Base => {
					let ordinal = base_ordinal;
					base_ordinal = next_ordinal(base_ordinal)?;
					ordinal
				}
				IndexScope::Effective {
					graph,
				} => {
					let next = graph_ordinals.entry(graph).or_insert(0_u32);
					let ordinal = *next;
					*next = next_ordinal(*next)?;
					ordinal
				}
			};
			sources.push(FulltextSource {
				spec: FulltextBuildSpec {
					index: id,
					index_version: index.version(),
					analyzer: options.analyzer(),
					analyzer_version: options.analyzer_version(),
					scope: index.scope(),
					class: class_id,
					field,
					options,
					build_revision: index.build_revision(),
				},
				analyzer,
				relative_path: format!("idx/ft_{ordinal:04}.fgidx"),
			});
		}
	}
	Ok(sources)
}

/// Visits documents.
fn visit_documents(
	database: &DatabaseRoot,
	spec: FulltextBuildSpec,
	analyzer: &SchemaAnalyzer,
	analyze_function: &mut dyn FnMut(
		&SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
	consumer: &mut dyn FnMut(FulltextDocument) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	match spec.scope {
		IndexScope::Base => visit_base_documents(
			database,
			spec.class,
			spec.field,
			analyzer,
			analyze_function,
			consumer,
		),
		IndexScope::Effective {
			graph,
		} => visit_effective_documents(
			database,
			graph,
			spec.class,
			spec.field,
			analyzer,
			analyze_function,
			consumer,
		),
	}
}

/// Visits base documents.
fn visit_base_documents(
	database: &DatabaseRoot,
	class: ClassId,
	key: PropertyKeyId,
	analyzer: &SchemaAnalyzer,
	analyze_function: &mut dyn FnMut(
		&SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
	consumer: &mut dyn FnMut(FulltextDocument) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	for atom in database.atoms.atoms().filter(|atom| atom.class_id() == class) {
		match atom.properties().get(key) {
			None | Some(Value::Null) => {}
			Some(Value::String(value)) => consumer(FulltextDocument {
				atom: DiskAtomRef::from_parts(atom.kind(), atom.id().slot())?,
				tokens: analyze(analyzer, value.as_str(), analyze_function)?,
			})?,
			Some(_) => return Err(invalid("Fulltext base field contains a non-string value")),
		}
	}
	Ok(())
}

/// Visits effective documents.
fn visit_effective_documents(
	database: &DatabaseRoot,
	graph: GraphId,
	class: ClassId,
	key: PropertyKeyId,
	analyzer: &SchemaAnalyzer,
	analyze_function: &mut dyn FnMut(
		&SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
	consumer: &mut dyn FnMut(FulltextDocument) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let graph_root = database
		.graph(graph)
		.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
	for occurrence in graph_root.occurrences() {
		let atom = database
			.atom(occurrence.global_id())
			.ok_or_else(|| invalid("Fulltext occurrence references an absent base atom"))?;
		if atom.class_id() != class {
			continue;
		}
		match occurrence.effective_value(atom.properties().get(key), key) {
			None | Some(Value::Null) => {}
			Some(Value::String(value)) => consumer(FulltextDocument {
				atom: local_address(occurrence.local_id())?,
				tokens: analyze(analyzer, value.as_str(), analyze_function)?,
			})?,
			Some(_) => return Err(invalid("Fulltext effective field contains a non-string value")),
		}
	}
	Ok(())
}

/// Analyzes the current state.
fn analyze(
	analyzer: &SchemaAnalyzer,
	input: &str,
	analyze_function: &mut dyn FnMut(
		&SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	if analyzer.function().is_some() {
		analyze_function(analyzer, input)
	} else {
		analyze_builtin(analyzer, input)
	}
}

/// Returns the explicit kind result.
const fn explicit_kind(spec: FulltextBuildSpec) -> ScopedIndexKind {
	ScopedIndexKind::Explicit {
		index: spec.index,
		version: spec.index_version,
		class: spec.class,
		kind: ClassIndexKind::Fulltext,
	}
}

/// Returns the next ordinal.
fn next_ordinal(value: u32) -> FerrosGrynnResult<u32> {
	value.checked_add(1).ok_or_else(|| invalid("Fulltext file ordinal exceeds u32"))
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
