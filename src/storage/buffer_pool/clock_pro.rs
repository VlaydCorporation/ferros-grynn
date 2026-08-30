#[allow(clippy::wildcard_imports, reason = "buffer-pool helpers use their facade contracts")]
use super::*;

/// Temperature of one resident Clock-Pro frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Temperature {
	/// New or not recently re-referenced page.
	Cold,
	/// Page retained after a re-reference or ghost hit.
	Hot,
}

/// Mutually exclusive lifecycle state of one fixed cache frame.
pub(super) enum FrameState {
	/// Frame contains no page image and retains its reusable aligned buffer.
	Free {
		/// Unique buffer allocated once when the pool is created.
		page: CachedPage,
	},
	/// Frame budget is reserved while its page is loaded without the metadata
	/// lock.
	Loading {
		/// Key being loaded.
		key: PageCacheKey,
		/// Temperature assigned when the load becomes resident.
		temperature: Temperature,
		/// Completion signal retained by coalesced callers.
		signal: Arc<LoadSignal>,
	},
	/// Immutable page image owned by the cache.
	Resident {
		/// Exact cached page version.
		key: PageCacheKey,
		/// Shared page; strong references above one are RAII pins.
		page: CachedPage,
		/// Current Clock-Pro temperature.
		temperature: Temperature,
		/// Reference bit set by resident hits.
		referenced: bool,
	},
}

/// Clock-Pro metadata protected by the pool mutex.
pub(super) struct ClockPro {
	/// Fixed number of frames defining the hard memory budget.
	pub(super) frames: Vec<FrameState>,
	/// Exact page key to fixed frame index.
	pub(super) entries: RapidHashMap<PageCacheKey, usize>,
	/// FIFO order of recently evicted Cold keys.
	ghost_queue: VecDeque<PageCacheKey>,
	/// Constant-time membership for the bounded ghost history.
	pub(super) ghost_set: RapidHashSet<PageCacheKey>,
	/// Current clock-hand index.
	hand: usize,
	/// Number of resident Hot frames.
	pub(super) hot_count: usize,
	/// Adaptive upper target for Hot frames.
	hot_target: usize,
	/// Cache counters updated under the same metadata mutex.
	pub(super) stats: BufferPoolStats,
}

impl ClockPro {
	/// Allocates every fixed frame and aligned payload exactly once.
	pub(super) fn new(capacity: NonZeroUsize) -> Self {
		let capacity = capacity.get();
		let stats = BufferPoolStats {
			page_buffer_allocations: capacity,
			..BufferPoolStats::default()
		};
		Self {
			frames: (0..capacity)
				.map(|_| FrameState::Free {
					page: Arc::new(Box::new(AlignedPage::zeroed())),
				})
				.collect(),
			entries: RapidHashMap::with_capacity(capacity),
			ghost_queue: VecDeque::with_capacity(capacity),
			ghost_set: RapidHashSet::with_capacity(capacity),
			hand: 0,
			hot_count: 0,
			hot_target: capacity / 2,
			stats,
		}
	}

	/// Resolves a mapped frame to either a resident hit or a coalesced wait.
	pub(super) fn access_resident(
		&mut self,
		frame_index: usize,
	) -> FerrosGrynnResult<AccessDecision> {
		match &mut self.frames[frame_index] {
			FrameState::Loading {
				signal,
				..
			} => {
				self.stats.coalesced_misses += 1;
				Ok(AccessDecision::Wait(Arc::clone(signal)))
			}
			FrameState::Resident {
				page,
				referenced,
				..
			} => {
				*referenced = true;
				self.stats.hits += 1;
				Ok(AccessDecision::Ready(Arc::clone(page)))
			}
			FrameState::Free {
				..
			} => Err(FerrosGrynnError::storage_io_failure(
				"buffer-pool entry pointed to a free frame",
			)),
		}
	}

	/// Reserves a free or evictable frame for one new physical load.
	pub(super) fn reserve(&mut self, key: PageCacheKey) -> Option<AccessDecision> {
		let frame_index = self.find_available_frame()?;
		let temperature = if self.remove_ghost(key) {
			self.hot_target = (self.hot_target + 1).min(self.frames.len());
			Temperature::Hot
		} else {
			Temperature::Cold
		};
		let signal = Arc::new(LoadSignal::new());
		let previous = std::mem::replace(&mut self.frames[frame_index], FrameState::Loading {
			key,
			temperature,
			signal: Arc::clone(&signal),
		});
		let page = match previous {
			FrameState::Free {
				page,
			} => page,
			FrameState::Resident {
				key: evicted_key,
				page,
				temperature: evicted_temperature,
				..
			} => {
				self.entries.remove(&evicted_key);
				if evicted_temperature == Temperature::Hot {
					self.hot_count -= 1;
				}
				self.add_ghost(evicted_key);
				self.stats.evictions += 1;
				page
			}
			loading @ FrameState::Loading {
				..
			} => {
				self.frames[frame_index] = loading;
				return None;
			}
		};
		self.entries.insert(key, frame_index);
		self.stats.misses += 1;
		Some(AccessDecision::Load {
			frame_index,
			signal,
			page,
		})
	}

	/// Returns the reserved temperature only while the exact load is active.
	pub(super) fn loading_temperature(
		&self,
		frame_index: usize,
		key: PageCacheKey,
	) -> Option<Temperature> {
		match self.frames.get(frame_index) {
			Some(FrameState::Loading {
				key: loading_key,
				temperature,
				..
			}) if *loading_key == key => Some(*temperature),
			Some(_) | None => None,
		}
	}

	/// Publishes a page after [`Self::loading_temperature`] verified the same
	/// reservation while the caller controlled metadata transitions.
	pub(super) fn publish_reserved(
		&mut self,
		frame_index: usize,
		key: PageCacheKey,
		page: CachedPage,
		temperature: Temperature,
	) {
		if temperature == Temperature::Hot {
			self.hot_count += 1;
		}
		self.frames[frame_index] = FrameState::Resident {
			key,
			page,
			temperature,
			referenced: false,
		};
	}

	/// Releases a loading frame after an error.
	pub(super) fn abort(&mut self, frame_index: usize, key: PageCacheKey, page: CachedPage) {
		if matches!(self.frames.get(frame_index), Some(FrameState::Loading { key: loading, .. }) if *loading == key)
		{
			self.frames[frame_index] = FrameState::Free {
				page,
			};
			self.entries.remove(&key);
		}
	}

	/// Finds one free or unpinned resident frame using Clock-Pro transitions.
	fn find_available_frame(&mut self) -> Option<usize> {
		if self.entries.len() < self.frames.len() {
			for _ in 0..self.frames.len() {
				let frame_index = self.advance_hand();
				if matches!(self.frames[frame_index], FrameState::Free { .. }) {
					return Some(frame_index);
				}
			}
		}

		for _ in 0..CLOCK_SCAN_PASSES {
			for _ in 0..self.frames.len() {
				let frame_index = self.advance_hand();
				let mut balance_hot = false;
				match &mut self.frames[frame_index] {
					FrameState::Free {
						..
					} => return Some(frame_index),
					FrameState::Loading {
						..
					} => {}
					FrameState::Resident {
						page,
						temperature,
						referenced,
						..
					} => match *temperature {
						Temperature::Hot => {
							if *referenced {
								*referenced = false;
							} else {
								*temperature = Temperature::Cold;
								self.hot_count -= 1;
							}
						}
						Temperature::Cold => {
							if *referenced {
								*referenced = false;
								*temperature = Temperature::Hot;
								self.hot_count += 1;
								balance_hot = true;
							} else if Arc::strong_count(page) == 1 {
								return Some(frame_index);
							}
						}
					},
				}
				if balance_hot {
					self.balance_hot_target();
				}
			}
		}
		None
	}

	/// Demotes unreferenced Hot pages until the adaptive target is met.
	fn balance_hot_target(&mut self) {
		if self.hot_count <= self.hot_target {
			return;
		}
		for frame in &mut self.frames {
			if self.hot_count <= self.hot_target {
				break;
			}
			if let FrameState::Resident {
				temperature,
				referenced,
				..
			} = frame && *temperature == Temperature::Hot
				&& !*referenced
			{
				*temperature = Temperature::Cold;
				self.hot_count -= 1;
			}
		}
	}

	/// Advances the clock hand and returns its previous frame index.
	fn advance_hand(&mut self) -> usize {
		let frame_index = self.hand;
		self.hand = if self.hand + 1 == self.frames.len() {
			0
		} else {
			self.hand + 1
		};
		frame_index
	}

	/// Inserts one evicted key into bounded FIFO ghost history.
	fn add_ghost(&mut self, key: PageCacheKey) {
		if self.ghost_queue.len() == self.frames.len()
			&& let Some(oldest) = self.ghost_queue.pop_front()
		{
			self.ghost_set.remove(&oldest);
		}
		self.ghost_queue.push_back(key);
		self.ghost_set.insert(key);
	}

	/// Removes and reports one key from the ghost history.
	fn remove_ghost(&mut self, key: PageCacheKey) -> bool {
		if !self.ghost_set.remove(&key) {
			return false;
		}
		self.ghost_queue.retain(|candidate| *candidate != key);
		true
	}
}
