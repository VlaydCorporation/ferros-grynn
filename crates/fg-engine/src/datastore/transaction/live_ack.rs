//! Checked GQL attachment authority and rollback-aware ACK witnesses.

use super::Transaction;
use crate::{
	datastore::live_queue::ConsumerKey,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveCursor, LiveId},
};

/// Opaque authority supplied only by a session's retained SDK attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GqlLiveAttachment {
	/// Exact attachment nonce; a cursor alone cannot create this authority.
	key: ConsumerKey,
}

impl GqlLiveAttachment {
	/// Constructs authority at the retained handle boundary.
	pub(in crate::datastore) const fn new(key: ConsumerKey) -> Self {
		Self {
			key,
		}
	}

	/// Looks up the stable query without exposing the attachment nonce.
	pub(crate) const fn id(self) -> LiveId {
		self.key.id
	}
}

/// Issuance evidence remains private and is never written into durable history.
#[derive(Debug, Clone)]
pub struct GqlLiveAck {
	/// Session attachment checked again before its consuming commit.
	pub(in crate::datastore) attachment: GqlLiveAttachment,
	/// Complete issued origin/query/version and whole-commit position.
	pub(in crate::datastore) cursor: LiveCursor,
}

impl Transaction {
	/// Stages a session-authorized ACK without opening a nested transaction.
	pub(crate) fn acknowledge_gql_live(
		&mut self,
		attachment: GqlLiveAttachment,
		cursor: LiveCursor,
	) -> FerrosGrynnResult<()> {
		self.prepare_control()?;
		let evidence = {
			let _commit = self.store.inner.commit.lock();
			self.store.inner.live_consumers.ack_evidence(attachment.key, &cursor)?
		};
		if self
			.gql_live_acks
			.iter()
			.any(|ack| ack.attachment == attachment && ack.cursor == evidence)
		{
			return Ok(());
		}
		let bytes = std::mem::size_of::<GqlLiveAck>() * 2;
		self.ensure_capacity(bytes + crate::datastore::consts::SCHEMA_WRITE_BYTES)?;
		self.gql_live_acks.try_reserve_exact(1).map_err(|error| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"GQL ACK witnesses",
				self.write_bytes as u64,
			)
			.with_technical_source(error)
		})?;
		self.acknowledge_live(&cursor)?;
		self.gql_live_acks.push(GqlLiveAck {
			attachment,
			cursor: evidence,
		});
		self.write_bytes += bytes;
		Ok(())
	}

	/// Borrowed evidence permits session revocation checks before COMMIT.
	pub(in crate::datastore) fn gql_live_acks(&self) -> &[GqlLiveAck] {
		&self.gql_live_acks
	}

	/// Rechecks issuance during optimistic validation and final locked
	/// validation.
	pub(super) fn validate_gql_live_acks(&self) -> FerrosGrynnResult<()> {
		for ack in &self.gql_live_acks {
			self.store.inner.live_consumers.validate_ack(ack.attachment.key, &ack.cursor)?;
		}
		Ok(())
	}
}

#[cfg(test)]
/// Regression tests for live ack behavior.
mod tests {
	use crate::{
		datastore::{ClassSchema, Datastore},
		gql_status::{FerrosGrynnError, FerrosGrynnResult},
		graph::AtomKind,
		live::{LiveCursor, LiveNext, LiveOptions, LiveQuery, LiveSource},
		values::PropertyMap,
	};

	/// Historical replay tokens retain at most a confirmed and an issued
	/// witness.
	#[test]
	fn live_gql_ack_historical_witnesses_are_bounded_and_rollback_aware() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
			.assert_ok();
		setup.commit().assert_ok();
		for index in 0..32 {
			let mut write = store.begin();
			write
				.create_atom(format!("person:p{index}").parse().assert_ok(), PropertyMap::new())
				.assert_ok();
			write.commit().assert_ok();
		}
		let mut handle = store
			.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
			.assert_ok();
		let confirmed = handle.cursor().clone();
		let attachment = handle.inner.gql_attachment();
		let mut write = store.begin();
		write.create_atom("person:next".parse().assert_ok(), PropertyMap::new()).assert_ok();
		write.commit().assert_ok();
		let LiveNext::Delivery(delivery) = handle.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("witness delivery missing"));
				test_result.assert_ok();
			};
		};
		let mut tx = store.begin();
		for stamp in 0..=confirmed.versionstamp().get() {
			let token = LiveCursor::new(
				confirmed.database_id(),
				confirmed.live_id(),
				confirmed.version(),
				stamp.into(),
			)
			.assert_ok();
			tx.acknowledge_gql_live(attachment, token).assert_ok();
			assert_eq!(tx.gql_live_acks().len(), 1);
			assert_eq!(tx.gql_live_acks()[0].cursor, confirmed);
		}
		let savepoint = tx.gql_savepoint();
		tx.acknowledge_gql_live(attachment, delivery.token().clone()).assert_ok();
		assert_eq!(tx.gql_live_acks().len(), 2);
		let bytes = tx.write_set_bytes();
		for _ in 0..100 {
			tx.acknowledge_gql_live(attachment, delivery.token().clone()).assert_ok();
		}
		assert_eq!(tx.write_set_bytes(), bytes);
		tx.restore_gql_savepoint(savepoint);
		assert_eq!(tx.gql_live_acks().len(), 1);
		assert_eq!(tx.live_info(confirmed.live_id()).assert_ok().cursor(), &confirmed);
	}
}
