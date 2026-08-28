# Graph Report - ferros-grynn  (2026-08-24)

## Corpus Check
- 158 files · ~231,272 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 5255 nodes · 14566 edges · 156 communities (147 shown, 9 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 110 edges (avg confidence: 0.81)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `a234ae67`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- TextSpan
- ForestStorage
- Dag<State>
- Hypergraph<State>
- MetaGraph
- RecordIdPart
- Error
- MvccTransaction
- AtomId
- GraphId
- PageId
- EdgeColumns
- Graph<State>
- Uuid
- FerrosGrynnResult
- Path
- TxId
- win_async.rs
- Number
- src/ecs.rs
- ir.rs
- StatusObject
- Subgraph<State>
- store.rs
- status_codes.rs
- PropertyKeyId
- EdgeTopology
- AtomId
- Value
- Geometry
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
- PropertyColumn<T>
- formatting.rs
- PosixDevice
- Closure
- Result
- benches/property_map.rs
- async_io.rs
- PageNo
- arrow.rs
- StatusReport
- Array
- Vector
- storage/buffer_pool.rs
- Range
- StatusCode
- storage/direct.rs
- graph/traversal.rs
- benchmarks.rs
- heap_estimator.rs
- Lsn
- Type
- Regex
- Set
- AsyncIo
- AdjacencyMatrix
- UringState
- Object
- get_metadata_inner
- StorageBackend
- AtomId
- PropertyBatch
- Result
- HashValue
- WorldComponents
- .new
- Self
- Value
- StatusWireV1
- BinaryLiftingIndex
- PropertyDictionary
- Subgraph<Mutable>
- DiagnosticValue
- EdgeEndpoint
- AccumList
- runtime/mod.rs
- Bytes
- bench_graph.rs
- os.rs
- .register_datafusion_table
- PropertyIter
- Graphify
- PropertyMap
- wy_hash.rs
- Option
- Values
- Item
- HashFunction
- Mutable
- NoHeapEstimatorCache
- graph_core.rs
- build_from_map
- memohash.rs
- Ferros-Grynn Repository Guidance
- Physical Query Execution
- Values Module Guidance
- GrynQL Language Specification
- atoms
- helpers.rs
- T
- AtomId
- fg-meta/src/lib.rs
- Specification Design Authority
- Persistent MVCC v2
- Unified Atom Incidence
- .set_null
- hashing/mod.rs
- XorShift32HashFunction
- Отчёт этапа 2: Sync Direct I/O и BufferPool
- laws.rs
- ferros-grynn-macros/src/lib.rs
- Hybrid Query Model
- struct_size.rs
- Canonical Storage Value Codec
- P3 Query Execution
- exact_ops.rs
- &'a Bytes
- .fmt
- Direct I/O and Clock-Pro
- Redacted External Error Sources
- FerrosGrynnResult
- execution.rs
- read_pool_extent
- MaterializedGraph
- FerrosGrynnResult
- .new
- DirtyTarget
- conversion_error
- .fmt
- DenseBitmap
- AtomId
- .next
- StorageValue
- Iter<'_, T>
- Q: What is MetaGraph?
- Handle<'_, T>
- GraphKind
- Graph
- Metagraph<State>
- SnapshotId

## God Nodes (most connected - your core abstractions)
1. `MetaGraph` - 153 edges
2. `Value` - 87 edges
3. `Graph` - 79 edges
4. `Uuid` - 60 edges
5. `Transaction` - 56 edges
6. `Frozen` - 55 edges
7. `PropertyKeyId` - 55 edges
8. `TxId` - 50 edges
9. `Lsn` - 48 edges
10. `PageNo` - 48 edges

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
- 1-file cycle: `src/common/memory/measurable.rs -> src/common/memory/measurable.rs`
- 1-file cycle: `src/values/runtime/text.rs -> src/values/runtime/text.rs`
- 1-file cycle: `src/values/runtime/graph/general.rs -> src/values/runtime/graph/general.rs`
- 1-file cycle: `src/values/runtime/regex.rs -> src/values/runtime/regex.rs`

## Hyperedges (group relationships)
- **GrynQL Contract Stack** — docs_gql_spec_grynql_language, docs_gql_grammar_grynql_grammar, docs_gql_tech_spec_query_engine [INFERRED 0.95]
- **Storage Persistence Contract** — docs_disk_storage_spec_frame_wal, docs_disk_storage_spec_checkpoint_recovery, docs_gql_tech_spec_page_manager, crates_fg_meta_implementation_notes_persistent_mvcc [INFERRED 0.85]
- **Values Runtime Storage Separation** — docs_values_plan_runtime_storage_boundary, docs_values_plan_storage_value, src_values_agents_runtime_invariants, src_values_agents_presence_bitmap [INFERRED 0.95]

## Communities (156 total, 9 thin omitted)

### Community 0 - "TextSpan"
Cohesion: 0.06
Nodes (61): Identifier, Config, Default, Self, TextSpan, cover(), Expr, ExprKind (+53 more)

### Community 1 - "ForestStorage"
Cohesion: 0.08
Nodes (37): build_parent_map(), build_storage(), constraint(), find_root(), Forest<Mutable>, forest_node_removal_marks_every_incident_edge_dirty(), forest_rejects_cycles_without_mutation(), ForestStorage (+29 more)

### Community 2 - "Dag<State>"
Cohesion: 0.06
Nodes (46): build_oriented_tree_storage(), build_storage(), constraint(), Dag<Frozen>, Dag<Mutable>, Dag<State>, DagStorage, frozen_layout_round_trips_to_mutable_adjacency() (+38 more)

### Community 3 - "Hypergraph<State>"
Cohesion: 0.06
Nodes (43): build_storage(), constraint(), graph_conversion_rejects_edge_endpoints(), Hypergraph<Frozen>, Hypergraph<Mutable>, Hypergraph<State>, HyperStorage, permits_a_vertex_in_both_directed_incidence_roles() (+35 more)

### Community 4 - "MetaGraph"
Cohesion: 0.08
Nodes (16): MetaGraph, AtomId, AtomKind, Box, Default, EdgeTopology, Option, PropertyMap (+8 more)

### Community 5 - "RecordIdPart"
Cohesion: 0.12
Nodes (23): JsonValue, assert_status(), enforces_the_composite_nesting_limit(), quotes_ambiguous_string_ids_for_round_trip(), RecordId, RecordIdPart, recursively_rejects_unsupported_parts(), rejects_empty_components() (+15 more)

### Community 6 - "Error"
Cohesion: 0.17
Nodes (27): Mmap, MmapMut, aligned_pages(), benchmark_page(), BenchmarkConfig, BenchPath, calibrate_batch_size(), consume_mmap() (+19 more)

### Community 7 - "MvccTransaction"
Cohesion: 0.06
Nodes (50): AtomMeta, basic_generic_rw(), BytesCodec, closure_codec_with_capture(), ClosureCodec, ClosureCodec<T>, Codec, exact_phantom_label() (+42 more)

### Community 8 - "AtomId"
Cohesion: 0.07
Nodes (32): bfs_spanning_tree(), BinaryLiftingTable, Condensation, DagSubGraph, extract_dag(), extract_forest(), extract_hyper(), extract_tree() (+24 more)

### Community 9 - "GraphId"
Cohesion: 0.05
Nodes (27): MetaGraphError, AtomId, AtomKind, Error, AtomId, AtomKind, EdgeKindMarker, GraphId (+19 more)

### Community 10 - "PageId"
Cohesion: 0.09
Nodes (24): backend(), BufferPool, clock_pro_scan_resistance(), ClockProInner, dirty_flushed_on_eviction(), eviction_produces_ghost(), Frame, FrameStatus (+16 more)

### Community 11 - "EdgeColumns"
Cohesion: 0.05
Nodes (36): clone_copies_only_the_changed_chunk(), CowVec, CowVec<T, CHUNK_LEN>, Iter, Iter<'a, T>, Arc, Default, FnMut (+28 more)

### Community 12 - "Graph<State>"
Cohesion: 0.06
Nodes (17): Copied, Graph<State>, Debug, EdgeIds, Eq, Formatter, H, Hash (+9 more)

### Community 13 - "Uuid"
Cohesion: 0.12
Nodes (13): InnerUlid, InnerUuid, Display, Err, Formatter, From, FromStr, Result (+5 more)

### Community 14 - "FerrosGrynnResult"
Cohesion: 0.08
Nodes (53): AlignedVec, Position, all_format_v1_tags_round_trip(), archive_envelope_checks_magic_version_schema_length_and_checksum(), ArchiveEnvelope, assert_status(), canonical_f32(), canonical_float_encoding_rejects_signed_zero_and_nan_payloads() (+45 more)

### Community 15 - "Path"
Cohesion: 0.07
Nodes (36): EndpointRole, Path, PathDirection, PathStep, PathTraceArena, PathTraceId, PathTraceNode, resolve_atom() (+28 more)

### Community 16 - "TxId"
Cohesion: 0.09
Nodes (39): atom_id_from(), BytesPersistentStore, checkpoint_then_recover(), closure_codec_with_capture_in_persistent_store(), decode_isolation(), encode_atom_meta(), encode_entry(), encode_isolation() (+31 more)

### Community 17 - "win_async.rs"
Cohesion: 0.06
Nodes (45): AtomicI64, cpu_affinity_cpu0(), goto_shutdown(), ioring_handle_ref(), ioring_raw_buffer_ref(), ioring_registered_buffer_ref(), new_fails_gracefully_if_no_ioring(), PendOp (+37 more)

### Community 18 - "Number"
Cohesion: 0.08
Nodes (30): BigInt, canonical_float(), canonical_fraction(), canonicalizes_nan_and_signed_zero_for_eq_ord_and_hash(), CanonicalNumber, conversion_error(), Decimal, f32 (+22 more)

### Community 19 - "src/ecs.rs"
Cohesion: 0.07
Nodes (34): AnyPool, ColorTag, FeatureVector, node_id(), NodeWeight, PageRank, Position2D, Position3D (+26 more)

### Community 20 - "ir.rs"
Cohesion: 0.19
Nodes (16): query_schema(), Column, LogicalNode, LogicalNodeKind, LogicalProgram, LogicalSelect, LogicalStatement, NodeId (+8 more)

### Community 21 - "StatusObject"
Cohesion: 0.11
Nodes (15): Condition, Domain, ErrorCategory, ErrorClassification, NotificationClassification, From, Option, String (+7 more)

### Community 22 - "Subgraph<State>"
Cohesion: 0.11
Nodes (14): Debug, EdgeIds, Eq, Formatter, H, Hash, NodeIds, Ord (+6 more)

### Community 23 - "store.rs"
Cohesion: 0.10
Nodes (31): auto_checkpoint_triggers(), checkpoint_clears_wal(), compress_payload(), crc32(), crc32_update(), create_and_reopen(), decompress_payload(), delete_graph() (+23 more)

### Community 24 - "status_codes.rs"
Cohesion: 0.09
Nodes (29): define_status_codes(), is_string(), is_vec_string(), last_path_ident(), option_lit(), Ident, Option, Parse (+21 more)

### Community 25 - "PropertyKeyId"
Cohesion: 0.07
Nodes (32): &'a PropertyMap, clone_is_independent(), empty_map_is_pointer_sized_and_does_not_reserve_storage(), entry_ref(), equality_order_and_hash_ignore_insertion_order(), from_iterator_uses_last_duplicate_value(), keeps_entries_sorted_and_replaces_existing_values(), PropertyKeyId (+24 more)

### Community 26 - "EdgeTopology"
Cohesion: 0.09
Nodes (21): INLINE_ENDPOINTS, distinguishes_binary_hyper_and_cross_level_topology(), EdgeDirection, EdgeTopology, QualifiedAtomId, AtomId, Box, Display (+13 more)

### Community 27 - "AtomId"
Cohesion: 0.12
Nodes (14): allocate_memory(), Result, atom_id_uses_u40_slots_and_u23_generations(), AtomId, AtomKind, Debug, Display, FerrosGrynnResult (+6 more)

### Community 28 - "Value"
Cohesion: 0.08
Nodes (29): AtomRef, bool, Closure, MaterializedGraph, Number, Path, RecordId, AtomRef (+21 more)

### Community 29 - "Geometry"
Cohesion: 0.07
Nodes (35): GeoJsonGeometry, GeoJsonPosition, assert_status(), compare_float_slices(), GeoJsonValue, Geometry, into_position(), into_positions() (+27 more)

### Community 30 - "GraphWorkspace<G>"
Cohesion: 0.08
Nodes (31): A, atom(), B, chooses_actual_smallest_pool_independent_of_type_order(), missing_pool_produces_empty_join(), AtomId, canonical_mutation_invalidates_derived_and_removes_dead_components(), component_mutation_does_not_change_graph_revision() (+23 more)

### Community 31 - "src/traversal.rs"
Cohesion: 0.10
Nodes (43): BellmanFordResult, bellman_ford(), BellmanFordError, bfs(), bfs_with(), dfs(), dfs_full(), dijkstra() (+35 more)

### Community 32 - "Transaction"
Cohesion: 0.05
Nodes (78): Fields, Instant, ActiveSnapshot, all_dirty(), atom_versions_keep_reused_slot_generations_distinct(), AtomVersions, CommitResult, configured_transaction_limits_have_domain_statuses() (+70 more)

### Community 33 - "Result"
Cohesion: 0.14
Nodes (12): Graph<Frozen>, Display, Formatter, H, IntoIterator, Item, Result, T (+4 more)

### Community 34 - "Text"
Cohesion: 0.08
Nodes (25): Borrow, Extend, CompactString, exposes_controlled_utf8_mutation(), round_trips_through_std_and_compact_strings(), AsRef, Deref, Display (+17 more)

### Community 35 - "FerrosGrynnError"
Cohesion: 0.08
Nodes (24): ErrorState, FerrosGrynnError, NonErrorStatus, Arc, Debug, Display, E, Error (+16 more)

### Community 36 - "DiagnosticRecord"
Cohesion: 0.08
Nodes (24): ArcStr, NonZeroU32, computes_unicode_locations_without_retaining_source(), DiagnosticRecord, ExecutionPhase, LabeledLocation, line_column_at(), LineColumn (+16 more)

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
Cohesion: 0.10
Nodes (13): Id, Handle, Handle<'a, T>, Metadata, Default, Index, IndexMut, Output (+5 more)

### Community 41 - "PropertyColumn<T>"
Cohesion: 0.16
Nodes (11): AnyPool, PropertyColumn<T>, PropertyState, AtomId, Default, ExactSizeIterator, FerrosGrynnResult, FnMut (+3 more)

### Community 42 - "formatting.rs"
Cohesion: 0.11
Nodes (27): And, Callable, Comma, DisplayValue, Formatted, Formatted<'a, F, T>, Formatted<'_, F, T>, Identifier (+19 more)

### Community 43 - "PosixDevice"
Cohesion: 0.06
Nodes (31): open_read_write_sync(), Arc, BufferPool, DirectFile, File, Path, PathBuf, Result (+23 more)

### Community 44 - "Closure"
Cohesion: 0.08
Nodes (25): ClosureFunction, Func, NonZeroU64, calls_function_and_keeps_process_identity(), Closure, different_closures_have_different_identity(), rejects_wrong_arity_before_invocation(), Arc (+17 more)

### Community 45 - "Result"
Cohesion: 0.13
Nodes (17): AlignedBuffer, alloc_aligned(), free_aligned(), platform_grow(), platform_open(), platform_open_readonly(), platform_read_page(), platform_write_page() (+9 more)

### Community 46 - "benches/property_map.rs"
Cohesion: 0.12
Nodes (32): bench_build(), bench_iterate_and_clone(), bench_lookup(), boxed_enum_map(), BoxedEnumIter, BoxedEnumPropertyMap, BoxedEnumRepr, btree_map() (+24 more)

### Community 47 - "async_io.rs"
Cohesion: 0.10
Nodes (33): AsyncIoEngine, AsyncUringEngine, BatchEntry, bg_loop(), blocking_op(), build(), build_ring(), bump_memlock() (+25 more)

### Community 48 - "PageNo"
Cohesion: 0.11
Nodes (24): PageFlags, PageType, aligned_page_has_exact_size_and_alignment(), aligned_page_slice_is_one_contiguous_byte_extent(), AlignedPage, mutable_validation_restores_persisted_checksum(), page_checksum(), page_checksum_mut() (+16 more)

### Community 49 - "arrow.rs"
Cohesion: 0.12
Nodes (32): ArrayRef, ArrowError, DataType, arrow_failure(), arrow_round_trip_preserves_none_null_and_native_scalars(), build_array(), ColumnKind, decode_rows() (+24 more)

### Community 50 - "StatusReport"
Cohesion: 0.11
Nodes (20): IntoSO, applies_gql_precedence(), Outcome, Outcome<T>, precedence(), rejects_errors(), Display, Error (+12 more)

### Community 51 - "Array"
Cohesion: 0.10
Nodes (17): &'a Array, &'a mut Array, Array, From, FromIterator, I, IntoIter, IntoIterator (+9 more)

### Community 52 - "Vector"
Cohesion: 0.10
Nodes (20): canonical_f32(), canonical_f64(), compare_f32(), compare_f32_value(), compare_f64(), compare_f64_value(), hash_f32(), hash_f64() (+12 more)

### Community 53 - "storage/buffer_pool.rs"
Cohesion: 0.05
Nodes (70): AtomicUsize, CachedPage, Condvar, Loader, RapidHashSet, AccessDecision, batch_handles_resident_loading_missing_and_duplicate_pages(), batch_preserves_order_duplicates_and_contiguous_extents() (+62 more)

### Community 54 - "Range"
Cohesion: 0.14
Nodes (26): End, borrow_bound(), Bound<T>, bound_value(), BoundExt, box_bound(), checks_inclusion(), compare_end() (+18 more)

### Community 55 - "StatusCode"
Cohesion: 0.11
Nodes (16): Deserialize, Ok, Serialize, InvalidSeverity, D, Display, Err, Error (+8 more)

### Community 56 - "storage/direct.rs"
Cohesion: 0.12
Nodes (49): OpenOptions, checked_file_length(), checked_page_extent(), checked_page_offset(), configure_direct_io(), create_new_never_overwrites_existing_file(), direct_file_batch_rejects_range_and_page_corruption(), direct_file_reads_and_validates_one_contiguous_extent() (+41 more)

### Community 57 - "graph/traversal.rs"
Cohesion: 0.18
Nodes (33): algorithm_failure(), bellman_ford(), bellman_ford_reports_negative_cycle_as_domain_error(), bfs_topology_and_components_are_correct(), binary_edges(), BinaryEdge, collect_weights(), condensation_collapses_cycles_and_deduplicates_bidirectional_steps() (+25 more)

### Community 58 - "benchmarks.rs"
Cohesion: 0.12
Nodes (27): Barrier, Read, aggregate_read_samples(), apply_gate(), BackendResult, BatchCalibration, CalibrationResult, comparison() (+19 more)

### Community 59 - "heap_estimator.rs"
Cohesion: 0.12
Nodes (13): Box, HashMap, K, S, String, T, V, Vec (+5 more)

### Community 60 - "Lsn"
Cohesion: 0.08
Nodes (31): BufReader, BufWriter, CheckpointHeader, crc32(), decode_atom_meta_payload(), decode_entry(), entry_crc(), Lsn (+23 more)

### Community 61 - "Type"
Cohesion: 0.09
Nodes (27): is_option(), analyze(), Analyzer, Analyzer<'a>, compatible(), is_integer(), is_number(), Box (+19 more)

### Community 62 - "Regex"
Cohesion: 0.09
Nodes (19): compiles_and_matches_pattern(), Regex, Display, Eq, Err, Error, Formatter, FromStr (+11 more)

### Community 63 - "Set"
Cohesion: 0.11
Nodes (15): &'a Set, BTreeSet<Value>, deduplicates_and_orders_values(), BTreeSet, From, FromIterator, I, IntoIter (+7 more)

### Community 64 - "AsyncIo"
Cohesion: 0.14
Nodes (17): async_io_batch_tag_routing(), async_io_write_read_sync(), AsyncIo, AsyncIoConfig, EngineConfig, Arc, Box, Default (+9 more)

### Community 65 - "AdjacencyMatrix"
Cohesion: 0.15
Nodes (13): AdjacencyMatrix, Incidence, IncidenceMatrix, IncidenceTensor, MetaAdjacencyMatrix, MetaTriple, AtomId, FxHashMap (+5 more)

### Community 66 - "UringState"
Cohesion: 0.17
Nodes (11): BufRingEntry, BufRing, FileSlot, IoUringEngine, Drop, Mutex, RawFd, Send (+3 more)

### Community 67 - "Object"
Cohesion: 0.06
Nodes (35): batch_random_access(), bench_collect(), bench_iter_sequential_borrowed(), bench_iter_sequential_owned(), bench_iter_sequential_with_step(), bench_push_preallocated(), bench_push_raw(), LargeData (+27 more)

### Community 68 - "get_metadata_inner"
Cohesion: 0.10
Nodes (23): add_m, Attrs, measurable_inner(), occurrence_error(), add, DeriveInput, Error, Parse (+15 more)

### Community 69 - "StorageBackend"
Cohesion: 0.10
Nodes (21): AsyncIoRingEngine, Arc, Default, ThreadPoolEngine, DirectFile, Any, AtomicU64, PathBuf (+13 more)

### Community 70 - "AtomId"
Cohesion: 0.18
Nodes (6): Condensation, CutFrame, CutSet, AtomId, Box, SpanningForest

### Community 71 - "PropertyBatch"
Cohesion: 0.16
Nodes (15): BooleanBuffer, imported_arrow_batch_requires_property_metadata(), invalid_batch(), PropertyBatch, BTreeMap, FerrosGrynnError, FerrosGrynnResult, Into (+7 more)

### Community 72 - "Result"
Cohesion: 0.23
Nodes (15): batch_selection_uses_smallest_candidate_within_limits(), buffered_read_at(), buffered_read_exact_at(), buffered_write_all_at(), buffered_write_at(), BufferedPageSource, calibration_page_count(), checked_byte_length() (+7 more)

### Community 73 - "HashValue"
Cohesion: 0.12
Nodes (14): AddAssign, ConstOne, Mul, MulAssign, One, HashValue, Add, Display (+6 more)

### Community 74 - "WorldComponents"
Cohesion: 0.13
Nodes (14): AnyPool, atom(), remove_atom_cleans_every_pool_and_respects_generation(), Any, Box, FerrosGrynnResult, Pred, RapidHashMap (+6 more)

### Community 75 - ".new"
Cohesion: 0.27
Nodes (7): IoBatch, IoRequest, Self, Vec, tag_routing(), threadpool_write_read(), Vec

### Community 76 - "Self"
Cohesion: 0.16
Nodes (10): ForestView, GraphView, Dag<State>, Forest<State>, RootedTree<State>, Self, super::Hypergraph<State>, super::Metagraph<State> (+2 more)

### Community 77 - "Value"
Cohesion: 0.17
Nodes (8): Box, From, FxHashMap, Option, Self, String, Vec, Value

### Community 78 - "StatusWireV1"
Cohesion: 0.20
Nodes (18): completion_omits_severity_and_classification(), DiagnosticRecordWireV1, error_classification(), FerrosGrynnError, labeled_location_to_wire(), LabeledLocationWireV1, location_to_wire(), LocationWireV1 (+10 more)

### Community 79 - "BinaryLiftingIndex"
Cohesion: 0.18
Nodes (10): BinaryLiftingIndex, QueueEntry, Eq, FnMut, Ord, Ordering, PartialEq, PartialOrd (+2 more)

### Community 80 - "PropertyDictionary"
Cohesion: 0.14
Nodes (12): assigns_dense_ids_and_resolves_both_directions(), interning_is_idempotent_and_iteration_uses_id_order(), PropertyDictionary, ExactSizeIterator, FerrosGrynnResult, IntoText, Item, Option (+4 more)

### Community 81 - "Subgraph<Mutable>"
Cohesion: 0.21
Nodes (7): Default, FerrosGrynnResult, GWB, PropertyMap, R, Self, Subgraph<Mutable>

### Community 82 - "DiagnosticValue"
Cohesion: 0.14
Nodes (11): bool, DiagnosticValue, f32, f64, BTreeMap, String, StatusParameter, str (+3 more)

### Community 83 - "EdgeEndpoint"
Cohesion: 0.17
Nodes (12): EdgeEndpoint, From, deduplicated_steps(), endpoint_kind(), local_steps(), AtomKind, DoubleEndedIterator, EdgeTopology (+4 more)

### Community 84 - "AccumList"
Cohesion: 0.15
Nodes (15): AccumList, FieldAccumArgs, Attribute, DeriveInput, Ident, Option, Parse, ParseStream (+7 more)

### Community 85 - "runtime/mod.rs"
Cohesion: 0.17
Nodes (12): Closure, atom_and_path_have_distinct_value_contracts(), display_is_deterministic_and_escapes_strings(), eq_ord_and_hash_agree_for_cross_number_values(), graph_transport_preserves_exact_type_and_typed_conversion(), reports_concrete_kind_and_static_type(), Number, String (+4 more)

### Community 86 - "Bytes"
Cohesion: 0.17
Nodes (7): Bytes, exposes_only_byte_oriented_api(), From, IntoVec, Self, Vec, Vec<u8>

### Community 87 - "bench_graph.rs"
Cohesion: 0.26
Nodes (16): bench_bfs_chain(), bench_bfs_dense(), bench_dfs_chain(), bench_dijkstra_chain(), bench_ecs_insert(), bench_ecs_iter(), bench_edge_insert_directed(), bench_materialise_chain() (+8 more)

### Community 88 - "os.rs"
Cohesion: 0.14
Nodes (9): MemoryError, NativeMemoryAllocationRefusedRepr, Display, Error, Formatter, Result, get_max_file_descriptors(), get_open_file_descriptors() (+1 more)

### Community 89 - ".register_datafusion_table"
Cohesion: 0.18
Nodes (11): IntoStr, MemTable, SessionContext, PropertyBatch, registers_only_after_arrow_batch_preserves_sentinels(), Arc, FerrosGrynnResult, Option (+3 more)

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

### Community 94 - "Option"
Cohesion: 0.27
Nodes (8): bfs(), dfs(), Predecessor, Option, Path, Vec, ShortestPaths, TraversalDirection

### Community 95 - "Values"
Cohesion: 0.23
Nodes (8): DagView, marks_materialized_result_without_new_storage_family(), DagView<'graph, R>, directed_forest_view(), FerrosGrynnResult, R, widening_views_do_not_rebuild_or_change_identity(), Values

### Community 97 - "HashFunction"
Cohesion: 0.21
Nodes (3): IncrementalXXH64, HashFunction, UtilHash

### Community 98 - "Mutable"
Cohesion: 0.08
Nodes (29): GraphWriteView, MutableGraphValue, Dag, PhantomData, Forest, Forest<Frozen>, Forest<State>, RootedTree (+21 more)

### Community 99 - "NoHeapEstimatorCache"
Cohesion: 0.19
Nodes (8): HeapEstimatorCache, NoHeapEstimatorCache, Self, Measurable, HashMap, K, V, size_of_hash_map()

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

### Community 109 - "T"
Cohesion: 0.31
Nodes (5): Option, SparseSet, T, PropertyColumn, SparseSet

### Community 110 - "AtomId"
Cohesion: 0.26
Nodes (3): AtomId, FnMut, SparseSet<T>

### Community 111 - "fg-meta/src/lib.rs"
Cohesion: 0.10
Nodes (15): EdgeIncidence, EdgeInfo, EdgeRole, EdgeTopology, AtomId, Box, Option, PropertyMap (+7 more)

### Community 112 - "Specification Design Authority"
Cohesion: 0.33
Nodes (6): Specification Design Authority, AtomWalk Syntax, MATCH Graph Patterns, GrynQL EBNF Grammar, Context-Sensitive Parser Disambiguation, GrynQL Statements

### Community 113 - "Persistent MVCC v2"
Cohesion: 0.33
Nodes (6): ClosureCodec, Exact Phantom Matching, Persistent MVCC v2, Serializable Snapshot Isolation, GrynQL Transaction Model, Execution MVCC and Snapshot Contract

### Community 114 - "Unified Atom Incidence"
Cohesion: 0.33
Nodes (6): Unified Atom Incidence, Graph Storage Families, Ferros-Grynn Graph Taxonomy, Mutable Frozen Type-State and Views, P4 Full Atom Incidence, GraphWorkspace ECS Sidecar

### Community 115 - ".set_null"
Cohesion: 0.29
Nodes (6): atom(), distinguishes_none_null_and_value(), removing_null_reports_null_instead_of_none(), replacement_removal_and_generation_keep_bitmaps_aligned(), Any, Self

### Community 116 - "hashing/mod.rs"
Cohesion: 0.33
Nodes (3): Option, Ordering, Hashable

### Community 118 - "Отчёт этапа 2: Sync Direct I/O и BufferPool"
Cohesion: 0.12
Nodes (14): Итоговая архитектура, Контракты и публичный API, Локальное дисковое хранилище Ferros-Grynn: sync-first, Проверки и критерии приёмки, Этапы реализации, Явно вне начального scope, Исправленный benchmark, Калибровка batch (+6 more)

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

### Community 125 - "exact_ops.rs"
Cohesion: 0.14
Nodes (24): CheckedAdd, CheckedMul, CheckedSub, PrimInt, Signed, arithmetic(), decimal_arithmetic(), float_arithmetic() (+16 more)

### Community 126 - "&'a Bytes"
Cohesion: 0.50
Nodes (3): &'a Bytes, IntoIter, IntoIterator

### Community 135 - "FerrosGrynnResult"
Cohesion: 0.11
Nodes (19): conversion_context_rejects_conflicting_reference_mappings(), DecodeContext, DiskAtomRef, DiskGraphRef, EncodeContext, AtomKind, AtomRef, BTreeMap (+11 more)

### Community 136 - "execution.rs"
Cohesion: 0.16
Nodes (21): QueryCatalog, binds_let_filters_projects_and_limits(), execute_program(), execute_text(), execute_transaction(), executes_return_with_pratt_precedence(), execution_phase(), gets_record_and_materializes_select_value() (+13 more)

### Community 137 - "read_pool_extent"
Cohesion: 0.31
Nodes (10): benchmark_key(), benchmark_lsn(), consume_pool(), consume_raw_direct(), read_pool_extent(), read_raw_direct_extent(), BufferPool, DirectFile (+2 more)

### Community 138 - "MaterializedGraph"
Cohesion: 0.10
Nodes (16): MaterializedGraph, preserves_exact_discriminant_in_generic_transport(), AtomId, EdgeIds, Eq, FerrosGrynnResult, H, Hash (+8 more)

### Community 139 - "FerrosGrynnResult"
Cohesion: 0.16
Nodes (27): binary(), build_cursor(), cast(), eval(), eval_usize(), execute_select(), FilterCursor, index_value() (+19 more)

### Community 141 - "DirtyTarget"
Cohesion: 0.07
Nodes (54): ApplyFn, ApplyUpdate, MutexGuard, ReadFn, ReplayFn, accept_remote_discards_local_conflicting_state(), apply_or_pause(), conflicted_live_graph() (+46 more)

### Community 142 - "conversion_error"
Cohesion: 0.47
Nodes (10): conversion_error(), random_order(), read_workloads(), ReadWorkload, NonZeroUsize, Vec, scan_hot_order(), sequential_order() (+2 more)

### Community 147 - ".next"
Cohesion: 0.50
Nodes (3): BoxedEnumIter<'a>, Item, Iterator

### Community 149 - "StorageValue"
Cohesion: 0.10
Nodes (25): array_storage_conversion_reuses_owned_elements(), datetime_conversion_preserves_pre_epoch_nanos_and_time_zone_kind(), graph_references_use_explicit_context_mapping(), QualifiedAtomRef, rejects_runtime_only_values_and_missing_references(), Bound, Box, Deref (+17 more)

### Community 152 - "Iter<'_, T>"
Cohesion: 0.67
Nodes (3): Iter<'_, T>, ExactSizeIterator, FusedIterator

### Community 157 - "Q: What is MetaGraph?"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: What is MetaGraph?, Source Nodes

### Community 159 - "Handle<'_, T>"
Cohesion: 0.50
Nodes (3): Handle<'_, T>, Deref, Target

### Community 162 - "GraphKind"
Cohesion: 0.16
Nodes (4): GraphKind, Display, Formatter, Result

### Community 187 - "Graph"
Cohesion: 0.07
Nodes (44): INLINE_INCIDENCE, atom_does_not_exist(), atom_kind_mismatch(), copy_from_read(), endpoint_kind(), freeze_thaw_preserves_identity_ids_and_topology(), GeneralStorage, Graph (+36 more)

### Community 205 - "Metagraph<State>"
Cohesion: 0.06
Nodes (32): exposes_explicit_meta_atom_api_over_general_layout(), Metagraph<Frozen>, Metagraph<Mutable>, Metagraph<State>, AtomId, Clone, Debug, Default (+24 more)

### Community 253 - "SnapshotId"
Cohesion: 0.04
Nodes (51): assert_snapshot_released(), AtomIds, AtomIds<'a>, AtomRef, cascade_removal_marks_removed_incident_edges_dirty(), disk_view_and_hybrid_materialization_have_the_same_read_result(), DiskGraphView, DiskGraphView<'_, R> (+43 more)

## Knowledge Gaps
- **71 isolated node(s):** `Position2D`, `Position3D`, `Timestamp`, `ColorTag`, `Velocity` (+66 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **9 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `PropertyKeyId` connect `PropertyKeyId` to `Transaction`, `PropertyBatch`, `PropertyColumn<T>`, `WorldComponents`, `T`, `AtomId`, `benches/property_map.rs`, `PropertyDictionary`, `arrow.rs`, `FerrosGrynnResult`, `.set_null`, `StorageValue`, `.register_datafusion_table`?**
  _High betweenness centrality (0.132) - this node is a cross-community bridge._
- **Why does `Type` connect `Type` to `Transaction`, `GraphKind`, `FerrosGrynnResult`, `Closure`, `ir.rs`, `runtime/mod.rs`, `Vector`, `status_codes.rs`?**
  _High betweenness centrality (0.111) - this node is a cross-community bridge._
- **Why does `Graph` connect `Graph` to `ForestStorage`, `Mutable`, `Dag<State>`, `graph_core.rs`, `Hypergraph<State>`, `MaterializedGraph`, `EdgeColumns`, `DirtyTarget`, `Metagraph<State>`, `runtime/mod.rs`, `Subgraph<State>`, `SnapshotId`?**
  _High betweenness centrality (0.108) - this node is a cross-community bridge._
- **What connects `Position2D`, `Position3D`, `Timestamp` to the rest of the system?**
  _71 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `TextSpan` be split into smaller, more focused modules?**
  _Cohesion score 0.06167212647770973 - nodes in this community are weakly interconnected._
- **Should `ForestStorage` be split into smaller, more focused modules?**
  _Cohesion score 0.0827653359298929 - nodes in this community are weakly interconnected._
- **Should `Dag<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.05824175824175824 - nodes in this community are weakly interconnected._