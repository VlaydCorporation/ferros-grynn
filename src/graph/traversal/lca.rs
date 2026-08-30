use rapidhash::{RapidHashMap, RapidHashSet};

use super::{algorithm_failure, connectivity::collect_node_ids};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomKind,
		Forest,
		GraphRead,
		RootedTree,
		Tree,
		general::{atom_does_not_exist, atom_kind_mismatch},
	},
	values::Uuid,
};

#[derive(Debug, Clone)]
pub struct BinaryLiftingIndex {
	identity: Uuid,
	revision: u64,
	nodes: Box<[AtomId]>,
	positions: RapidHashMap<AtomId, usize>,
	depths: Box<[usize]>,
	roots: Box<[usize]>,
	ancestors: Box<[Box<[Option<usize>]>]>,
}

impl BinaryLiftingIndex {
	pub fn from_forest<State>(forest: &Forest<State>) -> FerrosGrynnResult<Self> {
		Self::build(forest, |id| forest.parent(id))
	}

	pub fn from_tree<State>(tree: &Tree<State>) -> FerrosGrynnResult<Self> {
		Self::build(tree, |id| tree.parent(id))
	}

	pub fn from_rooted_tree<State>(tree: &RootedTree<State>) -> FerrosGrynnResult<Self> {
		Self::build(tree, |id| tree.parent(id))
	}

	pub const fn identity(&self) -> Uuid {
		self.identity
	}

	pub const fn revision(&self) -> u64 {
		self.revision
	}

	pub fn validate<R: GraphRead>(&self, graph: &R) -> FerrosGrynnResult<()> {
		if graph.identity() != self.identity {
			return Err(FerrosGrynnError::graph_identity_mismatch(
				self.identity.to_string(),
				graph.identity().to_string(),
			));
		}
		if graph.revision() != self.revision {
			return Err(FerrosGrynnError::graph_workspace_stale(
				self.identity.to_string(),
				self.revision,
				graph.identity().to_string(),
				graph.revision(),
			));
		}
		Ok(())
	}

	pub fn depth(&self, atom: AtomId) -> FerrosGrynnResult<usize> {
		let index = self.position(atom)?;
		Ok(self.depths[index])
	}

	pub fn lowest_common_ancestor(
		&self,
		left: AtomId,
		right: AtomId,
	) -> FerrosGrynnResult<Option<AtomId>> {
		let mut left = self.position(left)?;
		let mut right = self.position(right)?;
		if self.roots[left] != self.roots[right] {
			return Ok(None);
		}
		if self.depths[left] < self.depths[right] {
			std::mem::swap(&mut left, &mut right);
		}
		let difference = self.depths[left] - self.depths[right];
		for level in 0..self.ancestors.len() {
			if difference & (1_usize << level) != 0 {
				let Some(ancestor) = self.ancestors[level][left] else {
					return Ok(None);
				};
				left = ancestor;
			}
		}
		if left == right {
			return Ok(Some(self.nodes[left]));
		}
		for level in (0..self.ancestors.len()).rev() {
			let left_ancestor = self.ancestors[level][left];
			let right_ancestor = self.ancestors[level][right];
			if left_ancestor != right_ancestor {
				let (Some(next_left), Some(next_right)) = (left_ancestor, right_ancestor) else {
					continue;
				};
				left = next_left;
				right = next_right;
			}
		}
		Ok(self.ancestors[0][left].map(|index| self.nodes[index]))
	}

	fn build<R: GraphRead>(
		graph: &R,
		mut parent_of: impl FnMut(AtomId) -> Option<AtomId>,
	) -> FerrosGrynnResult<Self> {
		let nodes = collect_node_ids(graph)?;
		let positions = nodes
			.iter()
			.enumerate()
			.map(|(index, id)| (*id, index))
			.collect::<RapidHashMap<_, _>>();
		let mut parents = Vec::with_capacity(nodes.len());
		for node in &nodes {
			let parent = parent_of(*node)
				.map(|parent| {
					positions.get(&parent).copied().ok_or_else(|| {
						algorithm_failure(
							"binary lifting",
							format!("parent {parent} is absent from the graph"),
						)
					})
				})
				.transpose()?;
			parents.push(parent);
		}

		let mut depths = vec![None; nodes.len()];
		let mut roots = vec![None; nodes.len()];
		for start in 0..nodes.len() {
			if depths[start].is_some() {
				continue;
			}
			let mut chain = Vec::new();
			let mut seen = RapidHashSet::default();
			let mut current = start;
			while depths[current].is_none() {
				if !seen.insert(current) {
					return Err(algorithm_failure(
						"binary lifting",
						"the parent relation contains a cycle",
					));
				}
				chain.push(current);
				let Some(parent) = parents[current] else {
					depths[current] = Some(0);
					roots[current] = Some(current);
					break;
				};
				current = parent;
			}
			while let Some(child) = chain.pop() {
				if depths[child].is_some() {
					continue;
				}
				let Some(parent) = parents[child] else {
					depths[child] = Some(0);
					roots[child] = Some(child);
					continue;
				};
				let Some(parent_depth) = depths[parent] else {
					return Err(algorithm_failure("binary lifting", "parent depth is unavailable"));
				};
				let Some(root) = roots[parent] else {
					return Err(algorithm_failure(
						"binary lifting",
						"component root is unavailable",
					));
				};
				depths[child] = Some(parent_depth + 1);
				roots[child] = Some(root);
			}
		}

		let mut levels = 1_usize;
		let mut width = 1_usize;
		while width < nodes.len() {
			levels += 1;
			width = width.saturating_mul(2);
		}
		let mut ancestors = Vec::with_capacity(levels);
		ancestors.push(parents.into_boxed_slice());
		for level in 1..levels {
			let previous = &ancestors[level - 1];
			let next = previous
				.iter()
				.map(|ancestor| ancestor.and_then(|index| previous[index]))
				.collect::<Vec<_>>()
				.into_boxed_slice();
			ancestors.push(next);
		}
		Ok(Self {
			identity: graph.identity(),
			revision: graph.revision(),
			nodes: nodes.into_boxed_slice(),
			positions,
			depths: depths
				.into_iter()
				.collect::<Option<Vec<_>>>()
				.ok_or_else(|| algorithm_failure("binary lifting", "node depth is unavailable"))?
				.into_boxed_slice(),
			roots: roots
				.into_iter()
				.collect::<Option<Vec<_>>>()
				.ok_or_else(|| {
					algorithm_failure("binary lifting", "component root is unavailable")
				})?
				.into_boxed_slice(),
			ancestors: ancestors.into_boxed_slice(),
		})
	}

	pub(super) fn position(&self, atom: AtomId) -> FerrosGrynnResult<usize> {
		if atom.kind() != Some(AtomKind::Node) {
			return Err(atom_kind_mismatch(atom, AtomKind::Node));
		}
		self.positions.get(&atom).copied().ok_or_else(|| atom_does_not_exist(atom))
	}
}
