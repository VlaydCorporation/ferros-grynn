//! Page-backed HNSW/DiskANN base images and their common exact delta.

mod build;
mod codec;
mod delta;
mod search;
mod stream;

pub(in crate::storage) use build::{
	build_vector_graph_with_distance,
	build_vector_index_with_distance,
};
pub(in crate::storage) use codec::{BuiltVectorIndex, VectorDocument};
pub(crate) use codec::{
	DecodedVectorIndex,
	PagedVectorHeader,
	PagedVectorNode,
	STREAM_PAYLOAD_BYTES,
	StreamLocation,
	VECTOR_NODE_HEADER_BYTES,
	VectorAlgorithm,
	VectorBuildSpec,
	VectorNode,
	VectorNodeLocation,
	VectorNodeRecordLayout,
	decode_validated_vector_stream_page,
	decode_vector_directory_entry,
	decode_vector_header,
	decode_vector_index,
	decode_vector_node_parts,
	decode_vector_node_record,
	decode_vector_node_record_layout,
	validate_vector_file_pages,
	vector_directory_address,
};
pub(crate) use delta::{
	DecodedVectorDelta,
	VectorDeltaRecord,
	VectorDeltaSpec,
	build_vector_delta,
	decode_vector_delta,
};
pub(crate) use search::{
	VectorSearchHit,
	search_paged_diskann,
	search_vector_index_with_delta_and_distance,
	search_vector_index_with_distance,
};
pub(in crate::storage) use stream::write_vector_index_stream;
