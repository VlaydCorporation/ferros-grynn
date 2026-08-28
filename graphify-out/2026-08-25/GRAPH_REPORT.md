# Graph Report - ferros-grynn  (2026-08-25)

## Corpus Check
- 166 files · ~283,549 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 6423 nodes · 19232 edges · 291 communities (181 shown, 110 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 182 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `7c2aa7af`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- Parser
- FerrosGrynnResult
- Dag<State>
- Hypergraph<State>
- MetaGraph
- Values
- catalog.rs
- MvccTransaction
- AtomId
- AtomId
- PageId
- PageNo
- Graph<State>
- .new
- FerrosGrynnResult
- path.rs
- codec.rs
- win_async.rs
- Number
- src/ecs.rs
- wal.rs
- StatusObject
- Subgraph<State>
- store.rs
- status_codes.rs
- PropertyKeyId
- EdgeTopology
- AtomKind
- Value
- Geometry
- GraphRef
- src/traversal.rs
- .new
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
- async_io.rs
- PageLsn
- arrow.rs
- StatusReport
- Lsn
- Vector
- storage/buffer_pool.rs
- Range
- superblock.rs
- storage/direct.rs
- graph/traversal.rs
- benchmarks.rs
- heap_estimator.rs
- EdgeRead
- Type
- Regex
- Set
- AsyncIo
- AdjacencyMatrix
- Result
- Object
- get_metadata_inner
- StorageBackend
- .run
- PropertyBatch
- FerrosGrynnResult
- HashValue
- WorldComponents
- .new
- Self
- graph_index.rs
- StatusWireV1
- Path
- PropertyDictionary
- local.rs
- DiagnosticValue
- DoubleEndedIterator
- AccumList
- AtomRef
- Bytes
- bench_graph.rs
- os.rs
- .register_datafusion_table
- CreatedDatabaseDirectory
- Graphify
- PropertyMap
- wy_hash.rs
- EdgeInfo
- Error
- TextSpan
- HashFunction
- .new
- CowVec<T, CHUNK_LEN>
- graph_core.rs
- build_from_map
- memohash.rs
- Ferros-Grynn Repository Guidance
- Physical Query Execution
- Values Module Guidance
- GrynQL Language Specification
- atoms
- helpers.rs
- Array
- AtomId
- NodeInfo
- Specification Design Authority
- Persistent MVCC v2
- Unified Atom Incidence
- .new
- hashing/mod.rs
- XorShift32HashFunction
- MetaGraphError
- laws.rs
- ferros-grynn-macros/src/lib.rs
- Hybrid Query Model
- Transaction
- Canonical Storage Value Codec
- P3 Query Execution
- DataFileId
- &'a Bytes
- .fmt
- Direct I/O and Clock-Pro
- Redacted External Error Sources
- StorageValue
- StatusCode
- Uuid
- MaterializedGraph
- execution.rs
- TxHandle
- live.rs
- datastore.rs
- EdgeEndpoint
- GraphStorageMode
- ParserSettings
- Lsn
- ir.rs
- exact_ops.rs
- AtomId
- Vec
- Iter<'_, T>
- Self
- FerrosGrynnResult
- FerrosGrynnResult
- Option
- Q: What is MetaGraph?
- TableField
- Handle<'_, T>
- Отчёт этапа 2: Sync Direct I/O и BufferPool
- AtomId
- SnapshotId
- BufWriter
- Result
- GraphMetadata
- FerrosGrynnResult
- Mutable
- Value
- LocalGraph
- Result
- Text
- read.rs
- SlotAllocator
- AtomIds
- MaterializationOrigin
- AsyncUringEngine
- measure_fresh_write
- Iter
- CheckpointAuthority
- GraphWorkspace<G>
- conversion_error
- AtomKind
- StorageDuration
- BufRing
- Iter<'a, T>
- LocationError
- Graph
- .into_iter
- DecodeContext
- StorageValueRef
- .fmt
- Formatted
- PageRead
- IntoText
- DataFileRegistration
- .new
- CowVec
- D
- Default
- Err
- DirtyTarget
- Display
- FromStr
- S
- Metagraph<State>
- SystemTime
- SyncPageManager
- VecDeque
- BufferPool
- IntoIterator
- EdgeView
- File
- TxId
- ExactSizeIterator
- Formatter
- From
- Graph
- GraphMut
- GraphRefReader
- Mutable
- NodeView
- PropertyKeyId
- Range
- RootedTree
- AtomRef
- EdgeRead
- GraphKind
- GraphRead
- GraphRef
- GraphStorageMode
- MaterializedGraph
- NodeRead
- Path
- SnapshotId
- AtomKind
- GraphKind
- Path
- Path
- DiskAtomRef
- DiskAtomRef
- EncodeContext
- Path
- AtomId
- AtomKind
- DiskAtomRef
- EncodeContext
- GraphKind
- GraphRead
- GraphRef
- GraphStorageMode
- MaterializedGraph
- NodeRead
- .new
- Path
- SnapshotId
- Path
- AtomId
- AtomKind
- AtomId
- AtomId
- AtomKind
- SmallVec
- AtomId
- AtomKind
- AtomId
- AtomKind
- AtomId
- AtomKind
- AtomKind
- AtomRef
- AtomId
- AtomKind
- FusedIterator
- AtomId
- AtomKind
- AtomId
- AtomKind
- Path
- AtomId
- MapFn
- MutFn
- AtomKind
- AtomRef
- Bound
- Deref
- Target
- Tree
- TreeOrientation
- Value
- Weak

## God Nodes (most connected - your core abstractions)
1. `AtomId` - 322 edges
2. `MetaGraph` - 153 edges
3. `PageNo` - 112 edges
4. `AlignedPage` - 111 edges
5. `Value` - 87 edges
6. `Graph` - 79 edges
7. `Transaction` - 60 edges
8. `Frozen` - 55 edges
9. `MaterializedGraph` - 54 edges
10. `GraphRef` - 54 edges

## Surprising Connections (you probably didn't know these)
- `Public Graph Surface` --semantically_similar_to--> `Graph Storage Families`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/graphs.md
- `Claude Ferros-Grynn Guidance` --semantically_similar_to--> `Prototype-to-Production Migration Architecture`  [INFERRED] [semantically similar]
  CLAUDE.md → AGENTS.md
- `Value Runtime Invariants` --semantically_similar_to--> `Distinct None and Null Semantics`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/values_plan.md
- `Arrow Validity Plus Presence Bitmap` --semantically_similar_to--> `PropertyBatch Presence Model`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/values_plan.md
- `dag_chain()` --references--> `Dag`  [EXTRACTED]
  ferros-grynn-bench/benches/graph_core.rs → src/values/runtime/graph/dag.rs

## Import Cycles
- 1-file cycle: `src/values/runtime/text.rs -> src/values/runtime/text.rs`
- 1-file cycle: `src/values/runtime/graph/general.rs -> src/values/runtime/graph/general.rs`
- 1-file cycle: `src/values/runtime/graph/subgraph.rs -> src/values/runtime/graph/subgraph.rs`
- 1-file cycle: `src/values/runtime/number.rs -> src/values/runtime/number.rs`
- 1-file cycle: `src/values/runtime/set.rs -> src/values/runtime/set.rs`
- 1-file cycle: `src/common/memory/measurable.rs -> src/common/memory/measurable.rs`
- 1-file cycle: `src/values/runtime/regex.rs -> src/values/runtime/regex.rs`

## Hyperedges (group relationships)
- **GrynQL Contract Stack** — docs_gql_spec_grynql_language, docs_gql_grammar_grynql_grammar, docs_gql_tech_spec_query_engine [INFERRED 0.95]
- **Storage Persistence Contract** — docs_disk_storage_spec_frame_wal, docs_disk_storage_spec_checkpoint_recovery, docs_gql_tech_spec_page_manager, crates_fg_meta_implementation_notes_persistent_mvcc [INFERRED 0.85]
- **Values Runtime Storage Separation** — docs_values_plan_runtime_storage_boundary, docs_values_plan_storage_value, src_values_agents_runtime_invariants, src_values_agents_presence_bitmap [INFERRED 0.95]

## Communities (291 total, 110 thin omitted)

### Community 0 - "Parser"
Cohesion: 0.17
Nodes (13): Identifier, cover(), Symbol, Token, Parser, FerrosGrynnError, FerrosGrynnResult, Option (+5 more)

### Community 1 - "FerrosGrynnResult"
Cohesion: 0.10
Nodes (33): build_parent_map(), build_storage(), constraint(), find_root(), Forest<Mutable>, forest_node_removal_marks_every_incident_edge_dirty(), forest_rejects_cycles_without_mutation(), ForestStorage (+25 more)

### Community 2 - "Dag<State>"
Cohesion: 0.05
Nodes (50): build_oriented_tree_storage(), build_storage(), constraint(), Dag<Frozen>, Dag<Mutable>, Dag<State>, DagStorage, frozen_layout_round_trips_to_mutable_adjacency() (+42 more)

### Community 3 - "Hypergraph<State>"
Cohesion: 0.05
Nodes (46): build_storage(), constraint(), graph_conversion_rejects_edge_endpoints(), Hypergraph<Frozen>, Hypergraph<Mutable>, Hypergraph<State>, HyperStorage, permits_a_vertex_in_both_directed_incidence_roles() (+38 more)

### Community 4 - "MetaGraph"
Cohesion: 0.07
Nodes (19): MetaGraph, AtomId, AtomKind, Box, Default, EdgeTopology, GraphId, Item (+11 more)

### Community 5 - "Values"
Cohesion: 0.13
Nodes (8): marks_materialized_result_without_new_storage_family(), Clone, R, Self, State, Subgraph, Subgraph<Frozen>, Values

### Community 6 - "catalog.rs"
Cohesion: 0.08
Nodes (65): atom_kind_byte(), bootstrap_catalogs_round_trip_through_direct_file(), BootstrapCatalogPages, catalog_page_rejects_non_zero_reserved_bytes(), catalog_update_root_container_preserves_aligned_pages(), CatalogDecoder, CatalogDecoder<'a>, CatalogKind (+57 more)

### Community 7 - "MvccTransaction"
Cohesion: 0.06
Nodes (54): AtomMeta, basic_generic_rw(), BytesCodec, closure_codec_with_capture(), ClosureCodec, ClosureCodec<T>, Codec, exact_phantom_label() (+46 more)

### Community 8 - "AtomId"
Cohesion: 0.07
Nodes (32): bfs_spanning_tree(), BinaryLiftingTable, Condensation, DagSubGraph, extract_dag(), extract_forest(), extract_hyper(), extract_tree() (+24 more)

### Community 9 - "AtomId"
Cohesion: 0.05
Nodes (23): AtomId, AtomKind, EdgeKindMarker, GraphId, GraphKindMarker, kind_bit_encoding(), NodeKindMarker, Debug (+15 more)

### Community 10 - "PageId"
Cohesion: 0.09
Nodes (24): backend(), BufferPool, clock_pro_scan_resistance(), ClockProInner, dirty_flushed_on_eviction(), eviction_produces_ghost(), Frame, FrameStatus (+16 more)

### Community 11 - "PageNo"
Cohesion: 0.11
Nodes (77): DecodeLimits, EdgeSlotFlags, NodeSlotFlags, adjacency_overflow_codecs_preserve_order(), decode_adjacency_overflow(), decode_cross_level_overflow(), decode_freelist(), decode_graph_statistics() (+69 more)

### Community 12 - "Graph<State>"
Cohesion: 0.10
Nodes (16): Copied, Graph<State>, Debug, Eq, Formatter, H, Hash, Item (+8 more)

### Community 13 - ".new"
Cohesion: 0.28
Nodes (33): checkpoint_end_layout_is_fixed_without_dirty_page_table(), checkpoint_end_requires_begin_and_durable_target(), commit_during_backfill_remains_for_next_checkpoint(), committed_frame_advances_file_instance_clock(), committed_frames_preserve_snapshot_versions_across_reopen(), concurrent_commits_publish_monotonic_clocks(), graph_superblock_is_not_a_transaction_frame(), marked_page() (+25 more)

### Community 14 - "FerrosGrynnResult"
Cohesion: 0.16
Nodes (11): Decoder<'a>, invalid(), limit_error(), Box, FerrosGrynnError, FerrosGrynnResult, Into, String (+3 more)

### Community 15 - "path.rs"
Cohesion: 0.12
Nodes (23): EndpointRole, PathDirection, PathStep, PathTraceArena, PathTraceId, PathTraceNode, resolve_atom(), resolve_edge() (+15 more)

### Community 16 - "codec.rs"
Cohesion: 0.11
Nodes (43): AlignedVec, Position, all_format_v1_tags_round_trip(), archive_envelope_checks_magic_version_schema_length_and_checksum(), ArchiveEnvelope, assert_status(), canonical_f32(), canonical_float_encoding_rejects_signed_zero_and_nan_payloads() (+35 more)

### Community 17 - "win_async.rs"
Cohesion: 0.08
Nodes (39): cpu_affinity_cpu0(), goto_shutdown(), ioring_handle_ref(), ioring_raw_buffer_ref(), ioring_registered_buffer_ref(), new_fails_gracefully_if_no_ioring(), PendOp, probe_ioring() (+31 more)

### Community 18 - "Number"
Cohesion: 0.08
Nodes (29): BigInt, canonical_float(), canonical_fraction(), canonicalizes_nan_and_signed_zero_for_eq_ord_and_hash(), CanonicalNumber, conversion_error(), Decimal, f32 (+21 more)

### Community 19 - "src/ecs.rs"
Cohesion: 0.07
Nodes (34): AnyPool, ColorTag, FeatureVector, node_id(), NodeWeight, PageRank, Position2D, Position3D (+26 more)

### Community 20 - "wal.rs"
Cohesion: 0.08
Nodes (60): BufWriter, File, ActiveSegment, append_raw_commit(), append_raw_frame(), decode_checkpoint_end(), decode_page_init(), decode_page_key() (+52 more)

### Community 21 - "StatusObject"
Cohesion: 0.11
Nodes (17): DiagnosticRecord, Severity, Condition, Domain, ErrorCategory, ErrorClassification, NotificationClassification, From (+9 more)

### Community 22 - "Subgraph<State>"
Cohesion: 0.12
Nodes (15): Debug, Edge, Eq, Formatter, H, Hash, Node, Option (+7 more)

### Community 23 - "store.rs"
Cohesion: 0.10
Nodes (31): auto_checkpoint_triggers(), checkpoint_clears_wal(), compress_payload(), crc32(), crc32_update(), create_and_reopen(), decompress_payload(), delete_graph() (+23 more)

### Community 24 - "status_codes.rs"
Cohesion: 0.09
Nodes (29): define_status_codes(), is_string(), is_vec_string(), last_path_ident(), option_lit(), Ident, Option, Parse (+21 more)

### Community 25 - "PropertyKeyId"
Cohesion: 0.05
Nodes (41): &'a PropertyMap, clone_is_independent(), empty_map_is_pointer_sized_and_does_not_reserve_storage(), entry_ref(), equality_order_and_hash_ignore_insertion_order(), from_iterator_uses_last_duplicate_value(), keeps_entries_sorted_and_replaces_existing_values(), PropertyIter (+33 more)

### Community 26 - "EdgeTopology"
Cohesion: 0.09
Nodes (23): INLINE_ENDPOINTS, distinguishes_binary_hyper_and_cross_level_topology(), EdgeDirection, EdgeEndpoint, EdgeTopology, QualifiedAtomId, AtomId, Box (+15 more)

### Community 27 - "AtomKind"
Cohesion: 0.09
Nodes (11): atom_id_uses_u40_slots_and_u23_generations(), AtomKind, CowVec, Display, FerrosGrynnResult, Formatter, Option, Result (+3 more)

### Community 28 - "Value"
Cohesion: 0.08
Nodes (26): Closure, atom_and_path_have_distinct_value_contracts(), display_is_deterministic_and_escapes_strings(), eq_ord_and_hash_agree_for_cross_number_values(), graph_transport_preserves_exact_type_and_typed_conversion(), reports_concrete_kind_and_static_type(), AtomRef, Box (+18 more)

### Community 29 - "Geometry"
Cohesion: 0.07
Nodes (35): GeoJsonGeometry, GeoJsonPosition, assert_status(), compare_float_slices(), GeoJsonValue, Geometry, into_position(), into_positions() (+27 more)

### Community 30 - "GraphRef"
Cohesion: 0.12
Nodes (7): ensure_store_reference(), Uuid, TransactionGraphSource, TransactionGraphView, GraphKind, GraphRead, GraphRef

### Community 31 - "src/traversal.rs"
Cohesion: 0.10
Nodes (42): BellmanFordResult, bellman_ford(), BellmanFordError, bfs(), bfs_with(), dfs(), dfs_full(), dijkstra() (+34 more)

### Community 32 - ".new"
Cohesion: 0.16
Nodes (27): Fields, IntoText, atom_versions_keep_reused_slot_generations_distinct(), configured_transaction_limits_have_domain_statuses(), Datastore, disjoint_atom_writes_merge(), graph_id(), graph_removal_preserves_table_catalog_integrity() (+19 more)

### Community 33 - "Result"
Cohesion: 0.09
Nodes (27): AtomRef, bool, Closure, Graph<Frozen>, MaterializedGraph, Number, Path, RecordId (+19 more)

### Community 34 - "Text"
Cohesion: 0.08
Nodes (25): Borrow, Extend, CompactString, exposes_controlled_utf8_mutation(), round_trips_through_std_and_compact_strings(), AsRef, Deref, Display (+17 more)

### Community 35 - "FerrosGrynnError"
Cohesion: 0.09
Nodes (23): ErrorState, FerrosGrynnError, NonErrorStatus, Arc, Debug, Display, E, Error (+15 more)

### Community 36 - "DiagnosticRecord"
Cohesion: 0.10
Nodes (19): ArcStr, NonZeroU32, computes_unicode_locations_without_retaining_source(), DiagnosticRecord, ExecutionPhase, LabeledLocation, line_column_at(), LineColumn (+11 more)

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
Cohesion: 0.15
Nodes (19): And, Callable, Comma, DisplayValue, Formatted<'_, F, T>, Identifier, Join<S, F>, JoinSeparator (+11 more)

### Community 43 - "PosixDevice"
Cohesion: 0.06
Nodes (31): open_read_write_sync(), Arc, BufferPool, DirectFile, File, Path, PathBuf, Result (+23 more)

### Community 44 - "Closure"
Cohesion: 0.07
Nodes (25): ClosureFunction, Func, NonZeroU64, calls_function_and_keeps_process_identity(), Closure, different_closures_have_different_identity(), rejects_wrong_arity_before_invocation(), Arc (+17 more)

### Community 45 - "DirectFile"
Cohesion: 0.06
Nodes (30): AtomicI64, AlignedBuffer, alloc_aligned(), DirectFile, free_aligned(), platform_grow(), platform_open(), platform_open_readonly() (+22 more)

### Community 46 - "benches/property_map.rs"
Cohesion: 0.10
Nodes (35): bench_build(), bench_iterate_and_clone(), bench_lookup(), boxed_enum_map(), BoxedEnumIter, BoxedEnumIter<'a>, BoxedEnumPropertyMap, BoxedEnumRepr (+27 more)

### Community 47 - "async_io.rs"
Cohesion: 0.14
Nodes (20): AsyncIoEngine, build(), build_ring(), bump_memlock(), create_engine(), create_engine_full(), EngineConfig, FileSlot (+12 more)

### Community 48 - "PageLsn"
Cohesion: 0.10
Nodes (23): PageFlags, aligned_page_has_exact_size_and_alignment(), aligned_page_slice_is_one_contiguous_byte_extent(), mutable_validation_restores_persisted_checksum(), page_checksum(), page_checksum_mut(), page_checksum_preserving(), page_checksum_rejects_payload_corruption() (+15 more)

### Community 49 - "arrow.rs"
Cohesion: 0.11
Nodes (36): ArrayRef, ArrowError, DataType, arrow_failure(), arrow_round_trip_preserves_none_null_and_native_scalars(), build_array(), ColumnKind, decode_rows() (+28 more)

### Community 50 - "StatusReport"
Cohesion: 0.11
Nodes (20): IntoSO, applies_gql_precedence(), Outcome, Outcome<T>, precedence(), rejects_errors(), Display, Error (+12 more)

### Community 51 - "Lsn"
Cohesion: 0.08
Nodes (47): BufReader, CheckpointHeader, decode_atom_meta_payload(), decode_entry(), decode_isolation(), encode_atom_meta(), encode_entry(), encode_isolation() (+39 more)

### Community 52 - "Vector"
Cohesion: 0.10
Nodes (20): canonical_f32(), canonical_f64(), compare_f32(), compare_f32_value(), compare_f64(), compare_f64_value(), hash_f32(), hash_f64() (+12 more)

### Community 53 - "storage/buffer_pool.rs"
Cohesion: 0.08
Nodes (57): AtomicUsize, CachedPage, Condvar, Loader, AccessDecision, batch_handles_resident_loading_missing_and_duplicate_pages(), batch_preserves_order_duplicates_and_contiguous_extents(), BatchLoadReservation (+49 more)

### Community 54 - "Range"
Cohesion: 0.14
Nodes (26): End, borrow_bound(), Bound<T>, bound_value(), BoundExt, box_bound(), checks_inclusion(), compare_end() (+18 more)

### Community 55 - "superblock.rs"
Cohesion: 0.15
Nodes (27): GraphSuperblockFlags, database_fixture(), database_superblock_round_trip_matches_layout(), DatabaseSuperblock, graph_fixture(), graph_superblock_round_trip_matches_layout(), GraphSuperblock, invalid_superblock() (+19 more)

### Community 56 - "storage/direct.rs"
Cohesion: 0.11
Nodes (49): OpenOptions, checked_file_length(), checked_page_extent(), checked_page_offset(), configure_direct_io(), create_new_never_overwrites_existing_file(), direct_file_batch_rejects_range_and_page_corruption(), direct_file_reads_and_validates_one_contiguous_extent() (+41 more)

### Community 57 - "graph/traversal.rs"
Cohesion: 0.06
Nodes (67): DoubleEndedIterator, algorithm_failure(), bellman_ford(), bellman_ford_reports_negative_cycle_as_domain_error(), bfs(), bfs_topology_and_components_are_correct(), binary_edges(), BinaryEdge (+59 more)

### Community 58 - "benchmarks.rs"
Cohesion: 0.15
Nodes (22): aggregate_read_samples(), apply_gate(), BackendResult, BatchCalibration, CalibrationResult, comparison(), latency_ratio(), measure_order() (+14 more)

### Community 59 - "heap_estimator.rs"
Cohesion: 0.07
Nodes (21): HeapEstimatorCache, NoHeapEstimatorCache, Self, Measurable, HashMap, K, V, size_of_hash_map() (+13 more)

### Community 61 - "Type"
Cohesion: 0.09
Nodes (27): is_option(), analyze(), Analyzer, Analyzer<'a>, compatible(), is_integer(), is_number(), Box (+19 more)

### Community 62 - "Regex"
Cohesion: 0.09
Nodes (19): compiles_and_matches_pattern(), Regex, Display, Eq, Err, Error, Formatter, FromStr (+11 more)

### Community 63 - "Set"
Cohesion: 0.12
Nodes (15): &'a Set, BTreeSet<Value>, deduplicates_and_orders_values(), BTreeSet, From, FromIterator, I, IntoIter (+7 more)

### Community 64 - "AsyncIo"
Cohesion: 0.14
Nodes (17): async_io_batch_tag_routing(), async_io_write_read_sync(), AsyncIo, AsyncIoConfig, EngineConfig, Arc, Box, Default (+9 more)

### Community 65 - "AdjacencyMatrix"
Cohesion: 0.15
Nodes (13): AdjacencyMatrix, Incidence, IncidenceMatrix, IncidenceTensor, MetaAdjacencyMatrix, MetaTriple, AtomId, FxHashMap (+5 more)

### Community 66 - "Result"
Cohesion: 0.11
Nodes (21): atom_id_from(), BytesPersistentStore, checkpoint_then_recover(), closure_codec_with_capture_in_persistent_store(), crc32(), PersistentMvccStore, PersistentMvccStore<T, C>, PersistLayer (+13 more)

### Community 67 - "Object"
Cohesion: 0.06
Nodes (35): batch_random_access(), bench_collect(), bench_iter_sequential_borrowed(), bench_iter_sequential_owned(), bench_iter_sequential_with_step(), bench_push_preallocated(), bench_push_raw(), LargeData (+27 more)

### Community 68 - "get_metadata_inner"
Cohesion: 0.10
Nodes (23): add_m, Attrs, measurable_inner(), occurrence_error(), add, DeriveInput, Error, Parse (+15 more)

### Community 69 - "StorageBackend"
Cohesion: 0.18
Nodes (14): AsyncIoRingEngine, Arc, extract_fd(), extract_handle(), extract_path(), RawFdBackend, RawHandleBackend, HANDLE (+6 more)

### Community 70 - ".run"
Cohesion: 0.25
Nodes (14): decodes_quoted_names_parameters_and_string_escapes(), is_ident_continue(), is_ident_start(), keeps_utf8_byte_spans_and_greedy_operators(), lex(), Lexer, parse_error(), Box (+6 more)

### Community 71 - "PropertyBatch"
Cohesion: 0.20
Nodes (11): BooleanBuffer, imported_arrow_batch_requires_property_metadata(), PropertyBatch, BTreeMap, FerrosGrynnResult, Option, PropertyMap, RecordBatch (+3 more)

### Community 72 - "FerrosGrynnResult"
Cohesion: 0.07
Nodes (75): DataFileKind, GraphCatalogEntry, Lsn, allocate_file_id(), allocate_graph_id(), build_graph_image(), BuiltGraphGeneration, BuiltGraphImage (+67 more)

### Community 73 - "HashValue"
Cohesion: 0.12
Nodes (14): AddAssign, ConstOne, Mul, MulAssign, One, HashValue, Add, Display (+6 more)

### Community 74 - "WorldComponents"
Cohesion: 0.09
Nodes (22): AnyPool, atom(), remove_atom_cleans_every_pool_and_respects_generation(), Any, AtomId, Box, FerrosGrynnResult, FnMut (+14 more)

### Community 75 - ".new"
Cohesion: 0.25
Nodes (7): IoBatch, IoRequest, Self, Vec, tag_routing(), threadpool_write_read(), Vec

### Community 76 - "Self"
Cohesion: 0.12
Nodes (16): DagView, ForestView, GraphView, Dag<State>, DagView<'graph, R>, directed_forest_view(), Forest<State>, RootedTree<State> (+8 more)

### Community 77 - "graph_index.rs"
Cohesion: 0.17
Nodes (42): BTreeSet, build_primary_index(), BuiltPrimaryIndex, bulk_primary_tree_supports_multiple_levels_and_exact_lookup(), ChildPage, decode_dictionary_chain(), decode_label_dictionary(), encode_dictionary_entry_page() (+34 more)

### Community 78 - "StatusWireV1"
Cohesion: 0.21
Nodes (17): completion_omits_severity_and_classification(), DiagnosticRecordWireV1, FerrosGrynnError, labeled_location_to_wire(), LabeledLocationWireV1, location_to_wire(), LocationWireV1, Box (+9 more)

### Community 79 - "Path"
Cohesion: 0.10
Nodes (17): AsRef, Self, Path, Arc, Eq, ExactSizeIterator, H, Hash (+9 more)

### Community 80 - "PropertyDictionary"
Cohesion: 0.15
Nodes (12): assigns_dense_ids_and_resolves_both_directions(), interning_is_idempotent_and_iteration_uses_id_order(), PropertyDictionary, ExactSizeIterator, FerrosGrynnResult, IntoText, Item, Option (+4 more)

### Community 81 - "local.rs"
Cohesion: 0.16
Nodes (38): Decode, KeyOf, atomic_replace(), Candidate, corrupt_temporary_is_removed_after_main_selection(), database(), database_lock_is_exclusive_for_its_lifetime(), DatabaseLock (+30 more)

### Community 82 - "DiagnosticValue"
Cohesion: 0.14
Nodes (11): Str, bool, DiagnosticValue, f32, f64, BTreeMap, String, StatusParameter (+3 more)

### Community 84 - "AccumList"
Cohesion: 0.15
Nodes (15): AccumList, FieldAccumArgs, Attribute, DeriveInput, Ident, Option, Parse, ParseStream (+7 more)

### Community 86 - "Bytes"
Cohesion: 0.18
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

### Community 90 - "CreatedDatabaseDirectory"
Cohesion: 0.28
Nodes (4): CreatedDatabaseDirectory, Drop, PathBuf, TestDatabasePath

### Community 91 - "Graphify"
Cohesion: 0.15
Nodes (13): Add URL and Watch Folder, Graphify Extra Exports, Semantic Extraction Contract, GitHub Clone and Cross-Repo Merge, Graphify Commit and Claude Hooks, Graph Query Path and Explain, Video and Audio Transcription, Incremental Graph Update (+5 more)

### Community 92 - "PropertyMap"
Cohesion: 0.23
Nodes (7): bench_search(), Property, PropertyMap, Criterion, Self, String, Vec

### Community 93 - "wy_hash.rs"
Cohesion: 0.29
Nodes (7): i64_le(), u32(), u64_rotate32(), wy_hash64(), WyHash, wymum(), wyr3()

### Community 94 - "EdgeInfo"
Cohesion: 0.22
Nodes (10): EdgeIncidence, EdgeInfo, EdgeRole, EdgeTopology, AtomId, Box, GraphId, Option (+2 more)

### Community 95 - "Error"
Cohesion: 0.27
Nodes (21): Mmap, aligned_pages(), benchmark_page(), BenchmarkConfig, calibrate_batch_size(), consume_mmap(), consume_pool(), consume_raw_direct() (+13 more)

### Community 96 - "TextSpan"
Cohesion: 0.28
Nodes (20): TextSpan, Expr, ExprKind, Identifier, LetStatement, Literal, ObjectField, Program (+12 more)

### Community 97 - "HashFunction"
Cohesion: 0.21
Nodes (3): IncrementalXXH64, HashFunction, UtilHash

### Community 98 - ".new"
Cohesion: 0.29
Nodes (23): cataloged_database_create_reopen_and_exclusive_create(), concurrent_checkpoints_publish_once(), local_io(), marked_page(), mixed_batch_preserves_sources_order_and_duplicates(), mixed_batch_rejects_more_unique_pages_than_pool_capacity(), old_reader_survives_checkpoint_and_next_checkpoint_advances(), open_rejects_file_instance_clock_reuse() (+15 more)

### Community 99 - "CowVec<T, CHUNK_LEN>"
Cohesion: 0.16
Nodes (10): CowVec<T, CHUNK_LEN>, Default, FnMut, From, Index, IndexMut, Option, Output (+2 more)

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

### Community 109 - "Array"
Cohesion: 0.12
Nodes (13): Array, From, FromIterator, I, IntoValue, IterMut, Option, Self (+5 more)

### Community 110 - "AtomId"
Cohesion: 0.10
Nodes (15): AtomId, Debug, EdgeColumns, empty_topology(), ensure_unique_record(), NodeColumns, CowVec, Default (+7 more)

### Community 111 - "NodeInfo"
Cohesion: 0.18
Nodes (7): NodeInfo, NodeKind, AtomId, Box, GraphId, Option, PropertyMap

### Community 112 - "Specification Design Authority"
Cohesion: 0.33
Nodes (6): Specification Design Authority, AtomWalk Syntax, MATCH Graph Patterns, GrynQL EBNF Grammar, Context-Sensitive Parser Disambiguation, GrynQL Statements

### Community 113 - "Persistent MVCC v2"
Cohesion: 0.33
Nodes (6): ClosureCodec, Exact Phantom Matching, Persistent MVCC v2, Serializable Snapshot Isolation, GrynQL Transaction Model, Execution MVCC and Snapshot Contract

### Community 114 - "Unified Atom Incidence"
Cohesion: 0.33
Nodes (6): Unified Atom Incidence, Graph Storage Families, Ferros-Grynn Graph Taxonomy, Mutable Frozen Type-State and Views, P4 Full Atom Incidence, GraphWorkspace ECS Sidecar

### Community 116 - "hashing/mod.rs"
Cohesion: 0.33
Nodes (3): Option, Ordering, Hashable

### Community 118 - "MetaGraphError"
Cohesion: 0.40
Nodes (5): MetaGraphError, AtomId, AtomKind, Error, GraphId

### Community 119 - "laws.rs"
Cohesion: 0.53
Nodes (5): hash(), number(), Value, scalar_value(), Strategy

### Community 120 - "ferros-grynn-macros/src/lib.rs"
Cohesion: 0.70
Nodes (4): define_status_codes(), measurable(), TokenStream, structure_accumulator()

### Community 121 - "Hybrid Query Model"
Cohesion: 0.50
Nodes (5): Hybrid Query Model, MATCH Pattern Matching, StandardPath and AtomWalk Semantics, Traversal Pipeline, Ferros-Grynn Graph Engine

### Community 122 - "Transaction"
Cohesion: 0.11
Nodes (22): DatabaseRoot, GraphWrite, materialize_atom(), materialize_rows(), PredicateEvaluator, PredicateMatcher, PredicateRead, QuerySnapshot (+14 more)

### Community 123 - "Canonical Storage Value Codec"
Cohesion: 0.50
Nodes (4): Canonical Storage Value Codec, OLTP Property Heap and Column Cache, Runtime Storage Columnar Boundary, StorageValue and Canonical Codec

### Community 124 - "P3 Query Execution"
Cohesion: 0.50
Nodes (4): P3 Query Execution, P2 Query Frontend, P2 Storage WAL and MVCC, P1 Value Foundation

### Community 125 - "DataFileId"
Cohesion: 0.09
Nodes (16): benchmark_key(), benchmark_lsn(), FerrosGrynnResult, validate_benchmark_version(), CommitTs, data_file_id_layout_is_stable_and_validated(), DataFileId, DataFileKind (+8 more)

### Community 126 - "&'a Bytes"
Cohesion: 0.50
Nodes (3): &'a Bytes, IntoIter, IntoIterator

### Community 135 - "StorageValue"
Cohesion: 0.07
Nodes (47): Bound, Bytes, DateTime, Decimal, Geometry, array_storage_conversion_reuses_owned_elements(), conversion_context_rejects_conflicting_reference_mappings(), datetime_conversion_preserves_pre_epoch_nanos_and_time_zone_kind() (+39 more)

### Community 136 - "StatusCode"
Cohesion: 0.14
Nodes (13): D, Deserialize, Err, FromStr, Ok, Serialize, InvalidSeverity, Display (+5 more)

### Community 137 - "Uuid"
Cohesion: 0.07
Nodes (36): JsonValue, InnerUlid, InnerUuid, Display, Err, Formatter, From, FromStr (+28 more)

### Community 138 - "MaterializedGraph"
Cohesion: 0.09
Nodes (19): MaterializedGraph, preserves_exact_discriminant_in_generic_transport(), Cow, Edge, EdgeIds, Eq, FerrosGrynnResult, H (+11 more)

### Community 139 - "execution.rs"
Cohesion: 0.09
Nodes (54): QueryCatalog, arithmetic(), binary(), binds_let_filters_projects_and_limits(), build_cursor(), cast(), decimal_arithmetic(), eval() (+46 more)

### Community 140 - "TxHandle"
Cohesion: 0.10
Nodes (18): AtomicBool, RwLockReadGuard, ActiveSnapshot, encode_page_init(), FrameWal, ResolvedFrame, Arc, Drop (+10 more)

### Community 141 - "live.rs"
Cohesion: 0.07
Nodes (53): ApplyFn, ApplyUpdate, MutexGuard, ReadFn, ReplayFn, accept_remote_discards_local_conflicting_state(), apply_or_pause(), conflicted_live_graph() (+45 more)

### Community 142 - "datastore.rs"
Cohesion: 0.07
Nodes (33): ActiveSnapshot, ActiveSnapshot, AtomVersions, CheckpointMode, CheckpointPolicy, CommitResult, DatastoreOptions, InMemoryOptions (+25 more)

### Community 145 - "ParserSettings"
Cohesion: 0.11
Nodes (19): Config, Default, Self, parse(), QueryResult, QueryStatementKind, FerrosGrynnResult, Value (+11 more)

### Community 147 - "Lsn"
Cohesion: 0.17
Nodes (10): CommitTs, checkpoint_prunes_closed_prefix_and_seeded_recovery_continues_clocks(), CheckpointCommit, CheckpointEnd, CheckpointTicket, CommitOutcome, RecoverySeed, CheckpointGeneration (+2 more)

### Community 148 - "ir.rs"
Cohesion: 0.21
Nodes (15): Column, LogicalNode, LogicalNodeKind, LogicalProgram, LogicalSelect, LogicalStatement, NodeId, Box (+7 more)

### Community 149 - "exact_ops.rs"
Cohesion: 0.17
Nodes (19): CheckedAdd, CheckedMul, CheckedSub, PrimInt, Signed, integer_arithmetic(), add_exact(), AddExact (+11 more)

### Community 151 - "Vec"
Cohesion: 0.26
Nodes (8): Object, ParserSettings, QueryResult, all_dirty(), build_record_index(), EdgeIds, NodeIds, Vec

### Community 152 - "Iter<'_, T>"
Cohesion: 0.67
Nodes (3): Iter<'_, T>, ExactSizeIterator, FusedIterator

### Community 153 - "Self"
Cohesion: 0.10
Nodes (9): DiagnosticValue, ExecutionPhase, LabeledLocation, Location, S, BTreeMap, Self, Status (+1 more)

### Community 154 - "FerrosGrynnResult"
Cohesion: 0.12
Nodes (20): atom_label_mut(), atom_properties_mut(), AtomRefTopology, directed_pair(), forest_pair(), GraphMutationView, merge_materialized(), MutableGraphValue (+12 more)

### Community 155 - "FerrosGrynnResult"
Cohesion: 0.13
Nodes (11): cascade_removal_marks_removed_incident_edges_dirty(), DiskGraphView<'_, R>, GraphReadView<'_, R>, query_to_memory(), Edge, EdgeIds, FerrosGrynnResult, Node (+3 more)

### Community 156 - "Option"
Cohesion: 0.12
Nodes (11): EdgeRead, EdgeView, EdgeView<'a>, NodeRead, NodeView, Cow, EdgeTopology, Number (+3 more)

### Community 157 - "Q: What is MetaGraph?"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: What is MetaGraph?, Source Nodes

### Community 158 - "TableField"
Cohesion: 0.32
Nodes (4): ExactSizeIterator, Item, Type, TableField

### Community 159 - "Handle<'_, T>"
Cohesion: 0.50
Nodes (3): Handle<'_, T>, Deref, Target

### Community 160 - "Отчёт этапа 2: Sync Direct I/O и BufferPool"
Cohesion: 0.12
Nodes (14): Итоговая архитектура, Контракты и публичный API, Локальное дисковое хранилище Ferros-Grynn: sync-first, Проверки и критерии приёмки, Этапы реализации, Явно вне начального scope, Исправленный benchmark, Калибровка batch (+6 more)

### Community 162 - "SnapshotId"
Cohesion: 0.14
Nodes (4): Display, Formatter, Result, SnapshotId

### Community 164 - "Result"
Cohesion: 0.17
Nodes (21): MmapMut, batch_selection_uses_smallest_candidate_within_limits(), buffered_read_at(), buffered_read_exact_at(), buffered_write_all_at(), buffered_write_at(), BufferedPageSource, checked_byte_length() (+13 more)

### Community 165 - "GraphMetadata"
Cohesion: 0.09
Nodes (9): Vec, Vec, DirtyTarget, GraphMetadata, BTreeSet, ExactSizeIterator, Item, Self (+1 more)

### Community 166 - "FerrosGrynnResult"
Cohesion: 0.15
Nodes (9): Cow, Default, EdgeIds, FerrosGrynnResult, GWB, NodeIds, PropertyMap, Text (+1 more)

### Community 167 - "Mutable"
Cohesion: 0.07
Nodes (33): Dag, Forest, Hypergraph, Read, GraphWriteView, Dag, PhantomData, Forest (+25 more)

### Community 168 - "Value"
Cohesion: 0.17
Nodes (8): Box, From, FxHashMap, Option, Self, String, Vec, Value

### Community 169 - "LocalGraph"
Cohesion: 0.08
Nodes (46): TableKind, collect_local_ids(), copy_local_page(), decode_label(), decode_local_topology(), decode_record_key(), DecodedEdge, DecodedNode (+38 more)

### Community 170 - "Result"
Cohesion: 0.22
Nodes (12): BatchEntry, blocking_op(), FileOp, IoCompletion, IoOp, Mem, Pending, Any (+4 more)

### Community 171 - "Text"
Cohesion: 0.73
Nodes (3): ExactSizeIterator, Item, Text

### Community 172 - "read.rs"
Cohesion: 0.16
Nodes (15): assert_snapshot_released(), disk_view_and_hybrid_materialization_have_the_same_read_result(), DiskGraphView, FakeTransaction, GraphReadView, GraphReadView<'tx, R>, GraphRefReader, GraphWriteBack (+7 more)

### Community 174 - "AtomIds"
Cohesion: 0.16
Nodes (10): FusedIterator, Inner, AtomIds, AtomIds<'a>, FallibleAtomIds, FallibleAtomIds<Inner>, ExactSizeIterator, Item (+2 more)

### Community 175 - "MaterializationOrigin"
Cohesion: 0.19
Nodes (4): MaterializationOrigin, mutable_hybrid_flush_uses_origin_mapping_and_dirty_set(), BTreeMap, Uuid

### Community 176 - "AsyncUringEngine"
Cohesion: 0.14
Nodes (12): AsyncUringEngine, bg_loop(), CtrlMsg, file_worker(), Drop, File, JoinHandle, PathBuf (+4 more)

### Community 177 - "measure_fresh_write"
Cohesion: 0.21
Nodes (9): Barrier, BenchPath, create_dataset_from_pages(), measure_fresh_write(), measure_writes(), Drop, Path, PathBuf (+1 more)

### Community 178 - "Iter"
Cohesion: 0.31
Nodes (5): clone_copies_only_the_changed_chunk(), CowVec, Iter, Arc, Vec

### Community 179 - "CheckpointAuthority"
Cohesion: 0.33
Nodes (5): IntoIterator, CheckpointAuthority, GraphSuperblock, Item, RapidHashMap

### Community 180 - "GraphWorkspace<G>"
Cohesion: 0.07
Nodes (34): MapFn, MutFn, A, atom(), B, chooses_actual_smallest_pool_independent_of_type_order(), missing_pool_produces_empty_join(), AtomId (+26 more)

### Community 181 - "conversion_error"
Cohesion: 0.40
Nodes (11): calibration_page_count(), conversion_error(), random_order(), read_workloads(), ReadWorkload, NonZeroUsize, Vec, scan_hot_order() (+3 more)

### Community 183 - "StorageDuration"
Cohesion: 0.47
Nodes (3): Span, Duration, StorageDuration

### Community 185 - "Iter<'a, T>"
Cohesion: 0.40
Nodes (3): Iter<'a, T>, Item, Iterator

### Community 186 - "LocationError"
Cohesion: 0.33
Nodes (5): LocationError, Display, Error, Formatter, Result

### Community 187 - "Graph"
Cohesion: 0.06
Nodes (49): INLINE_INCIDENCE, QualifiedAtomId, SmallVec, atom_does_not_exist(), atom_kind_mismatch(), copy_from_read(), endpoint_kind(), freeze_thaw_preserves_identity_ids_and_topology() (+41 more)

### Community 188 - ".into_iter"
Cohesion: 0.38
Nodes (4): &'a Array, &'a mut Array, IntoIter, IntoIterator

### Community 190 - "StorageValueRef"
Cohesion: 0.40
Nodes (3): Deref, StorageValueRef, Target

### Community 192 - "Formatted"
Cohesion: 0.50
Nodes (5): Formatted, Join, F, PhantomData, S

### Community 193 - "PageRead"
Cohesion: 0.50
Nodes (3): collect_page_reads(), PageRead, Option

### Community 195 - "DataFileRegistration"
Cohesion: 0.18
Nodes (17): DataFileRegistration, DataFileRole, FileRegistry, open_registration(), RegisteredDataFile, Arc, DataFileId, DirectFile (+9 more)

### Community 196 - ".new"
Cohesion: 0.50
Nodes (3): Formatted<'a, F, T>, joins_zero_one_two_and_many_values(), Self

### Community 205 - "Metagraph<State>"
Cohesion: 0.05
Nodes (37): Metagraph, exposes_explicit_meta_atom_api_over_general_layout(), Metagraph<Frozen>, Metagraph<Mutable>, Metagraph<State>, Clone, Cow, Debug (+29 more)

### Community 207 - "SyncPageManager"
Cohesion: 0.12
Nodes (19): DatabaseLock, DatabaseSuperblock, aligned_extent(), recovery_seed(), Arc, AsRef, DataFileId, Duration (+11 more)

### Community 253 - ".new"
Cohesion: 0.12
Nodes (4): DiskGraphView<'tx, R>, NodeView<'a>, RecordId, Self

## Knowledge Gaps
- **70 isolated node(s):** `ResolvedAtom<Node, Edge>`, `AtomIds<'a>`, `DiskGraphView<'tx, R>`, `Dag<State>`, `super::Hypergraph<State>` (+65 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **110 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `AtomId` connect `AtomId` to `FerrosGrynnResult`, `Dag<State>`, `Hypergraph<State>`, `MaterializedGraph`, `Graph<State>`, `datastore.rs`, `Subgraph<State>`, `Vec`, `FerrosGrynnResult`, `AtomKind`, `FerrosGrynnResult`, `Option`, `.new`, `GraphMetadata`, `FerrosGrynnResult`, `Mutable`, `LocalGraph`, `read.rs`, `AtomIds`, `MaterializationOrigin`, `GraphWorkspace<G>`, `graph/traversal.rs`, `Graph`, `FerrosGrynnResult`, `Metagraph<State>`, `Transaction`, `.new`?**
  _High betweenness centrality (0.150) - this node is a cross-community bridge._
- **Why does `Graph` connect `Graph` to `Dag<State>`, `Hypergraph<State>`, `graph_core.rs`, `GraphMetadata`, `Values`, `Mutable`, `MaterializedGraph`, `read.rs`, `live.rs`, `Metagraph<State>`, `MaterializationOrigin`, `Value`, `FerrosGrynnResult`, `Option`?**
  _High betweenness centrality (0.139) - this node is a cross-community bridge._
- **Why does `MetaGraph` connect `MetaGraph` to `AdjacencyMatrix`, `Result`, `AtomId`, `AtomId`, `NodeInfo`, `.new`, `store.rs`, `bench_graph.rs`, `EdgeInfo`, `src/traversal.rs`?**
  _High betweenness centrality (0.089) - this node is a cross-community bridge._
- **What connects `ResolvedAtom<Node, Edge>`, `AtomIds<'a>`, `DiskGraphView<'tx, R>` to the rest of the system?**
  _70 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `FerrosGrynnResult` be split into smaller, more focused modules?**
  _Cohesion score 0.09525899912203688 - nodes in this community are weakly interconnected._
- **Should `Dag<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.05228442728442728 - nodes in this community are weakly interconnected._
- **Should `Hypergraph<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.05292929292929293 - nodes in this community are weakly interconnected._