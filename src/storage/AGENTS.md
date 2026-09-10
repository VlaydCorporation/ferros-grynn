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
- `catalog` и `superblock` — versioned database metadata;
- `graph_pages` — текущие физические graph page families; target 8.1 сохраняет
  `NodeHotPage`/`EdgeHotPage` как graph-local occurrence storage и добавляет
  database-level `AtomHotPage` для global base atoms;
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
  хранится в `FGFC v4.companion_of`; target 8.1 сохраняет раздельные файлы и
  переносит binding в `FGFC v5` с `DataSpaceId`. Standard и Unique называются
  соответственно `std_<ordinal:04>.fgidx` и `uniq_<ordinal:04>.fgidx`; `scalar`
  допустим только как внутреннее имя общей реализации.
- В target 8.1 `AtomHotPage` хранит только database-global class, RecordId и
  base properties. `NodeHotPage`/`EdgeHotPage` хранят membership occurrence,
  `GlobalAtomId`, label/overlay и topology; сами page families не являются
  legacy. Отклонён только прежний graph-owned payload с `table_id`, base
  properties и durable query-scratch flags. Точные offsets задают
  `docs/disk_storage_spec.md` §§10–11.
- Текущий implementation codec всё ещё использует `FGDC v5` и системный
  `GraphId = 0`; это не target-инвариант. Cutover A8–A16 принимает только
  `FGDC v6`/`FGFC v5`, использует `DataSpaceId::Database`, не создаёт системный
  graph и сохраняет ненулевой `GraphId` для каждого graph. Missing/unopenable/
  checksum-corrupt secondary по-прежнему переводится в `Stale` и даёт scan
  fallback, а primary/base corruption остаётся фатальной.
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
