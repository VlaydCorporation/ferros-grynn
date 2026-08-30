use super::super::consts::MAX_GRAPH_NAME_BYTES;
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

pub(crate) fn validate_graph_name(name: &str) -> FerrosGrynnResult<()> {
	if name.is_empty() {
		return Err(FerrosGrynnError::invalid_argument("graph name must not be empty"));
	}
	if name.contains('\0') {
		return Err(FerrosGrynnError::invalid_argument("graph name must not contain NUL"));
	}
	if name.len() > MAX_GRAPH_NAME_BYTES {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"graph name exceeds {MAX_GRAPH_NAME_BYTES} UTF-8 bytes"
		)));
	}
	Ok(())
}
