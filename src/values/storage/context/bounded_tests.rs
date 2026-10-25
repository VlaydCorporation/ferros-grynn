use super::*;
use crate::{gql_status::Status, test_assertions::AssertResult as _};

/// Tiny ID ranges prove literal diagnostic labels without a million-entry
/// fixture.
#[test]
fn bounded_dictionary_exhaustion_labels_limits_and_preserves_state() {
	let mut dictionary = StringDictionary::default();
	assert_eq!(dictionary.intern("one".into(), 1, 2).assert_ok(), 1);
	assert_eq!(dictionary.intern("two".into(), 1, 2).assert_ok(), 2);
	let error = dictionary.intern_bounded("three", 1, 2, &mut EncodeBudget::new(0)).assert_err();
	assert!(
		matches!(error.status().status(), Status::StorageDecodeLimitExceeded {
			actual: 3,
			maximum: 2,
			..
		}),
		"{error:?}"
	);
	assert_eq!(dictionary.names.len(), 2);
	assert_eq!(dictionary.ids.len(), 2);
	assert!(!dictionary.ids.contains_key("three"));
}

/// Both growth shapes reject one byte short before touching either dictionary
/// direction.
#[test]
fn bounded_dictionary_admits_growth_and_no_growth_before_mutation() {
	for (capacity, grow) in [(1, true), (4, false)] {
		for length in [3, 25, 31] {
			let name = "x".repeat(length);
			let mut dictionary = StringDictionary {
				ids: BTreeMap::new(),
				names: Vec::with_capacity(capacity),
			};
			dictionary.intern("seed".into(), 1, 10).assert_ok();
			assert_eq!(dictionary.names.capacity(), capacity);
			let text = 2 * Text::estimated_copy_heap_bytes(length);
			let tree = tree_allocation_bytes(2, size_of::<(Text, u32)>()).assert_ok();
			let growth = if grow {
				size_of::<Text>()
			} else {
				0
			};
			let overlap = if grow {
				2 * size_of::<Text>()
			} else {
				0
			};
			let peak = text + tree + growth + overlap;
			assert!(
				dictionary.intern_bounded(&name, 1, 10, &mut EncodeBudget::new(peak - 1)).is_err()
			);
			assert_eq!(dictionary.names.len(), 1);
			assert_eq!(dictionary.names.capacity(), capacity);
			assert!(!dictionary.ids.contains_key(name.as_str()));
			let mut budget = EncodeBudget::new(peak);
			assert_eq!(dictionary.intern_bounded(&name, 1, 10, &mut budget).assert_ok(), 2);
			assert_eq!(budget.context_bytes(), text + tree + growth);
			let mut empty = EncodeBudget::new(0);
			assert_eq!(dictionary.intern_bounded(&name, 1, 10, &mut empty).assert_ok(), 2);
			assert_eq!(empty.context_bytes(), 0);
		}
	}
}

/// One admitted bulk table fits while repeated whole-tree reservations would
/// not.
#[test]
fn restored_zone_bulk_admission_is_linear_and_keeps_legacy_ids() {
	let mut names = vec![Text::from("UTC"), Text::from("Europe/Paris"), Text::from("Europe/Paris")];
	names.extend(
		(0..256).map(|index| Text::from(format!("test-zone-with-owned-heap-name/{index:04}"))),
	);
	let mut context = EncodeContext::default();
	let retained = context.restore_time_zone_names_bounded(&names, 128 * 1024).assert_ok();
	assert!(retained > 0 && retained <= 128 * 1024);
	let mut legacy = EncodeContext::default();
	for name in &names {
		legacy.register_time_zone_name(name.clone()).assert_ok();
	}
	assert_eq!(context.time_zones.names, legacy.time_zones.names);
	assert_eq!(context.time_zones.ids, legacy.time_zones.ids);
	assert_eq!(context.time_zones.ids.len(), 258);
	assert_eq!(context.time_zones.ids.get("UTC"), Some(&1));
	assert_eq!(context.time_zones.ids.get("Europe/Paris"), Some(&2));
	let mut rejected = EncodeContext::default();
	let error = rejected.restore_time_zone_names_bounded(&names, retained - 1).assert_err();
	assert!(matches!(
		error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, actual, maximum }
			if limit == "runtime storage encoding"
				&& *actual == retained as u64
				&& *maximum == (retained - 1) as u64
	));
	assert!(rejected.time_zones.ids.is_empty());
	assert!(rejected.time_zones.names.is_empty());
	assert_eq!(rejected.time_zones.names.capacity(), 0);
}

/// Empty prefixes need no budget; a populated target fails without mutation.
#[test]
fn restored_zone_bulk_zero_and_nonempty_target_keep_state() {
	let mut context = EncodeContext::default();
	assert_eq!(context.restore_time_zone_names_bounded(&[], 0).assert_ok(), 0);
	assert!(context.time_zones.ids.is_empty());
	assert_eq!(context.time_zones.names.capacity(), 0);
	let error = context.restore_time_zone_names_bounded(&["Europe/Paris".into()], 0).assert_err();
	assert!(matches!(
		error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, maximum: 0, actual }
			if limit == "runtime storage encoding" && *actual > 0
	));
	assert!(context.time_zones.ids.is_empty());
	assert_eq!(context.time_zones.names.capacity(), 0);
	assert_eq!(context.register_time_zone_name("Europe/Paris".into()).assert_ok(), 1);
	assert!(context.restore_time_zone_names_bounded(&["UTC".into()], usize::MAX).is_err());
	assert_eq!(context.time_zones.ids.len(), 1);
	assert_eq!(context.time_zones.names.len(), 1);
	assert_eq!(context.time_zones.ids.get("Europe/Paris"), Some(&1));
	assert!(!context.time_zones.ids.contains_key("UTC"));
}
