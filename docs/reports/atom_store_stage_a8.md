# Отчёт по шагу A8: durable AtomStore

**Дата:** 2026-09-12  
**Статус:** завершён  
**Граница:** только A8 из `rework_plan.md`; A9 и A10 не начинались.

## Результат

Database-global AtomStore получил самостоятельный durable layout и больше не зависит от
фиктивного системного графа или graph-owned property storage:

- `AtomStoreSuperblock` имеет утверждённую offset table, checksummed page codec и проверяет
  counts, capacity, page ceilings и checkpoint tuple;
- `property_keys.fgb` хранит append-only `PropertyDictionary` с плотными стабильными
  `PropertyKeyId`, entry chain и collision-checked hash lookup;
- `FileCatalog FGFC v5` связывает отдельный `DataFileKind::PropertyDictionary` с
  `DataSpaceId::Database`; словарь не имеет freelist;
- AtomHot, properties, dictionary, global primary и ровно один логический superblock
  публикуются одной WAL-транзакцией;
- reopen сначала разрешает snapshot-visible superblock/data frames из WAL, затем обращается к
  home files в пределах опубликованных ceilings;
- checkpoint не перезаписывает home superblock in-place: обычные frames backfill-ятся отдельно,
  после `CheckpointEnd` metadata публикуется через `.tmp` → main с сохранением `.prev`, а
  `PageHeader.page_lsn` остаётся LSN логической WAL-версии;
- recovery никогда не повышает оставшийся `.tmp` до authority и выбирает только checksummed
  main/`.prev`, не опережающий database checkpoint authority.

## Изменённые компоненты

- `src/storage/atom_store_superblock.rs` — exact codec и семантические инварианты;
- `src/storage/property_dictionary.rs` — build, append-only resize и reopen словаря;
- `src/storage/metadata_cow.rs` — общий checkpoint-only tmp/main/prev protocol;
- `src/storage/atom_store_image.rs` — build/stage/open/backfill связного AtomStore image;
- `src/storage/wal` — superblock разрешён как обычный логический `PageFrame`;
- `src/storage/catalog/file.rs`, `src/storage/types.rs`, `src/storage/page.rs` — новые file/page
  discriminants и binding validation.

## Проверенные свойства

- golden offsets и round-trip `AtomStoreSuperblock`;
- wrong type/flags/version, non-zero reserved, checksum corruption, invalid counts/ceilings и
  старый `GraphSuperblock` отклоняются;
- dictionary проверяет UTF-8, duplicates, tail zeros, ceiling, page types/flags/checksum,
  collision resolution, append-only resize и reopen;
- незавершённая WAL-транзакция невидима, старый snapshot видит старую metadata version;
- повторное staging одного data space оставляет ровно один superblock frame;
- неизменившаяся metadata не создаёт frame;
- committed AtomStore восстанавливается после переоткрытия WAL, а повреждённый home
  superblock не мешает WAL-first reopen;
- checkpoint backfill сохраняет logical `page_lsn`; после WAL pruning данные открываются из
  home files;
- первая транзакция после checkpoint корректно читает WAL-superblock с
  `page_lsn > backfilled_lsn`, тогда как такой home COW-candidate отклоняется;
- ahead main отклоняется в пользу `.prev`, `.tmp` игнорируется даже при допускающей authority,
  torn main восстанавливается через `.prev`.

## Исправления по итоговому review

- counts и pointers повреждённого dictionary root проверяются относительно опубликованного
  ceiling до обхода и резервирования памяти;
- любой superblock page ceiling сначала ограничивается фактически доступным home/WAL image,
  поэтому повреждённая metadata не вызывает неограниченную предварительную аллокацию;
- проверка WAL-superblock отделена от проверки checkpoint COW-candidate: новый логический frame
  не обязан быть уже backfilled;
- Windows metadata rename использует одновременно `MOVEFILE_REPLACE_EXISTING` и
  `MOVEFILE_WRITE_THROUGH`, как требует утверждённый crash protocol.

## Проверка

Успешно выполнены:

```text
cargo test --lib --no-default-features storage::atom_store_superblock -- --test-threads=1
cargo test --lib --no-default-features storage::property_dictionary -- --test-threads=1
cargo test --lib --no-default-features storage::atom_store_image -- --test-threads=1
cargo test --lib --no-default-features storage::catalog -- --test-threads=1
cargo test --lib --no-default-features storage::wal::tests::superblocks_are_transaction_frames -- --test-threads=1
cargo test --lib --no-default-features -- --test-threads=1
cargo test --lib --no-default-features --features native-async-io storage:: -- --test-threads=1
cargo check --lib --no-default-features
cargo check --lib --no-default-features --features native-async-io
cargo +nightly fmt --all -- --check
```

Итоги полных тестовых gates: `244 passed, 0 failed, 1 ignored` без default features и
`111 passed, 0 failed, 2 ignored` для storage с `native-async-io`. Bounded review завершён без
оставшихся findings в A8-срезе. `cargo clippy --lib --no-default-features` всё ещё завершается с
кодом 101 из-за существующих repository-wide lint errors (прежде всего отсутствующей
документации вне текущего среза); фильтр по затронутым A8 storage-файлам содержит 0 errors.
Обычный Windows launcher `graphify update .` вернул `Failed to canonicalize script path`; тот же
incremental update через установленный Graphify Python module завершился успешно и обновил
`graph.json`, `graph.html` и `GRAPH_REPORT.md`.

## Отложенная работа

A8 не реализует graph occurrences, overlays, graph superblock, scoped indexes/statistics или
production datastore cutover. Эти зависимости принадлежат A9–A16. Полная crash-point матрица
с последней заменой `db.fgb`, multi-graph commit и RAM/local differential gate остаётся A15;
это не возвращает старый формат и не добавляет compatibility path.
