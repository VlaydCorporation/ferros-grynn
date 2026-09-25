use std::fmt::{self, Debug, Display, Formatter};

/// Opaque process-local continuation token owned by one GrynnQL session.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueryCursor {
	session: u64,
	id: u64,
}

impl QueryCursor {
	pub(crate) const fn new(session: u64, id: u64) -> Self {
		Self {
			session,
			id,
		}
	}

	pub(crate) const fn session(self) -> u64 {
		self.session
	}

	pub(crate) const fn id(self) -> u64 {
		self.id
	}
}

impl Debug for QueryCursor {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		write!(formatter, "QueryCursor({:016x}:{:016x})", self.session, self.id)
	}
}

impl Display for QueryCursor {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		write!(formatter, "cursor({:016x}:{:016x})", self.session, self.id)
	}
}
