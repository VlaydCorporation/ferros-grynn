//! Pending connectors retain their selected logical endpoints across private
//! changes.

use std::mem::size_of;

use fg_runtime::__internal::RecordIdInternal as _;

use super::{Mutation, Transaction};
use crate::{
	datastore::DatabaseRoot,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{EdgeEndpoint, EdgeTopology, GraphId},
	values::RecordId,
};

/// Rollback restores entries while preserving charged spare vector capacity.
#[derive(Clone, Copy)]
pub(super) struct Checkpoint {
	/// Accepted anchors.
	len: usize,
	/// Owned logical descriptor bytes.
	bytes: usize,
}

/// One endpoint index preserves source/target ordering and repeated routes.
struct Endpoint {
	/// Position in the complete topology endpoint iterator.
	index: usize,
	/// Ordered owners followed by the terminal, all snapshot-independent.
	path: Vec<(GraphId, RecordId)>,
}

/// Prepared before mutation; append cannot allocate after source publication.
pub(super) struct Anchor {
	/// Connector source graph.
	graph: GraphId,
	/// Stable connector identity, independent of replay allocation remapping.
	record: RecordId,
	/// The mutation position distinguishes a subsequent explicit BUILD
	/// replacement.
	mutation: usize,
	/// Complete selected logical routes.
	endpoints: Vec<Endpoint>,
	/// Admitted owned descriptor bytes.
	pub(super) bytes: usize,
}

impl Anchor {
	/// A built connector is selected after its own tree replacement.
	pub(super) fn after_build(&mut self, position: usize) {
		self.mutation = position;
	}
}

/// Only qualified connector writes allocate this transaction owner.
#[derive(Default)]
pub(super) struct Journal {
	/// Append-only anchors make TRY/item rollback a truncation.
	entries: Vec<Anchor>,
	/// Owned endpoint and record data retained across import seals.
	bytes: usize,
}

impl Journal {
	/// Records a constant-size rollback position.
	pub(super) const fn checkpoint(&self) -> Checkpoint {
		Checkpoint {
			len: self.entries.len(),
			bytes: self.bytes,
		}
	}

	/// Drops rejected descriptors without discarding charged spare capacity.
	pub(super) fn restore(&mut self, checkpoint: Checkpoint) {
		self.entries.truncate(checkpoint.len);
		self.bytes = checkpoint.bytes;
	}

	/// Includes the optional owner and actual vector capacity.
	pub(super) const fn retained_bytes(&self) -> usize {
		self.bytes
			.saturating_add(size_of::<Self>())
			.saturating_add(self.entries.capacity().saturating_mul(size_of::<Anchor>()))
	}
}

impl Transaction {
	/// Admits logical copies and the pending source cost before tracking or
	/// mutation.
	pub(super) fn prepare_qualified_anchor(
		&mut self,
		graph: GraphId,
		record: &RecordId,
		topology: &EdgeTopology,
		pending: usize,
	) -> FerrosGrynnResult<Option<Anchor>> {
		let count = topology.endpoints().filter(|endpoint| endpoint.is_cross_level()).count();
		if count == 0 {
			return Ok(None);
		}
		let mut bytes = record
			.estimated_heap_bytes()
			.checked_add(count.saturating_mul(size_of::<Endpoint>()))
			.ok_or_else(|| self.limit_error(usize::MAX))?;
		let mut steps = 0_usize;
		for endpoint in topology.endpoints().filter(|endpoint| endpoint.is_cross_level()) {
			let (_terminal_graph, terminal) =
				self.working.resolve_incidence_endpoint(graph, endpoint, |_graph, owner| {
					let atom = self.working.atom(owner.global_id()).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"qualified owner base atom is absent",
						)
					})?;
					bytes = bytes
						.checked_add(
							atom.record_id().estimated_heap_bytes()
								+ size_of::<(GraphId, RecordId)>(),
						)
						.ok_or_else(|| self.limit_error(usize::MAX))?;
					steps += 1;
					Ok(())
				})?;
			let atom = self.working.atom(terminal.global_id()).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("qualified terminal base atom is absent")
			})?;
			bytes = bytes
				.checked_add(
					atom.record_id().estimated_heap_bytes() + size_of::<(GraphId, RecordId)>(),
				)
				.ok_or_else(|| self.limit_error(usize::MAX))?;
			steps += 1;
		}
		let growth = match &self.qualified_incidence {
			Some(journal) if journal.entries.len() < journal.entries.capacity() => 0,
			Some(journal) => journal.entries.capacity().max(4),
			None => 4,
		};
		let destination = if growth == 0 {
			0
		} else {
			self.qualified_incidence
				.as_ref()
				.map_or(0, |journal| journal.entries.capacity())
				.checked_add(growth)
				.ok_or_else(|| self.limit_error(usize::MAX))?
		};
		let reserve = destination
			.checked_mul(size_of::<Anchor>())
			.and_then(|bytes| {
				bytes.checked_add(if self.qualified_incidence.is_none() {
					size_of::<Journal>()
				} else {
					0
				})
			})
			.ok_or_else(|| self.limit_error(usize::MAX))?;
		let peak = bytes
			.checked_add(reserve)
			.and_then(|bytes| bytes.checked_add(pending))
			.and_then(|bytes| {
				bytes.checked_add(steps.checked_mul(
					576 + size_of::<(GraphId, crate::graph::AtomId, crate::datastore::GlobalAtomId)>(
					),
				)?)
			})
			.ok_or_else(|| self.limit_error(usize::MAX))?;
		self.ensure_capacity(peak)?;
		let mut endpoints = Vec::new();
		endpoints
			.try_reserve_exact(count)
			.map_err(|error| self.limit_error(peak).with_technical_source(error))?;
		for (index, endpoint) in topology.endpoints().enumerate() {
			let EdgeEndpoint::Nested(qualified) = endpoint else {
				continue;
			};
			let mut path = Vec::new();
			path.try_reserve_exact(qualified.scope().len() + 1)
				.map_err(|error| self.limit_error(peak).with_technical_source(error))?;
			let (terminal_graph, terminal) = self.working.resolve_incidence_endpoint(
				graph,
				endpoint,
				|owner_graph, owner| {
					let atom = self.working.atom(owner.global_id()).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"qualified owner base atom is absent",
						)
					})?;
					path.push((owner_graph, atom.record_id().clone()));
					Ok(())
				},
			)?;
			let atom = self.working.atom(terminal.global_id()).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("qualified terminal base atom is absent")
			})?;
			path.push((terminal_graph, atom.record_id().clone()));
			endpoints.push(Endpoint {
				index,
				path,
			});
		}
		let anchor = Anchor {
			graph,
			record: record.clone(),
			mutation: self.mutations.len(),
			endpoints,
			bytes,
		};
		let journal = self.qualified_incidence.get_or_insert_with(|| Box::new(Journal::default()));
		if growth != 0 {
			journal.entries.try_reserve_exact(growth).map_err(|error| {
				FerrosGrynnError::transaction_write_set_limit_exceeded(
					self.id,
					peak as u64,
					self.store.inner.transaction_limits().max_write_set_bytes as u64,
				)
				.with_technical_source(error)
			})?;
		}
		Ok(Some(anchor))
	}

	/// All fallible work and capacity admission precede the source mutation.
	pub(super) fn retain_qualified_anchor(&mut self, anchor: Option<Anchor>) {
		if let (Some(journal), Some(anchor)) = (&mut self.qualified_incidence, anchor) {
			journal.bytes += anchor.bytes;
			journal.entries.push(anchor);
		}
	}

	/// A validated pending anchor is an explicit logical replacement decision.
	pub(super) fn has_qualified_anchor(&self, graph: GraphId, record: &RecordId) -> bool {
		self.qualified_incidence.as_ref().is_some_and(|journal| {
			journal.entries.iter().any(|anchor| anchor.graph == graph && &anchor.record == record)
		})
	}

	/// Checks pending identities on the final candidate, including import
	/// seals.
	pub(super) fn validate_pending_qualified_incidence(
		&self,
		root: &DatabaseRoot,
	) -> FerrosGrynnResult<()> {
		let Some(journal) = &self.qualified_incidence else {
			return Ok(());
		};
		// shortcut: latest-anchor lookup uses the flat rollback journal; add an
		// index if measured qualified fanout makes END/commit scans
		// costly.
		for (position, anchor) in journal.entries.iter().enumerate() {
			if journal.entries[position + 1..]
				.iter()
				.any(|later| later.graph == anchor.graph && later.record == anchor.record)
			{
				continue;
			}
			if self.mutations.iter().enumerate().any(|(position, mutation)| position >= anchor.mutation &&
				matches!(mutation, Mutation::InstallBuiltGraphTree(tree) if tree.graph_ids().any(|graph| graph == anchor.graph))) { continue; }
			let Some(connector) = root
				.resolve_atom(&anchor.record)
				.and_then(|global| root.membership(anchor.graph, global))
				.and_then(|local| root.occurrence(anchor.graph, local))
			else {
				continue;
			};
			let crate::datastore::OccurrenceStateRef::Edge {
				topology,
				..
			} = connector.state().view()
			else {
				continue;
			};
			if !topology.is_cross_level() {
				continue;
			}
			let changed = || {
				FerrosGrynnError::graph_constraint_violation(
					anchor.graph.to_string(),
					"pending qualified connector changed its selected logical endpoint",
				)
			};
			for expected in &anchor.endpoints {
				let endpoint = topology.endpoints().nth(expected.index).ok_or_else(changed)?;
				let mut path = expected.path.iter();
				let (graph, terminal) =
					root.resolve_incidence_endpoint(anchor.graph, endpoint, |graph, owner| {
						let (expected_graph, record) = path.next().ok_or_else(changed)?;
						if *expected_graph != graph
							|| root.atom(owner.global_id()).map(|atom| atom.record_id())
								!= Some(record)
						{
							return Err(changed());
						}
						Ok(())
					})?;
				let (expected_graph, record) = path.next().ok_or_else(changed)?;
				if *expected_graph != graph
					|| root.atom(terminal.global_id()).map(|atom| atom.record_id()) != Some(record)
					|| path.next().is_some()
				{
					return Err(changed());
				}
			}
		}
		Ok(())
	}
}
