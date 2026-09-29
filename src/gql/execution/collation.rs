//! Unicode order-key construction shared by SELECT and MATCH.

use std::sync::OnceLock;

use icu_collator::{
	Collator,
	CollatorBorrowed,
	CollatorPreferences,
	options::{CollatorOptions, Strength},
	preferences::CollationNumericOrdering,
};

use super::LogicalOrder;
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Bytes, Value},
};

static PRIMARY: OnceLock<Option<CollatorBorrowed<'static>>> = OnceLock::new();
static NUMERIC: OnceLock<Option<CollatorBorrowed<'static>>> = OnceLock::new();
static PRIMARY_NUMERIC: OnceLock<Option<CollatorBorrowed<'static>>> = OnceLock::new();

/// Converts a runtime string into a byte-comparable ICU4X key while retaining
/// the original UTF-8 string as the canonical tie-break.
pub(super) fn order_key(value: Value, order: &LogicalOrder) -> FerrosGrynnResult<Value> {
	if !order.collate && !order.numeric {
		return Ok(value);
	}
	let Value::String(text) = value else {
		return match value {
			Value::None | Value::Null => Ok(value),
			value => Err(FerrosGrynnError::invalid_argument(format!(
				"ORDER BY COLLATE/NUMERIC expects a string, null, or none; received {}.",
				value.kind()
			))),
		};
	};
	let collator = collator(order.collate, order.numeric)?;
	let mut key = Vec::new();
	collator.write_sort_key_to(text.as_str(), &mut key).map_err(|error| {
		FerrosGrynnError::invalid_argument(format!(
			"failed to build Unicode collation key: {error}"
		))
	})?;
	Ok(Value::Array(vec![Value::Bytes(Bytes::from(key)), Value::String(text)].into()))
}

fn collator(primary: bool, numeric: bool) -> FerrosGrynnResult<&'static CollatorBorrowed<'static>> {
	let slot = match (primary, numeric) {
		(true, true) => &PRIMARY_NUMERIC,
		(true, false) => &PRIMARY,
		(false, true) => &NUMERIC,
		(false, false) => {
			return Err(FerrosGrynnError::invalid_argument(
				"collation was requested without COLLATE or NUMERIC",
			));
		}
	};
	slot.get_or_init(|| build_collator(primary, numeric)).as_ref().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("ICU4X root collation data is unavailable")
	})
}

fn build_collator(primary: bool, numeric: bool) -> Option<CollatorBorrowed<'static>> {
	let mut preferences = CollatorPreferences::default();
	if numeric {
		preferences.numeric_ordering = Some(CollationNumericOrdering::True);
	}
	let mut options = CollatorOptions::default();
	if primary {
		options.strength = Some(Strength::Primary);
	}
	Collator::try_new(preferences, options).ok()
}
