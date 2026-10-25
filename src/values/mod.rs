//! Owned runtime SDK values and optional columnar materialization.
/// Arrow property batches and optional DataFusion adapters.
#[cfg(feature = "columnar")]
pub use fg_columnar as columnar;
pub use fg_runtime::values::*;
