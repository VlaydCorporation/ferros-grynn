//! Revision-pinned live materializations for the engine's concrete graph
//! families.

use std::{
	collections::BTreeSet,
	fmt::{self, Debug, Formatter},
	marker::PhantomData,
	ops::Deref,
	sync::Arc,
};

use parking_lot::{Mutex, RwLock};

use super::{DirtyTarget, Frozen, Graph, GraphWriteBack, Mutable};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::Uuid,
};

mod family;
#[cfg(test)]
mod family_tests;
mod state;
pub use family::LiveFamily;
use state::{
	MutableLiveState,
	apply_candidate,
	apply_or_pause,
	resume_queue,
	validate_identity,
	validate_update,
};

/// A deterministic owned callback shared by independent graph candidates.
type ApplyUpdate<G> = dyn Fn(&mut G) -> FerrosGrynnResult<()> + Send + Sync + 'static;

/// An adapter-defined complete delta for one materialized revision.
pub struct LiveUpdate<G = Graph<Mutable>>
where
	G: LiveFamily<State = Mutable, Mutable = G>,
{
	/// Identifies the single result incarnation allowed to accept this delta.
	origin: Uuid,
	/// Stores the last completely accepted remote watermark.
	remote_revision: u64,
	/// Owns the canonical logical targets changed by this complete delta.
	changes: BTreeSet<DirtyTarget>,
	/// Retains the deterministic adapter callback shared by candidate copies.
	apply: Arc<ApplyUpdate<G>>,
}

impl LiveUpdate {
	/// Creates a general-graph delta, preserving existing closure inference.
	///
	/// The callback must be deterministic and free of external side effects.
	pub fn new<Targets, ApplyFn>(
		origin: Uuid,
		remote_revision: u64,
		changes: Targets,
		apply: ApplyFn,
	) -> Self
	where
		Targets: IntoIterator<Item = DirtyTarget>,
		ApplyFn: Fn(&mut Graph<Mutable>) -> FerrosGrynnResult<()> + Send + Sync + 'static,
	{
		Self::for_family(origin, remote_revision, changes, apply)
	}
}

impl<G: LiveFamily<State = Mutable, Mutable = G>> LiveUpdate<G> {
	/// Creates a delta for an explicit family.
	///
	/// A mutable live graph can apply the callback independently to its remote
	/// base and local working copy, so it must be deterministic and have no
	/// external side effects.
	pub fn for_family<Targets, ApplyFn>(
		origin: Uuid,
		remote_revision: u64,
		changes: Targets,
		apply: ApplyFn,
	) -> Self
	where
		Targets: IntoIterator<Item = DirtyTarget>,
		ApplyFn: Fn(&mut G) -> FerrosGrynnResult<()> + Send + Sync + 'static,
	{
		Self {
			origin,
			remote_revision,
			changes: changes.into_iter().collect(),
			apply: Arc::new(apply),
		}
	}

	pub const fn origin(&self) -> Uuid {
		self.origin
	}

	pub const fn remote_revision(&self) -> u64 {
		self.remote_revision
	}

	pub const fn changes(&self) -> &BTreeSet<DirtyTarget> {
		&self.changes
	}

	/// Executes the retained deterministic callback against one private
	/// candidate.
	fn apply(&self, graph: &mut G) -> FerrosGrynnResult<()> {
		(self.apply)(graph)
	}
}

impl<G: LiveFamily<State = Mutable, Mutable = G>> Clone for LiveUpdate<G> {
	fn clone(&self) -> Self {
		Self {
			origin: self.origin,
			remote_revision: self.remote_revision,
			changes: self.changes.clone(),
			apply: Arc::clone(&self.apply),
		}
	}
}

impl<G: LiveFamily<State = Mutable, Mutable = G>> Debug for LiveUpdate<G> {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("LiveUpdate")
			.field("origin", &self.origin)
			.field("remote_revision", &self.remote_revision)
			.field("changes", &self.changes)
			.finish_non_exhaustive()
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveEvent {
	Applied {
		/// Stores the last completely accepted remote watermark.
		remote_revision: u64,
	},
	Queued {
		/// Stores the last completely accepted remote watermark.
		remote_revision: u64,
	},
	Conflict {
		incoming_changes: Vec<DirtyTarget>,
		local_changes: Vec<DirtyTarget>,
		conflicts: Vec<DirtyTarget>,
		/// Stores the last completely accepted remote watermark.
		remote_revision: u64,
	},
	Resumed {
		/// Stores the last completely accepted remote watermark.
		remote_revision: u64,
	},
}

/// Owns the concrete family layout through its matching state lock.
enum LiveInner<G: LiveFamily<State = Mutable, Mutable = G>> {
	/// Pins immutable revisions while new candidates publish independently.
	Frozen(RwLock<Arc<G::Frozen>>),
	/// Protects local edits and whole incoming deltas under one state mutex.
	Mutable(Mutex<MutableLiveState<G>>),
}

/// A live materialization retaining the selected family's concrete layout.
///
/// The state and the family's concrete type must agree:
///
/// ```compile_fail
/// use ferros_grynn::graph::{Dag, Frozen, LiveGraph, Mutable};
/// fn invalid_state(_live: LiveGraph<Mutable, Dag<Frozen>>) {}
/// ```
pub struct LiveGraph<State = Frozen, G = Graph<State>>
where
	G: LiveFamily<State = State>,
{
	/// Owns the family layout and its publication or mutable-state lock.
	inner: LiveInner<G::Mutable>,
	/// Ties the public mutability state to its sealed concrete family.
	state: PhantomData<State>,
}

/// An immutable guard that keeps reading its original revision after
/// publication.
pub struct LiveReadGuard<G = Graph<Frozen>>
where
	G: LiveFamily<State = Frozen, Frozen = G>,
{
	/// Retains the agreed graph revision independently of future publications.
	graph: Arc<G>,
}

impl<G: LiveFamily<State = Frozen, Frozen = G>> LiveReadGuard<G> {
	pub fn revision(&self) -> u64 {
		self.graph.revision()
	}
}

impl<G: LiveFamily<State = Frozen, Frozen = G>> Deref for LiveReadGuard<G> {
	type Target = G;

	fn deref(&self) -> &Self::Target {
		&self.graph
	}
}

impl<G: LiveFamily<State = Frozen, Frozen = G> + Clone> LiveGraph<Frozen, G> {
	pub fn new(graph: G) -> Self {
		Self {
			inner: LiveInner::Frozen(RwLock::new(Arc::new(graph))),
			state: PhantomData,
		}
	}

	/// Returns a guard pinned to the current immutable revision.
	///
	/// # Errors
	/// Returns a state error when called through an incompatible internal
	/// state.
	pub fn read(&self) -> FerrosGrynnResult<LiveReadGuard<G>> {
		let LiveInner::Frozen(current) = &self.inner else {
			return Err(invalid_state("a frozen read was requested from a mutable live graph"));
		};
		Ok(LiveReadGuard {
			graph: Arc::clone(&current.read()),
		})
	}

	/// Pins the current immutable revision.
	///
	/// # Errors
	/// Returns an incompatible internal-state error.
	pub fn snapshot(&self) -> FerrosGrynnResult<Arc<G>> {
		self.read().map(|guard| guard.graph)
	}

	/// Publishes a complete checked graph revision.
	///
	/// # Errors
	/// Returns identity, nonadvancing revision or family validation errors.
	pub fn publish(&self, graph: G) -> FerrosGrynnResult<()> {
		self.publish_candidate(graph, None)
	}

	/// Publishes a validated candidate only if the prepared base is still
	/// current.
	fn publish_candidate(&self, graph: G, expected_revision: Option<u64>) -> FerrosGrynnResult<()> {
		graph.validate()?;
		let LiveInner::Frozen(current) = &self.inner else {
			return Err(invalid_state(
				"a frozen publication was requested from a mutable live graph",
			));
		};
		let mut current = current.write();
		validate_identity(&graph, current.identity())?;
		if expected_revision.is_some_and(|expected| expected != current.revision()) {
			return Err(invalid_state("the live base changed during candidate preparation"));
		}
		if graph.revision() <= current.revision() {
			return Err(invalid_state(format!(
				"revision {} does not advance current revision {}",
				graph.revision(),
				current.revision()
			)));
		}
		*current = Arc::new(graph);
		Ok(())
	}

	/// Prepares the complete adapter delta before immutable publication.
	///
	/// # Errors
	/// Returns callback, identity, stale base or family validation errors.
	pub fn apply_update(&self, update: &LiveUpdate<G::Mutable>) -> FerrosGrynnResult<()> {
		let current = self.snapshot()?;
		validate_update(current.as_ref(), update, current.revision())?;
		let mut candidate = G::thaw(current.as_ref().clone())?;
		apply_candidate(&mut candidate, update)?;
		G::adopt_revision(&mut candidate, update.remote_revision());
		self.publish_candidate(G::freeze(candidate), Some(current.revision()))
	}

	/// Copies the accepted frozen revision into a mutable COW materialization.
	///
	/// # Errors
	/// Returns the concrete family conversion or internal-state error.
	pub fn into_mutable(&self) -> FerrosGrynnResult<LiveGraph<Mutable, G::Mutable>> {
		let graph = G::thaw(self.snapshot()?.as_ref().clone())?;
		Ok(LiveGraph::<Mutable, G::Mutable>::new_mutable(graph))
	}
}

impl<G: LiveFamily<State = Mutable, Mutable = G>> LiveGraph<Mutable, G> {
	pub fn new_mutable(graph: G) -> Self {
		Self {
			inner: LiveInner::Mutable(Mutex::new(MutableLiveState::new(graph))),
			state: PhantomData,
		}
	}

	/// Prepares one local mutation and validates its complete candidate.
	///
	/// # Errors
	/// Returns paused-state, callback, identity or family errors without
	/// changing local state.
	pub fn mutate_local<T, Targets, MutFn>(
		&self,
		changes: Targets,
		mutation: MutFn,
	) -> FerrosGrynnResult<T>
	where
		Targets: IntoIterator<Item = DirtyTarget>,
		MutFn: FnOnce(&mut G) -> FerrosGrynnResult<T>,
	{
		let mut state = self.mutable_state()?;
		if state.paused {
			return Err(invalid_state(
				"local mutation is paused until the live conflict is resolved",
			));
		}
		let mut candidate = G::clone_mutable(&state.graph);
		let result = mutation(&mut candidate)?;
		validate_identity(&candidate, state.graph.identity())?;
		candidate.validate()?;
		state.graph = candidate;
		state.local_changes.extend(changes);
		Ok(result)
	}

	/// Reads the current local graph under its mutable-state lock.
	///
	/// # Errors
	/// Returns an incompatible internal-state error.
	pub fn with_graph<T, ReadFn: FnOnce(&G) -> T>(&self, read: ReadFn) -> FerrosGrynnResult<T> {
		Ok(read(&self.mutable_state()?.graph))
	}

	/// Pins an immutable copy of the current local decisions.
	///
	/// # Errors
	/// Returns an incompatible internal-state error.
	pub fn static_snapshot(&self) -> FerrosGrynnResult<G::Frozen> {
		Ok(G::freeze(G::clone_mutable(&self.mutable_state()?.graph)))
	}

	/// Admission and cloning use one locked local revision.
	pub(crate) fn static_snapshot_admitted(
		&self,
		admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<G::Frozen> {
		let state = self.mutable_state()?;
		state.graph.admit_snapshot(admit)?;
		Ok(G::freeze(G::clone_mutable(&state.graph)))
	}

	/// Accepts one ordered remote delta or retains it as a conflict.
	///
	/// # Errors
	/// Returns identity, revision, callback or family errors without consuming
	/// the candidate.
	pub fn ingest(&self, update: LiveUpdate<G>) -> FerrosGrynnResult<LiveEvent> {
		let mut state = self.mutable_state()?;
		validate_update(&state.graph, &update, state.accepted_revision())?;
		if state.paused {
			let remote_revision = update.remote_revision();
			state.enqueue(update)?;
			return Ok(LiveEvent::Queued {
				remote_revision,
			});
		}
		apply_or_pause(&mut state, update)
	}

	/// Accepts the pending remote state and clears local decisions.
	///
	/// # Errors
	/// Returns missing-conflict, callback or validation errors; failed
	/// preparation preserves state.
	pub fn accept_remote(&self) -> FerrosGrynnResult<LiveEvent> {
		self.mutable_state()?.prepare(|state| {
			let (update, remote) = state.pending_remote()?;
			state.base = G::freeze(remote);
			state.graph = G::thaw(state.base.clone())?;
			state.remote_revision = update.remote_revision();
			state.local_changes.clear();
			state.pending = None;
			state.paused = false;
			resume_queue(state)
		})
	}

	/// Adopts the new remote base while retaining the opaque local working
	/// copy.
	///
	/// # Errors
	/// Returns missing-conflict, callback or queued-delta validation errors.
	pub fn keep_local(&self) -> FerrosGrynnResult<LiveEvent> {
		self.mutable_state()?.prepare(|state| {
			let (update, remote) = state.pending_remote()?;
			state.base = G::freeze(remote);
			state.pending = None;
			G::adopt_revision(&mut state.graph, update.remote_revision());
			state.remote_revision = update.remote_revision();
			state.paused = false;
			resume_queue(state)
		})
	}

	/// Discards local decisions and validates the complete pending remote
	/// state.
	///
	/// # Errors
	/// Returns callback or queued-delta validation errors without partial
	/// publication.
	pub fn discard_local(&self) -> FerrosGrynnResult<LiveEvent> {
		self.mutable_state()?.prepare(|state| {
			state.graph = G::thaw(state.base.clone())?;
			state.local_changes.clear();
			state.paused = false;
			if let Some(update) = state.pending.take() {
				let event = apply_or_pause(state, update)?;
				if state.queue.is_empty() {
					Ok(event)
				} else {
					resume_queue(state)
				}
			} else {
				resume_queue(state)
			}
		})
	}

	/// Builds an explicit local candidate over the new complete remote base.
	///
	/// # Errors
	/// Returns callback, identity or family errors while preserving the
	/// unresolved conflict.
	pub fn rebase<ReplayFn: FnOnce(&mut G) -> FerrosGrynnResult<()>>(
		&self,
		replay_local: ReplayFn,
	) -> FerrosGrynnResult<LiveEvent> {
		self.mutable_state()?.prepare(|state| {
			let (update, remote) = state.pending_remote()?;
			let base = G::freeze(remote);
			let mut rebased = G::thaw(base.clone())?;
			replay_local(&mut rebased)?;
			validate_identity(&rebased, state.graph.identity())?;
			rebased.validate()?;
			state.base = base;
			state.graph = rebased;
			state.remote_revision = update.remote_revision();
			state.pending = None;
			state.paused = false;
			resume_queue(state)
		})
	}

	/// Passes a complete validated local candidate to the caller's write-back
	/// adapter.
	///
	/// # Errors
	/// Returns unresolved-conflict, family or adapter errors; the adapter owns
	/// transaction atomicity.
	pub fn flush<GWB: GraphWriteBack<G>>(&self, writer: &mut GWB) -> FerrosGrynnResult<()> {
		let mut state = self.mutable_state()?;
		if state.paused {
			return Err(invalid_state("a conflicted live graph cannot be flushed"));
		}
		let mut candidate = G::clone_mutable(&state.graph);
		candidate.validate()?;
		G::flush(&mut candidate, writer)?;
		state.base = G::freeze(G::clone_mutable(&candidate));
		state.remote_revision = candidate.revision();
		state.graph = candidate;
		state.local_changes.clear();
		Ok(())
	}

	/// Resumes ordered updates after the pending conflict has been resolved.
	///
	/// # Errors
	/// Returns unresolved-conflict or queued-delta validation errors.
	pub fn resume(&self) -> FerrosGrynnResult<LiveEvent> {
		self.mutable_state()?.prepare(|state| {
			if state.pending.is_some() {
				return Err(invalid_state("the pending conflict must be resolved before resume"));
			}
			state.paused = false;
			resume_queue(state)
		})
	}

	/// Locks the mutable layout and rejects an incompatible internal state.
	fn mutable_state(&self) -> FerrosGrynnResult<parking_lot::MutexGuard<'_, MutableLiveState<G>>> {
		let LiveInner::Mutable(state) = &self.inner else {
			return Err(invalid_state(
				"a mutable operation was requested from a frozen live graph",
			));
		};
		Ok(state.lock())
	}
}

/// Builds the existing typed live-state diagnostic at the runtime boundary.
fn invalid_state(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::live_graph_state_error(reason.into())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::values::{
		PropertyMap,
		Value,
		runtime::graph::{AtomId, DirtyTarget, Graph, GraphMut, Mutable},
	};

	#[test]
	fn frozen_guards_keep_the_revision_they_observed() {
		let mut graph = Graph::<Mutable>::new();
		graph.add_node(None, PropertyMap::new()).unwrap();
		let live = LiveGraph::new(graph.freeze());
		let old = live.read().unwrap();
		let mut next = (*old).clone().thaw();
		next.add_node(None, PropertyMap::new()).unwrap();
		live.publish(next.freeze()).unwrap();

		assert_eq!(old.node_count(), 1);
		assert_eq!(live.read().unwrap().node_count(), 2);
	}

	#[test]
	fn mutable_live_graph_pauses_on_dirty_intersection() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).unwrap();
		let identity = graph.identity();
		let remote_revision = graph.revision() + 10;
		let live = LiveGraph::<Mutable>::new_mutable(graph);
		live.mutate_local([DirtyTarget::Atom(node)], |graph| {
			graph.node_properties_mut(node)?.insert(1_u32.into(), 1);
			Ok(())
		})
		.unwrap();
		let update =
			LiveUpdate::new(identity, remote_revision, [DirtyTarget::Atom(node)], move |graph| {
				graph.node_properties_mut(node)?.insert(2_u32.into(), 2);
				Ok(())
			});

		let event = live.ingest(update).unwrap();
		assert!(matches!(event, LiveEvent::Conflict { .. }));
		assert!(live.mutate_local([], |_| Ok(())).is_err());
	}

	#[test]
	fn disjoint_remote_update_is_applied_automatically() {
		let mut graph = Graph::<Mutable>::new();
		let local = graph.add_node(None, PropertyMap::new()).unwrap();
		let remote = graph.add_node(None, PropertyMap::new()).unwrap();
		let identity = graph.identity();
		let revision = graph.revision() + 10;
		let live = LiveGraph::<Mutable>::new_mutable(graph);
		live.mutate_local([DirtyTarget::Atom(local)], |_| Ok(())).unwrap();
		let update =
			LiveUpdate::new(identity, revision, [DirtyTarget::Atom(remote)], move |graph| {
				graph.node_properties_mut(remote)?.insert(9_u32.into(), 42);
				Ok(())
			});
		let event = live.ingest(update).unwrap();

		assert!(matches!(event, LiveEvent::Applied { .. }));
		assert!(
			live.with_graph(|graph| graph
				.node(remote)
				.unwrap()
				.properties()
				.contains_key(9_u32.into()))
				.unwrap()
		);
	}

	#[test]
	fn accept_remote_discards_local_conflicting_state() {
		let (live, node) = conflicted_live_graph();
		assert!(matches!(live.accept_remote().unwrap(), LiveEvent::Resumed { .. }));
		assert!(!has_property(&live, node, 1));
		assert!(has_property(&live, node, 2));
	}

	#[test]
	fn keep_local_discards_the_pending_remote_delta() {
		let (live, node) = conflicted_live_graph();
		assert!(matches!(live.keep_local().unwrap(), LiveEvent::Resumed { .. }));
		assert!(has_property(&live, node, 1));
		assert!(!has_property(&live, node, 2));
		let state = live.mutable_state().unwrap();
		assert!(
			crate::graph::GraphRead::node(&state.base, node)
				.unwrap()
				.unwrap()
				.properties()
				.contains_key(2_u32.into())
		);
		assert!(state.local_changes.contains(&DirtyTarget::Atom(node)));
	}

	#[test]
	fn discard_local_applies_the_pending_remote_delta() {
		let (live, node) = conflicted_live_graph();
		assert!(matches!(live.discard_local().unwrap(), LiveEvent::Applied { .. }));
		assert!(!has_property(&live, node, 1));
		assert!(has_property(&live, node, 2));
	}

	#[test]
	fn rebase_replays_local_changes_over_remote_state() {
		let (live, node) = conflicted_live_graph();
		assert!(matches!(
			live.rebase(move |graph| {
				graph.node_properties_mut(node)?.insert(1_u32.into(), Value::from("local"));
				Ok(())
			})
			.unwrap(),
			LiveEvent::Resumed { .. },
		));
		assert!(has_property(&live, node, 1));
		assert!(has_property(&live, node, 2));
	}

	fn conflicted_live_graph() -> (LiveGraph<Mutable>, AtomId) {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).unwrap();
		let identity = graph.identity();
		let remote_revision = graph.revision() + 10;
		let live = LiveGraph::<Mutable>::new_mutable(graph);
		live.mutate_local([DirtyTarget::Atom(node)], move |graph| {
			graph.node_properties_mut(node)?.insert(1_u32.into(), Value::from("local"));
			Ok(())
		})
		.unwrap();
		let update =
			LiveUpdate::new(identity, remote_revision, [DirtyTarget::Atom(node)], move |graph| {
				graph.node_properties_mut(node)?.insert(2_u32.into(), Value::from("remote"));
				Ok(())
			});
		assert!(matches!(live.ingest(update).unwrap(), LiveEvent::Conflict { .. }));
		(live, node)
	}

	fn has_property(live: &LiveGraph<Mutable>, node: AtomId, key: u32) -> bool {
		live.with_graph(|graph| {
			graph.node(node).is_some_and(|node| node.properties().contains_key(key.into()))
		})
		.unwrap()
	}
}
