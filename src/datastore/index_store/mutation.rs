use super::{
	IndexBinding,
	IndexEntries,
	IndexStoreRoot,
	key_preparation::{KeyPreparation, effective_index_key, index_key},
	model::IndexField,
};
use crate::{
	datastore::{
		BaseAtom,
		ClassId,
		ClassIndex,
		ClassIndexKind,
		ClassSchema,
		GlobalAtomId,
		GraphOccurrence,
		IndexId,
		IndexScope,
		SchemaCatalog,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, GraphId},
	values::{PropertyDictionary, PropertyMap, Text, storage::EncodeContext},
};

#[derive(Debug)]
/// Prepared key state.
struct PreparedKey {
	/// Index associated with this prepared key.
	index: IndexId,
	/// Key associated with this prepared key.
	key: Option<Vec<u8>>,
}

/// Validated index work for one new database-global atom.
#[derive(Debug)]
pub struct PreparedBaseInsert {
	/// Class associated with this prepared base insert.
	class: ClassId,
	/// Keys associated with this prepared base insert.
	keys: Vec<PreparedKey>,
}

/// Validated index work for one new graph occurrence.
#[derive(Debug)]
pub struct PreparedOccurrenceInsert {
	/// Class associated with this prepared occurrence insert.
	class: ClassId,
	/// Keys associated with this prepared occurrence insert.
	keys: Vec<PreparedKey>,
}

#[derive(Debug)]
/// Canonical net keys and their typed address, shared with physical planning.
pub struct PreparedIndexChange {
	/// Effective graph, or the database-global Base scope.
	pub(crate) graph: Option<GraphId>,
	/// Globally unique catalog identity of the changed binding.
	pub(crate) index: IndexId,
	/// Logical address selected by the binding's Base or Effective scope.
	pub(crate) address: IndexAddress,
	/// Source canonical key, absent when the indexed value is omitted.
	pub(crate) before: Option<Vec<u8>>,
	/// Final candidate key after replay and synchronous effects.
	pub(crate) after: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy)]
/// Scope-specific address retained independently of durable page codecs.
pub enum IndexAddress {
	/// Global case of address.
	Global(
		/// Global case of address.
		GlobalAtomId,
	),
	/// Local case of address.
	Local(
		/// Local case of address.
		AtomId,
	),
}

/// Validated scalar-index changes for one base or occurrence replacement.
#[derive(Debug, Default)]
pub struct PreparedReplace {
	/// Changes associated with this prepared replace.
	changes: Vec<PreparedIndexChange>,
}

impl PreparedReplace {
	/// Returns whether validation found any index entries to replace.
	pub(in crate::datastore) fn is_empty(&self) -> bool {
		self.changes.is_empty()
	}
}

/// Owned label transition prepared alongside the occurrence's explicit keys.
#[derive(Debug)]
struct PreparedLabelChange {
	/// Source label removed from the same graph and atom kind.
	before: Option<Text>,
	/// Final label inserted after source validation.
	after: Option<Text>,
}

/// Validated explicit and engine-owned work for one occurrence replacement.
#[derive(Debug)]
pub struct PreparedOccurrenceReplace {
	/// Scope remains independent of database-global Base bindings.
	graph: GraphId,
	/// Stable local identity shared by the old and new occurrence.
	local: AtomId,
	/// Canonical explicit scalar changes keep the Base preparation unchanged.
	explicit: PreparedReplace,
	/// Absent for overlay/state changes whose label is unchanged.
	label: Option<PreparedLabelChange>,
}

impl PreparedOccurrenceReplace {
	/// Includes label-only work even when no explicit index is defined.
	pub(in crate::datastore) fn is_empty(&self) -> bool {
		self.explicit.is_empty() && self.label.is_none()
	}
}

impl IndexStoreRoot {
	/// Encodes one accepted Base insertion/removal without revalidating Unique
	/// against the final index, which already contains the inserted atom.
	///
	/// # Errors
	/// Propagates canonical key encoding failures.
	pub(crate) fn prepare_base_transition(
		&self,
		atom: &BaseAtom,
		insert: bool,
		context: &mut EncodeContext,
		maximum: usize,
	) -> FerrosGrynnResult<(Vec<PreparedIndexChange>, usize)> {
		let mut preparation = KeyPreparation::bounded(maximum);
		let mut changes = Vec::new();
		for binding in self.base.values().filter(|binding| binding.class_id == atom.class_id()) {
			let key = preparation.key(binding, |key| atom.properties().get(key), context)?;
			if let Some(key) = key {
				let (before, after) = if insert {
					(None, Some(key))
				} else {
					(Some(key), None)
				};
				preparation.push(&mut changes, PreparedIndexChange {
					graph: None,
					index: binding.id,
					address: IndexAddress::Global(atom.id()),
					before,
					after,
				})?;
			}
		}
		preparation.finish(changes)
	}

	/// Encodes accepted membership transitions without checking final Unique
	/// entries against an atom that is already present in the candidate.
	///
	/// # Errors
	/// Propagates missing graph bindings and canonical key encoding failures.
	pub(crate) fn prepare_effective_transition(
		&self,
		graph: GraphId,
		atom: &BaseAtom,
		occurrence: &GraphOccurrence,
		insert: bool,
		context: &mut EncodeContext,
		maximum: usize,
	) -> FerrosGrynnResult<(Vec<PreparedIndexChange>, usize)> {
		let mut preparation = KeyPreparation::bounded(maximum);
		let indexes = self.graphs.get(&graph).ok_or_else(|| invalid("graph has no indexes"))?;
		let mut changes = Vec::new();
		for binding in
			indexes.explicit.values().filter(|binding| binding.class_id == atom.class_id())
		{
			if let Some(key) = preparation.effective_key(binding, atom, occurrence, context)? {
				let (before, after) = if insert {
					(None, Some(key))
				} else {
					(Some(key), None)
				};
				preparation.push(&mut changes, PreparedIndexChange {
					graph: Some(graph),
					index: binding.id,
					address: IndexAddress::Local(occurrence.local_id()),
					before,
					after,
				})?;
			}
		}
		preparation.finish(changes)
	}

	/// Registers class.
	pub(crate) fn register_class(&mut self, class: &ClassSchema) -> FerrosGrynnResult<()> {
		self.register_class_with_mode(class, false)
	}

	/// Registers explicit metadata without reconstructing immutable side-file
	/// entries during local reopen.
	pub(crate) fn register_class_deferred(&mut self, class: &ClassSchema) -> FerrosGrynnResult<()> {
		self.register_class_with_mode(class, true)
	}

	/// Registers class with mode.
	fn register_class_with_mode(
		&mut self,
		class: &ClassSchema,
		deferred: bool,
	) -> FerrosGrynnResult<()> {
		let class_id = class.id().ok_or_else(|| invalid("index owner has no ClassId"))?;
		if self.classes.insert(class_id, BTreeSet::default()).is_some() {
			return Err(invalid("class indexes are already registered"));
		}
		for index in class.indexes() {
			if index.state() != crate::datastore::ClassIndexState::Ready {
				continue;
			}
			if !matches!(
				index.kind(),
				ClassIndexKind::Standard | ClassIndexKind::Unique | ClassIndexKind::Count
			) {
				continue;
			}
			match index.scope() {
				IndexScope::Base => {
					let mut binding = binding::<GlobalAtomId>(class, index)?;
					binding.deferred = deferred;
					if self.base.insert(binding.id, binding).is_some() {
						return Err(invalid("duplicate base IndexId"));
					}
				}
				IndexScope::Effective {
					graph,
				} => {
					let mut binding = binding::<AtomId>(class, index)?;
					binding.deferred = deferred;
					let graph = self
						.graphs
						.get_mut(&graph)
						.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
					if graph.explicit.insert(binding.id, binding).is_some() {
						return Err(invalid("duplicate effective IndexId"));
					}
				}
			}
		}
		Ok(())
	}

	/// Prepares base insert.
	pub(crate) fn prepare_base_insert(
		&self,
		class: &ClassSchema,
		properties: &PropertyMap,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<PreparedBaseInsert> {
		class.validate_geometry_values(IndexScope::Base, |key| properties.get(key))?;
		let class_id = class.id().ok_or_else(|| invalid("atom class has no ClassId"))?;
		let mut keys = Vec::new();
		for binding in self.base.values().filter(|binding| binding.class_id == class_id) {
			binding.entries.checked_increment()?;
			let key = index_key(binding, |key| properties.get(key), context)?;
			if let Some(key) = &key {
				binding.entries.can_insert(key, None, &binding.name)?;
			}
			keys.push(PreparedKey {
				index: binding.id,
				key,
			});
		}
		Ok(PreparedBaseInsert {
			class: class_id,
			keys,
		})
	}

	/// Applies base insert.
	pub(crate) fn apply_base_insert(
		&mut self,
		prepared: PreparedBaseInsert,
		atom: GlobalAtomId,
	) -> FerrosGrynnResult<()> {
		self.classes.entry(prepared.class).or_default().insert(atom);
		for key in prepared.keys {
			self.base
				.get_mut(&key.index)
				.ok_or_else(|| invalid("prepared base index disappeared"))?
				.entries
				.insert(key.key, atom)?;
		}
		Ok(())
	}

	/// Removes base.
	pub(crate) fn remove_base(
		&mut self,
		atom: &BaseAtom,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<()> {
		for binding in self.base.values_mut().filter(|binding| binding.class_id == atom.class_id())
		{
			let key = index_key(binding, |key| atom.properties().get(key), context)?;
			binding.entries.remove(key.as_deref(), atom.id())?;
		}
		let members = self
			.classes
			.get_mut(&atom.class_id())
			.ok_or_else(|| invalid("atom class index is absent"))?;
		if !members.remove(&atom.id()) {
			return Err(invalid("atom class index does not contain the removed atom"));
		}
		Ok(())
	}

	/// Prepares occurrence insert.
	pub(crate) fn prepare_occurrence_insert(
		&self,
		graph: GraphId,
		atom: &BaseAtom,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<PreparedOccurrenceInsert> {
		let indexes = self
			.graphs
			.get(&graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
		let mut keys = Vec::new();
		for binding in
			indexes.explicit.values().filter(|binding| binding.class_id == atom.class_id())
		{
			binding.entries.checked_increment()?;
			let key = index_key(binding, |key| atom.properties().get(key), context)?;
			if let Some(key) = &key {
				binding.entries.can_insert(key, None, &binding.name)?;
			}
			keys.push(PreparedKey {
				index: binding.id,
				key,
			});
		}
		Ok(PreparedOccurrenceInsert {
			class: atom.class_id(),
			keys,
		})
	}

	/// Validates index keys for an occurrence restored with an existing
	/// overlay. Fresh attachments use [`Self::prepare_occurrence_insert`].
	pub(crate) fn prepare_restored_occurrence_insert(
		&self,
		graph: GraphId,
		atom: &BaseAtom,
		occurrence: &GraphOccurrence,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<PreparedOccurrenceInsert> {
		let indexes = self
			.graphs
			.get(&graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
		let mut keys = Vec::new();
		for binding in
			indexes.explicit.values().filter(|binding| binding.class_id == atom.class_id())
		{
			binding.entries.checked_increment()?;
			let key = effective_index_key(binding, atom, occurrence, context)?;
			if let Some(key) = &key {
				binding.entries.can_insert(key, None, &binding.name)?;
			}
			keys.push(PreparedKey {
				index: binding.id,
				key,
			});
		}
		Ok(PreparedOccurrenceInsert {
			class: atom.class_id(),
			keys,
		})
	}

	/// Applies occurrence insert.
	pub(crate) fn apply_occurrence_insert(
		&mut self,
		graph: GraphId,
		occurrence: &GraphOccurrence,
		prepared: PreparedOccurrenceInsert,
	) -> FerrosGrynnResult<()> {
		let indexes = self
			.graphs
			.get_mut(&graph)
			.ok_or_else(|| invalid("prepared graph indexes disappeared"))?;
		indexes.classes.entry(prepared.class).or_default().insert(occurrence.local_id());
		indexes.replace_label(occurrence.local_id(), None, occurrence.label())?;
		for key in prepared.keys {
			indexes
				.explicit
				.get_mut(&key.index)
				.ok_or_else(|| invalid("prepared effective index disappeared"))?
				.entries
				.insert(key.key, occurrence.local_id())?;
		}
		Ok(())
	}

	/// Prepares base replace.
	pub(crate) fn prepare_base_replace<'a>(
		&self,
		before: &BaseAtom,
		after: &BaseAtom,
		memberships: impl IntoIterator<Item = FerrosGrynnResult<(GraphId, &'a GraphOccurrence)>>,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<PreparedReplace> {
		self.prepare_base_replace_with(
			before,
			after,
			memberships,
			context,
			&mut KeyPreparation::logical(),
		)
	}

	/// Encodes one publication-only Base replacement within its remaining
	/// ceiling.
	///
	/// # Errors
	/// Preserves key/Unique validation and rejects preparation resource excess.
	pub(crate) fn prepare_base_replace_bounded(
		&self,
		before: &BaseAtom,
		after: &BaseAtom,
		context: &mut EncodeContext,
		maximum: usize,
	) -> FerrosGrynnResult<(Vec<PreparedIndexChange>, usize)> {
		let mut preparation = KeyPreparation::bounded(maximum);
		let prepared = self.prepare_base_replace_with(
			before,
			after,
			std::iter::empty(),
			context,
			&mut preparation,
		)?;
		preparation.finish(prepared.changes)
	}

	/// Shares the existing logical walk and Unique validation with publication.
	fn prepare_base_replace_with<'a>(
		&self,
		before: &BaseAtom,
		after: &BaseAtom,
		memberships: impl IntoIterator<Item = FerrosGrynnResult<(GraphId, &'a GraphOccurrence)>>,
		context: &mut EncodeContext,
		preparation: &mut KeyPreparation,
	) -> FerrosGrynnResult<PreparedReplace> {
		let mut prepared = PreparedReplace::default();
		for binding in self.base.values().filter(|binding| binding.class_id == before.class_id()) {
			push_change(
				&mut prepared,
				None,
				binding,
				before.id(),
				preparation.key(binding, |key| before.properties().get(key), context)?,
				preparation.key(binding, |key| after.properties().get(key), context)?,
				preparation,
			)?;
		}
		for membership in memberships {
			let (graph, occurrence) = membership?;
			self.append_effective_replace(
				&mut prepared,
				graph,
				(before, occurrence),
				(after, occurrence),
				context,
				preparation,
			)?;
		}
		Ok(prepared)
	}

	/// Prepares explicit keys and a possible engine-owned label transition.
	///
	/// # Errors
	/// Propagates missing graph, key encoding and uniqueness failures.
	pub(crate) fn prepare_occurrence_replace(
		&self,
		graph: GraphId,
		atom: &BaseAtom,
		before: &GraphOccurrence,
		after: &GraphOccurrence,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<PreparedOccurrenceReplace> {
		let prepared =
			self.prepare_effective_replace(graph, (atom, before), (atom, after), context)?;
		Ok(PreparedOccurrenceReplace {
			graph,
			local: before.local_id(),
			explicit: prepared,
			label: (before.label() != after.label()).then(|| PreparedLabelChange {
				before: before.label().cloned(),
				after: after.label().cloned(),
			}),
		})
	}

	/// Encodes one net Effective transition for a stable global/local identity.
	/// Each side owns its Base and occurrence, so mixed commits need no
	/// intermediate physical key.
	///
	/// # Errors
	/// Propagates missing graph, key encoding and uniqueness failures.
	pub(crate) fn prepare_effective_replace(
		&self,
		graph: GraphId,
		before: (&BaseAtom, &GraphOccurrence),
		after: (&BaseAtom, &GraphOccurrence),
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<PreparedReplace> {
		let mut prepared = PreparedReplace::default();
		self.append_effective_replace(
			&mut prepared,
			graph,
			before,
			after,
			context,
			&mut KeyPreparation::logical(),
		)?;
		Ok(prepared)
	}

	/// Encodes one final Effective pair without resetting the per-side
	/// envelope.
	///
	/// # Errors
	/// Preserves key/Unique validation and rejects preparation resource excess.
	pub(crate) fn prepare_effective_replace_bounded(
		&self,
		graph: GraphId,
		before: (&BaseAtom, &GraphOccurrence),
		after: (&BaseAtom, &GraphOccurrence),
		context: &mut EncodeContext,
		maximum: usize,
	) -> FerrosGrynnResult<(Vec<PreparedIndexChange>, usize)> {
		let mut preparation = KeyPreparation::bounded(maximum);
		let mut prepared = PreparedReplace::default();
		self.append_effective_replace(
			&mut prepared,
			graph,
			before,
			after,
			context,
			&mut preparation,
		)?;
		preparation.finish(prepared.changes)
	}

	/// Shares the borrowed key walk without allocating per-membership plans.
	fn append_effective_replace(
		&self,
		prepared: &mut PreparedReplace,
		graph: GraphId,
		before: (&BaseAtom, &GraphOccurrence),
		after: (&BaseAtom, &GraphOccurrence),
		context: &mut EncodeContext,
		preparation: &mut KeyPreparation,
	) -> FerrosGrynnResult<()> {
		let indexes = self.graphs.get(&graph).ok_or_else(|| invalid("graph has no indexes"))?;
		for binding in
			indexes.explicit.values().filter(|binding| binding.class_id == before.0.class_id())
		{
			push_change(
				prepared,
				Some(graph),
				binding,
				before.1.local_id(),
				preparation.effective_key(binding, before.0, before.1, context)?,
				preparation.effective_key(binding, after.0, after.1, context)?,
				preparation,
			)?;
		}
		Ok(())
	}

	/// Applies explicit and label changes within the same private index root.
	///
	/// # Errors
	/// Rejects inconsistent prepared bindings or missing source label postings.
	pub(crate) fn apply_occurrence_replace(
		&mut self,
		prepared: PreparedOccurrenceReplace,
	) -> FerrosGrynnResult<()> {
		self.apply_replace(prepared.explicit)?;
		if let Some(label) = prepared.label {
			self.graphs
				.get_mut(&prepared.graph)
				.ok_or_else(|| invalid("prepared graph indexes disappeared"))?
				.replace_label(prepared.local, label.before.as_ref(), label.after.as_ref())?;
		}
		Ok(())
	}

	/// Applies replace.
	pub(crate) fn apply_replace(&mut self, prepared: PreparedReplace) -> FerrosGrynnResult<()> {
		for change in prepared.changes {
			match (change.graph, change.address) {
				(None, IndexAddress::Global(address)) => replace(
					&mut self
						.base
						.get_mut(&change.index)
						.ok_or_else(|| invalid("prepared base index disappeared"))?
						.entries,
					address,
					change.before,
					change.after,
				),
				(Some(graph), IndexAddress::Local(address)) => replace(
					&mut self
						.graphs
						.get_mut(&graph)
						.ok_or_else(|| invalid("prepared graph indexes disappeared"))?
						.explicit
						.get_mut(&change.index)
						.ok_or_else(|| invalid("prepared effective index disappeared"))?
						.entries,
					address,
					change.before,
					change.after,
				),
				_ => return Err(invalid("prepared index address and scope disagree")),
			}?;
		}
		Ok(())
	}

	/// Removes occurrence.
	pub(crate) fn remove_occurrence(
		&mut self,
		graph: GraphId,
		atom: &BaseAtom,
		occurrence: &GraphOccurrence,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<()> {
		let indexes = self.graphs.get_mut(&graph).ok_or_else(|| invalid("graph has no indexes"))?;
		for binding in
			indexes.explicit.values_mut().filter(|binding| binding.class_id == atom.class_id())
		{
			let key = effective_index_key(binding, atom, occurrence, context)?;
			binding.entries.remove(key.as_deref(), occurrence.local_id())?;
		}
		if let Some(members) = indexes.classes.get_mut(&atom.class_id()) {
			members.remove(&occurrence.local_id());
		}
		indexes.replace_label(occurrence.local_id(), occurrence.label(), None)?;
		Ok(())
	}
}

/// Returns the binding result.
fn binding<Address: Copy + Ord>(
	class: &ClassSchema,
	index: &ClassIndex,
) -> FerrosGrynnResult<IndexBinding<Address>> {
	let id = index.id().ok_or_else(|| invalid("published index has no IndexId"))?;
	let class_id = class.id().ok_or_else(|| invalid("published index owner has no ClassId"))?;
	let fields = index
		.fields()
		.map(|name| {
			let field = class
				.fields()
				.find(|field| field.name() == name)
				.ok_or_else(|| invalid("index field disappeared from its class"))?;
			Ok(IndexField {
				key: field
					.key
					.ok_or_else(|| invalid("published index field has no property key"))?,
				value_type: field.value_type().clone(),
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	Ok(IndexBinding {
		id,
		version: index.version(),
		class_id,
		name: index.name().clone(),
		kind: index.kind(),
		fields,
		deferred: false,
		entries: IndexEntries::empty(index.kind()),
	})
}

/// Pushes change.
fn push_change<Address>(
	prepared: &mut PreparedReplace,
	graph: Option<GraphId>,
	binding: &IndexBinding<Address>,
	address: Address,
	before: Option<Vec<u8>>,
	after: Option<Vec<u8>>,
	preparation: &mut KeyPreparation,
) -> FerrosGrynnResult<()>
where
	Address: Copy + Ord + IntoAddress,
{
	if before == after {
		return preparation.discard_equal(before, after);
	}
	if let Some(key) = &after {
		binding.entries.can_insert(key, Some(address), &binding.name)?;
	}
	preparation.push(&mut prepared.changes, PreparedIndexChange {
		graph,
		index: binding.id,
		address: address.into_address(),
		before,
		after,
	})
}

/// Operations required for into address.
trait IntoAddress {
	/// Converts this value into address.
	fn into_address(self) -> IndexAddress;
}

impl IntoAddress for GlobalAtomId {
	/// Converts this value into address for `GlobalAtomId`.
	fn into_address(self) -> IndexAddress {
		IndexAddress::Global(self)
	}
}

impl IntoAddress for AtomId {
	/// Converts this value into address for `AtomId`.
	fn into_address(self) -> IndexAddress {
		IndexAddress::Local(self)
	}
}

/// Replaces the current state.
fn replace<Address: Copy + Ord>(
	entries: &mut IndexEntries<Address>,
	address: Address,
	before: Option<Vec<u8>>,
	after: Option<Vec<u8>>,
) -> FerrosGrynnResult<()> {
	if before == after {
		return Ok(());
	}
	if let Some(key) = before.as_deref() {
		entries.remove(Some(key), address)?;
	}
	if let Some(key) = after {
		entries.insert(Some(key), address)?;
	}
	Ok(())
}

/// Returns the index context result.
pub fn index_context(
	schema: &SchemaCatalog,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<EncodeContext> {
	let mut context = EncodeContext::new(dictionary.clone());
	for class in schema.classes() {
		context.register_class_name(
			class.id().ok_or_else(|| invalid("published class has no ClassId"))?.get(),
			class.name().clone(),
		)?;
	}
	for graph in schema.all_graphs() {
		context.register_graph(
			graph.reference().clone(),
			crate::values::storage::DiskGraphRef::new(graph.id()),
		)?;
	}
	Ok(context)
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
use std::collections::BTreeSet;
