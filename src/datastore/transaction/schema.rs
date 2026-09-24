use std::sync::Arc;

use super::{Mutation, Transaction};
use crate::{
	datastore::{
		ClassField,
		ClassIndex,
		ClassIndexState,
		ClassSchema,
		ClassSchemaMode,
		ConflictKey,
		IndexId,
		IndexScope,
		consts::SCHEMA_WRITE_BYTES,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphRef,
};

impl Transaction {
	/// Resolves a class from the transaction-visible schema root.
	///
	/// # Errors
	///
	/// Returns a snapshot-lifetime or read-committed refresh error.
	pub fn class(&mut self, name: &str) -> FerrosGrynnResult<Option<Arc<ClassSchema>>> {
		self.prepare_read()?;
		Ok(self.working.schema.class(name))
	}

	/// Resolves a named graph from the transaction-visible schema root.
	///
	/// # Errors
	///
	/// Returns a snapshot-lifetime or read-committed refresh error.
	pub fn named_graph(&mut self, name: &str) -> FerrosGrynnResult<Option<GraphRef>> {
		self.prepare_read()?;
		Ok(self.working.schema.named_graph(name).map(|schema| schema.reference().clone()))
	}

	/// Returns all transaction-visible class definitions in stable identity
	/// order.
	///
	/// # Errors
	///
	/// Returns a snapshot-lifetime or read-committed refresh error.
	pub fn classes(&mut self) -> FerrosGrynnResult<Vec<Arc<ClassSchema>>> {
		self.prepare_read()?;
		Ok(self.working.schema.classes().cloned().collect())
	}

	/// Returns all transaction-visible graph definitions in stable identity
	/// order.
	///
	/// # Errors
	///
	/// Returns a snapshot-lifetime or read-committed refresh error.
	pub fn graphs(&mut self) -> FerrosGrynnResult<Vec<crate::datastore::GraphSchema>> {
		self.prepare_read()?;
		Ok(self.working.schema.graphs().map(|graph| graph.as_ref().clone()).collect())
	}

	/// Adds one field to an existing class.
	///
	/// # Errors
	///
	/// Returns a schema, identity-capacity, read-only, snapshot, or
	/// write-budget error.
	pub fn define_field(&mut self, class: &str, mut field: ClassField) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let mut definition = self.require_class(class)?;
		let class_id = definition.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("published class is missing ClassId")
		})?;
		field.id = Some(self.store.inner.allocate_schema_id()?);
		field.class_id = Some(class_id);
		field.key = Some(self.intern_property(field.name().as_str())?);
		definition = definition.with_field(field)?;
		self.publish_class_replacement(definition)
	}

	/// Replaces one field type while preserving the field identity.
	///
	/// Existing values must already satisfy the new declaration; until the full
	/// value coercion matrix lands, a populated field cannot change type.
	///
	/// # Errors
	///
	/// Returns a schema, dependency, data, read-only, snapshot, or write-budget
	/// error.
	pub fn overwrite_field(&mut self, class: &str, field: ClassField) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let mut definition = self.require_class(class)?;
		let position = definition
			.fields
			.iter()
			.position(|current| current.name() == field.name())
			.ok_or_else(|| FerrosGrynnError::unknown_field(field.name().as_str()))?;
		let current = definition.fields[position].clone();
		if current.value_type() != field.value_type()
			&& self.class_property_is_populated(&definition, &current)?
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"field '{}.{}' contains values and cannot change type without validation",
				class,
				field.name()
			)));
		}
		let mut replacement = field;
		replacement.id = current.id;
		replacement.class_id = current.class_id;
		replacement.key = current.key;
		replacement.version = current.version.next()?;
		definition.fields[position] = replacement;
		self.publish_class_replacement(definition)
	}

	/// Removes one unused field definition.
	///
	/// # Errors
	///
	/// Returns an error when data or an index still depends on the field.
	pub fn remove_field(&mut self, class: &str, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let mut definition = self.require_class(class)?;
		let position = definition
			.fields
			.iter()
			.position(|field| field.name().as_str() == name)
			.ok_or_else(|| FerrosGrynnError::unknown_field(name))?;
		if definition.indexes.iter().any(|index| index.fields().any(|field| field.as_str() == name))
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"field '{class}.{name}' is referenced by an index"
			)));
		}
		if self.class_property_is_populated(&definition, &definition.fields[position])? {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"field '{class}.{name}' still contains values"
			)));
		}
		definition.fields.remove(position);
		self.publish_class_replacement(definition)
	}

	/// Adds one explicit base or effective class index.
	///
	/// # Errors
	///
	/// Returns a schema, graph, uniqueness, identity-capacity, or resource
	/// error.
	pub fn define_index(&mut self, class: &str, mut index: ClassIndex) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		self.validate_index_scope(index.scope())?;
		index.id = Some(IndexId::from_schema_id(self.store.inner.allocate_schema_id()?));
		let definition = self.require_class(class)?.with_index(index)?;
		self.publish_class_replacement(definition)
	}

	/// Replaces one class index while preserving its stable identity.
	///
	/// # Errors
	///
	/// Returns a schema, graph, uniqueness, read-only, snapshot, or resource
	/// error.
	pub fn overwrite_index(&mut self, class: &str, mut index: ClassIndex) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		self.validate_index_scope(index.scope())?;
		let mut definition = self.require_class(class)?;
		let position = definition
			.indexes
			.iter()
			.position(|current| current.name() == index.name())
			.ok_or_else(|| self.index_missing(class, index.name().as_str()))?;
		index.id = definition.indexes[position].id;
		index.version = definition.indexes[position].version.next()?;
		definition.indexes.remove(position);
		definition = definition.with_index(index)?;
		self.publish_class_replacement(definition)
	}

	/// Marks one index unavailable before explicit removal.
	///
	/// # Errors
	///
	/// Returns a schema, read-only, snapshot, or write-budget error.
	pub fn prepare_index_removal(&mut self, class: &str, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let mut definition = self.require_class(class)?;
		let index = definition
			.indexes
			.iter_mut()
			.find(|index| index.name().as_str() == name)
			.ok_or_else(|| self.index_missing(class, name))?;
		index.state = ClassIndexState::PreparedForRemoval;
		index.version = index.version.next()?;
		self.publish_class_replacement(definition)
	}

	/// Removes one explicit class index.
	///
	/// # Errors
	///
	/// Returns a schema, read-only, snapshot, or write-budget error.
	pub fn remove_index(&mut self, class: &str, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let mut definition = self.require_class(class)?;
		let position = definition
			.indexes
			.iter()
			.position(|index| index.name().as_str() == name)
			.ok_or_else(|| self.index_missing(class, name))?;
		definition.indexes.remove(position);
		self.publish_class_replacement(definition)
	}

	/// Changes only the validation mode of an existing class.
	///
	/// # Errors
	///
	/// Returns an error when undeclared stored properties prevent Schemafull.
	pub fn set_class_schema_mode(
		&mut self,
		class: &str,
		mode: ClassSchemaMode,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let mut definition = self.require_class(class)?;
		if mode == ClassSchemaMode::Schemafull && self.class_has_undeclared_properties(&definition)
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"class '{class}' contains undeclared properties"
			)));
		}
		definition.mode = mode;
		self.publish_class_replacement(definition)
	}

	/// Removes an empty class.
	///
	/// # Errors
	///
	/// Returns an error when the class is absent, owns atoms, or mutation
	/// policy rejects the operation.
	pub fn remove_class(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let class = self.require_class(name)?;
		let id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("published class is missing ClassId")
		})?;
		self.working.remove_class(id)?;
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.mutations.push(Mutation::RemoveClass(id));
		Ok(())
	}

	/// Removes a graph while retaining its global atoms.
	///
	/// # Errors
	///
	/// Returns an error when the graph is absent or mutation policy rejects the
	/// operation.
	pub fn remove_graph(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let graph = self
			.working
			.schema
			.named_graph(name)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name))?
			.id();
		let ranges = self
			.working
			.graph(graph)
			.into_iter()
			.flat_map(crate::datastore::GraphRoot::occurrences)
			.map(|occurrence| {
				self.atom_domain(occurrence.global_id()).map(|(class, kind)| (class, kind))
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		self.working.remove_graph(graph)?;
		for (class, kind) in ranges {
			self.mark_range_change(ConflictKey::GraphClassRange(graph, class));
			self.mark_range_change(ConflictKey::GraphKindRange(graph, kind));
		}
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.mutations.push(Mutation::RemoveGraph(graph));
		Ok(())
	}

	fn publish_class_replacement(&mut self, mut definition: ClassSchema) -> FerrosGrynnResult<()> {
		definition.version = definition.version.next()?;
		self.working.replace_class(definition.clone(), self.snapshot.commit_ts)?;
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.mutations.push(Mutation::ReplaceClass(definition));
		Ok(())
	}

	fn require_class(&self, name: &str) -> FerrosGrynnResult<ClassSchema> {
		self.working
			.schema
			.class(name)
			.map(|class| class.as_ref().clone())
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist(name))
	}

	fn validate_index_scope(&self, scope: IndexScope) -> FerrosGrynnResult<()> {
		if let IndexScope::Effective {
			graph,
		} = scope
			&& self.working.schema.graph(graph).is_none()
		{
			return Err(FerrosGrynnError::graph_does_not_exist(graph.to_string()));
		}
		Ok(())
	}

	fn class_property_is_populated(
		&self,
		class: &ClassSchema,
		field: &ClassField,
	) -> FerrosGrynnResult<bool> {
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("published class is missing ClassId")
		})?;
		let Some(key) = field.key else {
			return Ok(false);
		};
		for atom in self.working.atoms.atoms().filter(|atom| atom.class_id() == class_id) {
			if atom.properties().get(key).is_some() {
				return Ok(true);
			}
			for (graph, local) in self.working.memberships(atom.id()).into_iter().flatten() {
				if self
					.working
					.occurrence(*graph, *local)
					.and_then(|occurrence| occurrence.overlay())
					.and_then(|overlay| overlay.get(key))
					.is_some()
				{
					return Ok(true);
				}
			}
		}
		Ok(false)
	}

	fn class_has_undeclared_properties(&self, class: &ClassSchema) -> bool {
		let Some(class_id) = class.id() else {
			return true;
		};
		let declared = class
			.fields
			.iter()
			.filter_map(|field| field.key)
			.collect::<std::collections::BTreeSet<_>>();
		self.working
			.atoms
			.atoms()
			.filter(|atom| atom.class_id() == class_id)
			.any(|atom| atom.properties().iter().any(|(key, _value)| !declared.contains(&key)))
	}

	fn index_missing(&self, class: &str, name: &str) -> FerrosGrynnError {
		FerrosGrynnError::invalid_argument(format!("index '{class}.{name}' does not exist"))
	}
}

#[cfg(test)]
mod tests {
	use crate::{
		datastore::{
			ClassField,
			ClassIndex,
			ClassIndexKind,
			ClassSchema,
			ClassSchemaMode,
			Datastore,
			IndexScope,
		},
		values::{AtomKind, Type},
	};

	#[test]
	fn class_ddl_preserves_identities_and_rebuilds_indexes() {
		let store = Datastore::in_memory();
		let mut transaction = store.begin();
		let class_id = transaction
			.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap())
			.unwrap();
		transaction.define_field("person", ClassField::new("email", Type::String)).unwrap();
		transaction
			.define_index(
				"person",
				ClassIndex::new("by_email", ClassIndexKind::Unique, IndexScope::Base, ["email"])
					.unwrap(),
			)
			.unwrap();
		transaction.set_class_schema_mode("person", ClassSchemaMode::Schemafull).unwrap();
		transaction.commit().unwrap();

		let class = store.class("person").unwrap();
		assert_eq!(class.id(), Some(class_id));
		assert_eq!(class.fields().len(), 1);
		assert_eq!(class.indexes().len(), 1);
		assert_eq!(class.schema_mode(), ClassSchemaMode::Schemafull);
	}

	#[test]
	fn class_removal_rejects_live_atoms() {
		let store = Datastore::in_memory();
		let mut transaction = store.begin();
		transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
		transaction
			.create_atom(
				crate::values::RecordId::new(
					"person",
					crate::values::RecordIdPart::String("alice".into()),
				)
				.unwrap(),
				crate::values::PropertyMap::new(),
			)
			.unwrap();
		assert!(transaction.remove_class("person").is_err());
	}
}
