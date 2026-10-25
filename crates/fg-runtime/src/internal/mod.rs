//! Narrow workspace contracts excluded from the supported facade.

pub use crate::{
	graph::{
		analytics::all_shortest::TensorPathInternal,
		dag::DagMutableInternal,
		general::{GraphMutableInternal, remap_topology},
		id::{AtomIdInternal, SlotAllocator},
		materialized::{MaterializedGraphInternal, estimated_topology_heap_bytes},
		metagraph::validate_nested_references,
		path::{PathInternal, PathTraceArena, PathTraceId},
		read::{
			AtomIdsaInternal,
			EdgeViewaInternal,
			FallibleAtomIdsInnerInternal,
			GraphTopologyConstraintsInternal,
			MaterializationOriginInternal,
			NodeViewaInternal,
			nested_graph_reference,
		},
		tensor::{interop::TensorAtomInternal, matrix::CsrMatrixTInternal},
		topology::{EdgeEndpointInternal, EdgeTopologyInternal},
		traversal::{
			DirectedLandmarks,
			DistanceInterval,
			LandmarkBuildLimits,
			LandmarkDistanceRows,
			ProjectedCondensation,
			ProjectedRelation,
			RelationCsr,
			bellman_ford_with_check,
			bfs_with_check,
			condensation_with_check,
			dijkstra_to_with_control,
			find_cuts_with_check,
			has_cycle_directed_with_check,
			has_cycle_undirected_with_check,
			index::TraversalIndexInternal,
			minimum_spanning_forest_with_check,
			shortest_paths::ShortestPathsInternal,
			strongly_connected_components_with_check,
			topological_sort_with_check,
			visit_endpoint_steps,
			visit_incidence_targets,
			weakly_connected_components_with_check,
		},
		validate::validate_topology_constraints,
	},
	math::hyper_log_log::HyperLogLog,
	values::runtime::{
		closure::{ClosureInternal, RuntimeIdInternal},
		error_value::ErrorValueInternal,
		geometry::kernels::GeometryInternal,
		property_dictionary::PropertyDictionaryInternal,
		property_map::PropertyMapInternal,
		query_cursor::QueryCursorInternal,
		range::RangeInternal,
		record_id::RecordIdInternal,
		regex::RegexInternal,
		representation::{same_number, same_object, same_value},
		temporal::DurationInternal,
		text::TextInternal,
		types::TypeInternal,
		value::{ValueInternal, estimated_record_id_heap_bytes},
	},
};
/// Existing copy-on-write container, excluding inactive collections.
pub mod cow_vec {
	pub use crate::collections::cow_vec::{CowVec, Iter};
}
/// Checked tensor capture and assembly, independent of persistence.
pub mod tensor_build {
	pub use crate::graph::tensor::build::{
		InputConnector,
		InputEndpoint,
		PlaneBuilder,
		TensorBuilder,
	};
}
/// Checked tensor resource accounting shared with engine consumers.
pub mod tensor_control {
	pub use crate::graph::tensor::control::{Control, invalid, overflow, resource};
}

pub use crate::graph::{
	general::GraphStateInternal,
	live::LiveGraphMutableInternal,
	tensor::interop::{EndpointTensorInternal, TensorConnectorInternal},
};

mod dispatch;
pub use dispatch::EnumChoice;

/// Tests the node/edge distinction exhaustively in the atom-kind owner.
pub const fn is_node(kind: &crate::graph::AtomKind) -> bool {
	match kind {
		crate::graph::AtomKind::Node => true,
		crate::graph::AtomKind::Edge => false,
	}
}

mod operations;
pub use operations::{
	endpoint_outermost,
	endpoint_parts,
	monotone_path_prefix,
	tensor_atom_into_value,
	vector_component_as_f64,
	vector_element_width,
	visit_adjacency_endpoints,
};
