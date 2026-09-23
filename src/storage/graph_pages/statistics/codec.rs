//! Checked little-endian primitives shared by statistics page codecs.

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::page::PageNo,
};

pub(super) fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
	bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	let mut value = [0; 2];
	value.copy_from_slice(&bytes[offset..offset + 2]);
	u16::from_le_bytes(value)
}

pub(super) fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

pub(super) fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}

pub(super) fn write_optional_page(bytes: &mut [u8], offset: usize, value: Option<PageNo>) {
	write_u64(bytes, offset, value.unwrap_or(PageNo::NULL).get());
}

pub(super) fn read_optional_page(bytes: &[u8], offset: usize) -> Option<PageNo> {
	let page = PageNo::new(read_u64(bytes, offset));
	(page != PageNo::NULL).then_some(page)
}

pub(super) fn validate_zero(bytes: &[u8], page: PageNo, field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid_page(page, format!("{field} are non-zero")));
	}
	Ok(())
}

pub(super) fn invalid_page(page: PageNo, reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_page(page.get(), reason)
}
