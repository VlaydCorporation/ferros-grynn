//! Bounded production commit probe; stdout contains raw timings, never a
//! verdict. Usage: q05_indexed_commit BACKEND INDEX SCOPE ENTRY_ROUTE LAYOUT
//! BATCH DATABASE_ROOT [SAMPLES]. The seven bindings and acceptance protocol
//! are specified in docs/reports/q05_full_matrix_protocol_v3.md. The supervisor
//! must acknowledge the flushed completion footer with one newline on stdin.

use std::{
	collections::BTreeSet,
	fs,
	io::{self, Read, Write},
	path::PathBuf,
	time::{Duration, Instant},
};

use ferros_grynn::{
	datastore::{
		CheckpointMode,
		ClassField,
		ClassIndex,
		ClassIndexKind,
		ClassSchema,
		Datastore,
		ImportOptions,
		IndexScope,
		IoMode,
		LocalOptions,
		Transaction,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphKind, GraphRef},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Type, Uuid, Value},
};

/// Fixed admitted dataset shared by the initial baseline and candidate.
const ROWS: usize = 10_000;
/// Unmeasured commits before the fixed request sample sequence.
const WARMUP: usize = 8;

#[path = "q05_indexed_commit/config.rs"]
mod config;
#[path = "q05_indexed_commit/fixture.rs"]
mod fixture;

use config::{Config, IndexKind};
use fixture::Fixture;

/// Runs exactly one query and preserves statement-level failures.
fn query(
	store: &Datastore,
	scope: &str,
	hint: &str,
	parameters: &Object,
) -> FerrosGrynnResult<Value> {
	execute_value(
		store,
		&format!("SELECT VALUE id FROM item {scope} {hint} WHERE value = $value ORDER BY id;"),
		parameters,
	)
}

/// Preserves statement errors and requires a result for the single statement.
fn execute_value(
	store: &Datastore,
	statement: &str,
	parameters: &Object,
) -> FerrosGrynnResult<Value> {
	store
		.execute(statement, parameters)?
		.into_iter()
		.next()
		.ok_or_else(|| invalid("query returned no statement"))?
		.result
		.map(|outcome| outcome.into_parts().0)
}

/// Verifies backend authority rather than trusting the requested mode.
fn verify_mode(store: &Datastore, expected: Option<IoMode>) -> FerrosGrynnResult<()> {
	if store.local_storage_status().map(|status| status.options().io_mode) != expected {
		return Err(invalid("effective backend mode differs"));
	}
	Ok(())
}

/// Preserves the native I/O cause instead of turning a failed run into a
/// sample.
fn io_error(error: io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure("Q05 measurement fixture").with_technical_source(error)
}

/// Constructs a typed harness admission or oracle failure.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(reason)
}

/// Keeps the process alive until its supervisor finishes the last control
/// observation, outside all timers and after the reopen oracle.
///
/// # Errors
/// Returns an error if the supervisor disconnects, sends an invalid token or
/// the input cannot be read.
fn read_completion_ack(input: &mut impl Read) -> FerrosGrynnResult<()> {
	let mut token = [0];
	input.read_exact(&mut token).map_err(io_error)?;
	if token != *b"\n" {
		return Err(invalid("Q05 supervisor completion acknowledgement differs"));
	}
	Ok(())
}

/// Emits all raw observations; only an external complete-pair analysis may
/// decide a gate.
fn main() -> FerrosGrynnResult<()> {
	let args = std::env::args().skip(1).collect::<Vec<_>>();
	let config = Config::parse(&args)?;
	let mut fixture = Fixture::create(&config)?;
	let mut output = io::BufWriter::new(io::stdout().lock());
	writeln!(
		output,
		"#backend={:?},index={:?},fallback={},batch={},scope={},entry_route={}",
		config.io_mode,
		config.kind,
		config.fallback,
		config.batch,
		config.scope(),
		config.entry
	)
	.map_err(io_error)?;
	writeln!(output, "#q05_format=2,rows={ROWS},warmup={WARMUP},samples={}", config.samples)
		.map_err(io_error)?;
	writeln!(
		output,
		"phase,sample,commit_ns,staging_ns,checkpoint_ns,new_index_files,wal_unbackfilled_pages"
	)
	.map_err(io_error)?;
	for sample in 0..WARMUP + config.samples {
		let checkpoint = fixture.checkpoint()?;
		let before = fixture.index_paths(&config)?;
		let (commit, staging) = fixture.commit(&config, sample + 1)?;
		let after = fixture.index_paths(&config)?;
		let dirty = fixture
			.store()?
			.local_storage_status()
			.map_or(0, |status| status.wal_unbackfilled_pages());
		fixture.probe(&config, sample + 1)?;
		writeln!(
			output,
			"{},{sample},{},{},{},{},{}",
			if sample < WARMUP {
				"warmup"
			} else {
				"measure"
			},
			commit.as_nanos(),
			staging.as_nanos(),
			checkpoint.as_nanos(),
			after.difference(&before).count(),
			dirty
		)
		.map_err(io_error)?;
		output.flush().map_err(io_error)?;
	}
	fixture.validate(&config, WARMUP + config.samples)?;
	let reopened = fixture.reopen(&config)?;
	fixture.validate(&config, WARMUP + config.samples)?;
	writeln!(output, "#reopen_ns={}", reopened.as_nanos()).map_err(io_error)?;
	output.flush().map_err(io_error)?;
	read_completion_ack(&mut io::stdin().lock())?;
	fixture.finish()
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Missing or malformed supervisor input must never admit a completed run.
	#[test]
	fn completion_requires_the_exact_supervisor_ack() -> FerrosGrynnResult<()> {
		read_completion_ack(&mut &b"\n"[..])?;
		assert!(read_completion_ack(&mut &b"x"[..]).is_err());
		assert!(read_completion_ack(&mut &b""[..]).is_err());
		Ok(())
	}

	/// Rejected bindings must never allocate a fixture or silently change
	/// scope.
	#[test]
	fn invalid_workload_is_rejected_before_fixture_creation() {
		for (kind, scope, entry, batch) in [
			("primary", "effective", "overlay", "32"),
			("standard", "base", "overlay", "32"),
			("unique", "effective", "base", "32"),
			("standard", "base", "base", "0"),
			("unique", "base", "base", "257"),
		] {
			let args = ["ram", kind, scope, entry, "stable", batch, "."].map(str::to_owned);
			assert!(Config::parse(&args).is_err());
		}
	}

	/// Every declared binding remains admitted without depending on a database.
	#[test]
	fn exact_seven_bindings_are_admitted() -> FerrosGrynnResult<()> {
		for (kind, scope, entry) in [
			("primary", "base", "lifecycle"),
			("standard", "base", "base"),
			("unique", "base", "base"),
			("standard", "effective", "base_inherited"),
			("unique", "effective", "base_inherited"),
			("standard", "effective", "overlay"),
			("unique", "effective", "overlay"),
		] {
			let args = ["ram", kind, scope, entry, "stable", "32", "."].map(str::to_owned);
			assert_eq!(Config::parse(&args)?.scope().to_ascii_lowercase(), scope);
		}
		Ok(())
	}

	/// Primary stable identities stay fixed; fallback keys remain ordered and
	/// bounded.
	#[test]
	fn primary_target_geometry_preserves_anchors_and_phase_order() -> FerrosGrynnResult<()> {
		let args =
			["ram", "primary", "base", "lifecycle", "fallback", "256", "."].map(str::to_owned);
		let mut config = Config::parse(&args)?;
		for row in 1..=256 {
			let before = config.record(row, 0)?;
			let after = config.record(row, 1)?;
			let ferros_grynn::values::RecordIdPart::String(text) = before.id() else {
				return Err(invalid("Primary target is not a string"));
			};
			assert_eq!(text.len(), 8200);
			assert!(
				config.record(row - 1, 1)? < before
					&& before < after
					&& after < config.record(row + 1, 0)?
			);
		}
		assert_eq!(config.record(0, 0)?, config.record(0, 1)?);
		assert_eq!(config.record(257, 0)?, config.record(257, 1)?);
		config.fallback = false;
		assert_eq!(config.record(1, 0)?, config.record(1, 1)?);
		Ok(())
	}
}
