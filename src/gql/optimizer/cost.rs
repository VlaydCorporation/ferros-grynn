//! Fixed-point cardinality and cost primitives.

/// Bounded cardinality estimate carried between planning phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Cardinality {
	/// Proven lower bound.
	pub(crate) lower: u64,
	/// Most likely row count.
	pub(crate) expected: u64,
	/// Conservative upper bound.
	pub(crate) upper: u64,
}

impl Cardinality {
	/// Constructs a normalized estimate whose expected value lies in the
	/// inclusive bounds.
	pub(crate) const fn new(lower: u64, expected: u64, upper: u64) -> Self {
		let upper = if upper < lower {
			lower
		} else {
			upper
		};
		let expected = if expected < lower {
			lower
		} else if expected > upper {
			upper
		} else {
			expected
		};
		Self {
			lower,
			expected,
			upper,
		}
	}

	/// Returns an exact cardinality.
	pub(crate) const fn exact(rows: u64) -> Self {
		Self::new(rows, rows, rows)
	}

	/// Returns a conservative estimate when no usable statistic exists.
	pub(crate) const fn unknown(upper: u64) -> Self {
		Self::new(0, upper.div_ceil(2), upper)
	}
}

/// Saturating fixed-point cost broken down for EXPLAIN.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct PlanCost {
	/// One-time operator startup work.
	pub(crate) startup: u64,
	/// Sequential rows visited.
	pub(crate) sequential: u64,
	/// Random point or index lookups.
	pub(crate) random: u64,
	/// Expression evaluation work.
	pub(crate) expression: u64,
	/// Peak accounted memory units.
	pub(crate) memory: u64,
	/// Expected spill I/O units.
	pub(crate) spill: u64,
	/// Uncertainty and resource-risk penalty.
	pub(crate) risk: u64,
}

impl PlanCost {
	/// Returns the scalar value used for plan comparison.
	pub(crate) const fn total(self) -> u64 {
		self.startup
			.saturating_add(self.sequential)
			.saturating_add(self.random)
			.saturating_add(self.expression)
			.saturating_add(self.memory)
			.saturating_add(self.spill)
			.saturating_add(self.risk)
	}

	/// Adds two operator costs component by component.
	pub(crate) const fn saturating_add(self, other: Self) -> Self {
		Self {
			startup: self.startup.saturating_add(other.startup),
			sequential: self.sequential.saturating_add(other.sequential),
			random: self.random.saturating_add(other.random),
			expression: self.expression.saturating_add(other.expression),
			memory: self.memory.saturating_add(other.memory),
			spill: self.spill.saturating_add(other.spill),
			risk: self.risk.saturating_add(other.risk),
		}
	}
}
