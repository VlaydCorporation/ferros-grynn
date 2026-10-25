//! Complete source domains reconstructed only from captured CLASS images.

use std::{collections::BTreeMap, ops::Bound, path::PathBuf, sync::Arc};

use fg_runtime::__internal::{RecordIdInternal as _, ValueInternal as _};

use crate::{
	changefeed::{
		ChangeBatch,
		ChangeEntry,
		ChangeIdentity,
		ChangeImage,
		ChangefeedReadItem,
		ChangefeedReadOptions,
		ChangefeedSince,
		ReadBatches,
	},
	datastore::{
		BindingAdmission,
		ChangefeedDefinition,
		ClassSchema,
		DatabaseRoot,
		LiveDefinition,
		SchemaVersion,
		StoreInner,
		source_history::SourceReader,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{
		LiveBatch,
		LiveBindingOptions,
		LiveExpr,
		LivePayload,
		LiveProjection,
		LiveQuery,
		LiveSource,
	},
	storage::LocalChangefeedSnapshot,
	values::{RecordId, Value},
};

/// The supported expressions consume only a complete captured source domain;
/// all computation remains in the ordinary analyzer and aggregate executor.
pub(in crate::datastore::transaction) fn eligible(
	query: &LiveQuery,
	class: &ClassSchema,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<bool> {
	budget.admit(0, 1)?;
	if !matches!(query.source(), LiveSource::Class(_) | LiveSource::Record(_))
		|| query.graph_result().is_some()
		|| !query.joins().is_empty()
	{
		return Ok(false);
	}
	if let Some(filter) = query.filter()
		&& !expression(filter, class, 0, budget)?
	{
		return Ok(false);
	}
	for value in query.split() {
		if !expression(value, class, 0, budget)? {
			return Ok(false);
		}
	}
	if let Some(keys) = query.group() {
		for key in keys {
			if !expression(key, class, 0, budget)? {
				return Ok(false);
			}
		}
	}
	for order in query.order() {
		match order.key() {
			crate::live::LiveOrderKey::Expression(value) => {
				if !expression(value, class, 0, budget)? {
					return Ok(false);
				}
			}
			crate::live::LiveOrderKey::Name(_) => budget.admit(0, 1)?,
		}
	}
	for value in query.limit().into_iter().chain(query.start_offset()) {
		if !expression(value, class, 0, budget)? {
			return Ok(false);
		}
	}
	for value in query.parameters().values() {
		if !isolated(value, class, 0, budget)? {
			return Ok(false);
		}
	}
	for path in query.omit().iter().chain(query.fetch()) {
		budget.admit(0, path.segments().len() as u64)?;
	}
	match query.projection() {
		LiveProjection::Records => Ok(true),
		LiveProjection::Value(value) => expression(value, class, 0, budget),
		LiveProjection::Fields(fields) => {
			for field in fields {
				if !expression(&field.expression, class, 0, budget)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
	}
}

/// Record navigation and FETCH may read other members of this complete
/// domain. Ordinary evaluation still checks the actual recorded reads.
fn expression(
	value: &LiveExpr,
	class: &ClassSchema,
	depth: usize,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<bool> {
	budget.admit(0, 1)?;
	if depth > 64 {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"historical domain expression depth",
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
				if !expression(child, class, depth + 1, budget)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		LiveExpr::Literal(value) => isolated(value, class, 0, budget),
		LiveExpr::Parameter(_) | LiveExpr::Field(_) => Ok(true),
		LiveExpr::Not(value)
		| LiveExpr::Negate(value)
		| LiveExpr::Positive(value)
		| LiveExpr::Cast {
			value,
			..
		} => expression(value, class, depth + 1, budget),
		LiveExpr::Access {
			base,
			..
		} => expression(base, class, depth + 1, budget),
		LiveExpr::Index {
			base,
			index,
		} => Ok(expression(base, class, depth + 1, budget)?
			&& expression(index, class, depth + 1, budget)?),
		LiveExpr::Array(values) | LiveExpr::Set(values) => {
			for value in values {
				if !expression(value, class, depth + 1, budget)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		LiveExpr::Object(fields) => {
			for (_, value) in fields {
				if !expression(value, class, depth + 1, budget)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		LiveExpr::Binary {
			left,
			right,
			..
		} => Ok(expression(left, class, depth + 1, budget)?
			&& expression(right, class, depth + 1, budget)?),
		LiveExpr::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			for child in [condition, then_value, else_value] {
				if !expression(child, class, depth + 1, budget)? {
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
				if !expression(child, class, depth + 1, budget)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		LiveExpr::Call {
			name,
			arguments,
		} if name.as_str().eq_ignore_ascii_case("count")
			|| name.as_str().eq_ignore_ascii_case("sum") =>
		{
			for argument in arguments {
				if !expression(argument, class, depth + 1, budget)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		LiveExpr::CallWithOptions {
			name,
			type_argument: None,
			arguments,
			..
		} if (name.as_str().eq_ignore_ascii_case("count")
			|| name.as_str().eq_ignore_ascii_case("sum"))
			&& arguments.iter().all(|(name, _)| name.is_none()) =>
		{
			for (_, argument) in arguments {
				if !expression(argument, class, depth + 1, budget)? {
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

/// A single reconstructed class cannot prove foreign or graph reference
/// lookups. This bounded conservative gate also inspects missing references.
fn isolated(
	value: &Value,
	class: &ClassSchema,
	depth: usize,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<bool> {
	budget.admit(0, 1)?;
	if depth > 64 {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"historical reference depth",
			64,
		));
	}
	match value {
		Value::Record(record) => Ok(record.class() == class.name().as_str()),
		Value::Atom(atom) => {
			Ok(atom.id().class() == class.name().as_str() && atom.kind() == class.kind())
		}
		Value::AtomOccurrence(_) | Value::GraphRef(_) | Value::Graph(_) | Value::Path(_) => {
			Ok(false)
		}
		Value::Array(values) => {
			for value in values {
				if !isolated(value, class, depth + 1, budget)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		Value::Set(values) => {
			for value in values {
				if !isolated(value, class, depth + 1, budget)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		Value::Object(values) => {
			for value in values.values() {
				if !isolated(value, class, depth + 1, budget)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		Value::Range(range) => {
			for bound in [range.start(), range.end()] {
				if let Bound::Included(value) | Bound::Excluded(value) = bound
					&& !isolated(value, class, depth + 1, budget)?
				{
					return Ok(false);
				}
			}
			Ok(true)
		}
		_ => Ok(true),
	}
}

/// Logical state has no physical addresses; each NET before image must match
/// the state reconstructed from earlier whole commits.
#[derive(Default)]
struct Domain {
	/// Latest exact image for every source member, including unchanged rows.
	images: BTreeMap<RecordId, Arc<ChangeImage>>,
	/// Checked conservative owned image and tree-node bytes.
	retained: usize,
}

impl Domain {
	/// Validates chains and admits replacement ownership before map growth.
	fn apply(
		&mut self,
		batch: &ChangeBatch,
		class: &ClassSchema,
		budget: &mut BindingAdmission,
	) -> FerrosGrynnResult<()> {
		let mut previous: Option<&RecordId> = None;
		for entry in batch.entries() {
			budget.admit(0, 1)?;
			for image in [entry.before(), entry.after()].into_iter().flatten() {
				super::rows::validate_image(image, class)?;
			}
			let image = entry
				.after()
				.or(entry.before())
				.ok_or_else(|| invalid("NET entry has no image"))?;
			let ChangeIdentity::Record(atom) = image.identity() else {
				return Err(invalid("CLASS NET entry is not global"));
			};
			let key = atom.id();
			if previous.is_some_and(|previous| previous >= key)
				|| !super::super::operation_image::same_image(
					self.images.get(key).map(Arc::as_ref),
					entry.before(),
				) {
				return Err(invalid("NET image chain or identity ordering is inconsistent"));
			}
			if entry.before().is_some_and(|before| before.identity() != image.identity()) {
				return Err(invalid("NET image identity changed"));
			}
			previous = Some(key);
			let old = self
				.images
				.get(key)
				.map(|image| image_bytes(image, budget))
				.transpose()?
				.unwrap_or(0);
			let new =
				entry.after().map(|image| image_bytes(image, budget)).transpose()?.unwrap_or(0);
			budget.admit(new, 1)?;
			self.retained = self
				.retained
				.checked_sub(old)
				.and_then(|bytes| bytes.checked_add(new))
				.ok_or_else(|| {
					FerrosGrynnError::operation_overflow("historical source domain bytes")
				})?;
			if let Some(image) = &entry.after {
				self.images.insert(key.clone(), Arc::clone(image));
			} else {
				self.images.remove(key);
			}
		}
		Ok(())
	}

	/// Admits the full image-reference vector before the ordinary root builder.
	fn snapshot(
		&self,
		clock: &ChangeBatch,
		budget: &mut BindingAdmission,
	) -> FerrosGrynnResult<ChangeBatch> {
		budget.admit(
			self.images.len().checked_mul(std::mem::size_of::<ChangeEntry>()).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("historical aggregate snapshot")
			})?,
			self.images.len() as u64,
		)?;
		let mut entries = Vec::new();
		entries.try_reserve_exact(self.images.len()).map_err(|error| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"historical aggregate snapshot",
				self.images.len() as u64,
			)
			.with_technical_source(error)
		})?;
		for (ordinal, image) in self.images.values().enumerate() {
			let ordinal = u64::try_from(ordinal).map_err(|error| {
				FerrosGrynnError::operation_overflow("historical snapshot ordinal")
					.with_technical_source(error)
			})?;
			entries.push(ChangeEntry {
				ordinal,
				operation: ordinal,
				before: None,
				after: Some(Arc::clone(image)),
			});
		}
		Ok(ChangeBatch {
			source: clock.source(),
			versionstamp: clock.versionstamp(),
			committed_at: clock.committed_at().clone(),
			entries,
		})
	}

	/// The frozen cutoff is only a consistency witness, never a fallback that
	/// fills missing historical records or properties from primary data.
	fn verify_cutoff(
		&self,
		root: &DatabaseRoot,
		class: &ClassSchema,
		budget: &mut BindingAdmission,
	) -> FerrosGrynnResult<()> {
		let id = class.id().ok_or_else(|| invalid("historical source class identity is absent"))?;
		let mut count = 0_usize;
		for global in root.indexes.base_class_iter(id) {
			budget.admit(0, 1)?;
			count = count
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("historical domain count"))?;
			let atom = root.atom(global).ok_or_else(|| invalid("source class member is absent"))?;
			let saved = self
				.images
				.get(atom.record_id())
				.ok_or_else(|| invalid("historical source tail is incomplete"))?;
			if atom.properties().len() != saved.properties().len() {
				return Err(invalid("historical source properties are incomplete"));
			}
			for (name, value) in saved.properties().iter() {
				budget.admit(0, 1)?;
				if !root
					.dictionary
					.get(name.as_str())
					.and_then(|key| atom.properties().get(key))
					.is_some_and(|actual| crate::values::runtime::same_value(actual, value))
				{
					return Err(invalid(
						"historical source differs from its frozen cutoff witness",
					));
				}
			}
		}
		if count != self.images.len() {
			return Err(invalid("historical source membership differs from cutoff"));
		}
		Ok(())
	}
}

/// Builds one complete-domain history from an independently proven empty
/// anchor. A lost prefix cannot be repaired from current primary records.
#[allow(
	clippy::too_many_arguments,
	reason = "One fixed-root preparation binds source proof, clocks, pins and shared admission"
)]
pub(super) fn prepare(
	store: &StoreInner,
	base: &Arc<DatabaseRoot>,
	definition: &LiveDefinition,
	feed: &ChangefeedDefinition,
	floor: u64,
	physical: Option<LocalChangefeedSnapshot>,
	scratch_root: PathBuf,
	envelopes: &mut Vec<(u64, Arc<[u8]>)>,
	outer: &mut BindingAdmission,
) -> FerrosGrynnResult<bool> {
	let class = definition
		.source_class
		.and_then(|id| base.schema.class_by_id(id))
		.ok_or_else(|| invalid("historical aggregate class is absent"))?;
	outer.admit(0, feed.epochs.len() as u64 + 1)?;
	if class.version() != SchemaVersion::INITIAL
		|| class.published_at != feed.defined_at
		|| feed.purged_through != 0
		|| feed.epochs.is_empty()
		|| feed.epochs[0].started_at().get() != class.published_at
		|| feed.epochs.iter().any(|epoch| epoch.configuration().is_none())
		|| !definition.functions.is_empty()
	{
		return Ok(false);
	}
	let limits = outer.remaining_options()?;
	let mut reader = SourceReader::new(
		store,
		Arc::clone(base),
		feed.id,
		ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(0.into()))
			.with_budgets(limits.scratch_bytes(), limits.work())?,
		physical,
		false,
	)?;
	let mut state = Domain::default();
	let mut read_work = 0;
	loop {
		let remaining = outer.remaining_options()?;
		reader.constrain_work(remaining.work())?;
		let source_scratch = remaining
			.scratch_bytes()
			.checked_sub(state.retained)
			.and_then(|bytes| bytes.checked_sub(std::mem::size_of::<SourceReader<'_>>() + 512))
			.ok_or_else(|| {
				FerrosGrynnError::traversal_resource_limit_exceeded(
					"historical aggregate source",
					remaining.scratch_bytes() as u64,
				)
			})?;
		let item = reader.next_batch(Some(source_scratch))?;
		let work = reader.statistics().work;
		outer.admit(
			0,
			work.checked_sub(read_work)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("aggregate reader work"))?,
		)?;
		read_work = work;
		let Some(item) = item else {
			break;
		};
		let ChangefeedReadItem::Commit(batch) = item else {
			envelopes.clear();
			return Ok(false);
		};
		for entry in batch.entries() {
			for image in [entry.before(), entry.after()].into_iter().flatten() {
				for value in image.properties().values() {
					if !isolated(value, &class, 0, outer)? {
						envelopes.clear();
						return Ok(false);
					}
				}
			}
		}
		let source_bytes =
			usize::try_from(reader.statistics().peak_scratch_bytes).map_err(|error| {
				FerrosGrynnError::operation_overflow("aggregate source scratch")
					.with_technical_source(error)
			})?;
		let remaining = outer.remaining_options()?;
		let scratch = remaining
			.scratch_bytes()
			.checked_sub(state.retained)
			.and_then(|bytes| bytes.checked_sub(source_bytes))
			.ok_or_else(|| {
				FerrosGrynnError::traversal_resource_limit_exceeded(
					"historical aggregate result",
					remaining.scratch_bytes() as u64,
				)
			})?;
		let limits = LiveBindingOptions::default().with_budgets(scratch, remaining.work())?;
		let mut temporary = BindingAdmission::new(&limits);
		let selected = batch.versionstamp().get() > floor;
		let old = if selected {
			let snapshot = state.snapshot(&batch, &mut temporary)?;
			Some(super::rows::evaluate(
				base,
				definition,
				&snapshot,
				super::rows::ImageSelection::CompleteDomain,
				scratch_root.clone(),
				&mut temporary,
			)?)
		} else {
			None
		};
		state.apply(&batch, &class, &mut temporary)?;
		let mut retained = 0;
		if let Some(old) = old {
			let snapshot = state.snapshot(&batch, &mut temporary)?;
			let new = super::rows::evaluate(
				base,
				definition,
				&snapshot,
				super::rows::ImageSelection::CompleteDomain,
				scratch_root.clone(),
				&mut temporary,
			)?;
			let events =
				super::super::live_result::net_changes(old.rows, new.rows, &mut temporary)?;
			if !events.is_empty() {
				let mut payload = LivePayload::Batch(LiveBatch {
					id: definition.id,
					version: definition.version,
					stamp: batch.versionstamp(),
					committed_at: batch.committed_at().clone(),
					events,
					bindings: Vec::new(),
				});
				super::encode(
					definition,
					&mut payload,
					batch.versionstamp().get(),
					batch.committed_at().as_inner().timestamp().as_nanosecond(),
					envelopes,
					&mut temporary,
				)?;
				retained = envelopes
					.last()
					.ok_or_else(|| invalid("aggregate envelope is absent"))?
					.1
					.len()
					.checked_add(128)
					.ok_or_else(|| {
						FerrosGrynnError::operation_overflow("aggregate retained result")
					})?;
			}
		}
		outer.admit(retained, limits.work() - temporary.remaining_options()?.work())?;
	}
	state.verify_cutoff(base, &class, outer)?;
	Ok(true)
}

/// Includes the domain tree key/node and complete retained source image.
fn image_bytes(image: &ChangeImage, budget: &mut BindingAdmission) -> FerrosGrynnResult<usize> {
	let ChangeIdentity::Record(atom) = image.identity() else {
		return Err(invalid("aggregate image is not global"));
	};
	image.properties().iter().try_fold(
		atom.id()
			.estimated_heap_bytes()
			.checked_mul(2)
			.and_then(|bytes| bytes.checked_add(1024))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("aggregate identity bytes"))?,
		|bytes, (name, value)| {
			budget.admit(0, 1)?;
			bytes
				.checked_add(name.len())
				.and_then(|bytes| bytes.checked_add(value.estimated_heap_bytes()))
				.and_then(|bytes| bytes.checked_add(256))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("aggregate image bytes"))
		},
	)
}

/// Source-chain corruption is distinct from unavailable coverage.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
