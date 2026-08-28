# Graph Report - ferros-grynn  (2026-08-29)

## Corpus Check
- 172 files · ~303,170 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 6532 nodes · 20410 edges · 199 communities (187 shown, 12 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 192 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `8ceb890d`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- Parser
- AtomId
- FerrosGrynnResult
- Hypergraph<State>
- MetaGraph
- Metagraph<State>
- catalog.rs
- MvccTransaction
- AtomId
- AtomId
- PageId
- graph_pages.rs
- .new
- page_io.rs
- FerrosGrynnResult
- Path
- DiskAtomRef
- win_async.rs
- Number
- src/ecs.rs
- wal.rs
- StatusObject
- Subgraph<State>
- store.rs
- status_codes.rs
- PropertyKeyId
- EdgeEndpoint
- AtomId
- runtime/mod.rs
- Position
- TableSchema
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
- PageNo
- arrow.rs
- StatusReport
- Lsn
- Vector
- storage/buffer_pool.rs
- Range
- GraphSuperblock
- storage/direct.rs
- graph/traversal.rs
- benchmarks.rs
- heap_estimator.rs
- order_key.rs
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
- storage/store.rs
- HashValue
- WorldComponents
- .new
- Self
- AlignedPage
- StatusWireV1
- FerrosGrynnResult
- PropertyDictionary
- TxHandle
- DiagnosticValue
- TraversalIndex
- AccumList
- Frozen
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
- .kind
- LocalGraph
- Graph
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
- CowVec<T, CHUNK_LEN>
- GraphMetadata
- hashing/mod.rs
- XorShift32HashFunction
- MetaGraphError
- laws.rs
- ferros-grynn-macros/src/lib.rs
- Hybrid Query Model
- Transaction
- SyncPageManager
- P3 Query Execution
- PageKey
- &'a Bytes
- .fmt
- Direct I/O and Clock-Pro
- Redacted External Error Sources
- GraphRef
- StatusCode
- Uuid
- MaterializedGraph
- execution.rs
- Item
- DirtyTarget
- datastore.rs
- Dag<State>
- Value
- QueryResult
- RecordIdPart
- ir.rs
- Self
- AtomId
- .connect
- Iter<'_, T>
- exact_ops.rs
- FerrosGrynnResult
- GraphStorageMode
- Option
- Q: What is MetaGraph?
- Option
- Handle<'_, T>
- Отчёт этапа 5: Windows sync storage completion
- AtomId
- .new
- .open_cataloged
- FerrosGrynnResult
- SnapshotId
- TestDirectory
- memory.rs
- Value
- Lsn
- Result
- Values
- read.rs
- decode_page_chain
- Vec
- RecordId
- AsyncUringEngine
- NoHeapEstimatorCache
- Iter<'a, T>
- NodeColumns
- GraphWorkspace<G>
- Q: Как изменения этапа 7 связывают лимит write-set, индекс RecordId и публикацию disk-backed local graph?
- PageRead
- SlotAllocator
- async_io.rs
- .publish_graph_authority
- atom_does_not_exist
- AlignedBuffer
- Unified Atom Incidence
- RegisteredDataFile
- AtomIds
- Canonical Storage Value Codec
- BufRing
- GraphKind
- .next
- .fmt
- .as_any
- .raw_fd
- Graph<State>

## God Nodes (most connected - your core abstractions)
1. `MetaGraph` - 153 edges
2. `PageNo` - 138 edges
3. `AlignedPage` - 134 edges
4. `Value` - 90 edges
5. `Graph` - 88 edges
6. `Uuid` - 65 edges
7. `Frozen` - 64 edges
8. `DiskAtomRef` - 64 edges
9. `Transaction` - 61 edges
10. `LocalGraph` - 61 edges

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
- 1-file cycle: `src/storage/page_io.rs -> src/storage/page_io.rs`
- 1-file cycle: `src/values/runtime/graph/general.rs -> src/values/runtime/graph/general.rs`
- 1-file cycle: `src/values/runtime/regex.rs -> src/values/runtime/regex.rs`

## Hyperedges (group relationships)
- **GrynQL Contract Stack** — docs_gql_spec_grynql_language, docs_gql_grammar_grynql_grammar, docs_gql_tech_spec_query_engine [INFERRED 0.95]
- **Storage Persistence Contract** — docs_disk_storage_spec_frame_wal, docs_disk_storage_spec_checkpoint_recovery, docs_gql_tech_spec_page_manager, crates_fg_meta_implementation_notes_persistent_mvcc [INFERRED 0.85]
- **Values Runtime Storage Separation** — docs_values_plan_runtime_storage_boundary, docs_values_plan_storage_value, src_values_agents_runtime_invariants, src_values_agents_presence_bitmap [INFERRED 0.95]

## Communities (199 total, 12 thin omitted)

### Community 0 - "Parser"
Cohesion: 0.17
Nodes (13): Identifier, cover(), Symbol, Token, Parser, FerrosGrynnError, FerrosGrynnResult, Option (+5 more)

### Community 1 - "AtomId"
Cohesion: 0.12
Nodes (32): build_parent_map(), build_storage(), constraint(), find_root(), Forest<Mutable>, forest_node_removal_marks_every_incident_edge_dirty(), forest_rejects_cycles_without_mutation(), ForestStorage (+24 more)

### Community 2 - "FerrosGrynnResult"
Cohesion: 0.28
Nodes (16): CatalogDecoder, CatalogDecoder<'a>, decode_atom_kind(), decode_graph_kind(), decode_property_location(), decode_role(), decode_table_index_kind(), decode_table_kind() (+8 more)

### Community 3 - "Hypergraph<State>"
Cohesion: 0.06
Nodes (48): build_storage(), constraint(), graph_conversion_rejects_edge_endpoints(), Hypergraph, Hypergraph<Frozen>, Hypergraph<Mutable>, Hypergraph<State>, HyperStorage (+40 more)

### Community 4 - "MetaGraph"
Cohesion: 0.07
Nodes (18): MetaGraph, AtomId, AtomKind, Box, Default, EdgeTopology, GraphId, Option (+10 more)

### Community 5 - "Metagraph<State>"
Cohesion: 0.06
Nodes (37): exposes_explicit_meta_atom_api_over_general_layout(), Metagraph<Frozen>, Metagraph<Mutable>, Metagraph<State>, AtomId, AtomKind, Clone, Cow (+29 more)

### Community 6 - "catalog.rs"
Cohesion: 0.13
Nodes (31): TableIndexKind, atom_kind_byte(), BootstrapCatalogPages, CatalogPath, CatalogUpdatePages, DatabaseCatalog, encode_bootstrap_catalogs(), encode_catalog_update() (+23 more)

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

### Community 12 - ".new"
Cohesion: 0.15
Nodes (13): bootstrap_catalogs_round_trip_through_direct_file(), catalog_page_rejects_non_zero_reserved_bytes(), catalog_update_root_container_preserves_aligned_pages(), CatalogKind, encode_page_chain(), nested_catalog_reserves_local_slot_and_rejects_root_wide_collisions(), nested_graph_entry(), Error (+5 more)

### Community 13 - "page_io.rs"
Cohesion: 0.09
Nodes (39): c_void, IORING_HANDLE_REF, IORING_VERSION, checked_byte_len(), create_ring(), FilePageExtent, IoProfile, median_sample() (+31 more)

### Community 14 - "FerrosGrynnResult"
Cohesion: 0.08
Nodes (54): AlignedVec, all_format_v1_tags_round_trip(), archive_envelope_checks_magic_version_schema_length_and_checksum(), ArchiveEnvelope, assert_status(), canonical_f32(), canonical_float_encoding_rejects_signed_zero_and_nan_payloads(), CanonicalCodec (+46 more)

### Community 15 - "Path"
Cohesion: 0.07
Nodes (39): EndpointRole, Path, PathDirection, PathStep, PathTraceArena, PathTraceId, PathTraceNode, resolve_atom() (+31 more)

### Community 16 - "DiskAtomRef"
Cohesion: 0.10
Nodes (35): EndpointSide, build_graph_image(), build_scalar_indexes(), BuiltScalarIndexImage, checked_u32(), CrossLevelOverflowStoreBuilder, decode_topology(), DecodedGraph (+27 more)

### Community 17 - "win_async.rs"
Cohesion: 0.08
Nodes (39): cpu_affinity_cpu0(), goto_shutdown(), ioring_handle_ref(), ioring_raw_buffer_ref(), ioring_registered_buffer_ref(), new_fails_gracefully_if_no_ioring(), PendOp, probe_ioring() (+31 more)

### Community 18 - "Number"
Cohesion: 0.08
Nodes (30): BigInt, canonical_float(), canonical_fraction(), canonicalizes_nan_and_signed_zero_for_eq_ord_and_hash(), CanonicalNumber, conversion_error(), Decimal, f32 (+22 more)

### Community 19 - "src/ecs.rs"
Cohesion: 0.07
Nodes (34): AnyPool, ColorTag, FeatureVector, node_id(), NodeWeight, PageRank, Position2D, Position3D (+26 more)

### Community 20 - "wal.rs"
Cohesion: 0.14
Nodes (35): CheckpointEnd, decode_checkpoint_end(), decode_page_init(), decode_page_key(), decode_tx_commit(), ExactRead, file_instance_successor(), invalid_wal() (+27 more)

### Community 21 - "StatusObject"
Cohesion: 0.11
Nodes (15): Condition, Domain, ErrorCategory, ErrorClassification, NotificationClassification, From, Option, String (+7 more)

### Community 22 - "Subgraph<State>"
Cohesion: 0.10
Nodes (14): AtomKind, Debug, EdgeIds, Eq, Formatter, H, Hash, NodeIds (+6 more)

### Community 23 - "store.rs"
Cohesion: 0.10
Nodes (31): auto_checkpoint_triggers(), checkpoint_clears_wal(), compress_payload(), crc32(), crc32_update(), create_and_reopen(), decompress_payload(), delete_graph() (+23 more)

### Community 24 - "status_codes.rs"
Cohesion: 0.09
Nodes (29): define_status_codes(), is_string(), is_vec_string(), last_path_ident(), option_lit(), Ident, Option, Parse (+21 more)

### Community 25 - "PropertyKeyId"
Cohesion: 0.05
Nodes (41): &'a PropertyMap, clone_is_independent(), empty_map_is_pointer_sized_and_does_not_reserve_storage(), entry_ref(), equality_order_and_hash_ignore_insertion_order(), from_iterator_uses_last_duplicate_value(), keeps_entries_sorted_and_replaces_existing_values(), PropertyIter (+33 more)

### Community 26 - "EdgeEndpoint"
Cohesion: 0.09
Nodes (23): INLINE_ENDPOINTS, distinguishes_binary_hyper_and_cross_level_topology(), EdgeDirection, EdgeEndpoint, EdgeTopology, QualifiedAtomId, AtomId, Box (+15 more)

### Community 27 - "AtomId"
Cohesion: 0.17
Nodes (6): AtomId, Debug, Display, Formatter, Option, Result

### Community 28 - "runtime/mod.rs"
Cohesion: 0.11
Nodes (25): Closure, atom_and_path_have_distinct_value_contracts(), display_is_deterministic_and_escapes_strings(), eq_ord_and_hash_agree_for_cross_number_values(), estimated_bound_heap_bytes(), estimated_geometry_heap_bytes(), estimated_position_heap_bytes(), estimated_positions_heap_bytes() (+17 more)

### Community 29 - "Position"
Cohesion: 0.07
Nodes (35): GeoJsonGeometry, GeoJsonPosition, assert_status(), compare_float_slices(), GeoJsonValue, Geometry, into_position(), into_positions() (+27 more)

### Community 30 - "TableSchema"
Cohesion: 0.13
Nodes (12): ensure_store_reference(), graph_removal_preserves_table_catalog_integrity(), materialize_atom(), materialize_rows(), QuerySnapshot, QueryStorageRow, row_from_properties(), AtomId (+4 more)

### Community 31 - "src/traversal.rs"
Cohesion: 0.10
Nodes (42): BellmanFordResult, bellman_ford(), BellmanFordError, bfs(), bfs_with(), dfs(), dfs_full(), dijkstra() (+34 more)

### Community 32 - ".new"
Cohesion: 0.19
Nodes (32): atom_versions_keep_reused_slot_generations_distinct(), concurrent_writes_to_the_same_atom_fail_first_committer_wins(), configured_transaction_limits_have_domain_statuses(), Datastore, disjoint_atom_writes_merge(), graph_id(), local_graph_table_and_edge_survive_reopen(), local_large_properties_overflow_and_abort_round_trip() (+24 more)

### Community 33 - "Result"
Cohesion: 0.09
Nodes (26): AtomRef, bool, Closure, Graph<Frozen>, MaterializedGraph, Number, Path, RecordId (+18 more)

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
Cohesion: 0.07
Nodes (25): ClosureFunction, Func, NonZeroU64, calls_function_and_keeps_process_identity(), Closure, different_closures_have_different_identity(), rejects_wrong_arity_before_invocation(), Arc (+17 more)

### Community 45 - "DirectFile"
Cohesion: 0.22
Nodes (13): alloc_aligned(), DirectFile, platform_grow(), platform_open(), platform_open_readonly(), platform_write_page(), AtomicU64, File (+5 more)

### Community 46 - "benches/property_map.rs"
Cohesion: 0.12
Nodes (32): bench_build(), bench_iterate_and_clone(), bench_lookup(), boxed_enum_map(), BoxedEnumIter, BoxedEnumPropertyMap, BoxedEnumRepr, btree_map() (+24 more)

### Community 47 - "FrameWal"
Cohesion: 0.20
Nodes (6): RwLockReadGuard, FrameWal, ResolvedFrame, Option, visible_marker(), VisibleFrames

### Community 48 - "PageNo"
Cohesion: 0.11
Nodes (22): PageFlags, aligned_page_has_exact_size_and_alignment(), aligned_page_slice_is_one_contiguous_byte_extent(), page_checksum(), page_checksum_mut(), page_checksum_rejects_payload_corruption(), page_codec_rejects_wrong_page_number_and_invalid_raw_fields(), page_codec_round_trips_little_endian_header() (+14 more)

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
Nodes (21): vector_kind_byte(), canonical_f32(), canonical_f64(), compare_f32(), compare_f32_value(), compare_f64(), compare_f64_value(), hash_f32() (+13 more)

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
Cohesion: 0.10
Nodes (52): OpenOptions, checked_file_length(), checked_page_extent(), checked_page_offset(), configure_direct_io(), create_new_never_overwrites_existing_file(), direct_file_batch_rejects_range_and_page_corruption(), direct_file_reads_and_validates_one_contiguous_extent() (+44 more)

### Community 57 - "graph/traversal.rs"
Cohesion: 0.19
Nodes (31): algorithm_failure(), bellman_ford(), bellman_ford_reports_negative_cycle_as_domain_error(), bfs_topology_and_components_are_correct(), binary_edges(), BinaryEdge, collect_node_ids(), collect_weights() (+23 more)

### Community 58 - "benchmarks.rs"
Cohesion: 0.07
Nodes (95): Barrier, Mmap, MmapMut, Read, aggregate_read_samples(), aligned_pages(), apply_gate(), BackendResult (+87 more)

### Community 59 - "heap_estimator.rs"
Cohesion: 0.12
Nodes (13): Box, HashMap, K, S, String, T, V, Vec (+5 more)

### Community 60 - "order_key.rs"
Cohesion: 0.21
Nodes (28): append_escaped(), composite_strings_escape_zero_without_losing_lexicographic_order(), encode(), encode_decimal(), encode_duration(), encode_float(), encode_index_key(), encode_range() (+20 more)

### Community 61 - "Type"
Cohesion: 0.10
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

### Community 72 - "storage/store.rs"
Cohesion: 0.14
Nodes (31): allocate_graph_id(), build_graph_images(), BuiltGraphGeneration, BuiltTableIndexBinding, dense_ids(), encode_tables(), graph_tree_ids(), load_changed_graphs() (+23 more)

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
Cohesion: 0.14
Nodes (14): DagView, ForestView, GraphView, Dag<State>, DagView<'graph, R>, directed_forest_view(), Forest<State>, RootedTree<State> (+6 more)

### Community 77 - "AlignedPage"
Cohesion: 0.13
Nodes (69): build_primary_index(), build_scalar_index(), BuiltPrimaryIndex, BuiltScalarIndex, bulk_primary_tree_supports_multiple_levels_and_exact_lookup(), ChildPage, decode_dictionary_chain(), decode_label_dictionary() (+61 more)

### Community 78 - "StatusWireV1"
Cohesion: 0.20
Nodes (18): completion_omits_severity_and_classification(), DiagnosticRecordWireV1, error_classification(), FerrosGrynnError, labeled_location_to_wire(), LabeledLocationWireV1, location_to_wire(), LocationWireV1 (+10 more)

### Community 79 - "FerrosGrynnResult"
Cohesion: 0.17
Nodes (11): AtomId, Cow, Default, Edge, FerrosGrynnResult, GWB, Node, Option (+3 more)

### Community 80 - "PropertyDictionary"
Cohesion: 0.14
Nodes (13): database_property_dictionary(), assigns_dense_ids_and_resolves_both_directions(), interning_is_idempotent_and_iteration_uses_id_order(), PropertyDictionary, ExactSizeIterator, FerrosGrynnResult, IntoText, Item (+5 more)

### Community 81 - "TxHandle"
Cohesion: 0.13
Nodes (15): AtomicBool, ActiveSnapshot, FrameWalInner, PublishedState, Arc, AtomicU64, Drop, Instant (+7 more)

### Community 82 - "DiagnosticValue"
Cohesion: 0.14
Nodes (11): bool, DiagnosticValue, f32, f64, BTreeMap, String, StatusParameter, str (+3 more)

### Community 83 - "TraversalIndex"
Cohesion: 0.24
Nodes (14): bfs(), deduplicated_steps(), dfs(), local_arcs(), local_steps(), DoubleEndedIterator, Item, RapidHashMap (+6 more)

### Community 84 - "AccumList"
Cohesion: 0.15
Nodes (15): AccumList, FieldAccumArgs, Attribute, DeriveInput, Ident, Option, Parse, ParseStream (+7 more)

### Community 85 - "Frozen"
Cohesion: 0.11
Nodes (16): benchmark_access_modes(), benchmark_path_and_live_publication(), benchmark_state_and_family_conversions(), benchmark_traversal(), dag_chain(), general_chain(), path(), AtomId (+8 more)

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

### Community 90 - ".new"
Cohesion: 0.19
Nodes (39): ActiveSegment, checkpoint_end_layout_is_fixed_without_dirty_page_table(), checkpoint_end_requires_begin_and_durable_target(), commit_during_backfill_remains_for_next_checkpoint(), committed_frame_advances_file_instance_clock(), committed_frames_preserve_snapshot_versions_across_reopen(), concurrent_commits_publish_monotonic_clocks(), graph_superblock_is_not_a_transaction_frame() (+31 more)

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
Cohesion: 0.17
Nodes (36): Decode, KeyOf, atomic_replace(), Candidate, corrupt_temporary_is_removed_after_main_selection(), database(), database_lock_is_exclusive_for_its_lifetime(), equal_candidate_clocks_require_identical_bytes() (+28 more)

### Community 96 - "TextSpan"
Cohesion: 0.28
Nodes (20): TextSpan, Expr, ExprKind, Identifier, LetStatement, Literal, ObjectField, Program (+12 more)

### Community 97 - "HashFunction"
Cohesion: 0.21
Nodes (3): IncrementalXXH64, HashFunction, UtilHash

### Community 98 - ".kind"
Cohesion: 0.17
Nodes (5): LocalGraphView, record_for_local_atom(), AtomRef, Edge, Node

### Community 99 - "LocalGraph"
Cohesion: 0.20
Nodes (29): TableKind, collect_local_ids(), copy_local_index_page(), copy_local_page(), decode_cross_level_side(), decode_local_topology(), decode_record_key(), find_qualified_atom() (+21 more)

### Community 100 - "Graph"
Cohesion: 0.07
Nodes (31): GraphWriteView, Forest, Forest<Frozen>, Forest<State>, graph_to_tree_validates_classic_topology(), RootedTree, RootedTree<Frozen>, RootedTree<State> (+23 more)

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
Cohesion: 0.19
Nodes (10): EdgeColumns, empty_topology(), ensure_unique_record(), AtomId, EdgeTopology, Number, Option, PropertyMap (+2 more)

### Community 111 - "fg-meta/src/lib.rs"
Cohesion: 0.10
Nodes (17): EdgeIncidence, EdgeInfo, EdgeRole, EdgeTopology, AtomId, Box, GraphId, Option (+9 more)

### Community 112 - "Specification Design Authority"
Cohesion: 0.33
Nodes (6): Specification Design Authority, AtomWalk Syntax, MATCH Graph Patterns, GrynQL EBNF Grammar, Context-Sensitive Parser Disambiguation, GrynQL Statements

### Community 113 - "Persistent MVCC v2"
Cohesion: 0.33
Nodes (6): ClosureCodec, Exact Phantom Matching, Persistent MVCC v2, Serializable Snapshot Isolation, GrynQL Transaction Model, Execution MVCC and Snapshot Contract

### Community 114 - "CowVec<T, CHUNK_LEN>"
Cohesion: 0.13
Nodes (14): clone_copies_only_the_changed_chunk(), CowVec<T, CHUNK_LEN>, Iter, Arc, Default, FnMut, From, Index (+6 more)

### Community 115 - "GraphMetadata"
Cohesion: 0.15
Nodes (4): GraphMetadata, BTreeSet, Default, FerrosGrynnResult

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
Nodes (20): all_dirty(), build_record_index(), DatabaseRoot, GraphWrite, merge_materialized(), PredicateRead, ReadKey, Arc (+12 more)

### Community 123 - "SyncPageManager"
Cohesion: 0.16
Nodes (26): cataloged_database_create_reopen_and_exclusive_create(), concurrent_checkpoints_publish_once(), marked_page(), mixed_batch_preserves_sources_order_and_duplicates(), mixed_batch_rejects_more_unique_pages_than_pool_capacity(), old_reader_survives_checkpoint_and_next_checkpoint_advances(), open_rejects_file_instance_clock_reuse(), partial_graph_generation_is_not_database_authority() (+18 more)

### Community 124 - "P3 Query Execution"
Cohesion: 0.50
Nodes (4): P3 Query Execution, P2 Query Frontend, P2 Storage WAL and MVCC, P1 Value Foundation

### Community 125 - "PageKey"
Cohesion: 0.06
Nodes (14): PageLoadExtent, CommitTs, data_file_id_layout_is_stable_and_validated(), GraphId, Lsn, PageKey, FerrosGrynnResult, Option (+6 more)

### Community 126 - "&'a Bytes"
Cohesion: 0.50
Nodes (3): &'a Bytes, IntoIter, IntoIterator

### Community 135 - "GraphRef"
Cohesion: 0.07
Nodes (39): GraphRef, array_storage_conversion_reuses_owned_elements(), conversion_context_rejects_conflicting_reference_mappings(), datetime_conversion_preserves_pre_epoch_nanos_and_time_zone_kind(), DecodeContext, DiskGraphRef, EncodeContext, graph_references_use_explicit_context_mapping() (+31 more)

### Community 136 - "StatusCode"
Cohesion: 0.11
Nodes (16): Deserialize, Ok, Serialize, InvalidSeverity, D, Display, Err, Error (+8 more)

### Community 137 - "Uuid"
Cohesion: 0.10
Nodes (18): InnerUlid, InnerUuid, Display, Err, Formatter, From, FromStr, Result (+10 more)

### Community 138 - "MaterializedGraph"
Cohesion: 0.08
Nodes (20): MaterializedGraph, preserves_exact_discriminant_in_generic_transport(), AtomId, AtomKind, Cow, Edge, EdgeIds, Eq (+12 more)

### Community 139 - "execution.rs"
Cohesion: 0.10
Nodes (52): QueryCatalog, arithmetic(), binary(), binds_let_filters_projects_and_limits(), build_cursor(), cast(), collect_scalar_constraints(), decimal_arithmetic() (+44 more)

### Community 141 - "DirtyTarget"
Cohesion: 0.06
Nodes (56): ApplyFn, ApplyUpdate, MutexGuard, ReadFn, ReplayFn, accept_remote_discards_local_conflicting_state(), apply_or_pause(), conflicted_live_graph() (+48 more)

### Community 142 - "datastore.rs"
Cohesion: 0.07
Nodes (31): ActiveSnapshot, ActiveSnapshot, AtomVersions, CheckpointMode, CheckpointPolicy, CommitResult, DatastoreOptions, InMemoryOptions (+23 more)

### Community 143 - "Dag<State>"
Cohesion: 0.05
Nodes (52): build_oriented_tree_storage(), build_storage(), constraint(), Dag, Dag<Frozen>, Dag<Mutable>, Dag<State>, DagStorage (+44 more)

### Community 144 - "Value"
Cohesion: 0.10
Nodes (13): AtomRef, Box, Eq, From, Hash, MaterializedGraph, Option, Ord (+5 more)

### Community 145 - "QueryResult"
Cohesion: 0.10
Nodes (22): Config, Default, Self, execute_text(), execute_transaction(), Vec, parse(), QueryResult (+14 more)

### Community 147 - "RecordIdPart"
Cohesion: 0.13
Nodes (23): JsonValue, assert_status(), enforces_the_composite_nesting_limit(), quotes_ambiguous_string_ids_for_round_trip(), RecordId, RecordIdPart, recursively_rejects_unsupported_parts(), rejects_empty_components() (+15 more)

### Community 148 - "ir.rs"
Cohesion: 0.19
Nodes (16): query_schema(), Column, LogicalNode, LogicalNodeKind, LogicalProgram, LogicalSelect, LogicalStatement, NodeId (+8 more)

### Community 149 - "Self"
Cohesion: 0.36
Nodes (3): Ordering, R, Self

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
Cohesion: 0.14
Nodes (20): atom_label_mut(), atom_properties_mut(), AtomRefTopology, directed_pair(), forest_pair(), GraphMutationView, MutableGraphValue, AtomRef (+12 more)

### Community 156 - "Option"
Cohesion: 0.20
Nodes (9): EdgeView, NodeView, Cow, EdgeTopology, FerrosGrynnResult, Number, Option, PropertyMap (+1 more)

### Community 157 - "Q: What is MetaGraph?"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: What is MetaGraph?, Source Nodes

### Community 158 - "Option"
Cohesion: 0.16
Nodes (11): DecodedEdge, DecodedNode, LocalEdgeRead, LocalNodeRead, materialize_graph(), read_local_properties(), Cow, Number (+3 more)

### Community 159 - "Handle<'_, T>"
Cohesion: 0.50
Nodes (3): Handle<'_, T>, Deref, Target

### Community 160 - "Отчёт этапа 5: Windows sync storage completion"
Cohesion: 0.05
Nodes (39): Итоговая архитектура, Контракты и публичный API, Локальное дисковое хранилище Ferros-Grynn: sync-first, Проверки и критерии приёмки, Этапы реализации, Явно вне начального scope, Исправленный benchmark, Калибровка batch (+31 more)

### Community 161 - "AtomId"
Cohesion: 0.13
Nodes (8): AtomIds<'a>, DiskGraphView<'_, R>, GraphRead, GraphReadView<'_, R>, AtomId, Edge, EdgeIds, Node

### Community 163 - ".open_cataloged"
Cohesion: 0.13
Nodes (21): DataFileRegistration, DatabaseLock, File, PageIoMode, aligned_extent(), CreatedDatabaseDirectory, local_io(), recovery_seed() (+13 more)

### Community 164 - "FerrosGrynnResult"
Cohesion: 0.11
Nodes (36): GraphCatalogEntry, Lsn, allocate_file_id(), BuiltGraphImage, checked_page_index(), checkpoint_due(), decode_label(), encode_optional_properties() (+28 more)

### Community 165 - "SnapshotId"
Cohesion: 0.10
Nodes (9): DiskGraphView, DiskGraphView<'tx, R>, GraphReadView, GraphReadView<'tx, R>, MaterializationOrigin, BTreeMap, R, Self (+1 more)

### Community 166 - "TestDirectory"
Cohesion: 0.33
Nodes (4): Drop, Path, PathBuf, TestDirectory

### Community 167 - "memory.rs"
Cohesion: 0.12
Nodes (8): AtomicI64, allocate_memory(), compare_and_swap_i64(), compare_and_swap_u64(), get_and_add_i64(), get_and_set_i64(), AtomicU64, Result

### Community 168 - "Value"
Cohesion: 0.17
Nodes (8): Box, From, FxHashMap, Option, Self, String, Vec, Value

### Community 169 - "Lsn"
Cohesion: 0.12
Nodes (21): CheckpointGeneration, append_raw_commit(), append_raw_frame(), checkpoint_prunes_closed_prefix_and_seeded_recovery_continues_clocks(), CheckpointCommit, CheckpointTicket, encode_page_key(), FrameLocation (+13 more)

### Community 170 - "Result"
Cohesion: 0.22
Nodes (12): BatchEntry, blocking_op(), FileOp, IoCompletion, IoOp, Mem, Pending, Any (+4 more)

### Community 171 - "Values"
Cohesion: 0.29
Nodes (4): marks_materialized_result_without_new_storage_family(), super::Metagraph<State>, widening_views_do_not_rebuild_or_change_identity(), Values

### Community 172 - "read.rs"
Cohesion: 0.13
Nodes (20): assert_snapshot_released(), cascade_removal_marks_removed_incident_edges_dirty(), disk_view_and_hybrid_materialization_have_the_same_read_result(), EdgeRead, FakeTransaction, GraphRefReader, GraphWriteBack, mutable_hybrid_flush_uses_origin_mapping_and_dirty_set() (+12 more)

### Community 173 - "decode_page_chain"
Cohesion: 0.40
Nodes (6): decode_page_chain(), read_catalogs(), read_catalogs_with(), DirectFile, FnMut, validate_catalog_size()

### Community 174 - "Vec"
Cohesion: 0.14
Nodes (7): Fields, ExactSizeIterator, IntoText, Item, Vec, TableField, TableIndex

### Community 175 - "RecordId"
Cohesion: 0.11
Nodes (5): AtomRef, EdgeView<'a>, NodeView<'a>, AtomKind, RecordId

### Community 176 - "AsyncUringEngine"
Cohesion: 0.14
Nodes (12): AsyncUringEngine, bg_loop(), CtrlMsg, file_worker(), Drop, File, JoinHandle, PathBuf (+4 more)

### Community 177 - "NoHeapEstimatorCache"
Cohesion: 0.19
Nodes (8): HeapEstimatorCache, NoHeapEstimatorCache, Self, Measurable, HashMap, K, V, size_of_hash_map()

### Community 178 - "Iter<'a, T>"
Cohesion: 0.40
Nodes (3): Iter<'a, T>, Item, Iterator

### Community 179 - "NodeColumns"
Cohesion: 0.17
Nodes (5): CowVec, NodeColumns, Arc, RapidHashSet, SlotAllocator

### Community 180 - "GraphWorkspace<G>"
Cohesion: 0.08
Nodes (34): A, atom(), B, chooses_actual_smallest_pool_independent_of_type_order(), missing_pool_produces_empty_join(), AtomId, canonical_mutation_invalidates_derived_and_removes_dead_components(), component_mutation_does_not_change_graph_revision() (+26 more)

### Community 181 - "Q: Как изменения этапа 7 связывают лимит write-set, индекс RecordId и публикацию disk-backed local graph?"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: Как изменения этапа 7 связывают лимит write-set, индекс RecordId и публикацию disk-backed local graph?, Source Nodes

### Community 182 - "PageRead"
Cohesion: 0.50
Nodes (3): collect_page_reads(), PageRead, Option

### Community 183 - "SlotAllocator"
Cohesion: 0.27
Nodes (6): atom_id_uses_u40_slots_and_u23_generations(), AtomKind, FerrosGrynnResult, Self, slot_reuse_invalidates_stale_handles(), SlotAllocator

### Community 184 - "async_io.rs"
Cohesion: 0.14
Nodes (20): AsyncIoEngine, build(), build_ring(), bump_memlock(), create_engine(), create_engine_full(), EngineConfig, FileSlot (+12 more)

### Community 187 - "atom_does_not_exist"
Cohesion: 0.09
Nodes (34): INLINE_INCIDENCE, atom_does_not_exist(), endpoint_kind(), freeze_thaw_preserves_identity_ids_and_topology(), GeneralStorage, Graph<Mutable>, merge_transaction_atom(), merge_transaction_graph() (+26 more)

### Community 188 - "AlignedBuffer"
Cohesion: 0.17
Nodes (7): AlignedBuffer, free_aligned(), platform_read_page(), Drop, HANDLE, Send, Sync

### Community 192 - "Unified Atom Incidence"
Cohesion: 0.33
Nodes (6): Unified Atom Incidence, Graph Storage Families, Ferros-Grynn Graph Taxonomy, Mutable Frozen Type-State and Views, P4 Full Atom Incidence, GraphWorkspace ECS Sidecar

### Community 195 - "RegisteredDataFile"
Cohesion: 0.17
Nodes (14): FileRegistry, open_registration(), RegisteredDataFile, Arc, DirectFile, FerrosGrynnResult, GraphId, Option (+6 more)

### Community 196 - "AtomIds"
Cohesion: 0.21
Nodes (8): Inner, AtomIds, FallibleAtomIds, FallibleAtomIds<Inner>, ExactSizeIterator, FusedIterator, Item, Iterator

### Community 197 - "Canonical Storage Value Codec"
Cohesion: 0.50
Nodes (4): Canonical Storage Value Codec, OLTP Property Heap and Column Cache, Runtime Storage Columnar Boundary, StorageValue and Canonical Codec

### Community 205 - "GraphKind"
Cohesion: 0.16
Nodes (4): GraphKind, Display, Formatter, Result

### Community 206 - ".next"
Cohesion: 0.50
Nodes (3): BoxedEnumIter<'a>, Item, Iterator

### Community 261 - "Graph<State>"
Cohesion: 0.07
Nodes (22): Copied, copy_from_read(), Graph<State>, invalid_edges_are_transactional_and_use_graph_statuses(), AtomKind, BTreeSet, Debug, EdgeIds (+14 more)

## Knowledge Gaps
- **95 isolated node(s):** `Position2D`, `Position3D`, `Timestamp`, `ColorTag`, `Velocity` (+90 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **12 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Type` connect `Type` to `FerrosGrynnResult`, `catalog.rs`, `storage/store.rs`, `execution.rs`, `Closure`, `GraphKind`, `Vec`, `order_key.rs`, `Value`, `ir.rs`, `Vector`, `status_codes.rs`, `runtime/mod.rs`?**
  _High betweenness centrality (0.134) - this node is a cross-community bridge._
- **Why does `Graph` connect `Graph` to `Hypergraph<State>`, `Graph<State>`, `Metagraph<State>`, `storage/store.rs`, `MaterializedGraph`, `read.rs`, `DirtyTarget`, `runtime/mod.rs`, `Dag<State>`, `DiskAtomRef`, `Value`, `GraphMetadata`, `Frozen`, `Subgraph<State>`, `FerrosGrynnResult`, `atom_does_not_exist`, `Option`, `Option`?**
  _High betweenness centrality (0.127) - this node is a cross-community bridge._
- **Why does `Uuid` connect `Uuid` to `Hypergraph<State>`, `Graph<State>`, `Metagraph<State>`, `GraphRef`, `MaterializedGraph`, `DirtyTarget`, `FerrosGrynnResult`, `Dag<State>`, `DiskAtomRef`, `Path`, `Value`, `RecordIdPart`, `Subgraph<State>`, `AtomId`, `AtomId`, `.new`, `SnapshotId`, `read.rs`, `GraphWorkspace<G>`, `TraversalIndex`, `BinaryLiftingIndex`, `LocalGraph`, `GraphMetadata`?**
  _High betweenness centrality (0.081) - this node is a cross-community bridge._
- **What connects `Position2D`, `Position3D`, `Timestamp` to the rest of the system?**
  _95 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `AtomId` be split into smaller, more focused modules?**
  _Cohesion score 0.12321523003701745 - nodes in this community are weakly interconnected._
- **Should `Hypergraph<State>` be split into smaller, more focused modules?**
  _Cohesion score 0.05526512322628827 - nodes in this community are weakly interconnected._
- **Should `MetaGraph` be split into smaller, more focused modules?**
  _Cohesion score 0.07423687423687424 - nodes in this community are weakly interconnected._