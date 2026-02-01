mod bool;
mod number;
mod duration;
mod temporal;

use ferros_grynn_macros::Measurable;
use crate::common::hashing::{HashFunction, IncrementalXXH64};
use crate::values::{AnyValue, AnyValueExt};
use crate::values::memohash::MemoHashed;
use crate::values::sequence::SequenceValue;

pub use number::NumberValue;

pub trait ValueExt: AnyValueExt {
    fn hash64(&self) -> u64 {
        let xxh64 = IncrementalXXH64::default();
        let seed = 1;
        xxh64.finalize(self.update_hash(&xxh64, xxh64.initialize(seed)))
    }

    fn update_hash(&self, hash_function: &impl HashFunction, hash: u64) -> u64;

    fn pretty_print(&self) -> String;
}

#[derive(PartialEq, Measurable)]
pub enum Value {
    Scalar(ScalarValue),
    Array(ArrayValue),
}

pub trait ScalarValueExt: ValueExt {}

#[derive(PartialEq, Measurable)]
pub enum ScalarValue {
    Bool(bool::BoolValue),
    Number(NumberValue),
    Text(TextValue),
    Duration(DurationValue),
    Point(PointValue),
    Temporal(TemporalValue),
}

pub trait ArrayValueExt: ValueExt + MemoHashed + SequenceValue {
    fn size(&self) -> usize;
    fn has_compatible_type(&self) -> bool;
    fn copy_with_appended(&self, appended: AnyValue) -> ArrayValue;
    fn copy_with_prepended(&self, prepended: AnyValue) -> ArrayValue;
}

#[derive(PartialEq, Measurable)]
pub enum ArrayValue {
    Bool(BoolArray),
    Number(NumberArray),
    Text(TextArray),
    Duration(DurationArray),
    Point(PointArray),
    Temporal(TemporalArray),
}