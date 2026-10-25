//! Admission and deterministic identities/values for the seven Q-05 workloads.

use super::*;

/// Primary is a lifecycle workload, not a public scalar index kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IndexKind {
	/// Global RecordId-to-GlobalAtomId tree.
	Primary,
	/// Scalar posting lists.
	Standard,
	/// Scalar single-owner keys.
	Unique,
}

impl IndexKind {
	/// Returns a declarative scalar kind only when the fixture needs one.
	pub(super) fn scalar(self) -> Option<ClassIndexKind> {
		match self {
			Self::Primary => None,
			Self::Standard => Some(ClassIndexKind::Standard),
			Self::Unique => Some(ClassIndexKind::Unique),
		}
	}
}

/// One admitted workload; requested backend options never change defaults.
pub(super) struct Config {
	/// RAM has no physical leaf publication path.
	pub(super) io_mode: Option<IoMode>,
	/// Selected binding observed by the untimed file oracle.
	pub(super) kind: IndexKind,
	/// Exactly one of the seven scope/entry combinations from the protocol.
	pub(super) entry: &'static str,
	/// Cross a guarded layout boundary instead of preserving it.
	pub(super) fallback: bool,
	/// Number of final changed atoms, bounded before fixture allocation.
	pub(super) batch: usize,
	/// Full measured request count; confirmation requires 32 externally.
	pub(super) samples: usize,
	/// Parent of fresh directories owned by this process.
	pub(super) root: PathBuf,
}

impl Config {
	/// Validates the complete binding and dimensions before allocating data.
	///
	/// # Errors
	/// Returns an argument diagnostic for any undeclared workload or dimension.
	pub(super) fn parse(args: &[String]) -> FerrosGrynnResult<Self> {
		if !(7..=8).contains(&args.len()) {
			return Err(invalid(
				"expected BACKEND INDEX SCOPE ENTRY_ROUTE LAYOUT BATCH DATABASE_ROOT [SAMPLES]",
			));
		}
		let io_mode = match args[0].as_str() {
			"ram" => None,
			"sync" => Some(IoMode::Sync),
			"native" => Some(IoMode::NativeAsync),
			_ => return Err(invalid("backend must be ram, sync or native")),
		};
		let kind = match args[1].as_str() {
			"primary" => IndexKind::Primary,
			"standard" => IndexKind::Standard,
			"unique" => IndexKind::Unique,
			_ => return Err(invalid("index must be primary, standard or unique")),
		};
		let entry = match (kind, args[2].as_str(), args[3].as_str()) {
			(IndexKind::Primary, "base", "lifecycle") => "lifecycle",
			(IndexKind::Standard | IndexKind::Unique, "base", "base") => "base",
			(IndexKind::Standard | IndexKind::Unique, "effective", "base_inherited") => {
				"base_inherited"
			}
			(IndexKind::Standard | IndexKind::Unique, "effective", "overlay") => "overlay",
			_ => return Err(invalid("unsupported scope or entry route")),
		};
		let fallback = match args[4].as_str() {
			"stable" => false,
			"fallback" => true,
			_ => return Err(invalid("layout must be stable or fallback")),
		};
		let batch = args[5].parse::<usize>().map_err(|_error| invalid("invalid batch"))?;
		let samples = match args.get(7) {
			Some(value) => value.parse::<usize>().map_err(|_error| invalid("invalid samples"))?,
			None => 32,
		};
		if ![1, 32, 256].contains(&batch) || !(1..=4096).contains(&samples) {
			return Err(invalid("batch must be 1/32/256 and samples must be 1..=4096"));
		}
		Ok(Self {
			io_mode,
			kind,
			entry,
			fallback,
			batch,
			samples,
			root: PathBuf::from(&args[6]),
		})
	}

	/// Scope is derived from the validated route rather than stored twice.
	pub(super) fn effective(&self) -> bool {
		matches!(self.entry, "base_inherited" | "overlay")
	}

	/// CSV spelling remains part of the versioned raw format.
	pub(super) fn scope(&self) -> &'static str {
		if self.effective() {
			"Effective"
		} else {
			"Base"
		}
	}

	/// Returns a target row without touching immutable anchors.
	pub(super) fn target(&self, ordinal: usize) -> usize {
		if self.kind == IndexKind::Primary {
			ordinal + 1
		} else if self.fallback {
			ordinal
				+ if self.kind == IndexKind::Standard {
					8
				} else {
					0
				}
		} else if self.kind == IndexKind::Standard {
			ordinal * 2 + 1
		} else {
			ordinal + 1
		}
	}

	/// Long targets remain singleton leaves in both bulk and streaming layouts.
	///
	/// # Errors
	/// Propagates RecordId construction diagnostics.
	pub(super) fn record(&self, row: usize, round: usize) -> FerrosGrynnResult<RecordId> {
		let value = if self.kind == IndexKind::Primary && row <= 257 {
			let phase = if self.fallback && (1..=self.batch).contains(&row) && round % 2 == 1 {
				'b'
			} else {
				'a'
			};
			// 8208 canonical key bytes make two entries exceed a 16 KiB page.
			// Row zero anchors the first long key after the short-key prefix.
			Value::from(format!("p{row:08}{phase}{}", "x".repeat(8190)))
		} else {
			Value::from(row as i64)
		};
		RecordId::from_value("item", value)
	}

	/// Expected final property value, independent of index and publication
	/// code.
	pub(super) fn value(&self, row: usize, round: usize) -> i64 {
		if self.kind == IndexKind::Primary {
			return (row * 4
				+ if (1..=self.batch).contains(&row) {
					round
				} else {
					0
				}) as i64;
		}
		if self.fallback {
			if self.kind == IndexKind::Standard {
				return if row < 8 || (round % 2 == 1 && (8..8 + self.batch).contains(&row)) {
					0
				} else {
					(row * 4) as i64
				};
			}
			return if round % 2 == 1 && row < self.batch {
				if row == 0 {
					-4
				} else {
					(row * 4 + 1) as i64
				}
			} else {
				(row * 4) as i64
			};
		}
		if self.kind == IndexKind::Standard {
			let group = row / 2;
			return (if row % 2 == 1 && group < self.batch && round % 2 == 1 {
				group ^ 1
			} else {
				group
			} * 4) as i64;
		}
		if (1..=self.batch).contains(&row) {
			if self.batch == 1 {
				return 4 + (round % 2) as i64;
			}
			return (((row - 1 + round) % self.batch + 1) * 4) as i64;
		}
		(row * 4) as i64
	}
}
