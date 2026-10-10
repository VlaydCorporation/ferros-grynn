//! Owned comparison recipes; ordinary SELECT owns key resolution and sorting.

use super::LiveExpr;
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Text,
};

/// One deterministic ORDER key, independent of syntax and executable IR.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveOrderKey {
	/// A source-qualified logical expression.
	Expression(LiveExpr),
	/// A single identifier, resolved as a projected name or source column by
	/// ordinary SELECT. Dots remain literal identifier bytes.
	Name(Text),
}

/// Direction of one deterministic comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveOrderDirection {
	/// Smaller keys precede larger keys.
	Ascending,
	/// Larger keys precede smaller keys.
	Descending,
}

/// Placement of missing and null values in one comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveNullsOrder {
	/// Null values precede non-null values.
	First,
	/// Null values follow non-null values.
	Last,
}

/// One owned ORDER recipe with ordinary SELECT comparison semantics.
/// Defaults are ascending, nulls last, without collation or numeric strings.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct LiveOrder {
	/// Symbolic key, resolved against each source schema.
	pub(crate) key: LiveOrderKey,
	/// Ascending or descending comparison.
	pub(crate) direction: LiveOrderDirection,
	/// Explicit missing/null placement.
	pub(crate) nulls: LiveNullsOrder,
	/// Root Unicode collation with primary strength.
	pub(crate) collate: bool,
	/// Numeric comparison of digit sequences in strings.
	pub(crate) numeric: bool,
}

impl LiveOrder {
	/// Builds an expression key; type and PURE effect checks run at
	/// registration.
	pub fn new(expression: LiveExpr) -> Self {
		Self {
			key: LiveOrderKey::Expression(expression),
			direction: LiveOrderDirection::Ascending,
			nulls: LiveNullsOrder::Last,
			collate: false,
			numeric: false,
		}
	}

	/// Builds one identifier key, including a projected aggregate alias.
	///
	/// # Errors
	/// Returns an argument error for an empty name or a NUL byte. Name/type
	/// resolution runs during registration through ordinary SELECT.
	pub fn by_name<Name: Into<Text>>(name: Name) -> FerrosGrynnResult<Self> {
		let name = name.into();
		if name.is_empty() || name.as_str().contains('\0') {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE ORDER name is empty or contains NUL",
			));
		}
		Ok(Self {
			key: LiveOrderKey::Name(name),
			direction: LiveOrderDirection::Ascending,
			nulls: LiveNullsOrder::Last,
			collate: false,
			numeric: false,
		})
	}

	/// Selects ascending or descending comparison.
	pub const fn with_direction(mut self, direction: LiveOrderDirection) -> Self {
		self.direction = direction;
		self
	}

	/// Selects missing/null placement independently of direction.
	pub const fn with_nulls(mut self, nulls: LiveNullsOrder) -> Self {
		self.nulls = nulls;
		self
	}

	/// Enables or disables ordinary SELECT Unicode collation.
	pub const fn with_collation(mut self, enabled: bool) -> Self {
		self.collate = enabled;
		self
	}

	/// Enables or disables numeric comparison of string digit sequences.
	pub const fn with_numeric(mut self, enabled: bool) -> Self {
		self.numeric = enabled;
		self
	}

	/// Borrows the logical key without copying its expression or name.
	pub const fn key(&self) -> &LiveOrderKey {
		&self.key
	}

	/// Returns comparison direction.
	pub const fn direction(&self) -> LiveOrderDirection {
		self.direction
	}

	/// Returns missing/null placement.
	pub const fn nulls(&self) -> LiveNullsOrder {
		self.nulls
	}

	/// Returns whether Unicode collation is enabled.
	pub const fn collation(&self) -> bool {
		self.collate
	}

	/// Returns whether numeric string comparison is enabled.
	pub const fn numeric(&self) -> bool {
		self.numeric
	}
}
