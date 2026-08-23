#![allow(clippy::min_ident_chars, reason = "short names for macro params")]

use std::{iter::FusedIterator, marker::PhantomData};

use super::WorldComponents;
use crate::values::runtime::graph::AtomId;

macro_rules! define_join_iter {
	($name:ident, $( $type:ident : $value:ident ),+ $(,)?) => {
		pub struct $name<'world, $($type),+>
		where
			$($type: 'static + Send + Sync),+
		{
			world: &'world WorldComponents,
			driver: &'world [AtomId],
			index: usize,
			marker: PhantomData<($(&'world $type,)+)>,
		}

		impl<'world, $($type),+> $name<'world, $($type),+>
		where
			$($type: 'static + Send + Sync),+
		{
			pub fn new(world: &'world WorldComponents) -> Self {
				let candidates = [$(world.pool::<$type>().map(|pool| pool.atoms()),)+];
				let driver = if candidates.iter().any(Option::is_none) {
					&[]
				} else {
					candidates
						.into_iter()
						.flatten()
						.min_by_key(|atoms| atoms.len())
						.unwrap_or(&[])
				};
				Self {
					world,
					driver,
					index: 0,
					marker: PhantomData,
				}
			}

			pub const fn driver_len(&self) -> usize {
				self.driver.len()
			}
		}

		impl<'world, $($type),+> Iterator for $name<'world, $($type),+>
		where
			$($type: 'static + Send + Sync),+
		{
			type Item = (AtomId, $(&'world $type,)+);

			fn next(&mut self) -> Option<Self::Item> {
				while let Some(id) = self.driver.get(self.index).copied() {
					self.index += 1;
					if let ($(Some($value),)+) = ($(self.world.get::<$type>(id),)+) {
						return Some((id, $($value,)+));
					}
				}
				None
			}

			fn size_hint(&self) -> (usize, Option<usize>) {
				(0, Some(self.driver.len().saturating_sub(self.index)))
			}
		}

		impl<$($type),+> FusedIterator for $name<'_, $($type),+>
		where
			$($type: 'static + Send + Sync),+
		{}
	};
}

define_join_iter!(JoinIter2, A: a, B: b);
define_join_iter!(JoinIter3, A: a, B: b, C: c);
define_join_iter!(JoinIter4, A: a, B: b, C: c, D: d);
define_join_iter!(JoinIter5, A: a, B: b, C: c, D: d, E: e);
define_join_iter!(JoinIter6, A: a, B: b, C: c, D: d, E: e, F: f);
define_join_iter!(JoinIter7, A: a, B: b, C: c, D: d, E: e, F: f, G: g);
define_join_iter!(JoinIter8, A: a, B: b, C: c, D: d, E: e, F: f, G: g, H: h);

#[macro_export]
macro_rules! join {
	($world:expr, $a:ty, $b:ty $(,)?) => {
		$crate::ecs::JoinIter2::<$a, $b>::new($world)
	};
	($world:expr, $a:ty, $b:ty, $c:ty $(,)?) => {
		$crate::ecs::JoinIter3::<$a, $b, $c>::new($world)
	};
	($world:expr, $a:ty, $b:ty, $c:ty, $d:ty $(,)?) => {
		$crate::ecs::JoinIter4::<$a, $b, $c, $d>::new($world)
	};
	($world:expr, $a:ty, $b:ty, $c:ty, $d:ty, $e:ty $(,)?) => {
		$crate::ecs::JoinIter5::<$a, $b, $c, $d, $e>::new($world)
	};
	($world:expr, $a:ty, $b:ty, $c:ty, $d:ty, $e:ty, $f:ty $(,)?) => {
		$crate::ecs::JoinIter6::<$a, $b, $c, $d, $e, $f>::new($world)
	};
	($world:expr, $a:ty, $b:ty, $c:ty, $d:ty, $e:ty, $f:ty, $g:ty $(,)?) => {
		$crate::ecs::JoinIter7::<$a, $b, $c, $d, $e, $f, $g>::new($world)
	};
	($world:expr, $a:ty, $b:ty, $c:ty, $d:ty, $e:ty, $f:ty, $g:ty, $h:ty $(,)?) => {
		$crate::ecs::JoinIter8::<$a, $b, $c, $d, $e, $f, $g, $h>::new($world)
	};
}

#[cfg(test)]
mod tests {
	use super::JoinIter3;
	use crate::{ecs::WorldComponents, values::runtime::graph::AtomId};

	#[derive(Debug, PartialEq)]
	struct A(u32);
	#[derive(Debug, PartialEq)]
	struct B(u32);
	#[derive(Debug, PartialEq)]
	struct C(u32);

	fn atom(slot: u64) -> AtomId {
		AtomId::from_raw(slot)
	}

	#[test]
	fn chooses_actual_smallest_pool_independent_of_type_order() {
		let mut world = WorldComponents::new();
		for slot in 0..10 {
			world.insert(atom(slot), A(slot as u32)).unwrap();
		}
		for slot in [8, 2, 5] {
			world.insert(atom(slot), B(slot as u32)).unwrap();
		}
		for slot in 0..9 {
			world.insert(atom(slot), C(slot as u32)).unwrap();
		}

		let join = JoinIter3::<A, B, C>::new(&world);
		assert_eq!(join.driver_len(), 3);
		let ids = join.map(|(id, _, _, _)| id.slot()).collect::<Vec<_>>();
		assert_eq!(ids, [8, 2, 5]);
	}

	#[test]
	fn missing_pool_produces_empty_join() {
		let mut world = WorldComponents::new();
		world.insert(atom(1), A(1)).unwrap();
		assert_eq!(JoinIter3::<A, B, C>::new(&world).count(), 0);
	}
}
