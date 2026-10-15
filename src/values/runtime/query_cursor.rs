use std::fmt::{self, Debug, Display, Formatter};

/// Opaque process-local continuation token owned by one GrynnQL session.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueryCursor {
	/// Session associated with this query cursor.
	session: u64,
	/// Identifier used to locate this value within its owning subsystem.
	id: u64,
}

impl QueryCursor {
	/// Creates a new `QueryCursor` value.
	pub(crate) const fn new(session: u64, id: u64) -> Self {
		Self {
			session,
			id,
		}
	}

	/// Returns the session result.
	pub(crate) const fn session(self) -> u64 {
		self.session
	}

	/// Returns the id result.
	pub(crate) const fn id(self) -> u64 {
		self.id
	}
}

impl Debug for QueryCursor {
	/// Formats `QueryCursor` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "QueryCursor({:016x}:{:016x})", self.session, self.id)
	}
}

impl Display for QueryCursor {
	/// Formats `QueryCursor` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "cursor({:016x}:{:016x})", self.session, self.id)
	}
}
