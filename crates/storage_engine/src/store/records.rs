use crate::io::{self, FileHandle};
use binrw::{
	BinRead, BinWrite, binrw,
	io::{Cursor, Read, Seek, SeekFrom},
};
use std::io::Result;
use std::mem::size_of;

#[binrw]
#[brw(little)]
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct NodeRecord {
	#[br(map = |b: u8| b != 0)] // read as bool
	#[bw(map = |b: &bool| if *b { 1u8 } else { 0u8 })] // write as u8
	in_use: bool,
	first_relationship_id: u64,
	first_property_id: u64,
	label_block_id: u64,
	next_block_id: u64,
}

#[binrw]
#[brw(little)]
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct EdgeRecord {
	#[br(map = |b: u8| b != 0)] // read as bool
	#[bw(map = |b: &bool| if *b { 1u8 } else { 0u8 })] // write as u8
	in_use: bool,
	type_id: u32,
	prev_rel_id: u32,
	next_rel_id: u32,
	first_property_id: u64,
	start_node_id: u64,
	end_node_id: u64,
}

#[derive(Debug, Copy, Clone)]
enum PropertyValue {
	Integer(i64),
	Boolean(bool),

	StringOffset(u64),
	ArrayOffset(u64),

	Unknown,
}

#[binrw]
#[brw(little)]
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct PropertyRecord {
	#[br(map = |b: u8| b != 0)] // read as bool
	#[bw(map = |b: &bool| if *b { 1u8 } else { 0u8 })] // write as u8
	in_use: bool,
	key_id: u32,
	#[br(map = |val: u64| {
        let ty_raw = (val & 0xF) as u8;
        let next_raw = (val >> 4);
        (ty_raw, next_raw)
    })]
	#[bw(map = |(ty, next): &(u8, u64)| {
        (*ty as u64) | (next << 4)
    })]
	type_and_next: (u8, u64),
	#[br(map = |val: u64| {
        let (ty_raw, _) = type_and_next;
        match ty_raw {
            1 => PropertyValue::Integer(val as i64),
            2 => PropertyValue::Boolean(val != 0),
            3 => PropertyValue::StringOffset(val),
            4 => PropertyValue::ArrayOffset(val),
            _ => PropertyValue::Unknown,
        }
    })]
	#[bw(map = |value: &PropertyValue| match value {
        PropertyValue::Integer(v) => *v as u64,
        PropertyValue::StringOffset(v) => *v,
        PropertyValue::ArrayOffset(v) => *v,
        PropertyValue::Boolean(v) => if *v { 1 } else { 0 },
        PropertyValue::Unknown => 0, // Default value
    })]
	value: PropertyValue,
}

pub fn read_record<'a, T>(handle: &FileHandle, offset: u64) -> Result<T>
where
	T: BinRead + 'a,
	<T as BinRead>::Args<'a>: Sized + Default,
{
    let alignment = io::get_alignment();
    let record_size = size_of::<T>();
    if record_size > alignment {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Record size ({}) exceeds page size ({}) for O_DIRECT read", record_size, alignment),
        ));
    }

    
}
