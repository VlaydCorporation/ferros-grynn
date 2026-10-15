#![cfg(test)]

//! Independent optional-record state machine compared with public NET/FULL
//! reads.

/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use ferros_grynn::{
	changefeed::{
		ChangeIdentity,
		ChangefeedConfig,
		ChangefeedReadItem,
		ChangefeedReadMode,
		ChangefeedReadOptions,
		ChangefeedSince,
		ChangefeedSource,
	},
	datastore::{ClassField, ClassSchema, Datastore},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	values::{Duration, PropertyMap, RecordId, Type, Value},
};

/// Outcome of one independent record-state operation.
enum Transition {
	/// The operation is invalid for the current state and leaves no history
	/// entry.
	Rejected,
	/// The operation succeeds and removes the record.
	Deleted,
	/// The operation succeeds and stores the supplied integer value.
	Stored(
		/// The operation succeeds and stores the supplied integer value.
		i64,
	),
}

/// Computes an oracle transition without consulting datastore roots.
fn transition(state: Option<i64>, operation: usize) -> Transition {
	match (operation, state) {
		(0, None) => Transition::Stored(1),
		(1, Some(_)) => Transition::Stored(0),
		(2, Some(_)) => Transition::Stored(1),
		(3, Some(_)) => Transition::Deleted,
		_ => Transition::Rejected,
	}
}

/// Public image projection; absent images remain distinct from property values.
fn value(image: Option<&ferros_grynn::changefeed::ChangeImage>) -> FerrosGrynnResult<Option<i64>> {
	image
		.map(|image| {
			let property = image
				.properties()
				.get("value")
				.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle image value absent"))?;
			match property {
				Value::Number(number) => i64::try_from(*number),
				_ => Err(FerrosGrynnError::invalid_argument("oracle image is not integer")),
			}
		})
		.transpose()
}

/// Exhausts all length-three sequences of create/set0/set1/delete from both
/// states.
#[test]
fn public_changefeed_net_full_match_independent_exhaustive_state_machine() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::Int)])
				.assert_ok(),
		)
		.assert_ok();
	let configuration =
		ChangefeedConfig::new(Duration::new(jiff::Span::new().days(1))).assert_ok().with_full(true);
	let feed =
		setup.configure_changefeed(ChangefeedSource::Class(class), configuration).assert_ok();
	setup.commit().assert_ok();
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle field absent"))
		.assert_ok();
	for original in [false, true] {
		for initial in [None, Some(0_i64)] {
			let initial_tag = initial.is_some();
			for sequence in 0..64_usize {
				let record: RecordId =
					format!("item:r{original}_{initial_tag}_{sequence}").parse().assert_ok();
				let mut initialize = store.begin();
				if let Some(initial) = initial {
					let mut properties = PropertyMap::new();
					properties.insert(key, initial);
					initialize.create_atom(record.clone(), properties).assert_ok();
				}
				let since = initialize.commit().assert_ok().versionstamp();
				let mut tx = store.begin();
				let mut state = initial;
				let mut history = Vec::new();
				for shift in 0..3 {
					let operation = (sequence >> (2 * shift)) & 3;
					let expected = transition(state, operation);
					let actual = match operation {
						0 => {
							let mut properties = PropertyMap::new();
							properties.insert(key, 1);
							tx.create_atom(record.clone(), properties).map(|_| ())
						}
						1 | 2 => tx
							.set_base_property(&record, key, Value::from((operation - 1) as i64))
							.map(|_| ()),
						_ => tx.delete_atom(&record),
					};
					assert_eq!(
						actual.is_ok(),
						!matches!(expected, Transition::Rejected),
						"sequence={sequence}, initial={initial:?}, operation={operation}"
					);
					match expected {
						Transition::Rejected => {}
						Transition::Deleted => {
							history.push((state, None));
							state = None;
						}
						Transition::Stored(after) => {
							history.push((state, Some(after)));
							state = Some(after);
						}
					}
				}
				// Final validated policy applies to every earlier successful
				// operation.
				tx.configure_changefeed(
					ChangefeedSource::Class(class),
					configuration.with_include_original(original),
				)
				.assert_ok();
				let stamp = tx.commit().assert_ok().versionstamp();
				for mode in [ChangefeedReadMode::Net, ChangefeedReadMode::Full] {
					let expected = if mode == ChangefeedReadMode::Full {
						history.clone()
					} else if initial != state {
						vec![(initial, state)]
					} else {
						Vec::new()
					};
					let mut tx = store.begin();
					let mut reader = tx
						.changes(
							feed,
							ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(since))
								.with_mode(mode),
						)
						.assert_ok();
					let item = reader.next_batch().assert_ok();
					if expected.is_empty() {
						assert!(item.is_none(), "sequence={sequence}, mode={mode:?}");
					} else {
						let Some(ChangefeedReadItem::Commit(batch)) = item else {
							return {
								let test_result: FerrosGrynnResult<()> =
									Err(FerrosGrynnError::invalid_argument("oracle batch absent"));
								test_result.assert_ok();
							};
						};
						assert_eq!(batch.versionstamp(), stamp);
						assert_eq!(batch.entries().len(), expected.len());
						for (ordinal, (entry, (before, after))) in
							batch.entries().iter().zip(expected).enumerate()
						{
							assert_eq!(entry.ordinal(), ordinal as u64);
							if mode == ChangefeedReadMode::Full {
								assert_eq!(entry.operation(), ordinal as u64);
							}
							for image in [entry.before(), entry.after()].into_iter().flatten() {
								assert!(
									matches!(image.identity(), ChangeIdentity::Record(atom) if atom.id() == &record && atom.kind() == AtomKind::Node)
								);
							}
							assert_eq!(
								(
									value(entry.before()).assert_ok(),
									value(entry.after()).assert_ok()
								),
								(before, after),
								"sequence={sequence}, initial={initial:?}, mode={mode:?}"
							);
						}
						assert!(reader.next_batch().assert_ok().is_none());
					}
				}
			}
		}
	}
}
