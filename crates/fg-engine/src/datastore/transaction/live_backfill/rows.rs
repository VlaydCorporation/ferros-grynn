//! Exact row-local projection through the existing typed SELECT executor.

use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use fg_runtime::__internal::{RecordIdInternal as _, ValueInternal as _};

use crate::{
	changefeed::{ChangeBatch, ChangeIdentity, ChangeImage},
	datastore::{
		BindingAdmission,
		ClassSchema,
		ConflictKey,
		DatabaseRoot,
		LiveDefinition,
		SchemaCatalog,
	},
	gql::live_lowering::{QueryEvaluation, evaluate_query},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveExpr, LiveProjection, LiveQuery, LiveSource},
	values::PropertyMap,
};

/// A syntactic gate also protects missing foreign-record reads, which can
/// otherwise look like the source's own class-range dependency.
pub(in crate::datastore::transaction) fn eligible(
	query: &LiveQuery,
	class: &ClassSchema,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<bool> {
	budget.admit(0, 1)?;
	if !matches!(query.source(), LiveSource::Class(_) | LiveSource::Record(_))
		|| query.graph_result().is_some()
		|| !query.joins().is_empty()
		|| !query.fetch().is_empty()
		|| query.group().is_some()
		|| query.only()
		|| !query.split().is_empty()
		|| !query.order().is_empty()
		|| query.limit().is_some()
		|| query.start_offset().is_some()
	{
		return Ok(false);
	}
	if let Some(filter) = query.filter()
		&& !expression(filter, class, 0, budget, query.alias())?
	{
		return Ok(false);
	}
	match query.projection() {
		LiveProjection::Records => Ok(true),
		LiveProjection::Value(value) => expression(value, class, 0, budget, query.alias()),
		LiveProjection::Fields(fields) => {
			for field in fields {
				if !expression(&field.expression, class, 0, budget, query.alias())? {
					return Ok(false);
				}
			}
			Ok(true)
		}
	}
}

/// Optional class qualification uses exactly the normal lowering rule.
pub(super) fn expression(
	value: &LiveExpr,
	class: &ClassSchema,
	depth: usize,
	budget: &mut BindingAdmission,
	alias: Option<&crate::values::Text>,
) -> FerrosGrynnResult<bool> {
	budget.admit(0, 1)?;
	if depth > 64 {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE historical expression depth",
			64,
		));
	}
	match value {
		LiveExpr::Range {
			start,
			end,
			..
		} => {
			for child in start.iter().chain(end.iter()) {
				if !expression(child, class, depth + 1, budget, alias)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		LiveExpr::Literal(_) | LiveExpr::Parameter(_) => Ok(true),
		LiveExpr::Field(path) => {
			budget.admit(0, class.fields().len() as u64)?;
			let fields = crate::gql::live_lowering::source_field_segments(path, Some(class), alias);
			Ok(fields.len() == 1
				|| fields.len() == 2 && fields[0].as_str().eq_ignore_ascii_case("properties"))
		}
		LiveExpr::Not(value)
		| LiveExpr::Negate(value)
		| LiveExpr::Positive(value)
		| LiveExpr::Cast {
			value,
			..
		} => expression(value, class, depth + 1, budget, alias),
		LiveExpr::Access {
			base,
			..
		} => {
			// An object constructor cannot dereference a record at this step.
			// Every constructed member still needs the row-local proof.
			if matches!(base.as_ref(), LiveExpr::Object(_)) {
				expression(base, class, depth + 1, budget, alias)
			} else {
				Ok(false)
			}
		}
		LiveExpr::Index {
			base,
			index,
		} => Ok(expression(base, class, depth + 1, budget, alias)?
			&& expression(index, class, depth + 1, budget, alias)?),
		LiveExpr::Array(values) | LiveExpr::Set(values) => {
			for value in values {
				if !expression(value, class, depth + 1, budget, alias)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		LiveExpr::Object(fields) => {
			for (_, value) in fields {
				if !expression(value, class, depth + 1, budget, alias)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		LiveExpr::Binary {
			left,
			right,
			..
		} => Ok(expression(left, class, depth + 1, budget, alias)?
			&& expression(right, class, depth + 1, budget, alias)?),
		LiveExpr::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			for child in [condition, then_value, else_value] {
				if !expression(child, class, depth + 1, budget, alias)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		LiveExpr::Case {
			operand,
			branches,
			else_value,
		} => {
			for child in operand
				.iter()
				.map(Box::as_ref)
				.chain(branches.iter().flat_map(|(condition, value)| [condition, value]))
				.chain(else_value.iter().map(Box::as_ref))
			{
				if !expression(child, class, depth + 1, budget, alias)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		LiveExpr::Binding(_)
		| LiveExpr::CallWithOptions {
			..
		}
		| LiveExpr::Call {
			..
		}
		| LiveExpr::Select(_) => Ok(false),
	}
}

/// Changed-row reads can omit unrelated records; complete-domain reads must
/// retain them so FETCH and record navigation can resolve linked members.
#[derive(Clone, Copy)]
pub(super) enum ImageSelection {
	/// Only rows changed by this commit, before the mutation.
	BeforeChanges,
	/// Only rows changed by this commit, after the mutation.
	AfterChanges,
	/// All reconstructed class members needed for dependencies.
	CompleteDomain,
}

/// Reconstructs only captured rows and stable schema/dictionary metadata.
/// Foreign primary records, indexes and statistics are never copied.
pub(super) fn evaluate(
	base: &DatabaseRoot,
	definition: &LiveDefinition,
	batch: &ChangeBatch,
	selection: ImageSelection,
	scratch_root: PathBuf,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<QueryEvaluation> {
	let class_id = definition.source_class.ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("historical row source has no class owner")
	})?;
	let class = base
		.schema
		.class_by_id(class_id)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("historical row class is absent"))?;
	budget.admit(std::mem::size_of::<DatabaseRoot>() + 2048 + class.name().len(), 1)?;
	let mut schema = SchemaCatalog::default();
	schema.next_schema_id = base.schema.next_schema_id;
	schema.classes.insert(class_id, Arc::clone(&class));
	schema.class_names.insert(class.name().to_string(), class_id);
	schema.class_versions.insert(class_id, base.version(ConflictKey::SchemaClass(class_id)));
	let mut root = DatabaseRoot::default();
	root.database_uuid = base.database_uuid;
	root.commit_ts = batch.versionstamp().get();
	root.committed_at_ns = batch.committed_at().as_inner().timestamp().as_nanosecond();
	root.schema = Arc::new(schema);
	root.dictionary = Arc::clone(&base.dictionary);
	let mut installed = BTreeSet::new();
	let mut context_admitted = false;
	for entry in batch.entries() {
		budget.admit(0, 1)?;
		for image in [entry.before(), entry.after()].into_iter().flatten() {
			validate_image(image, &class)?;
		}
		if let (Some(before), Some(after)) = (entry.before(), entry.after())
			&& before.identity() != after.identity()
		{
			return Err(FerrosGrynnError::invalid_storage_file("historical NET identity changed"));
		}
		let Some(image) = (match selection {
			ImageSelection::BeforeChanges => entry.before(),
			ImageSelection::AfterChanges | ImageSelection::CompleteDomain => entry.after(),
		}) else {
			continue;
		};
		let ChangeIdentity::Record(atom) = image.identity() else {
			return Err(FerrosGrynnError::invalid_storage_file("historical row is not global"));
		};
		if !matches!(selection, ImageSelection::CompleteDomain)
			&& let LiveSource::Record(record) = definition.query.source()
			&& record != atom.id()
		{
			continue;
		}
		let mut bytes =
			atom.id().estimated_heap_bytes().checked_add(1024).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("historical record identity")
			})?;
		for value in image.properties().values() {
			bytes = bytes
				.checked_add(value.estimated_heap_bytes())
				.and_then(|bytes| {
					bytes.checked_add(2 * std::mem::size_of::<crate::values::Value>() + 64)
				})
				.ok_or_else(|| {
					FerrosGrynnError::operation_overflow("historical record properties")
				})?;
		}
		// Ordinary insertion builds implicit class membership and statistics.
		// Its canonical context dies on return, so one peak allowance covers
		// these sequential clones. Each insertion still pays clone work.
		if !context_admitted {
			let mut context = base.dictionary.len().checked_mul(96).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("historical dictionary context")
			})?;
			for (_, name) in base.dictionary.iter() {
				budget.admit(0, 1)?;
				context = context.checked_add(name.len()).ok_or_else(|| {
					FerrosGrynnError::operation_overflow("historical dictionary names")
				})?;
			}
			budget.admit(context, 0)?;
			context_admitted = true;
		}
		budget.admit(bytes, image.properties().len() as u64 + base.dictionary.len() as u64)?;
		let mut properties = PropertyMap::with_capacity(image.properties().len());
		for (name, value) in image.properties().iter() {
			let key = base.dictionary.get(name.as_str()).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("historical property has no dictionary key")
			})?;
			properties.insert(key, value.clone());
		}
		let id = root.insert_atom(&class, atom.id().clone(), properties)?;
		installed.insert(id);
	}
	let root = Arc::new(root);
	let limits = budget.remaining_options()?;
	let options = definition.options.clone().with_budgets(limits.scratch_bytes(), limits.work())?;
	let evaluation = evaluate_query(
		&root,
		&definition.query,
		&|_id| Err(FerrosGrynnError::invalid_argument("historical reactive value is unavailable")),
		&options,
		scratch_root,
		&|_key| {},
	)?;
	super::super::live_result::admit_evaluation(&evaluation, budget)?;
	if !evaluation.functions.is_empty()
		|| evaluation
			.record_sources
			.iter()
			.any(|(name, id)| name != class.name() || *id != class_id)
		|| evaluation.reads.iter().any(|key| match key {
			ConflictKey::PropertyDictionary => false,
			// The reconstructed schema has no graphs. DISTINCT spill metadata
			// can inspect that empty catalog without reading historical inputs.
			// Eligibility already excludes graph operations; the other graph
			// and foreign input reads remain forbidden below.
			ConflictKey::GraphCatalogRange => false,
			ConflictKey::SchemaClass(id) | ConflictKey::BaseClassRange(id) => *id != class_id,
			ConflictKey::BaseAtom(id) => !installed.contains(id),
			_ => true,
		}) {
		return Err(FerrosGrynnError::query_feature_not_supported(
			"historical row-local SELECT read outside captured images",
		));
	}
	Ok(evaluation)
}

/// Malformed global images are corruption, never silently skipped rows.
pub(super) fn validate_image(image: &ChangeImage, class: &ClassSchema) -> FerrosGrynnResult<()> {
	let ChangeIdentity::Record(atom) = image.identity() else {
		return Err(FerrosGrynnError::invalid_storage_file("CLASS history contains an occurrence"));
	};
	if atom.kind() != class.kind()
		|| atom.id().class() != class.name().as_str()
		|| image.label().is_some()
		|| !image.overlays().is_empty()
		|| image.graph_state().is_some()
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"CLASS history image has another owner",
		));
	}
	Ok(())
}
