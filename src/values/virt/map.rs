use crate::common::memory::{HeapEstimatorCache, Measurable, size_of_hash_map};
use crate::impl_simple_hashable;
use crate::unsafed::{shallow_size_of, size_of_string};
use crate::values::error::Result;
use crate::values::mapper::ValueMapper;
use crate::values::memohash::{HashValue, Hashable, MemoHashed, MemoizedHash, hash_str};
use crate::values::virt::VirtualValueExt;
use crate::values::writer::AnyValueWriter;
use crate::values::{AnyValue, AnyValueExt, ValueRepresentation};
use magic_utils::{Display, IntoVariants};
use std::collections::{HashMap, HashSet};

//#region ----------------- MACROS -----------------
macro_rules! impl_measurable {
    ($($ident:ident),+) => {
		$(
			impl Measurable for $ident {
				fn estimated_heap_usage(&self) -> usize {
					shallow_size_of::<Self>() + self.map.estimated_heap_usage()
				}

				fn estimated_heap_usage_cache(&self, estimator_cache: &impl HeapEstimatorCache) -> usize
				where
					Self: Sized,
				{
					shallow_size_of::<Self>() + self.map.estimated_heap_usage_cache(estimator_cache)
				}
			}
		)*
	};
}

macro_rules! impl_any_ext {
    ($($ident:ident),+) => {
        $(
            impl AnyValueExt for $ident {
				fn write_to<T: AnyValueWriter>(&self, writer: &mut T) -> Result<()> {
					writer.begin_map(self.size())?;
					self.for_each(|k, v| {
						writer.write_str(k)?;
						v.write_to(writer)
					})?;
					writer.end_map()
				}

				fn map<T>(&self, mapper: &impl ValueMapper<T>) {
					mapper.map_map(self);
				}

				fn representation(&self) -> ValueRepresentation {
					todo!()
				}

				fn get_type_name(&self) -> &str {
					"Map"
				}
			}
        )*
    };
}

macro_rules! impl_hash {
    ($($ident:ident),+) => {
		$(
			impl MemoHashed for $ident {
				fn compute_hash_to_memoize(&self) -> HashValue {
					let mut h: i32 = 0;
					self.for_each(|k, v| {
						h = h.wrapping_add(hash_str(k) ^ v.hash());
						Ok(())
					}).unwrap(); // <- infallible
					h
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
					let mut sep = "";
					self.for_each(|k, v| {
						write!(f, "{}{} -> {}", sep, k, v)?;
						sep = ", ";
						Ok(())
					}).unwrap(); // <- infallible
					write!(f, "}}")
				}
			}
		)*
	};
}

macro_rules! add_default_impls {
    ($s:expr) => {
		fn filter<F>(&self, f: F) -> MapValue
			where
				F: Fn(&str, &AnyValue) -> bool,
			{
				FilteringMapValue::new($s, f).into()
			}
			fn update(&self, key: &str, value: &AnyValue) -> MapValue {
				if !self.contains_key(key) {
					UpdatedMapValue::new($s, key, value).into()
				} else {
					let current = self.get(key);
					if current == value {
						$s
					} else {
						MappedMapValue::new($s, |k, v| {
							if k == key {
								value
							} else {
								v
							}
						})
						.into()
					}
				}
			}
			fn update_with(&self, other: MapValue) -> MapValue {
				CombinedMapValue::new($s, other).into()
			}
	};
}
//#endregion ----------------- MACROS -----------------

//#region ----------------- TRAITS -----------------
pub trait MapValueExt: VirtualValueExt {
	fn keys(&self) -> HashSet<&str>;
	fn keys_list(&self) -> ListValue {
		ListValue::from_iter(self.keys())
	}
	fn for_each<F>(&self, f: F) -> Result<()>
	where
		F: FnMut(&str, &AnyValue) -> Result<()>;
	fn contains_key(&self, key: &str) -> bool;
	fn get(&self, key: &str) -> &AnyValue;
	fn size(&self) -> usize;
	fn is_empty(&self) -> bool;
	fn filter<F>(&self, f: F) -> MapValue
	where
		F: Fn(&str, &AnyValue) -> bool;
	fn update(&self, key: &str, value: &AnyValue) -> MapValue;
	fn update_with(&self, other: MapValue) -> MapValue;
}
//#endregion ----------------- TRAITS -----------------

//#region ----------------- MAP VALUE -----------------
// TODO: impl IntoIter
#[derive(PartialEq, IntoVariants, Display, Measurable)]
pub enum MapValue {
	Empty,
	Wrapping(MapWrappingMapValue),
	Filtering(FilteringMapValue),
	Mapped(MappedMapValue),
	Updated(UpdatedMapValue),
	Combined(CombinedMapValue),
}

impl Hashable for MapValue {
	fn hash(&self) -> HashValue {
		match self {
			MapValue::Empty => 0,
			MapValue::Wrapping(w) => w.hash(),
			MapValue::Filtering(f) => f.hash(),
			MapValue::Mapped(m) => m.hash(),
			MapValue::Updated(u) => u.hash(),
			MapValue::Combined(c) => c.hash(),
		}
	}
}

impl VirtualValueExt for MapValue {}

impl MapValueExt for MapValue {
	add_default_impls! {self}

	fn keys(&self) -> HashSet<&str> {
		match self {
			MapValue::Empty => HashSet::new(),
			MapValue::Wrapping(w) => w.keys(),
			MapValue::Filtering(f) => f.keys(),
			MapValue::Mapped(m) => m.keys(),
			MapValue::Updated(u) => u.keys(),
			MapValue::Combined(c) => c.keys(),
		}
	}

	fn for_each<F>(&self, f: F) -> Result<()>
	where
		F: FnMut(&str, &AnyValue) -> Result<()>,
	{
		match self {
			MapValue::Empty => Ok(()),
			MapValue::Wrapping(w) => w.for_each(f),
			MapValue::Filtering(fil) => fil.for_each(f),
			MapValue::Mapped(m) => m.for_each(f),
			MapValue::Updated(u) => u.for_each(f),
			MapValue::Combined(c) => c.for_each(f),
		}
	}

	fn contains_key(&self, key: &str) -> bool {
		match self {
			MapValue::Empty => false,
			MapValue::Wrapping(w) => w.contains_key(key),
			MapValue::Filtering(f) => f.contains_key(key),
			MapValue::Mapped(m) => m.contains_key(key),
			MapValue::Updated(u) => u.contains_key(key),
			MapValue::Combined(c) => c.contains_key(key),
		}
	}

	fn get(&self, key: &str) -> &AnyValue {
		match self {
			MapValue::Empty => &AnyValue::NoValue,
			MapValue::Wrapping(w) => w.get(key),
			MapValue::Filtering(f) => f.get(key),
			MapValue::Mapped(m) => m.get(key),
			MapValue::Updated(u) => u.get(key),
			MapValue::Combined(c) => c.get(key),
		}
	}

	fn size(&self) -> usize {
		match self {
			MapValue::Empty => 0,
			MapValue::Wrapping(w) => w.size(),
			MapValue::Filtering(f) => f.size(),
			MapValue::Mapped(m) => m.size(),
			MapValue::Updated(u) => u.size(),
			MapValue::Combined(c) => c.size(),
		}
	}

	fn is_empty(&self) -> bool {
		match self {
			MapValue::Empty => true,
			MapValue::Wrapping(w) => w.is_empty(),
			MapValue::Filtering(f) => f.is_empty(),
			MapValue::Mapped(m) => m.is_empty(),
			MapValue::Updated(u) => u.is_empty(),
			MapValue::Combined(c) => c.is_empty(),
		}
	}
}

#[derive(PartialEq, Measurable)]
struct MapWrappingMapValue {
	map: HashMap<String, AnyValue>,
	#[m(add)]
	wrapper_size: usize,
	hash: MemoizedHash,
}

impl MapWrappingMapValue {
	pub fn new(map: HashMap<String, AnyValue>, payload_size: usize) -> Self {
		Self {
			wrapper_size: size_of_hash_map(&map) + payload_size,
			map,
			hash: MemoizedHash::new(),
		}
	}

	pub fn with_size(map: HashMap<String, AnyValue>, map_size: usize, payload_size: usize) -> Self {
		Self {
			map,
			wrapper_size: map_size + payload_size,
			hash: MemoizedHash::new(),
		}
	}
}

impl VirtualValueExt for MapWrappingMapValue {}

impl MapValueExt for MapWrappingMapValue {
	fn keys(&self) -> HashSet<&str> {
		self.map.keys().map(|k| k.as_str()).collect()
	}

	fn for_each<F>(&self, mut f: F) -> Result<()>
	where
		F: FnMut(&str, &AnyValue) -> Result<()>,
	{
		for (k, v) in self.map.iter() {
			f(k, v)?;
		}
		Ok(())
	}

	fn contains_key(&self, key: &str) -> bool {
		self.map.contains_key(key)
	}

	fn get(&self, key: &str) -> &AnyValue {
		self.map.get(key).unwrap_or(&AnyValue::NoValue)
	}

	fn size(&self) -> usize {
		self.map.len()
	}

	fn is_empty(&self) -> bool {
		self.map.is_empty()
	}
}

#[derive(PartialEq)]
struct FilteringMapValue {
	map: Box<MapValue>,
	filter_fn: Box<dyn Fn(&str, &AnyValue) -> bool>,
	size: usize,
	hash: MemoizedHash,
}

impl FilteringMapValue {
	pub fn new<F>(map: MapValue, filter: F) -> Self
	where
		F: Fn(&str, &AnyValue) -> bool + 'static,
	{
		let mut map = Self {
			map: Box::new(map),
			filter_fn: Box::new(filter),
			size: 0,
			hash: MemoizedHash::new(),
		};

		let mut size = 0;
		map.for_each(|k, v| {
			if (map.filter_fn)(k, v) {
				size += 1;
			}
			Ok(())
		})
		.unwrap();
		map.size = size;

		map
	}
}

impl VirtualValueExt for FilteringMapValue {}

impl MapValueExt for FilteringMapValue {
	fn keys(&self) -> HashSet<&str> {
		todo!()
	}

	fn for_each<F>(&self, mut f: F) -> Result<()>
	where
		F: FnMut(&str, &AnyValue) -> Result<()>,
	{
		self.map.for_each(|k, v| {
			if (self.filter_fn)(k, v) {
				f(k, v)?;
			}
			Ok(())
		})
	}

	fn contains_key(&self, key: &str) -> bool {
		let v = self.map.get(key);
		match v {
			AnyValue::NoValue => false,
			_ => (self.filter_fn)(key, v),
		}
	}

	fn get(&self, key: &str) -> &AnyValue {
		let v = self.map.get(key);
		match v {
			AnyValue::NoValue => &AnyValue::NoValue,
			_ if (self.filter_fn)(key, v) => v,
			_ => &AnyValue::NoValue,
		}
	}

	fn size(&self) -> usize {
		self.size
	}

	fn is_empty(&self) -> bool {
		self.size == 0
	}
}

#[derive(PartialEq)]
struct MappedMapValue {
	map: Box<MapValue>,
	map_fn: Box<dyn for<'a> Fn(&str, &'a AnyValue) -> &'a AnyValue>,
	hash: MemoizedHash,
}

impl MappedMapValue {
	pub fn new<F>(map: MapValue, map_fn: F) -> Self
	where
		F: 'static + for<'a> Fn(&str, &'a AnyValue) -> &'a AnyValue,
	{
		Self {
			map: Box::new(map),
			map_fn: Box::new(map_fn),
			hash: MemoizedHash::new(),
		}
	}
}

impl VirtualValueExt for MappedMapValue {}

impl MapValueExt for MappedMapValue {
	fn keys(&self) -> HashSet<&str> {
		self.map.keys()
	}

	fn for_each<F>(&self, mut f: F) -> Result<()>
	where
		F: FnMut(&str, &AnyValue) -> Result<()>,
	{
		self.map.for_each(|k, v| f(k, (self.map_fn)(k, v)))
	}

	fn contains_key(&self, key: &str) -> bool {
		self.map.contains_key(key)
	}

	fn get(&self, key: &str) -> &AnyValue {
		(self.map_fn)(key, self.map.get(key))
	}

	fn size(&self) -> usize {
		self.map.size()
	}

	fn is_empty(&self) -> bool {
		self.map.is_empty()
	}
}

#[derive(PartialEq)]
struct UpdatedMapValue {
	map: Box<MapValue>,
	updated_key: String,
	updated_value: Box<AnyValue>,
	hash: MemoizedHash,
}

impl UpdatedMapValue {
	pub fn new(map: MapValue, key: &str, value: &AnyValue) -> Self {
		assert!(map.contains_key(key));
		Self {
			map: Box::new(map),
			updated_key: key.to_string(),
			updated_value: Box::new(value.clone()),
			hash: MemoizedHash::new(),
		}
	}
}

impl Measurable for UpdatedMapValue {
	fn estimated_heap_usage(&self) -> usize {
		shallow_size_of::<Self>()
			+ self.map.estimated_heap_usage()
			+ size_of_string(&self.updated_key)
			+ self.updated_value.estimated_heap_usage()
	}

	fn estimated_heap_usage_cache(&self, estimator_cache: &impl HeapEstimatorCache) -> usize
	where
		Self: Sized,
	{
		shallow_size_of::<Self>()
			+ self.map.estimated_heap_usage_cache(estimator_cache)
			+ size_of_string(&self.updated_key)
			+ self.updated_value.estimated_heap_usage_cache(estimator_cache)
	}
}

impl VirtualValueExt for UpdatedMapValue {}

impl MapValueExt for UpdatedMapValue {
	fn keys(&self) -> HashSet<&str> {
		let mut keys = self.map.keys();
		keys.insert(self.updated_key.as_str());
		keys
	}

	fn for_each<F>(&self, mut f: F) -> Result<()>
	where
		F: FnMut(&str, &AnyValue) -> Result<()>,
	{
		self.map.for_each(&mut f)?;
		f(self.updated_key.as_str(), &self.updated_value)
	}

	fn contains_key(&self, key: &str) -> bool {
		self.map.contains_key(key) || key == self.updated_key
	}

	fn get(&self, key: &str) -> &AnyValue {
		if key == self.updated_key {
			&self.updated_value
		} else {
			self.map.get(key)
		}
	}

	fn size(&self) -> usize {
		self.map.size() + 1
	}

	fn is_empty(&self) -> bool {
		false
	}
}

#[derive(PartialEq)]
struct CombinedMapValue {
	map1: Box<MapValue>,
	map2: Box<MapValue>,
	hash: MemoizedHash,
}

impl CombinedMapValue {
	pub fn new(map1: MapValue, map2: MapValue) -> Self {
		Self {
			map1: Box::new(map1),
			map2: Box::new(map2),
			hash: MemoizedHash::new(),
		}
	}
}

impl Measurable for CombinedMapValue {
	fn estimated_heap_usage(&self) -> usize {
		shallow_size_of::<Self>()
			+ self.map1.estimated_heap_usage()
			+ self.map2.estimated_heap_usage()
	}

	fn estimated_heap_usage_cache(&self, estimator_cache: &impl HeapEstimatorCache) -> usize
	where
		Self: Sized,
	{
		shallow_size_of::<Self>()
			+ self.map1.estimated_heap_usage_cache(estimator_cache)
			+ self.map2.estimated_heap_usage_cache(estimator_cache)
	}
}

impl VirtualValueExt for CombinedMapValue {}

impl MapValueExt for CombinedMapValue {
	fn keys(&self) -> HashSet<&str> {
		let mut seen = HashSet::new();
		self.map1
			.keys()
			.into_iter()
			.chain(self.map2.keys().into_iter())
			.filter(|k| seen.insert(*k))
			.collect::<HashSet<_>>()
	}

	fn for_each<F>(&self, mut f: F) -> Result<()>
	where
		F: FnMut(&str, &AnyValue) -> Result<()>,
	{
		let mut seen = HashSet::new();
		let mut consume = |k: &str, v: &AnyValue| {
			if seen.insert(k.to_string()) {
				f(k, v)?;
			}
			Ok(())
		};
		self.map1.for_each(&mut consume)?;
		self.map2.for_each(consume)
	}

	fn contains_key(&self, key: &str) -> bool {
		self.map1.contains_key(key) || self.map2.contains_key(key)
	}

	fn get(&self, key: &str) -> &AnyValue {
		let v2 = self.map2.get(key);
		if v2 != &AnyValue::NoValue {
			v2
		} else {
			self.map2.get(key)
		}
	}

	fn size(&self) -> usize {
		let mut size = 0;
		let mut seen = HashSet::new();
		let mut consume = |k: &str, v: &AnyValue| {
			if seen.insert(k.to_string()) {
				size += 1;
			}
			Ok(())
		};
		self.map1.for_each(&mut consume).unwrap();
		self.map2.for_each(consume).unwrap();
		size
	}

	fn is_empty(&self) -> bool {
		self.map1.is_empty() && self.map2.is_empty()
	}
}
//#endregion ----------------- MAP VALUE -----------------

impl_measurable!(FilteringMapValue, MappedMapValue);
impl_simple_hashable!(
	MapWrappingMapValue,
	FilteringMapValue,
	MappedMapValue,
	UpdatedMapValue,
	CombinedMapValue
);
impl_hash!(
	MapValue,
	MapWrappingMapValue,
	FilteringMapValue,
	MappedMapValue,
	UpdatedMapValue,
	CombinedMapValue
);
impl_any_ext!(
	MapValue,
	MapWrappingMapValue,
	FilteringMapValue,
	MappedMapValue,
	UpdatedMapValue,
	CombinedMapValue
);
impl_display!(
	MapWrappingMapValue,
	FilteringMapValue,
	MappedMapValue,
	UpdatedMapValue,
	CombinedMapValue
);
