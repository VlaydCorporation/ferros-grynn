use std::cell::{Cell};
use crate::common::memory::{HeapEstimatorCache, Measurable};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};
use crate::impl_simple_hashable;
use crate::unsafed::{shallow_size_of, size_of_vec};
use crate::values::error::{Result, ValueError};
use crate::values::mapper::ValueMapper;
use crate::values::memohash::{HASH_CONSTANT, HashValue, Hashable, MemoHashed, MemoizedHash, hash_hashables, hash_u64};
use crate::values::representation::reprs::{ANYTHING_REPR, INT64_REPR};
use crate::values::sequence::SequenceValue;
use crate::values::virt::VirtualValueExt;
use crate::values::virt::path::VirtualPathValue;
use crate::values::virt::relationship::VirtualRelationshipValue;
use crate::values::writer::AnyValueWriter;
use crate::values::{AnyValue, AnyValueExt, ValueRepresentation};
use itertools::Itertools;
use magic_utils::{Display, IntoVariants};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use num::traits::ConstOne;
use crate::collections::iter::{AppendIterator, PrependIterator};
use crate::values::storable::number::NumberValue;

//#region ----------------- MACROS -----------------
macro_rules! impl_any_ext {
    ($($ident:ident),+) => {
        $(
            impl AnyValueExt for $ident {
                fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
                    writer.begin_list(self.size())?;
                    for e in self.into_iter() {
                        e.write_to(writer)?;
                    }
                    writer.end_list()
                }

                fn map<T>(&self, mapper: &impl ValueMapper<T>) {
                    mapper.map_sequence(self);
                }

                fn representation(&self) -> ValueRepresentation {
                    todo!()
                }

                fn get_type_name(&self) -> &str {
                    "List"
                }
            }
        )*
    };
}

macro_rules! impl_display {
    ($($ident:ident),+) => {
		$(
			impl ::std::fmt::Display for $ident {
				fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> std::fmt::Result {
					write!(f, "{}{{", self.get_type_name())?;
                    write!(f, "{}", self.into_iter().join(", "))?;
					write!(f, "}}")
				}
			}
		)*
	};
}
//#endregion ----------------- MACROS -----------------

//#region ----------------- TRAITS -----------------
pub trait ListValueExt: VirtualValueExt + SequenceValue {
	fn item_value_representation(&self) -> ValueRepresentation;
	fn non_empty(&self) -> bool {
		!self.is_empty()
	}
	fn first(&self) -> Result<&AnyValue> {
		if self.is_empty() {
			Err(ValueError::NoSuchElement("first of empty list"))
		} else {
			Ok(self.value_at(0).unwrap())
		}
	}
	fn last(&self) -> Result<&AnyValue> {
		if self.is_empty() {
			Err(ValueError::NoSuchElement("last of empty list"))
		} else {
			Ok(self.value_at(self.size() - 1).unwrap())
		}
	}

	fn slice(&self, from: usize, to: usize) -> ListValue {
		let to = to.min(self.size());
		if from > to {
			EMPTY_LIST.into()
		} else {
			ListSliceValue::new(self, from, to).into()
		}
	}

	fn tail_slice(&self) -> ListValue {
		self.slice(1, self.size())
	}

	fn drop(&self, n: usize) -> ListValue {
		let size = self.size();
		let start = 0.max(n.min(size));
		ListSliceValue::new(self, start, size).into()
	}

	fn take(&self, n: usize) -> ListValue {
		let end = 0.max(n.min(self.size()));
		ListSliceValue::new(self, 0, end).into()
	}

	fn reverse(&self) -> ListValue {
		ReversedList::new(self).into()
	}

	fn append(&self, value: AnyValue) -> ListValue {
		AppendList::new(self, value).into()
	}

	fn prepend(&self, value: AnyValue) -> ListValue {
		PrependList::new(self, value).into()
	}

	fn append_all(&self, value: ListValue) -> ListValue {
		ConcatList::new(&[self.into(), value]).into()
	}

	fn distinct(&self) -> ListValue {
		let mut kept_values_heap_size = 0;
		let mut seen = HashSet::new();
		let mut kept = Vec::new();
		let mut representation = ANYTHING_REPR;
		for value in self.into_iter() {
			if seen.insert(value) {
				kept.push(value);
				kept_values_heap_size += value.estimated_heap_usage();
			}
			representation = representation.coerce(value.value_representation());
		}
		VecList::new(kept, kept_values_heap_size, representation).into()
	}

	fn to_storable_array(&self) -> FerrosGrynnResult<ArrayValue>
	where
		Self: Sized,
	{
		if self.is_empty() {
			EMPTY_TEXT_ARRAY
		} else {
			self.item_value_representation().array_of(self)
		}
	}
}
//#endregion ----------------- TRAITS -----------------

//#region ----------------- LIST VALUE -----------------
#[derive(PartialEq, IntoVariants, Display, Measurable)]
pub enum ListValue {
	ArrayValueList(ArrayValueListValue),
	RelationshipList(RelationshipListValue),
	VecList(VecListValue),
	ListSlice(ListSliceValue),
	ReversedList(ReversedListValue),
	ConcatList(ConcatListValue),
	AppendList(AppendListValue),
	PrependList(PrependListValue),
	IntegralRangeList(IntegralRangeListValue),
}

impl Hashable for ListValue {
	fn hash(&self) -> HashValue {
		match self {
			ListValue::ArrayValueList(av) => av.hash(),
			ListValue::RelationshipList(rel) => rel.hash(),
			ListValue::VecList(v) => v.hash(),
			ListValue::ListSlice(l) => l.hash(),
			ListValue::ReversedList(rev) => rev.hash(),
			ListValue::ConcatList(c) => c.hash(),
			ListValue::AppendList(a) => a.hash(),
			ListValue::PrependList(p) => p.hash(),
			ListValue::IntegralRangeList(i) => i.hash(),
		}
	}
}

impl MemoHashed for ListValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		match self {
			ListValue::ArrayValueList(av) => av.compute_hash_to_memoize(),
			ListValue::RelationshipList(rel) => rel.compute_hash_to_memoize(),
			ListValue::VecList(v) => v.compute_hash_to_memoize(),
			ListValue::ListSlice(l) => l.compute_hash_to_memoize(),
			ListValue::ReversedList(rev) => rev.compute_hash_to_memoize(),
			ListValue::ConcatList(c) => c.compute_hash_to_memoize(),
			ListValue::AppendList(a) => a.compute_hash_to_memoize(),
			ListValue::PrependList(p) => p.compute_hash_to_memoize(),
			ListValue::IntegralRangeList(i) => i.compute_hash_to_memoize(),
		}
	}
}

impl VirtualValueExt for ListValue {}

impl IntoIterator for ListValue {
	type Item = AnyValue;
	type IntoIter = ();

	fn into_iter(self) -> Self::IntoIter {
		match self {
			ListValue::ArrayValueList(av) => av.into_iter(),
			ListValue::RelationshipList(rel) => rel.into_iter(),
			ListValue::VecList(v) => v.into_iter(),
			ListValue::ListSlice(l) => l.into_iter(),
			ListValue::ReversedList(rev) => rev.into_iter(),
			ListValue::ConcatList(c) => c.into_iter(),
			ListValue::AppendList(a) => a.into_iter(),
			ListValue::PrependList(p) => p.into_iter(),
			ListValue::IntegralRangeList(i) => i.into_iter(),
		}
	}
}

impl SequenceValue for ListValue {
	fn size(&self) -> usize {
		match self {
			ListValue::ArrayValueList(av) => av.size(),
			ListValue::RelationshipList(rel) => rel.size(),
			ListValue::VecList(v) => v.size(),
			ListValue::ListSlice(l) => l.size(),
			ListValue::ReversedList(rev) => rev.size(),
			ListValue::ConcatList(c) => c.size(),
			ListValue::AppendList(a) => a.size(),
			ListValue::PrependList(p) => p.size(),
			ListValue::IntegralRangeList(i) => i.size(),
		}
	}

	fn is_empty(&self) -> bool {
		match self {
			ListValue::ArrayValueList(av) => av.is_empty(),
			ListValue::RelationshipList(rel) => rel.is_empty(),
			ListValue::VecList(v) => v.is_empty(),
			ListValue::ListSlice(l) => l.is_empty(),
			ListValue::ReversedList(rev) => rev.is_empty(),
			ListValue::ConcatList(c) => c.is_empty(),
			ListValue::AppendList(a) => a.is_empty(),
			ListValue::PrependList(p) => p.is_empty(),
			ListValue::IntegralRangeList(i) => i.is_empty(),
		}
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue> {
		match self {
			ListValue::ArrayValueList(av) => av.value_at(offset),
			ListValue::RelationshipList(rel) => rel.value_at(offset),
			ListValue::VecList(v) => v.value_at(offset),
			ListValue::ListSlice(l) => l.value_at(offset),
			ListValue::ReversedList(rev) => rev.value_at(offset),
			ListValue::ConcatList(c) => c.value_at(offset),
			ListValue::AppendList(a) => a.value_at(offset),
			ListValue::PrependList(p) => p.value_at(offset),
			ListValue::IntegralRangeList(i) => i.value_at(offset),
		}
	}
}

impl ListValueExt for ListValue {
	fn item_value_representation(&self) -> ValueRepresentation {
		match self {
			ListValue::ArrayValueList(av) => av.item_value_representation(),
			ListValue::RelationshipList(rel) => rel.item_value_representation(),
			ListValue::VecList(v) => v.item_value_representation(),
			ListValue::ListSlice(l) => l.item_value_representation(),
			ListValue::ReversedList(rev) => rev.item_value_representation(),
			ListValue::ConcatList(c) => c.item_value_representation(),
			ListValue::AppendList(a) => a.item_value_representation(),
			ListValue::PrependList(p) => p.item_value_representation(),
			ListValue::IntegralRangeList(i) => i.item_value_representation(),
		}
	}
}

#[derive(PartialEq, Measurable)]
struct ArrayValueListValue {
	#[m(add_m)]
	array: ArrayValue,
	hash: MemoizedHash,
}

impl ArrayValueListValue {
	pub fn new(array: ArrayValue) -> Self {
		Self {
			array,
			hash: MemoizedHash::new(),
		}
	}
}

impl MemoHashed for ArrayValueListValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		self.array.compute_hash_to_memoize()
	}
}

impl VirtualValueExt for ArrayValueListValue {}

impl IntoIterator for ArrayValueListValue {
	type Item = AnyValue;
	type IntoIter = ();

	fn into_iter(self) -> Self::IntoIter {
		self.array.into_iter()
	}
}

impl SequenceValue for ArrayValueListValue {
	fn size(&self) -> usize {
		self.array.size()
	}

	fn is_empty(&self) -> bool {
		self.array.is_empty()
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue> {
		self.array.value_at(offset)
	}
}

impl ListValueExt for ArrayValueListValue {
	fn item_value_representation(&self) -> ValueRepresentation {
		self.first().map_or(ANYTHING_REPR, |v| v.representation())
	}

	fn to_storable_array(&self) -> FerrosGrynnResult<ArrayValue>
	where
		Self: Sized,
	{
		Ok(self.array)
	}
}

#[derive(PartialEq)]
struct RelationshipListValue {
	list: Vec<VirtualRelationshipValue>,
	hash: MemoizedHash,
}

impl RelationshipListValue {
	pub fn new(list: &[VirtualRelationshipValue]) -> Self {
		Self {
			list: list.to_vec(),
			hash: MemoizedHash::new(),
		}
	}
}

impl Measurable for RelationshipListValue {
	fn estimated_heap_usage(&self) -> usize {
		let size = self.size();
		if size == 0 {
			shallow_size_of::<Self>()
		} else {
			shallow_size_of::<Self>() + size_of_vec(&self.list)
		}
	}
}

impl MemoHashed for RelationshipListValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		let mut hash = 1;
		self.list.iter().for_each(|v| hash = HASH_CONSTANT * hash + v.hash());
		hash
	}
}

impl VirtualValueExt for RelationshipListValue {}

impl IntoIterator for RelationshipListValue {
	type Item = AnyValue;
	type IntoIter = ();

	fn into_iter(self) -> Self::IntoIter {
		self.list.into_iter()
	}
}

impl SequenceValue for RelationshipListValue {
	fn size(&self) -> usize {
		self.list.len()
	}

	fn is_empty(&self) -> bool {
		self.list.is_empty()
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue> {
		self.list.get(offset).map(|v| v.clone().into())
	}
}

impl ListValueExt for RelationshipListValue {
	fn item_value_representation(&self) -> ValueRepresentation {
		ANYTHING_REPR
	}

	fn to_storable_array(&self) -> ArrayValue
	where
		Self: Sized,
	{
		Err(FerrosGrynnError::cypher_type_message(
			"Collections containing relationship values can not be stored in properties.",
		))
	}
}

#[derive(PartialEq, Measurable)]
struct VecListValue {
	values: Vec<AnyValue>,
	#[m(add)]
	payload_size: usize,
	item_representation: ValueRepresentation,
	hash: MemoizedHash,
}

impl VecListValue {
	pub fn new(
		values: Vec<AnyValue>,
		payload_size: usize,
		item_representation: ValueRepresentation,
	) -> Self {
		// TODO: eliminate asserts
		assert!(assert_value_representation(values.as_slice(), item_representation));
		Self {
			values,
			payload_size,
			item_representation,
			hash: MemoizedHash::new(),
		}
	}
}

impl MemoHashed for VecListValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		hash_hashables(self.values.as_slice())
	}
}

impl VirtualValueExt for VecListValue {}

impl IntoIterator for VecListValue {
	type Item = AnyValue;
	type IntoIter = ();

	fn into_iter(self) -> Self::IntoIter {
		self.values.into_iter()
	}
}

impl SequenceValue for VecListValue {
	fn size(&self) -> usize {
		self.values.len()
	}

	fn is_empty(&self) -> bool {
		self.values.is_empty()
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue> {
		self.values.get(offset).cloned()
	}
}

impl ListValueExt for VecListValue {
	fn item_value_representation(&self) -> ValueRepresentation {
		self.item_representation
	}
}

#[derive(PartialEq, Measurable)]
struct ListSliceValue {
	#[m(add_m)]
	inner: Box<ListValue>,
	from: usize,
	to: usize,
	hash: MemoizedHash,
}

impl ListSliceValue {
	pub fn new(inner: ListValue, from: usize, to: usize) -> Self {
		// TODO: eliminate asserts
		assert!(to < inner.size() && from <= to);
		Self {
			inner: Box::new(inner),
			from,
			to,
			hash: MemoizedHash::new(),
		}
	}
}

impl MemoHashed for ListSliceValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		hash_hashables(self.into_iter())
	}
}

impl VirtualValueExt for ListSliceValue {}

impl IntoIterator for ListSliceValue {
	type Item = AnyValue;
	type IntoIter = ListSliceIterator;

	fn into_iter(self) -> Self::IntoIter {
		ListSliceIterator::new(self.inner.into_iter(), self.from, self.to)
	}
}

impl SequenceValue for ListSliceValue {
	fn size(&self) -> usize {
		self.to - self.from
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue> {
		self.inner.value_at(offset + self.from)
	}
}

impl ListValueExt for ListSliceValue {
	fn item_value_representation(&self) -> ValueRepresentation {
		self.inner.item_value_representation()
	}
}

#[derive(PartialEq, Measurable)]
struct ReversedListValue {
	#[m(add_m)]
	inner: Box<ListValue>,
	hash: MemoizedHash,
}

impl ReversedListValue {
	fn new(inner: ListValue) -> Self {
		Self {
			inner: Box::new(inner),
			hash: MemoizedHash::new(),
		}
	}
}

impl MemoHashed for ReversedListValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		hash_hashables(self.into_iter())
	}
}

impl VirtualValueExt for ReversedListValue {}

impl IntoIterator for ReversedListValue {
	type Item = AnyValue;
	type IntoIter = ();

	fn into_iter(self) -> Self::IntoIter {
		self.inner.into_iter()
	}
}

impl SequenceValue for ReversedListValue {
	fn size(&self) -> usize {
		self.inner.size()
	}

	fn is_empty(&self) -> bool {
		self.inner.is_empty()
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue> {
		self.inner.value_at(self.size() - 1 - offset)
	}
}

impl ListValueExt for ReversedListValue {
	fn item_value_representation(&self) -> ValueRepresentation {
		self.inner.item_value_representation()
	}
}

#[derive(PartialEq)]
struct ConcatListValue {
	lists: Vec<ListValue>,
	item_value_representation: ValueRepresentation,
	size: usize,
	hash: MemoizedHash,
}

impl ConcatListValue {
	pub fn new(lists: Vec<ListValue>) -> Self {
		let mut repr = ANYTHING_REPR;
		let mut size = 0;
		lists.iter().for_each(|l| {
			repr = repr.coerce(l.item_value_representation());
			size += l.size();
		});
		Self {
			lists,
			size,
			item_value_representation: repr,
			hash: MemoizedHash::new(),
		}
	}
}

impl Measurable for ConcatListValue {
	fn estimated_heap_usage(&self) -> usize {
		let mut s = 0;
		self.lists.iter().for_each(|v| s += v.estimated_heap_usage());
		shallow_size_of::<ConcatListValue>() + s
	}

	fn estimated_heap_usage_cache(&self, estimator_cache: &impl HeapEstimatorCache) -> usize
	where
		Self: Sized,
	{
		let mut s = 0;
		self.lists.iter().for_each(|v| s += v.estimated_heap_usage_cache(estimator_cache));
		shallow_size_of::<ConcatListValue>() + s
	}
}

impl MemoHashed for ConcatListValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		hash_hashables(self.into_iter())
	}
}

impl VirtualValueExt for ConcatListValue {}

impl IntoIterator for ConcatListValue {
	type Item = AnyValue;
	type IntoIter = ConcatListIterator;

	fn into_iter(self) -> Self::IntoIter {
		ConcatListIterator::new(self.lists)
	}
}

impl SequenceValue for ConcatListValue {
	fn size(&self) -> usize {
		self.size
	}

	fn is_empty(&self) -> bool {
		self.lists.iter().all(|list| list.is_empty())
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue> {
		let mut offset = offset;
		for list in self.lists.iter() {
			let size = list.size();
			if offset < size {
				return list.value_at(offset);
			}
			offset -= size;
		}
		None
	}
}

impl ListValueExt for ConcatListValue {
	fn item_value_representation(&self) -> ValueRepresentation {
		self.item_value_representation
	}

	fn append_all(&self, value: ListValue) -> ListValue {
		let mut new_lists = Vec::with_capacity(self.lists.len() + 1);
		new_lists.extend(self.lists.iter().cloned());
		new_lists.push(value);
		ConcatListValue::new(new_lists).into()
	}
}

#[derive(PartialEq)]
struct AppendListValue {
	base: Box<ListValue>,
	appended: AnyValue,
	size: Cell<Option<usize>>,
	memoized_estimated_heap_usage: Cell<Option<usize>>,
	hash: MemoizedHash,
}

impl AppendListValue {
	pub fn new(base: ListValue, appended: AnyValue) -> Self {
		Self {
			base: Box::new(base),
			appended,
			size: Cell::new(None),
			memoized_estimated_heap_usage: Cell::new(None),
			hash: MemoizedHash::new(),
		}
	}
}

impl Measurable for AppendListValue {
	fn estimated_heap_usage(&self) -> usize {
		if let Some(usage) = self.memoized_estimated_heap_usage.get() {
			usage
		} else {
			let tmp = shallow_size_of::<AppendListValue>() + self.base.estimated_heap_usage() + self.appended.estimated_heap_usage();
			self.memoized_estimated_heap_usage.set(Some(tmp));
			tmp
		}
	}

	fn estimated_heap_usage_cache(&self, estimator_cache: &impl HeapEstimatorCache) -> usize
	where
		Self: Sized,
	{
		let mut estimate = self.memoized_estimated_heap_usage.get();
		if estimate.is_none() {
			estimate = Some(shallow_size_of::<AppendListValue>() + self.base.estimated_heap_usage_cache(estimator_cache) + self.appended.estimated_heap_usage_cache(estimator_cache));
			self.memoized_estimated_heap_usage.set(estimate);
		}
		estimator_cache.estimated_heap_usage(self, estimate.unwrap())
	}
}

impl MemoHashed for AppendListValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		hash_hashables(self.into_iter())
	}
}

impl VirtualValueExt for AppendListValue {}

impl IntoIterator for AppendListValue {
	type Item = AnyValue;
	type IntoIter = AppendIterator<<ListValue as IntoIterator>::IntoIter, Self::Item>;

	fn into_iter(self) -> Self::IntoIter {
		AppendIterator::new(self.base.into_iter(), vec![self.appended])
	}
}

impl SequenceValue for AppendListValue {
	fn size(&self) -> usize {
		if let Some(size) = self.size.get() {
			size
		} else {
			let tmp = self.base.size() + 1;
			self.size.set(Some(tmp));
			tmp
		}
	}

	fn is_empty(&self) -> bool {
		false
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue> {
		let size = self.base.size();
		if offset < size {
			self.base.value_at(offset)
		} else if offset == size {
			Some(&self.appended)
		} else {
			None
		}
	}
}

impl ListValueExt for AppendListValue {
	fn item_value_representation(&self) -> ValueRepresentation {
		if self.base.is_empty() {
			self.appended.representation()
		} else {
			self.base.item_value_representation().coerce(self.appended.representation())
		}
	}

	fn last(&self) -> Result<&AnyValue> {
		Ok(&self.appended)
	}

	fn to_storable_array(&self) -> FerrosGrynnResult<ArrayValue>
	where
		Self: Sized,
	{
		match *self.base {
			ListValue::ArrayValueList(ArrayValueListValue {array, ..}) if array.has_compatible_type(self.appended) => {
				array.copy_with_appended(self.appended)
			}
			_ => {
				if self.is_empty() {
					EMPTY_TEXT_ARRAY
				} else {
					self.item_value_representation().array_of(self)
				}
			}
		}
	}
}

#[derive(PartialEq)]
struct PrependListValue {
	base: Box<ListValue>,
	prepended: AnyValue,
	size: Cell<Option<usize>>,
	memoized_estimated_heap_usage: Cell<Option<usize>>,
	hash: MemoizedHash,
}

impl PrependListValue {
	pub fn new(base: ListValue, prepended: AnyValue) -> Self {
		Self {
			base: Box::new(base),
			prepended,
			size: Cell::new(None),
			memoized_estimated_heap_usage: Cell::new(None),
			hash: MemoizedHash::new(),
		}
	}
}

impl Measurable for PrependListValue {
	fn estimated_heap_usage(&self) -> usize {
		if let Some(usage) = self.memoized_estimated_heap_usage.get() {
			usage
		} else {
			let tmp = shallow_size_of::<AppendListValue>() + self.base.estimated_heap_usage() + self.prepended.estimated_heap_usage();
			self.memoized_estimated_heap_usage.set(Some(tmp));
			tmp
		}
	}

	fn estimated_heap_usage_cache(&self, estimator_cache: &impl HeapEstimatorCache) -> usize
	where
		Self: Sized,
	{
		let mut estimate = self.memoized_estimated_heap_usage.get();
		if estimate.is_none() {
			estimate = Some(shallow_size_of::<AppendListValue>() + self.base.estimated_heap_usage_cache(estimator_cache) + self.prepended.estimated_heap_usage_cache(estimator_cache));
			self.memoized_estimated_heap_usage.set(estimate);
		}
		estimator_cache.estimated_heap_usage(self, estimate.unwrap())
	}
}

impl MemoHashed for PrependListValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		hash_hashables(self.into_iter())
	}
}

impl VirtualValueExt for PrependListValue {}

impl IntoIterator for PrependListValue {
	type Item = AnyValue;
	type IntoIter = PrependIterator<<ListValue as IntoIterator>::IntoIter, Self::Item>;

	fn into_iter(self) -> Self::IntoIter {
		PrependIterator::new(self.base.into_iter(), vec![self.prepended])
	}
}

impl SequenceValue for PrependListValue {
	fn size(&self) -> usize {
		if let Some(size) = self.size.get() {
			size
		} else {
			let tmp = self.base.size() + 1;
			self.size.set(Some(tmp));
			tmp
		}
	}

	fn is_empty(&self) -> bool {
		false
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue> {
		if offset == 0 {
			Some(&self.prepended)
		} else if offset < self.base.size() + 1 {
			self.base.value_at(offset - 1)
		} else {
			None
		}
	}
}

impl ListValueExt for PrependListValue {
	fn item_value_representation(&self) -> ValueRepresentation {
		if self.base.is_empty() {
			self.prepended.representation()
		} else {
			self.base.item_value_representation().coerce(self.prepended.representation())
		}
	}

	fn first(&self) -> Result<&AnyValue> {
		Ok(&self.prepended)
	}

	fn to_storable_array(&self) -> FerrosGrynnResult<ArrayValue>
	where
		Self: Sized,
	{
		match *self.base {
			ListValue::ArrayValueList(ArrayValueListValue {array, ..}) if array.has_compatible_type(self.prepended) => {
				array.copy_with_prepended(self.prepended)
			}
			_ => self.base.to_storable_array(),
		}
	}
}

#[derive(PartialEq, Measurable)]
pub struct IntegralRangeListValue {
	start: i64,
	end: i64,
	step: usize,
	length: usize,
	hash: MemoizedHash,
}

impl IntegralRangeListValue {
	pub fn new(start: i64, end: i64, step: usize) -> Self {
		let length = (end - start) / (step as i64) + 1;
		Self {
			start,
			end,
			step,
			length: length.max(0) as usize,
			hash: MemoizedHash::new(),
		}
	}
}

impl Display for IntegralRangeListValue {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "Range({}...{}, step = {})", self.start, self.end, self.step)
	}
}

impl MemoHashed for IntegralRangeListValue {
	fn compute_hash_to_memoize(&self) -> HashValue {
		let mut h = HashValue::ONE;
		let mut current = self.start;
		let size = self.size();
		for i in 0..size {
			h = HASH_CONSTANT * h + hash_u64(current);
			current += self.step as i64;
		}
		h
	}
}

impl VirtualValueExt for IntegralRangeListValue {}

impl IntoIterator for IntegralRangeListValue {
	type Item = AnyValue;
	type IntoIter = IntegralRangeIter;

	fn into_iter(self) -> Self::IntoIter {
		IntegralRangeIter::new(self.start, self.length)
	}
}

impl SequenceValue for IntegralRangeListValue {
	fn size(&self) -> usize {
		self.length
	}

	fn is_empty(&self) -> bool {
		self.length == 0
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue> {
		if offset >= self.length {
			None
		} else {
			let value = self.start + (offset as i64) * self.step as i64;
			Some(NumberValue::Int(value).into())
		}
	}
}

impl ListValueExt for IntegralRangeListValue {
	fn item_value_representation(&self) -> ValueRepresentation {
		INT64_REPR
	}

	fn to_storable_array(&self) -> FerrosGrynnResult<ArrayValue>
	where
		Self: Sized,
	{
		let size = self.size();
		let mut current = self.start;
		let array = Vec::with_capacity(size);
		for i in 0..size {
			array[i] = current;
			current += self.step as i64;
		}
		NumberArray::from_int_vec(array).into()
	}
}
//#endregion ----------------- LIST VALUE -----------------

//#region ----------------- BUILDER -----------------
trait ListValueBuilderExt {
	fn add(&mut self, value: AnyValue);
	fn add_with_cache(&mut self, value: AnyValue, cache: &impl HeapEstimatorCache);
	fn build(self: Box<Self>) -> ListValue;
}

pub struct  ListValueBuilder {
	values: Vec<AnyValue>,
	index: usize,
	representation: ValueRepresentation,
	estimated_heap_size: usize
}

impl ListValueBuilder {
	pub fn new() -> Self {
		Self {
			values: vec![],
			index: 0,
			representation: ANYTHING_REPR,
			estimated_heap_size: 0,
		}
	}

	pub fn with_size(size: usize) -> Self {
		Self {
			values: vec![AnyValue::NoValue; size],
			index: 0,
			representation: ANYTHING_REPR,
			estimated_heap_size: 0,
		}
	}

	pub fn add(&mut self, value: AnyValue) {
		self.estimated_heap_size += value.estimated_heap_usage();
		self.internal_add(value);
	}

	pub fn add_with_cache(&mut self, value: AnyValue, cache: &impl HeapEstimatorCache) {
		self.estimated_heap_size += value.estimated_heap_usage_cache(cache);
		self.internal_add(value);
	}

	fn internal_add(&mut self, value: AnyValue) {
		self.representation = self.representation.coerce(value.representation());
		if self.index < self.values.len() {
			self.values[self.index] = value;
			self.index += 1;
		} else {
			self.values.push(value);
		}
	}

	fn build(self) -> ListValue {
		VecListValue::new(self.values, self.estimated_heap_size, self.representation).into()
	}
}

// Collector
impl FromIterator<AnyValue> for ListValue {
	fn from_iter<I: IntoIterator<Item=AnyValue>>(iter: I) -> Self {
		let mut builder = ListValueBuilder::new();
		for value in iter {
			builder.add(value);
		}
		builder.build()
	}
}

// TODO: maybe rayon::FromParallelIterator
//#endregion ----------------- BUILDER -----------------

//#region ----------------- ITERATORS -----------------
struct ListSliceIterator {
	inner: <ListValue as IntoIterator>::IntoIter,
	index: usize,
	from: usize,
	to: usize,
}

impl ListSliceIterator {
	pub fn new(inner: <ListValue as IntoIterator>::IntoIter, from: usize, to: usize) -> Self {
		Self {
			inner,
			from,
			to,
			index: 0,
		}
	}
}

impl Iterator for ListSliceIterator {
	type Item = AnyValue;

	fn next(&mut self) -> Option<Self::Item> {
		while self.index < self.from {
			self.inner.next()?;
			self.index += 1;
		}

		if self.index >= self.to {
			return None;
		}

		let value = self.inner.next()?;
		self.index += 1;
		Some(value)
	}
}

struct ConcatListIterator {
	lists: Vec<ListValue>,
	inner: <ListValue as IntoIterator>::IntoIter,
	index: usize,
}

impl ConcatListIterator {
	pub fn new(lists: Vec<ListValue>) -> Self {
		Self {
			inner: lists[0].into_iter(),
			lists,
			index: 1
		}
	}
}

impl Iterator for ConcatListIterator {
	type Item = AnyValue;

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some(v) = self.next() {
				return Some(v);
			} else if self.index < self.lists.len() {
				self.inner = self.lists[self.index].into_iter();
				self.index += 1;
			} else {
				return None;
			}
		}
	}
}

struct IntegralRangeIter {
	index: usize,
	start: i64,
	length: usize,
}

impl IntegralRangeIter {
	pub fn new(start: i64, length: usize) -> Self {
		Self {
			start,
			length,
			index: 0,
		}
	}
}

impl Iterator for IntegralRangeIter {
	type Item = AnyValue;

	fn next(&mut self) -> Option<Self::Item> {
		if self.index < self.length {
			let value = self.start + (self.index as i64) * self.length as i64;
			self.index += 1;
			Some(NumberValue::Int(value).into())
		} else {
			None
		}
	}
}
//#endregion ----------------- ITERATORS -----------------

impl_simple_hashable!(
	ArrayValueListValue,
	RelationshipListValue,
	VecListValue,
	ListSliceValue,
	ReversedListValue,
	ConcatListValue,
	AppendListValue,
	PrependListValue,
	IntegralRangeListValue
);
impl_any_ext!(
	ListValue,
	ArrayValueListValue,
	RelationshipListValue,
	VecListValue,
	ListSliceValue,
	ReversedListValue,
	ConcatListValue,
	AppendListValue,
	PrependListValue,
	IntegralRangeListValue
);
impl_display!(
	ArrayValueListValue,
	RelationshipListValue,
	VecListValue,
	ListSliceValue,
	ReversedListValue,
	ConcatListValue,
	AppendListValue,
	PrependListValue
);

fn assert_value_representation(values: &[AnyValue], representation: ValueRepresentation) -> bool {
	let mut actual = ANYTHING_REPR;
	values.iter().for_each(|v| actual = actual.coerce(v.representation()));
	// TODO reviewer: should we require anything here?
	actual == representation
}
