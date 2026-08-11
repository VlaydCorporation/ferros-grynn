use std::{
	collections::{BTreeSet, VecDeque},
	fmt::{self, Debug, Formatter},
	marker::PhantomData,
	ops::Deref,
	sync::Arc,
};

use parking_lot::{Mutex, RwLock};

use super::{DirtyTarget, Frozen, Graph, GraphRead, GraphWriteBack, Mutable};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::Uuid,
};

type ApplyUpdate = dyn Fn(&mut Graph<Mutable>) -> FerrosGrynnResult<()> + Send + Sync + 'static;

pub struct LiveUpdate {
	origin: Uuid,
	remote_revision: u64,
	changes: BTreeSet<DirtyTarget>,
	apply: Arc<ApplyUpdate>,
}

impl LiveUpdate {
	/// Creates an adapter-defined changefeed delta.
	///
	/// The callback must be deterministic and free of external side effects: a
	/// mutable live graph may apply it independently to its remote base and its
	/// local working copy.
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

	fn apply(&self, graph: &mut Graph<Mutable>) -> FerrosGrynnResult<()> {
		(self.apply)(graph)
	}
}

impl Clone for LiveUpdate {
	fn clone(&self) -> Self {
		Self {
			origin: self.origin,
			remote_revision: self.remote_revision,
			changes: self.changes.clone(),
			apply: Arc::clone(&self.apply),
		}
	}
}

impl Debug for LiveUpdate {
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
		remote_revision: u64,
	},
	Queued {
		remote_revision: u64,
	},
	Conflict {
		incoming_changes: Vec<DirtyTarget>,
		local_changes: Vec<DirtyTarget>,
		conflicts: Vec<DirtyTarget>,
		remote_revision: u64,
	},
	Resumed {
		remote_revision: u64,
	},
}

struct MutableLiveState {
	base: Graph<Frozen>,
	graph: Graph<Mutable>,
	remote_revision: u64,
	local_changes: BTreeSet<DirtyTarget>,
	pending: Option<LiveUpdate>,
	queue: VecDeque<LiveUpdate>,
	paused: bool,
}

enum LiveInner {
	Frozen(RwLock<Arc<Graph<Frozen>>>),
	Mutable(Mutex<MutableLiveState>),
}

pub struct LiveGraph<State = Frozen> {
	inner: LiveInner,
	state: PhantomData<State>,
}

pub struct LiveReadGuard {
	graph: Arc<Graph<Frozen>>,
}

impl LiveReadGuard {
	pub fn revision(&self) -> u64 {
		self.graph.revision()
	}
}

impl Deref for LiveReadGuard {
	type Target = Graph<Frozen>;

	fn deref(&self) -> &Self::Target {
		&self.graph
	}
}

impl LiveGraph<Frozen> {
	pub fn new(graph: Graph<Frozen>) -> Self {
		Self {
			inner: LiveInner::Frozen(RwLock::new(Arc::new(graph))),
			state: PhantomData,
		}
	}

	/// Returns a revision-pinned guard. Publication after this call cannot
	/// alter the graph observed through the guard.
	pub fn read(&self) -> FerrosGrynnResult<LiveReadGuard> {
		let LiveInner::Frozen(current) = &self.inner else {
			return Err(invalid_state("a frozen read was requested from a mutable live graph"));
		};
		Ok(LiveReadGuard {
			graph: Arc::clone(&current.read()),
		})
	}

	pub fn snapshot(&self) -> FerrosGrynnResult<Arc<Graph<Frozen>>> {
		self.read().map(|guard| guard.graph)
	}

	pub fn publish(&self, graph: Graph<Frozen>) -> FerrosGrynnResult<()> {
		let LiveInner::Frozen(current) = &self.inner else {
			return Err(invalid_state(
				"a frozen publication was requested from a mutable live graph",
			));
		};
		let mut current = current.write();
		if graph.identity() != current.identity() {
			return Err(FerrosGrynnError::graph_identity_mismatch(
				current.identity().to_string(),
				graph.identity().to_string(),
			));
		}
		if graph.revision() <= current.revision() {
			return Err(invalid_state(format!(
				"revision {} does not advance current revision {}",
				graph.revision(),
				current.revision(),
			)));
		}
		*current = Arc::new(graph);
		Ok(())
	}

	pub fn apply_update(&self, update: &LiveUpdate) -> FerrosGrynnResult<()> {
		let current = self.snapshot()?;
		validate_update(current.as_ref(), update, current.revision())?;
		// ponytail: this copies the general SoA until graph storage is split into
		// copy-on-write chunks; the publication boundary already stays atomic.
		let mut candidate = current.as_ref().clone().thaw();
		update.apply(&mut candidate)?;
		candidate.adopt_external_revision(update.remote_revision());
		self.publish(candidate.freeze())
	}

	// TODO: does this consume `self`?
	pub fn into_mutable(&self) -> FerrosGrynnResult<LiveGraph<Mutable>> {
		let graph = self.snapshot()?.as_ref().clone().thaw();
		Ok(LiveGraph::<Mutable>::new_mutable(graph))
	}
}

impl LiveGraph<Mutable> {
	pub fn new_mutable(graph: Graph<Mutable>) -> Self {
		let base = graph.freeze();
		let remote_revision = base.revision();
		let graph = base.clone().thaw();
		Self {
			inner: LiveInner::Mutable(Mutex::new(MutableLiveState {
				base,
				graph,
				remote_revision,
				local_changes: BTreeSet::new(),
				pending: None,
				queue: VecDeque::new(),
				paused: false,
			})),
			state: PhantomData,
		}
	}

	pub fn mutate_local<T, Targets, MutFn>(
		&self,
		changes: Targets,
		mutation: MutFn,
	) -> FerrosGrynnResult<T>
	where
		Targets: IntoIterator<Item = DirtyTarget>,
		MutFn: FnOnce(&mut Graph<Mutable>) -> FerrosGrynnResult<T>,
	{
		let LiveInner::Mutable(state) = &self.inner else {
			return Err(invalid_state(
				"a mutable operation was requested from a frozen live graph",
			));
		};
		let mut state = state.lock();
		if state.paused {
			return Err(invalid_state(
				"local mutation is paused until the live conflict is resolved",
			));
		}
		let mut candidate = state.graph.clone_preserving_state();
		let result = mutation(&mut candidate)?;
		state.graph = candidate;
		state.local_changes.extend(changes);
		Ok(result)
	}

	pub fn with_graph<T, ReadFn: FnOnce(&Graph<Mutable>) -> T>(
		&self,
		read: ReadFn,
	) -> FerrosGrynnResult<T> {
		let LiveInner::Mutable(state) = &self.inner else {
			return Err(invalid_state("a mutable read was requested from a frozen live graph"));
		};
		Ok(read(&state.lock().graph))
	}

	pub fn static_snapshot(&self) -> FerrosGrynnResult<Graph<Frozen>> {
		let LiveInner::Mutable(state) = &self.inner else {
			return Err(invalid_state("a mutable snapshot was requested from a frozen live graph"));
		};
		Ok(state.lock().graph.clone_preserving_state().freeze())
	}

	pub fn ingest(&self, update: LiveUpdate) -> FerrosGrynnResult<LiveEvent> {
		let LiveInner::Mutable(state) = &self.inner else {
			return Err(invalid_state("a mutable update was sent to a frozen live graph"));
		};
		let mut state = state.lock();
		validate_update(&state.graph, &update, state.remote_revision)?;
		if state.paused {
			let revision = update.remote_revision();
			state.queue.push_back(update);
			return Ok(LiveEvent::Queued {
				remote_revision: revision,
			});
		}
		apply_or_pause(&mut state, update)
	}

	pub fn accept_remote(&self) -> FerrosGrynnResult<LiveEvent> {
		let mut state = self.mutable_state()?;
		let update =
			state.pending.take().ok_or_else(|| invalid_state("there is no pending conflict"))?;
		let mut candidate = state.base.clone().thaw();
		update.apply(&mut candidate)?;
		candidate.adopt_external_revision(update.remote_revision());
		state.base = candidate.freeze();
		state.graph = state.base.clone().thaw();
		state.remote_revision = update.remote_revision();
		state.local_changes.clear();
		state.paused = false;
		resume_queue(&mut state)
	}

	pub fn keep_local(&self) -> FerrosGrynnResult<LiveEvent> {
		let mut state = self.mutable_state()?;
		let update =
			state.pending.take().ok_or_else(|| invalid_state("there is no pending conflict"))?;
		state.base = state.graph.clone_preserving_state().freeze();
		state.remote_revision = update.remote_revision();
		state.paused = false;
		resume_queue(&mut state)
	}

	pub fn discard_local(&self) -> FerrosGrynnResult<LiveEvent> {
		let mut state = self.mutable_state()?;
		state.graph = state.base.clone().thaw();
		state.local_changes.clear();
		state.paused = false;
		if let Some(update) = state.pending.take() {
			apply_or_pause(&mut state, update)
		} else {
			resume_queue(&mut state)
		}
	}

	pub fn rebase<ReplayFn: FnOnce(&mut Graph<Mutable>) -> FerrosGrynnResult<()>>(
		&self,
		replay_local: ReplayFn,
	) -> FerrosGrynnResult<LiveEvent> {
		let mut state = self.mutable_state()?;
		let update =
			state.pending.take().ok_or_else(|| invalid_state("there is no pending conflict"))?;
		let mut remote = state.base.clone().thaw();
		update.apply(&mut remote)?;
		remote.adopt_external_revision(update.remote_revision());
		state.base = remote.freeze();
		let mut rebased = state.base.clone().thaw();
		replay_local(&mut rebased)?;
		state.graph = rebased;
		state.remote_revision = update.remote_revision();
		state.paused = false;
		resume_queue(&mut state)
	}

	pub fn flush<GWB: GraphWriteBack<Graph<Mutable>>>(
		&self,
		writer: &mut GWB,
	) -> FerrosGrynnResult<()> {
		let mut state = self.mutable_state()?;
		if state.paused {
			return Err(invalid_state("a conflicted live graph cannot be flushed"));
		}
		state.graph.flush(writer)?;
		state.base = state.graph.clone_preserving_state().freeze();
		state.remote_revision = state.graph.revision();
		state.local_changes.clear();
		Ok(())
	}

	pub fn resume(&self) -> FerrosGrynnResult<LiveEvent> {
		let mut state = self.mutable_state()?;
		if state.pending.is_some() {
			return Err(invalid_state("the pending conflict must be resolved before resume"));
		}
		state.paused = false;
		resume_queue(&mut state)
	}

	fn mutable_state(&self) -> FerrosGrynnResult<parking_lot::MutexGuard<'_, MutableLiveState>> {
		let LiveInner::Mutable(state) = &self.inner else {
			return Err(invalid_state(
				"a mutable operation was requested from a frozen live graph",
			));
		};
		Ok(state.lock())
	}
}

fn apply_or_pause(
	state: &mut MutableLiveState,
	update: LiveUpdate,
) -> FerrosGrynnResult<LiveEvent> {
	let conflicts = conflicting_changes(&state.local_changes, update.changes());
	if !conflicts.is_empty() {
		state.paused = true;
		let event = LiveEvent::Conflict {
			incoming_changes: update.changes().iter().cloned().collect(),
			local_changes: state.local_changes.iter().cloned().collect(),
			conflicts: conflicts.into_iter().collect(),
			remote_revision: update.remote_revision(),
		};
		state.pending = Some(update);
		return Ok(event);
	}

	let mut base = state.base.clone().thaw();
	update.apply(&mut base)?;
	base.adopt_external_revision(update.remote_revision());
	let mut current = state.graph.clone_preserving_state();
	update.apply(&mut current)?;
	state.base = base.freeze();
	state.graph = current;
	state.remote_revision = update.remote_revision();
	Ok(LiveEvent::Applied {
		remote_revision: update.remote_revision(),
	})
}

fn resume_queue(state: &mut MutableLiveState) -> FerrosGrynnResult<LiveEvent> {
	while let Some(update) = state.queue.pop_front() {
		let event = apply_or_pause(state, update)?;
		if matches!(event, LiveEvent::Conflict { .. }) {
			return Ok(event);
		}
	}
	Ok(LiveEvent::Resumed {
		remote_revision: state.remote_revision,
	})
}

fn validate_update<R: GraphRead>(
	graph: &R,
	update: &LiveUpdate,
	current_revision: u64,
) -> FerrosGrynnResult<()> {
	if graph.identity() != update.origin() {
		return Err(FerrosGrynnError::graph_identity_mismatch(
			graph.identity().to_string(),
			update.origin().to_string(),
		));
	}
	if update.remote_revision() <= current_revision {
		return Err(invalid_state(format!(
			"remote revision {} does not advance revision {current_revision}",
			update.remote_revision(),
		)));
	}
	Ok(())
}

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

fn invalid_state(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::live_graph_state_error(reason.into())
}

#[cfg(test)]
mod tests {
	use super::{LiveEvent, LiveGraph, LiveUpdate};
	use crate::values::{
		PropertyMap, Value,
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
