use aligned_box::AlignedBox;
use std::io::{Error, Result};
use std::path::Path;
use super::{close_file, get_alignment, open_file, read_file, write_file, FileHandle};

/// Pointer alignment check
pub fn is_aligned(ptr: *const u8, alignment: usize) -> bool {
    (ptr as usize) % alignment == 0
}

/// Check for size multiplicity
pub fn is_size_valid(size: usize, alignment: usize) -> bool {
    size % alignment == 0
}

pub struct DirectBuffer {
    inner: AlignedBox<[u8]>,
    size: usize,
}

impl DirectBuffer {
    pub fn new(size: usize) -> Result<Self> {
        let alignment = get_alignment();
        assert!(is_size_valid(size, alignment));
        let buf = AlignedBox::slice_from_default(alignment, size)
            .map_err(|ex| Error::new(std::io::ErrorKind::InvalidData, ex))?;
        assert!(is_aligned(buf.as_ptr() as *const _, alignment));
        Ok(Self {
            inner: buf,
            size,
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.inner[..self.size]
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.inner[..self.size]
    }
}

pub struct DirectIOFile {
    handle: FileHandle,
}

impl DirectIOFile {
    pub fn open<P: AsRef<Path>>(path: P, write: bool) -> Result<Self> {
        let handle = open_file(path, write)?;
        Ok(Self {
            handle,
        })
    }

    pub fn read(&self, size: usize) -> Result<DirectBuffer> {
        let mut buffer = DirectBuffer::new(size)?;
        read_file(self.handle, buffer.as_mut_slice())?;
        Ok(buffer)
    }

    pub fn write(&self, data: &[u8]) -> Result<()> {
        if data.len() % get_alignment() != 0 {
            return Err(Error::new(std::io::ErrorKind::InvalidInput, "Unaligned write size"));
        }
        write_file(self.handle, data)?;
        Ok(())
    }
}

impl Drop for DirectIOFile {
    fn drop(&mut self) {
        close_file(self.handle).unwrap();
    }
}