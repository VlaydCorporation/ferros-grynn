# Graph Report - ferros-grynn  (2026-08-10)

## Corpus Check
- 148 files · ~195,609 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 4470 nodes · 11501 edges · 241 communities (150 shown, 91 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 101 edges (avg confidence: 0.81)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `8d6a35c3`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- TextSpan
- ForestStorage
- Dag<State>
- Hypergraph<State>
- AtomId
- Mutable
- read.rs
- MvccTransaction
- AtomId
- AtomId
- BufferPool
- AtomId
- Graph
- Uuid
- Metagraph<State>
- Path
- mvcc_persist.rs
- win_async.rs
- Number
- src/ecs.rs
- FerrosGrynnResult
- Graph<State>
- Subgraph<State>
- store.rs
- status_codes.rs
- PropertyKeyId
- topology.rs
- AtomKind
- Value
- Position
- GraphWorkspace<G>
- src/traversal.rs
- AtomId
- Result
- Text
- FerrosGrynnError
- Location
- Result
- SparseSet<T>
- Duration
- SIV<T>
- status.rs
- formatting.rs
- PosixDevice
- Closure
- DirectFile
- benches/property_map.rs
- MvccManager
- async_io.rs
- arrow.rs
- StatusReport
- Array
- Vector
- PropertyColumn<T>
- Range
- Lsn
- .new
- graph/traversal.rs
- StatusCode
- heap_estimator.rs
- Self
- .expr
- Regex
- Set
- AsyncIo
- AdjacencyMatrix
- Option
- Object
- get_metadata_inner
- StorageBackend
- fg-meta/src/lib.rs
- PropertyBatch
- UringState
- HashValue
- WorldComponents
- .new
- exact_ops.rs
- ir.rs
- StatusWireV1
- FerrosGrynnResult
- PropertyDictionary
- Type
- Value
- EdgeEndpoint
- AccumList
- DiagnosticValue
- Bytes
- bench_graph.rs
- os.rs
- .register_datafusion_table
- PropertyIter
- Graphify
- PropertyMap
- wy_hash.rs
- ecs/mod.rs
- SparseSet<T>
- MetaGraph
- HashFunction
- property_column.rs
- GraphId
- bfs
- build_from_map
- memohash.rs
- Ferros-Grynn Repository Guidance
- Physical Query Execution
- Values Module Guidance
- GrynQL Language Specification
- atoms
- helpers.rs
- join.rs
- TreeSubGraph
- ClockProInner
- Specification Design Authority
- Persistent MVCC v2
- Unified Atom Incidence
- StatusObject
- hashing/mod.rs
- XorShift32HashFunction
- execution.rs
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
- DiagnosticRecord
- ValueKind
- MaterializedGraph
- SyncIo
- TxId
- Self
- runtime/mod.rs
- AlignedBuffer
- EdgeInfo
- PropertyColumn
- src/id.rs
- OrdFloat
- QueueEntry
- materialise
- .endpoints
- Condensation
- write_sequence
- bellman_ford
- .add_hyper_directed
- LocationError
- MetaGraphError
- BinaryOperator
- .fmt
- .with_extension
- .raw_fd
- AnyPool
- AtomId
- Criterion
- AtomId
- TypeId
- AtomId
- D
- Err
- FromStr
- String
- SystemTime
- AtomId
- EdgeTopology
- Number
- RecordId
- String
- Text
- AtomId
- EdgeTopology
- Number
- RecordId
- String
- Text
- AtomId
- AtomKind
- EdgeTopology
- Number
- RecordId
- String
- Text
- AtomId
- EdgeTopology
- Number
- RecordId
- String
- Text
- AtomId
- Deref
- Fn
- Mutex
- RwLock
- String
- Target
- VecDeque
- AtomId
- EdgeTopology
- Text
- AtomKind
- Number
- AtomKind
- EdgeTopology
- Number
- RecordId
- String
- Text
- AtomId
- AtomKind
- EdgeTopology
- Number
- RecordId
- SlotAllocator
- Text
- AtomId
- AtomId
- AtomId
- AtomKind
- DoubleEndedIterator
- EdgeTopology
- String
- AtomId
- Number
- RecordId
- String
- Text
- FromIterator
- IntoIter
- Value
- AtomKind
- Text
- Value

## God Nodes (most connected - your core abstractions)
1. `AtomId` - 284 edges
2. `MetaGraph` - 153 edges
3. `Value` - 90 edges
4. `Graph` - 74 edges
5. `Text` - 65 edges
6. `RecordId` - 51 edges
7. `TxId` - 50 edges
8. `Lsn` - 48 edges
9. `Frozen` - 46 edges
10. `EdgeTopology` - 45 edges

## Surprising Connections (you probably didn't know these)
- `Public Graph Surface` --semantically_similar_to--> `Graph Storage Families`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/graphs.md
- `Claude Ferros-Grynn Guidance` --semantically_similar_to--> `Prototype-to-Production Migration Architecture`  [INFERRED] [semantically similar]
  CLAUDE.md → AGENTS.md
- `Value Runtime Invariants` --semantically_similar_to--> `Distinct None and Null Semantics`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/values_plan.md
- `Arrow Validity Plus Presence Bitmap` --semantically_similar_to--> `PropertyBatch Presence Model`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/values_plan.md
- `benchmark_traversal()` --calls--> `bfs()`  [INFERRED]
  ferros-grynn-bench/benches/graph_core.rs → src/values/runtime/graph/traversal.rs

## Import Cycles
- 1-file cycle: `src/values/runtime/text.rs -> src/values/runtime/text.rs`
- 1-file cycle: `src/values/runtime/graph/general.rs -> src/values/runtime/graph/general.rs`
- 1-file cycle: `src/values/runtime/mod.rs -> src/values/runtime/mod.rs`
- 1-file cycle: `src/values/runtime/graph/hypergraph.rs -> src/values/runtime/graph/hypergraph.rs`
- 1-file cycle: `src/values/runtime/graph/metagraph.rs -> src/values/runtime/graph/metagraph.rs`
- 1-file cycle: `src/values/runtime/graph/subgraph.rs -> src/values/runtime/graph/subgraph.rs`
- 1-file cycle: `src/values/runtime/set.rs -> src/values/runtime/set.rs`
- 1-file cycle: `src/values/runtime/types.rs -> src/values/runtime/types.rs`
- 1-file cycle: `src/values/runtime/number.rs -> src/values/runtime/number.rs`
- 1-file cycle: `src/common/memory/measurable.rs -> src/common/memory/measurable.rs`
- 1-file cycle: `src/values/runtime/bytes.rs -> src/values/runtime/bytes.rs`
- 1-file cycle: `src/values/runtime/property_dictionary.rs -> src/values/runtime/property_dictionary.rs`
- 1-file cycle: `src/values/runtime/range.rs -> src/values/runtime/range.rs`
- 1-file cycle: `src/values/runtime/regex.rs -> src/values/runtime/regex.rs`
- 1-file cycle: `src/values/runtime/vector.rs -> src/values/runtime/vector.rs`
- 2-file cycle: `src/values/runtime/mod.rs -> src/values/runtime/set.rs -> src/values/runtime/mod.rs`

## Hyperedges (group relationships)
- **GrynQL Contract Stack** — docs_gql_spec_grynql_language, docs_gql_grammar_grynql_grammar, docs_gql_tech_spec_query_engine [INFERRED 0.95]
- **Storage Persistence Contract** — docs_disk_storage_spec_frame_wal, docs_disk_storage_spec_checkpoint_recovery, docs_gql_tech_spec_page_manager, crates_fg_meta_implementation_notes_persistent_mvcc [INFERRED 0.85]
- **Values Runtime Storage Separation** — docs_values_plan_runtime_storage_boundary, docs_values_plan_storage_value, src_values_agents_runtime_invariants, src_values_agents_presence_bitmap [INFERRED 0.95]

## Communities (241 total, 91 thin omitted)

### Community 0 - "TextSpan"
Cohesion: 0.06
Nodes (61): Identifier, Config, Default, Self, TextSpan, cover(), Expr, ExprKind (+53 more)

### Community 1 - "ForestStorage"
Cohesion: 0.06
Nodes (50): build_parent_map(), build_storage(), constraint(), find_root(), Forest, Forest<Frozen>, Forest<Mutable>, forest_node_removal_marks_every_incident_edge_dirty() (+42 more)

### Community 2 - "Dag<State>"
Cohesion: 0.06
Nodes (41): build_oriented_tree_storage(), build_storage(), constraint(), Dag, Dag<Frozen>, Dag<Mutable>, Dag<State>, DagStorage (+33 more)

### Community 3 - "Hypergraph<State>"
Cohesion: 0.06
Nodes (38): build_storage(), constraint(), graph_conversion_rejects_edge_endpoints(), Hypergraph, Hypergraph<Frozen>, Hypergraph<Mutable>, Hypergraph<State>, HyperStorage (+30 more)

### Community 4 - "AtomId"
Cohesion: 0.15
Nodes (5): AtomId, EdgeTopology, Option, Result, Value

### Community 5 - "Mutable"
Cohesion: 0.07
Nodes (56): ApplyUpdate, Deref, Fn, Mutex, MutexGuard, RwLock, accept_remote_discards_local_conflicting_state(), apply_or_pause() (+48 more)

### Community 6 - "read.rs"
Cohesion: 0.03
Nodes (47): assert_snapshot_released(), AtomRef, cascade_removal_marks_removed_incident_edges_dirty(), disk_view_and_hybrid_materialization_have_the_same_read_result(), DiskGraphView, DiskGraphView<'_, R>, DiskGraphView<'tx, R>, EdgeView (+39 more)

### Community 7 - "MvccTransaction"
Cohesion: 0.10
Nodes (33): basic_generic_rw(), BytesCodec, closure_codec_with_capture(), ClosureCodec, ClosureCodec<T>, Codec, exact_phantom_label(), MvccError (+25 more)

### Community 8 - "AtomId"
Cohesion: 0.13
Nodes (23): bfs_spanning_tree(), DagSubGraph, extract_dag(), extract_forest(), extract_hyper(), extract_tree(), ExtractionError, HyperSubGraph (+15 more)

### Community 9 - "AtomId"
Cohesion: 0.11
Nodes (8): AtomId, AtomKind, kind_bit_encoding(), Display, Formatter, Result, SlotAllocator<EdgeKindMarker>, SlotAllocator<NodeKindMarker>

### Community 10 - "BufferPool"
Cohesion: 0.14
Nodes (15): backend(), BufferPool, clock_pro_scan_resistance(), dirty_flushed_on_eviction(), eviction_produces_ghost(), Frame, FrameStatus, ghost_hit_promotes_to_hot() (+7 more)

### Community 11 - "AtomId"
Cohesion: 0.08
Nodes (17): AtomId, Debug, EdgeColumns, empty_topology(), ensure_unique_record(), GraphMetadata, NodeColumns, BTreeSet (+9 more)

### Community 12 - "Graph"
Cohesion: 0.07
Nodes (33): INLINE_ENDPOINTS, INLINE_INCIDENCE, atom_does_not_exist(), atom_kind_mismatch(), copy_from_read(), endpoint_kind(), GeneralStorage, Graph (+25 more)

### Community 13 - "Uuid"
Cohesion: 0.07
Nodes (36): JsonValue, InnerUlid, InnerUuid, Display, Err, Formatter, From, FromStr (+28 more)

### Community 14 - "Metagraph<State>"
Cohesion: 0.06
Nodes (30): exposes_explicit_meta_atom_api_over_general_layout(), Metagraph, Metagraph<Frozen>, Metagraph<Mutable>, Metagraph<State>, Clone, Debug, Default (+22 more)

### Community 15 - "Path"
Cohesion: 0.07
Nodes (35): EndpointRole, Path, PathDirection, PathStep, PathTraceArena, PathTraceId, PathTraceNode, resolve_atom() (+27 more)

### Community 16 - "mvcc_persist.rs"
Cohesion: 0.17
Nodes (23): atom_id_from(), BytesPersistentStore, checkpoint_then_recover(), CheckpointHeader, closure_codec_with_capture_in_persistent_store(), encode_atom_meta(), multi_segment_reader(), PersistentMvccStore<T, C> (+15 more)

### Community 17 - "win_async.rs"
Cohesion: 0.05
Nodes (47): AtomicI64, cpu_affinity_cpu0(), goto_shutdown(), ioring_handle_ref(), ioring_raw_buffer_ref(), ioring_registered_buffer_ref(), new_fails_gracefully_if_no_ioring(), PendOp (+39 more)

### Community 18 - "Number"
Cohesion: 0.07
Nodes (36): cmp_f64(), cmp_f64_scaled_integer(), cmp_float_decimal(), cmp_int_float(), decompose_f64(), BigInt, Ordering, canonical_float() (+28 more)

### Community 19 - "src/ecs.rs"
Cohesion: 0.07
Nodes (34): AnyPool, ColorTag, FeatureVector, node_id(), NodeWeight, PageRank, Position2D, Position3D (+26 more)

### Community 20 - "FerrosGrynnResult"
Cohesion: 0.16
Nodes (26): binary(), build_cursor(), cast(), eval(), eval_usize(), execute_select(), FilterCursor, index_value() (+18 more)

### Community 21 - "Graph<State>"
Cohesion: 0.09
Nodes (19): Copied, freeze_thaw_preserves_identity_ids_and_topology(), Graph<State>, Debug, EdgeIds, Eq, Formatter, H (+11 more)

### Community 22 - "Subgraph<State>"
Cohesion: 0.07
Nodes (24): marks_materialized_result_without_new_storage_family(), Clone, Debug, Default, EdgeIds, Eq, FerrosGrynnResult, Formatter (+16 more)

### Community 23 - "store.rs"
Cohesion: 0.10
Nodes (31): auto_checkpoint_triggers(), checkpoint_clears_wal(), compress_payload(), crc32(), crc32_update(), create_and_reopen(), decompress_payload(), delete_graph() (+23 more)

### Community 24 - "status_codes.rs"
Cohesion: 0.09
Nodes (29): define_status_codes(), is_string(), is_vec_string(), last_path_ident(), option_lit(), Ident, Option, Parse (+21 more)

### Community 25 - "PropertyKeyId"
Cohesion: 0.08
Nodes (29): clone_is_independent(), empty_map_is_pointer_sized_and_does_not_reserve_storage(), entry_ref(), equality_order_and_hash_ignore_insertion_order(), from_iterator_uses_last_duplicate_value(), keeps_entries_sorted_and_replaces_existing_values(), PropertyKeyId, PropertyMap (+21 more)

### Community 26 - "topology.rs"
Cohesion: 0.09
Nodes (17): distinguishes_binary_hyper_and_cross_level_topology(), EdgeDirection, QualifiedAtomId, Box, Display, FerrosGrynnResult, FnMut, Formatter (+9 more)

### Community 27 - "AtomKind"
Cohesion: 0.13
Nodes (11): atom_id_uses_u40_slots_and_u23_generations(), AtomKind, Display, FerrosGrynnResult, Formatter, Option, Result, Self (+3 more)

### Community 28 - "Value"
Cohesion: 0.08
Nodes (22): Array, Bytes, DateTime, Duration, Geometry, MaterializedGraph, Object, Range (+14 more)

### Community 29 - "Position"
Cohesion: 0.07
Nodes (35): GeoJsonGeometry, GeoJsonPosition, assert_status(), compare_float_slices(), GeoJsonValue, Geometry, into_position(), into_positions() (+27 more)

### Community 30 - "GraphWorkspace<G>"
Cohesion: 0.10
Nodes (23): canonical_mutation_invalidates_derived_and_removes_dead_components(), component_mutation_does_not_change_graph_revision(), G, GraphWorkspace, GraphWorkspace<G>, IntoGraphWorkspace, map_graph_moves_user_components_but_discards_derived_pools(), PageRank (+15 more)

### Community 31 - "src/traversal.rs"
Cohesion: 0.16
Nodes (30): bfs(), bfs_with(), dfs(), dfs_full(), dijkstra(), dijkstra_dir(), Direction, find_cuts() (+22 more)

### Community 33 - "Result"
Cohesion: 0.26
Nodes (10): AtomRef, bool, Graph<Frozen>, MaterializedGraph, Path, Error, Result, Self (+2 more)

### Community 34 - "Text"
Cohesion: 0.08
Nodes (24): AsRef, Borrow, Extend, CompactString, exposes_controlled_utf8_mutation(), round_trips_through_std_and_compact_strings(), Deref, Display (+16 more)

### Community 35 - "FerrosGrynnError"
Cohesion: 0.11
Nodes (20): ErrorState, FerrosGrynnError, NonErrorStatus, Arc, Debug, Display, E, Error (+12 more)

### Community 36 - "Location"
Cohesion: 0.19
Nodes (9): NonZeroU32, computes_unicode_locations_without_retaining_source(), LabeledLocation, line_column_at(), LineColumn, Location, rejects_non_character_boundary(), Arc (+1 more)

### Community 37 - "Result"
Cohesion: 0.12
Nodes (26): Decoder, Decoder<R>, Encoder, Encoder<W>, from_bytes(), from_file(), roundtrip(), roundtrip_atom_kind_preserved() (+18 more)

### Community 38 - "SparseSet<T>"
Cohesion: 0.11
Nodes (21): atom(), invalid_atom(), rejects_null_atom(), AtomId, Box, Default, ExactSizeIterator, FerrosGrynnError (+13 more)

### Community 39 - "Duration"
Cohesion: 0.09
Nodes (20): DateTime, Duration, Default, Display, Eq, Err, Formatter, From (+12 more)

### Community 40 - "SIV<T>"
Cohesion: 0.09
Nodes (15): Id, Index, IndexMut, Handle, Handle<'a, T>, Metadata, Default, Deref (+7 more)

### Community 41 - "status.rs"
Cohesion: 0.16
Nodes (11): Condition, Domain, ErrorCategory, ErrorClassification, NotificationClassification, status_object_builds_full_description(), StatusDefinition, StatusKind (+3 more)

### Community 42 - "formatting.rs"
Cohesion: 0.11
Nodes (27): And, Callable, Comma, DisplayValue, Formatted, Formatted<'a, F, T>, Formatted<'_, F, T>, Identifier (+19 more)

### Community 43 - "PosixDevice"
Cohesion: 0.09
Nodes (18): NvmeDevice, NvmeStats, open_best(), open_best_posix(), posix_device_rw(), PosixDevice, Any, Arc (+10 more)

### Community 44 - "Closure"
Cohesion: 0.07
Nodes (28): Closure, ClosureFunction, NonZeroU64, calls_function_and_keeps_process_identity(), Closure, different_closures_have_different_identity(), rejects_wrong_arity_before_invocation(), Arc (+20 more)

### Community 45 - "DirectFile"
Cohesion: 0.19
Nodes (14): alloc_aligned(), DirectFile, platform_grow(), platform_open(), platform_open_readonly(), platform_write_page(), Any, AtomicU64 (+6 more)

### Community 46 - "benches/property_map.rs"
Cohesion: 0.12
Nodes (32): bench_build(), bench_iterate_and_clone(), bench_lookup(), boxed_enum_map(), BoxedEnumIter, BoxedEnumPropertyMap, BoxedEnumRepr, btree_map() (+24 more)

### Community 47 - "MvccManager"
Cohesion: 0.10
Nodes (19): AtomMeta, IsolationLevel, MvccManager, MvccStore, decode_atom_meta_payload(), decode_isolation(), encode_isolation(), PersistentMvccStore (+11 more)

### Community 48 - "async_io.rs"
Cohesion: 0.09
Nodes (34): AsyncIoEngine, AsyncUringEngine, BatchEntry, bg_loop(), blocking_op(), build(), build_ring(), bump_memlock() (+26 more)

### Community 49 - "arrow.rs"
Cohesion: 0.12
Nodes (32): ArrayRef, ArrowError, DataType, arrow_failure(), arrow_round_trip_preserves_none_null_and_native_scalars(), build_array(), ColumnKind, decode_rows() (+24 more)

### Community 50 - "StatusReport"
Cohesion: 0.12
Nodes (20): applies_gql_precedence(), Outcome, Outcome<T>, precedence(), rejects_errors(), Display, Error, ExactSizeIterator (+12 more)

### Community 51 - "Array"
Cohesion: 0.10
Nodes (17): &'a Array, Array, From, FromIterator, I, Into, IntoIter, IntoIterator (+9 more)

### Community 52 - "Vector"
Cohesion: 0.10
Nodes (20): canonical_f32(), canonical_f64(), compare_f32(), compare_f32_value(), compare_f64(), compare_f64_value(), hash_f32(), hash_f64() (+12 more)

### Community 53 - "PropertyColumn<T>"
Cohesion: 0.16
Nodes (9): PropertyColumn<T>, PropertyState, Default, ExactSizeIterator, FerrosGrynnResult, FnMut, Item, Option (+1 more)

### Community 54 - "Range"
Cohesion: 0.15
Nodes (24): Bound, borrow_bound(), Bound<T>, bound_value(), BoundExt, box_bound(), checks_inclusion(), compare_end() (+16 more)

### Community 55 - "Lsn"
Cohesion: 0.12
Nodes (20): BufReader, PageId, crc32(), decode_entry(), encode_entry(), entry_crc(), Lsn, MultiSegmentReader (+12 more)

### Community 56 - ".new"
Cohesion: 0.10
Nodes (18): BufWriter, PageLsnTable, PersistLayer, Arc, AtomicU64, Default, File, HashMap (+10 more)

### Community 57 - "graph/traversal.rs"
Cohesion: 0.15
Nodes (31): algorithm_failure(), bellman_ford(), bellman_ford_reports_negative_cycle_as_domain_error(), bfs_topology_and_components_are_correct(), binary_edges(), BinaryEdge, collect_weights(), condensation_collapses_cycles_and_deduplicates_bidirectional_steps() (+23 more)

### Community 58 - "StatusCode"
Cohesion: 0.13
Nodes (15): D, Deserialize, Err, FromStr, Ok, Serialize, Severity, InvalidSeverity (+7 more)

### Community 59 - "heap_estimator.rs"
Cohesion: 0.08
Nodes (21): HeapEstimatorCache, NoHeapEstimatorCache, Self, Measurable, HashMap, K, V, size_of_hash_map() (+13 more)

### Community 60 - "Self"
Cohesion: 0.12
Nodes (16): DagView, ForestView, GraphView, Dag<State>, DagView<'graph, R>, directed_forest_view(), Forest<State>, RootedTree<State> (+8 more)

### Community 61 - ".expr"
Cohesion: 0.19
Nodes (16): analyze(), Analyzer, Analyzer<'a>, compatible(), is_number(), QueryCatalog, Box, BTreeMap (+8 more)

### Community 62 - "Regex"
Cohesion: 0.10
Nodes (19): compiles_and_matches_pattern(), Regex, Display, Eq, Err, Error, Formatter, FromStr (+11 more)

### Community 63 - "Set"
Cohesion: 0.10
Nodes (15): FromIterator, IntoIter, &'a Set, BTreeSet<Value>, deduplicates_and_orders_values(), BTreeSet, From, I (+7 more)

### Community 64 - "AsyncIo"
Cohesion: 0.15
Nodes (16): async_io_batch_tag_routing(), async_io_write_read_sync(), AsyncIo, AsyncIoConfig, EngineConfig, Arc, Box, Default (+8 more)

### Community 65 - "AdjacencyMatrix"
Cohesion: 0.15
Nodes (13): AdjacencyMatrix, Incidence, IncidenceMatrix, IncidenceTensor, MetaAdjacencyMatrix, MetaTriple, AtomId, FxHashMap (+5 more)

### Community 66 - "Option"
Cohesion: 0.30
Nodes (8): dfs(), Predecessor, Option, Path, RapidHashMap, Vec, shortest_paths(), ShortestPaths

### Community 67 - "Object"
Cohesion: 0.06
Nodes (34): batch_random_access(), bench_collect(), bench_iter_sequential_borrowed(), bench_iter_sequential_owned(), bench_iter_sequential_with_step(), bench_push_preallocated(), bench_push_raw(), LargeData (+26 more)

### Community 68 - "get_metadata_inner"
Cohesion: 0.10
Nodes (23): add_m, Attrs, measurable_inner(), occurrence_error(), add, DeriveInput, Error, Parse (+15 more)

### Community 69 - "StorageBackend"
Cohesion: 0.14
Nodes (17): AsyncIoRingEngine, Arc, Default, HashMap, ThreadPoolEngine, extract_fd(), extract_handle(), extract_path() (+9 more)

### Community 70 - "fg-meta/src/lib.rs"
Cohesion: 0.13
Nodes (7): SmallVec, NodeInfo, NodeKind, AtomId, Box, Option, PropertyMap

### Community 71 - "PropertyBatch"
Cohesion: 0.16
Nodes (15): BooleanBuffer, imported_arrow_batch_requires_property_metadata(), invalid_batch(), PropertyBatch, BTreeMap, FerrosGrynnError, FerrosGrynnResult, Into (+7 more)

### Community 72 - "UringState"
Cohesion: 0.23
Nodes (8): BufRingEntry, BufRing, FileSlot, RawFd, Send, VecDeque, UringState, IoUring

### Community 73 - "HashValue"
Cohesion: 0.12
Nodes (14): AddAssign, ConstOne, Mul, MulAssign, One, HashValue, Add, Display (+6 more)

### Community 74 - "WorldComponents"
Cohesion: 0.21
Nodes (8): Box, FerrosGrynnResult, Option, PropertyKeyId, SparseSet, T, WorldComponents, TypeId

### Community 75 - ".new"
Cohesion: 0.22
Nodes (8): IoBatch, IoRequest, IoRingEngine, Self, Vec, tag_routing(), threadpool_write_read(), Vec

### Community 76 - "exact_ops.rs"
Cohesion: 0.17
Nodes (20): CheckedAdd, CheckedMul, CheckedSub, PrimInt, Signed, integer_arithmetic(), add_exact(), AddExact (+12 more)

### Community 77 - "ir.rs"
Cohesion: 0.21
Nodes (16): Column, LogicalNode, LogicalNodeKind, LogicalProgram, LogicalSelect, LogicalStatement, NodeId, Box (+8 more)

### Community 78 - "StatusWireV1"
Cohesion: 0.19
Nodes (17): completion_omits_severity_and_classification(), DiagnosticRecordWireV1, FerrosGrynnError, labeled_location_to_wire(), LabeledLocationWireV1, location_to_wire(), LocationWireV1, Box (+9 more)

### Community 79 - "FerrosGrynnResult"
Cohesion: 0.30
Nodes (6): BinaryLiftingIndex, FerrosGrynnResult, FnMut, Self, State, Uuid

### Community 80 - "PropertyDictionary"
Cohesion: 0.16
Nodes (12): assigns_dense_ids_and_resolves_both_directions(), interning_is_idempotent_and_iteration_uses_id_order(), PropertyDictionary, ExactSizeIterator, FerrosGrynnResult, Into, Item, Option (+4 more)

### Community 81 - "Type"
Cohesion: 0.13
Nodes (10): is_option(), is_integer(), Box, Display, Formatter, Result, Self, Vec (+2 more)

### Community 82 - "Value"
Cohesion: 0.17
Nodes (8): Box, From, FxHashMap, Option, Self, String, Vec, Value

### Community 83 - "EdgeEndpoint"
Cohesion: 0.17
Nodes (11): DoubleEndedIterator, EdgeEndpoint, From, deduplicated_steps(), endpoint_kind(), local_steps(), Item, TraversalDirection (+3 more)

### Community 84 - "AccumList"
Cohesion: 0.15
Nodes (15): AccumList, FieldAccumArgs, Attribute, DeriveInput, Ident, Option, Parse, ParseStream (+7 more)

### Community 85 - "DiagnosticValue"
Cohesion: 0.17
Nodes (10): bool, DiagnosticValue, f32, f64, BTreeMap, String, StatusParameter, str (+2 more)

### Community 86 - "Bytes"
Cohesion: 0.18
Nodes (8): Bytes, exposes_only_byte_oriented_api(), From, Into, Iter, Self, Vec, Vec<u8>

### Community 87 - "bench_graph.rs"
Cohesion: 0.26
Nodes (16): bench_bfs_chain(), bench_bfs_dense(), bench_dfs_chain(), bench_dijkstra_chain(), bench_ecs_insert(), bench_ecs_iter(), bench_edge_insert_directed(), bench_materialise_chain() (+8 more)

### Community 88 - "os.rs"
Cohesion: 0.14
Nodes (9): MemoryError, NativeMemoryAllocationRefusedRepr, Display, Error, Formatter, Result, get_max_file_descriptors(), get_open_file_descriptors() (+1 more)

### Community 89 - ".register_datafusion_table"
Cohesion: 0.19
Nodes (11): MemTable, SessionContext, PropertyBatch, registers_only_after_arrow_batch_preserves_sentinels(), Arc, FerrosGrynnResult, Into, Option (+3 more)

### Community 90 - "PropertyIter"
Cohesion: 0.15
Nodes (9): FusedIterator, PropertyIter, PropertyIter<'a>, DoubleEndedIterator, Entry, ExactSizeIterator, Item, Iterator (+1 more)

### Community 91 - "Graphify"
Cohesion: 0.15
Nodes (13): Add URL and Watch Folder, Graphify Extra Exports, Semantic Extraction Contract, GitHub Clone and Cross-Repo Merge, Graphify Commit and Claude Hooks, Graph Query Path and Explain, Video and Audio Transcription, Incremental Graph Update (+5 more)

### Community 92 - "PropertyMap"
Cohesion: 0.23
Nodes (7): bench_search(), Property, PropertyMap, Criterion, Self, String, Vec

### Community 93 - "wy_hash.rs"
Cohesion: 0.29
Nodes (7): i64_le(), u32(), u64_rotate32(), wy_hash64(), WyHash, wymum(), wyr3()

### Community 94 - "ecs/mod.rs"
Cohesion: 0.36
Nodes (5): atom(), remove_atom_cleans_every_pool_and_respects_generation(), RapidHashMap, Self, separates_component_and_property_namespaces()

### Community 95 - "SparseSet<T>"
Cohesion: 0.15
Nodes (6): AnyPool, Any, FnMut, Send, Sync, SparseSet<T>

### Community 96 - "MetaGraph"
Cohesion: 0.10
Nodes (10): MetaGraph, Box, Default, Item, Iterator, PropertyMap, Self, SlotAllocator (+2 more)

### Community 97 - "HashFunction"
Cohesion: 0.21
Nodes (3): IncrementalXXH64, HashFunction, UtilHash

### Community 98 - "property_column.rs"
Cohesion: 0.29
Nodes (6): atom(), distinguishes_none_null_and_value(), removing_null_reports_null_instead_of_none(), replacement_removal_and_generation_keep_bitmaps_aligned(), Any, Self

### Community 99 - "GraphId"
Cohesion: 0.09
Nodes (8): GraphId, Debug, Default, Self, slot_allocator_edge(), slot_allocator_node(), SlotAllocator<GraphKindMarker>, SlotAllocator<K>

### Community 100 - "bfs"
Cohesion: 0.44
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
Cohesion: 0.46
Nodes (7): A, atom(), B, chooses_actual_smallest_pool_independent_of_type_order(), missing_pool_produces_empty_join(), AtomId, WorldComponents

### Community 110 - "TreeSubGraph"
Cohesion: 0.14
Nodes (8): BinaryLiftingTable, Condensation, ForestSubGraph, FxHashMap, Option, Self, Vec, TreeSubGraph

### Community 111 - "ClockProInner"
Cohesion: 0.14
Nodes (8): ClockProInner, MemBack, Any, HashMap, HashSet, Result, Vec, VecDeque

### Community 112 - "Specification Design Authority"
Cohesion: 0.33
Nodes (6): Specification Design Authority, AtomWalk Syntax, MATCH Graph Patterns, GrynQL EBNF Grammar, Context-Sensitive Parser Disambiguation, GrynQL Statements

### Community 113 - "Persistent MVCC v2"
Cohesion: 0.33
Nodes (6): ClosureCodec, Exact Phantom Matching, Persistent MVCC v2, Serializable Snapshot Isolation, GrynQL Transaction Model, Execution MVCC and Snapshot Contract

### Community 114 - "Unified Atom Incidence"
Cohesion: 0.33
Nodes (6): Unified Atom Incidence, Graph Storage Families, Ferros-Grynn Graph Taxonomy, Mutable Frozen Type-State and Views, P4 Full Atom Incidence, GraphWorkspace ECS Sidecar

### Community 115 - "StatusObject"
Cohesion: 0.16
Nodes (8): DiagnosticRecord, DiagnosticValue, BTreeMap, From, Into, Option, StatusObject, String

### Community 116 - "hashing/mod.rs"
Cohesion: 0.33
Nodes (3): Option, Ordering, Hashable

### Community 118 - "execution.rs"
Cohesion: 0.17
Nodes (20): binds_let_filters_projects_and_limits(), execute_program(), execute_text(), executes_return_with_pratt_precedence(), execution_phase(), gets_record_and_materializes_select_value(), initialize_slots(), keeps_none_and_null_distinct() (+12 more)

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

### Community 136 - "DiagnosticRecord"
Cohesion: 0.16
Nodes (9): DiagnosticRecord, ExecutionPhase, BTreeMap, Into, Self, String, SystemTime, Vec (+1 more)

### Community 137 - "ValueKind"
Cohesion: 0.13
Nodes (7): Display, FerrosGrynnError, H, Ordering, VectorKind, ValueKind, Vector

### Community 138 - "MaterializedGraph"
Cohesion: 0.17
Nodes (12): MaterializedGraph, preserves_exact_discriminant_in_generic_transport(), Eq, H, Hash, Option, Ord, Ordering (+4 more)

### Community 139 - "SyncIo"
Cohesion: 0.21
Nodes (9): open_read_write_sync(), Arc, File, Path, PathBuf, Result, Self, SyncIo (+1 more)

### Community 140 - "TxId"
Cohesion: 0.17
Nodes (7): BTreeMap, Display, Formatter, TxId, Version, Version<T>, VersionChain

### Community 141 - "Self"
Cohesion: 0.15
Nodes (6): ExecutionPhase, LabeledLocation, Location, Self, Status, SystemTime

### Community 142 - "runtime/mod.rs"
Cohesion: 0.23
Nodes (10): atom_and_path_have_distinct_value_contracts(), Closure, display_is_deterministic_and_escapes_strings(), eq_ord_and_hash_agree_for_cross_number_values(), graph_transport_preserves_exact_type_and_typed_conversion(), reports_concrete_kind_and_static_type(), Vec, runtime_only_values_are_explicitly_non_storable() (+2 more)

### Community 143 - "AlignedBuffer"
Cohesion: 0.17
Nodes (7): AlignedBuffer, free_aligned(), platform_read_page(), Drop, HANDLE, Send, Sync

### Community 144 - "EdgeInfo"
Cohesion: 0.24
Nodes (9): EdgeIncidence, EdgeInfo, EdgeRole, EdgeTopology, AtomId, Box, Option, PropertyMap (+1 more)

### Community 145 - "PropertyColumn"
Cohesion: 0.20
Nodes (5): DenseBitmap, PropertyColumn, PropertyKeyId, SparseSet, Vec

### Community 146 - "src/id.rs"
Cohesion: 0.22
Nodes (7): EdgeKindMarker, GraphKindMarker, NodeKindMarker, K, PhantomData, Vec, SlotAllocator

### Community 147 - "OrdFloat"
Cohesion: 0.31
Nodes (7): OrdFloat, Eq, Option, Ord, Ordering, PartialOrd, Self

### Community 148 - "QueueEntry"
Cohesion: 0.28
Nodes (6): QueueEntry, Eq, Ord, Ordering, PartialEq, PartialOrd

### Community 151 - "Condensation"
Cohesion: 0.29
Nodes (3): Condensation, CutSet, Box

### Community 152 - "write_sequence"
Cohesion: 0.36
Nodes (7): Formatter, IntoIterator, Item, T, write_debug_sequence(), write_json_string(), write_sequence()

### Community 153 - "bellman_ford"
Cohesion: 0.33
Nodes (6): BellmanFordResult, bellman_ford(), BellmanFordError, Display, Formatter, Result

### Community 155 - "LocationError"
Cohesion: 0.33
Nodes (5): LocationError, Display, Error, Formatter, Result

### Community 156 - "MetaGraphError"
Cohesion: 0.50
Nodes (4): MetaGraphError, AtomId, AtomKind, Error

### Community 157 - "BinaryOperator"
Cohesion: 0.80
Nodes (5): arithmetic(), decimal_arithmetic(), float_arithmetic(), operator_name(), BinaryOperator

## Knowledge Gaps
- **54 isolated node(s):** `RootedTree<State>`, `DiskGraphView<'tx, R>`, `Dag<State>`, `super::Hypergraph<State>`, `super::Metagraph<State>` (+49 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **91 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Type` connect `Type` to `read.rs`, `Closure`, `ir.rs`, `runtime/mod.rs`, `Graph`, `FerrosGrynnResult`, `status_codes.rs`, `AtomKind`, `Value`, `.expr`?**
  _High betweenness centrality (0.145) - this node is a cross-community bridge._
- **Why does `AtomId` connect `AtomId` to `ForestStorage`, `Dag<State>`, `Hypergraph<State>`, `Mutable`, `read.rs`, `Graph`, `Metagraph<State>`, `PropertyColumn`, `QueueEntry`, `Graph<State>`, `Subgraph<State>`, `Condensation`, `topology.rs`, `AtomKind`, `GraphWorkspace<G>`, `PropertyColumn<T>`, `graph/traversal.rs`, `Option`, `WorldComponents`, `FerrosGrynnResult`, `EdgeEndpoint`, `ecs/mod.rs`, `SparseSet<T>`, `property_column.rs`, `bfs`?**
  _High betweenness centrality (0.122) - this node is a cross-community bridge._
- **Why does `Graph` connect `Graph` to `ForestStorage`, `Dag<State>`, `Hypergraph<State>`, `bfs`, `Mutable`, `read.rs`, `MaterializedGraph`, `AtomId`, `Metagraph<State>`, `runtime/mod.rs`, `Subgraph<State>`?**
  _High betweenness centrality (0.109) - this node is a cross-community bridge._
- **What connects `RootedTree<State>`, `DiskGraphView<'tx, R>`, `Dag<State>` to the rest of the system?**
  _54 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `TextSpan` be split into smaller, more focused modules?**
  _Cohesion score 0.06243662175865566 - nodes in this community are weakly interconnected._
- **Should `ForestStorage` be split into smaller, more focused modules?**
  _Cohesion score 0.05833848690991548 - nodes in this community are weakly interconnected._
- **Should `Dag<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.0586340206185567 - nodes in this community are weakly interconnected._