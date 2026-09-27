//! Concrete physical-plan contracts shared by selection and EXPLAIN.

use std::hash::{Hash, Hasher};

use super::{Cardinality, PLAN_ENCODING_VERSION, PlanCost};
use crate::{datastore::IndexId, syn::SelectJoinKind, values::RecordId};

/// One observable fallback or planning-risk reason.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum PlanWarning {
	/// No histogram exists for a demanded class/field/scope.
	StatisticsMissing(Box<str>),
	/// A histogram exceeded the accepted dirty-row threshold.
	StatisticsStale(Box<str>),
	/// An optional index could not be used and scan remained correct.
	IndexUnavailable(Box<str>),
	/// Expected memory exceeds the in-memory barrier budget.
	ResourceRisk(Box<str>),
}

/// Physical source selected for one logical relation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SourceOperator {
	/// Stable primary lookup for one public record identity.
	PointGet(RecordId),
	/// Database-global class posting scan.
	ClassScan {
		/// Class name captured by typed analysis.
		class: Box<str>,
	},
	/// Graph-effective class membership scan.
	GraphClassScan {
		/// Class name captured by typed analysis.
		class: Box<str>,
		/// Exact graph identity, or `None` for a runtime graph expression.
		graph: Option<crate::graph::GraphId>,
	},
	/// Explicit scalar B+tree access.
	ScalarIndexScan {
		/// Stable logical index identity.
		index: IndexId,
		/// Catalog name used only for diagnostics and EXPLAIN.
		name: Box<str>,
		/// Access shape proved compatible with the index prefix.
		access: IndexAccessKind,
		/// Direction of the ordered index walk.
		direction: IndexDirection,
	},
	/// Exact count index lookup.
	CountLookup {
		/// Stable logical index identity.
		index: IndexId,
		/// Catalog name used only for diagnostics and EXPLAIN.
		name: Box<str>,
	},
	/// Ordered graph-catalog scan.
	GraphCatalogScan,
	/// Materialized relation produced by a nested SELECT.
	Derived,
}

/// Shape of a scalar-index access selected before runtime values are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum IndexAccessKind {
	/// One or more exact probes cover the complete composite key.
	Exact,
	/// Equality prefix and optional range constrain the ordered index.
	Range,
	/// The forced index is scanned in key order without key bounds.
	Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum IndexDirection {
	Forward,
	Reverse,
}

/// Canonical rule whose decision affects physical execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AppliedRule {
	ConstantFolding,
	ConjunctionDecomposition,
	PredicatePushdown,
	ProjectionPruning,
	LateMaterialization,
	LimitPushdown,
	CountRewrite,
	SortElimination,
}

/// One source node in stable preorder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PhysicalSource {
	/// Stable preorder node identifier.
	pub(crate) node_id: u32,
	/// Source alias from typed analysis.
	pub(crate) alias: Box<str>,
	/// First column occupied by this source in the analyzed row schema.
	pub(crate) column_offset: usize,
	/// Number of columns owned by this source.
	pub(crate) column_width: usize,
	/// Selected source operator.
	pub(crate) operator: SourceOperator,
	/// Estimated output cardinality.
	pub(crate) cardinality: Cardinality,
	/// Operator-local cost.
	pub(crate) cost: PlanCost,
}

/// Concrete join algorithm selected before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum JoinOperator {
	/// In-memory equality hash join.
	Hash,
	/// Partitioned equality hash join using query runs.
	PartitionedHash,
	/// Block nested-loop for arbitrary or cross predicates.
	BlockNestedLoop,
	/// Repeated probes of a Ready right-side scalar index.
	IndexNestedLoop,
}

/// Side materialized into the hash table for one equality join.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum JoinBuildSide {
	Left,
	Right,
}

/// Exact right-index probe driven by columns from the current left row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct IndexJoinProbe {
	pub(crate) index: IndexId,
	pub(crate) left_columns: Vec<usize>,
}

/// One join node in stable preorder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PhysicalJoin {
	/// Stable preorder node identifier.
	pub(crate) node_id: u32,
	/// Source indexes already present on the left side.
	pub(crate) left_sources: Vec<usize>,
	/// Source index introduced by this join.
	pub(crate) right_source: usize,
	/// Logical predicates first becoming evaluable at this join.
	pub(crate) predicates: Vec<usize>,
	/// Semantic join kind. LEFT and CROSS are never reordered.
	pub(crate) kind: SelectJoinKind,
	/// Equality columns in global analyzed-row coordinates.
	pub(crate) key_columns: Vec<(usize, usize)>,
	/// Selected concrete algorithm.
	pub(crate) operator: JoinOperator,
	/// Side selected for an in-memory or partition-local hash table.
	pub(crate) build_side: JoinBuildSide,
	/// Exact probe used instead of materializing the right source.
	pub(crate) index_probe: Option<IndexJoinProbe>,
	/// Estimated output cardinality.
	pub(crate) cardinality: Cardinality,
	/// Operator-local cost.
	pub(crate) cost: PlanCost,
}

/// Physical SELECT plan consumed by the relational executor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PhysicalSelectPlan {
	/// Physical source nodes in logical source-index order.
	pub(crate) sources: Vec<PhysicalSource>,
	/// Source indexes in physical left-deep execution order.
	pub(crate) execution_order: Vec<usize>,
	/// Physical join nodes in execution order.
	pub(crate) joins: Vec<PhysicalJoin>,
	/// Canonical rules that produced an executable decision.
	pub(crate) rules: Vec<AppliedRule>,
	/// Whether the top-level filter is supplied to the single-source access
	/// path.
	pub(crate) pushed_filter: bool,
	/// Whether a direct pull may stop after START plus LIMIT rows.
	pub(crate) pushed_limit: bool,
	/// Whether a separate result sort remains necessary.
	pub(crate) requires_sort: bool,
	/// Estimated result cardinality after joins and the final filter.
	pub(crate) cardinality: Cardinality,
	/// Total fixed-point plan cost.
	pub(crate) cost: PlanCost,
	/// Stable warnings emitted by planning.
	pub(crate) warnings: Vec<PlanWarning>,
}

/// Concrete operator selected for one MATCH plan node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum MatchOperator {
	/// Binds the first endpoint of one pattern.
	MatchSeed,
	/// Joins another required pattern with the current binding row.
	MatchJoin,
	/// Expands one StandardPath connector.
	FixedExpand {
		direction: crate::syn::MatchDirection,
	},
	/// Expands one AtomWalk connector.
	AtomExpand {
		direction: crate::syn::MatchDirection,
	},
	/// Validates and opens a qualified endpoint in a nested graph.
	ResolveNestedEndpoint,
	/// Enumerates bounded paths with the selected uniqueness mode.
	PathEnumerate {
		direction: crate::syn::MatchDirection,
		mode: crate::syn::MatchPathMode,
		min: u32,
		max: u32,
	},
	/// Finds one canonical unweighted shortest path per target.
	BreadthFirstShortest {
		direction: crate::syn::MatchDirection,
		mode: crate::syn::MatchPathMode,
		min: u32,
		max: u32,
	},
	/// Finds one canonical weighted shortest path per target.
	DijkstraShortest {
		direction: crate::syn::MatchDirection,
		mode: crate::syn::MatchPathMode,
		min: u32,
		max: u32,
	},
	/// Assigns endpoint positions of one hyperedge.
	HyperContains,
	/// Expands a connector constrained to hyperedge topology.
	HyperExpand {
		direction: crate::syn::MatchDirection,
	},
	/// Joins an atom with its direct containing owner.
	WithinJoin,
	/// Walks a bounded containment chain.
	TransitiveWithin {
		min: u32,
		max: u32,
	},
	/// Emits the unchanged input row when the whole optional clause is empty.
	OptionalFallback,
	/// Deduplicates complete occurrence bindings.
	BindingDistinct,
	/// Projects MATCH result expressions.
	MatchProject,
	/// Deduplicates projected values.
	ProjectionDistinct,
	/// Orders projected values through the shared query sorter.
	MatchOrder,
}

/// One stable-preorder MATCH plan node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PhysicalMatchNode {
	/// Stable node identifier used by EXPLAIN.
	pub(crate) node_id: u32,
	/// Logical pattern index, absent for clause-wide barriers.
	pub(crate) pattern_index: Option<usize>,
	/// Linear operation index, absent for pattern seeds and relation nodes.
	pub(crate) operation_index: Option<usize>,
	/// Selected physical operator.
	pub(crate) operator: MatchOperator,
	/// Operator output estimate.
	pub(crate) cardinality: Cardinality,
	/// Operator-local fixed-point cost.
	pub(crate) cost: PlanCost,
}

/// Current MATCH operator sequence selected by the shared cost model.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PhysicalMatchPlan {
	/// Pattern indexes in chosen execution order.
	pub(crate) pattern_order: Vec<usize>,
	/// Physical operators in stable execution preorder.
	pub(crate) nodes: Vec<PhysicalMatchNode>,
	/// Expected rows after the selected seed sequence.
	pub(crate) cardinality: Cardinality,
	/// Total estimated seed/expand cost.
	pub(crate) cost: PlanCost,
	/// Stable warnings emitted by planning.
	pub(crate) warnings: Vec<PlanWarning>,
}

impl PhysicalMatchPlan {
	/// Returns the selected expansion operator for one linear operation.
	pub(crate) fn operation(
		&self,
		pattern_index: usize,
		operation_index: usize,
	) -> Option<&MatchOperator> {
		self.nodes
			.iter()
			.find(|node| {
				node.pattern_index == Some(pattern_index)
					&& node.operation_index == Some(operation_index)
					&& !matches!(node.operator, MatchOperator::ResolveNestedEndpoint)
			})
			.map(|node| &node.operator)
	}

	/// Returns whether a linear operation must resolve graph-scoped endpoints.
	pub(crate) fn operation_is_scoped(&self, pattern_index: usize, operation_index: usize) -> bool {
		self.nodes.iter().any(|node| {
			node.pattern_index == Some(pattern_index)
				&& node.operation_index == Some(operation_index)
				&& matches!(node.operator, MatchOperator::ResolveNestedEndpoint)
		})
	}

	/// Returns whether any selected operator can cross a graph boundary.
	pub(crate) fn uses_scoped_traversal(&self) -> bool {
		self.nodes.iter().any(|node| matches!(node.operator, MatchOperator::ResolveNestedEndpoint))
	}
}

/// Write-side phase shown by EXPLAIN.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WriteOperator {
	/// Resolves statement targets against the private snapshot.
	TargetSelection,
	/// Applies one logical mutation.
	Mutation,
	/// Checks row and graph constraints.
	ConstraintValidation,
	/// Mutates catalog metadata.
	CatalogMutation,
	/// Builds or rebuilds one index image.
	IndexBuild,
	/// Publishes an autocommit root; explicit transactions defer this phase.
	Publication,
}

/// Physical phases of one DDL or DML statement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PhysicalWritePlan {
	/// Ordered physical phases.
	pub(crate) operators: Vec<WriteOperator>,
	/// Estimated target cardinality.
	pub(crate) cardinality: Cardinality,
	/// Total estimated work.
	pub(crate) cost: PlanCost,
}

/// Complete internal physical plan used by EXPLAIN and replay hashing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum PhysicalPlan {
	/// Relational SELECT.
	Select(PhysicalSelectPlan),
	/// Current executable MATCH profile.
	Match(PhysicalMatchPlan),
	/// DDL or DML phase sequence.
	Write(PhysicalWritePlan),
}

impl PhysicalPlan {
	/// Returns the versioned canonical physical-plan hash.
	pub(crate) fn hash(&self) -> u64 {
		let mut writer = super::CanonicalHasher::versioned(PLAN_ENCODING_VERSION, b"physical-plan");
		match self {
			Self::Select(plan) => {
				0_u8.hash(&mut writer);
				for source in &plan.sources {
					source.node_id.hash(&mut writer);
					source.alias.hash(&mut writer);
					source.column_offset.hash(&mut writer);
					source.column_width.hash(&mut writer);
					source.operator.hash(&mut writer);
				}
				plan.execution_order.hash(&mut writer);
				for join in &plan.joins {
					join.node_id.hash(&mut writer);
					join.left_sources.hash(&mut writer);
					join.right_source.hash(&mut writer);
					join.predicates.hash(&mut writer);
					join.kind.hash(&mut writer);
					join.key_columns.hash(&mut writer);
					join.operator.hash(&mut writer);
					join.build_side.hash(&mut writer);
					join.index_probe.hash(&mut writer);
				}
				plan.rules.hash(&mut writer);
				plan.pushed_filter.hash(&mut writer);
				plan.pushed_limit.hash(&mut writer);
				plan.requires_sort.hash(&mut writer);
			}
			Self::Match(plan) => {
				1_u8.hash(&mut writer);
				plan.pattern_order.hash(&mut writer);
				plan.nodes.hash(&mut writer);
				plan.cardinality.hash(&mut writer);
				plan.cost.hash(&mut writer);
				plan.warnings.hash(&mut writer);
			}
			Self::Write(plan) => {
				2_u8.hash(&mut writer);
				plan.operators.hash(&mut writer);
			}
		}
		writer.finish()
	}
}
