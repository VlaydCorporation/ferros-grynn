# Graph Report - ferros-grynn  (2026-08-12)

## Corpus Check
- 149 files · ~203,127 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 4851 nodes · 12453 edges · 270 communities (152 shown, 118 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 100 edges (avg confidence: 0.81)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `892cf463`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- Parser
- SnapshotId
- DagStorage
- Hypergraph<State>
- MetaGraph
- FerrosGrynnResult
- read.rs
- MvccTransaction
- AtomId
- GraphId
- ClockProInner
- EdgeColumns
- Graph
- Uuid
- Subgraph<Mutable>
- Path
- Result
- win_async.rs
- Number
- src/ecs.rs
- execution.rs
- Graph<State>
- Subgraph<State>
- store.rs
- status_codes.rs
- PropertyKeyId
- EdgeTopology
- AtomId
- Value
- Position
- GraphWorkspace<G>
- src/traversal.rs
- Transaction
- Result
- Text
- FerrosGrynnError
- Location
- Result
- SparseSet<T>
- Duration
- SIV<T>
- PropertyColumn<T>
- formatting.rs
- PosixDevice
- Closure
- DirectFile
- benches/property_map.rs
- LiveUpdate
- async_io.rs
- arrow.rs
- StatusObject
- Array
- Vector
- .new
- Range
- StatusObject
- Lsn
- graph/traversal.rs
- .new
- heap_estimator.rs
- GraphKind
- TextSpan
- Regex
- Set
- AsyncIo
- AdjacencyMatrix
- SlotAllocator
- Object
- get_metadata_inner
- StorageBackend
- fg-meta/src/lib.rs
- PropertyBatch
- Dag<State>
- HashValue
- WorldComponents
- .new
- Self
- UringState
- StatusWireV1
- FerrosGrynnResult
- PropertyDictionary
- AtomId
- Value
- EdgeEndpoint
- AccumList
- DiagnosticRecord
- Bytes
- bench_graph.rs
- os.rs
- .register_datafusion_table
- DiagnosticValue
- Graphify
- PropertyMap
- wy_hash.rs
- algorithm_failure
- MaterializationOrigin
- Item
- HashFunction
- Option
- Option
- graph_core.rs
- build_from_map
- memohash.rs
- Ferros-Grynn Repository Guidance
- Physical Query Execution
- Values Module Guidance
- GrynQL Language Specification
- atoms
- helpers.rs
- join.rs
- .run
- datastore.rs
- Specification Design Authority
- Persistent MVCC v2
- Unified Atom Incidence
- RecordId
- hashing/mod.rs
- XorShift32HashFunction
- QueueEntry
- laws.rs
- ferros-grynn-macros/src/lib.rs
- Hybrid Query Model
- struct_size.rs
- Canonical Storage Value Codec
- P3 Query Execution
- .next
- &'a Bytes
- .fmt
- Direct I/O and Clock-Pro
- Redacted External Error Sources
- Sized
- TransactionGraphView
- MaterializedGraph
- ir.rs
- TxId
- .clone
- Type
- .mutate_local
- ForestStorage
- Result
- TableSchema
- live.rs
- DirtyTarget
- PropertyIter
- .identity
- Subgraph
- Iter<'_, T>
- GraphStorageMode
- QueryResult
- AtomIds
- Option
- Self
- Subgraph<Frozen>
- Handle<'_, T>
- AnyPool
- PropertyMap
- AtomId
- Criterion
- Identifier
- FnMut
- Index
- IndexMut
- Output
- AtomicU64
- AtomId
- Drop
- Into
- IntoIterator
- MaterializedGraph
- Mutex
- RwLock
- IntoIter
- D
- Err
- FromStr
- Into
- S
- SystemTime
- Index
- AtomId
- AtomId
- AtomId
- AtomKind
- RapidHashMap
- SmallVec
- AtomId
- AtomId
- AtomId
- AtomId
- AtomKind
- Sized
- AtomId
- AtomKind
- IndexMut
- Output
- SparseSet
- TypeId
- SparseSet
- FnMut
- Metagraph<State>
- Value
- Into
- E
- Into
- StatusObject
- String
- F
- FnOnce
- Into
- StatusObject
- FerrosGrynnError
- Result
- Field
- PropertyMap
- Value
- Into
- PropertyMap
- Into
- Value
- Into
- Fn
- Send
- Sync
- Value
- Into
- Fn
- FnOnce
- IntoIterator
- Item
- Mutex
- RwLock
- Send
- Sync
- VecDeque
- AtomRef
- Into
- PropertyMap
- Into
- SmallVec
- Vec
- EdgeTopology
- FnOnce
- AtomRef
- MaterializedGraph
- RecordId
- Into
- K
- V
- Value
- Into
- Entry
- FusedIterator
- Into
- SliceIter
- Value
- Into
- Value
- Into
- Value
- Into
- Into
- Value
- Value
- WorldComponents

## God Nodes (most connected - your core abstractions)
1. `AtomId` - 203 edges
2. `MetaGraph` - 153 edges
3. `Value` - 143 edges
4. `Graph` - 63 edges
5. `Transaction` - 56 edges
6. `PropertyKeyId` - 51 edges
7. `TxId` - 50 edges
8. `Lsn` - 48 edges
9. `MvccTransaction` - 44 edges
10. `MaterializedGraph` - 42 edges

## Surprising Connections (you probably didn't know these)
- `Public Graph Surface` --semantically_similar_to--> `Graph Storage Families`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/graphs.md
- `Claude Ferros-Grynn Guidance` --semantically_similar_to--> `Prototype-to-Production Migration Architecture`  [INFERRED] [semantically similar]
  CLAUDE.md → AGENTS.md
- `Value Runtime Invariants` --semantically_similar_to--> `Distinct None and Null Semantics`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/values_plan.md
- `Arrow Validity Plus Presence Bitmap` --semantically_similar_to--> `PropertyBatch Presence Model`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/values_plan.md
- `MvccStore` --references--> `C`  [EXTRACTED]
  crates/fg-meta/src/mvcc.rs → src/ecs/join.rs

## Import Cycles
- 1-file cycle: `src/values/runtime/number.rs -> src/values/runtime/number.rs`
- 1-file cycle: `src/values/runtime/bytes.rs -> src/values/runtime/bytes.rs`
- 1-file cycle: `src/values/runtime/graph/general.rs -> src/values/runtime/graph/general.rs`
- 1-file cycle: `src/values/runtime/property_dictionary.rs -> src/values/runtime/property_dictionary.rs`
- 1-file cycle: `src/values/runtime/range.rs -> src/values/runtime/range.rs`
- 1-file cycle: `src/values/runtime/regex.rs -> src/values/runtime/regex.rs`
- 1-file cycle: `src/values/runtime/set.rs -> src/values/runtime/set.rs`
- 1-file cycle: `src/values/runtime/vector.rs -> src/values/runtime/vector.rs`
- 1-file cycle: `src/values/runtime/text.rs -> src/values/runtime/text.rs`
- 1-file cycle: `src/values/runtime/graph/materialized.rs -> src/values/runtime/graph/materialized.rs`
- 1-file cycle: `src/common/memory/measurable.rs -> src/common/memory/measurable.rs`
- 2-file cycle: `src/values/runtime/array.rs -> src/values/runtime/mod.rs -> src/values/runtime/array.rs`
- 2-file cycle: `src/values/runtime/mod.rs -> src/values/runtime/range.rs -> src/values/runtime/mod.rs`
- 2-file cycle: `src/values/runtime/mod.rs -> src/values/runtime/set.rs -> src/values/runtime/mod.rs`
- 2-file cycle: `src/ecs/join.rs -> src/ecs/mod.rs -> src/ecs/join.rs`

## Hyperedges (group relationships)
- **GrynQL Contract Stack** — docs_gql_spec_grynql_language, docs_gql_grammar_grynql_grammar, docs_gql_tech_spec_query_engine [INFERRED 0.95]
- **Storage Persistence Contract** — docs_disk_storage_spec_frame_wal, docs_disk_storage_spec_checkpoint_recovery, docs_gql_tech_spec_page_manager, crates_fg_meta_implementation_notes_persistent_mvcc [INFERRED 0.85]
- **Values Runtime Storage Separation** — docs_values_plan_runtime_storage_boundary, docs_values_plan_storage_value, src_values_agents_runtime_invariants, src_values_agents_presence_bitmap [INFERRED 0.95]

## Communities (270 total, 118 thin omitted)

### Community 0 - "Parser"
Cohesion: 0.08
Nodes (49): Config, Default, Self, cover(), Expr, ExprKind, Identifier, LetStatement (+41 more)

### Community 1 - "SnapshotId"
Cohesion: 0.16
Nodes (7): DiskGraphView, DiskGraphView<'tx, R>, GraphReadView, GraphReadView<'tx, R>, R, Self, SnapshotId

### Community 2 - "DagStorage"
Cohesion: 0.08
Nodes (31): build_oriented_tree_storage(), build_storage(), constraint(), Dag<Frozen>, Dag<Mutable>, DagStorage, frozen_layout_round_trips_to_mutable_adjacency(), node_removal_marks_incoming_and_outgoing_edges_dirty() (+23 more)

### Community 3 - "Hypergraph<State>"
Cohesion: 0.05
Nodes (45): build_storage(), constraint(), graph_conversion_rejects_edge_endpoints(), Hypergraph<Frozen>, Hypergraph<Mutable>, Hypergraph<State>, HyperStorage, permits_a_vertex_in_both_directed_incidence_roles() (+37 more)

### Community 4 - "MetaGraph"
Cohesion: 0.08
Nodes (16): MetaGraph, AtomId, AtomKind, Box, Default, EdgeTopology, Option, PropertyMap (+8 more)

### Community 5 - "FerrosGrynnResult"
Cohesion: 0.25
Nodes (13): MutexGuard, ReplayFn, apply_or_pause(), invalid_state(), LiveEvent, LiveGraph<Mutable>, resume_queue(), FerrosGrynnError (+5 more)

### Community 6 - "read.rs"
Cohesion: 0.12
Nodes (21): assert_snapshot_released(), cascade_removal_marks_removed_incident_edges_dirty(), disk_view_and_hybrid_materialization_have_the_same_read_result(), FakeTransaction, GraphRefReader, GraphWriteBack, mutable_hybrid_flush_uses_origin_mapping_and_dirty_set(), query_to_memory() (+13 more)

### Community 7 - "MvccTransaction"
Cohesion: 0.06
Nodes (52): AtomMeta, basic_generic_rw(), BytesCodec, closure_codec_with_capture(), ClosureCodec, ClosureCodec<T>, Codec, exact_phantom_label() (+44 more)

### Community 8 - "AtomId"
Cohesion: 0.07
Nodes (32): bfs_spanning_tree(), BinaryLiftingTable, Condensation, DagSubGraph, extract_dag(), extract_forest(), extract_hyper(), extract_tree() (+24 more)

### Community 9 - "GraphId"
Cohesion: 0.05
Nodes (27): MetaGraphError, AtomId, AtomKind, Error, AtomId, AtomKind, EdgeKindMarker, GraphId (+19 more)

### Community 10 - "ClockProInner"
Cohesion: 0.06
Nodes (32): backend(), BufferPool, clock_pro_scan_resistance(), ClockProInner, dirty_flushed_on_eviction(), eviction_produces_ghost(), Frame, FrameStatus (+24 more)

### Community 11 - "EdgeColumns"
Cohesion: 0.09
Nodes (18): CowVec, EdgeColumns, empty_topology(), ensure_unique_record(), generated_record_id(), GraphMetadata, NodeColumns, BTreeSet (+10 more)

### Community 12 - "Graph"
Cohesion: 0.10
Nodes (34): INLINE_INCIDENCE, RapidHashMap, SmallVec, copy_from_read(), freeze_thaw_preserves_identity_ids_and_topology(), GeneralStorage, Graph, Graph<Frozen> (+26 more)

### Community 13 - "Uuid"
Cohesion: 0.07
Nodes (35): JsonValue, InnerUlid, InnerUuid, Display, Err, Formatter, From, FromStr (+27 more)

### Community 14 - "Subgraph<Mutable>"
Cohesion: 0.24
Nodes (7): GraphMut, AtomId, Default, FerrosGrynnResult, GWB, R, Subgraph<Mutable>

### Community 15 - "Path"
Cohesion: 0.06
Nodes (39): EndpointRole, Path, PathDirection, PathStep, PathTraceArena, PathTraceId, PathTraceNode, resolve_atom() (+31 more)

### Community 16 - "Result"
Cohesion: 0.18
Nodes (14): PageId, atom_id_from(), BytesPersistentStore, checkpoint_then_recover(), CheckpointHeader, crc32(), PersistentMvccStore<T, C>, AtomId (+6 more)

### Community 17 - "win_async.rs"
Cohesion: 0.05
Nodes (47): AtomicI64, cpu_affinity_cpu0(), goto_shutdown(), ioring_handle_ref(), ioring_raw_buffer_ref(), ioring_registered_buffer_ref(), new_fails_gracefully_if_no_ioring(), PendOp (+39 more)

### Community 18 - "Number"
Cohesion: 0.06
Nodes (43): BigInt, batch_random_access(), bench_collect(), bench_iter_sequential_borrowed(), bench_iter_sequential_owned(), bench_iter_sequential_with_step(), bench_push_preallocated(), bench_push_raw() (+35 more)

### Community 19 - "src/ecs.rs"
Cohesion: 0.07
Nodes (34): AnyPool, ColorTag, FeatureVector, node_id(), NodeWeight, PageRank, Position2D, Position3D (+26 more)

### Community 20 - "execution.rs"
Cohesion: 0.06
Nodes (80): CheckedAdd, CheckedMul, CheckedSub, Decimal, IntoIter, LogicalSelect, NodeId, PrimInt (+72 more)

### Community 21 - "Graph<State>"
Cohesion: 0.09
Nodes (16): Graph<State>, Debug, EdgeIds, Eq, Formatter, H, Hash, Item (+8 more)

### Community 22 - "Subgraph<State>"
Cohesion: 0.11
Nodes (13): Debug, EdgeIds, Eq, GraphKind, GraphRead, GraphStorageMode, Hash, NodeIds (+5 more)

### Community 23 - "store.rs"
Cohesion: 0.10
Nodes (31): auto_checkpoint_triggers(), checkpoint_clears_wal(), compress_payload(), crc32(), crc32_update(), create_and_reopen(), decompress_payload(), delete_graph() (+23 more)

### Community 24 - "status_codes.rs"
Cohesion: 0.11
Nodes (24): define_status_codes(), last_path_ident(), option_lit(), Ident, Option, Parse, Path, String (+16 more)

### Community 25 - "PropertyKeyId"
Cohesion: 0.06
Nodes (32): Entry, &'a PropertyMap, clone_is_independent(), empty_map_is_pointer_sized_and_does_not_reserve_storage(), entry_ref(), equality_order_and_hash_ignore_insertion_order(), from_iterator_uses_last_duplicate_value(), keeps_entries_sorted_and_replaces_existing_values() (+24 more)

### Community 26 - "EdgeTopology"
Cohesion: 0.09
Nodes (20): INLINE_ENDPOINTS, distinguishes_binary_hyper_and_cross_level_topology(), EdgeDirection, EdgeTopology, QualifiedAtomId, AtomId, Box, Display (+12 more)

### Community 27 - "AtomId"
Cohesion: 0.07
Nodes (21): Copied, EdgeEndpoint, QualifiedAtomId, atom_does_not_exist(), atom_kind_mismatch(), endpoint_kind(), BTreeSet, FerrosGrynnError (+13 more)

### Community 28 - "Value"
Cohesion: 0.08
Nodes (25): Closure, RecordId, atom_and_path_have_distinct_value_contracts(), display_is_deterministic_and_escapes_strings(), eq_ord_and_hash_agree_for_cross_number_values(), graph_transport_preserves_exact_type_and_typed_conversion(), reports_concrete_kind_and_static_type(), Box (+17 more)

### Community 29 - "Position"
Cohesion: 0.07
Nodes (35): GeoJsonGeometry, GeoJsonPosition, assert_status(), compare_float_slices(), GeoJsonValue, Geometry, into_position(), into_positions() (+27 more)

### Community 30 - "GraphWorkspace<G>"
Cohesion: 0.08
Nodes (30): Sized, canonical_mutation_invalidates_derived_and_removes_dead_components(), component_mutation_does_not_change_graph_revision(), G, GraphWorkspace, GraphWorkspace<G>, IntoGraphWorkspace, map_graph_moves_user_components_but_discards_derived_pools() (+22 more)

### Community 31 - "src/traversal.rs"
Cohesion: 0.10
Nodes (43): BellmanFordResult, bellman_ford(), BellmanFordError, bfs(), bfs_with(), dfs(), dfs_full(), dijkstra() (+35 more)

### Community 32 - "Transaction"
Cohesion: 0.17
Nodes (8): Drop, ensure_in_memory_reference(), QuerySnapshot, Arc, FerrosGrynnError, FerrosGrynnResult, View, Transaction

### Community 33 - "Result"
Cohesion: 0.09
Nodes (26): bool, Closure, Graph<Frozen>, MaterializedGraph, Number, Path, RecordId, Display (+18 more)

### Community 34 - "Text"
Cohesion: 0.08
Nodes (26): AsRef, Borrow, Domain, Extend, CompactString, exposes_controlled_utf8_mutation(), round_trips_through_std_and_compact_strings(), Deref (+18 more)

### Community 35 - "FerrosGrynnError"
Cohesion: 0.08
Nodes (24): E, InvalidSeverity, ErrorState, FerrosGrynnError, NonErrorStatus, Arc, Debug, Display (+16 more)

### Community 36 - "Location"
Cohesion: 0.13
Nodes (15): ArcStr, NonZeroU32, computes_unicode_locations_without_retaining_source(), LabeledLocation, line_column_at(), LineColumn, Location, LocationError (+7 more)

### Community 37 - "Result"
Cohesion: 0.12
Nodes (26): Decoder, Decoder<R>, Encoder, Encoder<W>, from_bytes(), from_file(), roundtrip(), roundtrip_atom_kind_preserved() (+18 more)

### Community 38 - "SparseSet<T>"
Cohesion: 0.11
Nodes (19): atom(), invalid_atom(), rejects_null_atom(), AtomId, Box, Default, ExactSizeIterator, FerrosGrynnError (+11 more)

### Community 39 - "Duration"
Cohesion: 0.09
Nodes (20): DateTime, Duration, Default, Display, Eq, Err, Formatter, From (+12 more)

### Community 40 - "SIV<T>"
Cohesion: 0.05
Nodes (28): FnMut, Id, Index, IndexMut, Output, clone_copies_only_the_changed_chunk(), CowVec<T, CHUNK_LEN>, Iter (+20 more)

### Community 41 - "PropertyColumn<T>"
Cohesion: 0.11
Nodes (18): atom(), DenseBitmap, distinguishes_none_null_and_value(), PropertyColumn<T>, PropertyState, removing_null_reports_null_instead_of_none(), replacement_removal_and_generation_keep_bitmaps_aligned(), Any (+10 more)

### Community 42 - "formatting.rs"
Cohesion: 0.11
Nodes (27): F, And, Callable, Comma, DisplayValue, Formatted, Formatted<'a, F, T>, Formatted<'_, F, T> (+19 more)

### Community 43 - "PosixDevice"
Cohesion: 0.09
Nodes (18): NvmeDevice, NvmeStats, open_best(), open_best_posix(), posix_device_rw(), PosixDevice, Any, Arc (+10 more)

### Community 44 - "Closure"
Cohesion: 0.08
Nodes (24): ClosureFunction, Func, NonZeroU64, calls_function_and_keeps_process_identity(), Closure, different_closures_have_different_identity(), rejects_wrong_arity_before_invocation(), Arc (+16 more)

### Community 45 - "DirectFile"
Cohesion: 0.11
Nodes (22): AlignedBuffer, alloc_aligned(), DirectFile, free_aligned(), platform_grow(), platform_open(), platform_open_readonly(), platform_read_page() (+14 more)

### Community 46 - "benches/property_map.rs"
Cohesion: 0.12
Nodes (32): bench_build(), bench_iterate_and_clone(), bench_lookup(), boxed_enum_map(), BoxedEnumIter, BoxedEnumPropertyMap, BoxedEnumRepr, btree_map() (+24 more)

### Community 47 - "LiveUpdate"
Cohesion: 0.16
Nodes (14): ApplyUpdate, DirtyTarget, conflicting_changes(), LiveUpdate, MutableLiveState, BTreeSet, Clone, Debug (+6 more)

### Community 48 - "async_io.rs"
Cohesion: 0.10
Nodes (33): AsyncIoEngine, AsyncUringEngine, BatchEntry, bg_loop(), blocking_op(), build(), build_ring(), bump_memlock() (+25 more)

### Community 49 - "arrow.rs"
Cohesion: 0.12
Nodes (30): ArrayRef, ArrowError, DataType, Field, arrow_failure(), arrow_round_trip_preserves_none_null_and_native_scalars(), build_array(), ColumnKind (+22 more)

### Community 50 - "StatusObject"
Cohesion: 0.11
Nodes (20): IntoSO, applies_gql_precedence(), Outcome, Outcome<T>, precedence(), rejects_errors(), Display, Error (+12 more)

### Community 51 - "Array"
Cohesion: 0.08
Nodes (17): &'a Array, &'a mut Array, Array, From, FromIterator, I, IntoIter, IntoIterator (+9 more)

### Community 52 - "Vector"
Cohesion: 0.10
Nodes (20): canonical_f32(), canonical_f64(), compare_f32(), compare_f32_value(), compare_f64(), compare_f64_value(), hash_f32(), hash_f64() (+12 more)

### Community 53 - ".new"
Cohesion: 0.23
Nodes (18): Fields, IntoText, configured_transaction_limits_have_domain_statuses(), Datastore, disjoint_atom_writes_merge(), graph_id(), graph_removal_preserves_table_catalog_integrity(), predicate_validation_ignores_non_matching_changes_but_catches_phantoms() (+10 more)

### Community 54 - "Range"
Cohesion: 0.12
Nodes (24): Bound, End, borrow_bound(), Bound<T>, bound_value(), BoundExt, box_bound(), checks_inclusion() (+16 more)

### Community 55 - "StatusObject"
Cohesion: 0.05
Nodes (37): D, Deserialize, DiagnosticRecord, DiagnosticValue, Err, ExecutionPhase, FromStr, LabeledLocation (+29 more)

### Community 56 - "Lsn"
Cohesion: 0.12
Nodes (18): BufReader, decode_entry(), decode_isolation(), encode_entry(), encode_isolation(), entry_crc(), Lsn, MultiSegmentReader (+10 more)

### Community 57 - "graph/traversal.rs"
Cohesion: 0.21
Nodes (27): bellman_ford(), bellman_ford_reports_negative_cycle_as_domain_error(), bfs(), bfs_topology_and_components_are_correct(), collect_weights(), condensation_collapses_cycles_and_deduplicates_bidirectional_steps(), deduplicated_steps(), dfs() (+19 more)

### Community 58 - ".new"
Cohesion: 0.12
Nodes (14): BufWriter, PersistLayer, Arc, AtomicU64, File, Mutex, Path, PathBuf (+6 more)

### Community 59 - "heap_estimator.rs"
Cohesion: 0.08
Nodes (21): HeapEstimatorCache, NoHeapEstimatorCache, Self, Measurable, HashMap, K, V, size_of_hash_map() (+13 more)

### Community 60 - "GraphKind"
Cohesion: 0.18
Nodes (4): GraphKind, Display, Formatter, Result

### Community 61 - "TextSpan"
Cohesion: 0.13
Nodes (26): BinaryOperator, Expr, Literal, LogicalProgram, ScanTarget, SelectStatement, SelectTarget, analyze() (+18 more)

### Community 62 - "Regex"
Cohesion: 0.09
Nodes (19): compiles_and_matches_pattern(), Regex, Display, Eq, Err, Error, Formatter, FromStr (+11 more)

### Community 63 - "Set"
Cohesion: 0.10
Nodes (15): &'a Set, BTreeSet<Value>, deduplicates_and_orders_values(), BTreeSet, From, FromIterator, I, IntoIter (+7 more)

### Community 64 - "AsyncIo"
Cohesion: 0.15
Nodes (16): async_io_batch_tag_routing(), async_io_write_read_sync(), AsyncIo, AsyncIoConfig, EngineConfig, Arc, Box, Default (+8 more)

### Community 65 - "AdjacencyMatrix"
Cohesion: 0.15
Nodes (13): AdjacencyMatrix, Incidence, IncidenceMatrix, IncidenceTensor, MetaAdjacencyMatrix, MetaTriple, AtomId, FxHashMap (+5 more)

### Community 67 - "Object"
Cohesion: 0.08
Nodes (21): K, Keys, &'a mut Object, &'a Object, BTreeMap<Text, Value>, has_canonical_key_order_independent_of_insertion(), Object, BTreeMap (+13 more)

### Community 68 - "get_metadata_inner"
Cohesion: 0.10
Nodes (23): add_m, Attrs, measurable_inner(), occurrence_error(), add, DeriveInput, Error, Parse (+15 more)

### Community 69 - "StorageBackend"
Cohesion: 0.14
Nodes (16): AsyncIoRingEngine, Arc, Default, ThreadPoolEngine, extract_fd(), extract_handle(), extract_path(), RawFdBackend (+8 more)

### Community 70 - "fg-meta/src/lib.rs"
Cohesion: 0.10
Nodes (15): EdgeIncidence, EdgeInfo, EdgeRole, EdgeTopology, AtomId, Box, Option, PropertyMap (+7 more)

### Community 71 - "PropertyBatch"
Cohesion: 0.17
Nodes (14): BooleanBuffer, imported_arrow_batch_requires_property_metadata(), invalid_batch(), PropertyBatch, BTreeMap, FerrosGrynnError, FerrosGrynnResult, Into (+6 more)

### Community 72 - "Dag<State>"
Cohesion: 0.10
Nodes (19): Dag, Dag<State>, Debug, Eq, Error, Formatter, H, Hash (+11 more)

### Community 73 - "HashValue"
Cohesion: 0.12
Nodes (14): AddAssign, ConstOne, Mul, MulAssign, One, HashValue, Add, Display (+6 more)

### Community 74 - "WorldComponents"
Cohesion: 0.10
Nodes (22): AnyPool, atom(), remove_atom_cleans_every_pool_and_respects_generation(), Any, AtomId, Box, FerrosGrynnResult, FnMut (+14 more)

### Community 75 - ".new"
Cohesion: 0.27
Nodes (7): IoBatch, IoRequest, Self, Vec, tag_routing(), threadpool_write_read(), Vec

### Community 76 - "Self"
Cohesion: 0.12
Nodes (16): DagView, ForestView, GraphView, Dag<State>, DagView<'graph, R>, directed_forest_view(), Forest<State>, RootedTree<State> (+8 more)

### Community 77 - "UringState"
Cohesion: 0.17
Nodes (11): BufRingEntry, BufRing, FileSlot, IoUringEngine, Drop, Mutex, RawFd, Send (+3 more)

### Community 78 - "StatusWireV1"
Cohesion: 0.15
Nodes (22): Condition, ErrorCategory, ErrorClassification, NotificationClassification, completion_omits_severity_and_classification(), DiagnosticRecordWireV1, error_classification(), FerrosGrynnError (+14 more)

### Community 79 - "FerrosGrynnResult"
Cohesion: 0.27
Nodes (7): RootedTree, BinaryLiftingIndex, FerrosGrynnResult, FnMut, Self, State, Tree

### Community 80 - "PropertyDictionary"
Cohesion: 0.15
Nodes (12): assigns_dense_ids_and_resolves_both_directions(), interning_is_idempotent_and_iteration_uses_id_order(), PropertyDictionary, ExactSizeIterator, FerrosGrynnResult, IntoText, Item, Option (+4 more)

### Community 81 - "AtomId"
Cohesion: 0.20
Nodes (11): Condensation, CutFrame, CutSet, Predecessor, AtomId, Box, Option, Path (+3 more)

### Community 82 - "Value"
Cohesion: 0.17
Nodes (8): Box, From, FxHashMap, Option, Self, String, Vec, Value

### Community 83 - "EdgeEndpoint"
Cohesion: 0.20
Nodes (7): EdgeEndpoint, From, endpoint_kind(), AtomKind, TraversalIndex, TraversalSemantics, TraversalStep

### Community 84 - "AccumList"
Cohesion: 0.15
Nodes (15): AccumList, FieldAccumArgs, Attribute, DeriveInput, Ident, Option, Parse, ParseStream (+7 more)

### Community 85 - "DiagnosticRecord"
Cohesion: 0.15
Nodes (9): DiagnosticRecord, ExecutionPhase, BTreeMap, Self, Str, String, SystemTime, Vec (+1 more)

### Community 86 - "Bytes"
Cohesion: 0.16
Nodes (8): Bytes, exposes_only_byte_oriented_api(), From, IntoVec, Iter, Self, Vec, Vec<u8>

### Community 87 - "bench_graph.rs"
Cohesion: 0.26
Nodes (16): bench_bfs_chain(), bench_bfs_dense(), bench_dfs_chain(), bench_dijkstra_chain(), bench_ecs_insert(), bench_ecs_iter(), bench_edge_insert_directed(), bench_materialise_chain() (+8 more)

### Community 88 - "os.rs"
Cohesion: 0.14
Nodes (9): MemoryError, NativeMemoryAllocationRefusedRepr, Display, Error, Formatter, Result, get_max_file_descriptors(), get_open_file_descriptors() (+1 more)

### Community 89 - ".register_datafusion_table"
Cohesion: 0.18
Nodes (11): IntoStr, MemTable, SessionContext, PropertyBatch, registers_only_after_arrow_batch_preserves_sentinels(), Arc, FerrosGrynnResult, Option (+3 more)

### Community 90 - "DiagnosticValue"
Cohesion: 0.14
Nodes (11): Str, bool, DiagnosticValue, f32, f64, BTreeMap, String, StatusParameter (+3 more)

### Community 91 - "Graphify"
Cohesion: 0.15
Nodes (13): Add URL and Watch Folder, Graphify Extra Exports, Semantic Extraction Contract, GitHub Clone and Cross-Repo Merge, Graphify Commit and Claude Hooks, Graph Query Path and Explain, Video and Audio Transcription, Incremental Graph Update (+5 more)

### Community 92 - "PropertyMap"
Cohesion: 0.23
Nodes (7): bench_search(), Property, PropertyMap, Criterion, Self, String, Vec

### Community 93 - "wy_hash.rs"
Cohesion: 0.29
Nodes (7): i64_le(), u32(), u64_rotate32(), wy_hash64(), WyHash, wymum(), wyr3()

### Community 94 - "algorithm_failure"
Cohesion: 0.16
Nodes (13): algorithm_failure(), binary_edges(), BinaryEdge, DisjointSets, find_cuts(), has_cycle_undirected(), minimum_spanning_forest(), minimum_spanning_tree() (+5 more)

### Community 95 - "MaterializationOrigin"
Cohesion: 0.21
Nodes (3): MaterializationOrigin, BTreeMap, Uuid

### Community 97 - "HashFunction"
Cohesion: 0.21
Nodes (3): IncrementalXXH64, HashFunction, UtilHash

### Community 98 - "Option"
Cohesion: 0.20
Nodes (8): GraphWrite, GraphWriteView, MutableGraphValue, Mutable, Option, Hypergraph, PhantomData, Metagraph

### Community 99 - "Option"
Cohesion: 0.11
Nodes (10): DiskGraphView<'_, R>, EdgeView<'a>, GraphReadView<'_, R>, NodeView, EdgeTopology, Frozen, Number, Option (+2 more)

### Community 100 - "graph_core.rs"
Cohesion: 0.45
Nodes (10): Criterion, benchmark_access_modes(), benchmark_path_and_live_publication(), benchmark_state_and_family_conversions(), benchmark_traversal(), dag_chain(), general_chain(), path() (+2 more)

### Community 101 - "build_from_map"
Cohesion: 0.27
Nodes (9): AnyValue, AccumulatorBuilder, build_from_map(), FerrosGrynnResult, IntoIterator, Item, Output, String (+1 more)

### Community 102 - "memohash.rs"
Cohesion: 0.24
Nodes (6): Cell, ConstZero, MemoHashed, MemoizedHash, Self, Zero

### Community 103 - "Ferros-Grynn Repository Guidance"
Cohesion: 0.22
Nodes (9): Just Command Runner, Just Recipes, Ferros-Grynn Repository Guidance, Prototype-to-Production Migration Architecture, Claude Ferros-Grynn Guidance, gql_status Module Guidance, Primary and Labeled Source Locations, Generated Status Catalog DSL (+1 more)

### Community 104 - "Physical Query Execution"
Cohesion: 0.22
Nodes (9): Checkpoint and Single-Pass Recovery, Frame-WAL, Deterministic Execution, Lexer Parser and Semantic Frontend, Rule and Cost Based Optimizer, Snapshot-Aware PageManager, Physical Query Execution, Query Processing Pipeline (+1 more)

### Community 105 - "Values Module Guidance"
Cohesion: 0.25
Nodes (9): RecordId DiskAtomRef AtomId Mapping, Storage Index System, Distinct None and Null Semantics, PropertyBatch Presence Model, AtomId-Keyed ECS, Public Graph Surface, Arrow Validity Plus Presence Bitmap, Value Runtime Invariants (+1 more)

### Community 106 - "GrynQL Language Specification"
Cohesion: 0.33
Nodes (9): Universal Page Layouts, Ferros-Grynn Disk Storage Format, Three Storage Modes, GrynQL Language Specification, GrynQL Logical Plan, GrynQL Type System, GrynQL Query Engine Technical Specification, Ferros-Grynn Implementation Plan (+1 more)

### Community 107 - "atoms"
Cohesion: 0.46
Nodes (7): atoms(), benchmark_build(), benchmark_dense_iteration(), benchmark_lookup(), AtomId, Criterion, Vec

### Community 108 - "helpers.rs"
Cohesion: 0.25
Nodes (7): hash_bool(), hash_f64(), hash_hashables(), hash_i64(), hash_str(), hash_u64(), I

### Community 109 - "join.rs"
Cohesion: 0.57
Nodes (6): A, atom(), B, chooses_actual_smallest_pool_independent_of_type_order(), missing_pool_produces_empty_join(), AtomId

### Community 110 - ".run"
Cohesion: 0.23
Nodes (16): decodes_quoted_names_parameters_and_string_escapes(), is_ident_continue(), is_ident_start(), keeps_utf8_byte_spans_and_greedy_operators(), lex(), Lexer, parse_error(), Box (+8 more)

### Community 111 - "datastore.rs"
Cohesion: 0.09
Nodes (26): AtomicU64, Instant, Mutex, PropertyDictionary, ActiveSnapshot, atom_versions_keep_reused_slot_generations_distinct(), AtomVersions, CommitResult (+18 more)

### Community 112 - "Specification Design Authority"
Cohesion: 0.33
Nodes (6): Specification Design Authority, AtomWalk Syntax, MATCH Graph Patterns, GrynQL EBNF Grammar, Context-Sensitive Parser Disambiguation, GrynQL Statements

### Community 113 - "Persistent MVCC v2"
Cohesion: 0.33
Nodes (6): ClosureCodec, Exact Phantom Matching, Persistent MVCC v2, Serializable Snapshot Isolation, GrynQL Transaction Model, Execution MVCC and Snapshot Contract

### Community 114 - "Unified Atom Incidence"
Cohesion: 0.33
Nodes (6): Unified Atom Incidence, Graph Storage Families, Ferros-Grynn Graph Taxonomy, Mutable Frozen Type-State and Views, P4 Full Atom Incidence, GraphWorkspace ECS Sidecar

### Community 115 - "RecordId"
Cohesion: 0.24
Nodes (3): AtomRef, NodeView<'a>, RecordId

### Community 116 - "hashing/mod.rs"
Cohesion: 0.33
Nodes (3): Option, Ordering, Hashable

### Community 118 - "QueueEntry"
Cohesion: 0.28
Nodes (6): QueueEntry, Eq, Ord, Ordering, PartialEq, PartialOrd

### Community 119 - "laws.rs"
Cohesion: 0.53
Nodes (5): hash(), number(), Value, scalar_value(), Strategy

### Community 120 - "ferros-grynn-macros/src/lib.rs"
Cohesion: 0.70
Nodes (4): define_status_codes(), measurable(), TokenStream, structure_accumulator()

### Community 121 - "Hybrid Query Model"
Cohesion: 0.50
Nodes (5): Hybrid Query Model, MATCH Pattern Matching, StandardPath and AtomWalk Semantics, Traversal Pipeline, Ferros-Grynn Graph Engine

### Community 122 - "struct_size.rs"
Cohesion: 0.40
Nodes (3): Test1, Test2, Test3

### Community 123 - "Canonical Storage Value Codec"
Cohesion: 0.50
Nodes (4): Canonical Storage Value Codec, OLTP Property Heap and Column Cache, Runtime Storage Columnar Boundary, StorageValue and Canonical Codec

### Community 124 - "P3 Query Execution"
Cohesion: 0.50
Nodes (4): P3 Query Execution, P2 Query Frontend, P2 Storage WAL and MVCC, P1 Value Foundation

### Community 125 - ".next"
Cohesion: 0.50
Nodes (3): BoxedEnumIter<'a>, Item, Iterator

### Community 126 - "&'a Bytes"
Cohesion: 0.50
Nodes (3): &'a Bytes, IntoIter, IntoIterator

### Community 137 - "TransactionGraphView"
Cohesion: 0.20
Nodes (11): all_dirty(), materialize_atom(), materialize_rows(), QueryStorageRow, row_from_properties(), EdgeIds, NodeIds, PropertyMap (+3 more)

### Community 138 - "MaterializedGraph"
Cohesion: 0.09
Nodes (19): merge_materialized(), MaterializedGraph, preserves_exact_discriminant_in_generic_transport(), EdgeIds, Eq, FerrosGrynnResult, Frozen, H (+11 more)

### Community 139 - "ir.rs"
Cohesion: 0.21
Nodes (16): Column, LogicalNode, LogicalNodeKind, LogicalProgram, LogicalSelect, LogicalStatement, NodeId, Box (+8 more)

### Community 140 - "TxId"
Cohesion: 0.15
Nodes (23): closure_codec_with_capture_in_persistent_store(), encode_atom_meta(), multi_segment_reader(), PageLsnTable, prune_before_removes_old_segments(), recover(), recovery_restores_atom_meta(), RecoveryResult (+15 more)

### Community 141 - ".clone"
Cohesion: 0.20
Nodes (13): RwLock, disjoint_remote_update_is_applied_automatically(), frozen_guards_keep_the_revision_they_observed(), LiveGraph<Frozen>, LiveInner, LiveReadGuard, mutable_live_graph_pauses_on_dirty_intersection(), Arc (+5 more)

### Community 142 - "Type"
Cohesion: 0.10
Nodes (13): is_string(), is_vec_string(), is_option(), AtomKind, Box, Display, Formatter, GraphKind (+5 more)

### Community 143 - ".mutate_local"
Cohesion: 0.25
Nodes (5): ApplyFn, ReadFn, MutFn, T, Targets

### Community 144 - "ForestStorage"
Cohesion: 0.05
Nodes (55): build_parent_map(), build_storage(), constraint(), find_root(), Forest, Forest<Frozen>, Forest<Mutable>, forest_node_removal_marks_every_incident_edge_dirty() (+47 more)

### Community 145 - "Result"
Cohesion: 0.64
Nodes (3): ParseStream, Result, Self

### Community 146 - "TableSchema"
Cohesion: 0.21
Nodes (8): PropertyKeyId, ExactSizeIterator, Item, Text, Type, TableField, TableKind, TableSchema

### Community 147 - "live.rs"
Cohesion: 0.16
Nodes (15): Forest, accept_remote_discards_local_conflicting_state(), conflicted_live_graph(), discard_local_applies_the_pending_remote_delta(), has_property(), keep_local_discards_the_pending_remote_delta(), LiveGraph, rebase_replays_local_changes_over_remote_state() (+7 more)

### Community 148 - "DirtyTarget"
Cohesion: 0.29
Nodes (4): Vec, DirtyTarget, ExactSizeIterator, Item

### Community 149 - "PropertyIter"
Cohesion: 0.16
Nodes (8): FusedIterator, SliceIter, PropertyIter, PropertyIter<'a>, DoubleEndedIterator, ExactSizeIterator, Item, Iterator

### Community 150 - ".identity"
Cohesion: 0.32
Nodes (3): Formatter, H, Result

### Community 151 - "Subgraph"
Cohesion: 0.36
Nodes (3): Graph, State, Subgraph

### Community 152 - "Iter<'_, T>"
Cohesion: 0.67
Nodes (3): Iter<'_, T>, ExactSizeIterator, FusedIterator

### Community 154 - "QueryResult"
Cohesion: 0.23
Nodes (11): Outcome, Object, ParserSettings, Vec, parse(), QueryResult, QueryStatementKind, Duration (+3 more)

### Community 155 - "AtomIds"
Cohesion: 0.29
Nodes (5): AtomIds, ExactSizeIterator, FusedIterator, Item, Iterator

### Community 156 - "Option"
Cohesion: 0.29
Nodes (4): EdgeView, GraphRef, NodeView, Option

### Community 158 - "Subgraph<Frozen>"
Cohesion: 0.50
Nodes (3): QueryMaterialize, Clone, Subgraph<Frozen>

### Community 159 - "Handle<'_, T>"
Cohesion: 0.50
Nodes (3): Handle<'_, T>, Deref, Target

### Community 205 - "Metagraph<State>"
Cohesion: 0.06
Nodes (34): exposes_explicit_meta_atom_api_over_general_layout(), Metagraph<Frozen>, Metagraph<Mutable>, Metagraph<State>, Clone, Debug, Default, EdgeIds (+26 more)

## Knowledge Gaps
- **54 isolated node(s):** `Formatted<'a, F, T>`, `FerrosGrynnError`, `StatusReport`, `AtomIds<'a>`, `DiskGraphView<'tx, R>` (+49 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **118 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Value` connect `Value` to `Uuid`, `Type`, `Path`, `live.rs`, `PropertyIter`, `PropertyKeyId`, `Position`, `Result`, `Duration`, `Closure`, `arrow.rs`, `Array`, `Vector`, `Range`, `TextSpan`, `Regex`, `Set`, `Object`, `Bytes`?**
  _High betweenness centrality (0.210) - this node is a cross-community bridge._
- **Why does `TextSpan` connect `TextSpan` to `ir.rs`, `Location`, `DiagnosticRecord`, `.run`?**
  _High betweenness centrality (0.108) - this node is a cross-community bridge._
- **Why does `AtomId` connect `AtomId` to `DagStorage`, `Hypergraph<State>`, `read.rs`, `TransactionGraphView`, `MaterializedGraph`, `EdgeColumns`, `Graph`, `ForestStorage`, `DirtyTarget`, `AtomIds`, `Transaction`, `SIV<T>`, `.new`, `Dag<State>`, `Metagraph<State>`, `MaterializationOrigin`, `Option`, `Option`, `graph_core.rs`, `datastore.rs`, `RecordId`?**
  _High betweenness centrality (0.088) - this node is a cross-community bridge._
- **What connects `Formatted<'a, F, T>`, `FerrosGrynnError`, `StatusReport` to the rest of the system?**
  _54 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Parser` be split into smaller, more focused modules?**
  _Cohesion score 0.07643758765778401 - nodes in this community are weakly interconnected._
- **Should `DagStorage` be split into smaller, more focused modules?**
  _Cohesion score 0.08322981366459627 - nodes in this community are weakly interconnected._
- **Should `Hypergraph<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.052795031055900624 - nodes in this community are weakly interconnected._