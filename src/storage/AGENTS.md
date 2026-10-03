# Storage module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/storage`.

## Назначение и границы

`storage` — crate-private владелец локального on-disk состояния. Нормативный
контракт задаёт `docs/disk_storage_spec.md`; текущая реализация ориентирована
на синхронный Windows GA-core. Публичные операции БД принадлежат `datastore`,
а runtime-типы — `values` и `graph`.

- Не выдавать наружу страницы, LSN, физические atom ID или storage DTO.
- Не использовать `src/collections/radixdb` как второй storage engine.
- Не переносить layout из `fg-meta` без сверки с production-спецификацией.
- Native async I/O ограничен frame-oriented IoRing для заранее известного
  sparse-окна; синхронный Direct I/O остаётся fallback для singleton miss.
- Test-only storage-engine прототипы принадлежат
  `crates/fg-storage-engine-prototypes`, а не `src/storage`. Скрытый non-default
  `prototype_api` открывает этому workspace-крейту только измерительные
  production-типы и функции под feature `storage-engine-prototype-api`; feature
  не входит в GA public API, не выбирает production-поведение и не должен
  становиться runtime selector. Измерительные операции не добавлять как методы
  публичного `Datastore`.

## Карта владельцев

- `direct_io` — платформенные aligned Direct-I/O файлы;
- `page_io` — синхронный Direct transport и Windows frame-IoRing;
- `page` и `page_cache` — page layout, exact-version cache key и проверки;
- `hybrid_engine` — R8 Atomic Arena, source locators, read scopes, router,
  transient budget, scatter/P1 и sparse IoRing;
- `buffer_pool` — прежний Clock-Pro, доступный только тестам и prototype-control;
- `page_manager` — staged/WAL/home resolution и production HybridReadEngine;
- `wal` — codec, published state, writer/segments, recovery и checkpoint;
- `catalog` — `FGFC v6` physical registry и bounded `FGDC v10` page-chain;
- `job_catalog` — checksummed metadata-COW `FGJB v1` с `main/.prev` recovery;
- `quarantine` — immutable streaming `FGQR v1`; файл не является positional page source;
- `atom_store_superblock` — exact database-global AtomStore ceilings/counts и checkpoint tuple;
- `property_dictionary` — append-only database-global `PropertyKeyId ↔ UTF-8` pages и hash lookup;
- `metadata_cow` — общий checkpoint-протокол `.tmp`/main/`.prev`, не используемый обычным commit;
- `graph_pages` — graph-local `NodeHotPage`/`EdgeHotPage` occurrences,
  topology, labels, sparse overlays and scoped statistics;
- `atom_page`/`atom_store_image` — database-global `AtomHotPage`, base
  properties, global primary и reverse memberships;
- `graph_index` — order-preserving primary и общий внутренний B+tree codec для
  раздельных Standard/Unique контрактов, bounded Direct-I/O
  external sort, bottom-up packing, immutable Label arrays и single-page Count;
- `geometry_index` — общий STR page builder/walker и exact replacement delta;
  resident RAM pages и local WAL/COW files используют один checked layout.
- `reachability_index`/`neighbourhood_index`/`path_index` — immutable graph-owned header/array sections, full definition
  descriptor, canonical contiguous roots и aggregate decode admission; status остаётся в catalog.
- `derived_index` — borrowed array/record sections, checked ordinal chains,
  reference bounds и reserved bytes для snapshot graph artifacts.
- `edge_endpoint_index` — engine-owned qualified postings, shared Unique B+tree
  с concrete leaf/header и snapshot identity. Header page 0 и topology публикуются
  одной WAL/COW generation; lookup возвращает только connector/side, не query values.
- `store` — open/load, transaction views, commit и startup GC;
- `local` — адаптер локального datastore.

## Инварианты

- WAL durable publication предшествует видимости page frames; incomplete tail
  repair не должен маскировать полную corruption.
- Checkpoint публикуется атомарно и не удаляет данные, удерживаемые активным
  snapshot.
- Pinned physical snapshot допускает home page LSN до своего `visible_lsn`:
  passive checkpoint может backfill/prune после capture. Direct/IoRing bypass
  проверяет тот же page identity/checksum/ceiling, что resident admission.
- После COW publication прежняя UUID-generation удаляется в runtime только
  после освобождения exact physical snapshots, удерживающих старый registry.
  Cleanup failure не отменяет уже опубликованный commit: generation остаётся
  pending, счётчик ошибки наблюдаем, checkpoint/следующий commit повторяет
  удаление.
- Direct I/O сохраняет alignment, полный page transfer и платформенную
  семантику ошибок.
- On-disk числа, magic, tags и checksums меняются только вместе со
  спецификацией, golden vectors и recovery tests.
- `IndexId` — стабильная логическая identity определения из общего schema
  clock; `SchemaVersion` — версия определения; `DataFileId` — identity
  физического COW-файла; ordinal используется только в безопасном локальном
  filename. Binding индекса разрешается по `IndexId`, не по имени.
- `GraphSuperblock` хранится только в `superblock.fgb`. В текущем codec ровно
  шесть изменяемых graph data-файлов имеют отдельные `freelist_*.fgb`, а binding
  хранится в `FGFC v6.companion_of` вместе с `DataSpaceId`. Standard и Unique называются
  соответственно `std_<ordinal:04>.fgidx` и `uniq_<ordinal:04>.fgidx`; `scalar`
  допустим только как внутреннее имя общей реализации.
- `AtomHotPage` хранит только database-global class, RecordId и
  base properties. `NodeHotPage`/`EdgeHotPage` хранят membership occurrence,
  `GlobalAtomId`, label/overlay и topology. Точные offsets задают
  `docs/disk_storage_spec.md` §§10–11.
- Собираемый target использует только `FGDC v10`/`FGFC v6`, `DataSpaceId::Database` для AtomStore
  и не создаёт системный graph; `GraphId` каждого graph ненулевой. AtomStore и
  graph occurrences публикуются одним WAL transaction и открываются по одной
  snapshot boundary. Missing/unopenable/checksum-corrupt secondary переводится в `Stale` и даёт scan
  fallback, а primary/base corruption остаётся фатальной.
- `FGDC v10` хранит topology constraint bits, optional rooted-tree `RecordId`,
  public owner root и dense arena slots. Несколько slots могут указывать на один
  child `GraphId`, но `(scope, child)` уникален, containment cycle запрещён, а
  name map содержит только public graphs. Он также хранит durable parameter/function
  sections и `IndexMaintenanceMode`; `FGDC v9` и `FGFC v5` не принимаются и не мигрируются.
- `JobCatalog` регистрируется ровно как `admin/jobs.fgb`; quarantine — только как
  `admin/quarantine/<uuid>.fgq`. Оба auxiliary kind не открываются через `PageManager`.
- Bulk builders на Windows MSVC должны сначала выделять окончательные
  `Vec`-slots/arena и только затем заполнять 16 KiB `AlignedPage` через `&mut`.
  В ASan-инструментированном большом builder-path наблюдалась codegen-sensitive
  порча первого byte при возврате уже заполненной страницы по значению, хотя
  проверенные адреса оставались выровненными. `Box` стабилизирует адрес
  конкретного call site, но сам по себе не исключает временное stack-значение;
  границы доказанного описаны в
  `docs/reports/aligned_page_corruption_investigation.md`.
- Immutable external-sort B+tree generations заполняют leaves до capacity.
  Point-DML сначала применяет page-WAL leaf update при стабильных separators и
  posting layout; bulk fallback использует measured mutable fill `0.90` для
  primary/Unique и `0.80` для Standard. Это writer policy, не новый disk tag.
- Любая входная длина/счётчик проверяется до allocation; ошибки проходят через
  `gql_status` без раскрытия локальных путей и сырых страниц.
- Graph-owned definitions хранятся в FGDC v10 отдельно от availability/build revision,
  checked canonical descriptor и полные bytes обязательны наряду с fingerprint. GraphIndex
  durable job имеет target tag 5; graph-wide target и class index target не смешиваются.
- Ready Reachability/Neighbourhood/LANDMARK_SSSP binding удерживает один immutable файл своей проекции. Независимый
  commit сохраняет DataFileId; reopen проверяет maps/CSR/labels против того же snapshot.
  Ошибка secondary decode исключает artifact и делает logical definition Stale.
- Tensor binding использует только FileKind/PageType TensorCache. AUTO descriptor
  сохраняется полностью; reopen проверяет записанный concrete CSR/DENSE format,
  canonical sections, metadata и durable graph incidence revisions. Checkpoint LSN
  не определяет актуальность. Publication/retirement используют общие WAL/COW paths.
  Recovery использует maintenance budget открывающего datastore. Tensor restore
  учитывает encoding reservation ровно один раз и отдельно проверяет все stored cells.

## Проверка

Минимум для изменения: узкий unit/integration test владельца, `cargo check
--lib --no-default-features`, `cargo +nightly fmt --check`. Для WAL/page/layout
добавлять reopen/recovery или golden-vector проверку; для platform I/O —
соответствующую Windows/Unix конфигурацию.
