use std::hash::{DefaultHasher, Hash, Hasher};

#[repr(C)]
struct Test1 {
    in_use: bool,
    first_relationship_id: u64,
    first_property_id: u64,
    label_block_id: u64,
    next_block_id: u64
}

#[repr(C)]
struct Test2 {
    in_use: bool,
    type_id: u32,
    prev_rel_id: u32,
    next_rel_id: u32,
    first_property_id: u64,
    start_node_id: u64,
    end_node_id: u64,
}

#[repr(C)]
struct Test3 {
    in_use: bool,
    key_id: u32,
    type_and_next: u64,
    value: u64,
}

#[test]
fn size_test() {
    assert_eq!(std::mem::size_of::<Test1>(), 40);
    assert_eq!(std::mem::size_of::<Test2>(), 40);
    assert_eq!(std::mem::size_of::<Test3>(), 24);
}