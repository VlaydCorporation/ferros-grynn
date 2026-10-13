//! Automatic whole-commit graph materialization over the existing LIVE handle.

use std::{collections::BTreeMap, sync::Arc};

use super::{
	LiveCursor,
	LiveGap,
	LiveGapReason,
	LiveGraphImage,
	LiveNext,
	LiveOptions,
	LivePayload,
	LiveSubscription,
};
use crate::{
	changefeed::Versionstamp,
	datastore::BindingAdmission,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{Frozen, Graph, LiveFamily, LiveGraph, Mutable},
	values::{PropertyKeyId, Text},
};

/// Qualified dirty tracking and explicit resolution own mutable SDK behavior.
mod mutable;

/// Retains the whole incoming revision until an explicit caller decision.
pub(super) struct PendingConflict {
	/// An issued queue token, absent for exact commit feedback awaiting
	/// delivery.
	delivery: Option<super::LiveDelivery>,
	/// The image against which the retained local decisions were computed.
	local_base: Arc<LiveGraphImage>,
	/// Pins the owned qualified changes presented to the user for resolution.
	conflict: Arc<super::LiveGraphConflict>,
	/// Emits each pending conflict once before waiting for user commands.
	reported: bool,
}

/// A committed source write awaits ordered result delivery, without retaining a
/// transaction.
pub(super) struct PendingFlush {
	/// Exact accepted checkpoint returned by the commit publication boundary.
	checkpoint: Arc<super::LiveGraphBatch>,
	/// Local decisions included in the successful source write.
	local: Arc<LiveGraphImage>,
}

/// Result of accepting one complete remote graph revision.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LiveGraphNext {
	/// The subscription remains active without a queued revision.
	Idle,
	/// The complete graph was accepted and its delivery confirmed.
	Updated {
		revision: Versionstamp,
	},
	/// Application and ACK pause until the caller explicitly resolves this
	/// state.
	Conflict(Arc<super::LiveGraphConflict>),
	/// Coverage is unavailable; the last agreed materialization remains pinned.
	Gap(LiveGap),
	/// Evaluation failed while primary data committed successfully.
	Fault {
		diagnostic: FerrosGrynnError,
		gap: LiveGap,
	},
	/// The consumer has terminated; retained read guards remain valid.
	Closed,
}

/// One graph result incarnation with explicit mutable transition.
#[must_use = "dropping this handle detaches its LIVE consumer"]
pub struct LiveGraphSubscription<State = Frozen, G = Graph<State>>
where
	G: LiveFamily<State = State>,
{
	/// Owns the existing LIVE consumer and durable ACK cursor.
	pub(crate) subscription: LiveSubscription,
	/// Retains the accepted subscription resource and queue controls.
	pub(crate) options: LiveOptions,
	/// Retains the agreed graph revision independently of future publications.
	pub(crate) graph: Option<LiveGraph<State, G>>,
	/// Pins the exact last accepted logical remote base.
	pub(crate) remote: Option<Arc<LiveGraphImage>>,
	/// Stores the complete accepted durable graph watermark.
	pub(crate) revision: Versionstamp,
	/// Retains an applied token until its durable ACK succeeds.
	pub(crate) unconfirmed: Option<LiveCursor>,
	/// Owns the stable SDK property handles independently of source dictionary
	/// IDs.
	pub(crate) names: BTreeMap<PropertyKeyId, Text>,
	/// Retains the unresolved whole incoming delta without acknowledgement.
	pub(crate) pending: Option<PendingConflict>,
	/// Retains exact source-commit feedback until ordered result adoption.
	pub(crate) flush: Option<PendingFlush>,
}

impl<State, G: LiveFamily<State = State>> LiveGraphSubscription<State, G> {
	/// Borrows the currently agreed materialization.
	///
	/// # Errors
	/// Returns unavailable coverage until an explicit resnapshot supplies it.
	pub fn materialization(&self) -> FerrosGrynnResult<&LiveGraph<State, G>> {
		self.graph.as_ref().ok_or_else(|| {
			FerrosGrynnError::invalid_argument(
				"LIVE graph has no covered materialization; resnapshot is required",
			)
		})
	}

	/// Borrows the last confirmed whole-delivery cursor.
	pub const fn cursor(&self) -> &LiveCursor {
		self.subscription.cursor()
	}

	/// Returns the stable subscription incarnation.
	pub const fn id(&self) -> super::LiveId {
		self.subscription.id()
	}

	/// Returns the last accepted graph watermark, including unconfirmed apply.
	pub fn revision(&self) -> Option<Versionstamp> {
		self.remote.as_ref().map(|_| self.revision)
	}

	/// Borrows an applied token whose ACK still needs confirmation.
	pub const fn applied_unconfirmed(&self) -> Option<&LiveCursor> {
		self.unconfirmed.as_ref()
	}

	/// Borrows exact qualified identities and source mappings of the remote
	/// base.
	pub fn remote_image(&self) -> Option<&LiveGraphImage> {
		self.remote.as_deref()
	}

	/// Stops capture durably and detaches the consumer.
	///
	/// # Errors
	/// Returns the existing transaction, capacity or persistence failure.
	pub fn close(&mut self) -> FerrosGrynnResult<()> {
		self.subscription.close()
	}

	/// Starts a fresh frozen incarnation from current data, closing this
	/// capture atomically. Existing materializations and old read guards
	/// remain usable.
	///
	/// # Errors
	/// Returns origin, source, family, resource or publication errors; no
	/// implicit resnapshot occurs while polling GAP/fault.
	pub fn resnapshot(
		&mut self,
		store: &crate::datastore::Datastore,
	) -> FerrosGrynnResult<LiveGraphSubscription<Frozen, G::Frozen>> {
		let replacement = store.resnapshot_live_graph::<G::Frozen>(self.cursor())?;
		self.subscription.inner.detach_closed();
		Ok(replacement)
	}

	/// Reports unavailable coverage while retaining the last agreed
	/// materialization.
	fn gap(&self, through: Versionstamp) -> LiveGraphNext {
		LiveGraphNext::Gap(LiveGap {
			id: self.id(),
			reason: LiveGapReason::InsufficientCoverage,
			from: Some(self.cursor().versionstamp()),
			through: Some(through),
		})
	}
}

/// Restores one candidate using the subscription's existing resource controls.
fn restore<G: LiveFamily<State = Frozen, Frozen = G>>(
	image: &LiveGraphImage,
	id: super::LiveId,
	revision: Versionstamp,
	options: &LiveOptions,
	previous: Option<&LiveGraphImage>,
) -> FerrosGrynnResult<G> {
	restore_named(image, id, revision, options, previous, None)
}

/// Mutable SDK name handles are independent of durable source dictionary IDs.
fn restore_named<G: LiveFamily<State = Frozen, Frozen = G>>(
	image: &LiveGraphImage,
	id: super::LiveId,
	revision: Versionstamp,
	options: &LiveOptions,
	previous: Option<&LiveGraphImage>,
	names: Option<&BTreeMap<PropertyKeyId, Text>>,
) -> FerrosGrynnResult<G> {
	let limits = super::LiveBindingOptions::default()
		.with_budgets(options.scratch_bytes(), options.work())?;
	let mut budget = BindingAdmission::new(&limits);
	budget.admit(image.estimated_heap_bytes()?, 1)?;
	if let Some(previous) = previous {
		budget.admit(
			previous.estimated_heap_bytes()?.checked_mul(2).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("LIVE graph previous materialization bytes")
			})?,
			1,
		)?;
	}
	G::from_materialized(
		image
			.restore_with_keys(id, revision, names, &mut |bytes, work| budget.admit(bytes, work))?,
	)
}

impl<G: LiveFamily<State = Frozen, Frozen = G> + Clone> LiveGraphSubscription<Frozen, G> {
	/// Waits for one typed event through a single consumer, suitable for
	/// select. Dropping the future preserves every unaccepted delta and ACK
	/// token.
	///
	/// # Errors
	/// Returns the same checked graph, resource or persistence error as `next`.
	pub async fn next_event(&mut self) -> FerrosGrynnResult<LiveGraphNext> {
		loop {
			let event = self.next()?;
			if !matches!(event, LiveGraphNext::Idle) {
				return Ok(event);
			}
			self.subscription.inner.ready(false).await?;
		}
	}

	/// Uses the accepted activation snapshot or a proven predecessor at ACK.
	pub(crate) fn registered(
		mut subscription: LiveSubscription,
		options: LiveOptions,
	) -> FerrosGrynnResult<Self> {
		let initial = subscription.inner.graph_bootstrap()?;
		let (graph, remote, revision) = if let Some((image, revision)) = initial {
			let graph = restore::<G>(&image, subscription.id(), revision, &options, None)?;
			(Some(LiveGraph::new(graph)), Some(image), revision)
		} else {
			(None, None, 0.into())
		};
		Ok(Self {
			subscription,
			options,
			graph,
			remote,
			revision,
			unconfirmed: None,
			names: BTreeMap::new(),
			pending: None,
			flush: None,
		})
	}

	/// Accepts a complete candidate, then ACKs; failed ACK never reapplies it.
	///
	/// # Errors
	/// Returns family, graph closure, resource, transaction or persistence
	/// errors.
	#[allow(
		clippy::should_implement_trait,
		reason = "idle-aware fallible polling is not an Iterator"
	)]
	pub fn next(&mut self) -> FerrosGrynnResult<LiveGraphNext> {
		if let Some(token) = &self.unconfirmed {
			if matches!(self.subscription.next()?, LiveNext::Closed) {
				return Ok(LiveGraphNext::Closed);
			}
			self.subscription.ack(token)?;
			self.unconfirmed = None;
			return Ok(LiveGraphNext::Updated {
				revision: self.revision,
			});
		}
		match self.subscription.next()? {
			LiveNext::Idle => Ok(LiveGraphNext::Idle),
			LiveNext::Closed => Ok(LiveGraphNext::Closed),
			LiveNext::Delivery(delivery) => match delivery.payload() {
				LivePayload::Gap(gap) => Ok(LiveGraphNext::Gap(gap.clone())),
				LivePayload::Fault {
					diagnostic,
					gap,
				} => Ok(LiveGraphNext::Fault {
					diagnostic: diagnostic.clone(),
					gap: gap.clone(),
				}),
				LivePayload::Batch(_) => Err(FerrosGrynnError::invalid_storage_file(
					"graph subscription received row data",
				)),
				LivePayload::Graph(batch) => {
					if self.remote.is_none() || batch.delta().base() != self.revision {
						return Ok(self.gap(batch.delta().base()));
					}
					let candidate = restore::<G>(
						batch.delta().after(),
						self.id(),
						batch.versionstamp(),
						&self.options,
						self.remote.as_deref(),
					)?;
					self.materialization()?.publish(candidate)?;
					self.remote = Some(Arc::clone(&batch.delta.after));
					self.revision = batch.versionstamp();
					self.unconfirmed = Some(delivery.token().clone());
					self.subscription.ack(delivery.token())?;
					self.unconfirmed = None;
					Ok(LiveGraphNext::Updated {
						revision: self.revision,
					})
				}
			},
		}
	}

	/// Explicitly switches this subscription to local mutable COW state.
	///
	/// # Errors
	/// Returns a checked family thaw or unavailable materialization error.
	pub fn into_mutable(self) -> FerrosGrynnResult<LiveGraphSubscription<Mutable, G::Mutable>> {
		let graph = self.materialization()?.into_mutable()?;
		let names = mutable::names(
			self.remote
				.as_deref()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("mutable source image absent"))?,
			&self.options,
		)?;
		Ok(LiveGraphSubscription {
			subscription: self.subscription,
			options: self.options,
			graph: Some(graph),
			remote: self.remote,
			revision: self.revision,
			unconfirmed: self.unconfirmed,
			names,
			pending: None,
			flush: None,
		})
	}
}
