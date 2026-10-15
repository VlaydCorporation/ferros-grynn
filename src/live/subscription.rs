//! Owned pull handle; waiting never retains a datastore transaction snapshot.

use super::{LiveCursor, LiveId, LiveNext};
use crate::{datastore::SubscriptionHandle, gql_status::FerrosGrynnResult};

/// One attached consumer of an immutable LIVE query incarnation.
///
/// Dropping this handle detaches the consumer. Use `close` to stop capture.
/// `next` is nonblocking and repeats the same delivery until successful ACK.
///
/// ```
/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
/// use ferros_grynn::{
/// 	datastore::{ClassSchema, Datastore},
/// 	graph::AtomKind,
/// 	live::{LiveCursor, LiveNext, LiveOptions, LiveQuery, LiveSource},
/// 	values::PropertyMap,
/// };
/// let store = Datastore::in_memory();
/// let mut schema = store.begin();
/// let class = schema.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
/// schema.commit()?;
/// let mut subscription =
/// 	store.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
/// let mut write = store.begin();
/// write.create_atom("item:a".parse()?, PropertyMap::new())?;
/// write.commit()?;
/// if let LiveNext::Delivery(delivery) = subscription.next()? {
/// 	subscription.ack(delivery.token())?;
/// }
/// let saved = subscription.cursor().encode();
/// assert_eq!(LiveCursor::decode(saved.as_slice())?, *subscription.cursor());
/// subscription.close()?;
/// # Ok(())
/// # }
/// ```
#[must_use = "dropping the handle detaches its LIVE consumer"]
#[allow(
	clippy::tabs_in_doc_comments,
	reason = "nightly rustfmt uses required hard tabs in Rustdoc examples"
)]
pub struct LiveSubscription {
	/// Private concrete datastore adapter; no storage types enter the SDK.
	pub(crate) inner: SubscriptionHandle,
}

impl std::fmt::Debug for LiveSubscription {
	/// Formats `LiveSubscription` using the requested formatter.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("LiveSubscription")
			.field("id", &self.id())
			.field("cursor", self.cursor())
			.finish_non_exhaustive()
	}
}

impl LiveSubscription {
	/// Returns the stable subscription incarnation used for deduplication.
	pub const fn id(&self) -> LiveId {
		self.inner.id()
	}

	/// Polls a whole commit, explicit GAP/fault, idle state or termination.
	///
	/// # Errors
	///
	/// Returns an attachment or checked local state error.
	#[allow(
		clippy::should_implement_trait,
		reason = "LIVE next is fallible idle-aware polling with ACK, not an Iterator"
	)]
	pub fn next(&mut self) -> FerrosGrynnResult<LiveNext> {
		self.inner.next()
	}

	/// Confirms a whole previously issued delivery. Repeated accepted ACK is
	/// idempotent; failed persistence leaves the delivery retained.
	///
	/// # Errors
	///
	/// Returns origin/query/version, unissued-token, FCW/SSI, resource or I/O
	/// errors.
	pub fn ack(&mut self, token: &LiveCursor) -> FerrosGrynnResult<()> {
		self.inner.ack(token)
	}

	/// Returns the last confirmed whole-commit watermark as owned handle data.
	pub const fn cursor(&self) -> &LiveCursor {
		self.inner.cursor()
	}

	/// Stops the subscription durably and detaches this consumer.
	/// Delivery termination or a fused read error does not replace this
	/// operation.
	///
	/// # Errors
	///
	/// Returns writer, conflict, resource or persistence errors. Failure
	/// preserves the cursor and any existing attachment so the caller can
	/// inspect or retry.
	pub fn close(&mut self) -> FerrosGrynnResult<()> {
		self.inner.close()
	}
}
