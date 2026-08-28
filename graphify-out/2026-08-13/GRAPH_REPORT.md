# Graph Report - ferros-grynn  (2026-08-13)

## Corpus Check
- 150 files · ~210,416 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 4889 nodes · 13258 edges · 160 communities (149 shown, 11 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 107 edges (avg confidence: 0.81)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `e7e0aa33`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- Parser
- ForestStorage
- Dag<State>
- Hypergraph<State>
- MetaGraph
- RecordIdPart
- read.rs
- MvccTransaction
- AtomId
- GraphId
- BufferPool
- EdgeColumns
- Graph<State>
- Uuid
- FerrosGrynnResult
- Path
- Result
- win_async.rs
- Number
- src/ecs.rs
- execution.rs
- status.rs
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
- DirectFile
- benches/property_map.rs
- ClockProInner
- async_io.rs
- arrow.rs
- StatusReport
- Array
- Vector
- TextSpan
- Range
- StatusObject
- Lsn
- graph/traversal.rs
- .new
- heap_estimator.rs
- GraphKind
- .expr
- Regex
- Set
- AsyncIo
- AdjacencyMatrix
- SyncIo
- Object
- get_metadata_inner
- StorageBackend
- fg-meta/src/lib.rs
- PropertyBatch
- RootedTree
- HashValue
- WorldComponents
- Result
- Self
- StatusDefinition
- StatusWireV1
- BinaryLiftingIndex
- PropertyDictionary
- AtomId
- Value
- EdgeEndpoint
- AccumList
- .new
- Bytes
- bench_graph.rs
- os.rs
- .register_datafusion_table
- DiagnosticValue
- Graphify
- PropertyMap
- wy_hash.rs
- Option
- ParserSettings
- Item
- HashFunction
- Mutable
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
- AlignedBuffer
- .run
- EdgeInfo
- Specification Design Authority
- Persistent MVCC v2
- Unified Atom Incidence
- Token
- hashing/mod.rs
- XorShift32HashFunction
- Forest<Mutable>
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
- StorageValue
- Result
- OrdFloat
- MaterializedGraph
- StringDictionary
- TxId
- DirtyTarget
- Type
- .fmt
- Self
- .new
- .raw_fd
- MetaGraphError
- .deref
- TypeTag
- Values
- Iter<'_, T>
- GraphStorageMode
- Q: What is MetaGraph?
- Handle<'_, T>
- AtomId
- Graph
- Metagraph<State>
- .call
- StatusInput

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
10. `Decoder<'a>` - 45 edges

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

## Communities (160 total, 11 thin omitted)

### Community 0 - "Parser"
Cohesion: 0.21
Nodes (8): Identifier, cover(), Parser, FerrosGrynnError, FerrosGrynnResult, Option, Self, Stk

### Community 1 - "ForestStorage"
Cohesion: 0.19
Nodes (15): forest_node_removal_marks_every_incident_edge_dirty(), forest_rejects_cycles_without_mutation(), ForestStorage, Arc, AtomId, BTreeSet, EdgeTopology, FerrosGrynnResult (+7 more)

### Community 2 - "Dag<State>"
Cohesion: 0.06
Nodes (46): build_oriented_tree_storage(), build_storage(), constraint(), Dag<Frozen>, Dag<Mutable>, Dag<State>, DagStorage, frozen_layout_round_trips_to_mutable_adjacency() (+38 more)

### Community 3 - "Hypergraph<State>"
Cohesion: 0.06
Nodes (42): build_storage(), constraint(), graph_conversion_rejects_edge_endpoints(), Hypergraph<Frozen>, Hypergraph<Mutable>, Hypergraph<State>, HyperStorage, permits_a_vertex_in_both_directed_incidence_roles() (+34 more)

### Community 4 - "MetaGraph"
Cohesion: 0.09
Nodes (14): MetaGraph, AtomId, AtomKind, Box, Default, EdgeTopology, Option, PropertyMap (+6 more)

### Community 5 - "RecordIdPart"
Cohesion: 0.12
Nodes (23): JsonValue, assert_status(), enforces_the_composite_nesting_limit(), quotes_ambiguous_string_ids_for_round_trip(), RecordId, RecordIdPart, recursively_rejects_unsupported_parts(), rejects_empty_components() (+15 more)

### Community 6 - "read.rs"
Cohesion: 0.05
Nodes (34): Vec, assert_snapshot_released(), AtomIds<'a>, AtomRef, cascade_removal_marks_removed_incident_edges_dirty(), disk_view_and_hybrid_materialization_have_the_same_read_result(), DiskGraphView, DiskGraphView<'tx, R> (+26 more)

### Community 7 - "MvccTransaction"
Cohesion: 0.06
Nodes (51): AtomMeta, basic_generic_rw(), BytesCodec, closure_codec_with_capture(), ClosureCodec, ClosureCodec<T>, Codec, exact_phantom_label() (+43 more)

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
Cohesion: 0.05
Nodes (35): clone_copies_only_the_changed_chunk(), CowVec, CowVec<T, CHUNK_LEN>, Iter, Iter<'a, T>, Arc, Default, FnMut (+27 more)

### Community 12 - "Graph<State>"
Cohesion: 0.07
Nodes (24): Copied, copy_from_read(), freeze_thaw_preserves_identity_ids_and_topology(), Graph<Frozen>, Graph<State>, invalid_edges_are_transactional_and_use_graph_statuses(), BTreeSet, Clone (+16 more)

### Community 13 - "Uuid"
Cohesion: 0.10
Nodes (16): InnerUlid, InnerUuid, Display, Err, Formatter, From, FromStr, Result (+8 more)

### Community 14 - "FerrosGrynnResult"
Cohesion: 0.08
Nodes (53): AlignedVec, Position, all_format_v1_tags_round_trip(), archive_envelope_checks_magic_version_schema_length_and_checksum(), ArchiveEnvelope, assert_status(), canonical_f32(), canonical_float_encoding_rejects_signed_zero_and_nan_payloads() (+45 more)

### Community 15 - "Path"
Cohesion: 0.07
Nodes (36): EndpointRole, Path, PathDirection, PathStep, PathTraceArena, PathTraceId, PathTraceNode, resolve_atom() (+28 more)

### Community 16 - "Result"
Cohesion: 0.17
Nodes (15): PageId, atom_id_from(), BytesPersistentStore, checkpoint_then_recover(), CheckpointHeader, crc32(), encode_entry(), PersistentMvccStore<T, C> (+7 more)

### Community 17 - "win_async.rs"
Cohesion: 0.08
Nodes (39): cpu_affinity_cpu0(), goto_shutdown(), ioring_handle_ref(), ioring_raw_buffer_ref(), ioring_registered_buffer_ref(), new_fails_gracefully_if_no_ioring(), PendOp, probe_ioring() (+31 more)

### Community 18 - "Number"
Cohesion: 0.08
Nodes (30): BigInt, canonical_float(), canonical_fraction(), canonicalizes_nan_and_signed_zero_for_eq_ord_and_hash(), CanonicalNumber, conversion_error(), Decimal, f32 (+22 more)

### Community 19 - "src/ecs.rs"
Cohesion: 0.07
Nodes (34): AnyPool, ColorTag, FeatureVector, node_id(), NodeWeight, PageRank, Position2D, Position3D (+26 more)

### Community 20 - "execution.rs"
Cohesion: 0.05
Nodes (88): CheckedAdd, CheckedMul, CheckedSub, PrimInt, Signed, QueryCatalog, arithmetic(), binary() (+80 more)

### Community 21 - "status.rs"
Cohesion: 0.12
Nodes (13): Condition, Domain, ErrorCategory, ErrorClassification, NotificationClassification, Option, String, status_object_builds_full_description() (+5 more)

### Community 22 - "Subgraph<State>"
Cohesion: 0.06
Nodes (25): AtomId, Clone, Debug, Default, EdgeIds, Eq, FerrosGrynnResult, Formatter (+17 more)

### Community 23 - "store.rs"
Cohesion: 0.10
Nodes (31): auto_checkpoint_triggers(), checkpoint_clears_wal(), compress_payload(), crc32(), crc32_update(), create_and_reopen(), decompress_payload(), delete_graph() (+23 more)

### Community 24 - "status_codes.rs"
Cohesion: 0.16
Nodes (15): define_status_codes(), last_path_ident(), Option, Parse, Path, String, StatusBody, StatusKindInput (+7 more)

### Community 25 - "PropertyKeyId"
Cohesion: 0.05
Nodes (41): &'a PropertyMap, clone_is_independent(), empty_map_is_pointer_sized_and_does_not_reserve_storage(), entry_ref(), equality_order_and_hash_ignore_insertion_order(), from_iterator_uses_last_duplicate_value(), keeps_entries_sorted_and_replaces_existing_values(), PropertyIter (+33 more)

### Community 26 - "EdgeTopology"
Cohesion: 0.09
Nodes (21): INLINE_ENDPOINTS, distinguishes_binary_hyper_and_cross_level_topology(), EdgeDirection, EdgeTopology, QualifiedAtomId, AtomId, Box, Display (+13 more)

### Community 27 - "AtomId"
Cohesion: 0.07
Nodes (20): AtomicI64, allocate_memory(), compare_and_swap_i64(), compare_and_swap_u64(), get_and_add_i64(), get_and_set_i64(), AtomicU64, Result (+12 more)

### Community 28 - "Value"
Cohesion: 0.07
Nodes (26): Closure, atom_and_path_have_distinct_value_contracts(), display_is_deterministic_and_escapes_strings(), eq_ord_and_hash_agree_for_cross_number_values(), graph_transport_preserves_exact_type_and_typed_conversion(), reports_concrete_kind_and_static_type(), AtomRef, Box (+18 more)

### Community 29 - "Geometry"
Cohesion: 0.07
Nodes (35): GeoJsonGeometry, GeoJsonPosition, assert_status(), compare_float_slices(), GeoJsonValue, Geometry, into_position(), into_positions() (+27 more)

### Community 30 - "GraphWorkspace<G>"
Cohesion: 0.08
Nodes (31): A, atom(), B, chooses_actual_smallest_pool_independent_of_type_order(), missing_pool_produces_empty_join(), AtomId, canonical_mutation_invalidates_derived_and_removes_dead_components(), component_mutation_does_not_change_graph_revision() (+23 more)

### Community 31 - "src/traversal.rs"
Cohesion: 0.12
Nodes (37): BellmanFordResult, bellman_ford(), BellmanFordError, bfs(), bfs_with(), dfs(), dfs_full(), dijkstra() (+29 more)

### Community 32 - "Transaction"
Cohesion: 0.05
Nodes (78): Fields, Instant, ActiveSnapshot, all_dirty(), atom_versions_keep_reused_slot_generations_distinct(), AtomVersions, CommitResult, configured_transaction_limits_have_domain_statuses() (+70 more)

### Community 33 - "Result"
Cohesion: 0.09
Nodes (27): AtomRef, bool, Closure, Graph<Frozen>, MaterializedGraph, Number, Path, RecordId (+19 more)

### Community 34 - "Text"
Cohesion: 0.08
Nodes (25): AsRef, Borrow, Extend, CompactString, exposes_controlled_utf8_mutation(), round_trips_through_std_and_compact_strings(), Deref, Display (+17 more)

### Community 35 - "FerrosGrynnError"
Cohesion: 0.09
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
Cohesion: 0.10
Nodes (19): AnyPool, atom(), DenseBitmap, distinguishes_none_null_and_value(), PropertyColumn<T>, PropertyState, removing_null_reports_null_instead_of_none(), replacement_removal_and_generation_keep_bitmaps_aligned() (+11 more)

### Community 42 - "formatting.rs"
Cohesion: 0.11
Nodes (27): And, Callable, Comma, DisplayValue, Formatted, Formatted<'a, F, T>, Formatted<'_, F, T>, Identifier (+19 more)

### Community 43 - "PosixDevice"
Cohesion: 0.09
Nodes (18): NvmeDevice, NvmeStats, open_best(), open_best_posix(), posix_device_rw(), PosixDevice, Any, Arc (+10 more)

### Community 44 - "Closure"
Cohesion: 0.10
Nodes (21): ClosureFunction, Func, NonZeroU64, calls_function_and_keeps_process_identity(), Closure, different_closures_have_different_identity(), rejects_wrong_arity_before_invocation(), Arc (+13 more)

### Community 45 - "DirectFile"
Cohesion: 0.19
Nodes (14): alloc_aligned(), DirectFile, platform_grow(), platform_open(), platform_open_readonly(), platform_write_page(), Any, AtomicU64 (+6 more)

### Community 46 - "benches/property_map.rs"
Cohesion: 0.12
Nodes (32): bench_build(), bench_iterate_and_clone(), bench_lookup(), boxed_enum_map(), BoxedEnumIter, BoxedEnumPropertyMap, BoxedEnumRepr, btree_map() (+24 more)

### Community 47 - "ClockProInner"
Cohesion: 0.13
Nodes (9): ClockProInner, MemBack, Any, HashMap, HashSet, Mutex, Result, Vec (+1 more)

### Community 48 - "async_io.rs"
Cohesion: 0.09
Nodes (33): BufRingEntry, AsyncIoEngine, AsyncUringEngine, BufRing, build(), build_ring(), bump_memlock(), create_engine() (+25 more)

### Community 49 - "arrow.rs"
Cohesion: 0.11
Nodes (36): ArrayRef, ArrowError, DataType, arrow_failure(), arrow_round_trip_preserves_none_null_and_native_scalars(), build_array(), ColumnKind, decode_rows() (+28 more)

### Community 50 - "StatusReport"
Cohesion: 0.11
Nodes (20): IntoSO, applies_gql_precedence(), Outcome, Outcome<T>, precedence(), rejects_errors(), Display, Error (+12 more)

### Community 51 - "Array"
Cohesion: 0.10
Nodes (17): &'a Array, &'a mut Array, Array, From, FromIterator, I, IntoIter, IntoIterator (+9 more)

### Community 52 - "Vector"
Cohesion: 0.10
Nodes (20): canonical_f32(), canonical_f64(), compare_f32(), compare_f32_value(), compare_f64(), compare_f64_value(), hash_f32(), hash_f64() (+12 more)

### Community 53 - "TextSpan"
Cohesion: 0.22
Nodes (20): TextSpan, Expr, ExprKind, Identifier, LetStatement, Literal, ObjectField, Program (+12 more)

### Community 54 - "Range"
Cohesion: 0.14
Nodes (26): End, borrow_bound(), Bound<T>, bound_value(), BoundExt, box_bound(), checks_inclusion(), compare_end() (+18 more)

### Community 55 - "StatusObject"
Cohesion: 0.10
Nodes (19): Deserialize, Ok, Serialize, InvalidSeverity, D, Display, Err, Error (+11 more)

### Community 56 - "Lsn"
Cohesion: 0.14
Nodes (15): BufReader, decode_entry(), decode_isolation(), encode_isolation(), entry_crc(), Lsn, MultiSegmentReader, Display (+7 more)

### Community 57 - "graph/traversal.rs"
Cohesion: 0.18
Nodes (33): algorithm_failure(), bellman_ford(), bellman_ford_reports_negative_cycle_as_domain_error(), bfs_topology_and_components_are_correct(), binary_edges(), BinaryEdge, collect_weights(), condensation_collapses_cycles_and_deduplicates_bidirectional_steps() (+25 more)

### Community 58 - ".new"
Cohesion: 0.12
Nodes (15): BufWriter, PersistentMvccStore, PersistLayer, Arc, AtomicU64, File, Mutex, Path (+7 more)

### Community 59 - "heap_estimator.rs"
Cohesion: 0.08
Nodes (21): HeapEstimatorCache, NoHeapEstimatorCache, Self, Measurable, HashMap, K, V, size_of_hash_map() (+13 more)

### Community 60 - "GraphKind"
Cohesion: 0.15
Nodes (4): GraphKind, Display, Formatter, Result

### Community 61 - ".expr"
Cohesion: 0.18
Nodes (16): analyze(), Analyzer, Analyzer<'a>, compatible(), is_integer(), is_number(), Box, BTreeMap (+8 more)

### Community 62 - "Regex"
Cohesion: 0.09
Nodes (19): compiles_and_matches_pattern(), Regex, Display, Eq, Err, Error, Formatter, FromStr (+11 more)

### Community 63 - "Set"
Cohesion: 0.07
Nodes (29): batch_random_access(), bench_collect(), bench_iter_sequential_borrowed(), bench_iter_sequential_owned(), bench_iter_sequential_with_step(), bench_push_preallocated(), bench_push_raw(), LargeData (+21 more)

### Community 64 - "AsyncIo"
Cohesion: 0.15
Nodes (16): async_io_batch_tag_routing(), async_io_write_read_sync(), AsyncIo, AsyncIoConfig, EngineConfig, Arc, Box, Default (+8 more)

### Community 65 - "AdjacencyMatrix"
Cohesion: 0.15
Nodes (13): AdjacencyMatrix, Incidence, IncidenceMatrix, IncidenceTensor, MetaAdjacencyMatrix, MetaTriple, AtomId, FxHashMap (+5 more)

### Community 66 - "SyncIo"
Cohesion: 0.19
Nodes (9): open_read_write_sync(), Arc, File, Path, PathBuf, Result, Self, SyncIo (+1 more)

### Community 67 - "Object"
Cohesion: 0.09
Nodes (21): Keys, &'a mut Object, &'a Object, BTreeMap<Text, Value>, has_canonical_key_order_independent_of_insertion(), Object, BTreeMap, From (+13 more)

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

### Community 72 - "RootedTree"
Cohesion: 0.23
Nodes (11): Forest<State>, graph_to_tree_validates_classic_topology(), RootedTree, RootedTree<Mutable>, RootedTree<State>, Error, From, Result (+3 more)

### Community 73 - "HashValue"
Cohesion: 0.12
Nodes (14): AddAssign, ConstOne, Mul, MulAssign, One, HashValue, Add, Display (+6 more)

### Community 74 - "WorldComponents"
Cohesion: 0.09
Nodes (22): AnyPool, atom(), remove_atom_cleans_every_pool_and_respects_generation(), Any, AtomId, Box, FerrosGrynnResult, FnMut (+14 more)

### Community 75 - "Result"
Cohesion: 0.13
Nodes (17): BatchEntry, bg_loop(), blocking_op(), IoBatch, IoCompletion, IoOp, IoRequest, Mem (+9 more)

### Community 76 - "Self"
Cohesion: 0.12
Nodes (16): DagView, ForestView, GraphView, Dag<State>, DagView<'graph, R>, directed_forest_view(), Forest<State>, RootedTree<State> (+8 more)

### Community 77 - "StatusDefinition"
Cohesion: 0.28
Nodes (5): option_lit(), Ident, TokenStream, StatusDefinition, LitStr

### Community 78 - "StatusWireV1"
Cohesion: 0.21
Nodes (17): completion_omits_severity_and_classification(), DiagnosticRecordWireV1, FerrosGrynnError, labeled_location_to_wire(), LabeledLocationWireV1, location_to_wire(), LocationWireV1, Box (+9 more)

### Community 79 - "BinaryLiftingIndex"
Cohesion: 0.18
Nodes (10): BinaryLiftingIndex, QueueEntry, Eq, FnMut, Ord, Ordering, PartialEq, PartialOrd (+2 more)

### Community 80 - "PropertyDictionary"
Cohesion: 0.14
Nodes (12): assigns_dense_ids_and_resolves_both_directions(), interning_is_idempotent_and_iteration_uses_id_order(), PropertyDictionary, ExactSizeIterator, FerrosGrynnResult, IntoText, Item, Option (+4 more)

### Community 81 - "AtomId"
Cohesion: 0.18
Nodes (6): Condensation, CutFrame, CutSet, AtomId, Box, SpanningForest

### Community 82 - "Value"
Cohesion: 0.17
Nodes (8): Box, From, FxHashMap, Option, Self, String, Vec, Value

### Community 83 - "EdgeEndpoint"
Cohesion: 0.17
Nodes (12): EdgeEndpoint, From, deduplicated_steps(), endpoint_kind(), local_steps(), AtomKind, DoubleEndedIterator, EdgeTopology (+4 more)

### Community 84 - "AccumList"
Cohesion: 0.15
Nodes (15): AccumList, FieldAccumArgs, Attribute, DeriveInput, Ident, Option, Parse, ParseStream (+7 more)

### Community 85 - ".new"
Cohesion: 0.25
Nodes (12): build_parent_map(), build_storage(), constraint(), find_root(), infer_directed_orientation(), rooted_tree_tree_forest_conversions_reuse_atom_ids(), BTreeMap, FerrosGrynnError (+4 more)

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

### Community 90 - "DiagnosticValue"
Cohesion: 0.14
Nodes (11): bool, DiagnosticValue, f32, f64, BTreeMap, String, StatusParameter, str (+3 more)

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

### Community 95 - "ParserSettings"
Cohesion: 0.19
Nodes (9): Config, Default, Self, parse(), ParserSettings, Default, FerrosGrynnResult, From (+1 more)

### Community 97 - "HashFunction"
Cohesion: 0.21
Nodes (3): IncrementalXXH64, HashFunction, UtilHash

### Community 98 - "Mutable"
Cohesion: 0.20
Nodes (11): GraphWriteView, MutableGraphValue, Dag, PhantomData, Forest, PhantomData, Tree, Hypergraph (+3 more)

### Community 99 - "EdgeView"
Cohesion: 0.09
Nodes (18): AtomIds, DiskGraphView<'_, R>, EdgeView, EdgeView<'a>, GraphReadView<'_, R>, NodeView, NodeView<'a>, AtomId (+10 more)

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

### Community 109 - "AlignedBuffer"
Cohesion: 0.17
Nodes (7): AlignedBuffer, free_aligned(), platform_read_page(), Drop, HANDLE, Send, Sync

### Community 110 - ".run"
Cohesion: 0.24
Nodes (15): decodes_quoted_names_parameters_and_string_escapes(), is_ident_continue(), is_ident_start(), keeps_utf8_byte_spans_and_greedy_operators(), lex(), Lexer, parse_error(), Box (+7 more)

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

### Community 115 - "Token"
Cohesion: 0.26
Nodes (9): Token, distinguishes_spaced_record_separator(), enforces_query_depth(), parse(), reports_unsupported_statement_and_unclosed_delimiter(), respects_precedence_and_record_adjacency(), Vec, TokenBuffer (+1 more)

### Community 116 - "hashing/mod.rs"
Cohesion: 0.33
Nodes (3): Option, Ordering, Hashable

### Community 118 - "Forest<Mutable>"
Cohesion: 0.25
Nodes (3): Forest<Mutable>, Default, GWB

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

### Community 135 - "StorageValue"
Cohesion: 0.08
Nodes (35): array_storage_conversion_reuses_owned_elements(), conversion_context_rejects_conflicting_reference_mappings(), datetime_conversion_preserves_pre_epoch_nanos_and_time_zone_kind(), DecodeContext, DiskAtomRef, DiskGraphRef, EncodeContext, graph_references_use_explicit_context_mapping() (+27 more)

### Community 136 - "Result"
Cohesion: 0.64
Nodes (3): ParseStream, Result, Self

### Community 137 - "OrdFloat"
Cohesion: 0.36
Nodes (6): OrdFloat, Eq, Ord, Ordering, PartialOrd, Self

### Community 138 - "MaterializedGraph"
Cohesion: 0.10
Nodes (16): MaterializedGraph, preserves_exact_discriminant_in_generic_transport(), AtomId, EdgeIds, Eq, FerrosGrynnResult, H, Hash (+8 more)

### Community 139 - "StringDictionary"
Cohesion: 0.47
Nodes (4): BTreeMap, Option, Text, StringDictionary

### Community 140 - "TxId"
Cohesion: 0.13
Nodes (25): closure_codec_with_capture_in_persistent_store(), encode_atom_meta(), multi_segment_reader(), PageLsnTable, prune_before_removes_old_segments(), read_snapshot_streaming(), recover(), recovery_restores_atom_meta() (+17 more)

### Community 141 - "DirtyTarget"
Cohesion: 0.06
Nodes (57): ApplyFn, ApplyUpdate, MutexGuard, ReadFn, ReplayFn, Vec, accept_remote_discards_local_conflicting_state(), apply_or_pause() (+49 more)

### Community 142 - "Type"
Cohesion: 0.09
Nodes (13): is_string(), is_vec_string(), is_option(), AtomKind, Box, Display, Formatter, Result (+5 more)

### Community 144 - "Self"
Cohesion: 0.16
Nodes (9): Forest<Frozen>, RootedTree<Frozen>, Clone, R, Self, Tree<Frozen>, Tree<Mutable>, QueryMaterialize (+1 more)

### Community 147 - "MetaGraphError"
Cohesion: 0.50
Nodes (4): MetaGraphError, AtomId, AtomKind, Error

### Community 151 - "Values"
Cohesion: 0.40
Nodes (3): marks_materialized_result_without_new_storage_family(), Subgraph, Values

### Community 152 - "Iter<'_, T>"
Cohesion: 0.67
Nodes (3): Iter<'_, T>, ExactSizeIterator, FusedIterator

### Community 157 - "Q: What is MetaGraph?"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: What is MetaGraph?, Source Nodes

### Community 159 - "Handle<'_, T>"
Cohesion: 0.50
Nodes (3): Handle<'_, T>, Deref, Target

### Community 187 - "AtomId"
Cohesion: 0.14
Nodes (23): atom_does_not_exist(), atom_kind_mismatch(), endpoint_kind(), Graph<Mutable>, merge_transaction_atom(), remap_topology(), removal_cascades_through_edge_endpoints_and_reuses_generation(), AtomId (+15 more)

### Community 189 - "Graph"
Cohesion: 0.13
Nodes (13): INLINE_INCIDENCE, GeneralStorage, Graph, merge_transaction_graph(), Arc, Default, PhantomData, RapidHashMap (+5 more)

### Community 205 - "Metagraph<State>"
Cohesion: 0.06
Nodes (32): exposes_explicit_meta_atom_api_over_general_layout(), Metagraph<Frozen>, Metagraph<Mutable>, Metagraph<State>, AtomId, Clone, Debug, Default (+24 more)

### Community 270 - ".call"
Cohesion: 0.33
Nodes (4): FerrosGrynnError, Formatter, Result, Value

### Community 281 - "StatusInput"
Cohesion: 0.40
Nodes (4): Vec, StatusInput, StatusDefinition, ToTokens

## Knowledge Gaps
- **59 isolated node(s):** `Position2D`, `Position3D`, `Timestamp`, `ColorTag`, `Velocity` (+54 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **11 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Graph` connect `Graph` to `Mutable`, `Dag<State>`, `graph_core.rs`, `Hypergraph<State>`, `EdgeView`, `read.rs`, `RootedTree`, `MaterializedGraph`, `EdgeColumns`, `Graph<State>`, `DirtyTarget`, `Metagraph<State>`, `Subgraph<State>`, `Values`, `AtomId`, `Value`?**
  _High betweenness centrality (0.139) - this node is a cross-community bridge._
- **Why does `Type` connect `Type` to `Transaction`, `Closure`, `GraphKind`, `execution.rs`, `Vector`, `status_codes.rs`, `Value`, `.expr`?**
  _High betweenness centrality (0.111) - this node is a cross-community bridge._
- **Why does `PropertyKeyId` connect `PropertyKeyId` to `Transaction`, `PropertyBatch`, `StorageValue`, `PropertyColumn<T>`, `WorldComponents`, `benches/property_map.rs`, `FerrosGrynnResult`, `PropertyDictionary`, `arrow.rs`, `.register_datafusion_table`?**
  _High betweenness centrality (0.104) - this node is a cross-community bridge._
- **What connects `Position2D`, `Position3D`, `Timestamp` to the rest of the system?**
  _59 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Dag<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.059394631639063396 - nodes in this community are weakly interconnected._
- **Should `Hypergraph<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.06201550387596899 - nodes in this community are weakly interconnected._
- **Should `MetaGraph` be split into smaller, more focused modules?**
  _Cohesion score 0.08857808857808858 - nodes in this community are weakly interconnected._