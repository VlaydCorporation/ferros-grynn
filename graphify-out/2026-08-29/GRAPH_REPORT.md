# Graph Report - ferros-grynn  (2026-08-29)

## Corpus Check
- 172 files · ~303,170 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 6699 nodes · 20401 edges · 271 communities (195 shown, 76 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 192 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `626a7113`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- Parser
- AtomId
- atom_does_not_exist
- Hypergraph<State>
- MetaGraph
- Metagraph<State>
- catalog.rs
- MvccTransaction
- AtomId
- AtomId
- PageId
- PageNo
- TransactionGraphView
- page_io.rs
- FerrosGrynnResult
- Path
- storage/store.rs
- win_async.rs
- Number
- src/ecs.rs
- wal.rs
- StatusObject
- Subgraph<State>
- store.rs
- status_codes.rs
- PropertyMap
- EdgeEndpoint
- AtomId
- runtime/mod.rs
- Position
- Transaction
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
- FrameWal
- AlignedPage
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
- Type
- .expr
- Regex
- Set
- AsyncIo
- AdjacencyMatrix
- mvcc_persist.rs
- Object
- get_metadata_inner
- StorageBackend
- .run
- PropertyBatch
- RecordId
- HashValue
- WorldComponents
- .new
- Values
- graph_index.rs
- StatusWireV1
- .new
- PropertyDictionary
- Vec
- DiagnosticValue
- TraversalIndex
- AccumList
- graph_core.rs
- Bytes
- bench_graph.rs
- os.rs
- .register_datafusion_table
- .new
- Graphify
- PropertyMap
- wy_hash.rs
- BinaryLiftingIndex
- local.rs
- TextSpan
- HashFunction
- LocalGraphView
- LocalGraph
- Option
- build_from_map
- memohash.rs
- Ferros-Grynn Repository Guidance
- Physical Query Execution
- Values Module Guidance
- GrynQL Language Specification
- atoms
- helpers.rs
- Array
- Text
- fg-meta/src/lib.rs
- Specification Design Authority
- Persistent MVCC v2
- CowVec<T, CHUNK_LEN>
- GraphMetadata
- hashing/mod.rs
- XorShift32HashFunction
- MetaGraphError
- laws.rs
- ferros-grynn-macros/src/lib.rs
- Hybrid Query Model
- Arc
- .new
- P3 Query Execution
- PageKey
- &'a Bytes
- .fmt
- Direct I/O and Clock-Pro
- Redacted External Error Sources
- DiskAtomRef
- number.rs
- Uuid
- MaterializedGraph
- execution.rs
- Item
- live.rs
- datastore.rs
- Dag<State>
- Value
- ParserSettings
- RecordIdPart
- ir.rs
- .append
- AtomId
- .connect
- Iter<'_, T>
- exact_ops.rs
- FerrosGrynnResult
- .get
- Frozen
- Q: What is MetaGraph?
- FerrosGrynnResult
- Handle<'_, T>
- Отчёт этапа 5: Windows sync storage completion
- ValueKind
- .estimated_heap_bytes
- .open_cataloged
- Path
- SnapshotId
- TestDatabase
- memory.rs
- Value
- Lsn
- Result
- C
- read.rs
- collections.rs
- TableIndex
- src/id.rs
- AsyncUringEngine
- .open
- Option
- Iter
- GraphWorkspace<G>
- Q: Как изменения этапа 7 связывают лимит write-set, индекс RecordId и публикацию disk-backed local graph?
- PageRead
- SlotAllocator
- async_io.rs
- SyncPageManager
- Vec
- Graph
- AlignedBuffer
- .into_iter
- .into_iter
- property_page_round_trips_canonical_map
- Unified Atom Incidence
- write_sequence
- Self
- LargeData
- AtomIds
- Canonical Storage Value Codec
- BufRing
- .call
- .writeback_context
- .names
- ActiveSnapshot
- AtomicU64
- AtomRef
- IntoText
- MaterializedGraph
- .fmt
- .as_any
- .raw_fd
- Number
- Path
- PropertyMap
- RecordId
- String
- Text
- Value
- View
- File
- Path
- AsRef
- Into
- Path
- String
- AtomRef
- FnMut
- MaterializedGraph
- Number
- Path
- PropertyMap
- RecordId
- String
- Text
- Value
- Clone
- GWB
- Number
- PhantomData
- PropertyMap
- R
- RecordId
- SmallVec
- String
- Text
- Number
- PropertyMap
- RapidHashSet
- RecordId
- SlotAllocator
- Text
- AtomRef
- Bound
- MaterializedGraph
- Number
- Path
- Position
- RecordId
- String
- T
- Text
- TryFrom
- Graph<State>
- DoubleEndedIterator
- Entry
- FromIterator
- FusedIterator
- I
- IntoIter
- IntoValue
- SliceIter
- Value

## God Nodes (most connected - your core abstractions)
1. `MetaGraph` - 153 edges
2. `AlignedPage` - 102 edges
3. `Value` - 96 edges
4. `PageNo` - 96 edges
5. `Graph` - 88 edges
6. `PropertyMap` - 86 edges
7. `RecordId` - 74 edges
8. `Transaction` - 61 edges
9. `LocalGraph` - 61 edges
10. `PropertyKeyId` - 60 edges

## Surprising Connections (you probably didn't know these)
- `Public Graph Surface` --semantically_similar_to--> `Graph Storage Families`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/graphs.md
- `Claude Ferros-Grynn Guidance` --semantically_similar_to--> `Prototype-to-Production Migration Architecture`  [INFERRED] [semantically similar]
  CLAUDE.md → AGENTS.md
- `Value Runtime Invariants` --semantically_similar_to--> `Distinct None and Null Semantics`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/values_plan.md
- `Arrow Validity Plus Presence Bitmap` --semantically_similar_to--> `PropertyBatch Presence Model`  [INFERRED] [semantically similar]
  src/values/AGENTS.md → docs/values_plan.md
- `general_chain()` --references--> `Graph`  [EXTRACTED]
  ferros-grynn-bench/benches/graph_core.rs → src/values/runtime/graph/general.rs

## Import Cycles
- 1-file cycle: `src/values/runtime/graph/general.rs -> src/values/runtime/graph/general.rs`
- 1-file cycle: `src/values/runtime/mod.rs -> src/values/runtime/mod.rs`
- 1-file cycle: `src/values/runtime/graph/subgraph.rs -> src/values/runtime/graph/subgraph.rs`
- 1-file cycle: `src/values/runtime/number.rs -> src/values/runtime/number.rs`
- 1-file cycle: `src/values/runtime/set.rs -> src/values/runtime/set.rs`
- 1-file cycle: `src/common/memory/measurable.rs -> src/common/memory/measurable.rs`
- 1-file cycle: `src/values/runtime/text.rs -> src/values/runtime/text.rs`
- 1-file cycle: `src/storage/page_io.rs -> src/storage/page_io.rs`
- 1-file cycle: `src/values/runtime/regex.rs -> src/values/runtime/regex.rs`

## Hyperedges (group relationships)
- **GrynQL Contract Stack** — docs_gql_spec_grynql_language, docs_gql_grammar_grynql_grammar, docs_gql_tech_spec_query_engine [INFERRED 0.95]
- **Storage Persistence Contract** — docs_disk_storage_spec_frame_wal, docs_disk_storage_spec_checkpoint_recovery, docs_gql_tech_spec_page_manager, crates_fg_meta_implementation_notes_persistent_mvcc [INFERRED 0.85]
- **Values Runtime Storage Separation** — docs_values_plan_runtime_storage_boundary, docs_values_plan_storage_value, src_values_agents_runtime_invariants, src_values_agents_presence_bitmap [INFERRED 0.95]

## Communities (271 total, 76 thin omitted)

### Community 0 - "Parser"
Cohesion: 0.17
Nodes (13): Identifier, cover(), Symbol, Token, Parser, FerrosGrynnError, FerrosGrynnResult, Option (+5 more)

### Community 1 - "AtomId"
Cohesion: 0.06
Nodes (55): build_parent_map(), build_storage(), constraint(), find_root(), Forest, Forest<Frozen>, Forest<Mutable>, forest_node_removal_marks_every_incident_edge_dirty() (+47 more)

### Community 2 - "atom_does_not_exist"
Cohesion: 0.12
Nodes (19): Copied, INLINE_INCIDENCE, Iter, atom_does_not_exist(), atom_kind_mismatch(), endpoint_kind(), GeneralStorage, remap_topology() (+11 more)

### Community 3 - "Hypergraph<State>"
Cohesion: 0.06
Nodes (48): build_storage(), constraint(), graph_conversion_rejects_edge_endpoints(), Hypergraph, Hypergraph<Frozen>, Hypergraph<Mutable>, Hypergraph<State>, HyperStorage (+40 more)

### Community 4 - "MetaGraph"
Cohesion: 0.08
Nodes (17): MetaGraph, AtomId, AtomKind, Box, Default, EdgeTopology, GraphId, Option (+9 more)

### Community 5 - "Metagraph<State>"
Cohesion: 0.06
Nodes (38): exposes_explicit_meta_atom_api_over_general_layout(), Metagraph, Metagraph<Frozen>, Metagraph<Mutable>, Metagraph<State>, AtomId, AtomKind, Clone (+30 more)

### Community 6 - "catalog.rs"
Cohesion: 0.07
Nodes (71): atom_kind_byte(), bootstrap_catalogs_round_trip_through_direct_file(), BootstrapCatalogPages, catalog_page_rejects_non_zero_reserved_bytes(), catalog_update_root_container_preserves_aligned_pages(), CatalogDecoder, CatalogDecoder<'a>, CatalogKind (+63 more)

### Community 7 - "MvccTransaction"
Cohesion: 0.06
Nodes (55): AtomMeta, basic_generic_rw(), BytesCodec, closure_codec_with_capture(), ClosureCodec, ClosureCodec<T>, Codec, exact_phantom_label() (+47 more)

### Community 8 - "AtomId"
Cohesion: 0.07
Nodes (32): bfs_spanning_tree(), BinaryLiftingTable, Condensation, DagSubGraph, extract_dag(), extract_forest(), extract_hyper(), extract_tree() (+24 more)

### Community 9 - "AtomId"
Cohesion: 0.06
Nodes (16): AtomId, AtomKind, GraphId, kind_bit_encoding(), Debug, Default, Display, Formatter (+8 more)

### Community 10 - "PageId"
Cohesion: 0.09
Nodes (24): backend(), BufferPool, clock_pro_scan_resistance(), ClockProInner, dirty_flushed_on_eviction(), eviction_produces_ghost(), Frame, FrameStatus (+16 more)

### Community 11 - "PageNo"
Cohesion: 0.12
Nodes (70): EdgeSlotFlags, NodeSlotFlags, adjacency_overflow_codecs_preserve_order(), decode_adjacency_overflow(), decode_cross_level_overflow(), decode_freelist(), decode_graph_statistics(), decode_large_property() (+62 more)

### Community 12 - "TransactionGraphView"
Cohesion: 0.08
Nodes (15): build_record_index(), Cow, EdgeIds, EdgeRead, EdgeView, Frozen, GraphRead, NodeIds (+7 more)

### Community 13 - "page_io.rs"
Cohesion: 0.09
Nodes (40): c_void, IORING_HANDLE_REF, IORING_VERSION, checked_byte_len(), create_ring(), FilePageExtent, IoProfile, median_sample() (+32 more)

### Community 14 - "FerrosGrynnResult"
Cohesion: 0.08
Nodes (54): AlignedVec, all_format_v1_tags_round_trip(), archive_envelope_checks_magic_version_schema_length_and_checksum(), ArchiveEnvelope, assert_status(), canonical_f32(), canonical_float_encoding_rejects_signed_zero_and_nan_payloads(), CanonicalCodec (+46 more)

### Community 15 - "Path"
Cohesion: 0.07
Nodes (39): EndpointRole, Path, PathDirection, PathStep, PathTraceArena, PathTraceId, PathTraceNode, resolve_atom() (+31 more)

### Community 16 - "storage/store.rs"
Cohesion: 0.12
Nodes (38): DiskAtomRef, EdgeTopologyCode, EndpointSide, GraphStatistics, build_graph_image(), checked_page_index(), checked_u32(), CrossLevelOverflowStoreBuilder (+30 more)

### Community 17 - "win_async.rs"
Cohesion: 0.08
Nodes (39): cpu_affinity_cpu0(), goto_shutdown(), ioring_handle_ref(), ioring_raw_buffer_ref(), ioring_registered_buffer_ref(), new_fails_gracefully_if_no_ioring(), PendOp, probe_ioring() (+31 more)

### Community 18 - "Number"
Cohesion: 0.10
Nodes (19): Decimal, f32, f64, Number, Default, Display, Eq, Err (+11 more)

### Community 19 - "src/ecs.rs"
Cohesion: 0.07
Nodes (34): AnyPool, ColorTag, FeatureVector, node_id(), NodeWeight, PageRank, Position2D, Position3D (+26 more)

### Community 20 - "wal.rs"
Cohesion: 0.15
Nodes (33): CheckpointEnd, decode_checkpoint_end(), decode_page_init(), decode_page_key(), decode_tx_commit(), ExactRead, file_instance_successor(), invalid_wal() (+25 more)

### Community 21 - "StatusObject"
Cohesion: 0.06
Nodes (31): Deserialize, Ok, Serialize, Severity, Condition, Domain, ErrorCategory, ErrorClassification (+23 more)

### Community 22 - "Subgraph<State>"
Cohesion: 0.06
Nodes (32): marks_materialized_result_without_new_storage_family(), AtomId, AtomKind, Clone, Cow, Debug, Default, Edge (+24 more)

### Community 23 - "store.rs"
Cohesion: 0.10
Nodes (31): auto_checkpoint_triggers(), checkpoint_clears_wal(), compress_payload(), crc32(), crc32_update(), create_and_reopen(), decompress_payload(), delete_graph() (+23 more)

### Community 24 - "status_codes.rs"
Cohesion: 0.09
Nodes (29): define_status_codes(), is_string(), is_vec_string(), last_path_ident(), option_lit(), Ident, Option, Parse (+21 more)

### Community 25 - "PropertyMap"
Cohesion: 0.05
Nodes (40): DoubleEndedIterator, Entry, FromIterator, FusedIterator, I, IntoIter, IntoValue, SliceIter (+32 more)

### Community 26 - "EdgeEndpoint"
Cohesion: 0.09
Nodes (23): INLINE_ENDPOINTS, distinguishes_binary_hyper_and_cross_level_topology(), EdgeDirection, EdgeEndpoint, EdgeTopology, QualifiedAtomId, AtomId, Box (+15 more)

### Community 27 - "AtomId"
Cohesion: 0.17
Nodes (6): AtomId, Debug, Display, Formatter, Option, Result

### Community 28 - "runtime/mod.rs"
Cohesion: 0.14
Nodes (16): Geometry, Position, atom_and_path_have_distinct_value_contracts(), Closure, display_is_deterministic_and_escapes_strings(), eq_ord_and_hash_agree_for_cross_number_values(), estimated_geometry_heap_bytes(), estimated_position_heap_bytes() (+8 more)

### Community 29 - "Position"
Cohesion: 0.07
Nodes (35): GeoJsonGeometry, GeoJsonPosition, assert_status(), compare_float_slices(), GeoJsonValue, Geometry, into_position(), into_positions() (+27 more)

### Community 30 - "Transaction"
Cohesion: 0.11
Nodes (21): GraphRefReader, DatabaseRoot, ensure_store_reference(), graph_removal_preserves_table_catalog_integrity(), materialize_atom(), materialize_rows(), PredicateRead, QuerySnapshot (+13 more)

### Community 31 - "src/traversal.rs"
Cohesion: 0.10
Nodes (43): BellmanFordResult, bellman_ford(), BellmanFordError, bfs(), bfs_with(), dfs(), dfs_full(), dijkstra() (+35 more)

### Community 32 - ".new"
Cohesion: 0.22
Nodes (28): concurrent_writes_to_the_same_atom_fail_first_committer_wins(), configured_transaction_limits_have_domain_statuses(), disjoint_atom_writes_merge(), graph_id(), local_graph_table_and_edge_survive_reopen(), local_large_properties_overflow_and_abort_round_trip(), local_metagraph_round_trips_nested_content_and_cross_level_incidence(), local_multi_graph_commit_and_same_page_merge_survive_reopen() (+20 more)

### Community 33 - "Result"
Cohesion: 0.28
Nodes (5): Error, FerrosGrynnError, Result, Self, value_conversion_error()

### Community 34 - "Text"
Cohesion: 0.08
Nodes (25): Borrow, Extend, CompactString, exposes_controlled_utf8_mutation(), round_trips_through_std_and_compact_strings(), AsRef, Deref, Display (+17 more)

### Community 35 - "FerrosGrynnError"
Cohesion: 0.08
Nodes (24): ErrorState, FerrosGrynnError, NonErrorStatus, Arc, Debug, Display, E, Error (+16 more)

### Community 36 - "DiagnosticRecord"
Cohesion: 0.09
Nodes (23): ArcStr, NonZeroU32, computes_unicode_locations_without_retaining_source(), DiagnosticRecord, ExecutionPhase, LabeledLocation, line_column_at(), LineColumn (+15 more)

### Community 37 - "Result"
Cohesion: 0.12
Nodes (26): Decoder, Decoder<R>, Encoder, Encoder<W>, from_bytes(), from_file(), roundtrip(), roundtrip_atom_kind_preserved() (+18 more)

### Community 38 - "SparseSet<T>"
Cohesion: 0.11
Nodes (21): atom(), invalid_atom(), rejects_null_atom(), AtomId, Box, Default, ExactSizeIterator, FerrosGrynnError (+13 more)

### Community 39 - "Duration"
Cohesion: 0.08
Nodes (21): DateTime, Duration, Default, Display, Eq, Err, Formatter, From (+13 more)

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
Cohesion: 0.06
Nodes (31): open_read_write_sync(), Arc, BufferPool, DirectFile, File, Path, PathBuf, Result (+23 more)

### Community 44 - "Closure"
Cohesion: 0.09
Nodes (22): Closure, ClosureFunction, Func, NonZeroU64, calls_function_and_keeps_process_identity(), Closure, different_closures_have_different_identity(), rejects_wrong_arity_before_invocation() (+14 more)

### Community 45 - "DirectFile"
Cohesion: 0.22
Nodes (13): alloc_aligned(), DirectFile, platform_grow(), platform_open(), platform_open_readonly(), platform_write_page(), AtomicU64, File (+5 more)

### Community 46 - "benches/property_map.rs"
Cohesion: 0.10
Nodes (35): bench_build(), bench_iterate_and_clone(), bench_lookup(), boxed_enum_map(), BoxedEnumIter, BoxedEnumIter<'a>, BoxedEnumPropertyMap, BoxedEnumRepr (+27 more)

### Community 47 - "FrameWal"
Cohesion: 0.11
Nodes (13): AtomicBool, RwLockReadGuard, ActiveSnapshot, FrameWal, ResolvedFrame, Arc, BTreeMap, Instant (+5 more)

### Community 48 - "AlignedPage"
Cohesion: 0.11
Nodes (21): PageFlags, aligned_page_has_exact_size_and_alignment(), aligned_page_slice_is_one_contiguous_byte_extent(), AlignedPage, page_checksum(), page_checksum_mut(), page_checksum_rejects_payload_corruption(), page_codec_rejects_wrong_page_number_and_invalid_raw_fields() (+13 more)

### Community 49 - "arrow.rs"
Cohesion: 0.11
Nodes (36): ArrayRef, ArrowError, DataType, arrow_failure(), arrow_round_trip_preserves_none_null_and_native_scalars(), build_array(), ColumnKind, decode_rows() (+28 more)

### Community 50 - "StatusReport"
Cohesion: 0.11
Nodes (20): IntoSO, applies_gql_precedence(), Outcome, Outcome<T>, precedence(), rejects_errors(), Display, Error (+12 more)

### Community 51 - "Lsn"
Cohesion: 0.07
Nodes (29): BufReader, decode_atom_meta_payload(), decode_entry(), encode_entry(), entry_crc(), Lsn, MultiSegmentReader, PageLsnTable (+21 more)

### Community 52 - "Vector"
Cohesion: 0.11
Nodes (19): canonical_f32(), canonical_f64(), compare_f32(), compare_f32_value(), compare_f64(), compare_f64_value(), hash_f32(), hash_f64() (+11 more)

### Community 53 - "storage/buffer_pool.rs"
Cohesion: 0.08
Nodes (55): AtomicUsize, CachedPage, Condvar, Loader, AccessDecision, batch_handles_resident_loading_missing_and_duplicate_pages(), batch_preserves_order_duplicates_and_contiguous_extents(), BatchLoadReservation (+47 more)

### Community 54 - "Range"
Cohesion: 0.14
Nodes (26): End, borrow_bound(), Bound<T>, bound_value(), BoundExt, box_bound(), checks_inclusion(), compare_end() (+18 more)

### Community 55 - "superblock.rs"
Cohesion: 0.16
Nodes (26): GraphSuperblockFlags, database_fixture(), database_superblock_round_trip_matches_layout(), DatabaseSuperblock, graph_fixture(), graph_superblock_round_trip_matches_layout(), GraphSuperblock, invalid_superblock() (+18 more)

### Community 56 - "storage/direct.rs"
Cohesion: 0.06
Nodes (71): OpenOptions, checked_file_length(), checked_page_extent(), checked_page_offset(), configure_direct_io(), create_new_never_overwrites_existing_file(), direct_file_batch_rejects_range_and_page_corruption(), direct_file_reads_and_validates_one_contiguous_extent() (+63 more)

### Community 57 - "graph/traversal.rs"
Cohesion: 0.19
Nodes (31): algorithm_failure(), bellman_ford(), bellman_ford_reports_negative_cycle_as_domain_error(), bfs_topology_and_components_are_correct(), binary_edges(), BinaryEdge, collect_node_ids(), collect_weights() (+23 more)

### Community 58 - "benchmarks.rs"
Cohesion: 0.07
Nodes (99): Barrier, File, Mmap, MmapMut, PageCacheKey, aggregate_read_samples(), aligned_pages(), apply_gate() (+91 more)

### Community 59 - "heap_estimator.rs"
Cohesion: 0.07
Nodes (21): HeapEstimatorCache, NoHeapEstimatorCache, Self, Measurable, HashMap, K, V, size_of_hash_map() (+13 more)

### Community 60 - "Type"
Cohesion: 0.10
Nodes (39): is_option(), AtomKind, Box, Display, Formatter, Result, Self, Text (+31 more)

### Community 61 - ".expr"
Cohesion: 0.18
Nodes (16): analyze(), Analyzer, Analyzer<'a>, compatible(), is_integer(), is_number(), Box, BTreeMap (+8 more)

### Community 62 - "Regex"
Cohesion: 0.10
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

### Community 66 - "mvcc_persist.rs"
Cohesion: 0.12
Nodes (37): atom_id_from(), BytesPersistentStore, checkpoint_then_recover(), CheckpointHeader, closure_codec_with_capture_in_persistent_store(), crc32(), decode_isolation(), encode_atom_meta() (+29 more)

### Community 67 - "Object"
Cohesion: 0.12
Nodes (16): Keys, BTreeMap<Text, Value>, has_canonical_key_order_independent_of_insertion(), Object, BTreeMap, From, FromIterator, IntoText (+8 more)

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

### Community 72 - "RecordId"
Cohesion: 0.23
Nodes (15): allocate_graph_id(), build_scalar_indexes(), database_property_dictionary(), encode_qualified_endpoint(), graph_tree_ids(), LocalCommitGraph, plan_graph(), plan_graph_tree() (+7 more)

### Community 73 - "HashValue"
Cohesion: 0.12
Nodes (14): AddAssign, ConstOne, Mul, MulAssign, One, HashValue, Add, Display (+6 more)

### Community 74 - "WorldComponents"
Cohesion: 0.09
Nodes (22): AnyPool, atom(), remove_atom_cleans_every_pool_and_respects_generation(), Any, AtomId, Box, FerrosGrynnResult, FnMut (+14 more)

### Community 75 - ".new"
Cohesion: 0.25
Nodes (7): IoBatch, IoRequest, Self, Vec, tag_routing(), threadpool_write_read(), Vec

### Community 76 - "Values"
Cohesion: 0.12
Nodes (17): DagView, ForestView, GraphView, Dag<State>, DagView<'graph, R>, directed_forest_view(), Forest<State>, RootedTree<State> (+9 more)

### Community 77 - "graph_index.rs"
Cohesion: 0.12
Nodes (68): build_primary_index(), build_scalar_index(), BuiltPrimaryIndex, BuiltScalarIndex, bulk_primary_tree_supports_multiple_levels_and_exact_lookup(), ChildPage, decode_dictionary_chain(), decode_label_dictionary() (+60 more)

### Community 78 - "StatusWireV1"
Cohesion: 0.19
Nodes (19): completion_omits_severity_and_classification(), DiagnosticRecordWireV1, error_classification(), FerrosGrynnError, labeled_location_to_wire(), LabeledLocationWireV1, location_to_wire(), LocationWireV1 (+11 more)

### Community 79 - ".new"
Cohesion: 0.13
Nodes (14): GraphReadView, SmallVec, invalid_edges_are_transactional_and_use_graph_statuses(), record_index_rejects_cross_kind_duplicates_and_releases_removed_ids(), removal_cascades_through_edge_endpoints_and_reuses_generation(), EdgeIds, NodeIds, RapidHashMap (+6 more)

### Community 80 - "PropertyDictionary"
Cohesion: 0.14
Nodes (12): assigns_dense_ids_and_resolves_both_directions(), interning_is_idempotent_and_iteration_uses_id_order(), PropertyDictionary, ExactSizeIterator, FerrosGrynnResult, IntoText, Item, Option (+4 more)

### Community 81 - "Vec"
Cohesion: 0.17
Nodes (20): build_graph_images(), BuiltScalarIndexImage, BuiltTableIndexBinding, encode_subgraph_pages(), encode_tables(), LocalCommitResult, LocalCommitTable, LocalCommitTableField (+12 more)

### Community 82 - "DiagnosticValue"
Cohesion: 0.14
Nodes (11): bool, DiagnosticValue, f32, f64, BTreeMap, String, StatusParameter, str (+3 more)

### Community 83 - "TraversalIndex"
Cohesion: 0.24
Nodes (14): bfs(), deduplicated_steps(), dfs(), local_arcs(), local_steps(), DoubleEndedIterator, Item, RapidHashMap (+6 more)

### Community 84 - "AccumList"
Cohesion: 0.15
Nodes (15): AccumList, FieldAccumArgs, Attribute, DeriveInput, Ident, Option, Parse, ParseStream (+7 more)

### Community 85 - "graph_core.rs"
Cohesion: 0.45
Nodes (10): benchmark_access_modes(), benchmark_path_and_live_publication(), benchmark_state_and_family_conversions(), benchmark_traversal(), dag_chain(), general_chain(), path(), AtomId (+2 more)

### Community 86 - "Bytes"
Cohesion: 0.20
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

### Community 90 - ".new"
Cohesion: 0.31
Nodes (31): checkpoint_end_layout_is_fixed_without_dirty_page_table(), commit_during_backfill_remains_for_next_checkpoint(), committed_frame_advances_file_instance_clock(), committed_frames_preserve_snapshot_versions_across_reopen(), concurrent_commits_publish_monotonic_clocks(), graph_superblock_is_not_a_transaction_frame(), marked_page(), page_frame_key_layout_distinguishes_file_instances() (+23 more)

### Community 91 - "Graphify"
Cohesion: 0.15
Nodes (13): Add URL and Watch Folder, Graphify Extra Exports, Semantic Extraction Contract, GitHub Clone and Cross-Repo Merge, Graphify Commit and Claude Hooks, Graph Query Path and Explain, Video and Audio Transcription, Incremental Graph Update (+5 more)

### Community 92 - "PropertyMap"
Cohesion: 0.23
Nodes (7): bench_search(), Property, PropertyMap, Criterion, Self, String, Vec

### Community 93 - "wy_hash.rs"
Cohesion: 0.29
Nodes (7): i64_le(), u32(), u64_rotate32(), wy_hash64(), WyHash, wymum(), wyr3()

### Community 94 - "BinaryLiftingIndex"
Cohesion: 0.18
Nodes (10): BinaryLiftingIndex, QueueEntry, Eq, FnMut, Ord, Ordering, PartialEq, PartialOrd (+2 more)

### Community 95 - "local.rs"
Cohesion: 0.16
Nodes (38): Decode, KeyOf, atomic_replace(), Candidate, corrupt_temporary_is_removed_after_main_selection(), database(), database_lock_is_exclusive_for_its_lifetime(), DatabaseLock (+30 more)

### Community 96 - "TextSpan"
Cohesion: 0.22
Nodes (20): TextSpan, Expr, ExprKind, Identifier, LetStatement, Literal, ObjectField, Program (+12 more)

### Community 97 - "HashFunction"
Cohesion: 0.21
Nodes (3): IncrementalXXH64, HashFunction, UtilHash

### Community 98 - "LocalGraphView"
Cohesion: 0.10
Nodes (10): DecodedGraph, DecodedNode, LocalGraphView, EdgeIds, GraphKind, GraphRead, GraphStorageMode, NodeIds (+2 more)

### Community 99 - "LocalGraph"
Cohesion: 0.15
Nodes (36): DataFileKind, EdgeHotSlot, EncodeContext, FnMut, LabelDictionary, TableKind, allocate_file_id(), collect_local_ids() (+28 more)

### Community 100 - "Option"
Cohesion: 0.30
Nodes (10): Dag, Forest, Hypergraph, Metagraph, RootedTree, GraphWriteView, MutableGraphValue, Mutable (+2 more)

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
Cohesion: 0.13
Nodes (12): Array, From, FromIterator, IntoValue, IterMut, Option, Self, T (+4 more)

### Community 110 - "Text"
Cohesion: 0.13
Nodes (13): RapidHashSet, SlotAllocator, EdgeColumns, empty_topology(), ensure_unique_record(), NodeColumns, Arc, AtomId (+5 more)

### Community 111 - "fg-meta/src/lib.rs"
Cohesion: 0.09
Nodes (17): EdgeIncidence, EdgeInfo, EdgeRole, EdgeTopology, AtomId, Box, GraphId, Option (+9 more)

### Community 112 - "Specification Design Authority"
Cohesion: 0.33
Nodes (6): Specification Design Authority, AtomWalk Syntax, MATCH Graph Patterns, GrynQL EBNF Grammar, Context-Sensitive Parser Disambiguation, GrynQL Statements

### Community 113 - "Persistent MVCC v2"
Cohesion: 0.33
Nodes (6): ClosureCodec, Exact Phantom Matching, Persistent MVCC v2, Serializable Snapshot Isolation, GrynQL Transaction Model, Execution MVCC and Snapshot Contract

### Community 114 - "CowVec<T, CHUNK_LEN>"
Cohesion: 0.17
Nodes (10): CowVec<T, CHUNK_LEN>, Default, FnMut, From, Index, IndexMut, Output, Self (+2 more)

### Community 115 - "GraphMetadata"
Cohesion: 0.08
Nodes (12): GraphMetadata, BTreeSet, Default, DirtyTarget, ExactSizeIterator, FerrosGrynnResult, GraphRef, GraphStorageMode (+4 more)

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

### Community 122 - "Arc"
Cohesion: 0.11
Nodes (18): AtomicU64, ActiveSnapshot, all_dirty(), CheckpointMode, Datastore, GraphWrite, merge_materialized(), Arc (+10 more)

### Community 123 - ".new"
Cohesion: 0.31
Nodes (22): CommitOutcome, cataloged_database_create_reopen_and_exclusive_create(), concurrent_checkpoints_publish_once(), marked_page(), mixed_batch_preserves_sources_order_and_duplicates(), mixed_batch_rejects_more_unique_pages_than_pool_capacity(), old_reader_survives_checkpoint_and_next_checkpoint_advances(), open_rejects_file_instance_clock_reuse() (+14 more)

### Community 124 - "P3 Query Execution"
Cohesion: 0.50
Nodes (4): P3 Query Execution, P2 Query Frontend, P2 Storage WAL and MVCC, P1 Value Foundation

### Community 125 - "PageKey"
Cohesion: 0.08
Nodes (13): PageLoadExtent, CommitTs, data_file_id_layout_is_stable_and_validated(), GraphId, Lsn, PageKey, FerrosGrynnResult, Self (+5 more)

### Community 126 - "&'a Bytes"
Cohesion: 0.50
Nodes (3): &'a Bytes, IntoIter, IntoIterator

### Community 135 - "DiskAtomRef"
Cohesion: 0.07
Nodes (43): GraphRef, array_storage_conversion_reuses_owned_elements(), conversion_context_rejects_conflicting_reference_mappings(), datetime_conversion_preserves_pre_epoch_nanos_and_time_zone_kind(), DecodeContext, DiskAtomRef, DiskGraphRef, EncodeContext (+35 more)

### Community 136 - "number.rs"
Cohesion: 0.18
Nodes (9): BigInt, canonical_float(), canonical_fraction(), canonicalizes_nan_and_signed_zero_for_eq_ord_and_hash(), CanonicalNumber, conversion_error(), FerrosGrynnError, Ord (+1 more)

### Community 137 - "Uuid"
Cohesion: 0.13
Nodes (13): InnerUlid, InnerUuid, Display, Err, Formatter, From, FromStr, Result (+5 more)

### Community 138 - "MaterializedGraph"
Cohesion: 0.08
Nodes (20): MaterializedGraph, preserves_exact_discriminant_in_generic_transport(), AtomId, AtomKind, Cow, Edge, EdgeIds, Eq (+12 more)

### Community 139 - "execution.rs"
Cohesion: 0.09
Nodes (56): QueryCatalog, arithmetic(), binary(), binds_let_filters_projects_and_limits(), build_cursor(), cast(), collect_scalar_constraints(), decimal_arithmetic() (+48 more)

### Community 141 - "live.rs"
Cohesion: 0.07
Nodes (56): ApplyFn, ApplyUpdate, MutexGuard, Read, ReadFn, ReplayFn, accept_remote_discards_local_conflicting_state(), apply_or_pause() (+48 more)

### Community 142 - "datastore.rs"
Cohesion: 0.09
Nodes (27): atom_versions_keep_reused_slot_generations_distinct(), AtomVersions, CheckpointPolicy, CommitResult, DatastoreOptions, InMemoryOptions, IoMode, IsolationLevel (+19 more)

### Community 143 - "Dag<State>"
Cohesion: 0.05
Nodes (52): build_oriented_tree_storage(), build_storage(), constraint(), Dag, Dag<Frozen>, Dag<Mutable>, Dag<State>, DagStorage (+44 more)

### Community 144 - "Value"
Cohesion: 0.08
Nodes (22): Array, Bytes, DateTime, Range, Regex, Set, bool, Graph<Frozen> (+14 more)

### Community 145 - "ParserSettings"
Cohesion: 0.11
Nodes (19): Config, Default, Self, parse(), QueryResult, QueryStatementKind, FerrosGrynnResult, Value (+11 more)

### Community 147 - "RecordIdPart"
Cohesion: 0.12
Nodes (23): JsonValue, assert_status(), enforces_the_composite_nesting_limit(), quotes_ambiguous_string_ids_for_round_trip(), RecordId, RecordIdPart, recursively_rejects_unsupported_parts(), rejects_empty_components() (+15 more)

### Community 148 - "ir.rs"
Cohesion: 0.21
Nodes (15): Column, LogicalNode, LogicalNodeKind, LogicalProgram, LogicalSelect, LogicalStatement, NodeId, Box (+7 more)

### Community 149 - ".append"
Cohesion: 0.31
Nodes (6): ActiveSegment, RecordLocation, AsRef, BufWriter, Path, Self

### Community 150 - "AtomId"
Cohesion: 0.15
Nodes (10): Condensation, CutFrame, CutSet, Predecessor, AtomId, Box, Option, Path (+2 more)

### Community 151 - ".connect"
Cohesion: 0.29
Nodes (3): endpoint_kind(), AtomKind, EdgeTopology

### Community 152 - "Iter<'_, T>"
Cohesion: 0.67
Nodes (3): Iter<'_, T>, ExactSizeIterator, FusedIterator

### Community 153 - "exact_ops.rs"
Cohesion: 0.17
Nodes (19): CheckedAdd, CheckedMul, CheckedSub, PrimInt, Signed, integer_arithmetic(), add_exact(), AddExact (+11 more)

### Community 154 - "FerrosGrynnResult"
Cohesion: 0.16
Nodes (13): atom_label_mut(), atom_properties_mut(), AtomRefTopology, directed_pair(), forest_pair(), GraphMutationView, EdgeTopology, FerrosGrynnResult (+5 more)

### Community 155 - ".get"
Cohesion: 0.27
Nodes (9): append_raw_commit(), append_raw_frame(), checkpoint_end_requires_begin_and_durable_target(), encode_page_key(), Vec, StagedPage, WalKind, WriterHealth (+1 more)

### Community 156 - "Frozen"
Cohesion: 0.08
Nodes (18): Frozen, EdgeView, EdgeView<'a>, MaterializationOrigin, NodeView, NodeView<'a>, AtomId, BTreeMap (+10 more)

### Community 157 - "Q: What is MetaGraph?"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: What is MetaGraph?, Source Nodes

### Community 158 - "FerrosGrynnResult"
Cohesion: 0.13
Nodes (18): NodeHotSlot, decode_label(), LocalEdgeRead, LocalNodeRead, materialize_graph(), read_local_edge(), read_local_node(), read_local_properties() (+10 more)

### Community 159 - "Handle<'_, T>"
Cohesion: 0.50
Nodes (3): Handle<'_, T>, Deref, Target

### Community 160 - "Отчёт этапа 5: Windows sync storage completion"
Cohesion: 0.05
Nodes (39): Итоговая архитектура, Контракты и публичный API, Локальное дисковое хранилище Ferros-Grynn: sync-first, Проверки и критерии приёмки, Этапы реализации, Явно вне начального scope, Исправленный benchmark, Калибровка batch (+31 more)

### Community 161 - "ValueKind"
Cohesion: 0.21
Nodes (5): Display, H, Ordering, ValueKind, VectorKind

### Community 162 - ".estimated_heap_bytes"
Cohesion: 0.20
Nodes (10): Bound, RecordIdPart, estimated_bound_heap_bytes(), estimated_record_id_heap_bytes(), estimated_record_part_heap_bytes(), estimated_values_heap_bytes(), estimated_vector_heap_bytes(), Item (+2 more)

### Community 163 - ".open_cataloged"
Cohesion: 0.20
Nodes (17): AsRef, DatabaseLock, DatabaseSuperblock, Into, RecoverySeed, aligned_extent(), local_io(), recovery_seed() (+9 more)

### Community 164 - "Path"
Cohesion: 0.12
Nodes (36): FileCatalogEntry, GraphCatalogEntry, PropertyLocation, PropertyPageBuilder, BuiltGraphGeneration, BuiltGraphImage, CatalogState, encode_optional_properties() (+28 more)

### Community 165 - "SnapshotId"
Cohesion: 0.07
Nodes (14): AtomIds<'a>, AtomRef, DiskGraphView, DiskGraphView<'tx, R>, GraphKind, GraphReadView, GraphReadView<'tx, R>, AtomKind (+6 more)

### Community 166 - "TestDatabase"
Cohesion: 0.23
Nodes (5): CreatedDatabaseDirectory, Drop, PathBuf, TestDatabase, TestDatabasePath

### Community 167 - "memory.rs"
Cohesion: 0.12
Nodes (8): AtomicI64, allocate_memory(), compare_and_swap_i64(), compare_and_swap_u64(), get_and_add_i64(), get_and_set_i64(), AtomicU64, Result

### Community 168 - "Value"
Cohesion: 0.17
Nodes (8): Box, From, FxHashMap, Option, Self, String, Vec, Value

### Community 169 - "Lsn"
Cohesion: 0.10
Nodes (24): CheckpointGeneration, checkpoint_prunes_closed_prefix_and_seeded_recovery_continues_clocks(), CheckpointCommit, CheckpointTicket, FrameLocation, FrameVersion, FrameWalInner, list_segments() (+16 more)

### Community 170 - "Result"
Cohesion: 0.22
Nodes (12): BatchEntry, blocking_op(), FileOp, IoCompletion, IoOp, Mem, Pending, Any (+4 more)

### Community 171 - "C"
Cohesion: 0.33
Nodes (7): A, atom(), B, C, chooses_actual_smallest_pool_independent_of_type_order(), missing_pool_produces_empty_join(), AtomId

### Community 172 - "read.rs"
Cohesion: 0.06
Nodes (28): assert_snapshot_released(), cascade_removal_marks_removed_incident_edges_dirty(), disk_view_and_hybrid_materialization_have_the_same_read_result(), DiskGraphView<'_, R>, EdgeRead, FakeTransaction, GraphMut, GraphRead (+20 more)

### Community 173 - "collections.rs"
Cohesion: 0.33
Nodes (10): batch_random_access(), bench_collect(), bench_iter_sequential_borrowed(), bench_iter_sequential_owned(), bench_iter_sequential_with_step(), bench_push_preallocated(), bench_push_raw(), random_indices() (+2 more)

### Community 174 - "TableIndex"
Cohesion: 0.18
Nodes (8): Fields, IntoText, ExactSizeIterator, Item, Type, TableField, TableIndex, TableIndexKind

### Community 175 - "src/id.rs"
Cohesion: 0.22
Nodes (7): EdgeKindMarker, GraphKindMarker, NodeKindMarker, K, PhantomData, Vec, SlotAllocator

### Community 176 - "AsyncUringEngine"
Cohesion: 0.14
Nodes (12): AsyncUringEngine, bg_loop(), CtrlMsg, file_worker(), Drop, File, JoinHandle, PathBuf (+4 more)

### Community 177 - ".open"
Cohesion: 0.29
Nodes (6): checkpoint_due(), Duration, NonZeroUsize, PageIoMode, PathLike, Self

### Community 178 - "Option"
Cohesion: 0.29
Nodes (4): Iter<'a, T>, Item, Iterator, Option

### Community 179 - "Iter"
Cohesion: 0.38
Nodes (4): clone_copies_only_the_changed_chunk(), CowVec, Iter, Arc

### Community 180 - "GraphWorkspace<G>"
Cohesion: 0.09
Nodes (26): canonical_mutation_invalidates_derived_and_removes_dead_components(), component_mutation_does_not_change_graph_revision(), G, GraphWorkspace, GraphWorkspace<G>, map_graph_moves_user_components_but_discards_derived_pools(), PageRank, Position (+18 more)

### Community 181 - "Q: Как изменения этапа 7 связывают лимит write-set, индекс RecordId и публикацию disk-backed local graph?"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: Как изменения этапа 7 связывают лимит write-set, индекс RecordId и публикацию disk-backed local graph?, Source Nodes

### Community 182 - "PageRead"
Cohesion: 0.38
Nodes (5): PageGuard, collect_page_reads(), PageRead, AlignedPage, Option

### Community 183 - "SlotAllocator"
Cohesion: 0.27
Nodes (6): atom_id_uses_u40_slots_and_u23_generations(), AtomKind, FerrosGrynnResult, Self, slot_reuse_invalidates_stale_handles(), SlotAllocator

### Community 184 - "async_io.rs"
Cohesion: 0.14
Nodes (20): AsyncIoEngine, build(), build_ring(), bump_memlock(), create_engine(), create_engine_full(), EngineConfig, FileSlot (+12 more)

### Community 185 - "SyncPageManager"
Cohesion: 0.09
Nodes (22): CatalogUpdatePages, FileRegistry, FrameWal, PageCacheVersion, PageIo, RegisteredDataFile, ResolvedFrame, CheckpointAuthority (+14 more)

### Community 186 - "Vec"
Cohesion: 0.50
Nodes (5): ParserSettings, QueryResult, Object, Vec, select_uses_explicit_schema_and_record_id_order()

### Community 187 - "Graph"
Cohesion: 0.12
Nodes (20): Clone, GWB, PhantomData, QueryMaterialize, R, copy_from_read(), Graph, Graph<Frozen> (+12 more)

### Community 188 - "AlignedBuffer"
Cohesion: 0.17
Nodes (7): AlignedBuffer, free_aligned(), platform_read_page(), Drop, HANDLE, Send, Sync

### Community 189 - ".into_iter"
Cohesion: 0.28
Nodes (5): &'a Array, &'a mut Array, I, IntoIter, IntoIterator

### Community 190 - ".into_iter"
Cohesion: 0.28
Nodes (5): &'a mut Object, &'a Object, I, IntoIter, IntoIterator

### Community 191 - "property_page_round_trips_canonical_map"
Cohesion: 0.36
Nodes (6): decode_property_map(), encode_property_map(), property_page_round_trips_canonical_map(), PropertyPageBuilder, PropertyMap, write_u16()

### Community 192 - "Unified Atom Incidence"
Cohesion: 0.33
Nodes (6): Unified Atom Incidence, Graph Storage Families, Ferros-Grynn Graph Taxonomy, Mutable Frozen Type-State and Views, P4 Full Atom Incidence, GraphWorkspace ECS Sidecar

### Community 193 - "write_sequence"
Cohesion: 0.43
Nodes (6): Formatter, IntoIterator, write_debug_sequence(), write_json_string(), write_sequence(), T

### Community 195 - "LargeData"
Cohesion: 0.53
Nodes (4): LargeData, From, Self, u32

### Community 196 - "AtomIds"
Cohesion: 0.21
Nodes (8): Inner, AtomIds, FallibleAtomIds, FallibleAtomIds<Inner>, ExactSizeIterator, FusedIterator, Item, Iterator

### Community 197 - "Canonical Storage Value Codec"
Cohesion: 0.50
Nodes (4): Canonical Storage Value Codec, OLTP Property Heap and Column Cache, Runtime Storage Columnar Boundary, StorageValue and Canonical Codec

### Community 199 - ".call"
Cohesion: 0.33
Nodes (4): FerrosGrynnError, Formatter, Result, Value

### Community 200 - ".writeback_context"
Cohesion: 0.40
Nodes (3): DirtyTarget, MaterializationOrigin, Vec

### Community 261 - "Graph<State>"
Cohesion: 0.08
Nodes (21): freeze_thaw_preserves_identity_ids_and_topology(), Graph<State>, AtomKind, Debug, EdgeView, Eq, Formatter, GraphRead (+13 more)

## Knowledge Gaps
- **95 isolated node(s):** `Решение`, `Что означает spill`, `Диагностический benchmark`, `Исправление лимита write-set`, `Устранение O(n²) проверки `RecordId`` (+90 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **76 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Graph` connect `Graph` to `AtomId`, `atom_does_not_exist`, `Hypergraph<State>`, `Metagraph<State>`, `MaterializedGraph`, `TransactionGraphView`, `live.rs`, `Dag<State>`, `Value`, `Subgraph<State>`, `runtime/mod.rs`, `Frozen`, `FerrosGrynnResult`, `read.rs`, `RecordId`, `.new`, `graph_core.rs`, `Option`, `GraphMetadata`?**
  _High betweenness centrality (0.170) - this node is a cross-community bridge._
- **Why does `Subgraph` connect `Subgraph<State>` to `MaterializedGraph`, `Graph`, `live.rs`, `fg-meta/src/lib.rs`?**
  _High betweenness centrality (0.093) - this node is a cross-community bridge._
- **Why does `MetaGraph` connect `MetaGraph` to `AdjacencyMatrix`, `Result`, `AtomId`, `Item`, `fg-meta/src/lib.rs`, `src/id.rs`, `store.rs`, `bench_graph.rs`, `src/traversal.rs`?**
  _High betweenness centrality (0.088) - this node is a cross-community bridge._
- **What connects `Решение`, `Что означает spill`, `Диагностический benchmark` to the rest of the system?**
  _95 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `AtomId` be split into smaller, more focused modules?**
  _Cohesion score 0.05868653935724266 - nodes in this community are weakly interconnected._
- **Should `atom_does_not_exist` be split into smaller, more focused modules?**
  _Cohesion score 0.11846689895470383 - nodes in this community are weakly interconnected._
- **Should `Hypergraph<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.05526512322628827 - nodes in this community are weakly interconnected._