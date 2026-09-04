//! Exact immutable page identities shared by resident cache implementations.

use super::{
	page::{PageHeader, PageLsn, PageNo},
	types::{CheckpointGeneration, Lsn, PageKey},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Immutable source version represented by one resident entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PageCacheVersion {
	/// Exact full-page image retained in WAL.
	Wal(PageLsn),
	/// Home-file image under one logical checkpoint generation.
	Home {
		/// Generation that invalidates every older home-cache entry.
		generation: CheckpointGeneration,
		/// Highest page LSN permitted by the database checkpoint.
		ceiling: Lsn,
	},
}

/// Complete identity of one immutable page version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageCacheKey {
	/// Complete database page identity.
	page_key: PageKey,
	/// WAL-exact or checkpoint-generation home version.
	version: PageCacheVersion,
}

impl PageCacheKey {
	/// Creates a cache key for one exact page image.
	pub const fn new(page_key: PageKey, version: PageCacheVersion) -> Self {
		Self {
			page_key,
			version,
		}
	}

	/// Returns the complete physical page identity.
	pub const fn page_key(self) -> PageKey {
		self.page_key
	}

	/// Returns the physical page number.
	pub const fn page_no(self) -> PageNo {
		self.page_key.page_no()
	}

	/// Returns the immutable cache-source version.
	pub const fn version(self) -> PageCacheVersion {
		self.version
	}
}

/// Ensures a loader returned the immutable page version named by its key.
pub fn validate_loaded_header(key: PageCacheKey, header: PageHeader) -> FerrosGrynnResult<()> {
	if header.page_no() != key.page_no() {
		return Err(FerrosGrynnError::invalid_storage_page(
			key.page_no().get(),
			format!("loader returned page {}", header.page_no().get()),
		));
	}
	match key.version() {
		PageCacheVersion::Wal(expected) if header.page_lsn() != expected => {
			return Err(FerrosGrynnError::invalid_storage_page(
				key.page_no().get(),
				format!(
					"loader returned page LSN {}, expected WAL LSN {}",
					header.page_lsn().get(),
					expected.get()
				),
			));
		}
		PageCacheVersion::Home {
			ceiling,
			..
		} if header.page_lsn().get() > ceiling.get() => {
			return Err(FerrosGrynnError::invalid_storage_page(
				key.page_no().get(),
				format!(
					"home page LSN {} exceeds checkpoint ceiling {}",
					header.page_lsn().get(),
					ceiling.get()
				),
			));
		}
		PageCacheVersion::Wal(_)
		| PageCacheVersion::Home {
			..
		} => {}
	}
	Ok(())
}
