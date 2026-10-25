//! Characterizes bounded copies at the runtime/codec boundary.
use compact_str::CompactString;
use fg_runtime::__internal::TextInternal as _;

use crate::{test_assertions::AssertResult as _, values::Text};
/// Bounds copied text without charging source spare or static storage.
#[test]
fn bounded_storage_copies_cover_static_and_spare_text() {
	use crate::values::{
		PropertyKeyId,
		PropertyMap,
		Value,
		storage::{EncodeContext, StorageValue, codec::CanonicalCodec},
	};

	let mut sources =
		vec![Text::from(CompactString::const_new("a static string longer than the inline buffer"))];
	for length in [0, 24, 25, 31, 32, 128] {
		let mut inner = CompactString::with_capacity(4096);
		inner.push_str(&"x".repeat(length));
		sources.push(Text::from(inner));
		sources.push(Text::from("x".repeat(length)));
	}
	for value in sources {
		let clone = value.clone();
		let copy = if value.estimated_clone_heap_bytes() == 0 {
			0
		} else {
			Text::estimated_copy_heap_bytes(value.len())
		};
		assert!(copy >= clone.estimated_heap_bytes());
		let key = PropertyKeyId::new(0);
		let control = StorageValue::Object(vec![(key, StorageValue::String(clone))]);
		let expected = CanonicalCodec::encode(control.as_ref()).assert_ok();
		let maximum = size_of::<(PropertyKeyId, StorageValue)>() + copy + expected.len();
		let mut properties = PropertyMap::new();
		let value = Value::String(value);
		assert_eq!(
			StorageValue::prime_context_bounded(&value, &mut EncodeContext::default(), copy)
				.assert_ok(),
			0
		);
		if copy != 0 {
			assert!(
				StorageValue::prime_context_bounded(
					&value,
					&mut EncodeContext::default(),
					copy - 1
				)
				.is_err()
			);
		}
		properties.insert(key, value);
		let mut context = EncodeContext::default();
		assert!(
			StorageValue::encode_properties_bounded(&properties, &mut context, maximum - 1)
				.is_err()
		);
		let (actual, retained) =
			StorageValue::encode_properties_bounded(&properties, &mut context, maximum).assert_ok();
		assert_eq!(actual, expected);
		assert_eq!(retained, 0);
	}
}
