use std::fmt::{Display, Formatter};
use paste::paste;
use crate::common::hashing::HashFunction;
use crate::common::memory::Measurable;
use crate::values::AnyValueExt;
use crate::values::mapper::ValueMapper;
use crate::values::memohash::{HashValue, Hashable};
use crate::values::representation::reprs::{FLOAT32_REPR, FLOAT64_REPR, INT16_REPR, INT32_REPR, INT64_REPR, INT8_REPR, UINT16_REPR, UINT32_REPR, UINT64_REPR, UINT8_REPR};
use crate::values::representation::ValueRepresentation;
use crate::values::storable::{ScalarValueExt, ValueExt};
use crate::values::writer::AnyValueWriter;

#[repr(u8)]
#[derive(PartialEq)]
enum NumberKind {
	I8,
	I16,
	I32,
	I64,
	U8,
	U16,
	U32,
	U64,
	F32,
	F64,
}

#[derive(PartialEq, Measurable)]
pub struct NumberValue {
	kind: NumberKind,
	raw: u64, // raw bits
}

impl NumberValue {
	pub fn is_integral(&self) -> bool {
		!matches!(self.kind, NumberKind::F32 | NumberKind::F64)
	}
	
	pub fn to_runtime(&self) -> RuntimeNumber {
		match self.kind {
			NumberKind::I8
			| NumberKind::I16
			| NumberKind::I32
			| NumberKind::I64 => RuntimeNumber::Signed(self.raw as i64),

			NumberKind::U8
			| NumberKind::U16
			| NumberKind::U32
			| NumberKind::U64 => RuntimeNumber::Unsigned(self.raw),

			NumberKind::F32 => RuntimeNumber::Float(
				f32::from_bits(self.raw as u32) as f64
			),
			NumberKind::F64 => RuntimeNumber::Float(
				f64::from_bits(self.raw)
			),
		}
	}

	pub fn canonicalize(&self) -> CanonicalNumber {
		match self.kind {
			NumberKind::I8
			| NumberKind::I16
			| NumberKind::I32
			| NumberKind::I64 => {
				CanonicalNumber::I64(self.raw as i64)
			}

			NumberKind::U8
			| NumberKind::U16
			| NumberKind::U32
			| NumberKind::U64 => {
				CanonicalNumber::U64(self.raw)
			}

			NumberKind::F32 => {
				let v = f32::from_bits(self.raw as u32) as f64;
				canonicalize_f64(v)
			}

			NumberKind::F64 => {
				let v = f64::from_bits(self.raw);
				canonicalize_f64(v)
			}
		}
	}

	pub fn is_nan(&self) -> bool {
		match self.kind {
			NumberKind::F32 => self.as_f32().is_nan(),
			NumberKind::F64 => self.as_f64().is_nan(),
			_ => false,
		}
	}

	pub fn as_f32(&self) -> f32 {
		f32::from_bits(self.raw as u32)
	}

	pub fn as_f64(&self) -> f64 {
		f64::from_bits(self.raw)
	}
}

impl Hashable for NumberValue {
	fn hash(&self) -> HashValue {
		hash_u64(self.raw).into()
	}
}

impl Display for NumberValue {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		match self.kind {
			NumberKind::I8 => write!(f, "{}({})", self.get_type_name(), self.as_i8()),
			NumberKind::I16 => write!(f, "{}({})", self.get_type_name(), self.as_i16()),
			NumberKind::I32 => write!(f, "{}({})", self.get_type_name(), self.as_i32()),
			NumberKind::I64 => write!(f, "{}({})", self.get_type_name(), self.as_i64()),
			NumberKind::U8 => write!(f, "{}({})", self.get_type_name(), self.as_u8()),
			NumberKind::U16 => write!(f, "{}({})", self.get_type_name(), self.as_u16()),
			NumberKind::U32 => write!(f, "{}({})", self.get_type_name(), self.as_u32()),
			NumberKind::U64 => write!(f, "{}({})", self.get_type_name(), self.as_u64()),
			NumberKind::F32 => write!(f, "{}({})", self.get_type_name(), self.as_f32()),
			NumberKind::F64 => write!(f, "{}({})", self.get_type_name(), self.as_f64()),
		}
	}
}

impl AnyValueExt for NumberValue {
	fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> crate::values::error::Result<()> {
		writer.write_number(self)
	}

	fn map<T>(&self, mapper: &impl ValueMapper<T>) {
		mapper.map_number(self);
	}

	fn representation(&self) -> ValueRepresentation {
		match self.kind {
			NumberKind::I8 => INT8_REPR,
			NumberKind::I16 => INT16_REPR,
			NumberKind::I32 => INT32_REPR,
			NumberKind::I64 => INT64_REPR,
			NumberKind::U8 => UINT8_REPR,
			NumberKind::U16 => UINT16_REPR,
			NumberKind::U32 => UINT32_REPR,
			NumberKind::U64 => UINT64_REPR,
			NumberKind::F32 => FLOAT32_REPR,
			NumberKind::F64 => FLOAT64_REPR,
		}
	}

	fn get_type_name(&self) -> &str {
		match self.kind {
			NumberKind::I8 => "Byte",
			NumberKind::I16 => "Int16",
			NumberKind::I32 => "Int32",
			NumberKind::I64 => "Int64",
			NumberKind::U8 => "UnsignedByte",
			NumberKind::U16 => "UnsignedInt16",
			NumberKind::U32 => "UnsignedInt32",
			NumberKind::U64 => "UnsignedInt64",
			NumberKind::F32 => "Float",
			NumberKind::F64 => "Double",
		}
	}
}

impl ValueExt for NumberValue {
	fn update_hash(&self, hash_function: &impl HashFunction, hash: u64) -> u64 {
		hash_function.update(hash, self.raw)
	}

	fn pretty_print(&self) -> String {
		match self.kind {
			NumberKind::I8
			| NumberKind::I16
			| NumberKind::I32
			| NumberKind::I64 => self.as_i64().to_string(),
			NumberKind::U8
			| NumberKind::U16
			| NumberKind::U32
			| NumberKind::U64 => self.as_u64().to_string(),
			NumberKind::F32 => self.as_f32().to_string(),
			NumberKind::F64 => self.as_f64().to_string(),
		}
	}
}

impl ScalarValueExt for NumberValue {}

macro_rules! impl_from_nums {
    ($(Num = $num:ident, Mask = $mask:ident, Kind = $kind:ident);*) => {
		paste!{
			impl NumberValue {
				$(
					pub fn [<from_$num>](num: $num) -> Self {
						NumberValue {
							kind: NumberKind::$kind,
							raw: num as $mask as u64,
						}
					}
				)*
			}

			$(
				impl From<$num> for NumberValue {
					fn from(num: $num) -> Self {
						NumberValue::[<from_$num>](num)
					}
				}
			)*
		}
	};
}

macro_rules! impl_to_nums {
    ($(Num = $num:ident, Kind = $kind:ident);*) => {
		paste!{
			impl NumberValue {
				$(
					pub fn [<as_$num>](&self) -> $num {
						self.raw as $num
					}

					pub fn [<to_$num>](&self) -> Option<$num> {
						match self.kind {
							NumberKind::$kind => Some(self.raw as $num),
							_ => None
						}
					}
				)*
			}
		}

		$(
			impl From<NumberValue> for $num {
				fn from(value: NumberValue) -> Self {
					value.raw as $num
				}
			}
		)*
	};
}

impl_from_nums!(
	Num = i8, Mask = i64, Kind = I8;
	Num = i16, Mask = i64, Kind = I16;
	Num = i32, Mask = i64, Kind = I32;
	Num = i64, Mask = i64, Kind = I64;
	Num = u8, Mask = u64, Kind = U8;
	Num = u16, Mask = u64, Kind = U16;
	Num = u32, Mask = u64, Kind = U32;
	Num = u64, Mask = u64, Kind = U64
);

impl_to_nums!(
	Num = i8, Kind = I8;
	Num = i16,  Kind = I16;
	Num = i32,  Kind = I32;
	Num = i64,  Kind = I64;
	Num = u8, Kind = U8;
	Num = u16,  Kind = U16;
	Num = u32,  Kind = U32;
	Num = u64,  Kind = U64
);

impl From<f32> for NumberValue {
	fn from(value: f32) -> Self {
		NumberValue {
			kind: NumberKind::F32,
			raw: value.to_bits() as u64,
		}
	}
}

impl From<f64> for NumberValue {
	fn from(value: f64) -> Self {
		NumberValue {
			kind: NumberKind::F32,
			raw: value.to_bits(),
		}
	}
}

enum CanonicalNumber {
	I64(i64),
	U64(u64),
	F64(f64),
}

fn f64_as_exact_int(v: f64) -> Option<i64> {
	if v.is_finite() {
		let i = v as i64;
		if (i as f64) == v {
			return Some(i);
		}
	}
	None
}

fn canonicalize_f64(v: f64) -> CanonicalNumber {
	if let Some(i) = f64_as_exact_int(v) {
		CanonicalNumber::I64(i)
	} else {
		CanonicalNumber::F64(v)
	}
}

fn hash_i64(v: i64) -> u32 {
	let as_i32 = v as i32;
	if as_i32 as i64 == v {
		as_i32 as u32
	} else {
		let x = v as u64;
		(x ^ (x >> 32)) as u32
	}
}

fn hash_u64(v: u64) -> u32 {
	let as_u32 = v as u32;
	if as_u32 as u64 == v {
		as_u32
	} else {
		(v ^ (v >> 32)) as u32
	}
}

fn hash_f64(v: f64) -> u32 {
	let bits = v.to_bits();
	(bits ^ (bits >> 32)) as u32
}

fn hash_number(n: &NumberValue) -> u32 {
	match n.canonicalize() {
		CanonicalNumber::I64(v) => hash_i64(v),
		CanonicalNumber::U64(v) => hash_u64(v),
		CanonicalNumber::F64(v) => hash_f64(v),
	}
}