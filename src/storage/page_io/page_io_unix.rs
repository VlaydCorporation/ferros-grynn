//! Unix boundary for the Windows-only native page transport.

use crate::gql_status::FerrosGrynnError;

/// Reports that the current native page mode is specified only for Windows.
pub(super) fn native_async_unavailable() -> FerrosGrynnError {
	FerrosGrynnError::io_mode_unavailable(
		"NativeAsync",
		"Windows IoRing support requires a Windows build with feature native-async-io",
	)
}
