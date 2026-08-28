# Graph Report - ferros-grynn  (2026-08-26)

## Corpus Check
- 168 files · ~295,400 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 6406 nodes · 20005 edges · 194 communities (188 shown, 6 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 192 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `9e876f31`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- Parser
- AtomId
- Dag<State>
- Hypergraph<State>
- MetaGraph
- Lsn
- catalog.rs
- MvccTransaction
- AtomId
- AtomId
- PageId
- graph_pages.rs
- TableSchema
- .new
- FerrosGrynnResult
- Path
- codec.rs
- win_async.rs
- Number
- src/ecs.rs
- wal.rs
- status.rs
- Subgraph<State>
- store.rs
- status_codes.rs
- PropertyKeyId
- EdgeTopology
- AtomId
- Value
- Position
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
- Result
- benches/property_map.rs
- DiskAtomRef
- PageNo
- arrow.rs
- StatusReport
- .new
- Vector
- storage/buffer_pool.rs
- Range
- GraphSuperblock
- storage/direct.rs
- graph/traversal.rs
- benchmarks.rs
- heap_estimator.rs
- order_key.rs
- .expr
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
- .new
- HashValue
- WorldComponents
- .new
- Values
- AlignedPage
- StatusWireV1
- .append
- PropertyDictionary
- local.rs
- DiagnosticValue
- EdgeEndpoint
- AccumList
- mvcc_persist.rs
- Bytes
- bench_graph.rs
- os.rs
- .register_datafusion_table
- CowVec<T, CHUNK_LEN>
- Graphify
- PropertyMap
- wy_hash.rs
- BinaryLiftingIndex
- record_id.rs
- TextSpan
- HashFunction
- .new
- FerrosGrynnResult
- Frozen
- build_from_map
- memohash.rs
- Ferros-Grynn Repository Guidance
- Physical Query Execution
- Values Module Guidance
- GrynQL Language Specification
- atoms
- helpers.rs
- Array
- EdgeColumns
- fg-meta/src/lib.rs
- Specification Design Authority
- Persistent MVCC v2
- Unified Atom Incidence
- exact_ops.rs
- hashing/mod.rs
- XorShift32HashFunction
- MetaGraphError
- laws.rs
- ferros-grynn-macros/src/lib.rs
- Hybrid Query Model
- Transaction
- Canonical Storage Value Codec
- P3 Query Execution
- CheckpointGeneration
- &'a Bytes
- .fmt
- Direct I/O and Clock-Pro
- Redacted External Error Sources
- StorageValue
- StatusObject
- Uuid
- MaterializedGraph
- execution.rs
- Lsn
- DirtyTarget
- datastore.rs
- Type
- TxHandle
- QueryResult
- checkpoint_prunes_closed_prefix_and_seeded_recovery_continues_clocks
- ir.rs
- .open_cataloged
- AtomId
- collections.rs
- Iter<'_, T>
- .decode
- FerrosGrynnResult
- DiskGraphView<'_, R>
- Option
- Q: What is MetaGraph?
- TableIndex
- Handle<'_, T>
- Отчёт этапа 5: Windows sync storage completion
- RecordIdPart
- .insert
- .push
- .into_iter
- .call
- Iter<'a, T>
- Graph
- Value
- LocalGraph
- async_io.rs
- StorageRecordValue
- read.rs
- PageRead
- AtomIds
- SnapshotId
- Option
- NoHeapEstimatorCache
- .open
- Item
- GraphWorkspace<G>
- join.rs
- .next
- GraphRead
- UringState
- .fmt
- atom_does_not_exist
- RegisteredDataFile
- Metagraph<State>
- SyncPageManager
- storage/store.rs
- MaterializationOrigin
- Graph<State>
- AtomId

## God Nodes (most connected - your core abstractions)
1. `MetaGraph` - 153 edges
2. `PageNo` - 132 edges
3. `AlignedPage` - 125 edges
4. `Graph` - 88 edges
5. `Value` - 87 edges
6. `Uuid` - 65 edges
7. `Frozen` - 64 edges
8. `DiskAtomRef` - 64 edges
9. `LocalGraph` - 61 edges
10. `Transaction` - 60 edges

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

## Communities (194 total, 6 thin omitted)

### Community 0 - "Parser"
Cohesion: 0.17
Nodes (13): Identifier, cover(), Symbol, Token, Parser, FerrosGrynnError, FerrosGrynnResult, Option (+5 more)

### Community 1 - "AtomId"
Cohesion: 0.12
Nodes (30): build_parent_map(), build_storage(), find_root(), Forest<Mutable>, forest_node_removal_marks_every_incident_edge_dirty(), forest_rejects_cycles_without_mutation(), ForestStorage, infer_directed_orientation() (+22 more)

### Community 2 - "Dag<State>"
Cohesion: 0.05
Nodes (52): build_oriented_tree_storage(), build_storage(), constraint(), Dag, Dag<Frozen>, Dag<Mutable>, Dag<State>, DagStorage (+44 more)

### Community 3 - "Hypergraph<State>"
Cohesion: 0.06
Nodes (46): build_storage(), constraint(), graph_conversion_rejects_edge_endpoints(), Hypergraph<Frozen>, Hypergraph<Mutable>, Hypergraph<State>, HyperStorage, permits_a_vertex_in_both_directed_incidence_roles() (+38 more)

### Community 4 - "MetaGraph"
Cohesion: 0.08
Nodes (17): MetaGraph, AtomId, AtomKind, Box, Default, EdgeTopology, GraphId, Option (+9 more)

### Community 5 - "Lsn"
Cohesion: 0.13
Nodes (18): BufReader, decode_atom_meta_payload(), decode_entry(), encode_atom_meta(), encode_entry(), entry_crc(), Lsn, MultiSegmentReader (+10 more)

### Community 6 - "catalog.rs"
Cohesion: 0.07
Nodes (71): atom_kind_byte(), bootstrap_catalogs_round_trip_through_direct_file(), BootstrapCatalogPages, catalog_page_rejects_non_zero_reserved_bytes(), catalog_update_root_container_preserves_aligned_pages(), CatalogDecoder, CatalogDecoder<'a>, CatalogKind (+63 more)

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

### Community 11 - "graph_pages.rs"
Cohesion: 0.11
Nodes (73): EdgeSlotFlags, NodeSlotFlags, adjacency_overflow_codecs_preserve_order(), decode_adjacency_overflow(), decode_cross_level_overflow(), decode_freelist(), decode_graph_statistics(), decode_large_property() (+65 more)

### Community 12 - "TableSchema"
Cohesion: 0.15
Nodes (17): all_dirty(), build_record_index(), GraphWrite, materialize_atom(), materialize_rows(), QueryStorageRow, row_from_properties(), AtomId (+9 more)

### Community 13 - ".new"
Cohesion: 0.30
Nodes (33): checkpoint_end_layout_is_fixed_without_dirty_page_table(), checkpoint_end_requires_begin_and_durable_target(), commit_during_backfill_remains_for_next_checkpoint(), committed_frame_advances_file_instance_clock(), committed_frames_preserve_snapshot_versions_across_reopen(), concurrent_commits_publish_monotonic_clocks(), graph_superblock_is_not_a_transaction_frame(), marked_page() (+25 more)

### Community 14 - "FerrosGrynnResult"
Cohesion: 0.16
Nodes (11): Decoder<'a>, invalid(), limit_error(), Box, FerrosGrynnError, FerrosGrynnResult, Into, String (+3 more)

### Community 15 - "Path"
Cohesion: 0.07
Nodes (39): EndpointRole, Path, PathDirection, PathStep, PathTraceArena, PathTraceId, PathTraceNode, resolve_atom() (+31 more)

### Community 16 - "codec.rs"
Cohesion: 0.16
Nodes (30): AlignedVec, Position, ArchiveEnvelope, canonical_f32(), CanonicalCodec, canonicalize_set(), crc32(), encode_duration() (+22 more)

### Community 17 - "win_async.rs"
Cohesion: 0.06
Nodes (45): AtomicI64, cpu_affinity_cpu0(), goto_shutdown(), ioring_handle_ref(), ioring_raw_buffer_ref(), ioring_registered_buffer_ref(), new_fails_gracefully_if_no_ioring(), PendOp (+37 more)

### Community 18 - "Number"
Cohesion: 0.08
Nodes (30): BigInt, canonical_float(), canonical_fraction(), canonicalizes_nan_and_signed_zero_for_eq_ord_and_hash(), CanonicalNumber, conversion_error(), Decimal, f32 (+22 more)

### Community 19 - "src/ecs.rs"
Cohesion: 0.07
Nodes (34): AnyPool, ColorTag, FeatureVector, node_id(), NodeWeight, PageRank, Position2D, Position3D (+26 more)

### Community 20 - "wal.rs"
Cohesion: 0.13
Nodes (38): append_raw_commit(), append_raw_frame(), CheckpointEnd, decode_checkpoint_end(), decode_page_init(), decode_page_key(), decode_tx_commit(), encode_page_init() (+30 more)

### Community 21 - "status.rs"
Cohesion: 0.12
Nodes (13): Condition, Domain, ErrorCategory, ErrorClassification, NotificationClassification, Option, String, status_object_builds_full_description() (+5 more)

### Community 22 - "Subgraph<State>"
Cohesion: 0.05
Nodes (32): marks_materialized_result_without_new_storage_family(), AtomId, AtomKind, Clone, Cow, Debug, Default, Edge (+24 more)

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
Nodes (21): INLINE_ENDPOINTS, distinguishes_binary_hyper_and_cross_level_topology(), EdgeDirection, EdgeTopology, QualifiedAtomId, AtomId, Box, Display (+13 more)

### Community 27 - "AtomId"
Cohesion: 0.11
Nodes (14): allocate_memory(), Result, atom_id_uses_u40_slots_and_u23_generations(), AtomId, AtomKind, Debug, Display, FerrosGrynnResult (+6 more)

### Community 28 - "Value"
Cohesion: 0.08
Nodes (26): Closure, atom_and_path_have_distinct_value_contracts(), display_is_deterministic_and_escapes_strings(), eq_ord_and_hash_agree_for_cross_number_values(), graph_transport_preserves_exact_type_and_typed_conversion(), reports_concrete_kind_and_static_type(), AtomRef, Box (+18 more)

### Community 29 - "Position"
Cohesion: 0.07
Nodes (35): GeoJsonGeometry, GeoJsonPosition, assert_status(), compare_float_slices(), GeoJsonValue, Geometry, into_position(), into_positions() (+27 more)

### Community 30 - "GraphRef"
Cohesion: 0.11
Nodes (6): ensure_store_reference(), graph_removal_preserves_table_catalog_integrity(), View, GraphKind, GraphRef, GraphStorageMode

### Community 31 - "src/traversal.rs"
Cohesion: 0.10
Nodes (43): BellmanFordResult, bellman_ford(), BellmanFordError, bfs(), bfs_with(), dfs(), dfs_full(), dijkstra() (+35 more)

### Community 32 - ".new"
Cohesion: 0.18
Nodes (30): atom_versions_keep_reused_slot_generations_distinct(), concurrent_writes_to_the_same_atom_fail_first_committer_wins(), configured_transaction_limits_have_domain_statuses(), disjoint_atom_writes_merge(), graph_id(), local_graph_table_and_edge_survive_reopen(), local_large_properties_overflow_and_abort_round_trip(), local_metagraph_round_trips_nested_content_and_cross_level_incidence() (+22 more)

### Community 33 - "Result"
Cohesion: 0.09
Nodes (27): AtomRef, bool, Closure, Graph<Frozen>, MaterializedGraph, Number, Path, RecordId (+19 more)

### Community 34 - "Text"
Cohesion: 0.08
Nodes (25): Borrow, Extend, CompactString, exposes_controlled_utf8_mutation(), round_trips_through_std_and_compact_strings(), AsRef, Deref, Display (+17 more)

### Community 35 - "FerrosGrynnError"
Cohesion: 0.08
Nodes (24): ErrorState, FerrosGrynnError, NonErrorStatus, Arc, Debug, Display, E, Error (+16 more)

### Community 36 - "DiagnosticRecord"
Cohesion: 0.08
Nodes (23): ArcStr, NonZeroU32, computes_unicode_locations_without_retaining_source(), DiagnosticRecord, ExecutionPhase, LabeledLocation, line_column_at(), LineColumn (+15 more)

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
Cohesion: 0.06
Nodes (31): open_read_write_sync(), Arc, BufferPool, DirectFile, File, Path, PathBuf, Result (+23 more)

### Community 44 - "Closure"
Cohesion: 0.10
Nodes (21): ClosureFunction, Func, NonZeroU64, calls_function_and_keeps_process_identity(), Closure, different_closures_have_different_identity(), rejects_wrong_arity_before_invocation(), Arc (+13 more)

### Community 45 - "Result"
Cohesion: 0.13
Nodes (17): AlignedBuffer, alloc_aligned(), free_aligned(), platform_grow(), platform_open(), platform_open_readonly(), platform_read_page(), platform_write_page() (+9 more)

### Community 46 - "benches/property_map.rs"
Cohesion: 0.12
Nodes (32): bench_build(), bench_iterate_and_clone(), bench_lookup(), boxed_enum_map(), BoxedEnumIter, BoxedEnumPropertyMap, BoxedEnumRepr, btree_map() (+24 more)

### Community 47 - "DiskAtomRef"
Cohesion: 0.14
Nodes (27): EndpointSide, CrossLevelOverflowStoreBuilder, decode_topology(), encode_endpoints(), encode_hot_endpoint_side(), encode_record_key(), encode_topology(), EncodedEndpoint (+19 more)

### Community 48 - "PageNo"
Cohesion: 0.11
Nodes (21): PageFlags, aligned_page_has_exact_size_and_alignment(), aligned_page_slice_is_one_contiguous_byte_extent(), page_checksum(), page_checksum_mut(), page_checksum_rejects_payload_corruption(), page_codec_rejects_wrong_page_number_and_invalid_raw_fields(), page_codec_round_trips_little_endian_header() (+13 more)

### Community 49 - "arrow.rs"
Cohesion: 0.12
Nodes (32): ArrayRef, ArrowError, DataType, arrow_failure(), arrow_round_trip_preserves_none_null_and_native_scalars(), build_array(), ColumnKind, decode_rows() (+24 more)

### Community 50 - "StatusReport"
Cohesion: 0.11
Nodes (20): IntoSO, applies_gql_precedence(), Outcome, Outcome<T>, precedence(), rejects_errors(), Display, Error (+12 more)

### Community 51 - ".new"
Cohesion: 0.12
Nodes (16): PageLsnTable, PersistentMvccStore, PersistLayer, Arc, AtomicU64, BufferPool, BufWriter, Mutex (+8 more)

### Community 52 - "Vector"
Cohesion: 0.10
Nodes (19): canonical_f32(), canonical_f64(), compare_f32(), compare_f32_value(), compare_f64(), compare_f64_value(), hash_f32(), hash_f64() (+11 more)

### Community 53 - "storage/buffer_pool.rs"
Cohesion: 0.08
Nodes (55): AtomicUsize, CachedPage, Condvar, Loader, AccessDecision, batch_handles_resident_loading_missing_and_duplicate_pages(), batch_preserves_order_duplicates_and_contiguous_extents(), BatchLoadReservation (+47 more)

### Community 54 - "Range"
Cohesion: 0.14
Nodes (26): End, borrow_bound(), Bound<T>, bound_value(), BoundExt, box_bound(), checks_inclusion(), compare_end() (+18 more)

### Community 55 - "GraphSuperblock"
Cohesion: 0.15
Nodes (28): GraphSuperblockFlags, CheckpointAuthority, RapidHashMap, database_fixture(), database_superblock_round_trip_matches_layout(), DatabaseSuperblock, graph_fixture(), graph_superblock_round_trip_matches_layout() (+20 more)

### Community 56 - "storage/direct.rs"
Cohesion: 0.11
Nodes (49): OpenOptions, checked_file_length(), checked_page_extent(), checked_page_offset(), configure_direct_io(), create_new_never_overwrites_existing_file(), direct_file_batch_rejects_range_and_page_corruption(), direct_file_reads_and_validates_one_contiguous_extent() (+41 more)

### Community 57 - "graph/traversal.rs"
Cohesion: 0.18
Nodes (34): algorithm_failure(), bellman_ford(), bellman_ford_reports_negative_cycle_as_domain_error(), bfs_topology_and_components_are_correct(), binary_edges(), BinaryEdge, collect_node_ids(), collect_weights() (+26 more)

### Community 58 - "benchmarks.rs"
Cohesion: 0.07
Nodes (91): Barrier, Mmap, MmapMut, Read, aggregate_read_samples(), aligned_pages(), apply_gate(), BackendResult (+83 more)

### Community 59 - "heap_estimator.rs"
Cohesion: 0.12
Nodes (13): Box, HashMap, K, S, String, T, V, Vec (+5 more)

### Community 60 - "order_key.rs"
Cohesion: 0.21
Nodes (28): append_escaped(), composite_strings_escape_zero_without_losing_lexicographic_order(), encode(), encode_decimal(), encode_duration(), encode_float(), encode_index_key(), encode_range() (+20 more)

### Community 61 - ".expr"
Cohesion: 0.18
Nodes (16): analyze(), Analyzer, Analyzer<'a>, compatible(), is_integer(), is_number(), Box, BTreeMap (+8 more)

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

### Community 66 - "Result"
Cohesion: 0.18
Nodes (14): atom_id_from(), BytesPersistentStore, checkpoint_then_recover(), crc32(), decode_isolation(), encode_isolation(), PersistentMvccStore<T, C>, AtomId (+6 more)

### Community 67 - "Object"
Cohesion: 0.17
Nodes (10): Keys, Object, BTreeMap, FromIterator, IterMut, K, Option, Text (+2 more)

### Community 68 - "get_metadata_inner"
Cohesion: 0.10
Nodes (23): add_m, Attrs, measurable_inner(), occurrence_error(), add, DeriveInput, Error, Parse (+15 more)

### Community 69 - "StorageBackend"
Cohesion: 0.10
Nodes (21): AsyncIoRingEngine, Arc, Default, ThreadPoolEngine, DirectFile, Any, AtomicU64, PathBuf (+13 more)

### Community 70 - ".run"
Cohesion: 0.25
Nodes (14): decodes_quoted_names_parameters_and_string_escapes(), is_ident_continue(), is_ident_start(), keeps_utf8_byte_spans_and_greedy_operators(), lex(), Lexer, parse_error(), Box (+6 more)

### Community 71 - "PropertyBatch"
Cohesion: 0.16
Nodes (15): BooleanBuffer, imported_arrow_batch_requires_property_metadata(), invalid_batch(), PropertyBatch, BTreeMap, FerrosGrynnError, FerrosGrynnResult, Into (+7 more)

### Community 72 - ".new"
Cohesion: 0.13
Nodes (24): GraphCatalogEntry, Lsn, BuiltGraphImage, copy_local_index_page(), graph_file_ids(), graph_file_paths(), load_database(), load_graph() (+16 more)

### Community 73 - "HashValue"
Cohesion: 0.12
Nodes (14): AddAssign, ConstOne, Mul, MulAssign, One, HashValue, Add, Display (+6 more)

### Community 74 - "WorldComponents"
Cohesion: 0.09
Nodes (22): AnyPool, atom(), remove_atom_cleans_every_pool_and_respects_generation(), Any, AtomId, Box, FerrosGrynnResult, FnMut (+14 more)

### Community 75 - ".new"
Cohesion: 0.27
Nodes (7): IoBatch, IoRequest, Self, Vec, tag_routing(), threadpool_write_read(), Vec

### Community 76 - "Values"
Cohesion: 0.12
Nodes (17): DagView, ForestView, GraphView, Dag<State>, DagView<'graph, R>, directed_forest_view(), Forest<State>, RootedTree<State> (+9 more)

### Community 77 - "AlignedPage"
Cohesion: 0.12
Nodes (70): build_primary_index(), build_scalar_index(), BuiltPrimaryIndex, BuiltScalarIndex, bulk_primary_tree_supports_multiple_levels_and_exact_lookup(), ChildPage, decode_dictionary_chain(), decode_label_dictionary() (+62 more)

### Community 78 - "StatusWireV1"
Cohesion: 0.19
Nodes (17): completion_omits_severity_and_classification(), DiagnosticRecordWireV1, FerrosGrynnError, labeled_location_to_wire(), LabeledLocationWireV1, location_to_wire(), LocationWireV1, Box (+9 more)

### Community 79 - ".append"
Cohesion: 0.21
Nodes (12): ActiveSegment, io_failure(), list_segments(), recover_directory(), AsRef, BufWriter, FerrosGrynnError, Into (+4 more)

### Community 80 - "PropertyDictionary"
Cohesion: 0.13
Nodes (14): database_property_dictionary(), LocalCommitResult, assigns_dense_ids_and_resolves_both_directions(), interning_is_idempotent_and_iteration_uses_id_order(), PropertyDictionary, ExactSizeIterator, FerrosGrynnResult, IntoText (+6 more)

### Community 81 - "local.rs"
Cohesion: 0.17
Nodes (36): Decode, KeyOf, atomic_replace(), Candidate, corrupt_temporary_is_removed_after_main_selection(), database(), database_lock_is_exclusive_for_its_lifetime(), equal_candidate_clocks_require_identical_bytes() (+28 more)

### Community 82 - "DiagnosticValue"
Cohesion: 0.13
Nodes (12): Str, bool, DiagnosticValue, f32, f64, BTreeMap, String, StatusParameter (+4 more)

### Community 83 - "EdgeEndpoint"
Cohesion: 0.17
Nodes (14): EdgeEndpoint, From, deduplicated_steps(), dfs(), endpoint_kind(), local_steps(), AtomKind, DoubleEndedIterator (+6 more)

### Community 84 - "AccumList"
Cohesion: 0.15
Nodes (15): AccumList, FieldAccumArgs, Attribute, DeriveInput, Ident, Option, Parse, ParseStream (+7 more)

### Community 85 - "mvcc_persist.rs"
Cohesion: 0.19
Nodes (20): CheckpointHeader, closure_codec_with_capture_in_persistent_store(), multi_segment_reader(), prune_before_removes_old_segments(), recover(), recovery_restores_atom_meta(), RecoveryResult, rm() (+12 more)

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

### Community 90 - "CowVec<T, CHUNK_LEN>"
Cohesion: 0.16
Nodes (11): CowVec<T, CHUNK_LEN>, Iter, Arc, Default, From, Index, IndexMut, Option (+3 more)

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
Cohesion: 0.16
Nodes (10): BinaryLiftingIndex, QueueEntry, Eq, FnMut, Ord, Ordering, PartialEq, PartialOrd (+2 more)

### Community 95 - "record_id.rs"
Cohesion: 0.21
Nodes (15): JsonValue, assert_status(), enforces_the_composite_nesting_limit(), quotes_ambiguous_string_ids_for_round_trip(), recursively_rejects_unsupported_parts(), rejects_empty_components(), Err, FerrosGrynnError (+7 more)

### Community 96 - "TextSpan"
Cohesion: 0.28
Nodes (20): TextSpan, Expr, ExprKind, Identifier, LetStatement, Literal, ObjectField, Program (+12 more)

### Community 97 - "HashFunction"
Cohesion: 0.21
Nodes (3): IncrementalXXH64, HashFunction, UtilHash

### Community 98 - ".new"
Cohesion: 0.34
Nodes (19): cataloged_database_create_reopen_and_exclusive_create(), concurrent_checkpoints_publish_once(), marked_page(), mixed_batch_preserves_sources_order_and_duplicates(), mixed_batch_rejects_more_unique_pages_than_pool_capacity(), old_reader_survives_checkpoint_and_next_checkpoint_advances(), open_rejects_file_instance_clock_reuse(), partial_graph_generation_is_not_database_authority() (+11 more)

### Community 99 - "FerrosGrynnResult"
Cohesion: 0.14
Nodes (17): build_graph_image(), build_scalar_indexes(), checked_page_index(), checked_u32(), checkpoint_due(), DecodedGraph, dense_ids(), encode_optional_properties() (+9 more)

### Community 100 - "Frozen"
Cohesion: 0.18
Nodes (12): benchmark_access_modes(), benchmark_path_and_live_publication(), benchmark_state_and_family_conversions(), benchmark_traversal(), dag_chain(), general_chain(), path(), AtomId (+4 more)

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
Cohesion: 0.10
Nodes (17): &'a Array, &'a mut Array, Array, From, FromIterator, I, IntoIter, IntoIterator (+9 more)

### Community 110 - "EdgeColumns"
Cohesion: 0.08
Nodes (18): CowVec, EdgeColumns, empty_topology(), ensure_unique_record(), GraphMetadata, NodeColumns, AtomId, BTreeSet (+10 more)

### Community 111 - "fg-meta/src/lib.rs"
Cohesion: 0.09
Nodes (17): EdgeIncidence, EdgeInfo, EdgeRole, EdgeTopology, AtomId, Box, GraphId, Option (+9 more)

### Community 112 - "Specification Design Authority"
Cohesion: 0.33
Nodes (6): Specification Design Authority, AtomWalk Syntax, MATCH Graph Patterns, GrynQL EBNF Grammar, Context-Sensitive Parser Disambiguation, GrynQL Statements

### Community 113 - "Persistent MVCC v2"
Cohesion: 0.33
Nodes (6): ClosureCodec, Exact Phantom Matching, Persistent MVCC v2, Serializable Snapshot Isolation, GrynQL Transaction Model, Execution MVCC and Snapshot Contract

### Community 114 - "Unified Atom Incidence"
Cohesion: 0.33
Nodes (6): Unified Atom Incidence, Graph Storage Families, Ferros-Grynn Graph Taxonomy, Mutable Frozen Type-State and Views, P4 Full Atom Incidence, GraphWorkspace ECS Sidecar

### Community 115 - "exact_ops.rs"
Cohesion: 0.17
Nodes (19): CheckedAdd, CheckedMul, CheckedSub, PrimInt, Signed, integer_arithmetic(), add_exact(), AddExact (+11 more)

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
Cohesion: 0.14
Nodes (16): AtomVersions, DatabaseRoot, Datastore, PredicateEvaluator, PredicateMatcher, PredicateRead, QuerySnapshot, ReadKey (+8 more)

### Community 123 - "Canonical Storage Value Codec"
Cohesion: 0.50
Nodes (4): Canonical Storage Value Codec, OLTP Property Heap and Column Cache, Runtime Storage Columnar Boundary, StorageValue and Canonical Codec

### Community 124 - "P3 Query Execution"
Cohesion: 0.50
Nodes (4): P3 Query Execution, P2 Query Frontend, P2 Storage WAL and MVCC, P1 Value Foundation

### Community 125 - "CheckpointGeneration"
Cohesion: 0.08
Nodes (12): CheckpointGeneration, CommitTs, data_file_id_layout_is_stable_and_validated(), Lsn, FerrosGrynnResult, Self, StorageSnapshot, TxId (+4 more)

### Community 126 - "&'a Bytes"
Cohesion: 0.50
Nodes (3): &'a Bytes, IntoIter, IntoIterator

### Community 135 - "StorageValue"
Cohesion: 0.08
Nodes (33): array_storage_conversion_reuses_owned_elements(), conversion_context_rejects_conflicting_reference_mappings(), datetime_conversion_preserves_pre_epoch_nanos_and_time_zone_kind(), DecodeContext, DiskGraphRef, graph_references_use_explicit_context_mapping(), QualifiedAtomRef, rejects_runtime_only_values_and_missing_references() (+25 more)

### Community 136 - "StatusObject"
Cohesion: 0.10
Nodes (19): Deserialize, Ok, Serialize, InvalidSeverity, D, Display, Err, Error (+11 more)

### Community 137 - "Uuid"
Cohesion: 0.10
Nodes (13): InnerUlid, InnerUuid, Display, Err, Formatter, From, FromStr, Result (+5 more)

### Community 138 - "MaterializedGraph"
Cohesion: 0.08
Nodes (20): MaterializedGraph, preserves_exact_discriminant_in_generic_transport(), AtomId, AtomKind, Cow, Edge, EdgeIds, Eq (+12 more)

### Community 139 - "execution.rs"
Cohesion: 0.09
Nodes (56): QueryCatalog, arithmetic(), binary(), binds_let_filters_projects_and_limits(), build_cursor(), cast(), collect_scalar_constraints(), decimal_arithmetic() (+48 more)

### Community 140 - "Lsn"
Cohesion: 0.12
Nodes (16): RwLockReadGuard, GraphId, PageKey, FrameLocation, FrameVersion, FrameWal, PendingTransaction, PublishedState (+8 more)

### Community 141 - "DirtyTarget"
Cohesion: 0.06
Nodes (56): ApplyFn, ApplyUpdate, MutexGuard, ReadFn, ReplayFn, accept_remote_discards_local_conflicting_state(), apply_or_pause(), conflicted_live_graph() (+48 more)

### Community 142 - "datastore.rs"
Cohesion: 0.07
Nodes (29): ActiveSnapshot, ActiveSnapshot, CheckpointMode, CheckpointPolicy, CommitResult, DatastoreOptions, InMemoryOptions, IoMode (+21 more)

### Community 143 - "Type"
Cohesion: 0.10
Nodes (11): is_option(), AtomKind, Box, Display, Formatter, Result, Self, Text (+3 more)

### Community 144 - "TxHandle"
Cohesion: 0.13
Nodes (14): AtomicBool, ActiveSnapshot, FrameWalInner, Arc, AtomicU64, BTreeMap, Drop, Instant (+6 more)

### Community 145 - "QueryResult"
Cohesion: 0.11
Nodes (19): Config, Default, Self, parse(), QueryResult, QueryStatementKind, FerrosGrynnResult, Value (+11 more)

### Community 147 - "checkpoint_prunes_closed_prefix_and_seeded_recovery_continues_clocks"
Cohesion: 0.33
Nodes (3): checkpoint_prunes_closed_prefix_and_seeded_recovery_continues_clocks(), CheckpointCommit, WalClocks

### Community 148 - "ir.rs"
Cohesion: 0.21
Nodes (15): Column, LogicalNode, LogicalNodeKind, LogicalProgram, LogicalSelect, LogicalStatement, NodeId, Box (+7 more)

### Community 149 - ".open_cataloged"
Cohesion: 0.13
Nodes (20): DataFileRegistration, DatabaseLock, File, aligned_extent(), CreatedDatabaseDirectory, local_io(), recovery_seed(), AsRef (+12 more)

### Community 150 - "AtomId"
Cohesion: 0.16
Nodes (12): bfs(), Condensation, CutFrame, CutSet, Predecessor, AtomId, Box, Option (+4 more)

### Community 151 - "collections.rs"
Cohesion: 0.20
Nodes (14): batch_random_access(), bench_collect(), bench_iter_sequential_borrowed(), bench_iter_sequential_owned(), bench_iter_sequential_with_step(), bench_push_preallocated(), bench_push_raw(), LargeData (+6 more)

### Community 152 - "Iter<'_, T>"
Cohesion: 0.67
Nodes (3): Iter<'_, T>, ExactSizeIterator, FusedIterator

### Community 153 - ".decode"
Cohesion: 0.25
Nodes (13): all_format_v1_tags_round_trip(), archive_envelope_checks_magic_version_schema_length_and_checksum(), assert_status(), canonical_float_encoding_rejects_signed_zero_and_nan_payloads(), DecodeLimits, Decoder, duration_rejects_reserved_bits_zero_units_and_negative_zero(), enforces_depth_length_and_allocation_limits_before_reserve() (+5 more)

### Community 154 - "FerrosGrynnResult"
Cohesion: 0.12
Nodes (21): atom_label_mut(), atom_properties_mut(), AtomRefTopology, directed_pair(), forest_pair(), GraphMutationView, merge_materialized(), MutableGraphValue (+13 more)

### Community 155 - "DiskGraphView<'_, R>"
Cohesion: 0.18
Nodes (3): DiskGraphView<'_, R>, GraphReadView<'_, R>, EdgeIds

### Community 156 - "Option"
Cohesion: 0.16
Nodes (10): EdgeView, EdgeView<'a>, NodeView, Cow, EdgeTopology, FerrosGrynnResult, Number, Option (+2 more)

### Community 157 - "Q: What is MetaGraph?"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: What is MetaGraph?, Source Nodes

### Community 158 - "TableIndex"
Cohesion: 0.27
Nodes (5): Fields, ExactSizeIterator, Item, TableIndex, TableIndexKind

### Community 159 - "Handle<'_, T>"
Cohesion: 0.50
Nodes (3): Handle<'_, T>, Deref, Target

### Community 160 - "Отчёт этапа 5: Windows sync storage completion"
Cohesion: 0.07
Nodes (24): Итоговая архитектура, Контракты и публичный API, Локальное дисковое хранилище Ferros-Grynn: sync-first, Проверки и критерии приёмки, Этапы реализации, Явно вне начального scope, Исправленный benchmark, Калибровка batch (+16 more)

### Community 161 - "RecordIdPart"
Cohesion: 0.24
Nodes (8): RecordId, RecordIdPart, Display, Formatter, FromStr, Result, Text, string_id_needs_quotes()

### Community 162 - ".insert"
Cohesion: 0.17
Nodes (7): BTreeMap<Text, Value>, has_canonical_key_order_independent_of_insertion(), From, I, IntoText, IntoValue, Self

### Community 163 - ".push"
Cohesion: 0.36
Nodes (3): clone_copies_only_the_changed_chunk(), FnMut, Self

### Community 164 - ".into_iter"
Cohesion: 0.38
Nodes (4): &'a mut Object, &'a Object, IntoIter, IntoIterator

### Community 165 - ".call"
Cohesion: 0.33
Nodes (4): FerrosGrynnError, Formatter, Result, Value

### Community 166 - "Iter<'a, T>"
Cohesion: 0.40
Nodes (3): Iter<'a, T>, Item, Iterator

### Community 167 - "Graph"
Cohesion: 0.07
Nodes (34): GraphWriteView, constraint(), Forest, Forest<Frozen>, Forest<State>, graph_to_tree_validates_classic_topology(), RootedTree, RootedTree<Frozen> (+26 more)

### Community 168 - "Value"
Cohesion: 0.17
Nodes (8): Box, From, FxHashMap, Option, Self, String, Vec, Value

### Community 169 - "LocalGraph"
Cohesion: 0.14
Nodes (33): TableKind, collect_local_ids(), copy_local_page(), decode_cross_level_side(), decode_local_topology(), decode_record_key(), find_qualified_atom(), local_atom_id() (+25 more)

### Community 170 - "async_io.rs"
Cohesion: 0.10
Nodes (33): AsyncIoEngine, AsyncUringEngine, BatchEntry, bg_loop(), blocking_op(), build(), build_ring(), bump_memlock() (+25 more)

### Community 171 - "StorageRecordValue"
Cohesion: 0.24
Nodes (9): BTreeMap, ExactSizeIterator, Item, Option, Text, Vec, StorageRecordIdPart, StorageRecordValue (+1 more)

### Community 172 - "read.rs"
Cohesion: 0.13
Nodes (20): assert_snapshot_released(), cascade_removal_marks_removed_incident_edges_dirty(), disk_view_and_hybrid_materialization_have_the_same_read_result(), EdgeRead, FakeTransaction, GraphRefReader, GraphWriteBack, mutable_hybrid_flush_uses_origin_mapping_and_dirty_set() (+12 more)

### Community 173 - "PageRead"
Cohesion: 0.50
Nodes (3): collect_page_reads(), PageRead, Option

### Community 174 - "AtomIds"
Cohesion: 0.21
Nodes (8): Inner, AtomIds, FallibleAtomIds, FallibleAtomIds<Inner>, ExactSizeIterator, FusedIterator, Item, Iterator

### Community 175 - "SnapshotId"
Cohesion: 0.10
Nodes (11): AtomIds<'a>, DiskGraphView, DiskGraphView<'tx, R>, GraphReadView, GraphReadView<'tx, R>, Display, Formatter, R (+3 more)

### Community 176 - "Option"
Cohesion: 0.16
Nodes (12): decode_label(), DecodedEdge, DecodedNode, LocalEdgeRead, LocalNodeRead, materialize_graph(), read_local_properties(), Cow (+4 more)

### Community 177 - "NoHeapEstimatorCache"
Cohesion: 0.19
Nodes (8): HeapEstimatorCache, NoHeapEstimatorCache, Self, Measurable, HashMap, K, V, size_of_hash_map()

### Community 178 - ".open"
Cohesion: 0.38
Nodes (5): LocalDatabaseSnapshot, LocalSnapshotGraph, NonZeroUsize, PathLike, Self

### Community 180 - "GraphWorkspace<G>"
Cohesion: 0.13
Nodes (15): GraphWorkspace<G>, AtomId, Cow, Debug, Edge, EdgeIds, FerrosGrynnResult, Formatter (+7 more)

### Community 181 - "join.rs"
Cohesion: 0.57
Nodes (6): A, atom(), B, chooses_actual_smallest_pool_independent_of_type_order(), missing_pool_produces_empty_join(), AtomId

### Community 182 - ".next"
Cohesion: 0.50
Nodes (3): BoxedEnumIter<'a>, Item, Iterator

### Community 183 - "GraphRead"
Cohesion: 0.16
Nodes (14): GraphRead, canonical_mutation_invalidates_derived_and_removes_dead_components(), component_mutation_does_not_change_graph_revision(), G, GraphWorkspace, IntoGraphWorkspace, map_graph_moves_user_components_but_discards_derived_pools(), PageRank (+6 more)

### Community 184 - "UringState"
Cohesion: 0.17
Nodes (11): BufRingEntry, BufRing, FileSlot, IoUringEngine, Drop, Mutex, RawFd, Send (+3 more)

### Community 187 - "atom_does_not_exist"
Cohesion: 0.09
Nodes (33): INLINE_INCIDENCE, atom_does_not_exist(), endpoint_kind(), freeze_thaw_preserves_identity_ids_and_topology(), GeneralStorage, Graph<Mutable>, merge_transaction_atom(), merge_transaction_graph() (+25 more)

### Community 195 - "RegisteredDataFile"
Cohesion: 0.17
Nodes (14): FileRegistry, open_registration(), RegisteredDataFile, Arc, DirectFile, FerrosGrynnResult, GraphId, Option (+6 more)

### Community 205 - "Metagraph<State>"
Cohesion: 0.06
Nodes (37): exposes_explicit_meta_atom_api_over_general_layout(), Metagraph<Frozen>, Metagraph<Mutable>, Metagraph<State>, AtomId, AtomKind, Clone, Cow (+29 more)

### Community 207 - "SyncPageManager"
Cohesion: 0.15
Nodes (9): Arc, BufferPool, FerrosGrynnResult, GraphId, IntoIterator, Item, Mutex, RwLock (+1 more)

### Community 251 - "storage/store.rs"
Cohesion: 0.14
Nodes (29): allocate_file_id(), allocate_graph_id(), build_graph_images(), BuiltGraphGeneration, BuiltScalarIndexImage, BuiltTableIndexBinding, encode_tables(), graph_tree_ids() (+21 more)

### Community 253 - "MaterializationOrigin"
Cohesion: 0.11
Nodes (6): AtomRef, MaterializationOrigin, NodeView<'a>, AtomKind, BTreeMap, RecordId

### Community 261 - "Graph<State>"
Cohesion: 0.07
Nodes (24): Copied, copy_from_read(), Graph<Frozen>, Graph<State>, invalid_edges_are_transactional_and_use_graph_statuses(), AtomKind, BTreeSet, Clone (+16 more)

### Community 271 - "AtomId"
Cohesion: 0.33
Nodes (3): AtomId, Edge, Node

## Knowledge Gaps
- **79 isolated node(s):** `Position2D`, `Position3D`, `Timestamp`, `ColorTag`, `Velocity` (+74 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **6 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Type` connect `Type` to `.new`, `catalog.rs`, `.new`, `execution.rs`, `Closure`, `order_key.rs`, `ir.rs`, `status_codes.rs`, `FerrosGrynnResult`, `storage/store.rs`, `Value`, `.expr`, `GraphRef`?**
  _High betweenness centrality (0.143) - this node is a cross-community bridge._
- **Why does `Graph` connect `Graph` to `Dag<State>`, `FerrosGrynnResult`, `Frozen`, `Graph<State>`, `Hypergraph<State>`, `MaterializedGraph`, `read.rs`, `atom_does_not_exist`, `EdgeColumns`, `DirtyTarget`, `Option`, `Metagraph<State>`, `Value`, `Subgraph<State>`, `FerrosGrynnResult`, `storage/store.rs`, `Option`?**
  _High betweenness centrality (0.123) - this node is a cross-community bridge._
- **Why does `PropertyKeyId` connect `PropertyKeyId` to `.new`, `PropertyBatch`, `.new`, `PropertyColumn<T>`, `WorldComponents`, `LocalGraph`, `StorageValue`, `benches/property_map.rs`, `FerrosGrynnResult`, `PropertyDictionary`, `arrow.rs`, `.register_datafusion_table`, `FerrosGrynnResult`?**
  _High betweenness centrality (0.095) - this node is a cross-community bridge._
- **What connects `Position2D`, `Position3D`, `Timestamp` to the rest of the system?**
  _79 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `AtomId` be split into smaller, more focused modules?**
  _Cohesion score 0.11538461538461539 - nodes in this community are weakly interconnected._
- **Should `Dag<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.0530812324929972 - nodes in this community are weakly interconnected._
- **Should `Hypergraph<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.05797979797979798 - nodes in this community are weakly interconnected._