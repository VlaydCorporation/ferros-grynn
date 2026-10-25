/// Owned Rust SDK values and their type, comparison and conversion contracts.
pub mod runtime {
	pub(crate) use fg_runtime::__internal::{
		estimated_record_id_heap_bytes,
		same_number,
		same_object,
		same_value,
	};
	pub use fg_runtime::values::runtime::*;
}
pub(crate) use fg_value_codec::__internal as storage;
pub use runtime::*;
