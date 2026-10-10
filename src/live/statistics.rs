//! Payload-free process diagnostics; never authoritative history bounds.

/// Monotonic process counters and current bounded consumer gauges.
///
/// Counters reset when a datastore opens. Concurrent observations need not
/// describe one transaction snapshot; use `LiveInfo` for durable bounds.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LiveStatistics {
	/// Query commit-preparation attempts, including retries and failures.
	pub query_evaluation_attempts: u64,
	/// Reactive binding commit-preparation attempts.
	pub binding_evaluation_attempts: u64,
	/// Successfully charged evaluation work; failed evaluator partial work is
	/// unknown.
	pub admitted_evaluation_work: u64,
	/// Largest observed conservative admission reserve, not process RSS.
	pub peak_admitted_scratch_bytes: u64,
	/// Current-stamp live batches accepted by a successful persistence barrier.
	pub published_batches: u64,
	/// Net result events in those batches.
	pub published_events: u64,
	/// Explicit current-stamp live GAP payloads.
	pub published_gaps: u64,
	/// Current-stamp evaluation fault notifications.
	pub published_faults: u64,
	/// Durable computed groups, including controls.
	pub durable_history_groups: u64,
	/// Durable logical envelope bytes, excluding physical framing/padding.
	pub durable_history_bytes: u64,
	/// Durable GAP/fault envelopes, including import suppression.
	pub durable_history_controls: u64,
	/// Whole deliveries first issued; repeated `next` is excluded.
	pub deliveries_issued: u64,
	/// Successful ACK watermark advances; registration/repeated ACK are
	/// excluded.
	pub acknowledgements: u64,
	/// Successfully charged history replay work, including initial metadata.
	pub admitted_replay_work: u64,
	/// History page requests, including cache/WAL hits and failed requests.
	pub history_page_reads: u64,
	/// Currently attached transient consumer handles.
	pub consumers: u64,
	/// Issued/pending data events; GAP/fault controls are excluded.
	pub queued_events: u64,
	/// Current admitted consumer retention, including metadata/control/head
	/// reserves.
	pub retained_consumer_bytes: u64,
	/// Catalog query incarnations, including closed definitions.
	pub registered_queries: u64,
	/// Catalog reactive binding incarnations, including removed definitions.
	pub registered_bindings: u64,
}
