//! Independent optional-record state machine compared with public NET/FULL
//! reads.

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

/// One transition knows only optional integer state, without datastore roots.
fn transition(state: Option<i64>, operation: usize) -> Option<Option<i64>> {
	match (operation, state) {
		(0, None) => Some(Some(1)),
		(1, Some(_)) => Some(Some(0)),
		(2, Some(_)) => Some(Some(1)),
		(3, Some(_)) => Some(None),
		_ => None,
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
fn public_changefeed_net_full_match_independent_exhaustive_state_machine() -> FerrosGrynnResult<()>
{
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"value",
			Type::Int,
		)])?)?;
	let configuration =
		ChangefeedConfig::new(Duration::new(jiff::Span::new().days(1)))?.with_full(true);
	let feed = setup.configure_changefeed(ChangefeedSource::Class(class), configuration)?;
	setup.commit()?;
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle field absent"))?;
	for original in [false, true] {
		for initial in [None, Some(0_i64)] {
			let initial_tag = initial.is_some();
			for sequence in 0..64_usize {
				let record: RecordId =
					format!("item:r{original}_{initial_tag}_{sequence}").parse()?;
				let mut initialize = store.begin();
				if let Some(initial) = initial {
					let mut properties = PropertyMap::new();
					properties.insert(key, initial);
					initialize.create_atom(record.clone(), properties)?;
				}
				let since = initialize.commit()?.versionstamp();
				let mut tx = store.begin();
				let mut state = initial;
				let mut history = Vec::new();
				for shift in 0..3 {
					let operation = sequence >> (2 * shift) & 3;
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
						expected.is_some(),
						"sequence={sequence}, initial={initial:?}, operation={operation}"
					);
					if let Some(after) = expected {
						history.push((state, after));
						state = after;
					}
				}
				// Final validated policy applies to every earlier successful
				// operation.
				tx.configure_changefeed(
					ChangefeedSource::Class(class),
					configuration.with_include_original(original),
				)?;
				let stamp = tx.commit()?.versionstamp();
				for mode in [ChangefeedReadMode::Net, ChangefeedReadMode::Full] {
					let expected = if mode == ChangefeedReadMode::Full {
						history.clone()
					} else if initial != state {
						vec![(initial, state)]
					} else {
						Vec::new()
					};
					let mut tx = store.begin();
					let mut reader = tx.changes(
						feed,
						ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(since))
							.with_mode(mode),
					)?;
					let item = reader.next_batch()?;
					if expected.is_empty() {
						assert!(item.is_none(), "sequence={sequence}, mode={mode:?}");
					} else {
						let Some(ChangefeedReadItem::Commit(batch)) = item else {
							return Err(FerrosGrynnError::invalid_argument("oracle batch absent"));
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
								(value(entry.before())?, value(entry.after())?),
								(before, after),
								"sequence={sequence}, initial={initial:?}, mode={mode:?}"
							);
						}
						assert!(reader.next_batch()?.is_none());
					}
				}
			}
		}
	}
	Ok(())
}
