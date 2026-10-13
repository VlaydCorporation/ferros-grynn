//! Owned descriptors and whole-commit delivery for reactive database reads.
//!
//! Query builders contain logical expressions, not GQL text or executable IR.
//! Datastore owns registration and evaluation; storage owns durable encoding.
//! Ordinary parameters are captured by value, while reactive bindings use
//! stable identities. Events and materialized LiveGraph updates are separate
//! surfaces.

mod cursor;
mod delivery;
pub(crate) mod dependencies;
mod graph_conflict;
mod graph_image;
mod graph_result;
mod graph_subscription;
mod identity;
mod info;
mod options;
mod order;
mod presentation;
mod query;
mod result_identity;
mod select;
mod statistics;
mod subscription;
pub use cursor::LiveCursor;
pub use delivery::{
	LiveBatch,
	LiveDelivery,
	LiveEvent,
	LiveGap,
	LiveGapReason,
	LiveGraphBatch,
	LiveNext,
	LivePayload,
};
pub use graph_conflict::LiveGraphConflict;
pub use graph_image::{
	LiveGraphAtomId,
	LiveGraphDelta,
	LiveGraphEdge,
	LiveGraphEndpoint,
	LiveGraphImage,
	LiveGraphNode,
	LiveGraphScope,
	LiveGraphScopeId,
	LiveGraphSource,
	LiveGraphTarget,
};
pub use graph_result::LiveGraphResult;
pub use graph_subscription::{LiveGraphNext, LiveGraphSubscription};
pub use identity::{LiveBindingId, LiveId, LiveVersion};
pub use info::{
	LiveBindingInfo,
	LiveBindingRevision,
	LiveCaptureEpoch,
	LiveCapturePolicy,
	LiveHistoryInfo,
	LiveInfo,
	LiveState,
};
pub use options::{LiveBindingOptions, LiveOptions, LiveOverflowPolicy, LiveSince};
pub use order::{LiveNullsOrder, LiveOrder, LiveOrderDirection, LiveOrderKey};
pub use presentation::{LiveEventOutput, LiveFieldChange, LivePatchKind, LivePatchOperation};
pub(crate) use presentation::{PreparedOutput, prepare_batch_presentation};
pub(crate) use query::admit_cast_type_name;
pub use query::{
	LiveBinaryOperator,
	LiveExpr,
	LiveField,
	LiveJoin,
	LiveJoinKind,
	LiveOutputMode,
	LivePath,
	LiveProjection,
	LiveQuery,
	LiveSource,
};
pub(crate) use result_identity::valid_composite_parts;
pub use result_identity::{LiveIdentity, LiveIdentityPart};
pub use statistics::LiveStatistics;
pub use subscription::LiveSubscription;
