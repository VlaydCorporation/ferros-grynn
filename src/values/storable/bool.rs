use std::fmt::{Display, Formatter};
use crate::common::hashing::HashFunction;
use crate::impl_zero_heap;
use crate::values::AnyValueExt;
use crate::values::mapper::ValueMapper;
use crate::values::memohash::{hash_bool, HashValue, Hashable};
use crate::values::representation::reprs::BOOL_REPR;
use crate::values::representation::ValueRepresentation;
use crate::values::storable::{ScalarValueExt, ValueExt};
use crate::values::writer::AnyValueWriter;

#[derive(PartialEq)]
pub struct BoolValue {
    value: bool,
}

impl BoolValue {
    pub fn new(value: bool) -> BoolValue {
        BoolValue { value }
    }

    pub fn value(&self) -> bool {
        self.value
    }
}

impl Hashable for BoolValue {
    fn hash(&self) -> HashValue {
        hash_bool(self.value)
    }
}

impl Display for BoolValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}('{}')", self.get_type_name(), self.value)
    }
}

impl AnyValueExt for BoolValue {
    fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> crate::values::error::Result<()> {
        writer.write_bool(self.value)
    }

    fn map<T>(&self, mapper: &impl ValueMapper<T>) {
        mapper.map_bool(self);
    }

    fn representation(&self) -> ValueRepresentation {
        BOOL_REPR
    }

    fn get_type_name(&self) -> &str {
        "Boolean"
    }
}

impl ValueExt for BoolValue {
    fn update_hash(&self, hash_function: &impl HashFunction, hash: u64) -> u64 {
        hash_function.update(hash, self.hash().into())
    }

    fn pretty_print(&self) -> String {
        if self.value {
            "true".to_string()
        } else {
            "false".to_string()
        }
    }
}

impl ScalarValueExt for BoolValue {}

impl_zero_heap!(BoolValue);