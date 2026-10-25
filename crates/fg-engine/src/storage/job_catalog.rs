//! Checksummed `FGJB v1` maintenance-job catalog.

use std::{
	num::NonZeroUsize,
	str::FromStr,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use rapidhash::v3::rapidhash_v3;

use crate::{
	datastore::{
		ClassId,
		IndexId,
		JobId,
		MaintenanceAnalyzeMode,
		MaintenanceJobInfo,
		MaintenanceJobKind,
		MaintenanceJobState,
		MaintenanceTarget,
		SchemaVersion,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult, StatusCode},
	graph::GraphId,
	values::Uuid,
};

/// Configured magic value.
const MAGIC: [u8; 4] = *b"FGJB";
/// Configured version value.
const VERSION: u16 = 1;
/// Configured header bytes value.
const HEADER_BYTES: usize = 32;
/// Byte offset of checksum in its encoded layout.
const CHECKSUM_OFFSET: usize = 24;
/// Configured max job catalog bytes value.
pub(super) const MAX_JOB_CATALOG_BYTES: usize = 16 * 1024 * 1024;
/// Configured max job count value.
const MAX_JOB_COUNT: usize = 16_384;
/// Configured max diagnostic bytes value.
const MAX_DIAGNOSTIC_BYTES: usize = 4096;

/// Durable job records plus the metadata-COW authority clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JobCatalog {
	/// Generation identifying this version of the data.
	pub(super) generation: u64,
	/// Jobs associated with this job catalog.
	pub(super) jobs: Vec<MaintenanceJobInfo>,
}

impl Default for JobCatalog {
	/// Constructs the default initial state of `JobCatalog`.
	fn default() -> Self {
		Self {
			generation: 1,
			jobs: Vec::new(),
		}
	}
}

impl JobCatalog {
	/// Encodes canonical `FGJB v1` bytes.
	pub(super) fn encode(&self) -> FerrosGrynnResult<Vec<u8>> {
		if self.generation == 0 || self.jobs.len() > MAX_JOB_COUNT {
			return Err(invalid("job catalog clock or record count is outside its format domain"));
		}
		let mut output = Vec::with_capacity(HEADER_BYTES + self.jobs.len() * 160);
		output.extend_from_slice(&MAGIC);
		output.extend_from_slice(&VERSION.to_le_bytes());
		output.extend_from_slice(&0_u16.to_le_bytes());
		output.extend_from_slice(&self.generation.to_le_bytes());
		write_u32(&mut output, self.jobs.len(), "job count")?;
		output.extend_from_slice(&0_u32.to_le_bytes());
		output.extend_from_slice(&0_u64.to_le_bytes());
		for job in &self.jobs {
			encode_job(&mut output, job)?;
		}
		if output.len() > MAX_JOB_CATALOG_BYTES {
			return Err(invalid("job catalog exceeds its format limit"));
		}
		let payload_len = u32::try_from(output.len() - HEADER_BYTES)
			.map_err(|_error| invalid("job catalog payload length overflows"))?;
		output[20..24].copy_from_slice(&payload_len.to_le_bytes());
		let checksum = catalog_checksum(&output);
		output[CHECKSUM_OFFSET..HEADER_BYTES].copy_from_slice(&checksum.to_le_bytes());
		Ok(output)
	}

	/// Decodes and validates exact `FGJB v1` bytes.
	pub(super) fn decode(bytes: &[u8]) -> FerrosGrynnResult<Self> {
		if bytes.len() < HEADER_BYTES || bytes.len() > MAX_JOB_CATALOG_BYTES {
			return Err(invalid("job catalog length is outside its format bounds"));
		}
		if bytes[..4] != MAGIC {
			return Err(invalid("job catalog magic does not match FGJB"));
		}
		let mut decoder = Decoder::new(bytes);
		decoder.skip(4)?;
		if decoder.read_u16()? != VERSION {
			return Err(invalid("job catalog version is incompatible with FGJB v1"));
		}
		decoder.expect_zero(2, "job catalog header reserved bytes")?;
		let generation = decoder.read_u64()?;
		if generation == 0 {
			return Err(invalid("job catalog generation zero is reserved"));
		}
		let count = decoder.read_count("job count", MAX_JOB_COUNT)?;
		let payload_len = decoder.read_count("job payload length", MAX_JOB_CATALOG_BYTES)?;
		let checksum = decoder.read_u64()?;
		if payload_len != bytes.len() - HEADER_BYTES {
			return Err(invalid("job catalog payload length does not consume the file"));
		}
		if checksum != catalog_checksum(bytes) {
			return Err(invalid("job catalog checksum does not match"));
		}
		let mut jobs = Vec::with_capacity(count);
		for _ in 0..count {
			jobs.push(decode_job(&mut decoder)?);
		}
		if !decoder.is_empty() {
			return Err(invalid("job catalog contains trailing bytes"));
		}
		jobs.sort_unstable_by_key(MaintenanceJobInfo::id);
		if jobs.windows(2).any(|pair| pair[0].id() == pair[1].id()) {
			return Err(invalid("job catalog contains duplicate job identities"));
		}
		Ok(Self {
			generation,
			jobs,
		})
	}
}

/// Encodes job.
fn encode_job(output: &mut Vec<u8>, job: &MaintenanceJobInfo) -> FerrosGrynnResult<()> {
	if job.event.is_some()
		|| job.kind == MaintenanceJobKind::EventInvocation
		|| job.state == MaintenanceJobState::Running
	{
		return Err(invalid("EVENT projections belong to the transactional outbox"));
	}
	validate_job_target(job.kind, job.target)?;
	output.extend_from_slice(job.id.as_uuid().as_inner().as_bytes());
	output.push(kind_byte(job.kind));
	output.push(state_byte(job.state));
	let (target_kind, class, index, version, graph) = match job.target {
		MaintenanceTarget::GraphIndex {
			graph,
			index,
			version,
		} => (5, 0, index.get(), version.get(), graph.get()),
		MaintenanceTarget::Database => (0, 0, 0, 0, 0),
		MaintenanceTarget::Graph(graph) => (1, 0, 0, 0, graph.get()),
		MaintenanceTarget::Index {
			class,
			index,
			version,
		} => (2, class.get(), index.get(), version.get(), 0),
		MaintenanceTarget::BaseClass(class) => (3, class.get(), 0, 0, 0),
		MaintenanceTarget::EffectiveClass {
			class,
			graph,
		} => (4, class.get(), 0, 0, graph.get()),
	};
	output.push(target_kind);
	let flags = u8::from(job.candidate_fingerprint.is_some())
		| (u8::from(job.terminal_status.is_some()) << 1)
		| (u8::from(job.diagnostic.is_some()) << 2);
	output.push(flags);
	let (analyze_mode, analyze_sample_count) = encode_analyze_mode(job)?;
	output.push(analyze_mode);
	output.extend_from_slice(&analyze_sample_count.to_le_bytes());
	output.extend_from_slice(&class.to_le_bytes());
	output.extend_from_slice(&index.to_le_bytes());
	output.extend_from_slice(&version.to_le_bytes());
	output.extend_from_slice(&job.restart_count.to_le_bytes());
	output.extend_from_slice(&graph.to_le_bytes());
	output.extend_from_slice(&job.source_revision.to_le_bytes());
	output.extend_from_slice(&job.processed_items.to_le_bytes());
	output.extend_from_slice(&job.written_bytes.to_le_bytes());
	output.extend_from_slice(&job.sort_runs.to_le_bytes());
	output.extend_from_slice(
		&u64::try_from(job.max_memory_bytes.get())
			.map_err(|_error| invalid("job memory limit exceeds u64"))?
			.to_le_bytes(),
	);
	output.extend_from_slice(
		&u32::try_from(job.max_parallelism.get())
			.map_err(|_error| invalid("job parallelism exceeds u32"))?
			.to_le_bytes(),
	);
	write_time(output, job.created_at)?;
	write_time(output, job.updated_at)?;
	output.extend_from_slice(&job.candidate_fingerprint.unwrap_or(0).to_le_bytes());
	match job.terminal_status {
		Some(code) => output.extend_from_slice(code.body().as_bytes()),
		None => output.extend_from_slice(&[0; 5]),
	}
	let diagnostic = job.diagnostic.as_deref().unwrap_or("");
	if diagnostic.len() > MAX_DIAGNOSTIC_BYTES || diagnostic.contains('\0') {
		return Err(invalid("job diagnostic is outside its format domain"));
	}
	write_u16(output, diagnostic.len(), "job diagnostic length")?;
	output.extend_from_slice(diagnostic.as_bytes());
	Ok(())
}

/// Decodes job.
fn decode_job(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<MaintenanceJobInfo> {
	let mut uuid = [0; 16];
	uuid.copy_from_slice(decoder.read_exact(16)?);
	let id = JobId::from_uuid(Uuid::from(uuid::Uuid::from_bytes(uuid)));
	let kind = decode_kind(decoder.read_u8()?)?;
	let state = decode_state(decoder.read_u8()?)?;
	let target_kind = decoder.read_u8()?;
	let flags = decoder.read_u8()?;
	if flags & !0b111 != 0 {
		return Err(invalid("job record contains unknown flag bits"));
	}
	let analyze_mode = decode_analyze_mode(kind, decoder.read_u8()?, decoder.read_u32()?)?;
	let class = decoder.read_u32()?;
	let index = decoder.read_u32()?;
	let version = decoder.read_u32()?;
	let restart_count = decoder.read_u32()?;
	let graph = decoder.read_u64()?;
	let target = match (target_kind, class, index, version, graph) {
		(5, 0, index, version, graph) => MaintenanceTarget::GraphIndex {
			graph: GraphId::new(graph).ok_or_else(|| invalid("job graph index owner is zero"))?,
			index: IndexId::new(index)
				.ok_or_else(|| invalid("job graph index identity is reserved"))?,
			version: SchemaVersion::new(version)
				.ok_or_else(|| invalid("job graph index version is zero"))?,
		},
		(0, 0, 0, 0, 0) => MaintenanceTarget::Database,
		(1, 0, 0, 0, graph) => MaintenanceTarget::Graph(
			GraphId::new(graph).ok_or_else(|| invalid("job graph target uses identity zero"))?,
		),
		(2, class, index, version, 0) => MaintenanceTarget::Index {
			class: ClassId::new(class)
				.ok_or_else(|| invalid("job class target uses a reserved identity"))?,
			index: IndexId::new(index)
				.ok_or_else(|| invalid("job index target uses a reserved identity"))?,
			version: SchemaVersion::new(version)
				.ok_or_else(|| invalid("job index target uses schema version zero"))?,
		},
		(3, class, 0, 0, 0) => MaintenanceTarget::BaseClass(
			ClassId::new(class)
				.ok_or_else(|| invalid("job base-class target uses a reserved identity"))?,
		),
		(4, class, 0, 0, graph) => MaintenanceTarget::EffectiveClass {
			class: ClassId::new(class)
				.ok_or_else(|| invalid("job effective-class target uses a reserved identity"))?,
			graph: GraphId::new(graph)
				.ok_or_else(|| invalid("job effective-class graph uses identity zero"))?,
		},
		_ => return Err(invalid("job target encoding is not canonical")),
	};
	let source_revision = decoder.read_u64()?;
	let processed_items = decoder.read_u64()?;
	let written_bytes = decoder.read_u64()?;
	let sort_runs = decoder.read_u64()?;
	let max_memory_bytes = usize::try_from(decoder.read_u64()?)
		.ok()
		.and_then(NonZeroUsize::new)
		.ok_or_else(|| invalid("job memory limit is outside this platform domain"))?;
	let max_parallelism = usize::try_from(decoder.read_u32()?)
		.ok()
		.and_then(NonZeroUsize::new)
		.ok_or_else(|| invalid("job parallelism is outside this platform domain"))?;
	let created_at = read_time(decoder)?;
	let updated_at = read_time(decoder)?;
	let fingerprint = decoder.read_u64()?;
	let status_bytes = decoder.read_exact(5)?;
	let diagnostic_len = usize::from(decoder.read_u16()?);
	if diagnostic_len > MAX_DIAGNOSTIC_BYTES {
		return Err(invalid("job diagnostic exceeds its format limit"));
	}
	let diagnostic = std::str::from_utf8(decoder.read_exact(diagnostic_len)?)
		.map_err(|_error| invalid("job diagnostic is not UTF-8"))?;
	if diagnostic.contains('\0') {
		return Err(invalid("job diagnostic contains NUL"));
	}
	let candidate_fingerprint = if flags & 1 == 0 {
		if fingerprint != 0 {
			return Err(invalid("job fingerprint bytes are non-zero while absent"));
		}
		None
	} else {
		Some(fingerprint)
	};
	let terminal_status = if flags & 2 == 0 {
		if status_bytes.iter().any(|byte| *byte != 0) {
			return Err(invalid("job status bytes are non-zero while absent"));
		}
		None
	} else {
		let body = std::str::from_utf8(status_bytes)
			.map_err(|_error| invalid("job status code is not ASCII"))?;
		Some(
			StatusCode::from_str(&format!("FG-{body}"))
				.map_err(|_error| invalid("job status code is malformed"))?,
		)
	};
	let diagnostic = if flags & 4 == 0 {
		if !diagnostic.is_empty() {
			return Err(invalid("job diagnostic bytes are non-empty while absent"));
		}
		None
	} else {
		Some(diagnostic.to_owned())
	};
	validate_job_target(kind, target)?;
	Ok(MaintenanceJobInfo {
		event: None,
		id,
		kind,
		state,
		target,
		source_revision,
		processed_items,
		written_bytes,
		sort_runs,
		restart_count,
		max_memory_bytes,
		max_parallelism,
		candidate_fingerprint,
		analyze_mode,
		terminal_status,
		diagnostic,
		created_at,
		updated_at,
	})
}

/// Validates job target.
fn validate_job_target(
	kind: MaintenanceJobKind,
	target: MaintenanceTarget,
) -> FerrosGrynnResult<()> {
	let compatible = matches!(
		(kind, target),
		(
			MaintenanceJobKind::BuildIndex
				| MaintenanceJobKind::RebuildIndex
				| MaintenanceJobKind::DeferredIndexCatchUp
				| MaintenanceJobKind::CompactIndex,
			MaintenanceTarget::Index { .. } | MaintenanceTarget::GraphIndex { .. }
		) | (
			MaintenanceJobKind::Analyze,
			MaintenanceTarget::BaseClass(_)
				| MaintenanceTarget::EffectiveClass { .. }
				| MaintenanceTarget::Graph(_)
		) | (MaintenanceJobKind::CompactGraph, MaintenanceTarget::Graph(_))
			| (
				MaintenanceJobKind::CompactDatabase | MaintenanceJobKind::PurgeChangefeed,
				MaintenanceTarget::Database
			)
	);
	if compatible {
		Ok(())
	} else {
		Err(invalid("job kind and logical target are incompatible"))
	}
}

/// Encodes analyze mode.
fn encode_analyze_mode(job: &MaintenanceJobInfo) -> FerrosGrynnResult<(u8, u32)> {
	match (job.kind, job.analyze_mode) {
		(MaintenanceJobKind::Analyze, Some(MaintenanceAnalyzeMode::Automatic)) => Ok((1, 0)),
		(MaintenanceJobKind::Analyze, Some(MaintenanceAnalyzeMode::Full)) => Ok((2, 0)),
		(
			MaintenanceJobKind::Analyze,
			Some(MaintenanceAnalyzeMode::Approximate {
				sample_count,
			}),
		) if (1_024..=1_000_000).contains(&sample_count) => Ok((3, sample_count)),
		(MaintenanceJobKind::Analyze, _) => {
			Err(invalid("ANALYZE job has a non-canonical statistics mode"))
		}
		(_, None) => Ok((0, 0)),
		(_, Some(_)) => Err(invalid("non-ANALYZE job contains a statistics mode")),
	}
}

/// Decodes analyze mode.
fn decode_analyze_mode(
	kind: MaintenanceJobKind,
	mode: u8,
	sample_count: u32,
) -> FerrosGrynnResult<Option<MaintenanceAnalyzeMode>> {
	match (kind, mode, sample_count) {
		(MaintenanceJobKind::Analyze, 1, 0) => Ok(Some(MaintenanceAnalyzeMode::Automatic)),
		(MaintenanceJobKind::Analyze, 2, 0) => Ok(Some(MaintenanceAnalyzeMode::Full)),
		(MaintenanceJobKind::Analyze, 3, sample_count)
			if (1_024..=1_000_000).contains(&sample_count) =>
		{
			Ok(Some(MaintenanceAnalyzeMode::Approximate {
				sample_count,
			}))
		}
		(MaintenanceJobKind::Analyze, _, _) => {
			Err(invalid("ANALYZE job has a non-canonical statistics mode"))
		}
		(_, 0, 0) => Ok(None),
		_ => Err(invalid("non-ANALYZE job contains a statistics mode")),
	}
}

/// Writes time.
fn write_time(output: &mut Vec<u8>, value: SystemTime) -> FerrosGrynnResult<()> {
	let duration = value
		.duration_since(UNIX_EPOCH)
		.map_err(|_error| invalid("job timestamp predates the Unix epoch"))?;
	output.extend_from_slice(&duration.as_secs().to_le_bytes());
	output.extend_from_slice(&duration.subsec_nanos().to_le_bytes());
	Ok(())
}

/// Reads time.
fn read_time(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<SystemTime> {
	let seconds = decoder.read_u64()?;
	let nanos = decoder.read_u32()?;
	if nanos >= 1_000_000_000 {
		return Err(invalid("job timestamp nanoseconds are outside their domain"));
	}
	UNIX_EPOCH
		.checked_add(Duration::new(seconds, nanos))
		.ok_or_else(|| invalid("job timestamp is outside the platform domain"))
}

/// Returns the kind byte result.
const fn kind_byte(kind: MaintenanceJobKind) -> u8 {
	match kind {
		MaintenanceJobKind::EventInvocation => 8,
		MaintenanceJobKind::BuildIndex => 0,
		MaintenanceJobKind::RebuildIndex => 1,
		MaintenanceJobKind::DeferredIndexCatchUp => 2,
		MaintenanceJobKind::Analyze => 3,
		MaintenanceJobKind::CompactIndex => 4,
		MaintenanceJobKind::CompactGraph => 5,
		MaintenanceJobKind::CompactDatabase => 6,
		MaintenanceJobKind::PurgeChangefeed => 7,
	}
}

/// Decodes kind.
fn decode_kind(value: u8) -> FerrosGrynnResult<MaintenanceJobKind> {
	match value {
		0 => Ok(MaintenanceJobKind::BuildIndex),
		1 => Ok(MaintenanceJobKind::RebuildIndex),
		2 => Ok(MaintenanceJobKind::DeferredIndexCatchUp),
		3 => Ok(MaintenanceJobKind::Analyze),
		4 => Ok(MaintenanceJobKind::CompactIndex),
		5 => Ok(MaintenanceJobKind::CompactGraph),
		6 => Ok(MaintenanceJobKind::CompactDatabase),
		7 => Ok(MaintenanceJobKind::PurgeChangefeed),
		_ => Err(invalid("job record contains an unknown kind")),
	}
}

/// Returns the state byte result.
const fn state_byte(state: MaintenanceJobState) -> u8 {
	match state {
		MaintenanceJobState::Running => 8,
		MaintenanceJobState::Queued => 0,
		MaintenanceJobState::BuildingInitial => 1,
		MaintenanceJobState::CatchingUp => 2,
		MaintenanceJobState::Publishing => 3,
		MaintenanceJobState::Completed => 4,
		MaintenanceJobState::Cancelled => 5,
		MaintenanceJobState::Failed => 6,
		MaintenanceJobState::Interrupted => 7,
	}
}

/// Decodes state.
fn decode_state(value: u8) -> FerrosGrynnResult<MaintenanceJobState> {
	match value {
		0 => Ok(MaintenanceJobState::Queued),
		1 => Ok(MaintenanceJobState::BuildingInitial),
		2 => Ok(MaintenanceJobState::CatchingUp),
		3 => Ok(MaintenanceJobState::Publishing),
		4 => Ok(MaintenanceJobState::Completed),
		5 => Ok(MaintenanceJobState::Cancelled),
		6 => Ok(MaintenanceJobState::Failed),
		7 => Ok(MaintenanceJobState::Interrupted),
		_ => Err(invalid("job record contains an unknown state")),
	}
}

/// Returns the catalog checksum result.
fn catalog_checksum(bytes: &[u8]) -> u64 {
	let mut canonical = bytes.to_vec();
	canonical[CHECKSUM_OFFSET..HEADER_BYTES].fill(0);
	rapidhash_v3(&canonical)
}

/// Writes u16.
fn write_u16(output: &mut Vec<u8>, value: usize, field: &str) -> FerrosGrynnResult<()> {
	let value = u16::try_from(value).map_err(|_error| invalid(format!("{field} overflows u16")))?;
	output.extend_from_slice(&value.to_le_bytes());
	Ok(())
}

/// Writes u32.
fn write_u32(output: &mut Vec<u8>, value: usize, field: &str) -> FerrosGrynnResult<()> {
	let value = u32::try_from(value).map_err(|_error| invalid(format!("{field} overflows u32")))?;
	output.extend_from_slice(&value.to_le_bytes());
	Ok(())
}

/// Decoder state.
struct Decoder<'a> {
	/// Bytes associated with this decoder.
	bytes: &'a [u8],
	/// Position relative to the beginning of the associated buffer.
	offset: usize,
}

impl<'a> Decoder<'a> {
	/// Creates a new `Decoder<'a>` value.
	const fn new(bytes: &'a [u8]) -> Self {
		Self {
			bytes,
			offset: 0,
		}
	}

	/// Reads exact.
	fn read_exact(&mut self, length: usize) -> FerrosGrynnResult<&'a [u8]> {
		let end = self
			.offset
			.checked_add(length)
			.ok_or_else(|| invalid("job catalog offset overflows"))?;
		let value =
			self.bytes.get(self.offset..end).ok_or_else(|| invalid("job catalog is truncated"))?;
		self.offset = end;
		Ok(value)
	}

	/// Returns the skip result.
	fn skip(&mut self, length: usize) -> FerrosGrynnResult<()> {
		self.read_exact(length).map(|_bytes| ())
	}

	/// Reads u8.
	fn read_u8(&mut self) -> FerrosGrynnResult<u8> {
		Ok(self.read_exact(1)?[0])
	}

	/// Reads u16.
	fn read_u16(&mut self) -> FerrosGrynnResult<u16> {
		let mut bytes = [0; 2];
		bytes.copy_from_slice(self.read_exact(2)?);
		Ok(u16::from_le_bytes(bytes))
	}

	/// Reads u32.
	fn read_u32(&mut self) -> FerrosGrynnResult<u32> {
		let mut bytes = [0; 4];
		bytes.copy_from_slice(self.read_exact(4)?);
		Ok(u32::from_le_bytes(bytes))
	}

	/// Reads u64.
	fn read_u64(&mut self) -> FerrosGrynnResult<u64> {
		let mut bytes = [0; 8];
		bytes.copy_from_slice(self.read_exact(8)?);
		Ok(u64::from_le_bytes(bytes))
	}

	/// Reads count.
	fn read_count(&mut self, field: &str, maximum: usize) -> FerrosGrynnResult<usize> {
		let value = usize::try_from(self.read_u32()?)
			.map_err(|_error| invalid(format!("{field} exceeds this platform")))?;
		if value > maximum {
			return Err(invalid(format!("{field} exceeds its format limit")));
		}
		Ok(value)
	}

	/// Returns the expect zero result.
	fn expect_zero(&mut self, length: usize, field: &str) -> FerrosGrynnResult<()> {
		if self.read_exact(length)?.iter().any(|byte| *byte != 0) {
			return Err(invalid(format!("{field} are non-zero")));
		}
		Ok(())
	}

	/// Returns whether there are no logical elements.
	fn is_empty(&self) -> bool {
		self.offset == self.bytes.len()
	}
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
/// Regression tests for job catalog behavior.
mod tests {
	use super::*;

	/// Returns sample for this module.
	fn sample() -> FerrosGrynnResult<MaintenanceJobInfo> {
		let now = UNIX_EPOCH + Duration::new(1_700_000_000, 123);
		Ok(MaintenanceJobInfo {
			event: None,
			id: JobId::from_uuid(Uuid::from(uuid::Uuid::from_u128(
				0x018b_cfe5_6800_7000_8000_0000_0000_0001,
			))),
			kind: MaintenanceJobKind::RebuildIndex,
			state: MaintenanceJobState::Publishing,
			target: MaintenanceTarget::Index {
				class: ClassId::new(1).ok_or_else(|| invalid("test class identity"))?,
				index: IndexId::new(2).ok_or_else(|| invalid("test index identity"))?,
				version: SchemaVersion::INITIAL,
			},
			source_revision: 7,
			processed_items: 9,
			written_bytes: 11,
			sort_runs: 2,
			restart_count: 1,
			max_memory_bytes: NonZeroUsize::new(1024).unwrap_or(NonZeroUsize::MIN),
			max_parallelism: NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
			candidate_fingerprint: Some(44),
			analyze_mode: None,
			terminal_status: None,
			diagnostic: Some("publishing".to_owned()),
			created_at: now,
			updated_at: now,
		})
	}

	/// Returns sample analyze for this module.
	fn sample_analyze() -> FerrosGrynnResult<MaintenanceJobInfo> {
		let now = UNIX_EPOCH + Duration::new(1_700_000_001, 456);
		Ok(MaintenanceJobInfo {
			event: None,
			id: JobId::from_uuid(Uuid::from(uuid::Uuid::from_u128(
				0x018b_cfe5_6801_7000_8000_0000_0000_0002,
			))),
			kind: MaintenanceJobKind::Analyze,
			state: MaintenanceJobState::Queued,
			target: MaintenanceTarget::EffectiveClass {
				class: ClassId::new(3).ok_or_else(|| invalid("test class identity"))?,
				graph: GraphId::new(4).ok_or_else(|| invalid("test graph identity"))?,
			},
			source_revision: 12,
			processed_items: 0,
			written_bytes: 0,
			sort_runs: 0,
			restart_count: 0,
			max_memory_bytes: NonZeroUsize::new(2048).unwrap_or(NonZeroUsize::MIN),
			max_parallelism: NonZeroUsize::MIN,
			candidate_fingerprint: None,
			analyze_mode: Some(MaintenanceAnalyzeMode::Approximate {
				sample_count: 30_000,
			}),
			terminal_status: None,
			diagnostic: None,
			created_at: now,
			updated_at: now,
		})
	}

	/// Verifies fg jb v1 round trips and rejects corruption.
	#[test]
	fn fg_jb_v1_round_trips_and_rejects_corruption() {
		use crate::test_assertions::AssertResult as _;

		let catalog = JobCatalog {
			generation: 3,
			jobs: vec![sample().assert_ok(), sample_analyze().assert_ok()],
		};
		let bytes = catalog.encode().assert_ok();
		assert_eq!(&bytes[..4], b"FGJB");
		assert_eq!(&bytes[4..6], &1_u16.to_le_bytes());
		assert_eq!(JobCatalog::decode(&bytes).assert_ok(), catalog);

		let mut old = bytes.clone();
		old[4..6].copy_from_slice(&0_u16.to_le_bytes());
		assert!(JobCatalog::decode(&old).is_err());
		let mut corrupt = bytes;
		let last = corrupt.len() - 1;
		corrupt[last] ^= 1;
		assert!(JobCatalog::decode(&corrupt).is_err());

		let mut invalid_mode = sample_analyze().assert_ok();
		invalid_mode.analyze_mode = None;
		assert!(
			JobCatalog {
				generation: 1,
				jobs: vec![invalid_mode],
			}
			.encode()
			.is_err()
		);

		let mut invalid_target = sample().assert_ok();
		invalid_target.target = MaintenanceTarget::Database;
		assert!(
			JobCatalog {
				generation: 1,
				jobs: vec![invalid_target],
			}
			.encode()
			.is_err()
		);
	}

	/// Verifies graph index job target round trips and rejects reserved owner.
	#[test]
	fn graph_index_job_target_round_trips_and_rejects_reserved_owner() {
		use crate::test_assertions::AssertResult as _;

		let mut job = sample().assert_ok();
		job.target = MaintenanceTarget::GraphIndex {
			graph: GraphId::new(7).ok_or_else(|| invalid("test graph identity")).assert_ok(),
			index: IndexId::new(9).ok_or_else(|| invalid("test index identity")).assert_ok(),
			version: SchemaVersion::INITIAL,
		};
		let catalog = JobCatalog {
			generation: 1,
			jobs: vec![job.clone()],
		};
		let bytes = catalog.encode().assert_ok();
		assert_eq!(JobCatalog::decode(&bytes).assert_ok(), catalog);
		let mut record = Vec::new();
		encode_job(&mut record, &job).assert_ok();
		assert_eq!(record[18], 5);
		assert_eq!(&record[25..37], &[0, 0, 0, 0, 9, 0, 0, 0, 1, 0, 0, 0]);
		record[41..49].fill(0);
		assert!(decode_job(&mut Decoder::new(&record)).is_err());
	}
}
