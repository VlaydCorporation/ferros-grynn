//! One checked, self-contained commit containing NET and optional FULL views.

use super::{
	CodecBudget,
	Decoder,
	append,
	append_blob,
	checked_payload,
	diff,
	image,
	invalid,
	values,
};
use crate::{
	changefeed::{
		ChangeBatch,
		ChangeEntry,
		ChangeIdentity,
		ChangefeedGap,
		ChangefeedGapReason,
		ChangefeedId,
	},
	datastore::CapturedCommit,
	gql_status::FerrosGrynnResult,
	values::{DateTime, storage::EncodeContext},
};

/// Fixed logical header, separate from the page continuation framing.
const HEADER_BYTES: usize = 80;
/// Current development envelope version, without compatibility decoders.
const VERSION: u16 = 1;
/// Historical complete-original capture flag.
const ORIGINAL: u16 = 1;
/// Historical operation capture flag, including successful targeted no-ops.
const FULL: u16 = 2;

/// One authoritative parsed header shared by metadata and image readers.
struct GroupHeader {
	/// Historical capture flags.
	flags: u16,
	/// Exact source incarnation.
	source: ChangefeedId,
	/// Complete commit position.
	stamp: u64,
	/// Persisted UTC nanoseconds.
	time: i128,
	/// Validated source revision at capture.
	revision: u32,
	/// Historical dictionary length.
	dictionary_bytes: usize,
	/// Complete entry section length.
	body_bytes: usize,
	/// Number of ordinary entries.
	net_count: usize,
	/// Number of operation entries.
	full_count: usize,
	/// Zero for events, otherwise a known suppression reason.
	gap: u8,
}

/// Reads only checked framing, without dictionary or image allocations.
pub(in crate::storage) fn inspect(
	prefix: &[u8],
	length: u64,
) -> FerrosGrynnResult<super::ChangefeedGroupInfo> {
	let header = parse_header(prefix, length)?;
	Ok(super::ChangefeedGroupInfo {
		source: header.source,
		stamp: header.stamp,
		time: header.time,
		full: header.flags & FULL != 0,
		net_entries: header.net_count as u32,
		gap: header.gap != 0,
		next_page: 0,
		pages: 0,
		revision: header.revision,
	})
}

/// Validates the fixed header against the complete declared payload length.
fn parse_header(prefix: &[u8], length: u64) -> FerrosGrynnResult<GroupHeader> {
	let length = checked_payload(length)?;
	let mut decoder = Decoder::new(
		prefix
			.get(..HEADER_BYTES)
			.ok_or_else(|| invalid("changefeed commit header is truncated"))?,
	);
	if decoder.take(4)? != b"FGCB" || decoder.take(2)? != VERSION.to_le_bytes() {
		return Err(invalid("changefeed commit marker/version is invalid"));
	}
	let flags = u16::from_le_bytes(
		decoder
			.take(2)?
			.try_into()
			.map_err(|_error| invalid("changefeed capture flags are truncated"))?,
	);
	if flags & !(ORIGINAL | FULL) != 0 {
		return Err(invalid("changefeed capture flags are unknown"));
	}
	let source = ChangefeedId::from_uuid(
		uuid::Uuid::from_slice(decoder.take(16)?)
			.map_err(|_error| invalid("changefeed commit source is malformed"))?
			.into(),
	)?;
	let stamp = decoder.u64()?;
	let time = i128::from_le_bytes(
		decoder
			.take(16)?
			.try_into()
			.map_err(|_error| invalid("changefeed commit UTC time is truncated"))?,
	);
	let revision = decoder.u32()?;
	let dictionary_bytes = decoder.u32()? as usize;
	let body_bytes = checked_payload(decoder.u64()?)?;
	let net_count = decoder.u32()? as usize;
	let full_count = decoder.u32()? as usize;
	let gap = decoder.u8()?;
	decoder.zero(7)?;
	decoder.finish()?;
	if stamp == 0
		|| revision == 0
		|| gap > 2
		|| gap != 0 && (net_count != 0 || full_count != 0 || flags & FULL != 0)
		|| gap == 0 && net_count == 0 && full_count == 0
	{
		return Err(invalid("changefeed commit clocks or suppression metadata are invalid"));
	}
	jiff::Timestamp::from_nanosecond(time)
		.map_err(|_error| invalid("changefeed commit UTC time is outside its range"))?;
	if dictionary_bytes.checked_add(body_bytes).and_then(|bytes| bytes.checked_add(HEADER_BYTES))
		!= Some(length)
		|| net_count > 1_000_000
		|| full_count > 1_000_000
		|| flags & FULL == 0 && full_count != 0
	{
		return Err(invalid("changefeed commit section lengths/counts are inconsistent"));
	}
	Ok(GroupHeader {
		flags,
		source,
		stamp,
		time,
		revision,
		dictionary_bytes,
		body_bytes,
		net_count,
		full_count,
		gap,
	})
}

/// Decoded group descriptor and exact owned views.
pub(in crate::storage) struct DecodedGroup {
	/// Complete exact historical NET/FULL images or one suppression gap.
	pub(in crate::storage) commit: CapturedCommit,
}

/// Writes complete images and inverse before patches under one aggregate
/// budget.
pub(in crate::storage) fn encode(
	commit: &CapturedCommit,
	revision: u32,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	validate_commit(commit, revision)?;
	let mut context = EncodeContext::default();
	let mut body = Vec::new();
	encode_entries(
		&mut body,
		commit.net.entries(),
		commit.include_original,
		&mut context,
		budget,
		false,
	)?;
	if let Some(full) = &commit.full {
		encode_entries(
			&mut body,
			full.entries(),
			commit.include_original,
			&mut context,
			budget,
			true,
		)?;
	}
	let dictionary = values::encode_dictionary(&context, budget)?;
	let flags =
		(u16::from(commit.include_original) * ORIGINAL) | (u16::from(commit.full.is_some()) * FULL);
	let gap = match commit.gap.as_ref().map(ChangefeedGap::reason) {
		None => 0,
		Some(ChangefeedGapReason::SourceDropped) => 1,
		Some(ChangefeedGapReason::ImportSuppressed) => 2,
		Some(_) => return Err(invalid("changefeed group cannot persist a reader-generated gap")),
	};
	let dictionary_len = u32::try_from(dictionary.len())
		.map_err(|_error| invalid("changefeed dictionary bytes exceed u32"))?;
	let mut output = Vec::new();
	append(&mut output, b"FGCB", budget)?;
	append(&mut output, &VERSION.to_le_bytes(), budget)?;
	append(&mut output, &flags.to_le_bytes(), budget)?;
	append(&mut output, commit.net.source().as_uuid().as_inner().as_bytes(), budget)?;
	append(&mut output, &commit.net.versionstamp().get().to_le_bytes(), budget)?;
	append(
		&mut output,
		&commit.net.committed_at().as_inner().timestamp().as_nanosecond().to_le_bytes(),
		budget,
	)?;
	append(&mut output, &revision.to_le_bytes(), budget)?;
	append(&mut output, &dictionary_len.to_le_bytes(), budget)?;
	append(&mut output, &(body.len() as u64).to_le_bytes(), budget)?;
	values::write_count(&mut output, commit.net.entries().len(), budget)?;
	values::write_count(
		&mut output,
		commit.full.as_ref().map_or(0, |full| full.entries().len()),
		budget,
	)?;
	append(&mut output, &[gap, 0, 0, 0, 0, 0, 0, 0], budget)?;
	append(&mut output, &dictionary, budget)?;
	append(&mut output, &body, budget)?;
	Ok(output)
}

/// Restores one whole commit; no partially decoded row is exposed on failure.
pub(in crate::storage) fn decode(
	bytes: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<DecodedGroup> {
	let GroupHeader {
		flags,
		source,
		stamp,
		time,
		revision,
		dictionary_bytes,
		body_bytes,
		net_count,
		full_count,
		gap,
	} = parse_header(bytes, bytes.len() as u64)?;
	let mut decoder = Decoder::new(bytes);
	decoder.take(HEADER_BYTES)?;
	let committed_at = DateTime::new(
		jiff::Timestamp::from_nanosecond(time)
			.map_err(|_error| invalid("changefeed commit UTC time is outside its range"))?
			.to_zoned(jiff::tz::TimeZone::UTC),
	);
	let context = values::decode_dictionary(decoder.take(dictionary_bytes)?, budget)?;
	let mut body = Decoder::new(decoder.take(body_bytes)?);
	decoder.finish()?;
	let original = flags & ORIGINAL != 0;
	let net = decode_entries(&mut body, net_count, original, &context, budget, false)?;
	let full = decode_entries(&mut body, full_count, original, &context, budget, true)?;
	body.finish()?;
	let batch = ChangeBatch {
		source,
		versionstamp: stamp.into(),
		committed_at,
		entries: net,
	};
	let full = (flags & FULL != 0).then(|| ChangeBatch {
		source,
		versionstamp: stamp.into(),
		committed_at: batch.committed_at().clone(),
		entries: full,
	});
	let gap = match gap {
		0 => None,
		1 | 2 => Some(ChangefeedGap {
			source,
			reason: if gap == 1 {
				ChangefeedGapReason::SourceDropped
			} else {
				ChangefeedGapReason::ImportSuppressed
			},
			from: Some(stamp.into()),
			through: Some(stamp.into()),
		}),
		_ => return Err(invalid("changefeed suppression reason is unknown")),
	};
	let commit = CapturedCommit {
		net: batch,
		full,
		include_original: original,
		gap,
	};
	validate_commit(&commit, revision)?;
	Ok(DecodedGroup {
		commit,
	})
}

/// Validates independent group metadata, modes and suppression controls.
fn validate_commit(commit: &CapturedCommit, revision: u32) -> FerrosGrynnResult<()> {
	if revision == 0 || commit.net.versionstamp().get() == 0 {
		return Err(invalid("changefeed commit revision/stamp is zero"));
	}
	if let Some(full) = &commit.full
		&& (full.source() != commit.net.source()
			|| full.versionstamp() != commit.net.versionstamp()
			|| full.committed_at() != commit.net.committed_at())
	{
		return Err(invalid("changefeed FULL and NET commit metadata disagree"));
	}
	if let Some(gap) = &commit.gap {
		if !commit.net.entries().is_empty()
			|| commit.full.is_some()
			|| gap.source() != commit.net.source()
			|| gap.from() != Some(commit.net.versionstamp())
			|| gap.through() != Some(commit.net.versionstamp())
			|| !matches!(
				gap.reason(),
				ChangefeedGapReason::SourceDropped | ChangefeedGapReason::ImportSuppressed
			) {
			return Err(invalid(
				"changefeed suppression control contains entries or inconsistent bounds",
			));
		}
	} else if commit.net.entries().is_empty()
		&& commit.full.as_ref().is_none_or(|full| full.entries().is_empty())
	{
		return Err(invalid("changefeed commit has no captured entries or suppression control"));
	}
	Ok(())
}

/// Writes checked mode-local ordinals and reversible exact representation
/// bytes.
fn encode_entries(
	output: &mut Vec<u8>,
	entries: &[ChangeEntry],
	original: bool,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	full: bool,
) -> FerrosGrynnResult<()> {
	let mut previous = None;
	for (ordinal, entry) in entries.iter().enumerate() {
		check_entry(entry, ordinal as u64, previous, full)?;
		let before =
			entry.before().map(|before| image::encode(before, context, budget)).transpose()?;
		let after = entry.after().map(|after| image::encode(after, context, budget)).transpose()?;
		if !full && before == after {
			return Err(invalid("changefeed NET entry has no representation change"));
		}
		let stored_before = before
			.as_ref()
			.map(|before| {
				if original {
					budget.admit(before.len())?;
					Ok(before.clone())
				} else {
					diff::encode(before, after.as_deref().unwrap_or(&[]), budget)
				}
			})
			.transpose()?;
		append(output, &entry.ordinal().to_le_bytes(), budget)?;
		append(output, &entry.operation().to_le_bytes(), budget)?;
		append(
			output,
			&[u8::from(before.is_some()) | (u8::from(after.is_some()) << 1), 0, 0, 0, 0, 0, 0, 0],
			budget,
		)?;
		append_blob(output, after.as_deref().unwrap_or(&[]), budget)?;
		append_blob(output, stored_before.as_deref().unwrap_or(&[]), budget)?;
		previous = Some(entry);
	}
	Ok(())
}

/// Admits the complete entry array before decoding any untrusted image lengths.
fn decode_entries(
	decoder: &mut Decoder<'_>,
	count: usize,
	original: bool,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	full: bool,
) -> FerrosGrynnResult<Vec<ChangeEntry>> {
	if count > (decoder.bytes.len() - decoder.position) / 40 {
		return Err(invalid("changefeed entry count exceeds its body bounds"));
	}
	budget.admit(
		count
			.checked_mul(std::mem::size_of::<ChangeEntry>())
			.ok_or_else(|| invalid("changefeed entry allocation overflows"))?,
	)?;
	let mut output = Vec::new();
	output
		.try_reserve_exact(count)
		.map_err(|_error| invalid("changefeed entry allocation failed"))?;
	for ordinal in 0..count {
		let stored_ordinal = decoder.u64()?;
		let operation = decoder.u64()?;
		if stored_ordinal != ordinal as u64
			|| full
				&& output.last().is_some_and(|entry: &ChangeEntry| entry.operation() > operation)
		{
			return Err(invalid("changefeed entry ordinal or operation order is invalid"));
		}
		let flags = decoder.u8()?;
		decoder.zero(7)?;
		let after_bytes = decoder.blob()?;
		let before_bytes = decoder.blob()?;
		if flags == 0
			|| flags > 3
			|| (flags & 2 == 0) != after_bytes.is_empty()
			|| (flags & 1 == 0) != before_bytes.is_empty()
		{
			return Err(invalid("changefeed entry image presence and bytes disagree"));
		}
		let after = if flags & 2 != 0 {
			Some(image::decode(after_bytes, context, budget)?)
		} else {
			None
		};
		let before = if flags & 1 != 0 {
			let restored;
			let bytes = if original {
				before_bytes
			} else {
				restored = diff::decode(before_bytes, after_bytes, budget)?;
				&restored
			};
			if !full && flags == 3 && bytes == after_bytes {
				return Err(invalid("changefeed NET entry has no representation change"));
			}
			Some(image::decode(bytes, context, budget)?)
		} else {
			None
		};
		let entry = ChangeEntry {
			ordinal: stored_ordinal,
			operation,
			before,
			after,
		};
		check_entry(&entry, ordinal as u64, output.last(), full)?;
		output.push(entry);
	}
	Ok(output)
}

/// Returns a row's logical identity without cloning its owned image.
fn identity(entry: &ChangeEntry) -> FerrosGrynnResult<&ChangeIdentity> {
	entry
		.after()
		.or_else(|| entry.before())
		.map(|image| image.identity())
		.ok_or_else(|| invalid("changefeed entry contains no image"))
}

/// Validates ordinals, target identity and canonical NET/execution FULL order.
fn check_entry(
	entry: &ChangeEntry,
	ordinal: u64,
	previous: Option<&ChangeEntry>,
	full: bool,
) -> FerrosGrynnResult<()> {
	if entry.ordinal() != ordinal {
		return Err(invalid("changefeed entry ordinal is not contiguous"));
	}
	if let (Some(before), Some(after)) = (entry.before(), entry.after())
		&& before.identity() != after.identity()
	{
		return Err(invalid("changefeed entry changes logical identity"));
	}
	let current = identity(entry)?;
	if let Some(previous) = previous {
		if full {
			if previous.operation() > entry.operation() {
				return Err(invalid("changefeed FULL operation order decreases"));
			}
		} else if identity(previous)? >= current {
			return Err(invalid("changefeed NET identities are not strictly ordered"));
		}
	}
	Ok(())
}
