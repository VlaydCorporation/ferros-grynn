//! Final capture policies and RAM snapshot ownership through public operations.

use super::*;
use crate::{
	datastore::{ClassSchema, Datastore},
	graph::AtomKind,
	live::{LiveExpr, LiveOptions, LiveProjection, LiveQuery},
	values::{PropertyMap, RecordId, Value},
};

/// Enabling capture after DML applies to the whole commit; disabling leaves a
/// durable epoch and never persists the volatile tail as historical results.
#[test]
fn live_history_final_policy_epochs_and_old_snapshots() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let key = setup.intern_property("value").assert_ok();
	let record: RecordId = "item:a".parse().assert_ok();
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(1))])).assert_ok();
	let id = setup
		.register_live(
			LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
				LiveExpr::Field(
					crate::live::LivePath::new(vec!["properties".into(), "value".into()])
						.assert_ok(),
				),
			)),
			LiveOptions::default(),
		)
		.assert_ok();
	let registered = setup.commit().assert_ok().versionstamp();
	assert!(store.inner.current_root().live_catalog().histories.is_empty());
	let initial = store.begin().live_info(id).assert_ok();
	assert!(initial.history().is_none());
	assert_eq!(initial.starting_after(), registered);
	assert_eq!(initial.snapshot(), registered);
	assert_eq!(initial.capture_source(), ChangefeedSource::Class(class));
	assert_eq!(initial.source_class(), Some(class));
	let mut enable = store.begin();
	enable.set_base_property(&record, key, Value::from(2)).assert_ok();
	let source = enable
		.configure_changefeed(
			ChangefeedSource::Class(class),
			crate::changefeed::ChangefeedConfig::from_nanoseconds(86_400_000_000_000).assert_ok(),
		)
		.assert_ok();
	let stamp = enable.commit().assert_ok().versionstamp().get();
	let old = store.inner.current_root();
	let mut snapshot_reader = store.begin();
	let public = snapshot_reader.live_info(id).assert_ok();
	let history = public
		.history()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("public history missing"))
		.assert_ok();
	assert_eq!(history.last_stamp().get(), stamp);
	assert_eq!(history.purged_through().get(), 0);
	assert!(history.purged_at().is_none());
	let policy = history.epochs()[0]
		.policy()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("public capture policy missing"))
		.assert_ok();
	assert_eq!(policy.source(), source);
	assert_eq!(policy.revision(), old.changefeeds().definitions[&source].version);
	assert_eq!(policy.retention().fixed_nanoseconds(), Some(86_400_000_000_000));
	let bytes = old
		.live_history()
		.get(&(id, stamp))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing historical result"))
		.assert_ok();
	let crate::live::LivePayload::Batch(batch) =
		crate::storage::live_history::decode(bytes, 8 * 1024 * 1024).assert_ok()
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("expected LIVE batch"));
			test_result.assert_ok();
		};
	};
	assert_eq!(batch.events()[0].before(), Some(&Value::from(1)));
	assert_eq!(batch.events()[0].after(), Some(&Value::from(2)));
	assert_eq!(
		old.live_catalog().histories[&id].epochs[0].policy.map(|policy| policy.source),
		Some(source)
	);
	let mut disable = store.begin();
	disable.set_base_property(&record, key, Value::from(3)).assert_ok();
	disable.drop_changefeed(ChangefeedSource::Class(class)).assert_ok();
	let disabled = disable.commit().assert_ok().versionstamp().get();
	let current = store.inner.current_root();
	assert!(!current.live_history().contains_key(&(id, disabled)));
	assert_eq!(current.live_catalog().prepared.len(), 1, "volatile changes remain queue eligible");
	assert_eq!(current.live_catalog().histories[&id].epochs.len(), 2);
	assert_eq!(current.live_catalog().histories[&id].epochs[1].policy, None);
	assert_eq!(current.live_catalog().histories[&id].last_stamp, stamp);
	assert_eq!(old.live_catalog().histories[&id].epochs.len(), 1);
	let latest = store.begin().live_info(id).assert_ok();
	assert_eq!(latest.snapshot().get(), disabled);
	let latest_history = latest
		.history()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("disabled public history missing"))
		.assert_ok();
	assert_eq!(latest_history.epochs().len(), 2);
	assert!(latest_history.epochs()[1].policy().is_none());
	assert_eq!(public.history().map(|history| history.epochs().len()), Some(1));
	assert_eq!(
		snapshot_reader.live_info(id).assert_ok().history().map(|history| history.epochs().len()),
		Some(1)
	);
	let mut reenable = store.begin();
	reenable
		.configure_changefeed(
			ChangefeedSource::Class(class),
			crate::changefeed::ChangefeedConfig::from_nanoseconds(86_400_000_000_000).assert_ok(),
		)
		.assert_ok();
	reenable.commit().assert_ok();
	assert_eq!(store.inner.current_root().live_catalog().histories[&id].epochs.len(), 3);
	let idle = store.begin();
	idle.commit().assert_ok();
	assert_eq!(
		store.inner.current_root().live_history().len(),
		1,
		"no duplicate old prepared group"
	);
}
