//! Resident ownership of the same immutable STR pages and exact replacements as
//! local storage.

use std::{
	collections::{BTreeMap, BTreeSet},
	sync::Arc,
};

use super::{
	GeometryDeltaRecord,
	GeometryHeader,
	decode_geometry_header,
	geometry_delta_records,
	invalid,
	search_geometry_generation,
};
use crate::{
	datastore::{ClassIndexKind, ClassIndexState, DatabaseRoot, IndexId},
	gql_status::FerrosGrynnResult,
	storage::{
		page::AlignedPage,
		scoped_index_image::{ScopedIndexKind, build_geometry_index_images},
	},
	values::RecordId,
};

/// Snapshot-owned physical base and bounded exact replacement set.
#[derive(Clone)]
pub struct ResidentGeometryIndex {
	/// Validated base metadata.
	pub(crate) header: GeometryHeader,
	/// Sealed pages stay in their final aligned allocation across snapshots.
	#[allow(
		clippy::rc_buffer,
		reason = "Converting sealed aligned pages into an Arc slice would move their storage"
	)]
	base: Arc<Vec<AlignedPage>>,
	/// Immutable replacements shared by snapshots.
	delta: Arc<[GeometryDeltaRecord]>,
}

impl ResidentGeometryIndex {
	/// Searches resident pages through the common admitted page walker.
	pub(crate) fn search(
		&self,
		query: [f64; 4],
		admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<super::GeometryCandidates> {
		search_geometry_generation(self.header, &self.delta, query, admit, |page, output| {
			let page = usize::try_from(page.get())
				.ok()
				.and_then(|page| self.base.get(page))
				.ok_or_else(|| invalid("resident geometry page is absent"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		})
	}
}

/// Builds or retains geometry generations before publishing a RAM root.
pub fn build_resident_geometry_indexes(
	previous: Option<&DatabaseRoot>,
	next: &DatabaseRoot,
	changed: Option<&BTreeSet<RecordId>>,
) -> FerrosGrynnResult<BTreeMap<IndexId, ResidentGeometryIndex>> {
	let mut result = BTreeMap::new();
	if let (Some(previous), Some(changed)) = (previous, changed) {
		for class in next.schema.classes() {
			for index in class.indexes().filter(|index| {
				index.kind() == ClassIndexKind::Geometry && index.state() == ClassIndexState::Ready
			}) {
				let id =
					index.id().ok_or_else(|| invalid("resident geometry index is unpublished"))?;
				let Some(old) = previous.resident_geometry.get(&id) else {
					continue;
				};
				let spec = old.header.spec;
				let field = index
					.fields()
					.next()
					.and_then(|name| class.fields().find(|field| field.name() == name))
					.and_then(|field| field.key);
				if Some(spec.class) != class.id()
					|| spec.index_version != index.version()
					|| spec.build_revision != index.build_revision()
					|| Some(spec.options) != index.geometry_options()
					|| spec.scope != index.scope()
					|| Some(spec.field) != field
				{
					continue;
				}
				if let Some(records) = geometry_delta_records(
					previous,
					next,
					changed,
					old.header,
					old.delta.iter().copied(),
				)? {
					let mut generation = old.clone();
					if records.values().copied().ne(old.delta.iter().copied()) {
						generation.delta = records.into_values().collect();
					}
					result.insert(id, generation);
				}
			}
		}
	}
	let skipped = result.keys().copied().collect::<BTreeSet<_>>();
	let images = build_geometry_index_images(next, &skipped)?;
	for file in images.base.into_iter().chain(images.graphs.into_values().flatten()) {
		let ScopedIndexKind::Explicit {
			index,
			..
		} = file.kind
		else {
			return Err(invalid("resident geometry image has no definition"));
		};
		let header = decode_geometry_header(
			file.pages.first().ok_or_else(|| invalid("resident geometry image has no header"))?,
			file.pages.len() as u64,
		)?;
		result.insert(index, ResidentGeometryIndex {
			header,
			base: Arc::new(file.pages),
			delta: Arc::from([]),
		});
	}
	Ok(result)
}

#[cfg(test)]
/// Regression tests for resident behavior.
mod tests {
	#[cfg(test)]
	use crate::test_assertions::AssertOption as _;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::{
		datastore::{
			ClassField,
			ClassIndex,
			ClassSchema,
			Datastore,
			GeometryIndexOptions,
			IndexScope,
		},
		graph::AtomKind,
		values::{PropertyMap, RecordId, RecordIdPart, Type},
	};

	/// Verifies empty geometry index does not reserve pages for other classes.
	#[test]
	fn empty_geometry_index_does_not_reserve_pages_for_other_classes() {
		let store = Datastore::in_memory();
		let mut define = store.begin();
		define.define_class(ClassSchema::new("other", AtomKind::Node, []).assert_ok()).assert_ok();
		define.commit().assert_ok();
		store
			.import(crate::datastore::ImportOptions::default(), |session| {
				for ordinal in 0..1000_i64 {
					session.apply_item(|transaction| {
						transaction.create_atom(
							RecordId::new("other", RecordIdPart::Int(ordinal))?,
							PropertyMap::new(),
						)
					})?;
				}
				Ok(())
			})
			.assert_ok();
		let mut define = store.begin();
		define
			.define_class(
				ClassSchema::new("place", AtomKind::Node, [ClassField::new(
					"shape",
					Type::Geometry,
				)])
				.assert_ok()
				.with_index(
					ClassIndex::new_geometry(
						"by_shape",
						IndexScope::Base,
						"shape",
						GeometryIndexOptions::default(),
					)
					.assert_ok(),
				)
				.assert_ok(),
			)
			.assert_ok();
		define.commit().assert_ok();
		let root = store.inner.current_root();
		let index = root.resident_geometry.values().next().assert_some();
		assert_eq!(index.header.row_count, 0);
		assert_eq!(index.header.page_count, 1);
		assert_eq!(index.base.len(), 1);
		assert_eq!(index.base.capacity(), 1);
	}
}
