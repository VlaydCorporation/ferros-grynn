# Freeze-manifest storage core v1

**Статус: утверждён этапом 19 W4, 2026-09-28.** Этот документ перечисляет
байтовые контракты Windows GA-core. Нормативные layout остаются в
[`disk_storage_spec.md`](disk_storage_spec.md); manifest фиксирует их набор,
версии и границу совместимости.

## 1. Версии и совместимость

| Контракт                             |          Замороженная версия | Authority                     |
|--------------------------------------|-----------------------------:|-------------------------------|
| common page header и page types      | `STORAGE_FORMAT_VERSION = 1` | `disk_storage_spec.md` §§6, 8 |
| database catalog payload             |                    `FGDC v9` | §7.2                          |
| physical-file catalog payload        |                    `FGFC v5` | §7.2                          |
| catalog page chain                   |                    `FGC1 v1` | §§7.2, 18.2                   |
| canonical value envelope             |                    `FGAR v1` | §5                            |
| frame WAL                            |                       `FGW1` | §22                           |
| statistics root layouts              |           layout version `1` | §19                           |
| AtomStore/graph/database superblocks |   format/compatibility `1/1` | §17                           |

Decoder принимает только перечисленные версии. Автоматической миграции,
reinterpretation старых `FGDC`/`FGFC` и test-only compatibility path нет.
Любое изменение замороженных bytes требует `STORAGE_FORMAT_VERSION = 2` либо
новой однозначно versioned side-file family.

## 2. Общие инварианты

- размер физической страницы равен `16 384` байтам; все integers little-endian;
- common header содержит typed magic, page type, format version, flags, page LSN,
  page number и checksum; unknown type/flag/version и checksum mismatch являются
  typed corruption;
- `PageFlags::COMPRESSED` зарезервирован численно, но ни один GA-core-v1 codec
  его не принимает; compressed payload требует новой versioned интерпретации;
- decoder применяет checked arithmetic, bounded counts/depth/length, canonical
  ordering, exact consumption и нулевые reserved/trailing bytes;
- runtime-only `Value` (`Path`, graph result, occurrence, cursor, closure,
  `ErrorValue`) не имеет durable encoding;
- `DataSpaceId = 0` означает только database AtomStore/catalog domain;
  `GraphId` всегда ненулевой;
- `DataFileId` различает exact physical file, а logical schema/index identity
  задаётся `ClassId`/`IndexId` и `SchemaVersion`;
- staged/WAL/home resolution никогда не подменяет видимую WAL-версию home-page.

## 3. Замороженные core codecs

В core v1 входят:

1. `DatabaseSuperblock`, `AtomStoreSuperblock` и `GraphSuperblock`, публикуемые
   metadata COW;
2. `FGDC v9`, `FGFC v5` и bounded checksummed `FGC1` chains;
3. `AtomHotPage`, graph-local node/edge hot pages, graph overlays, adjacency и
   cross-level overflow, property heap/large-property chains, subgraph directory
   и per-target freelists;
4. database property dictionary и graph label dictionary/hash pages;
5. base/class, graph/degree, label и property statistics в раздельных files;
6. primary, membership, Standard, Unique, Count и Label indexes;
7. `FGW1` page-init/page-frame/commit/checkpoint records и checkpoint metadata
   COW;
8. canonical `StorageValue`, order-preserving scalar/composite index key и
   `RecordId` codecs.

Названия файлов и UUID-generation layout заморожены §7. Primary/membership и
engine-owned lookup indexes являются частью core; specialized fulltext, vector,
geometry, reachability, neighbourhood, path, endpoint, changefeed и tensor
страницы остаются versioned future side-files. Зарезервированный числовой
`PageType` сам по себе не делает такой codec частью GA-core.

## 4. Решение `Q-20`: B+tree keys

Core v1 сохраняет существующий **full-key layout**:

- internal separator и leaf entry хранят полный canonical order key;
- Unique/primary mutable leaf fill равен `90%`, Standard — `80%`;
- immutable bulk packer использует доступную page capacity;
- B+tree pages принимают только существующие flags: internal — empty, leaf —
  `LEAF`; `COMPRESSED` недопустим;
- prefix- и whole-page block-compression не входят в format v1.

Повторяемый profiling probe
`storage::graph_index::profile::stage19_index_key_layout_profile` сравнил
100 000 production-encoded keys. Для common-string/composite/mixed-string
получено `290→94 / 314→75 / 290→217` страниц, но median packing-layout CPU
вырос в `7,3× / 9,9× / 5,1×`. Page-prefix prototype требовал отдельного leaf
layout и не имел доказательства point-read/mutation/recovery latency. Whole-page compression
дополнительно требует полной распаковки, нового codec/config/discriminator и
production dependency. Поэтому ни один кандидат не прошёл полный GA gate; draft
варианты удалены вместо замораживания двух интерпретаций одной страницы.

Возвращаться к compression можно только для format v2 после end-to-end paired
gate: point/range read, bulk build, mutation/rebuild, recovery, page count,
CPU/p99/RSS и golden/malformed vectors. Core-v1 bytes при этом не меняются.

## 5. Обязательное evidence

Минимальная freeze-матрица:

- `storage::page` — exact header, all assigned types, checksum, wrong
  page/type/version/flags;
- `storage::catalog::database` — `FGDC v9` round-trip, truncation, old version,
  duplicate identity/property/scope, invalid root/ownership/constraint bits;
- `storage::catalog::file` — exact `FGFC v5`, отказ v4, companion/path/role и
  reserved bytes;
- `storage::catalog::page_chain` — exact `FGC1`, multi-page chain, bounds и
  trailing bytes;
- `values::storage::codec` — все v1 tags, `FGAR`, canonical collections,
  numeric edge cases, truncation и arbitrary bounded input;
- superblock, atom/property/dictionary, graph-page/statistics и B+tree focused
  tests;
- WAL record/checksum/torn-tail/corruption/checkpoint/reopen tests;
- RAM/local/reopen differential gate и обе I/O feature-конфигурации.

Команды и фактические результаты этапа сохраняются в
[`reports/windows_ga_core_stage_19_report.md`](reports/windows_ga_core_stage_19_report.md),
а profiler/CSV artifacts — в `target/windows-ga-core-stage19/`.
