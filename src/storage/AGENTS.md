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
  `crates/fg-storage-engine-prototypes`, а не `src/storage`. Временный скрытый
  `prototype_api` открывает этому крейту только измерительные production-типы и
  методы под feature `storage-engine-prototype-api`; feature не выбирает
  production-поведение и не должен становиться runtime selector.

## Карта владельцев

- `direct_io` — платформенные aligned Direct-I/O файлы;
- `page_io` — синхронный Direct transport и Windows frame-IoRing;
- `page` и `page_cache` — page layout, exact-version cache key и проверки;
- `hybrid_engine` — R8 Atomic Arena, source locators, read scopes, router,
  transient budget, scatter/P1 и sparse IoRing;
- `buffer_pool` — прежний Clock-Pro, доступный только тестам и prototype-control;
- `page_manager` — staged/WAL/home resolution и production HybridReadEngine;
- `wal` — codec, published state, writer/segments, recovery и checkpoint;
- `catalog` — `FGFC v5` physical registry и bounded `FGDC v8` page-chain;
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
- `store` — open/load, transaction views, commit и startup GC;
- `local` — адаптер локального datastore.

## Инварианты

- WAL durable publication предшествует видимости page frames; incomplete tail
  repair не должен маскировать полную corruption.
- Checkpoint публикуется атомарно и не удаляет данные, удерживаемые активным
  snapshot.
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
  хранится в `FGFC v5.companion_of` вместе с `DataSpaceId`. Standard и Unique называются
  соответственно `std_<ordinal:04>.fgidx` и `uniq_<ordinal:04>.fgidx`; `scalar`
  допустим только как внутреннее имя общей реализации.
- `AtomHotPage` хранит только database-global class, RecordId и
  base properties. `NodeHotPage`/`EdgeHotPage` хранят membership occurrence,
  `GlobalAtomId`, label/overlay и topology. Точные offsets задают
  `docs/disk_storage_spec.md` §§10–11.
- Собираемый target использует `FGDC v8`/`FGFC v5`, `DataSpaceId::Database` для AtomStore
  и не создаёт системный graph; `GraphId` каждого graph ненулевой. AtomStore и
  graph occurrences публикуются одним WAL transaction и открываются по одной
  snapshot boundary. Missing/unopenable/checksum-corrupt secondary переводится в `Stale` и даёт scan
  fallback, а primary/base corruption остаётся фатальной.
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

## Проверка

Минимум для изменения: узкий unit/integration test владельца, `cargo check
--lib --no-default-features`, `cargo +nightly fmt --check`. Для WAL/page/layout
добавлять reopen/recovery или golden-vector проверку; для platform I/O —
соответствующую Windows/Unix конфигурацию.
