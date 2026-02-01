use super::os::{
	get_committed_virtual_memory, get_free_physical_memory, get_total_physical_memory,
};
use std::fmt;
use thiserror::Error;

pub(crate) type Result<T> = core::result::Result<T, MemoryError>;

#[derive(Error, Debug)]
pub enum MemoryError {
	#[error(transparent)]
	NativeMemoryAllocationRefused(#[from] NativeMemoryAllocationRefusedRepr),
	#[error(transparent)]
	Io(std::io::Error),
}

#[derive(Error, Debug)]
pub(crate) struct NativeMemoryAllocationRefusedRepr {
	pub(crate) size: usize,
	pub(crate) already_allocated: usize,
}

impl fmt::Display for NativeMemoryAllocationRefusedRepr {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_fmt(format_args!("Failed to allocate {} bytes.", self.size))?;
		f.write_fmt(format_args!(
			"So far {} bytes have already been successfully allocated.",
			self.already_allocated
		))?;
		f.write_fmt(format_args!(
			"The system currently has \
			{} total physical memory, \
			{} committed virtual memory, and \
			{} free physical memory. ",
			get_total_physical_memory(),
			get_committed_virtual_memory(),
			get_free_physical_memory()
		))
	}
}
