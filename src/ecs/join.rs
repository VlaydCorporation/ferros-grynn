#![allow(clippy::min_ident_chars, reason = "short names for macro params")]

use std::{iter::FusedIterator, marker::PhantomData};

use super::WorldComponents;
use crate::values::runtime::graph::AtomId;

/// Expands the define join iter declarations used by this module.
macro_rules! define_join_iter {
	($name:ident, $( $type:ident : $value:ident ),+ $(,)?) => {
		/// Borrowed component join visiting atoms present in every participating component pool.
		pub struct $name<'world, $($type),+>
		where
			$($type: 'static + Send + Sync),+
		{
			/// Component registry supplying the participating component pools.
			world: &'world WorldComponents,
			/// Smallest participating component pool used to choose candidate atom identifiers.
			driver: &'world [AtomId],
			/// Next candidate position in the selected driver pool.
			index: usize,
			/// Lifetime and type marker for the borrowed component tuple.
			marker: PhantomData<($(&'world $type,)+)>,
		}

		impl<'world, $($type),+> $name<'world, $($type),+>
		where
			$($type: 'static + Send + Sync),+
		{
			/// Constructs the borrowed view or join from its supplied source.
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

			/// Returns the selected driver pool length for this component join.
			pub const fn driver_len(&self) -> usize {
				self.driver.len()
			}
		}

		impl<'world, $($type),+> Iterator for $name<'world, $($type),+>
		where
			$($type: 'static + Send + Sync),+
		{
			type Item = (AtomId, $(&'world $type,)+);

			/// Advances the driver until the next matching component tuple is found.
			fn next(&mut self) -> Option<Self::Item> {
				while let Some(id) = self.driver.get(self.index).copied() {
					self.index += 1;
					if let ($(Some($value),)+) = ($(self.world.get::<$type>(id),)+) {
						return Some((id, $($value,)+));
					}
				}
				None
			}

			/// Returns lower and optional upper bounds on remaining join items.
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

/// Creates an ECS component join driven by the smallest participating component
/// pool.
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
/// Regression tests for join behavior.
mod tests {
	use super::JoinIter3;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::{ecs::WorldComponents, values::runtime::graph::AtomId};

	/// First distinct component type used to verify ECS joins.
	#[derive(Debug, PartialEq)]
	struct A(
		/// Payload distinguishing this component instance in the join
		/// assertions.
		u32,
	);
	/// Second distinct component type used to verify ECS joins.
	#[derive(Debug, PartialEq)]
	struct B(
		/// Payload distinguishing this component instance in the join
		/// assertions.
		u32,
	);
	/// Third distinct component type used to verify ECS joins.
	#[derive(Debug, PartialEq)]
	struct C(
		/// Payload distinguishing this component instance in the join
		/// assertions.
		u32,
	);

	/// Returns atom for this module.
	fn atom(slot: u64) -> AtomId {
		AtomId::from_raw(slot)
	}

	/// Verifies chooses actual smallest pool independent of type order.
	#[test]
	fn chooses_actual_smallest_pool_independent_of_type_order() {
		let mut world = WorldComponents::new();
		for slot in 0..10 {
			world.insert(atom(slot), A(slot as u32)).assert_ok();
		}
		for slot in [8, 2, 5] {
			world.insert(atom(slot), B(slot as u32)).assert_ok();
		}
		for slot in 0..9 {
			world.insert(atom(slot), C(slot as u32)).assert_ok();
		}

		let join = JoinIter3::<A, B, C>::new(&world);
		assert_eq!(join.driver_len(), 3);
		let ids = join.map(|(id, _, _, _)| id.slot()).collect::<Vec<_>>();
		assert_eq!(ids, [8, 2, 5]);
	}

	/// Verifies missing pool produces empty join.
	#[test]
	fn missing_pool_produces_empty_join() {
		let mut world = WorldComponents::new();
		world.insert(atom(1), A(1)).assert_ok();
		assert_eq!(JoinIter3::<A, B, C>::new(&world).count(), 0);
	}
}
