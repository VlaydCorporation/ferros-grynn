//! Sparse page sets, resize and malformed-source regressions.

use super::{
	super::{
		BuiltPropertyDictionary,
		HASH_NEXT_PAGE_OFFSET,
		HASH_USED_OFFSET,
		build_property_dictionary,
		decode_property_dictionary,
		decode_root,
		extend_property_dictionary,
		open_property_dictionary,
		page_no,
	},
	*,
};
use crate::test_assertions::{AssertOption as _, AssertResult as _};

/// Builds a dense runtime dictionary with predictable names.
fn dictionary(count: usize) -> PropertyDictionary {
	let mut result = PropertyDictionary::new();
	for id in 0..count {
		result.intern(format!("key-{id}")).assert_ok();
	}
	result
}

/// Copies aligned bytes without requiring a clone implementation on pages.
fn copied_pages(pages: &[AlignedPage]) -> Vec<AlignedPage> {
	pages
		.iter()
		.map(|source| {
			let mut page = AlignedPage::zeroed();
			page.as_bytes_mut().copy_from_slice(source.as_bytes());
			page
		})
		.collect()
}

/// Plans against immutable source bytes using the production reader contract.
fn plan(
	image: &BuiltPropertyDictionary,
	after: &PropertyDictionary,
) -> FerrosGrynnResult<PropertyDictionaryUpdate> {
	plan_property_dictionary_update(
		&mut |number, destination: &mut AlignedPage| {
			let index = usize::try_from(number.get())
				.map_err(|_error| invalid("test dictionary address exceeds usize"))?;
			let page =
				image.pages.get(index).ok_or_else(|| invalid("test source page is absent"))?;
			destination.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		},
		u64::try_from(image.pages.len()).assert_ok(),
		after,
		PageLsn::new(2),
	)
}

/// Applies an owned plan to a copied source and checks the full-image oracle.
fn assert_plan(
	image: BuiltPropertyDictionary,
	after: &PropertyDictionary,
	update: PropertyDictionaryUpdate,
) {
	let mut pages = copied_pages(&image.pages);
	let ceiling = usize::try_from(update.page_ceiling).assert_ok();
	pages.resize_with(ceiling, AlignedPage::zeroed);
	let mut unique = BTreeSet::new();
	for (number, page) in update.pages {
		assert!(unique.insert(number));
		let index = usize::try_from(number.get()).assert_ok();
		assert!(index < ceiling);
		page.validate(number).assert_ok();
		pages[index].as_bytes_mut().copy_from_slice(page.as_bytes());
	}
	let actual = open_property_dictionary(&pages, update.page_ceiling).assert_ok();
	let control = extend_property_dictionary(image, after, PageLsn::new(2)).assert_ok();
	let control =
		open_property_dictionary(&control.pages, u64::try_from(control.pages.len()).assert_ok())
			.assert_ok();
	assert_eq!(actual.iter().collect::<Vec<_>>(), after.iter().collect::<Vec<_>>());
	assert_eq!(actual.iter().collect::<Vec<_>>(), control.iter().collect::<Vec<_>>());
}

/// A small append changes only root, old entry tail, one hash and new entries.
#[test]
fn append_reuses_hash_capacity_and_preserves_source() {
	let mut after = dictionary(8);
	let image = build_property_dictionary(&after, PageLsn::new(1)).assert_ok();
	let source = copied_pages(&image.pages);
	let root = decode_root(&image.pages[0]).assert_ok();
	after.intern("fresh").assert_ok();
	let update = plan(&image, &after).assert_ok();
	assert_eq!(update.page_ceiling, u64::try_from(source.len() + 1).assert_ok());
	assert_eq!(
		update.pages.iter().map(|(number, _)| *number).collect::<BTreeSet<_>>(),
		BTreeSet::from([
			ROOT_PAGE,
			root.entry_last_page,
			root.hash_first_page,
			page_no(source.len()).assert_ok()
		])
	);
	for (before, current) in source.iter().zip(&image.pages) {
		assert_eq!(before.as_bytes(), current.as_bytes());
	}
	assert_plan(image, &after, update);
}

/// Sparse hashing follows validated chain addresses, including noncontiguous
/// ranges.
#[test]
fn append_changes_only_the_selected_hash_page_in_a_linked_range() {
	let mut after = dictionary(1024);
	let mut image = build_property_dictionary(&after, PageLsn::new(1)).assert_ok();
	let root = decode_root(&image.pages[0]).assert_ok();
	assert_eq!(root.hash_page_count, 2);
	let decoded = decode_property_dictionary(&image.pages, image.pages.len() as u64).assert_ok();
	let first_hash = decoded.hash_pages[0];
	let old_second = decoded.hash_pages[1];
	let relocated = page_no(image.pages.len()).assert_ok();
	image.pages.push(AlignedPage::zeroed());
	encode_hash_page(
		image.pages.last_mut().assert_some(),
		relocated,
		PageNo::NULL,
		&decoded.hash_buckets[HASH_BUCKETS_PER_PAGE..],
		PageLsn::new(1),
	)
	.assert_ok();
	let first = &mut image.pages[usize::try_from(first_hash.get()).assert_ok()];
	write_u64(first.as_bytes_mut(), HASH_NEXT_PAGE_OFFSET, relocated.get());
	first.seal().assert_ok();
	let name = (0..10000)
		.map(|id| format!("fresh-{id}"))
		.find(|name| {
			let slot = property_hash(name.as_bytes()) as usize % decoded.hash_buckets.len();
			slot >= HASH_BUCKETS_PER_PAGE && decoded.hash_buckets[slot].is_none()
		})
		.assert_some();
	after.intern(name).assert_ok();
	let update = plan(&image, &after).assert_ok();
	let changed = update.pages.iter().map(|(number, _)| *number).collect::<BTreeSet<_>>();
	assert!(changed.contains(&relocated));
	assert!(!changed.contains(&first_hash));
	assert!(!changed.contains(&old_second));
	assert_eq!(changed.len(), 4);
	assert_plan(image, &after, update);
}

/// Crossing the admitted hash load appends a replacement and leaves old hashes
/// intact.
#[test]
fn resize_appends_only_the_new_hash_range() {
	let count = HASH_BUCKETS_PER_PAGE * 3 / 4;
	let mut after = dictionary(count);
	let image = build_property_dictionary(&after, PageLsn::new(1)).assert_ok();
	let old = decode_root(&image.pages[0]).assert_ok();
	after.intern("resize").assert_ok();
	let update = plan(&image, &after).assert_ok();
	assert_eq!(update.page_ceiling, u64::try_from(image.pages.len() + 3).assert_ok());
	assert!(!update.pages.iter().any(|(number, _)| *number == old.hash_first_page));
	assert_eq!(update.pages.len(), 5);
	assert_plan(image, &after, update);
}

/// Long batched keys span new entry pages and collide across the probing tail.
#[test]
fn batch_spans_entry_pages_and_wraps_probing() {
	let mut after = dictionary(1);
	let image = build_property_dictionary(&after, PageLsn::new(1)).assert_ok();
	let prefix = "x".repeat(9000);
	let names = (0..100_000)
		.map(|id| format!("{prefix}-{id}"))
		.filter(|name| {
			property_hash(name.as_bytes()) as usize % HASH_BUCKETS_PER_PAGE
				== HASH_BUCKETS_PER_PAGE - 1
		})
		.take(2)
		.collect::<Vec<_>>();
	assert_eq!(names.len(), 2);
	for name in names {
		after.intern(name).assert_ok();
	}
	let update = plan(&image, &after).assert_ok();
	assert_eq!(update.page_ceiling, u64::try_from(image.pages.len() + 2).assert_ok());
	assert_eq!(update.pages.len(), 5);
	assert_plan(image, &after, update);
}

/// Empty dictionaries acquire their first chains; an exact no-op emits no
/// pages.
#[test]
fn empty_append_and_noop_have_exact_page_sets() {
	let before = PropertyDictionary::new();
	let image = build_property_dictionary(&before, PageLsn::new(1)).assert_ok();
	let noop = plan(&image, &before).assert_ok();
	assert!(noop.pages.is_empty());
	assert_eq!(noop.page_ceiling, 1);
	let after = dictionary(1);
	let update = plan(&image, &after).assert_ok();
	assert_eq!(update.pages.len(), 3);
	assert_eq!(update.page_ceiling, 3);
	assert_plan(image, &after, update);
}

/// Invalid source bytes, changed IDs and reader failure reject before staging.
#[test]
fn invalid_source_and_nonprefix_dictionary_are_rejected() {
	let before = dictionary(2);
	let mut image = build_property_dictionary(&before, PageLsn::new(1)).assert_ok();
	assert!(plan(&image, &dictionary(1)).is_err());
	let mut reordered = PropertyDictionary::new();
	for name in ["key-1", "key-0", "fresh"] {
		reordered.intern(name).assert_ok();
	}
	assert!(plan(&image, &reordered).is_err());
	let mut after = before;
	after.intern("fresh").assert_ok();
	image.pages[2].as_bytes_mut()[HASH_USED_OFFSET] ^= 1;
	assert!(plan(&image, &after).is_err());
	assert!(
		plan_property_dictionary_update(
			&mut |_number, _page| Err(invalid("injected dictionary read failure")),
			3,
			&after,
			PageLsn::ZERO,
		)
		.is_err()
	);
}
