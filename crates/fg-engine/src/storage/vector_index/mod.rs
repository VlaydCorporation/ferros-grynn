//! Page-backed HNSW/DiskANN base images and their common exact delta.

mod build;
mod codec;
mod delta;
mod hnsw_memory;
mod hnsw_search;
mod search;
mod stream;

pub(in crate::storage) use build::{
	build_vector_graph_with_distance,
	build_vector_index_with_distance,
};
pub(in crate::storage) use codec::VectorDocument;
#[cfg(test)]
pub use codec::VectorNode;
pub use codec::{
	DecodedHnswIndex,
	DecodedVectorIndex,
	HnswGraphNode,
	PagedVectorHeader,
	PagedVectorNode,
	STREAM_PAYLOAD_BYTES,
	StreamLocation,
	VECTOR_NODE_HEADER_BYTES,
	VectorBuildSpec,
	VectorNodeLocation,
	VectorNodeRecordLayout,
	assemble_hnsw_index,
	decode_validated_vector_stream_page,
	decode_vector_directory_entry,
	decode_vector_header,
	decode_vector_index,
	decode_vector_node_parts,
	decode_vector_node_record_layout,
	validate_vector_file_pages,
	vector_directory_address,
};
pub use delta::{
	DecodedVectorDelta,
	VectorDeltaRecord,
	VectorDeltaSpec,
	build_vector_delta,
	decode_vector_delta,
};
pub use hnsw_memory::{hnsw_vector_cache_get, hnsw_vector_cache_insert, purge_hnsw_vector_cache};
pub use hnsw_search::search_cached_hnsw_with_distance;
#[cfg(test)]
pub use search::search_vector_index_with_distance;
pub use search::{VectorSearchHit, search_paged_diskann};
pub(in crate::storage) use stream::write_vector_index_stream;
