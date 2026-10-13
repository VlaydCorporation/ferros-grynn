//! Candidate preparation and conflict/queue transitions for mutable families.

use std::{
	collections::{BTreeSet, VecDeque},
	sync::Arc,
};

use super::{DirtyTarget, LiveEvent, LiveFamily, LiveUpdate, Mutable, invalid_state};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphRead,
};

/// Keeps the remote base, local candidate and ordered conflict state together.
pub(super) struct MutableLiveState<G: LiveFamily<State = Mutable, Mutable = G>> {
	/// Pins the immutable remote base used for local dirty comparison.
	pub(super) base: G::Frozen,
	/// Retains the agreed graph revision independently of future publications.
	pub(super) graph: G,
	/// Stores the last completely accepted remote watermark.
	pub(super) remote_revision: u64,
	/// Retains canonical local targets awaiting write-back or resolution.
	pub(super) local_changes: BTreeSet<DirtyTarget>,
	/// Retains the unresolved whole incoming delta without acknowledgement.
	pub(super) pending: Option<LiveUpdate<G>>,
	/// Retains subsequent ordered deltas while application is paused.
	pub(super) queue: VecDeque<LiveUpdate<G>>,
	/// Pins the validated remote tail while pending/queued updates remain
	/// unapplied.
	validated_tail: Option<Arc<G::Frozen>>,
	/// Prevents ordinary application from bypassing unresolved local decisions.
	pub(super) paused: bool,
}

impl<G: LiveFamily<State = Mutable, Mutable = G>> Clone for MutableLiveState<G> {
	fn clone(&self) -> Self {
		Self {
			base: self.base.clone(),
			graph: G::clone_mutable(&self.graph),
			remote_revision: self.remote_revision,
			local_changes: self.local_changes.clone(),
			pending: self.pending.clone(),
			queue: self.queue.clone(),
			validated_tail: self.validated_tail.clone(),
			paused: self.paused,
		}
	}
}

impl<G: LiveFamily<State = Mutable, Mutable = G>> MutableLiveState<G> {
	/// Initializes the working copy and immutable remote base from one graph.
	pub(super) fn new(mut graph: G) -> Self {
		let base = G::freeze(G::clone_mutable(&graph));
		let remote_revision = base.revision();
		G::clear_dirty(&mut graph);
		Self {
			base,
			graph,
			remote_revision,
			local_changes: BTreeSet::new(),
			pending: None,
			queue: VecDeque::new(),
			validated_tail: None,
			paused: false,
		}
	}

	/// Resolving/draining is one publication: errors preserve pending and
	/// queue.
	pub(super) fn prepare<T>(
		&mut self,
		operation: impl FnOnce(&mut Self) -> FerrosGrynnResult<T>,
	) -> FerrosGrynnResult<T> {
		let mut candidate = self.clone();
		let result = operation(&mut candidate)?;
		*self = candidate;
		Ok(result)
	}

	/// Prepares the pending remote base without consuming the conflict.
	pub(super) fn pending_remote(&self) -> FerrosGrynnResult<(LiveUpdate<G>, G)> {
		let update =
			self.pending.clone().ok_or_else(|| invalid_state("there is no pending conflict"))?;
		let mut remote = G::thaw(self.base.clone())?;
		apply_candidate(&mut remote, &update)?;
		G::adopt_revision(&mut remote, update.remote_revision());
		Ok((update, remote))
	}

	/// Returns the newest accepted queue watermark for monotonic delivery
	/// validation.
	pub(super) fn accepted_revision(&self) -> u64 {
		self.queue
			.back()
			.map(LiveUpdate::remote_revision)
			.or_else(|| self.pending.as_ref().map(LiveUpdate::remote_revision))
			.unwrap_or(self.remote_revision)
	}

	/// Validates against the accepted remote tail, never the conflicted local
	/// copy.
	pub(super) fn enqueue(&mut self, update: LiveUpdate<G>) -> FerrosGrynnResult<()> {
		let tail = self
			.validated_tail
			.as_ref()
			.ok_or_else(|| invalid_state("a paused live graph has no validated remote tail"))?;
		let mut candidate = G::thaw(tail.as_ref().clone())?;
		apply_candidate(&mut candidate, &update)?;
		G::adopt_revision(&mut candidate, update.remote_revision());
		self.queue.try_reserve(1).map_err(|error| {
			invalid_state("live callback queue allocation failed").with_technical_source(error)
		})?;
		let tail = Arc::new(G::freeze(candidate));
		self.queue.push_back(update);
		self.validated_tail = Some(tail);
		Ok(())
	}
}

/// Prepares both graph copies or retains an overlapping delta as a conflict.
pub(super) fn apply_or_pause<G: LiveFamily<State = Mutable, Mutable = G>>(
	state: &mut MutableLiveState<G>,
	update: LiveUpdate<G>,
) -> FerrosGrynnResult<LiveEvent> {
	validate_update(&state.graph, &update, state.remote_revision)?;
	let mut base = G::thaw(state.base.clone())?;
	apply_candidate(&mut base, &update)?;
	G::adopt_revision(&mut base, update.remote_revision());
	let conflicts = conflicting_changes(&state.local_changes, update.changes());
	if !conflicts.is_empty() {
		let event = LiveEvent::Conflict {
			incoming_changes: update.changes().iter().cloned().collect(),
			local_changes: state.local_changes.iter().cloned().collect(),
			conflicts: conflicts.into_iter().collect(),
			remote_revision: update.remote_revision(),
		};
		if state
			.validated_tail
			.as_ref()
			.is_none_or(|tail| tail.revision() < update.remote_revision())
		{
			state.validated_tail = Some(Arc::new(G::freeze(base)));
		}
		state.paused = true;
		state.pending = Some(update);
		return Ok(event);
	}

	let mut current = G::clone_mutable(&state.graph);
	apply_candidate(&mut current, &update)?;
	state.base = G::freeze(base);
	state.graph = current;
	state.remote_revision = update.remote_revision();
	Ok(LiveEvent::Applied {
		remote_revision: update.remote_revision(),
	})
}

/// Drains complete queued deltas until idle or the next unresolved conflict.
pub(super) fn resume_queue<G: LiveFamily<State = Mutable, Mutable = G>>(
	state: &mut MutableLiveState<G>,
) -> FerrosGrynnResult<LiveEvent> {
	while let Some(update) = state.queue.front().cloned() {
		let event = apply_or_pause(state, update)?;
		state.queue.pop_front();
		if matches!(event, LiveEvent::Conflict { .. }) {
			return Ok(event);
		}
	}
	state.validated_tail = None;
	Ok(LiveEvent::Resumed {
		remote_revision: state.remote_revision,
	})
}

/// Runs the adapter callback and validates its identity and family.
pub(super) fn apply_candidate<G: LiveFamily<State = Mutable, Mutable = G>>(
	graph: &mut G,
	update: &LiveUpdate<G>,
) -> FerrosGrynnResult<()> {
	update.apply(graph)?;
	validate_identity(graph, update.origin())?;
	graph.validate()
}

/// Rejects foreign identities and nonadvancing remote revisions.
pub(super) fn validate_update<R: GraphRead, G: LiveFamily<State = Mutable, Mutable = G>>(
	graph: &R,
	update: &LiveUpdate<G>,
	current_revision: u64,
) -> FerrosGrynnResult<()> {
	validate_identity(graph, update.origin())?;
	if update.remote_revision() <= current_revision {
		return Err(invalid_state(format!(
			"remote revision {} does not advance revision {current_revision}",
			update.remote_revision(),
		)));
	}
	Ok(())
}

/// Requires the original materialization incarnation after every candidate
/// edit.
pub(super) fn validate_identity<R: GraphRead>(
	graph: &R,
	expected: crate::values::runtime::Uuid,
) -> FerrosGrynnResult<()> {
	if graph.identity() != expected {
		return Err(FerrosGrynnError::graph_identity_mismatch(
			expected.to_string(),
			graph.identity().to_string(),
		));
	}
	Ok(())
}

/// Finds target intersections, treating graph-wide edits as overlapping all
/// atoms.
fn conflicting_changes(
	local: &BTreeSet<DirtyTarget>,
	incoming: &BTreeSet<DirtyTarget>,
) -> BTreeSet<DirtyTarget> {
	if local.contains(&DirtyTarget::Graph) && !incoming.is_empty() {
		return local.clone();
	}
	if incoming.contains(&DirtyTarget::Graph) && !local.is_empty() {
		return incoming.clone();
	}
	local.intersection(incoming).cloned().collect()
}
