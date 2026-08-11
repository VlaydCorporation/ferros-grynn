# Graph Report - ferros-grynn  (2026-08-10)

## Corpus Check
- 149 files · ~201,523 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 4651 nodes · 12401 edges · 158 communities (150 shown, 8 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 102 edges (avg confidence: 0.81)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `1f75dcb6`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- Parser
- SnapshotId
- Dag<State>
- Hypergraph<State>
- MetaGraph
- Mutable
- read.rs
- MvccTransaction
- AtomId
- GraphId
- BufferPool
- EdgeColumns
- Frozen
- Uuid
- Graph
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
- EdgeEndpoint
- AtomId
- Value
- Position
- GraphWorkspace<G>
- src/traversal.rs
- Transaction
- Result
- Text
- FerrosGrynnError
- DiagnosticRecord
- Result
- SparseSet<T>
- Duration
- SIV<T>
- status.rs
- formatting.rs
- PosixDevice
- Closure
- Result
- benches/property_map.rs
- TransactionGraphView
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
- StatusObject
- heap_estimator.rs
- Self
- Type
- Regex
- Set
- AsyncIo
- AdjacencyMatrix
- datastore.rs
- Object
- get_metadata_inner
- StorageBackend
- fg-meta/src/lib.rs
- PropertyBatch
- CowVec<T, CHUNK_LEN>
- HashValue
- WorldComponents
- Result
- ClockProInner
- DatabaseRoot
- StatusWireV1
- BinaryLiftingIndex
- PropertyDictionary
- AtomId
- Value
- TraversalIndex
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
- SyncIo
- DirectFile
- Item
- HashFunction
- encode_union
- EdgeView
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
- EdgeInfo
- Specification Design Authority
- Persistent MVCC v2
- Unified Atom Incidence
- TableSchema
- hashing/mod.rs
- XorShift32HashFunction
- QueryResult
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
- GraphStorageMode
- invalid_batch
- MaterializedGraph
- TextSpan
- TxId
- OrdFloat
- runtime/mod.rs
- parse
- ForestStorage
- .new
- AtomIds
- Iter
- MetaGraphError
- Subgraph<Mutable>
- Iter<'a, T>
- ParserSettings
- Iter<'_, T>
- .fmt
- Values
- AtomId
- Metagraph<Mutable>
- MaterializationOrigin

## God Nodes (most connected - your core abstractions)
1. `MetaGraph` - 153 edges
2. `Value` - 87 edges
3. `Graph` - 84 edges
4. `Transaction` - 55 edges
5. `Frozen` - 55 edges
6. `PropertyKeyId` - 51 edges
7. `TxId` - 50 edges
8. `Uuid` - 49 edges
9. `Lsn` - 48 edges
10. `MvccTransaction` - 44 edges

## Surprising Connections (you probably didn't know these)
- `Public Graph Surface` --semantically_similar_to--> `Graph Storage Families`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/graphs.md
- `Claude Ferros-Grynn Guidance` --semantically_similar_to--> `Prototype-to-Production Migration Architecture`  [INFERRED] [semantically similar]
  CLAUDE.md → AGENTS.md
- `Value Runtime Invariants` --semantically_similar_to--> `Distinct None and Null Semantics`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/values_plan.md
- `Arrow Validity Plus Presence Bitmap` --semantically_similar_to--> `PropertyBatch Presence Model`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/values_plan.md
- `StatusParameterDefinition` --references--> `Type`  [EXTRACTED]
  crates/ferros-grynn-macros/src/status_codes.rs → src/values/runtime/types.rs

## Import Cycles
- 1-file cycle: `src/values/runtime/graph/subgraph.rs -> src/values/runtime/graph/subgraph.rs`
- 1-file cycle: `src/values/runtime/number.rs -> src/values/runtime/number.rs`
- 1-file cycle: `src/values/runtime/set.rs -> src/values/runtime/set.rs`
- 1-file cycle: `src/values/runtime/bytes.rs -> src/values/runtime/bytes.rs`
- 1-file cycle: `src/common/memory/measurable.rs -> src/common/memory/measurable.rs`
- 1-file cycle: `src/values/runtime/text.rs -> src/values/runtime/text.rs`
- 1-file cycle: `src/values/runtime/graph/general.rs -> src/values/runtime/graph/general.rs`
- 1-file cycle: `src/values/runtime/range.rs -> src/values/runtime/range.rs`
- 1-file cycle: `src/values/runtime/regex.rs -> src/values/runtime/regex.rs`
- 1-file cycle: `src/values/runtime/vector.rs -> src/values/runtime/vector.rs`

## Hyperedges (group relationships)
- **GrynQL Contract Stack** — docs_gql_spec_grynql_language, docs_gql_grammar_grynql_grammar, docs_gql_tech_spec_query_engine [INFERRED 0.95]
- **Storage Persistence Contract** — docs_disk_storage_spec_frame_wal, docs_disk_storage_spec_checkpoint_recovery, docs_gql_tech_spec_page_manager, crates_fg_meta_implementation_notes_persistent_mvcc [INFERRED 0.85]
- **Values Runtime Storage Separation** — docs_values_plan_runtime_storage_boundary, docs_values_plan_storage_value, src_values_agents_runtime_invariants, src_values_agents_presence_bitmap [INFERRED 0.95]

## Communities (158 total, 8 thin omitted)

### Community 0 - "Parser"
Cohesion: 0.18
Nodes (11): Identifier, cover(), Symbol, Token, Parser, FerrosGrynnError, FerrosGrynnResult, Option (+3 more)

### Community 1 - "SnapshotId"
Cohesion: 0.08
Nodes (12): AtomIds<'a>, DiskGraphView, DiskGraphView<'tx, R>, GraphKind, GraphReadView, GraphReadView<'tx, R>, Display, Formatter (+4 more)

### Community 2 - "Dag<State>"
Cohesion: 0.06
Nodes (47): build_oriented_tree_storage(), build_storage(), constraint(), Dag, Dag<Frozen>, Dag<Mutable>, Dag<State>, DagStorage (+39 more)

### Community 3 - "Hypergraph<State>"
Cohesion: 0.06
Nodes (41): build_storage(), constraint(), graph_conversion_rejects_edge_endpoints(), Hypergraph<Frozen>, Hypergraph<Mutable>, Hypergraph<State>, HyperStorage, permits_a_vertex_in_both_directed_incidence_roles() (+33 more)

### Community 4 - "MetaGraph"
Cohesion: 0.09
Nodes (14): MetaGraph, AtomId, AtomKind, Box, Default, EdgeTopology, Option, PropertyMap (+6 more)

### Community 5 - "Mutable"
Cohesion: 0.06
Nodes (57): ApplyUpdate, MutexGuard, GraphWriteView<'a>, accept_remote_discards_local_conflicting_state(), apply_or_pause(), conflicted_live_graph(), conflicting_changes(), discard_local_applies_the_pending_remote_delta() (+49 more)

### Community 6 - "read.rs"
Cohesion: 0.09
Nodes (21): assert_snapshot_released(), cascade_removal_marks_removed_incident_edges_dirty(), disk_view_and_hybrid_materialization_have_the_same_read_result(), DiskGraphView<'_, R>, FakeTransaction, GraphMut, GraphRead, GraphReadView<'_, R> (+13 more)

### Community 7 - "MvccTransaction"
Cohesion: 0.06
Nodes (52): AtomMeta, basic_generic_rw(), BytesCodec, closure_codec_with_capture(), ClosureCodec, ClosureCodec<T>, Codec, exact_phantom_label() (+44 more)

### Community 8 - "AtomId"
Cohesion: 0.07
Nodes (32): bfs_spanning_tree(), BinaryLiftingTable, Condensation, DagSubGraph, extract_dag(), extract_forest(), extract_hyper(), extract_tree() (+24 more)

### Community 9 - "GraphId"
Cohesion: 0.05
Nodes (23): AtomId, AtomKind, EdgeKindMarker, GraphId, GraphKindMarker, kind_bit_encoding(), NodeKindMarker, Debug (+15 more)

### Community 10 - "BufferPool"
Cohesion: 0.15
Nodes (14): backend(), BufferPool, clock_pro_scan_resistance(), dirty_flushed_on_eviction(), eviction_produces_ghost(), Frame, FrameStatus, ghost_hit_promotes_to_hot() (+6 more)

### Community 11 - "EdgeColumns"
Cohesion: 0.09
Nodes (20): CowVec, EdgeColumns, empty_topology(), ensure_unique_record(), GraphMetadata, NodeColumns, AtomId, BTreeSet (+12 more)

### Community 12 - "Frozen"
Cohesion: 0.17
Nodes (12): Graph<Mutable>, merge_transaction_atom(), merge_transaction_graph(), EdgeTopology, FerrosGrynnResult, Number, Option, PropertyMap (+4 more)

### Community 13 - "Uuid"
Cohesion: 0.06
Nodes (36): JsonValue, InnerUlid, InnerUuid, Display, Err, Formatter, From, FromStr (+28 more)

### Community 14 - "Graph"
Cohesion: 0.07
Nodes (21): Graph, PhantomData, State, Metagraph<State>, Debug, EdgeIds, Eq, Error (+13 more)

### Community 15 - "Path"
Cohesion: 0.07
Nodes (36): EndpointRole, Path, PathDirection, PathStep, PathTraceArena, PathTraceId, PathTraceNode, resolve_atom() (+28 more)

### Community 16 - "Result"
Cohesion: 0.17
Nodes (15): PageId, atom_id_from(), BytesPersistentStore, checkpoint_then_recover(), CheckpointHeader, crc32(), encode_entry(), PersistentMvccStore<T, C> (+7 more)

### Community 17 - "win_async.rs"
Cohesion: 0.06
Nodes (45): AtomicI64, cpu_affinity_cpu0(), goto_shutdown(), ioring_handle_ref(), ioring_raw_buffer_ref(), ioring_registered_buffer_ref(), new_fails_gracefully_if_no_ioring(), PendOp (+37 more)

### Community 18 - "Number"
Cohesion: 0.08
Nodes (29): BigInt, canonical_float(), canonical_fraction(), canonicalizes_nan_and_signed_zero_for_eq_ord_and_hash(), CanonicalNumber, conversion_error(), Decimal, f32 (+21 more)

### Community 19 - "src/ecs.rs"
Cohesion: 0.07
Nodes (34): AnyPool, ColorTag, FeatureVector, node_id(), NodeWeight, PageRank, Position2D, Position3D (+26 more)

### Community 20 - "execution.rs"
Cohesion: 0.05
Nodes (89): CheckedAdd, CheckedMul, CheckedSub, PrimInt, Signed, QueryCatalog, arithmetic(), binary() (+81 more)

### Community 21 - "Graph<State>"
Cohesion: 0.07
Nodes (29): copy_from_read(), freeze_thaw_preserves_identity_ids_and_topology(), Graph<Frozen>, Graph<State>, invalid_edges_are_transactional_and_use_graph_statuses(), remap_topology(), removal_cascades_through_edge_endpoints_and_reuses_generation(), BTreeMap (+21 more)

### Community 22 - "Subgraph<State>"
Cohesion: 0.11
Nodes (14): Debug, EdgeIds, Eq, Formatter, H, Hash, NodeIds, Option (+6 more)

### Community 23 - "store.rs"
Cohesion: 0.10
Nodes (31): auto_checkpoint_triggers(), checkpoint_clears_wal(), compress_payload(), crc32(), crc32_update(), create_and_reopen(), decompress_payload(), delete_graph() (+23 more)

### Community 24 - "status_codes.rs"
Cohesion: 0.09
Nodes (29): define_status_codes(), is_string(), is_vec_string(), last_path_ident(), option_lit(), Ident, Option, Parse (+21 more)

### Community 25 - "PropertyKeyId"
Cohesion: 0.07
Nodes (29): clone_is_independent(), empty_map_is_pointer_sized_and_does_not_reserve_storage(), entry_ref(), equality_order_and_hash_ignore_insertion_order(), from_iterator_uses_last_duplicate_value(), keeps_entries_sorted_and_replaces_existing_values(), PropertyKeyId, PropertyMap (+21 more)

### Community 26 - "EdgeEndpoint"
Cohesion: 0.07
Nodes (27): INLINE_ENDPOINTS, distinguishes_binary_hyper_and_cross_level_topology(), EdgeDirection, EdgeEndpoint, EdgeTopology, QualifiedAtomId, AtomId, Box (+19 more)

### Community 27 - "AtomId"
Cohesion: 0.12
Nodes (14): allocate_memory(), Result, atom_id_uses_u40_slots_and_u23_generations(), AtomId, AtomKind, Debug, Display, FerrosGrynnResult (+6 more)

### Community 28 - "Value"
Cohesion: 0.09
Nodes (16): Closure, AtomRef, Box, Eq, From, Hash, MaterializedGraph, Number (+8 more)

### Community 29 - "Position"
Cohesion: 0.07
Nodes (35): GeoJsonGeometry, GeoJsonPosition, assert_status(), compare_float_slices(), GeoJsonValue, Geometry, into_position(), into_positions() (+27 more)

### Community 30 - "GraphWorkspace<G>"
Cohesion: 0.12
Nodes (20): canonical_mutation_invalidates_derived_and_removes_dead_components(), component_mutation_does_not_change_graph_revision(), G, GraphWorkspace, GraphWorkspace<G>, map_graph_moves_user_components_but_discards_derived_pools(), PageRank, Position (+12 more)

### Community 31 - "src/traversal.rs"
Cohesion: 0.12
Nodes (37): BellmanFordResult, bellman_ford(), BellmanFordError, bfs(), bfs_with(), dfs(), dfs_full(), dijkstra() (+29 more)

### Community 32 - "Transaction"
Cohesion: 0.19
Nodes (7): Instant, Drop, FerrosGrynnError, FerrosGrynnResult, View, Transaction, GraphRef

### Community 33 - "Result"
Cohesion: 0.09
Nodes (27): AtomRef, bool, Closure, Graph<Frozen>, MaterializedGraph, Number, Path, RecordId (+19 more)

### Community 34 - "Text"
Cohesion: 0.08
Nodes (24): AsRef, Borrow, Extend, CompactString, exposes_controlled_utf8_mutation(), round_trips_through_std_and_compact_strings(), Deref, Display (+16 more)

### Community 35 - "FerrosGrynnError"
Cohesion: 0.09
Nodes (24): ErrorState, FerrosGrynnError, NonErrorStatus, Arc, Debug, Display, E, Error (+16 more)

### Community 36 - "DiagnosticRecord"
Cohesion: 0.09
Nodes (22): NonZeroU32, computes_unicode_locations_without_retaining_source(), DiagnosticRecord, ExecutionPhase, LabeledLocation, line_column_at(), LineColumn, Location (+14 more)

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
Nodes (15): Id, Handle, Handle<'a, T>, Metadata, Default, Deref, Index, IndexMut (+7 more)

### Community 41 - "status.rs"
Cohesion: 0.11
Nodes (13): Severity, Condition, Domain, ErrorCategory, ErrorClassification, NotificationClassification, Option, String (+5 more)

### Community 42 - "formatting.rs"
Cohesion: 0.11
Nodes (27): And, Callable, Comma, DisplayValue, Formatted, Formatted<'a, F, T>, Formatted<'_, F, T>, Identifier (+19 more)

### Community 43 - "PosixDevice"
Cohesion: 0.09
Nodes (18): NvmeDevice, NvmeStats, open_best(), open_best_posix(), posix_device_rw(), PosixDevice, Any, Arc (+10 more)

### Community 44 - "Closure"
Cohesion: 0.07
Nodes (27): ClosureFunction, NonZeroU64, calls_function_and_keeps_process_identity(), Closure, different_closures_have_different_identity(), rejects_wrong_arity_before_invocation(), Arc, Debug (+19 more)

### Community 45 - "Result"
Cohesion: 0.17
Nodes (15): AlignedBuffer, alloc_aligned(), free_aligned(), platform_grow(), platform_open(), platform_open_readonly(), platform_read_page(), platform_write_page() (+7 more)

### Community 46 - "benches/property_map.rs"
Cohesion: 0.12
Nodes (32): bench_build(), bench_iterate_and_clone(), bench_lookup(), boxed_enum_map(), BoxedEnumIter, BoxedEnumPropertyMap, BoxedEnumRepr, btree_map() (+24 more)

### Community 47 - "TransactionGraphView"
Cohesion: 0.15
Nodes (17): all_dirty(), GraphWrite, materialize_atom(), materialize_rows(), merge_materialized(), QueryStorageRow, row_from_properties(), AtomId (+9 more)

### Community 48 - "async_io.rs"
Cohesion: 0.09
Nodes (33): BufRingEntry, AsyncIoEngine, AsyncUringEngine, BufRing, build(), build_ring(), bump_memlock(), create_engine() (+25 more)

### Community 49 - "arrow.rs"
Cohesion: 0.14
Nodes (18): ArrowError, DataType, arrow_failure(), arrow_round_trip_preserves_none_null_and_native_scalars(), decode_rows(), empty_and_explicit_none_rows_keep_row_count_without_columns(), incompatible_scalar_kinds_use_dense_union_without_coercion(), property_field() (+10 more)

### Community 50 - "StatusReport"
Cohesion: 0.11
Nodes (20): applies_gql_precedence(), Outcome, Outcome<T>, precedence(), rejects_errors(), Display, Error, ExactSizeIterator (+12 more)

### Community 51 - "Array"
Cohesion: 0.10
Nodes (16): &'a Array, Array, From, FromIterator, I, Into, IntoIter, IntoIterator (+8 more)

### Community 52 - "Vector"
Cohesion: 0.10
Nodes (20): canonical_f32(), canonical_f64(), compare_f32(), compare_f32_value(), compare_f64(), compare_f64_value(), hash_f32(), hash_f64() (+12 more)

### Community 53 - "PropertyColumn<T>"
Cohesion: 0.10
Nodes (19): AnyPool, atom(), DenseBitmap, distinguishes_none_null_and_value(), PropertyColumn<T>, PropertyState, removing_null_reports_null_instead_of_none(), replacement_removal_and_generation_keep_bitmaps_aligned() (+11 more)

### Community 54 - "Range"
Cohesion: 0.15
Nodes (24): Bound, borrow_bound(), Bound<T>, bound_value(), BoundExt, box_bound(), checks_inclusion(), compare_end() (+16 more)

### Community 55 - "Lsn"
Cohesion: 0.14
Nodes (15): BufReader, decode_entry(), decode_isolation(), encode_isolation(), entry_crc(), Lsn, MultiSegmentReader, Display (+7 more)

### Community 56 - ".new"
Cohesion: 0.12
Nodes (14): BufWriter, PersistLayer, Arc, AtomicU64, File, Mutex, Path, PathBuf (+6 more)

### Community 57 - "graph/traversal.rs"
Cohesion: 0.19
Nodes (31): algorithm_failure(), bellman_ford(), bellman_ford_reports_negative_cycle_as_domain_error(), bfs(), bfs_topology_and_components_are_correct(), binary_edges(), BinaryEdge, collect_weights() (+23 more)

### Community 58 - "StatusObject"
Cohesion: 0.09
Nodes (20): Deserialize, Ok, Serialize, InvalidSeverity, D, Display, Err, Error (+12 more)

### Community 59 - "heap_estimator.rs"
Cohesion: 0.08
Nodes (21): HeapEstimatorCache, NoHeapEstimatorCache, Self, Measurable, HashMap, K, V, size_of_hash_map() (+13 more)

### Community 60 - "Self"
Cohesion: 0.14
Nodes (14): DagView, ForestView, GraphView, Dag<State>, DagView<'graph, R>, directed_forest_view(), Forest<State>, RootedTree<State> (+6 more)

### Community 61 - "Type"
Cohesion: 0.09
Nodes (27): is_option(), analyze(), Analyzer, Analyzer<'a>, compatible(), is_integer(), is_number(), Box (+19 more)

### Community 62 - "Regex"
Cohesion: 0.09
Nodes (19): compiles_and_matches_pattern(), Regex, Display, Eq, Err, Error, Formatter, FromStr (+11 more)

### Community 63 - "Set"
Cohesion: 0.12
Nodes (15): &'a Set, BTreeSet<Value>, deduplicates_and_orders_values(), BTreeSet, From, FromIterator, I, Into (+7 more)

### Community 64 - "AsyncIo"
Cohesion: 0.15
Nodes (16): async_io_batch_tag_routing(), async_io_write_read_sync(), AsyncIo, AsyncIoConfig, EngineConfig, Arc, Box, Default (+8 more)

### Community 65 - "AdjacencyMatrix"
Cohesion: 0.15
Nodes (13): AdjacencyMatrix, Incidence, IncidenceMatrix, IncidenceTensor, MetaAdjacencyMatrix, MetaTriple, AtomId, FxHashMap (+5 more)

### Community 66 - "datastore.rs"
Cohesion: 0.19
Nodes (17): CommitResult, configured_transaction_limits_have_domain_statuses(), disjoint_atom_writes_merge(), graph_id(), IsolationLevel, predicate_validation_ignores_non_matching_changes_but_catches_phantoms(), predicate_validation_includes_own_uncommitted_writes(), read_only_snapshot_is_safe_from_serialization_aborts() (+9 more)

### Community 67 - "Object"
Cohesion: 0.07
Nodes (33): batch_random_access(), bench_collect(), bench_iter_sequential_borrowed(), bench_iter_sequential_owned(), bench_iter_sequential_with_step(), bench_push_preallocated(), bench_push_raw(), LargeData (+25 more)

### Community 68 - "get_metadata_inner"
Cohesion: 0.10
Nodes (23): add_m, Attrs, measurable_inner(), occurrence_error(), add, DeriveInput, Error, Parse (+15 more)

### Community 69 - "StorageBackend"
Cohesion: 0.14
Nodes (17): AsyncIoRingEngine, IoRingEngine, Arc, Default, ThreadPoolEngine, extract_fd(), extract_handle(), extract_path() (+9 more)

### Community 70 - "fg-meta/src/lib.rs"
Cohesion: 0.15
Nodes (7): SmallVec, NodeInfo, NodeKind, AtomId, Box, Option, PropertyMap

### Community 71 - "PropertyBatch"
Cohesion: 0.20
Nodes (11): BooleanBuffer, imported_arrow_batch_requires_property_metadata(), PropertyBatch, BTreeMap, FerrosGrynnResult, Option, PropertyMap, RecordBatch (+3 more)

### Community 72 - "CowVec<T, CHUNK_LEN>"
Cohesion: 0.15
Nodes (11): CowVec<T, CHUNK_LEN>, Default, FnMut, From, Index, IndexMut, Option, Output (+3 more)

### Community 73 - "HashValue"
Cohesion: 0.12
Nodes (14): AddAssign, ConstOne, Mul, MulAssign, One, HashValue, Add, Display (+6 more)

### Community 74 - "WorldComponents"
Cohesion: 0.10
Nodes (21): AnyPool, atom(), remove_atom_cleans_every_pool_and_respects_generation(), Any, AtomId, Box, FerrosGrynnResult, FnMut (+13 more)

### Community 75 - "Result"
Cohesion: 0.13
Nodes (17): BatchEntry, bg_loop(), blocking_op(), IoBatch, IoCompletion, IoOp, IoRequest, Mem (+9 more)

### Community 76 - "ClockProInner"
Cohesion: 0.13
Nodes (9): ClockProInner, MemBack, Any, HashMap, HashSet, Mutex, Result, Vec (+1 more)

### Community 77 - "DatabaseRoot"
Cohesion: 0.16
Nodes (15): DatabaseRoot, Datastore, InMemoryOptions, PredicateEvaluator, PredicateMatcher, PredicateRead, ReadKey, Arc (+7 more)

### Community 78 - "StatusWireV1"
Cohesion: 0.20
Nodes (18): completion_omits_severity_and_classification(), DiagnosticRecordWireV1, error_classification(), FerrosGrynnError, labeled_location_to_wire(), LabeledLocationWireV1, location_to_wire(), LocationWireV1 (+10 more)

### Community 79 - "BinaryLiftingIndex"
Cohesion: 0.18
Nodes (10): BinaryLiftingIndex, QueueEntry, Eq, FnMut, Ord, Ordering, PartialEq, PartialOrd (+2 more)

### Community 80 - "PropertyDictionary"
Cohesion: 0.16
Nodes (12): assigns_dense_ids_and_resolves_both_directions(), interning_is_idempotent_and_iteration_uses_id_order(), PropertyDictionary, ExactSizeIterator, FerrosGrynnResult, Into, Item, Option (+4 more)

### Community 81 - "AtomId"
Cohesion: 0.15
Nodes (12): Condensation, CutFrame, CutSet, Predecessor, AtomId, Box, Option, Path (+4 more)

### Community 82 - "Value"
Cohesion: 0.17
Nodes (8): Box, From, FxHashMap, Option, Self, String, Vec, Value

### Community 83 - "TraversalIndex"
Cohesion: 0.26
Nodes (11): deduplicated_steps(), dfs(), local_steps(), DoubleEndedIterator, Item, Vec, topological_sort_with_index(), TraversalDirection (+3 more)

### Community 84 - "AccumList"
Cohesion: 0.15
Nodes (15): AccumList, FieldAccumArgs, Attribute, DeriveInput, Ident, Option, Parse, ParseStream (+7 more)

### Community 85 - "DiagnosticValue"
Cohesion: 0.14
Nodes (11): bool, DiagnosticValue, f32, f64, BTreeMap, String, StatusParameter, str (+3 more)

### Community 86 - "Bytes"
Cohesion: 0.21
Nodes (7): Bytes, exposes_only_byte_oriented_api(), From, Into, Self, Vec, Vec<u8>

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
Nodes (9): PropertyIter, PropertyIter<'a>, DoubleEndedIterator, Entry, ExactSizeIterator, FusedIterator, Item, Iterator (+1 more)

### Community 91 - "Graphify"
Cohesion: 0.15
Nodes (13): Add URL and Watch Folder, Graphify Extra Exports, Semantic Extraction Contract, GitHub Clone and Cross-Repo Merge, Graphify Commit and Claude Hooks, Graph Query Path and Explain, Video and Audio Transcription, Incremental Graph Update (+5 more)

### Community 92 - "PropertyMap"
Cohesion: 0.23
Nodes (7): bench_search(), Property, PropertyMap, Criterion, Self, String, Vec

### Community 93 - "wy_hash.rs"
Cohesion: 0.29
Nodes (7): i64_le(), u32(), u64_rotate32(), wy_hash64(), WyHash, wymum(), wyr3()

### Community 94 - "SyncIo"
Cohesion: 0.19
Nodes (9): open_read_write_sync(), Arc, File, Path, PathBuf, Result, Self, SyncIo (+1 more)

### Community 95 - "DirectFile"
Cohesion: 0.15
Nodes (7): DirectFile, Any, AtomicU64, HANDLE, Option, PathBuf, RawFd

### Community 97 - "HashFunction"
Cohesion: 0.21
Nodes (3): IncrementalXXH64, HashFunction, UtilHash

### Community 98 - "encode_union"
Cohesion: 0.45
Nodes (9): ArrayRef, build_array(), ColumnKind, encode_column(), encode_union(), BTreeSet, FerrosGrynnResult, Option (+1 more)

### Community 99 - "EdgeView"
Cohesion: 0.11
Nodes (14): EdgeView, EdgeView<'a>, NodeView, NodeView<'a>, AtomId, EdgeTopology, Number, Option (+6 more)

### Community 100 - "graph_core.rs"
Cohesion: 0.45
Nodes (10): benchmark_access_modes(), benchmark_path_and_live_publication(), benchmark_state_and_family_conversions(), benchmark_traversal(), dag_chain(), general_chain(), path(), AtomId (+2 more)

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
Cohesion: 0.25
Nodes (14): decodes_quoted_names_parameters_and_string_escapes(), is_ident_continue(), is_ident_start(), keeps_utf8_byte_spans_and_greedy_operators(), lex(), Lexer, parse_error(), Box (+6 more)

### Community 111 - "EdgeInfo"
Cohesion: 0.24
Nodes (9): EdgeIncidence, EdgeInfo, EdgeRole, EdgeTopology, AtomId, Box, Option, PropertyMap (+1 more)

### Community 112 - "Specification Design Authority"
Cohesion: 0.33
Nodes (6): Specification Design Authority, AtomWalk Syntax, MATCH Graph Patterns, GrynQL EBNF Grammar, Context-Sensitive Parser Disambiguation, GrynQL Statements

### Community 113 - "Persistent MVCC v2"
Cohesion: 0.33
Nodes (6): ClosureCodec, Exact Phantom Matching, Persistent MVCC v2, Serializable Snapshot Isolation, GrynQL Transaction Model, Execution MVCC and Snapshot Contract

### Community 114 - "Unified Atom Incidence"
Cohesion: 0.33
Nodes (6): Unified Atom Incidence, Graph Storage Families, Ferros-Grynn Graph Taxonomy, Mutable Frozen Type-State and Views, P4 Full Atom Incidence, GraphWorkspace ECS Sidecar

### Community 115 - "TableSchema"
Cohesion: 0.24
Nodes (7): ExactSizeIterator, Into, Item, Text, TableField, TableKind, TableSchema

### Community 116 - "hashing/mod.rs"
Cohesion: 0.33
Nodes (3): Option, Ordering, Hashable

### Community 118 - "QueryResult"
Cohesion: 0.33
Nodes (6): Vec, parse(), QueryResult, QueryStatementKind, FerrosGrynnResult, Value

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

### Community 137 - "invalid_batch"
Cohesion: 0.31
Nodes (9): decode_value(), downcast(), fixed_16(), Field, T, invalid_batch(), FerrosGrynnError, Into (+1 more)

### Community 138 - "MaterializedGraph"
Cohesion: 0.10
Nodes (16): MaterializedGraph, preserves_exact_discriminant_in_generic_transport(), AtomId, EdgeIds, Eq, FerrosGrynnResult, H, Hash (+8 more)

### Community 139 - "TextSpan"
Cohesion: 0.30
Nodes (20): TextSpan, Expr, ExprKind, Identifier, LetStatement, Literal, ObjectField, Program (+12 more)

### Community 140 - "TxId"
Cohesion: 0.13
Nodes (25): closure_codec_with_capture_in_persistent_store(), encode_atom_meta(), multi_segment_reader(), PageLsnTable, prune_before_removes_old_segments(), read_snapshot_streaming(), recover(), recovery_restores_atom_meta() (+17 more)

### Community 141 - "OrdFloat"
Cohesion: 0.36
Nodes (6): OrdFloat, Eq, Ord, Ordering, PartialOrd, Self

### Community 142 - "runtime/mod.rs"
Cohesion: 0.16
Nodes (10): atom_and_path_have_distinct_value_contracts(), display_is_deterministic_and_escapes_strings(), eq_ord_and_hash_agree_for_cross_number_values(), graph_transport_preserves_exact_type_and_typed_conversion(), reports_concrete_kind_and_static_type(), String, Vec, runtime_only_values_are_explicitly_non_storable() (+2 more)

### Community 143 - "parse"
Cohesion: 0.39
Nodes (7): distinguishes_spaced_record_separator(), enforces_query_depth(), parse(), reports_unsupported_statement_and_unclosed_delimiter(), respects_precedence_and_record_adjacency(), Vec, TokenBuffer

### Community 144 - "ForestStorage"
Cohesion: 0.06
Nodes (57): GraphWriteView, MutableGraphValue, build_parent_map(), build_storage(), constraint(), find_root(), Forest, Forest<Frozen> (+49 more)

### Community 146 - "AtomIds"
Cohesion: 0.29
Nodes (5): AtomIds, ExactSizeIterator, FusedIterator, Item, Iterator

### Community 147 - "Iter"
Cohesion: 0.40
Nodes (3): clone_copies_only_the_changed_chunk(), Iter, Arc

### Community 148 - "MetaGraphError"
Cohesion: 0.50
Nodes (4): MetaGraphError, AtomId, AtomKind, Error

### Community 149 - "Subgraph<Mutable>"
Cohesion: 0.18
Nodes (9): AtomId, Clone, Default, FerrosGrynnResult, PropertyMap, R, Self, Subgraph<Frozen> (+1 more)

### Community 150 - "Iter<'a, T>"
Cohesion: 0.40
Nodes (3): Iter<'a, T>, Item, Iterator

### Community 151 - "ParserSettings"
Cohesion: 0.19
Nodes (9): Config, Default, Self, parse(), ParserSettings, Default, FerrosGrynnResult, From (+1 more)

### Community 152 - "Iter<'_, T>"
Cohesion: 0.67
Nodes (3): Iter<'_, T>, ExactSizeIterator, FusedIterator

### Community 156 - "Values"
Cohesion: 0.29
Nodes (4): marks_materialized_result_without_new_storage_family(), super::Metagraph<State>, widening_views_do_not_rebuild_or_change_identity(), Values

### Community 184 - "AtomId"
Cohesion: 0.12
Nodes (15): Copied, INLINE_INCIDENCE, atom_does_not_exist(), atom_kind_mismatch(), endpoint_kind(), GeneralStorage, Arc, AtomId (+7 more)

### Community 205 - "Metagraph<Mutable>"
Cohesion: 0.12
Nodes (17): exposes_explicit_meta_atom_api_over_general_layout(), Metagraph<Frozen>, Metagraph<Mutable>, AtomId, Clone, Default, EdgeTopology, FerrosGrynnResult (+9 more)

### Community 210 - "MaterializationOrigin"
Cohesion: 0.19
Nodes (4): AtomRef, MaterializationOrigin, AtomKind, BTreeMap

## Knowledge Gaps
- **54 isolated node(s):** `Position2D`, `Position3D`, `Timestamp`, `ColorTag`, `Velocity` (+49 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **8 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Graph` connect `Graph` to `Dag<State>`, `Hypergraph<State>`, `graph_core.rs`, `Mutable`, `EdgeView`, `read.rs`, `MaterializedGraph`, `EdgeColumns`, `Frozen`, `Metagraph<Mutable>`, `runtime/mod.rs`, `ForestStorage`, `Graph<State>`, `AtomId`, `Value`?**
  _High betweenness centrality (0.153) - this node is a cross-community bridge._
- **Why does `Type` connect `Type` to `SnapshotId`, `Closure`, `runtime/mod.rs`, `TableSchema`, `execution.rs`, `Vector`, `status_codes.rs`?**
  _High betweenness centrality (0.117) - this node is a cross-community bridge._
- **Why does `PropertyKeyId` connect `PropertyKeyId` to `encode_union`, `PropertyBatch`, `WorldComponents`, `benches/property_map.rs`, `PropertyDictionary`, `arrow.rs`, `PropertyColumn<T>`, `.register_datafusion_table`?**
  _High betweenness centrality (0.087) - this node is a cross-community bridge._
- **What connects `Position2D`, `Position3D`, `Timestamp` to the rest of the system?**
  _54 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `SnapshotId` be split into smaller, more focused modules?**
  _Cohesion score 0.08067226890756303 - nodes in this community are weakly interconnected._
- **Should `Dag<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.05876010781671159 - nodes in this community are weakly interconnected._
- **Should `Hypergraph<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.06228281208233093 - nodes in this community are weakly interconnected._