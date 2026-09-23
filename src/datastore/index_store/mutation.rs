use super::{IndexBinding, IndexEntries, IndexStoreRoot, model::IndexField};
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
	values::{PropertyDictionary, PropertyMap, Value, storage::EncodeContext},
};

#[derive(Debug)]
struct PreparedKey {
	index: IndexId,
	key: Option<Vec<u8>>,
}

/// Validated index work for one new database-global atom.
#[derive(Debug)]
pub(crate) struct PreparedBaseInsert {
	class: ClassId,
	keys: Vec<PreparedKey>,
}

/// Validated index work for one new graph occurrence.
#[derive(Debug)]
pub(crate) struct PreparedOccurrenceInsert {
	class: ClassId,
	keys: Vec<PreparedKey>,
}

#[derive(Debug)]
struct KeyChange {
	graph: Option<GraphId>,
	index: IndexId,
	address: Address,
	before: Option<Vec<u8>>,
	after: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy)]
enum Address {
	Global(GlobalAtomId),
	Local(AtomId),
}

/// Validated scalar-index changes for one base or occurrence replacement.
#[derive(Debug, Default)]
pub(crate) struct PreparedReplace {
	changes: Vec<KeyChange>,
}

impl IndexStoreRoot {
	pub(crate) fn register_class(&mut self, class: &ClassSchema) -> FerrosGrynnResult<()> {
		let class_id = class.id().ok_or_else(|| invalid("index owner has no ClassId"))?;
		if self.classes.insert(class_id, Default::default()).is_some() {
			return Err(invalid("class indexes are already registered"));
		}
		for index in class.indexes() {
			match index.scope() {
				IndexScope::Base => {
					let binding = binding::<GlobalAtomId>(class, index)?;
					if self.base.insert(binding.id, binding).is_some() {
						return Err(invalid("duplicate base IndexId"));
					}
				}
				IndexScope::Effective {
					graph,
				} => {
					let binding = binding::<AtomId>(class, index)?;
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

	pub(crate) fn prepare_base_insert(
		&self,
		class: &ClassSchema,
		properties: &PropertyMap,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<PreparedBaseInsert> {
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
		if let Some(label) = occurrence.label() {
			indexes
				.labels
				.entry((
					occurrence
						.local_id()
						.kind()
						.ok_or_else(|| invalid("live occurrence kind is null"))?,
					label.clone(),
				))
				.or_default()
				.insert(occurrence.local_id());
		}
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

	pub(crate) fn prepare_base_replace(
		&self,
		before: &BaseAtom,
		after: &BaseAtom,
		memberships: &[(GraphId, GraphOccurrence)],
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<PreparedReplace> {
		let mut prepared = PreparedReplace::default();
		for binding in self.base.values().filter(|binding| binding.class_id == before.class_id()) {
			push_change(
				&mut prepared,
				None,
				binding,
				before.id(),
				index_key(binding, |key| before.properties().get(key), context)?,
				index_key(binding, |key| after.properties().get(key), context)?,
			)?;
		}
		for (graph, occurrence) in memberships {
			let indexes =
				self.graphs.get(graph).ok_or_else(|| invalid("membership graph has no indexes"))?;
			for binding in
				indexes.explicit.values().filter(|binding| binding.class_id == before.class_id())
			{
				push_change(
					&mut prepared,
					Some(*graph),
					binding,
					occurrence.local_id(),
					effective_index_key(binding, before, occurrence, context)?,
					effective_index_key(binding, after, occurrence, context)?,
				)?;
			}
		}
		Ok(prepared)
	}

	pub(crate) fn prepare_occurrence_replace(
		&self,
		graph: GraphId,
		atom: &BaseAtom,
		before: &GraphOccurrence,
		after: &GraphOccurrence,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<PreparedReplace> {
		let indexes = self.graphs.get(&graph).ok_or_else(|| invalid("graph has no indexes"))?;
		let mut prepared = PreparedReplace::default();
		for binding in
			indexes.explicit.values().filter(|binding| binding.class_id == atom.class_id())
		{
			push_change(
				&mut prepared,
				Some(graph),
				binding,
				before.local_id(),
				effective_index_key(binding, atom, before, context)?,
				effective_index_key(binding, atom, after, context)?,
			)?;
		}
		Ok(prepared)
	}

	pub(crate) fn apply_replace(&mut self, prepared: PreparedReplace) -> FerrosGrynnResult<()> {
		for change in prepared.changes {
			match (change.graph, change.address) {
				(None, Address::Global(address)) => replace(
					&mut self
						.base
						.get_mut(&change.index)
						.ok_or_else(|| invalid("prepared base index disappeared"))?
						.entries,
					address,
					change.before,
					change.after,
				),
				(Some(graph), Address::Local(address)) => replace(
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
			}?
		}
		Ok(())
	}

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
		if let Some(label) = occurrence.label()
			&& let Some(members) = indexes.labels.get_mut(&(
				occurrence.local_id().kind().ok_or_else(|| invalid("occurrence kind is null"))?,
				label.clone(),
			)) {
			members.remove(&occurrence.local_id());
		}
		Ok(())
	}
}

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
		entries: IndexEntries::empty(index.kind()),
	})
}

fn index_key<'value, Address>(
	binding: &IndexBinding<Address>,
	mut value: impl FnMut(crate::values::PropertyKeyId) -> Option<&'value Value>,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Option<Vec<u8>>> {
	if binding.kind == ClassIndexKind::Count {
		return Ok(None);
	}
	let missing = Value::None;
	let components = binding
		.fields
		.iter()
		.map(|field| (value(field.key).unwrap_or(&missing), &field.value_type))
		.collect::<Vec<_>>();
	crate::values::storage::encode_index_key(&components, context).map(Some)
}

fn effective_index_key(
	binding: &IndexBinding<AtomId>,
	atom: &BaseAtom,
	occurrence: &GraphOccurrence,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Option<Vec<u8>>> {
	index_key(binding, |key| occurrence.effective_value(atom.properties().get(key), key), context)
}

fn push_change<Address: Copy + Ord>(
	prepared: &mut PreparedReplace,
	graph: Option<GraphId>,
	binding: &IndexBinding<Address>,
	address: Address,
	before: Option<Vec<u8>>,
	after: Option<Vec<u8>>,
) -> FerrosGrynnResult<()>
where
	Address: IntoAddress,
{
	if before == after {
		return Ok(());
	}
	if let Some(key) = &after {
		binding.entries.can_insert(key, Some(address), &binding.name)?;
	}
	prepared.changes.push(KeyChange {
		graph,
		index: binding.id,
		address: address.into_address(),
		before,
		after,
	});
	Ok(())
}

trait IntoAddress {
	fn into_address(self) -> Address;
}

impl IntoAddress for GlobalAtomId {
	fn into_address(self) -> Address {
		Address::Global(self)
	}
}

impl IntoAddress for AtomId {
	fn into_address(self) -> Address {
		Address::Local(self)
	}
}

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

pub(crate) fn index_context(
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
	for graph in schema.graphs() {
		context.register_graph(
			graph.reference().clone(),
			crate::values::storage::DiskGraphRef::new(graph.id()),
		)?;
	}
	Ok(context)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
