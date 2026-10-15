use std::fmt;

use thiserror::Error;

use super::os::{
	get_committed_virtual_memory,
	get_free_physical_memory,
	get_total_physical_memory,
};

pub(crate) type Result<T> = core::result::Result<T, MemoryError>;

/// Allocation refusal or I/O failure reported by the disconnected memory
/// primitives.
#[derive(Error, Debug)]
pub enum MemoryError {
	/// A native allocation request exceeded the accepted memory policy.
	#[error(transparent)]
	NativeMemoryAllocationRefused(
		/// A native allocation request exceeded the accepted memory policy.
		#[from]
		NativeMemoryAllocationRefusedRepr,
	),
	/// An I/O operation required by the memory primitive failed.
	#[error(transparent)]
	Io(
		/// An I/O operation required by the memory primitive failed.
		std::io::Error,
	),
}

/// Requested allocation size and bytes already allocated when admission fails.
#[derive(Error, Debug)]
pub(crate) struct NativeMemoryAllocationRefusedRepr {
	/// Configured extent or payload size used by this representation.
	pub(crate) size: usize,
	/// Bytes already allocated when the new request was refused.
	pub(crate) already_allocated: usize,
}

impl fmt::Display for NativeMemoryAllocationRefusedRepr {
	/// Formats `NativeMemoryAllocationRefusedRepr` using the requested
	/// formatter.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_fmt(format_args!("Failed to allocate {} bytes.", self.size))?;
		f.write_fmt(format_args!(
			"So far {} bytes have already been successfully allocated.",
			self.already_allocated
		))?;
		f.write_fmt(format_args!(
			"The system currently has {} total physical memory, {} committed virtual memory, and \
			 {} free physical memory. ",
			get_total_physical_memory(),
			get_committed_virtual_memory(),
			get_free_physical_memory()
		))
	}
}
