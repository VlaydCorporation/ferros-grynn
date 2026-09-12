# Ferros-Grynn — Спецификация дискового хранилища

**Версия:** 0.1
**Статус:** проектирование
**Нормативная область:** байтовый формат, page/index layouts, WAL, checkpoint/recovery и
durability. Семантику GrynQL задаёт [gql_spec.md](gql_spec.md), точный синтаксис —
[gql_grammar.md](gql_grammar.md), техническую реализацию — [gql_tech_spec.md](gql_tech_spec.md).

**Статус core format v1:** Windows-кандидат. Он фиксируется только после Windows sync
performance gate и Windows GA-core gate из [full_plan.md](full_plan.md) (этапы 3 и 19). Linux
sync/NativeAsync не блокируют эту фиксацию: после неё несовместимый Linux layout требует format
v2 и явной миграции.

**Целевая ревизия этапа 8.1:** `approved-for-implementation`. Документационные шаги
A0–A4/S0–S6 и кодовые шаги A5–A8 завершены; A9–A16 остаются открытыми.
Она заменяет graph-owned atom/table candidate на database-level AtomStore и graph-local
occurrences. При конфликте с сохранёнными ниже legacy layout-примечаниями действуют §§2, 4, 7,
10–11, 17–18 и 22 в редакции 8.1. Cutover принимает только `FGDC v7`/`FGFC v5`; миграции
промежуточных кандидатов нет.

---

## Содержание

1. [Цели и принципы](#1-цели-и-принципы)
2. [Режимы хранилища](#2-режимы-хранилища)
3. [Система типов — хранимые значения](#3-система-типов--хранимые-значения)
4. [RecordId — публичный идентификатор](#4-recordid--публичный-идентификатор)
5. [Бинарное кодирование значений](#5-бинарное-кодирование-значений)
6. [Константы и магические числа](#6-константы-и-магические-числа)
7. [Файловая организация](#7-файловая-организация)
8. [Универсальный заголовок страницы](#8-универсальный-заголовок-страницы)
9. [GlobalAtomId и graph-local AtomId/DiskAtomRef](#9-globalatomid-и-graph-local-atomiddiskatomref)
10. [AtomHotPage и graph-local NodeHotPage](#10-atomhotpage-и-graph-local-nodehotpage)
11. [Graph-local EdgeHotPage](#11-graph-local-edgehotpage)
12. [AdjacencyOverflowPage](#12-adjacencyoverflowpage)
13. [PropHeapPage — куча variable records](#13-propheappage--куча-variable-records)
14. [SubgraphDirPage — директория подграфов](#14-subgraphdirpage--директория-подграфов)
15. [FreelistPage — управление свободными страницами](#15-freelistpage--управление-свободными-страницами)
16. [LabelDictionary — интернирование строк](#16-labeldictionary--интернирование-строк)
17. [AtomStoreSuperblock и GraphSuperblock](#17-atomstoresuperblock-и-graphsuperblock)
18. [ClassCatalog и остальные schema definitions](#18-classcatalog-и-остальные-schema-definitions)
19. [StatisticsStore — статистика для планировщика](#19-statisticsstore--статистика-для-планировщика)
20. [Индексы](#20-индексы)
21. [Changefeed и Live-запросы](#21-changefeed-и-live-запросы)
22. [WAL для дискового режима](#22-wal-для-дискового-режима)
23. [Checkpoint и Recovery](#23-checkpoint-и-recovery)
24. [Compaction и дефрагментация](#24-compaction-и-дефрагментация)
25. [Адресация и навигация](#25-адресация-и-навигация)
26. [Тензорные представления](#26-тензорные-представления)
27. [Конфигурация](#27-конфигурация)
28. [Эволюция формата](#28-эволюция-формата)
29. [Новые Rust-компоненты](#29-новые-rust-компоненты)
30. [План доработки кодовой базы](#30-план-доработки-кодовой-базы)

---

## 1. Цели и принципы

### 1.1. Цели

- Поддержать **Режим 2** (`gql_tech_spec.md §2.2`): база не помещается в RAM;
  горячие AtomStore/graph pages обслуживает production `HybridReadEngine`.
- Поддержать **Режим 3** (гибридный): материализация effective graph view из
  Mode 1/2 source в owning in-memory graph через `query_to_memory`.
- Обеспечить хранение всех **persistable** типов GQL (`gql_spec.md §5`); runtime-only,
  mutable/live и transaction-bound значения отклоняются явно.
- Обеспечить хранение **схемы** (classes, поля, scoped indexes, события, функции, анализаторы, параметры).
- Обеспечить хранение **статистики** для cost model планировщика (степени, гистограммы, selectivity).
- Поддержать в core format-v1 candidate primary и scalar standard/unique B+tree. Для count,
  fulltext (BM25), HNSW, DiskANN, geometry, path, reachability и neighbourhood использовать
  утверждённые отдельно версионируемые side-file контракты последующих этапов (§20);
  их наличие в формате не означает досрочную реализацию.
- Поддержать **changefeed** и инфраструктуру live-запросов.
- Сохранить **SoA-дизайн** кодовой базы: горячие данные обхода (`flags`, `kinds`, `adjacency`) — отдельно от холодных (
  `props`).
- Обеспечить **Direct I/O** через production-слой `src/storage` без промежуточного page-cache ОС.

### 1.2. Принципы дизайна

| Принцип                         | Реализация                                                                        |
|---------------------------------|-----------------------------------------------------------------------------------|
| Спецификация первична           | Если текущая кодовая база противоречит требованиям — кодовая база меняется        |
| SoA на диске                    | Горячие данные обхода отделены от холодных свойств (разные файлы)                 |
| Горячий путь — горячая страница | `adjacency + kind + flags + weight` в одной странице без холодных свойств         |
| Фиксированные слоты             | Hot-страницы — фиксированный размер слота → O(1) адресация без B+tree             |
| Переменная длина в куче         | Свойства, строки, бинарные данные → PropHeapPage (slotted page)                   |
| Атомарность записи              | WAL-first: WAL append и fsync **до** модификации любой страницы                   |
| Идемпотентное восстановление    | `pageLSN` в каждой странице — REDO применяется только если `entry_lsn > page_lsn` |
| Direct I/O                      | `.fgb`/`.fgidx`: `O_DIRECT` на Linux, `FILE_FLAG_NO_BUFFERING` на Windows         |
| Последовательный WAL            | `.wal` использует buffered I/O и один явный `flush + sync_data` на commit         |
| Атомарность суперблоков         | Логический WAL PageFrame; home публикуется checkpoint COW через tmp/main/prev     |
| Стабильный дисковый ID          | `DiskAtomRef = u64` (kind-бит + 40-битный slot-индекс, без поколения)             |
| RecordId                        | Публичный идентификатор записи отделён от внутреннего AtomId                      |

Windows-first implementation использует `FILE_FLAG_NO_BUFFERING`. Linux `O_DIRECT` в таблице —
обязательный целевой контракт, но Unix runtime сейчас является typed unsupported stub и не
считается выпущенным до отдельного Linux/NVMe gate.

### 1.3. Что требует изменения в кодовой базе

Принципиально: `value.rs`, `serial.rs`, `mvcc_persist.rs`, `id.rs` не удовлетворяют полной системе типов GQL.
Детальный план — в §29.

### 1.4. Что остаётся без изменений

- `PAGE_SIZE = 16384` — используется без изменений.
- `fg-meta` остаётся сырым примером для проверки гипотез и сценариев, но не источником
  требований или production-гарантий. Его `DirectFile`, `BufferPool` и async-код напрямую не
  используются. Production-реализация находится в `src/storage`.
- Единый database WAL и `HybridReadEngine` переиспользуются; их ключ расширяется до
  `DataSpaceId`, а не заменяется новым storage framework.
- 64-битный generation-aware layout сохраняется, но типы разделяются: `GlobalAtomId` адресует
  AtomStore, graph-local `AtomId` — occurrence.

---

## 2. Режимы хранилища

```text
Mode 1: Full In-Memory
  DatabaseRoot
    ├── global AtomStore: RecordId → base atom/properties
    └── graphs: memberships + topology + labels + sparse overlays

Mode 2: Disk-Backed
  atoms/<generation>/    -- database-level AtomStore
  graphs/<id>/<generation>/ -- graph-local occurrences
  shared HybridReadEngine keyed by DataSpaceId

Mode 3: Query To Memory
  effective view from Mode 1 or Mode 2
  ↓ bounded materialization
  T<Frozen> | T<Mutable> { origin_mapping, dirty_set }
```

Standalone SDK graph остаётся owning materialization и не подменяет нормализованный Mode 1 root.
Mode 3 загружается на одном `SnapshotId` и освобождает исходный snapshot после полной
материализации. `Frozen` не имеет flush API; explicit flush `Mutable` пишет только graph-local
topology/overlay и никогда не меняет base atom неявно или из `Drop`.

### 2.1. Выбор I/O-режима

Синхронный Direct I/O — обязательный и стандартный production-режим. Data/index-файлы
поддерживаются только на Linux (`O_DIRECT`) и Windows (`FILE_FLAG_NO_BUFFERING`); macOS и
прочие платформы возвращают типизированный статус `DirectIoUnavailable`, без скрытого buffered
fallback. `O_SYNC` и `FILE_FLAG_WRITE_THROUGH` не используются: долговечность задают явные
sync-точки протокола.

WAL является единственным исключением: это последовательный buffered-файл, где commit сначала
дописывает все `PageFrame` и `TxCommit`, затем выполняет один `flush` и один `sync_data`.
Production `mmap` не входит в архитектуру; он допустим только как benchmark page source.
Windows Native Async I/O включается feature `native-async-io` и не меняет синхронную pull-модель
запросов. Он использует IoRing через сгенерированные bindings
`windows::Win32::Storage::FileSystem` и companion handle с
`FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED`. Менеджер может поставить несколько независимых
extent-ов в одну очередь, но возвращает управление только после извлечения всех CQE; поэтому
borrowed aligned buffers и file handles живут до завершения операций. Для одного extent допустим
синхронный positional fast path на отдельном non-overlapped handle, если он не нарушает выбранную
Direct-I/O и durability семантику.

IoRing-режим требует Windows build 22000 или новее и runtime-поддержку read/write. Пользователь
выбирает режим при открытии хранилища; отсутствие compile-time feature или runtime-возможности
завершается `FG-51I04`, без IOCP/buffered fallback. Windows NativeAsync реализован и прошёл
собственный QD-gate; Linux NativeAsync и `tokio-uring` отложены до нативной проверки.
I/O-backend не является частью байтового формата и не меняет его версию: любой будущий backend
обязан доказать те же Direct-I/O и durability-инварианты на тех же format-v1 bytes.

---

## 3. Система типов — хранимые значения

Полный набор типов GQL (`gql_spec.md §5`), разделённых на хранимые и нехранимые.

### 3.1. Хранимые типы

| Тег  | GQL тип                     | Описание                                 | Размер на диске            |
|------|-----------------------------|------------------------------------------|----------------------------|
| 0x00 | `none`                      | Поле отсутствует                         | 1 байт (тег)               |
| 0x01 | `null`                      | Поле есть, значение пусто                | 1 байт                     |
| 0x02 | `bool (false)`              | Логическое                               | 1 байт                     |
| 0x03 | `bool (true)`               | Логическое                               | 1 байт                     |
| 0x04 | `int`                       | i64                                      | 2..11 байт (LEB128 zigzag) |
| 0x05 | `float`                     | f64 IEEE 754                             | 9 байт                     |
| 0x06 | `decimal`                   | i128 мантисса + scale (Arrow-совместимо) | 18 байт                    |
| 0x07 | `string`                    | UTF-8                                    | 1+len(uLEB128)+N байт      |
| 0x08 | `bytes`                     | Произвольные байты                       | 1+len(uLEB128)+N байт      |
| 0x09 | `datetime`                  | RFC 3339 + timezone                      | 1+8+4+4 байт               |
| 0x0A | `duration`                  | Временной интервал (знаковый)            | 3..18 байт (переменная)    |
| 0x0B | `uuid`                      | UUID v7 (128 бит)                        | 17 байт                    |
| 0x0C | `ulid`                      | ULID (128 бит)                           | 17 байт                    |
| 0x10 | `record_id`                 | RecordId (class:id)                      | 1+переменная               |
| 0x11 | `array<T>`                  | Типизированный массив                    | 1+4+Σ(elements)            |
| 0x12 | `set<T>`                    | Множество уникальных                     | 1+4+Σ(elements)            |
| 0x13 | `object`                    | Map ключ→значение                        | 1+4+Σ(key_id+value)        |
| 0x15 | `range<T>`                  | Диапазон [lo..hi] с флагами              | 1+flags+[lo]+[hi]          |
| 0x21 | `geometry::Point`           | Точка (lon, lat)                         | 17 байт                    |
| 0x22 | `geometry::LineString`      | Ломаная                                  | 1+4+N×16 байт              |
| 0x23 | `geometry::Polygon`         | Многоугольник + holes                    | переменная                 |
| 0x24 | `geometry::MultiPoint`      |                                          | переменная                 |
| 0x25 | `geometry::MultiLineString` |                                          | переменная                 |
| 0x26 | `geometry::MultiPolygon`    |                                          | переменная                 |
| 0x27 | `geometry::Collection`      | GeometryCollection                       | переменная                 |
| 0x31 | `vector_f64`                | Вектор f64 (для HNSW)                    | 1+2+N×8 байт               |
| 0x32 | `vector_f32`                | Вектор f32                               | 1+2+N×4 байт               |
| 0x33 | `vector_i64`                | Вектор i64                               | 1+2+N×8 байт               |
| 0x34 | `vector_i32`                | Вектор i32                               | 1+2+N×4 байт               |
| 0x35 | `vector_i16`                | Вектор i16                               | 1+2+N×2 байт               |
| 0x36 | `atom_ref`                  | kind(u8) + canonical RecordId            | переменная                 |
| 0x37 | `graph_ref`                 | ненулевой graph_id(u64 LE)               | 1 + 8 байт                 |
| 0x38 | `qualified_atom_ref`        | graph_id(u64 LE) + local_ref(u64 LE)     | 1 + 16 байт                |

Таблица в §5.2 — единственный нормативный реестр `TypeTag`. Теги в этой таблице приведены для
навигации и обязаны совпадать с ним. `0x14` зарезервирован в format v1 и не переиспользуется:
`option<T>` является статическим типом, `Some(v)` кодируется как `v`, `None` — как `none`.

### 3.2. Нехранимые типы (runtime only)

| GQL тип        | Причина                                                        |
|----------------|----------------------------------------------------------------|
| `closure<...>` | Функция времени выполнения; сериализовать безопасно невозможно |
| `regex`        | Хранится как строка-шаблон; парсится при каждом использовании  |
| `matcher`      | Состояние поиска; только runtime                               |
| `computed<T>`  | Не хранится; вычисляется при SELECT                            |
| `formatter`    | Передаётся как строка-формат                                   |
| `any`          | Псевдотип; конкретный тип известен в runtime                   |

### 3.3. Decimal (фиксированная высокая точность)

Используется формат **coefficient + scale** (модель Arrow/Parquet `Decimal128`, она же `DECIMAL` в DuckDB и `Decimal128` в ClickHouse), а **не** IEEE 754-2008 decimal128.

**Почему не decimal128.** В экосистеме Rust нет ни одной пригодной чисто-Rust реализации IEEE 754-2008 decimal128. Все существующие — обёртки над C-библиотеками decNumber / Intel DFP (`dec`, `decimal`, `dfp-number`); `decmathlib-rs` заархивирован автором; `decstr` — кодек без арифметики. Зависимость от C противоречит требованиям проекта, поэтому требование decimal128 снимается.

**Runtime-тип: `rust_decimal::Decimal`** (16 байт: 96-битная мантисса + знак + масштаб).

- Точность: **28 значащих десятичных цифр** (29 частично).
- Диапазон: `±7.9228162514264337593543950335 × 10²⁸`; минимальное ненулевое `1e-28`.
- Масштаб: `0..=28`; положительной экспоненты нет.
- **NaN и ±Infinity не поддерживаются** — это соответствует семантике `NUMERIC` в SQL/GQL. Арифметическое переполнение возвращает ошибку, а не `Inf`.
- `-0` нормализуется в `0`; `Eq`/`Ord` не зависят от масштаба (`1.0 == 1.00`), а `Hash` нормализует значение — это требуется для `set<T>` и unique-индексов, иначе одно и то же число попало бы в множество дважды.

**Дисковый формат намеренно шире runtime-типа:**

```
decimal → 0x06 + mantissa: i128 (16 байт LE, дополнительный код) + scale: u8   = 18 байт
```

Мантисса `i128` даёт 38 цифр против 28 у runtime-типа. Запас сделан осознанно:
- замена реализации decimal (на другой крейт или на собственный тип) не потребует миграции файлов;
- байты мантиссы **побайтово совпадают** с элементом Arrow `Decimal128Array`, поэтому колоночный кэш свойств (§13.6) строится обычным `memcpy`, без переформатирования.

Запись выполняется как `(d.mantissa() as i128, d.scale() as u8)` — экспорт всегда точен, поскольку 96-битная мантисса заведомо помещается в `i128`. Чтение — `Decimal::try_from_i128_with_scale(m, s)`; значение с `|mantissa| ≥ 2⁹⁶` (такое может быть записано только более поздней версией движка) возвращает ошибку `DecimalPrecisionExceeded`, а не молча теряет точность.

Runtime использует `rust_decimal::Decimal` напрямую внутри `Number::Decimal`; отдельный newtype
для сокрытия типа не вводится. Дисковая форма остаётся независимым `i128 + scale`.

### 3.4. DateTime и Duration

**DateTime** хранится как:

- `seconds: i64` — Unix timestamp в секундах (UTC).
- `nanos: u32` — субсекундная точность `[0 .. 999_999_999]`.
- `tz_id: u32`  -- кодирование часового пояса (см. ниже)

Кодирование tz_id (u32):
- `0` = UTC (по умолчанию)
- `1 .. 999_999` = ID пояса в IANA timezone dictionary (§16, отдельная секция).
Хватит на любые пополнения tzdb (2024 год: ~600 поясов).
- `1_000_000 ..1_172_799` = Фиксированное UTC-смещение.
Декодирование: offset_seconds = tz_id − 1_000_000 − 86_399 (86_399 — сдвиг для представления отрицательных смещений).
Диапазон: `−86_399 .. +86_400` секунд (≈ ±24 ч с запасом). Реальный диапазон UTC-смещений: `−43_200 .. +50_400` с.

Итоговый размер DateTime на диске: `тег(1) + i64 + 32 + u32 = 17 байт`.

Реализация: крейт `jiff` (`Timestamp` + `TimeZone`).

**Duration** хранится как:

Duration хранится как переменно-длинный blob через UnitSet bitmask.
Это сохраняет оригинальную семантику jiff::Span, не сводя к наносекундам.

Формат (переменная длина, минимум 3 байта):
```
header: u16
  bit 15:      sign      (0 = положительный, 1 = отрицательный) знак единый для всего Span
  bit 14:      has_year
  bit 13:      has_month
  bit 12:      has_week
  bit 11:      has_day
  bit 10:      has_hour
  bit 9:       has_minute
  bit 8:       has_second
  bit 7:       has_millisecond
  bit 6:       has_microsecond
  bit 5:       has_nanosecond
  bits 4..0:   reserved (0)
[value: uLEB128]  × (число установленных битов has_*)
  значения перечисляются в порядке убывания единиц:
  year → month → week → day → hour → minute → second → ms → μs → ns
```

Примеры:
- "5 часов" → `header=0x0400 ([0x00, 0x04] в LE -> 2 байта) + LEB128(5) = 3 байта`
- "1 год 2 мес 3 дня" → `header=0x6800 + LEB128(1,2,3) = 5 байт`
- "-30 секунд" → `header=0x8100 + LEB128(30) = 3 байта`
- "1 год 2 мес 3 дня 4 ч 5 мин 6 с" → `header=0x7C00 + 6×LEB128 = 8 байт`

**Примечание о знаке**: поддерживаются только Span с единым знаком; 
попытка сохранить Span с разными знаками компонент приведёт к ошибке валидации.

Это совпадает с `jiff::Span` (гражданское время, не физическое).

### 3.5. Geometry (GeoJSON-совместимый)

Все геометрии хранятся в **WGS-84 (EPSG:4326)**: longitude (x) и latitude (y) как f64.

| Тег       | Тип        | Кодирование                                             |
|-----------|------------|---------------------------------------------------------|
| 0x21      | Point      | lon(f64) + lat(f64) = 16 байт                           |
| 0x22      | LineString | count(u32) + [lon,lat]×count                            |
| 0x23      | Polygon    | ring_count(u32) + [count(u32) + [lon,lat]×N]×ring_count |
| 0x24–0x26 | Multi*     | count(u32) + [geometry]×count                           |
| 0x27      | Collection | count(u32) + [tagged_geometry]×count                    |

Полная запись: `tag(u8) + encoded_bytes`.

### 3.6. Vector типы

Используются для HNSW-индекса. Хранятся в PropHeapPage как обычное значение.

| Тег  | Элемент | Кодирование                              |
|------|---------|------------------------------------------|
| 0x31 | f64     | dim(u16) + [f64 LE]×dim = 2 + dim×8 байт |
| 0x32 | f32     | dim(u16) + [f32 LE]×dim = 2 + dim×4 байт |
| 0x33 | i64     | dim(u16) + [i64 LE]×dim = 2 + dim×8 байт |
| 0x34 | i32     | dim(u16) + [i32 LE]×dim = 2 + dim×4 байт |
| 0x35 | i16     | dim(u16) + [i16 LE]×dim = 2 + dim×2 байт |

Максимальная размерность: 65535 (dim ≤ u16::MAX). На практике HNSW ограничен до 4096 для производительности.

### 3.7. Range

```
range<T>:
  flags(u8):  bit0 = lo_inclusive, bit1 = hi_inclusive,
              bit2 = lo_unbounded, bit3 = hi_unbounded
  [lo: Value]    если !lo_unbounded
  [hi: Value]    если !hi_unbounded
```

### 3.8. Доменные типы

GQL определяет доменные типы для атомов и структур графа (gql_spec.md §5).
Они разделяются на две категории:

#### 3.8.1. Хранимые ссылочные типы

Используются как значения свойств для хранения ссылок на элементы графа.
Тип хранится в одном байте тега. Публичная atom reference использует `RecordId`, а не
физический slot: ссылка остаётся валидной после COW rebuild и между graph memberships.

| Тег  | GQL тип     | Описание                                 | Размер на диске |
|------|-------------|------------------------------------------|-----------------|
| 0x36 | `atom`      | kind + ссылка на global atom по RecordId | переменная      |
| 0x37 | `graph_ref` | каталожная ссылка по ненулевому GraphId  | 1 + 8 байт      |

Кодирование:
- `atom → tag(u8) + kind(u8) + canonical RecordId`
- `graph_ref → tag(u8) + graph_id(u64 LE)`

**Семантика**: тег кодирует тип (атом или граф), разделение атомов происходит на уровне валидации схемы по kind-биту; 
generic-параметр T (например, `vertex<Person>`) является schema-level ограничением; проверяемым при записи, и не хранится в значении — только в FieldDefinition (§18.4).

#### 3.8.2. Runtime / interpretation-time типы (не хранимые)

| GQL тип          | Причина                                                                |
|------------------|------------------------------------------------------------------------|
| `metavertex<T?>` | Обычный node occurrence с `NodeHotSlot.flags.META`; тип задаёт schema  |
| `metaedge<T?>`   | Хранится как ребро с `META` и `transition_gid`/внутренним подграфом    |
| `path`           | Результат запроса; не хранится как значение свойства                   |
| graph subtypes   | Интерпретационное ограничение на граф; не самостоятельный тип хранения |

Примечание: `metavertex` и `metaedge` — не отдельные хранимые value types. Их graph occurrence
содержит `is_meta` и binding вложенного/transition graph. Пользовательская atom reference всё
равно кодируется тегом 0x36, а самостоятельный `GraphRef` — тегом 0x37.

### 3.9. QualifiedAtomRef и физический QualifiedDiskAtomRef

Когда endpoint edge occurrence принадлежит другому graph space, одного graph-local
`DiskAtomRef (u64)` недостаточно. Runtime использует каталожный `GraphId`, а graph files —
компактный root-arena `sg_slot`:

```rust
// Runtime identity после catalog resolution.
struct QualifiedAtomRef {
    graph_id:   u64,   // ненулевой GraphId пространства occurrence
    local_ref:  u64,   // DiskAtomRef в координатах graph
}

// Physical identity внутри одного root graph arena.
struct QualifiedDiskAtomRef {
    sg_slot:    u32,   // 0 = текущий graph, остальные slots разрешаются через SubgraphDir
    reserved:   u32,   // 0
    local_ref:  u64,
}
```

`sg_slot` разрешается через `SubgraphDir` и проверенную catalog binding в конкретный ненулевой
`GraphId`; он не экспортируется из storage как публичная identity.

Обычная property-ссылка `atom_ref` всегда разрешается по global `RecordId` и не зависит от
membership. Обе qualified формы являются внутренними маркерами cross-level topology, а не
публичной заменой `AtomRef`.

При чтении `EdgeHotSlot.source_inline[i]`/`target_inline[i]`:
- при `CROSS_LEVEL = 0` значение является обычным `DiskAtomRef` текущего графа;
- при `CROSS_LEVEL = 1` inline хранит только `local_ref`, а соответствующий `*_sg_slot`
  дополняет его до физического `QualifiedDiskAtomRef`. Inline допустим лишь для не более двух
  endpoint'ов одного root-arena graph slot; иначе вся сторона находится в
  `CrossLevelAdjacencyOverflowPage`.

---

## 4. RecordId — публичный идентификатор

`RecordId` — публичный database-local идентификатор глобального атома вида `class:id`.
Первая часть задаёт immutable CLASS namespace, вторая уникальна внутри этого class. `RecordId`
не кодирует graph membership и отделён от внутренних `GlobalAtomId` и graph-local `AtomId`.

### 4.1. Структура

```rust
struct RecordId {
    class: Text,
    id: RecordIdPart,
}

enum RecordIdPart {
    Int(i64),
    String(Text),
    Uuid(u128),           // UUIDv7
    Ulid(u128),           // ULID
    Array(Vec<Value>),    // composite key
    Object(Object),       // public string-keyed object, не PropertyMap
}
```

### 4.2. Кодирование RecordId на диске

```
tag(u8) = 0x10
class_id(u32)             -- stable ClassId из FGDC v7
id_kind(u8):
  0x00 = Int
  0x01 = String
  0x02 = Uuid
  0x03 = Ulid
  0x04 = Array
  0x05 = Object
[id_bytes: variable encoding per kind]
```

Payload кодируется так:

- `Int` — zigzag LEB128;
- `String` — `len(uLEB128) + UTF-8`;
- `Uuid`/`Ulid` — 16 raw big-endian bytes;
- `Array` — `count(uLEB128) + [StorageValue]`;
- `Object` — `count(uLEB128) + [key_len(uLEB128) + UTF-8 key + StorageValue]`.

Class name разрешается в стабильный `ClassId` только через snapshot `ClassCatalog`; неизвестный
либо удалённый class является ошибкой, а не новым неявным namespace. Object-часть `RecordId` не использует `LabelDictionary`: её ключи сортируются
лексикографически по UTF-8 bytes, duplicate keys запрещены. Это делает ID каноническим и
переносимым между графами. Индексирование произвольного object как обычного B+tree key не
поддерживается; primary index сравнивает канонически закодированный `RecordId`.

### 4.3. Отображение RecordId ↔ global atom ↔ graph occurrence

Каждый atom имеет:

- `RecordId` — публичный `class:id`, логически стабильный в пределах БД;
- `GlobalAtomId` — внутренний generation-aware адрес base slot в AtomStore;
- ноль или более graph occurrences, каждая со своим graph-local `AtomId` и `DiskAtomRef`.

AtomStore `primary.fgidx` хранит один unique mapping `canonical RecordId → GlobalAtomId` для всей
БД. Class включён в canonical key, поэтому одинаковые id-parts разных classes не конфликтуют.
Graph `membership.fgidx` хранит `GlobalAtomId → graph-local DiskAtomRef`, а database
`memberships.fgidx` — обратное перечисление occurrences. Ни один из этих индексов не выводит
membership из строки `RecordId`.

### 4.4. Генерация ID по умолчанию

При `CREATE person CONTENT {...}` без явного id:

- Генерируется ULID (криптографически случайный, монотонный по времени).
- `rand::id()` генерирует алфавитно-цифровую строку 20 символов.
- `uuid()` генерирует UUIDv7 (time-ordered).

---

## 5. Бинарное кодирование значений

Канонический value codec используется для **PropHeapPage** и value payload в **WAL**.
Snapshot/cache archives могут использовать отдельный versioned rkyv envelope. B+tree использует
отдельное order-preserving кодирование (§20.2), а не байты этого codec.

### 5.1. Принципы

- Первый байт — **тег типа** (`TypeTag`); §5.2 является единственным нормативным реестром тегов.
- Числа без знака кодируются **LEB128 unsigned** (переменная длина).
- Числа со знаком — **LEB128 zigzag**.
- f64 — 8 байт little-endian.
- Строки — `len(LEB128) + UTF-8 bytes` (без null-terminator).
- Коллекции — `count(LEB128) + [elements]`.
- LEB128 обязан быть каноническим (кратчайшим); decoder ограничивает глубину, длины коллекций,
  строк и общий allocation budget.

### 5.2. Полная таблица кодирования

```
none                →  0x00
null                →  0x01
bool (false)        →  0x02
bool (true)         →  0x03
int                 →  0x04 + i64(zigzag LEB128)
float               →  0x05 + f64(8 байт LE)
decimal             →  0x06 + mantissa i128 (16 байт LE) + scale u8       (18 байт)
string              →  0x07 + len(uLEB128) + utf8_bytes
bytes               →  0x08 + len(uLEB128) + raw_bytes
datetime            →  0x09 + i64(seconds LE) + u32(nanos LE) + u32(tz_id LE)
duration            →  0x0A + header(u16 LE) + [uLEB128 per unit × count]
uuid                →  0x0B + 16 байт (raw big-endian)
ulid                →  0x0C + 16 байт (raw big-endian)
record_id           →  0x10 + class_id(u32 LE) + id_kind(u8) + [id_payload]
array               →  0x11 + count(uLEB128) + [StorageValue]×count
set                 →  0x12 + count(uLEB128) + [StorageValue sorted]×count
object              →  0x13 + count(uLEB128) + [(key_id u32 LE + StorageValue)]×count
0x14                →  reserved in format v1
range               →  0x15 + flags(u8) + [lo: StorageValue] + [hi: StorageValue]
point               →  0x21 + f64(lon LE) + f64(lat LE)
linestr             →  0x22 + count(u32 LE) + [f64 lon + f64 lat]×count
polygon             →  0x23 + ring_count(u32 LE) + [count(u32 LE) + [f64×2]×count]×ring_count
multipt             →  0x24 + count(u32 LE) + [f64×2]×count
multiline           →  0x25 + count(u32 LE) + [linestring_body]×count
multipoly           →  0x26 + count(u32 LE) + [polygon_body]×count
geocoll             →  0x27 + count(u32 LE) + [tagged_geometry]×count
vec_f64             →  0x31 + dim(u16 LE) + [f64 LE]×dim
vec_f32             →  0x32 + dim(u16 LE) + [f32 LE]×dim
vec_i64             →  0x33 + dim(u16 LE) + [i64 LE]×dim
vec_i32             →  0x34 + dim(u16 LE) + [i32 LE]×dim
vec_i16             →  0x35 + dim(u16 LE) + [i16 LE]×dim
atom_ref            →  0x36 + kind(u8) + record_id_body
graph_ref           →  0x37 + graph_id(u64 LE)
qualified_atom_ref  →  0x38 + graph_id(u64 LE) + local_ref(u64 LE)
```

`option<T>` не имеет собственного кодирования: `Some(v)` кодируется как `v`, `None` — как
`none`. Встреча `0x14` в format v1 является `UnknownTypeTag`, а не option payload.

Элементы `set` сортируются лексикографически по полному каноническому байтовому
представлению; повтор такого представления является decode error. При преобразовании в
runtime дополнительно запрещены разные байтовые представления, равные по runtime-семантике
`Value` (например, `int(1)` и `float(1.0)`).

### 5.3. Кодирование object-ключей

В property-`object` ключи хранятся как `key_id (u32)` из `LabelDictionary` (§16).
Это устраняет повторение строк-ключей в PropHeapPage. Пары обязаны быть отсортированы по
`key_id`; duplicate key и неканонический порядок являются decode error. Object-часть
`RecordId` является отдельным контекстом и кодируется строковыми ключами по §4.2.

### 5.4. Границы codec

Canonical codec из §5.2 реализуется вручную и версионируется вместе с storage format. Неизвестный
тег в format v1 возвращает структурированную ошибку `UnknownTypeTag { tag, offset }`. Decoder не
может сохранить его как `Bytes`: длина неизвестного payload не выводится без внешнего framing.
Если внешняя запись имеет длину (например, slot в `PropHeapPage`), её можно целиком пропустить
для recovery/диагностики, но нельзя выдавать как успешно декодированный `Value`.

Фиксированные page headers и slots кодируются безопасными операциями stdlib
(`to_le_bytes`/`from_le_bytes`); layout Rust-структуры не является дисковым ABI.
`rkyv + bytecheck` допускается для Mode 1 snapshots, rebuildable caches и временных query
archives, но не определяет вечный формат §5. Каждый archive имеет envelope:

```text
magic[4] = "FGAR"
format_version: u16 LE = 1
schema_id: u32 LE
payload_length: u64 LE
checksum: u32 LE = CRC32/ISO-HDLC от rkyv_payload
reserved[10] = 0
rkyv_payload: [u8; payload_length]
```

Decoder требует точного совпадения длины записи, версии, `schema_id`, checksum и 16-байтового
выравнивания payload до доступа к архивированному корню. Заголовок имеет размер 32 байта,
поэтому payload сохраняет выравнивание rkyv при чтении всего envelope в `AlignedVec` или из
выровненного mmap. Изменение rkyv schema требует новой версии envelope. `bincode` и `binrw`
могут использоваться для
benchmark/reference tooling, но не задают production value format.

---

## 6. Константы и магические числа

```
PAGE_SIZE             = 16384     -- байт; совпадает с io/mod.rs
PAGE_HEADER_SIZE      = 32        -- байт у всех страниц
PAGE_PAYLOAD_SIZE     = 16352     -- PAGE_SIZE - PAGE_HEADER_SIZE
GEN_MAX               = 0x7F_FFFF -- максимум u23 generation

NULL_PAGE             = 0xFFFF_FFFF_FFFF_FFFF  -- u64::MAX: нет страницы
NULL_SLOT             = 0xFFFF_FFFF            -- u32::MAX: нет слота
NULL_ATOM_SLOT        = 0xFF_FFFF_FFFF          -- максимум u40: не выделяется
NULL_LABEL            = 0xFFFF_FFFF            -- u32::MAX: нет метки
NULL_CLASS            = 0xFFFF_FFFF            -- u32::MAX: нет class
NULL_GLOBAL_ATOM      = 0xFFFF_FFFF_FFFF_FFFF  -- u64::MAX: нет global atom

-- Магические числа страниц (4 байта, little-endian в поле header.magic)
MAGIC_NODE_HOT        = 0x01_4E_48_46   -- b"FHN\x01"
MAGIC_EDGE_HOT        = 0x01_45_48_46   -- b"FHE\x01"
MAGIC_ADJ_OVER        = 0x01_4F_41_46   -- b"FAO\x01"
MAGIC_ADJ_OVER_CL     = 0x01_43_41_46   -- b"FAC\x01"
MAGIC_PROP_HEAP       = 0x01_50_48_46   -- b"FHP\x01"
MAGIC_SUBGR_DIR       = 0x01_47_53_46   -- b"FSG\x01"
MAGIC_FREELIST        = 0x01_4C_46_46   -- b"FFL\x01"
MAGIC_LABEL_DICT      = 0x01_44_4C_46   -- b"FLD\x01"
MAGIC_SUPERBLOCK      = 0x01_42_53_46   -- b"FSB\x01"
MAGIC_SCHEMA          = 0x01_48_43_46   -- b"FCH\x01"  (catalog)
MAGIC_STATS           = 0x01_54_53_46   -- b"FST\x01"
MAGIC_IDX_HASH        = 0x01_48_49_46   -- b"FIH\x01"
MAGIC_IDX_BTREE       = 0x01_42_49_46   -- b"FIB\x01"
MAGIC_IDX_LABEL       = 0x01_4C_49_46   -- b"FIL\x01"
MAGIC_IDX_FT          = 0x01_54_49_46   -- b"FIT\x01"
MAGIC_IDX_VEC         = 0x01_56_49_46   -- b"FIV\x01"
MAGIC_IDX_RTREE       = 0x01_52_49_46   -- b"FIR\x01"  (geometry R-tree)
MAGIC_IDX_COUNT       = 0x01_43_49_46   -- b"FIC\x01"
MAGIC_IDX_REACH       = 0x01_41_49_46   -- b"FIA\x01"  (reachability)
MAGIC_IDX_NBH         = 0x01_4E_49_46   -- b"FIN\x01"  (neighbourhood)
MAGIC_IDX_PATH        = 0x01_50_49_46   -- b"FIP\x01"  (path)
MAGIC_IDX_EE          = 0x01_45_49_46   -- b"FIE\x01"  (EdgeEndpoint)
MAGIC_CHANGEFEED      = 0x01_46_43_46   -- b"FCF\x01"
MAGIC_TENSOR          = 0x01_43_54_46   -- b"FTC\x01"  (TensorCache)
MAGIC_ATOM_STORE      = 0x01_53_41_46   -- b"FAS\x01"  (AtomStoreSuperblock)
MAGIC_ATOM_HOT        = 0x01_48_41_46   -- b"FAH\x01"  (AtomHotPage)
MAGIC_OVERLAY         = 0x01_56_4F_46   -- b"FOV\x01"  (GraphOverlayPage)
MAGIC_CLASS_STATS     = 0x01_53_43_46   -- b"FCS\x01"  (ClassStatistics)

-- Версия формата
STORAGE_FORMAT_VERSION = 1
```

---

## 7. Файловая организация

Нормативная организация target layout этапа 8.1 разделяет database-level AtomStore и
graph-local occurrences:

```text
<database>/
  db.lock                               -- exclusive ownership handle
  db.fgb                                -- атомарно заменяемый DatabaseSuperblock
  db.fgb.tmp                            -- только временный checkpoint output; не authority
  db.fgb.prev                           -- предыдущая валидная checkpoint authority
  wal/
    <first_lsn:016x>.wal                -- единый frame-WAL уровня БД
  schema/
    catalog.fgb                         -- DatabaseCatalog + FileCatalog
  atoms/
    <generation_uuid>/
      superblock.fgb
      superblock.fgb.tmp                -- только временный checkpoint output
      superblock.fgb.prev               -- предыдущий завершённый checkpoint
      atoms_hot.fgb
      properties.fgb
      property_keys.fgb                 -- database-wide PropertyDictionary
      freelist_atoms.fgb
      freelist_props.fgb
      primary.fgidx
      memberships.fgidx
      stats/
        class_stats.fgb
        prop_stats.fgb
      idx/
        std_<ordinal:04>.fgidx
        uniq_<ordinal:04>.fgidx
        cnt_<class_id>.fgidx
  graphs/
    <graph_id:016x>/
      <generation_uuid>/
        superblock.fgb
        nodes_hot.fgb
        edges_hot.fgb
        adjacency_overflow.fgb
        adj_over_cl.fgb
        overlays.fgb
        subgraphs.fgb
        labels.fgb
        freelist_nodes.fgb
        freelist_edges.fgb
        freelist_adj.fgb
        freelist_adj_cl.fgb
        freelist_overlays.fgb
        freelist_subgraphs.fgb
        membership.fgidx
        stats/
          graph_stats.fgb               -- graph counters + degree histograms
          label_stats.fgb               -- per-label cardinality
          prop_stats.fgb                -- per-property histograms
        idx/
          cls_<class_id>.fgidx
          lbl_node_<label_id>.fgidx
          lbl_edge_<label_id>.fgidx
          std_<ordinal:04>.fgidx
          uniq_<ordinal:04>.fgidx
          cnt_<class_id>.fgidx
```

`atoms/<generation>/superblock.fgb` содержит логически WAL-versioned
`AtomStoreSuperblock`; home-копия публикуется только checkpoint metadata COW и не является
`GraphSuperblock`. Global primary отображает canonical `RecordId → GlobalAtomId`.
`memberships.fgidx` является обратным database index `GlobalAtomId → graph occurrences`.
В каждом графе `membership.fgidx` отображает `GlobalAtomId → DiskAtomRef` локальной occurrence.
Base secondary indexes размещаются в AtomStore; explicit effective indexes — в выбранном графе.

Путь поколения и каждый `DataFileId` публикуются только через каталоги; пользовательские имена
в filesystem path не попадают. `ordinal`, `class_id` и `label_id` — проверенные числовые
компоненты. Старые поколения удаляются startup-GC после захвата `db.lock` и только если они
недостижимы из durable catalog.

Primary, standard/unique scalar, class membership, Label и Count layouts входят в target core
format v1. Specialized index layouts остаются отдельными draft side-files. Их добавление после
freeze требует собственного conformance gate, а изменение core bytes — format v2.

### 7.1. Домен журналирования и `DataSpaceId`

Журнал (`wal/`), clocks и checkpoint publication принадлежат **базе данных**, а не отдельному
графу. Один commit атомарно публикует base atoms, reverse memberships, graph occurrences,
topology, overlays и indexes и выполняет один WAL durability barrier независимо от числа
затронутых графов.

Адресное пространство задаёт crate-private value:

```text
DataSpaceId::Database          = 0
DataSpaceId::Graph(GraphId)    = GraphId.get(), всегда > 0
PageKey = (DataSpaceId, DataFileId, PageNo)
```

`GraphId` является `NonZeroU64`; system graph не существует. Значение `0` адресует только
database-level AtomStore и catalog files. Транзакция не может охватывать две базы данных или два
namespace. Backfill можно планировать по data space, но обрезка единого WAL использует общий
watermark.

### 7.2. DatabaseCatalog `FGDC v7` и FileCatalog `FGFC v5`

`db.fgb` хранит root-page ссылки; канонические каталоги лежат в `schema/catalog.fgb` и
публикуются тем же frame-WAL commit, что data pages:

- `DatabaseCatalog` (`FGDC`, payload version 7): `next_graph_id`, `next_schema_id`, IANA timezone
  table, versioned CLASS/FIELD/INDEX definitions, graph catalog, AtomStore generation/bindings и
  глобальные параметры. Property-key strings в payload не дублируются;
- `FileCatalog` (`FGFC`, payload version 5): sealed mapping
  `(DataSpaceId, DataFileId) → (companion_of, role, validated relative path)`.

Каждая запись `FGFC v5` сохраняет 32-байтовый fixed header:

```text
0   8   space_id; 0 = Database, >0 = GraphId
8   8   file_id
16  8   companion_of_file_id; 0 = отсутствует
24  2   path_len
26  1   role
27  5   reserved = 0
32  ... canonical UTF-8 relative path
```

`companion_of` обязателен только для `DataFileKind::Freelist`; для прочих kinds он отсутствует.
Target существует в том же `DataSpaceId` и UUID-generation, не является freelist, superblock,
statistics либо immutable side-file и имеет ровно один companion. В AtomStore companions нужны
для `AtomsHot` и `Properties`; в graph space — для `NodesHot`, `EdgesHot`,
`AdjacencyOverflow`, `CrossLevelAdjacencyOverflow`, `Overlays` и `Subgraphs`.

Оба catalog payload разбиты на bounded chain страниц `SystemCatalog`. Каждая страница содержит
`FGC1`, catalog kind, page-codec version, `next_page`, длину chunk/полного payload и нулевые
reserved/trailing bytes; суммарный payload ограничен 16 МиБ до allocation.

Class definition хранит стабильные `ClassId`/`SchemaVersion`, immutable имя и `NODE|EDGE` kind,
`SCHEMALESS|SCHEMAFULL`, versioned fields и metadata base indexes. Graph definition хранит
ненулевой `GraphId`, имя, family, storage mode, generation и bindings graph-local indexes.
`GraphRef` не кодируется как `RecordId`. Удаление graph удаляет только его catalog entry,
occurrences, topology, overlays и effective indexes; global atoms и classes сохраняются.

Decoder target layout принимает только `FGDC v7` и `FGFC v5`. Реализованные промежуточные
`FGDC v5/v6`, `FGFC v4` и graph-owned atom layout возвращают typed incompatible-format error и не
мигрируются, поскольку core format v1 ещё не заморожен.

**Расширения файлов:**

- `.fgb` — Ferros-Grynn Binary;
- `.fgidx` — Ferros-Grynn Index;
- `.wal` — Write-Ahead Log;
- `.snap` — Snapshot;
- `.fcf` — Ferros-Grynn Changefeed;
- `.chk` — Checkpoint header.

### 7.3. Идентичность physical data-файла (format v1)

| Значение        | Назначение                                                   |
|-----------------|--------------------------------------------------------------|
| `ClassId`       | стабильная logical identity class из общего `next_schema_id` |
| `IndexId`       | стабильная logical identity index из того же schema clock    |
| `SchemaVersion` | версия definition                                            |
| `DataSpaceId`   | database-level либо graph-local address space                |
| `DataFileId`    | identity конкретного physical file одного COW-generation     |
| `ordinal`       | безопасная generation-local часть index filename             |

Physical index binding разрешается по `IndexId`; затем сверяются name, scope, kind, ordered
fields и `SchemaVersion`. `DataFileId` может измениться при COW rebuild, а `IndexId` сохраняется.

| Значение | `DataFileKind`                |
|----------|-------------------------------|
| `0x01`   | `SystemCatalog`               |
| `0x02`   | `GraphSuperblock`             |
| `0x03`   | `NodesHot`                    |
| `0x04`   | `EdgesHot`                    |
| `0x05`   | `AdjacencyOverflow`           |
| `0x06`   | `CrossLevelAdjacencyOverflow` |
| `0x07`   | `Properties`                  |
| `0x08`   | `Subgraphs`                   |
| `0x09`   | `Labels`                      |
| `0x0a`   | `Freelist`                    |
| `0x0b`   | `Statistics`                  |
| `0x0c`   | `Index`                       |
| `0x0d`   | `TensorCache`                 |
| `0x0e`   | `LabelStatistics`             |
| `0x0f`   | `PropertyStatistics`          |
| `0x10`   | `AtomStoreSuperblock`         |
| `0x11`   | `AtomsHot`                    |
| `0x12`   | `Overlays`                    |
| `0x13`   | `ClassStatistics`             |
| `0x14`   | `PropertyDictionary`          |

`0x00` и `0x15..=0xff` зарезервированы. До написания codecs новые discriminants и соответствующие
page types обязаны получить golden vectors. Неизвестное значение при open/recovery является
повреждением либо incompatible format, но не молча пропускаемым расширением.

```text
DataFileId = (instance_id << 8) | DataFileKind
PageKey    = (DataSpaceId, DataFileId, PageNo)
```

`instance_id` занимает 56 бит и не переиспользуется. Registry строится только из проверенного
`FGFC v5`: `(DataSpaceId, DataFileId) → (role, trusted path, DirectFile)`. Superblock files не
удерживают долгоживущий Windows handle, чтобы atomic replace не оставлял ссылку на старый file
object. Отсутствующий id является fatal corruption.

---

Файл `store.fgb` - глобальный суперблок хранилища:
```
Offset  Size  Тип   Поле            Описание
0       32    -     PageHeader      (magic=MAGIC_SUPERBLOCK, type=StorageSuperblock)
32      4     u32   engine_version  -- вверсия движка, при которой был создан store
36      4     -     _pad
40      var   -     ns_list         -- список namespace    
```

Файл `ns.fgb` - суперблок пространства имён:
```
Offset  Size  Тип   Поле            Описание
0       32    -     PageHeader      (magic=MAGIC_SUPERBLOCK, type=NamespaceSuperblock)
32      32    [u8]  ns_name         -- имя namespace, макс. 32 символа в ASCII кодировке
64      var   -     db_list         -- список database
```

Файл `db.fgb` - суперблок базы данных:
```
Offset  Size  Тип   Поле                        Описание
0       32    -     PageHeader (type=DatabaseSuperblock, page_no=0)
32      32    [u8]  db_name                     -- UTF-8 NUL-terminated, max 31 bytes
64      4     u32   format_version              -- версия формата хранилища (§6: STORAGE_FORMAT_VERSION)
68      4     u32   last_back_compat_version    -- последняя версия, совместимая с текущей
72      8     u64   generation
80      8     u64   checkpoint_lsn
88      8     u64   checkpoint_record_lsn
96      8     u64   durable_visible_lsn
104     8     u64   next_lsn
112     8     u64   commit_ts
120     8     u64   next_tx_id
128     8     u64   next_file_instance_id       -- следующий id; 2^56 = exhausted sentinel
136     8     u64   system_backfilled_lsn
144     8     u64   graph_catalog_root_page     -- root DatabaseCatalog chain
152     8     u64   file_catalog_root_page      -- root FileCatalog chain
160     4     u32   graph_count
164     4     u32   flags                       -- format v1: 0
168     ...   -     _reserved                   -- только нули
```

`PageHeader.page_lsn == checkpoint_record_lsn`. Для `generation == 0` оба checkpoint-LSN равны
нулю; `checkpoint_lsn ≤ durable_visible_lsn`, а `next_lsn` строго больше всех сохранённых WAL
часов. Для bootstrap generation 0 поля root могут быть `NULL_PAGE`; `create_local` публикует обе
не-NULL chain roots до возврата пользователю. После этого missing root — corruption.

## 8. Универсальный заголовок страницы

Все страницы всех файлов начинаются с идентичного 32-байтового заголовка.

```
PageHeader:
Offset  Size  Тип   Поле        Описание
0       4     u32   magic       Тип страницы (§6)
4       2     u16   page_type   Тип страницы
6       2     u16   flags       Per-page flags
8       8     u64   page_lsn    LSN последней успешной записи на страницу
16      8     u64   checksum    portable RapidHash V3 всего PAGE_SIZE (seed=0, checksum=0)
24      8     u64   page_no     Самоссылка (для обнаружения torn write)
Total = 32 байт
```

Старший байт `magic` равен `STORAGE_FORMAT_VERSION`; reader format v1 отклоняет другое значение.
Биты `flags`, не перечисленные ниже, зарезервированы и обязаны быть нулевыми. Checksum использует
`rapidhash::v3::RapidSecrets::seed_cpp(0)` (набор secrets совместим с reference C++), поэтому его
байты стабильны между платформами и версиями компилятора. Нормативные векторы format v1:

| Вход (ровно 16384 байта)                                                                   | RapidHash V3         |
|--------------------------------------------------------------------------------------------|----------------------|
| Все байты `0x00`                                                                           | `0x3796d02825bf3d75` |
| `MAGIC_SUPERBLOCK`, `page_type=0x0001`, остальные поля/полезная нагрузка `0`, checksum `0` | `0x3fa0f2d42971e713` |

**PageType:**

```
0x0001  StorageSuperblock             
0x0002  NamespaceSuperblock             
0x0003  DatabseSuperblock             
0x0004  NodeHot             
0x0005  EdgeHot             
0x0006  AdjacencyOverflow
0x0007  CrossLevelAdjacencyOverflow
0x0008  Reserved (не переиспользовать)
0x0009  Reserved (не переиспользовать)
0x000A  PropHeap            
0x000B  PropLargeHeap      
0x000C  SubgraphDir         
0x000D  Freelist            
0x000E  LabelDict           
0x000F  LabelDictEntry    
0x0010  HashIndex     
0x0011  GraphSuperblock          
0x0012  SchemaRoot          
0x0013  StatisticsRoot      
0x0014  LabelStatistic     
0x0015  PropertyHistogram     
0x0016  DegreeHistogram     
0x0017  IndexBtreeInternal  
0x0018  IndexNonUniqueBtreeLeaf  
0x0019  IndexUniqueBtreeLeaf
0x001A  IndexPostingBtreeLeaf
0x001B  IndexLabelArray
0x001C  IndexFulltextHeader
0x001D  IndexFulltextDict
0x001E  IndexFulltextHash
0x001F  IndexFulltextPosting
0x0020  IndexVector
0x0021  IndexVectorData
0x0022  IndexGeometryRtreeHeader
0x0023  IndexGeometryRtreeInternal
0x0024  IndexGeometryRtreeLeaf
0x0025  IndexCountSingle
0x0026  IndexReachabilityHeader
0x0027  IndexReachabilityTreeCover
0x0028  IndexReachabilityGrail
0x0029  IndexReachabilityFerrari
0x002A  IndexReachabilityBfl
0x002B  IndexReachabilityChainCover
0x002C  IndexNeighbourhoodHeader
0x002D  IndexNeighbourhoodExact
0x002E  IndexNeighbourhoodSketch
0x002F  IndexNeighbourhoodLandmarkList
0x0030  IndexPathHeader
0x0031  IndexPathLandmarkList
0x0032  IndexPathPatternCatalog
0x0033  IndexEdgeEndpointHeader
0x0034  IndexEdgeEndpointEntry
0x0035  ChangefeedHeader
0x0036  ChangefeedEntry
0x0037  TensorCache
0x0038  AtomStoreSuperblock
0x0039  AtomHot
0x003A  GraphOverlay
0x003B  ClassStatistic
0x003C  PropertyDictionaryRoot
0x003D  PropertyDictionaryEntry
0x003E  IndexDerivedArray
0x003F  IndexDerivedRecords
```

`0x0038..=0x003D` принадлежат target layout 8.1; `0x003E..=0x003F` зарезервированы утверждёнными
derived-index codecs §20.8. Точные `AtomHotSlot`, `NodeHotSlot` и `EdgeHotSlot` заданы в
§§10–11, dictionary layouts — в §16, overlay — в §17.5. Использовать Rust struct layout как
дисковый ABI запрещено.

**Флаги страницы (битовые):**

```
0x0001  FLAG_DIRTY        -- в памяти изменена, не сброшена на диск (runtime)
0x0002  FLAG_COMPRESSED   -- payload сжат LZ4 (для PropHeap при sparse props)
0x0004  FLAG_OVERFLOW     -- продолжение overflow-цепочки
0x0008  FLAG_LEAF         -- leaf в B+tree
0x0010  FLAG_SCHEMA       -- страница схемы (не данные)
0x0020  FLAG_STATS        -- страница статистики
```

**Torn write detection:**

```
При чтении: if page_no != expected_page_no
            OR rapid_hash_v3(bytes with checksum=0, seed=0) != stored_checksum
         → страница повреждена → REDO из WAL
```

---

## 9. GlobalAtomId и graph-local AtomId/DiskAtomRef

`GlobalAtomId` и graph-local `AtomId` — разные Rust newtypes с одинаковой packed layout. Первый
адресует base slot AtomStore, второй — occurrence slot одного graph. Смешивание типов без явного
resolve запрещено. Для локальной `DiskAtomRef` поколение хранится в hot slot и восстанавливается
при загрузке; database primary/reverse membership сохраняют полный generation-aware
`GlobalAtomId`, чтобы stale global handle не мог привязаться к переиспользованному base slot.

### 9.1. Разрядность

**GlobalAtomId / AtomId = u64**

```
биты 63..41   gen         u23  (8 388 608 поколений на слот)
бит  40       kind        0=Node, 1=Edge
биты 39..0    slot_index  u40
```

Для live ID `gen <= GEN_MAX` и `slot_index != NULL_ATOM_SLOT`; значения slot-полей hot pages
проверяются по тем же границам до сборки Rust newtype.

**DiskAtomRef = u64** — graph-local occurrence reference без поколения:

```
бит  40       kind        0=Node, 1=Edge
биты 39..0    slot_index  u40
биты 63..41   зарезервированы, при записи нули
```

Null-значение физической ссылки: `NULL_DISKATOMREF = u64::MAX`. `NULL_ATOM_SLOT` (все 40 бит
`slot_index`) никогда не выделяется; `NULL_SLOT = u32::MAX` остаётся отдельным sentinel для
32-битных heap/subgraph/catalog slots. Decoder распознаёт full-width null references до проверки
зарезервированных битов.

**Почему u40, а не u31.** u40 даёт ~1.1 трлн slots. В AtomStore это единое пространство
слотов обоих kinds; kind-бит является быстрым type check и обязан совпадать с class и slot.
В graph space node/edge slots разнесены по отдельным файлам, поэтому одинаковый `slot_index`
может существовать у node и edge. `u128` удвоил бы размер hot references и ухудшил плотность
SoA.

**Адресация не переполняется.** При `slot_index < 2⁴⁰` AtomStore требует менее 3.24·10⁹
страниц (`slot / 340`, около 53 ТБ), а каждый graph-local kind — менее 1.08·10¹⁰ страниц
(`slot / 102`, около 177 ТБ). Оба byte offset укладываются в `u64` с запасом. Пространства имён
`label_id`, `class_id`, `key_id` и локальные subgraph slots остаются `u32` — они нумеруют
schema/graph metadata, а не атомы, и с числом атомов не связаны.

```rust
const KIND_SHIFT: u32 = 40;
const SLOT_MASK:  u64 = (1 << 40) - 1;

// AtomId → DiskAtomRef
fn to_disk_ref(id: AtomId) -> DiskAtomRef {
    ((id.is_edge() as u64) << KIND_SHIFT) | (id.slot() & SLOT_MASK)
}

// DiskAtomRef → AtomId (требует восстановленного поколения из hot-слота)
fn from_disk_ref(r: DiskAtomRef, gen: u32) -> AtomId {
    let slot = r & SLOT_MASK;
    if (r >> KIND_SHIFT) & 1 == 0 { AtomId::new_node(slot, gen) } else { AtomId::new_edge(slot, gen) }
}
```

### 9.2. Переполнение поколений

Поколение занимает 23 бита — 8 388 608 переиспользований одного слота. Достижение этого предела маловероятно, но должно иметь определённое поведение, а не приводить к ABA (когда устаревший `AtomId` внезапно снова становится валидным и указывает на чужой атом).

**Стратегия — retirement слота.** Слот, поколение которого достигло `GEN_MAX`, при освобождении **не возвращается** во freelist и больше не выделяется. Он остаётся помеченным как tombstone до полного дефрагментирования (§24.3), которое переназначает слоты и обнуляет поколения.

Отказ вырождается в микро-утечку одного слота вместо нарушения корректности. Счётчик выбывших слотов доступен через `INFO`; систематический рост — сигнал, что рабочая нагрузка пересоздаёт атомы в одних и тех же слотах и стоит запланировать дефрагментацию.

---

## 10. AtomHotPage и graph-local NodeHotPage

Это две разные durable families. `AtomHotPage` хранит database-level base atom, а
`NodeHotPage` — occurrence этого атома в одном графе. Объединять их нельзя: один
`GlobalAtomId` может одновременно иметь разные labels, overlays, adjacency и nested bindings
в нескольких graphs.

### 10.1. AtomHotSlot

`AtomHotSlot` имеет размер 48 байт и не содержит graph-local данных:

```text
Offset  Size  Тип       Поле                 Описание
0       4     u32       generation           Поколение GlobalAtomId
4       4     u32       class_id             ClassId; NULL_CLASS только у tombstone
8       1     u8        flags                bit 0 TOMBSTONE; bit 1 EDGE; bits 2..7 = 0
9       7     u8[7]     _reserved            Только нули
16      8     u64       record_id_page        PageNo canonical RecordId; NULL_PAGE у tombstone
24      4     u32       record_id_slot        SlotDirectory index; NULL_SLOT у tombstone
28      4     u32       record_id_len         Длина canonical RecordId bytes
32      8     u64       properties_page       PageNo base PropertyMap; NULL_PAGE = пустая map
40      4     u32       properties_slot       SlotDirectory index; NULL_SLOT = пустая map
44      4     u32       properties_len        Длина canonical PropertyMap bytes; 0 = пустая map
Total = 48 байт
```

Обе ссылки адресуют `properties.fgb`: `record_id_*` — canonical `RecordId` из §4.2,
`properties_*` — запись §13.2. Контекст поля задаёт decoder, поэтому дополнительный record tag
не нужен. Значение slot `0xFFFF_FFFE` обозначает large-object chain (§13.4).

Инварианты live slot:

- `record_id_*` обязателен; его class совпадает с `class_id`, а global primary содержит ровно
  `RecordId → GlobalAtomId` этого слота;
- `flags.EDGE`, kind-бит `GlobalAtomId` и immutable kind `ClassSchema` совпадают;
- `properties_page = NULL_PAGE`, `properties_slot = NULL_SLOT` и `properties_len = 0`
  кодируют пустую base map;
- graph membership, label, topology и overlay здесь отсутствуют.

Tombstone кодируется канонически: значим только новый `generation`; `class_id = NULL_CLASS`,
установлен только `TOMBSTONE`, обе ссылки null, длины равны нулю. Неизвестные flags,
несогласованные sentinels и ненулевые reserved bytes являются corruption.

### 10.2. AtomHotPage layout

```text
Offset   Size   Содержимое
0        32     PageHeader (magic=MAGIC_ATOM_HOT, type=AtomHot)
32       16320  340 × AtomHotSlot (340 × 48 = 16320)
16352    32     _padding = 0
Total = PAGE_SIZE байт
```

AtomStore использует единое `slot_index` пространство для NODE и EDGE:

```text
page_no          = GlobalAtomId.slot_index / 340
slot_within_page = GlobalAtomId.slot_index % 340
byte_offset      = 32 + slot_within_page × 48
```

### 10.3. Graph-local NodeHotSlot

`NodeHotSlot` имеет размер 160 байт и хранит только node occurrence:

```text
Offset  Size  Тип        Поле                Описание
0       4     u32        generation          Поколение graph-local AtomId
4       4     u32        subgraph_slot       root-arena slot вложенного graph; NULL_SLOT иначе
8       4     u32        label_id            Graph-local LabelId; NULL_LABEL = нет
12      4     u32        _reserved0          Только нули; прежний table_id удалён
16      4     u32        adj_out_count       Полное число исходящих edges
20      4     u32        adj_in_count        Полное число входящих edges
24      4     u32        adj_undir_count     Полное число undirected edges
28      1     u8         flags               [7] META [6] TOMBSTONE [4] HAS_OVERLAY;
                                              [5], [3:0] = 0
29      3     u8[3]      _pad                Только нули
32      8     u64        overflow_out        Первая out overflow page; NULL_PAGE = нет
40      8     u64        overflow_in         Первая in overflow page; NULL_PAGE = нет
48      8     u64        overflow_undir      Первая undirected overflow page; NULL_PAGE = нет
56      8     u64        overlay_page        PageNo в overlays.fgb; NULL_PAGE = нет overlay
64      4     u32        overlay_slot        SlotDirectory index; NULL_SLOT = нет overlay
68      4     u32        overlay_len         Длина canonical sparse overlay; 0 = нет overlay
72      8     u64        global_atom         Полный generation-aware GlobalAtomId NODE
80      32    [u64; 4]   adj_out             Inline graph-local DiskAtomRef edges
112     32    [u64; 4]   adj_in              Inline graph-local DiskAtomRef edges
144     16    [u64; 2]   adj_undir           Inline graph-local DiskAtomRef edges
Total = 160 байт
```

`HAS_OVERLAY` установлен тогда и только тогда, когда `overlay_page != NULL_PAGE`,
`overlay_slot != NULL_SLOT` и `overlay_len > 0`. При снятом флаге эта тройка имеет канонические
значения `NULL_PAGE`/`NULL_SLOT`/`0`, а read path не открывает и не читает `overlays.fgb`.
`META` совпадает с наличием `subgraph_slot`; `global_atom` обязан быть не `NULL_GLOBAL_ATOM`,
иметь NODE kind и существовать в AtomStore на snapshot. Class и base properties разрешаются
через него и в occurrence не дублируются.

Inline arrays плотно заполнены от индекса 0. Для out/in count не больше 4 не имеет overflow, а
больший count требует первую overflow page; для undirected граница равна 2. `u32` counts
сохраняют степени выше 65 535. У tombstone значим только новый `generation`: counts равны нулю,
`label_id = NULL_LABEL`, `subgraph_slot = NULL_SLOT`, `global_atom = NULL_GLOBAL_ATOM`, все page
и slot references имеют null sentinel, inline arrays заполнены `NULL_DISKATOMREF`, а из flags
установлен только `TOMBSTONE`.

`VISITED`, `IN_RESULT` и `CUT_VERTEX` являются snapshot/query-local состоянием алгоритмов. Они
хранятся в runtime/ECS sidecar и никогда не записываются в `NodeHotSlot`.

### 10.4. NodeHotPage layout

```text
Offset   Size   Содержимое
0        32     PageHeader (magic=MAGIC_NODE_HOT, type=NodeHot)
32       16320  102 × NodeHotSlot (102 × 160 = 16320)
16352    32     _padding = 0
Total = PAGE_SIZE байт
```

```text
page_no          = AtomId.slot_index / 102
slot_within_page = AtomId.slot_index % 102
byte_offset      = 32 + slot_within_page × 160
```

---

## 11. Graph-local EdgeHotPage

`EdgeHotPage` хранит edge occurrences. Base identity и свойства разрешаются через
`GlobalAtomId`, но topology, endpoints, weight, label, overlay и nested/transition binding
принадлежат конкретному graph. Поэтому один global edge может иметь независимые occurrences в
нескольких graphs.

### 11.1. EdgeHotSlot

```text
Offset  Size  Тип        Поле                     Описание
0       1     u8         topology                 0 Directed, 1 Undirected, 2 Bidirectional,
                                                  3 HyperDirected, 4 HyperUndirected, 5 Loop
1       1     u8         flags                    bit 0 TOMBSTONE; 2 META; 6 CROSS_LEVEL;
                                                  7 HAS_OVERLAY; bits 1, 3..5 = 0
2       1     u8         edge_kind_ext            bit 0 HAS_TRANSITION; bits 1..7 = 0
3       1     u8         _pad                     Ноль
4       4     u32        generation               Поколение graph-local AtomId
8       4     u32        source_count             Полное число source endpoints
12      4     u32        target_count             Полное число target endpoints
16      4     u32        label_id                 Graph-local LabelId; NULL_LABEL = нет
20      4     u32        _reserved0               Только нули; прежний table_id удалён
24      8     f64        weight                   0x7FF8_0000_0000_0000 = веса нет
32      4     u32        transition_sg_slot        root-arena transition slot; NULL_SLOT = нет
36      4     u32        source_sg_slot            root-arena slot inline source side; иначе NULL_SLOT
40      4     u32        target_sg_slot            root-arena slot inline target side; иначе NULL_SLOT
44      4     u32        overlay_slot             SlotDirectory index; NULL_SLOT = нет overlay
48      4     u32        overlay_len              Длина canonical sparse overlay
52      4     u32        _pad1                    Ноль
56      8     u64        overlay_page             PageNo в overlays.fgb; NULL_PAGE = нет overlay
64      8     u64        source_overflow          Первая local source overflow page; иначе NULL_PAGE
72      8     u64        target_overflow          Первая local target overflow page; иначе NULL_PAGE
80      8     u64        global_atom              Полный generation-aware GlobalAtomId EDGE
88      8     u64        source_sg_overflow       Qualified source overflow page; иначе NULL_PAGE
96      8     u64        target_sg_overflow       Qualified target overflow page; иначе NULL_PAGE
104     8     u64        _reserved1               Ноль
112     16    [u64; 2]   source_inline            Inline graph-local DiskAtomRef endpoints
128     16    [u64; 2]   target_inline            Inline graph-local DiskAtomRef endpoints
144     16    u8[16]     _reserved2               Нули
Total = 160 байт
```

`HAS_OVERLAY` обязан точно совпадать с наличием overlay reference. `META`, `HAS_TRANSITION` и
`transition_sg_slot != NULL_SLOT` совпадают; неизвестные bits являются corruption.
Для отсутствующего overlay действует та же каноническая тройка `NULL_PAGE`/`NULL_SLOT`/`0`, что
и у node occurrence. `global_atom` не равен `NULL_GLOBAL_ATOM`, имеет EDGE kind и существует в
AtomStore на snapshot. Class/base properties не дублируются. Writer использует для отсутствия
веса canonical quiet-NaN `0x7FF8_0000_0000_0000` (little-endian bytes
`00 00 00 00 00 00 F8 7F`); иной NaN decoder отклоняет, а пользовательский numeric NaN остаётся
запрещён §3.

Инварианты topology и counts:

- для `Directed`, `Bidirectional` и `Loop`: `source_count + target_count = 2`;
- для `Undirected`: сумма равна 2 и `target_count = 0`;
- для `HyperDirected`: сумма не меньше 2;
- для `HyperUndirected`: сумма не меньше 2 и `target_count = 0`;
- count не больше 2 хранится inline; больший count требует соответствующий overflow;
- каждый endpoint является graph-local `DiskAtomRef` любого kind.

При `CROSS_LEVEL = 0` inline/ordinary overflow принадлежат текущему graph, оба `*_sg_slot` равны
`NULL_SLOT`, а оба `*_sg_overflow` — `NULL_PAGE`. При `CROSS_LEVEL = 1` непустая сторона остаётся
inline только если все её endpoints принадлежат одному root-arena graph slot; этот slot явно
записывается в соответствующий `*_sg_slot`. Иначе вся ordered сторона хранится как
`QualifiedDiskAtomRef` в `*_sg_overflow`. Делить сторону между local и qualified storage
запрещено. Slot 0 означает текущий graph; остальные root-arena slots не переиспользуются на
другой глубине.

У tombstone значим только новый `generation`: topology и `edge_kind_ext` кодируются нулём,
counts и padding равны нулю, `weight` содержит canonical sentinel, `label_id`,
`transition_sg_slot`, оба `*_sg_slot` и `overlay_slot` равны своим null sentinel,
`overlay_len = 0`, `global_atom = NULL_GLOBAL_ATOM`, page references имеют `NULL_PAGE`, inline
arrays заполнены `NULL_DISKATOMREF`, а из flags установлен только `TOMBSTONE`. `VISITED`,
`IN_RESULT` и `BRIDGE` являются runtime/ECS markers и на диск не записываются.

### 11.2. EdgeHotPage layout

```text
Offset   Size   Содержимое
0        32     PageHeader (magic=MAGIC_EDGE_HOT, type=EdgeHot)
32       16320  102 × EdgeHotSlot (102 × 160 = 16320)
16352    32     _padding = 0
Total = PAGE_SIZE байт
```

```text
page_no          = AtomId.slot_index / 102
slot_within_page = AtomId.slot_index % 102
byte_offset      = 32 + slot_within_page × 160
```

Node и edge occurrences имеют одинаковый размер и плотность, но остаются в разных файлах:
так direct addressing сохраняет отдельные kind-local slot spaces, а traversal не читает
edge-only topology вместе с node adjacency.

### 11.3. Зарезервированные коды

Коды PageType `0x0008` и `0x0009` зарезервированы для совместимости формата и не
переиспользуются. Отдельных страниц edge-to-edge incidence нет: такие endpoints хранятся в
`EdgeHotSlot` и обычных endpoint overflow pages (§11–§12).

---

## 12. AdjacencyOverflowPage

```
Offset  Size    Тип    Поле       Описание
0       32      —      PageHeader (magic=MAGIC_ADJ_OVER, type=AdjacencyOverflow)
32      8       u64    next_page  Следующая страница цепочки; NULL_PAGE = последняя
40      4       u32    count      Число entries на этой странице
44      4       u32    _pad
48      16336   —      entries    Массив u64 (DiskAtomRef ребра)
                                  → 16336 / 8 = 2042 entries per page
Total = PAGE_SIZE байт
```

**Важно:** entries в overflow-странице являются **продолжением** inline-массива из hot-слота.
Читать: сначала inline из hot-слота, затем последовательно все страницы цепочки.

**Ёмкость:** Для узла со степенью 100 000:
- Inline: 4 ребра в NodeHotSlot
- Overflow: `ceil(99 996 / 2042) = 49` страниц цепочки

### 12.1. CrossLevelAdjacencyOverflowPage

То же самое, что и AdjacencyOverflowPage, но имеет другой вид entries для overflow-страницы с несколькими cross-level endpoints:

```
PageHeader: (magic=MAGIC_ADJ_OVER_CL, type=CrossLevelAdjacencyOverflow)
```

```
overflow_entry_cl (16 байт):
      sg_slot:    u32   -- root-arena slot подграфа; 0 = текущий граф
      side:       u8    -- 0=source, 1=target
      _pad:       u8[3]
      local_ref:  u64   -- DiskAtomRef в пространстве подграфа
→ 16336 / 16 = 1021 entries per page
```

Пара `{ sg_slot: u32, local_ref: u64 }` соответствует физическому `QualifiedDiskAtomRef`;
overflow entry использует его четыре промежуточных байта для `side` и трёх нулевых padding
bytes. Entries одной стороны сохраняют пользовательский порядок; `side` обязан совпадать с
chain, указанной соответствующим полем hot-slot.

---

## 13. PropHeapPage — куча variable records

В `AtomStore/properties.fgb` хранит canonical `RecordId` и base `PropertyMap` как отдельные
opaque records в **slotted page** формате. Тип записи задаётся owning-полем `AtomHotSlot` и не
дублируется внутри heap; decoder не вправе интерпретировать запись без этого контекста. В
graph-local `overlays.fgb` тот же slotted-page принцип применяется отдельным
`GraphOverlayPage` codec. Records растут с конца к началу, SlotDirectory — от offset 40 к концу.

### 13.1. Layout

```
Offset  Size  Тип    Поле         Описание
0       32    —      PageHeader (magic=MAGIC_PROP_HEAP, type=PropHeap)
32      2     u16    slot_count   Число слотов (включая удалённые)
34      2     u16    free_start   Смещение начала свободного пространства
36      2     u16    free_end      Начало zone записей (следующая позиция для новой записи, записи растут к меньшим адресам)
38      2     u16    _pad
40      16334 —      [SlotDirectory ... GAP ... records]
              SlotEntry(4B) × slot_count  ← растёт вниз от offset 40
              [свободное место]
              [records] ← растут вверх от конца страницы
Total = PAGE_SIZE байт
```

SlotDirectory — это массив SlotEntry, начинающийся с offset 40 в PropHeapPage
и растущий в сторону увеличения смещений (к концу страницы).

**SlotEntry (4 байта):**

```
Offset  Size  Тип    Поле
0       2     u16    record_offset  Смещение записи от начала страницы; 0 = слот пустой
2       2     u16    record_len     Длина записи в байтах
```

Records (PropertyMap записи) размещаются с конца страницы в обратном порядке
(растут к меньшим смещениям, навстречу SlotDirectory).

Инварианты:
- `free_start = 40 + slot_count × 4` (конец SlotDirectory)
- `free_end` — явно хранится; начало zone записей со стороны конца страницы 
  (начало свободной зоны со стороны records)
- `free_bytes = free_end − free_start`

Доступ к записи i:
- `SlotEntry = page_bytes[40 + i×4 .. 40 + i×4 + 4]`
- `offset = SlotEntry.record_offset`, если 0 → слот удалён (tombstone)
- `len = SlotEntry.record_len`
- `data = page_bytes[offset .. offset + len]`

Вставка новой записи:
1. Проверить: `free_bytes ≥ record_len + 4` (4 байта на новый SlotEntry)
2. `new_record_start = free_end − record_len`
3. Записать данные в `page_bytes[new_record_start .. new_record_start + record_len]`
4. Добавить `SlotEntry { record_offset: new_record_start, record_len }` в SlotDirectory 
   по адресу: `page_bytes[free_start .. free_start + 4]`
5. `slot_count += 1`; `free_start += 4`; `free_end = new_record_start`

Удаление записи (tombstone):
1. `SlotEntry[i].record_offset = 0` — tombstone
2. slot_count не изменяем (слот остаётся)
3. free_end не изменяем (место физически не возвращается до compaction)

Compaction страницы:
1. Пройти все `SlotEntry` с `record_offset != 0`
2. Переупаковать записи подряд с конца страницы
3. Обновить record_offset в SlotEntry для каждой перемещённой записи
4. Обновить free_end

### 13.2. Формат PropertyMap записи

```
num_props: uLEB128
для каждого свойства:
  key_id:  u32 LE       -- плотный database-wide id из PropertyDictionary
  value:   encoded StorageValue  -- §5
```

Пары записываются строго по возрастанию `key_id`. Decoder отклоняет duplicate key и
неканонический порядок. Эта сортировка относится только к дисковой записи: порядок свойств
runtime `PropertyMap` семантически незначим.

### 13.3. Сжатие

Если `FLAG_COMPRESSED = 1` в PageHeader, bytes `[free_end..PAGE_SIZE]` сжаты LZ4 frame.
Байты `[32..40]` сжимать нельзя, так как это метаданные, и их сжатие не позволит прочитать их быстро без декомпрессии всей страницы.
Применяется при `serialized_size > FG_PROPS_COMPRESS_THRESHOLD` (default 512 байт).
Сжатие опциональное; решение принимается при записи страницы. 
Сжатие также применимо к PropLargeChunkPage.

### 13.4. Мультистраничные записи (Large Objects)

Если canonical `RecordId` или `PropertyMap` больше `PAGE_PAYLOAD_SIZE / 2` (>8176 байт), запись
хранится как Large Object: цепочка `PropLargeChunkPage` с `next_page` указателями.

```
PropLargeChunkPage:
  PageHeader (magic=MAGIC_PROP_HEAP, type=PropLargeHeap; FLAG_OVERFLOW set)
  next_page: u64
  data_len:  u32   -- байт данных на этой странице
  _pad:      u32
  data:      [u8]  -- raw canonical RecordId либо PropertyMap bytes
```

В соответствующем `AtomHotSlot.*_slot = 0xFFFF_FFFE` значение `*_page` указывает на первый
chunk. Graph overlay использует тот же sentinel в `NodeHotSlot.overlay_slot` или
`EdgeHotSlot.overlay_slot`, но цепочка принадлежит `overlays.fgb` и сохраняет
`MAGIC_OVERLAY`/`GraphOverlay` page type.

### 13.5. Ссылка на запись — PropertyRef

```rust
struct PropertyRef {
    page_no:    u64,  // *_page из owning hot-слота
    slot_index: u32,  // индекс SlotDirectory или large-object sentinel
    length:     u32,  // canonical byte length для быстрой проверки
}
```

Для inline record `slot_index ≤ u16::MAX`; значения `0x0001_0000..=0xFFFF_FFFD`
зарезервированы. `NULL_SLOT` означает отсутствие, `0xFFFF_FFFE` — large object. Смещение inline
record берётся из `SlotEntry[slot_index].record_offset` и не сохраняется в hot slot.

### 13.6. Свойства и аналитика: OLTP row-store + колоночный кэш

`PropHeapPage` — **намеренно строково-ориентированное OLTP-хранилище**. В AtomStore он
содержит отдельно canonical `RecordId` и base `PropertyMap`, на которые ссылается
`AtomHotSlot`; graph-local sparse overlays хранятся в `GraphOverlayPage`. Это оптимально для
доминирующего сценария (взять атом — получить все его свойства) и плохо для аналитического
скана одного поля по миллионам атомов, где приходится читать целые записи ради одного значения.

Разрешение — не превращать `PropHeapPage` в колоночный формат, а дать **явный путь материализации**:

```
DiskMetaGraph → выгрузка нужного поля колонкой → SparseSet<T> / ECS → алгоритм
```

Это соответствует существующему разделению: горячая топология на диске, аналитические компоненты — в ECS-слое, который уже колоночный по построению.

#### PropertyColumnCachePage — дисковый колоночный кэш

Чтобы повторные аналитические проходы не платили за материализацию каждый раз, вводится кэш по образцу `TensorCachePage` (§26) — с той же логикой инвалидации.

Файл: `props_cache/<label_id>_<key_id>.fgb`, magic `MAGIC_PROP_COL`, тип страницы `PropColumnCache`.

```
Offset  Size  Тип   Поле          Описание
0       32    —     PageHeader
32      4     u32   label_id
36      4     u32   key_id        Интернированный ключ свойства
40      1     u8    elem_type     TypeTag элемента колонки
41      7     —     _pad
48      8     u64   first_slot    DiskAtomRef.slot первого элемента диапазона
56      8     u64   elem_count    Число элементов на странице
64      8     u64   next_page     Следующая страница цепочки; NULL_PAGE = последняя
72      8     u64   build_lsn     LSN, на котором построен кэш
80      8     u64   presence_page page_no bitmap существования свойства; NULL_PAGE = все present
88      8     u64   validity_page page_no bitmap null-validity; NULL_PAGE = все non-null
96      16288 —     data          Плотный массив [value; elem_count] фиксированной ширины
Total = PAGE_SIZE байт
```

Свойства и ограничения:
- Кэшируются **только поля фиксированной ширины** (числа, `datetime`, `duration`, `decimal`, векторы фиксированной размерности, `bool`). Строки и вложенные структуры не кэшируются.
- Элементы упорядочены по `DiskAtomRef.slot`, поэтому доступ по атому — арифметика, а не поиск.
  `presence=0` означает `none`; `presence=1 && validity=0` — `null`;
  `presence=1 && validity=1` — значение. Одна validity bitmap недостаточна, потому что она не
  различает `none` и `null`.
- Инвалидация — как у `TensorCachePage`: `build_lsn < текущий checkpoint_lsn` при любой мутации соответствующего поля делает кэш устаревшим. Устаревший кэш не используется молча: планировщик либо перестраивает его, либо идёт по обычному пути.
- Раскладка `decimal` (§3.3) выбрана так, что колонка `decimal` побайтово совместима с Arrow `Decimal128Array`.

#### Продвинутая аналитика — через Arrow, а не своим кодом

Собственные реализации dictionary/RLE/bitpacking-кодирования и колоночного исполнителя сознательно **не разрабатываются**: это отдельная колоночная СУБД внутри графовой, с несоразмерной стоимостью сопровождения.

Вместо этого набор колонок материализуется как `PropertyBatch`: Arrow `RecordBatch`, отдельные
presence bitmaps и mapping `PropertyKeyId → Arrow field`. `Arrow::MapArray` не заменяет
row-oriented `PropertyMap`. Для типов фиксированной ширины данные кэша могут передаваться в
Arrow без копирования; DataFusion работает только поверх этого batch-слоя.

Граница ответственности: движок отвечает за граф, топологию, транзакции и материализацию колонок; тяжёлая колоночная аналитика — за пределами его ядра.

---

## 14. SubgraphDirPage — директория подграфов

Хранит компактную проверяемую привязку **непосредственных** дочерних графов текущего address
space. Полное дерево получается рекурсивным обходом дочерних address spaces; содержимое графа в
эти 32 байта не встраивается.

### 14.1. SubgraphDirEntry (32 байт)

```
Offset  Size  Тип    Поле           Описание
0       4     u32    sg_slot        Уникальный slot единой root-arena; 0 зарезервирован
4       4     u32    gen            Поколение container-атома в родительском графе
8       8     u64    node_capacity  Ёмкость node pool подграфа
16      8     u64    edge_capacity  Ёмкость edge pool подграфа
24      4     u32    node_count     Число живых вершин
28      4     u32    edge_count     Число живых рёбер
Total = 32 байт
```

### 14.2. SubgraphDirPage layout

```
Offset  Size   Содержимое
0       32     PageHeader (magic=MAGIC_SUBGR_DIR, type=SubgraphDir)
32      8      next_page (u64, NULL_PAGE = последняя)
40      4      entry_count (u32)
44      4      live_count        -- число живых (не удалённых) подграфов
48      16      _pad
64      16320  510 × SubgraphDirEntry (510 × 32 = 16320 байт)
Total = PAGE_SIZE байт
```

`SubgraphDirEntry` не является authority для filesystem path. Durable binding хранится в
`DatabaseCatalog`: дочерняя запись содержит собственный `GraphId` и обязательные
`parent_graph_id`, `subgraph_slot`, `parent_atom`. При открытии обе стороны сверяются, включая
kind/generation container-атома. Несовпадение, duplicate slot или ссылка на отсутствующий child
считаются corruption. Вложенный и transition graph имеют полный обычный набор Direct-I/O файлов,
собственный primary index и graph superblock, но скрыты от публичного named-graph каталога.

---

## 15. FreelistPage — управление свободными страницами

Format-v1 создаёт отдельный `FreelistPage` для двух изменяемых файлов AtomStore —
`atoms_hot.fgb` и `properties.fgb` — и для шести graph-local файлов — `nodes_hot.fgb`,
`edges_hot.fgb`, `adjacency_overflow.fgb`, `adj_over_cl.fgb`, `overlays.fgb` и
`subgraphs.fgb`. Их companions называются соответственно `freelist_atoms.fgb`,
`freelist_props.fgb`, `freelist_nodes.fgb`, `freelist_edges.fgb`, `freelist_adj.fgb`,
`freelist_adj_cl.fgb`, `freelist_overlays.fgb` и `freelist_subgraphs.fgb`; связь хранится явно
в `FGFC v5.companion_of`.
Superblock, labels, statistics и immutable index side-files freelist не имеют. Пока
incremental page allocation не реализован, каждый из восьми freelist-файлов содержит одну
валидную пустую страницу и не читается обычным index/page hot path.

### 15.1. Layout

```
Offset  Size    Тип    Поле                 Описание
0       32      —      PageHeader (magic=MAGIC_FREELIST, type=Freelist)  
32      8       u64    first_page_covered   Первая страница, покрытая этим bitmap
40      8       u64    next_freelist_page   Следующая страница freelist; NULL_PAGE = одна
48      16336   bits   Bitmap               1 бит = 1 страница; 1=свободна, 0=занята
  → 16336 × 8 = 130688 страницы покрыто одной FreelistPage
  → 130688 × PAGE_SIZE = 2 ГиБ покрытия на один freelist
Total = PAGE_SIZE байт
```

### 15.2. Алгоритм поиска свободной страницы

1. Загрузить FreelistPage в BufferPool (будет hot, если часто аллоцируем).
2. BSF (bit-scan forward) по bitmap → первый bit=1.
3. Если не найден — расширить файл (grow), добавить страницы, обновить bitmap.
4. Установить bit=0 (занять), записать WAL
   `PageInit { space_id: DataSpaceId, file_id: DataFileId, page_type, page_no }`; первая содержательная запись
   этой страницы обязана использовать полный `PageFrame` (§22.2).

---

## 16. Словари property keys и graph labels

Строковые пространства разделены. Database-wide `PropertyDictionary` интернирует только ключи
свойств. `LabelDictionary` каждого графа интернирует только graph-local node/edge labels и имеет
две независимые плотные последовательности ID. Имена classes принадлежат `ClassCatalog`, а IANA
timezone names — `FGDC v7`.

### 16.1. Database-wide `PropertyDictionary`

`atoms/<generation>/property_keys.fgb` имеет `DataFileKind::PropertyDictionary = 0x14`, является
append-only, WAL-versioned и не имеет freelist. `PropertyKeyId` плотны, стабильны и монотонны в
диапазоне `[0, entry_count)`; удалённый ключ не переиспользуется. `FileCatalog` — единственная
authority физического binding, поэтому `DataFileId` словаря не дублируется в `FGDC v7`.

```text
PropertyDictionaryRoot (PageType 0x003C):
0       32    PageHeader
32      4     entry_count
36      4     entry_page_count
40      8     entry_first_page
48      8     entry_last_page
56      8     hash_first_page
64      4     hash_page_count
68      4     hash_capacity
72      ...   reserved = 0

PropertyDictionaryEntry (PageType 0x003D):
0       32    PageHeader
32      8     next_page
40      4     first_id
44      2     count
46      2     used_bytes
48      ...   [name_len:u16 LE, utf8_name] × count
tail          zero padding
```

Entry pages образуют строгую возрастающую ID-chain. Name lookup использует `HashIndexPage` с
`hash = low_u32(RapidHashV3(name_bytes, seed=0))` и обязательным сравнением исходных UTF-8 bytes
после hash match. Resize сначала дописывает новый hash range, затем одной WAL-транзакцией переключает root;
старые ranges становятся недостижимыми. Decoder проверяет границы, `first_id`, exact count,
UTF-8, `used_bytes`, нулевой tail и все page references относительно
`AtomStoreSuperblock.property_dictionary_pages`.

### 16.2. Структура graph-local `labels.fgb`

```
Page 0:      LabelDictHeader
Page 1..N:   DictEntryPage    (NodeLabel entries)
Page N+1..:  DictEntryPage    (EdgeLabel entries)
...          HashIndexPage    (string → id lookup)
```

### 16.3. LabelDictHeader (Page 0)

```
Offset  Size  Тип    Поле                                 Описание
0       32    —      PageHeader (magic=MAGIC_LABEL_DICT, type=LabelDict)
32      4     u32    node_label_count                     Число зарегистрированных node-меток
36      4     u32    edge_label_count                     Число зарегистрированных edge-меток
40      8     u64    node_entries_first_page
48      8     u64    node_entries_last_page
56      8     u64    edge_entries_first_page
64      8     u64    edge_entries_last_page
72      8     u64    hash_first_page
80      4     u32    hash_page_count
84      4     u32    hash_capacity
88      ...   —      reserved = 0
```

### 16.4. LabelDictEntryPage

```
Offset  Size   Содержимое
0       32     PageHeader (magic=MAGIC_LABEL_DICT, type=LabelDictEntry)
32      8      next_page (u64)
40      4      first_id (u32)
44      2      count (u16)
46      2      used_bytes (u16)
48      1      kind: 0=NodeLabel, 1=EdgeLabel
49      7      reserved = 0
56      var    Entries: [name_len:u16 LE, utf8_bytes[name_len]] × count
tail           zero padding
```

Node и Edge chains не пересекаются. Внутри каждой chain `first_id` начинается с нуля и
непрерывно продолжается; одинаковая строка в двух namespaces может иметь разные ID.

### 16.5. Hash Index (string → id)

Для быстрого lookup по строке → id (нужен при INSERT/DEFINE).

Open-addressed hash table, одна или несколько страниц:

```
HashIndexPage:
  PageHeader(32) (magic=MAGIC_IDX_HASH, type=HashIndex)
  next_page(8)
  used(u32) + capacity(u32)
  entries: [hash(u32) + id(u32) + name_page(u64) + name_offset(u32)] × (capacity)
  → entry size = 20 байт
  → (PAGE_SIZE - 32 - 8 - 8) / 20 = 816 entries per page max
```

- `name_page=0` обозначает пустой bucket; entry pages всегда начинаются с page 1.
- Несколько hash pages образуют один логически непрерывный диапазон buckets; `hash_capacity`
  root равен сумме `capacity` всех страниц, а линейное пробирование может переходить между
  страницами с циклическим возвратом в начало диапазона.
- Load factor всего диапазона не превышает 0.75. При resize новый диапазон полностью
  дописывается, а старый остаётся недостижимым.

Канонический ключ graph-label hash имеет вид
`{kind:u8, name_len:u16 LE, utf8_name}`. Коллизия всегда разрешается сравнением полного ключа.
Неизвестный `kind`, cross-kind link, нарушение `first_id`, invalid UTF-8, неверный `used_bytes`,
ненулевые reserved/tail bytes или выход root/chain за `GraphSuperblock.labels_pages` являются
corruption.

---

## 17. AtomStoreSuperblock и GraphSuperblock

Target layout имеет два логически WAL-versioned суперблока. Их полные checksummed образы входят
в тот же `TxCommit`, что и изменённые data/catalog pages. Единственная home-page не мутируется
in-place: checkpoint публикует её metadata COW через `.tmp/main/.prev`, а последняя замена
`db.fgb` остаётся общей checkpoint authority.

### 17.1. Общий префикс

```
Offset  Size  Тип    Поле
0       32    —      PageHeader
32      8     u64    data_space_id
40      4     u32    storage_format_version = 1
44      4     u32    last_back_compat_version
48      8     u64    checkpoint_generation
56      8     u64    backfilled_lsn
64      8     u64    checkpoint_record_lsn
```

`data_space_id=0` допустим только для AtomStore; graph superblock содержит ненулевой `GraphId`.
`PageHeader.page_lsn` — LSN последнего логического `PageFrame`, а не `CheckpointEnd`. Checkpoint
меняет только три checkpoint-поля и сохраняет этот page LSN. Только `DatabaseSuperblock`
продолжает иметь `PageHeader.page_lsn == checkpoint_record_lsn`.

### 17.2. AtomStoreSuperblock (`PageType=0x0038`)

```text
72      8     atom_capacity
80      8     node_count
88      8     edge_count
96      8     atoms_hot_pages
104     8     properties_pages
112     8     property_dictionary_pages
120     ...   reserved = 0
```

`node_count + edge_count <= atom_capacity <= NULL_ATOM_SLOT`;
`atoms_hot_pages == ceil(atom_capacity / 340)`. Все live primary/property/dictionary references
лежат ниже соответствующих ceilings; `property_dictionary_pages >= 1`; global primary содержит
ровно `node_count + edge_count` live mappings. `atoms_hot.fgb` и `properties.fgb` имеют отдельные
freelist companions, append-only `property_keys.fgb` freelist не имеет.

### 17.3. GraphSuperblock (`PageType=0x0011`)

```text
72      8     node_capacity
80      8     edge_capacity
88      8     node_count
96      8     edge_count
104     8     subgraph_capacity
112     8     subgraph_count
120     8     node_hot_pages
128     8     edge_hot_pages
136     8     adjacency_overflow_pages
144     8     cross_level_adjacency_overflow_pages
152     8     overlays_pages
160     8     subgraphs_pages
168     8     labels_pages
176     ...   reserved = 0
```

Counts не превышают capacities; `node_hot_pages == ceil(node_capacity / 102)` и
`edge_hot_pages == ceil(edge_capacity / 102)`. Все overflow/overlay references лежат ниже
ceilings; пустые overflow/overlay files могут иметь ceiling 0; `subgraphs_pages` и
`labels_pages` не меньше 1. Ровно шесть freelists обслуживают nodes, edges, adjacency,
cross-level adjacency, overlays и subgraphs.

Graph name, timestamps/properties, schema/statistics roots, storage flags, file IDs, index roots
и freelist lengths не дублируются в superblock. Они принадлежат catalog definitions, physical
bindings и собственным файловым root pages.

### 17.4. Логическая публикация и metadata COW

1. После FCW/SSI под database commit lock строится итоговый superblock.
2. Если counts/capacities/ceilings не изменились, frame отсутствует; иначе в общую транзакцию
   добавляется ровно один полный frame для затронутого data space.
3. `TxCommit` и один WAL `flush + sync_data` публикуют data, catalog и superblock вместе.
4. Home `superblock.fgb` при commit не записывается.
5. Checkpoint исключает superblocks из обычного in-place backfill, записывает `CheckpointEnd`,
   затем materializes последнюю логическую версию не новее target в `.tmp`.
6. После sync `.tmp` заменяет main с сохранением прежнего main как `.prev`; `db.fgb` заменяется
   последним. `.tmp` никогда не повышается до authority при recovery.
7. Recovery выбирает наиболее новый checksummed `main/prev`, чья checkpoint tuple не опережает
   `db.fgb`, затем применяет более новые committed WAL frames.
8. Нет подходящего candidate и полного WAL frame — typed corruption. WAL не обрезается ниже
   oldest visible snapshot и до успешной публикации `db.fgb`.

Superblock не является пользовательским conflict key: это производный образ итогового merged
state. Commit и checkpoint сериализуются текущей database-level critical section. Direct I/O не
гарантирует атомарность 16 KiB; поэтому единственную root-page нельзя мутировать in-place:
checksum обнаруживает torn write, но без `.prev` не восстанавливает предыдущую карту достижимых
файлов и согласованные ceilings.

### 17.5. GraphOverlayPage

Обычная overlay page использует проверенный slotted-page принцип `PropHeapPage`, но отдельные
magic и `PageType::GraphOverlay`:

```text
0       32    PageHeader(GraphOverlay, flags=0)
32      2     slot_count
34      2     free_start
36      2     free_end
38      2     reserved = 0
40      ...   SlotEntry { offset:u16, len:u16 }[]
tail          records, упакованные с конца страницы
```

Canonical record:

```text
entry_count: uLEB128, обязательно > 0
для PropertyKeyId по строгому возрастанию:
    key_id: u32 LE
    state: u8        -- 0 Hidden, 1 Value
    value: StorageValue, только при state=1
```

Отсутствующий key наследует base value; `Hidden` скрывает его; `Value(null)` отличается от
`Hidden`. Пустой overlay не хранится. Large overlay использует тот же page type с
`FLAG_OVERFLOW`: `next_page:u64` по offset 32, `data_len:u32` по 40, reserved `u32=0` по 44 и
canonical bytes с offset 48. Sentinel ссылки равен `0xFFFF_FFFE`; вся цепь проверяется до
публикации.

Поэтому point delta не читает, не копирует и не синхронизирует остальные файлы поколения:
write amplification зависит от числа изменённых страниц, полного образа перестраиваемого индекса
и одной metadata page, а не от размера графа. Полное COW-поколение остаётся протоколом создания
графа, явной полной materialization и несовместимого изменения набора физических файлов. Физические
data-файлы могут быть длиннее логически достижимой версии после замены полного образа индекса;
reader следует опубликованным page counts/roots и никогда не считает лишний tail видимыми данными.

Обычная локальная мутация атомов не имеет скрытого перехода к полной materialization. Создание,
изменение и удаление узлов/рёбер, labels, properties и incidence формируют page delta: hot-образы
заменяются, а продолжения properties и adjacency дописываются с учётом WAL-visible длины файла.
Новый snapshot видит эти изменения только после durable `TxCommit`, а уже открытый snapshot
продолжает читать прежние версии. Пока scalar indexes не поддерживают atom-delta maintenance,
bounded mutation такого графа завершается явной ошибкой до durable I/O; вызывающий код может
отдельно запросить полную materialization, но storage никогда не выбирает её неявно.

---

## 18. ClassCatalog и остальные schema definitions

`SchemaCatalog` владеет schema definitions, но не data membership. `ClassCatalog` задаёт
namespace `RecordId`, immutable `NODE|EDGE` kind, fields и base/effective index definitions.
Принадлежность class выводится из `RecordId.class`; graph membership хранится отдельно в graph
state. Target format v1 хранит root graphs, classes, fields, scoped Standard/Unique/Count indexes,
глобальные параметры и engine-owned primary/class/label/adjacency indexes.
События, функции, анализаторы, changefeed/live definitions и расширенные index kinds ниже
остаются зарезервированным расширением соответствующих этапов и не сериализуются writer'ом
format v1 до реализации их полного контракта.

### 18.1. Общая структура

Каждый реализованный schema object имеет:

- уникальную identity, монотонно выделяемую общим clock в пределах БД и не
  переиспользуемый после удаления;
- ненулевой `version: u32`; семантически изменённое определение сохраняет identity и
  увеличивает version, а новое после удаления получает новый identity;
- owning class или graph identity там, где объект вложен в другое определение;
- типизированное определение, которое кодируется явно, а не произвольным Rust derive.

Identity `0` и `u32::MAX` зарезервированы; допустимые `ClassId`, `SchemaId` и `IndexId` лежат в
`1..u32::MAX`. `ClassId` и `IndexId` — отдельные публичные newtypes для
индексного контекста, но отдельного clock или второго durable integer не вводит.
`next_schema_id: u64` хранит первую ещё не выданную identity
и входит в атомарно публикуемый `DatabaseCatalog`.

### 18.2. Физическая публикация реализованного ядра

Schema definitions входят в `DatabaseCatalog` payload `FGDC` version 7 (§7.2). Перед первым
`StorageValue` payload кодируется length-framed IANA timezone table; UTC id 0 и fixed offsets
остаются implicit. Inline property-key strings удалены: definitions сохраняют только
`PropertyKeyId` и разрешают их через §16.1.

`DatabaseCatalog` и `FileCatalog` используют один bounded page-chain codec:

```text
0       32    PageHeader(SystemCatalog)
32      4     marker = "FGC1"
36      1     catalog_kind: 1=Database, 2=Files
37      1     page_codec_version = 1
38      2     reserved = 0
40      8     next_page; NULL_PAGE = конец
48      4     chunk_len
52      4     total_len; ненулевой только на первой странице
56      8     reserved = 0
64      ...   payload chunk
tail          zero padding
```

Checksum проверяется до payload; chain не зацикливается, page numbers не повторяются, chunks
потребляются точно, а `total_len` и общая allocation ограничены 16 МиБ. `DatabaseCatalog` v7
принимает только v7; старые v5/v6 не имеют compatibility decoder.

| Объект                | Обязательное durable определение                                                                    |
|-----------------------|-----------------------------------------------------------------------------------------------------|
| Root graph            | `GraphId`/version, имя, graph family/storage mode и engine-owned graph index metadata               |
| Class                 | `ClassId`/version, immutable имя, `NODE                                                             |EDGE`, schema mode и поля                                                  |
| Field                 | identity/version, owning `ClassId`, имя, `PropertyKeyId` и `Type`                                   |
| Standard/Unique/Count | `IndexId`/version, owning `ClassId`, explicit scope, kind, state, ordered fields и physical binding |
| Parameter             | identity/version, имя и canonical `StorageValue` bytes со словарями                                 |
| Graph membership      | `GraphId + GlobalAtomId → graph-local DiskAtomRef`; data relation, не schema definition             |

RAM и local backends используют одну identity model. Один global atom принадлежит ровно одному
class по `RecordId`, но может иметь любое число graph memberships. Удаление class требует
отсутствия atoms и schema/index/reference dependencies; удаление graph не удаляет global atoms.

Schema change проходит FCW validation относительно snapshot, а определения, изменённые graph
pages/index generations и оба catalog root публикуются одним database-level WAL commit.
Primary/label/adjacency имеют каталожную identity даже когда физически используют общий или
inline layout; пользователь не может удалить их отдельно.

Подразделы 18.3–18.7 задают логические определения для последующего расширения DDL. Их
псевдо-layout не является byte-offset layout payload `FGDC` v7; writer format v1 кодирует
только реализованные поля из таблицы выше и обязан отвергать ещё не поддержанные варианты.

### 18.3. ClassDefinition (def format)

```
Offset  Size  Тип    Поле
0       1     u8     atom_kind                  -- 0=Node, 1=Edge
1       1     u8     schema_mode                -- 0=SCHEMALESS, 1=SCHEMAFULL
2       2     u8[2]  reserved = 0
4       8     u64    changefeed_duration_ns     -- 0 = нет changefeed
```

### 18.4. FieldDefinition

```
field_name_id: u32
type_tag:      u8     -- TypeTag (§3.1)
type_detail:   [u8]   -- зависит от типа (напр. для array<T>: element TypeTag)
flags:         u8     -- bit0=READONLY, bit1=VALUE, bit2=COMPUTED, bit3=DEFAULT
default_len:   u32
default_bytes: [u8]   -- закодированное значение по умолчанию (§5)
assert_len:    u32
assert_bytes:  [u8]   -- выражение ASSERT (сериализованный IR)
expr_len:      u32
expr_bytes:    [u8]   -- выражение VALUE / COMPUTED (сериализованный IR)
```

**Примечание о COMPUTED**:
COMPUTED-поле не хранится в PropHeapPage. При SELECT executor вычисляет его значение из expr_bytes, применяя к остальным полям записи.
Признак COMPUTED определяется исключительно флагом в FieldDefinition.flags (bit2=COMPUTED) — per-record хранение флага не требуется.

### 18.5. IndexDefinition

```
index_name_id: u32
class_id:      u32
index_kind:    u8  -- 0=Standard, 1=Unique, 2=Count, 3=Fulltext,
                   -- 4=HNSW, 5=DiskANN, 6=Geometry,
                   -- 7=Reachability, 8=Neighbourhood, 9=Path
field_count:   u8
field_ids:     [u32] × field_count
flags:         u8  -- bit0=CONCURRENT, bit1=DEFERRED
status:        u8  -- 0=building_initial, 1=building_update, 2=ready, 3=error
scope_kind:    u8  -- 0=Base, 1=Effective
scope_graph:   u64 -- 0 для Base; ненулевой GraphId для Effective
[fulltext_params]  -- если kind=3: analyzer_id(u32) + bm25_k1(f32) + bm25_b(f32) + highlights(u8)
[vector_params]    -- если kind=4/5: dimensions(u32) + elem_type(u8: F64/F32/I64/I32/I16)
                   -- dist(u8) + algorithm params + hashed_vector(u8; только DiskANN)
[geo_params]       -- если kind=6: geometry_field_type(u8)
```

Для пользовательских Standard/Unique/Count `scope_kind` обязателен. Base index читает только
global class atoms; Effective index является полным index по memberships одного `scope_graph` и
их `overlay Value|Hidden|absent` view. Implicit scope и частичный overlay-only index запрещены.

Vector definition хранит separately-versioned side-format binding и exact distance identity.
`HASHED_VECTOR` допустим только для DiskANN и входит в `SchemaVersion`; unknown vector flags
отклоняются, а не интерпретируются как Geometry или Standard index.

### 18.6. EventDefinition

```
event_name_id:  u32
async_flag:     u8   -- 0=sync, 1=async
retry_count:    u8
max_depth:      u8
when_expr_len:  u32
when_expr:      [u8] -- сериализованный IR выражения WHEN
then_expr_len:  u32
then_expr:      [u8] -- сериализованный IR THEN-тела
```

### 18.7. FunctionDefinition

```
fn_name_id:   u32
purity:       u8  -- 0=IMPURE, 1=PURE, 2=STABLE
is_cost_fn:   u8
arg_count:    u8
return_tag:   u8  -- TypeTag возвращаемого типа
args:         [name_id(u32) + type_tag(u8) + type_detail([u8])] × arg_count
body_len:     u32
body:         [u8]  -- сериализованный IR тела функции
```

### 18.8. AnalyzerDefinition

```
analyzer_name_id: u32
tokenizer_flags:  u8   -- bitfield: blank|camel|class|punct
filter_count:     u8
filters:          [filter_kind(u8) + params(variable)]
custom_fn_id:     u32  -- NULL_LABEL = нет пользовательской функции
```

Формат `filter_kind`:

```
0x01 ascii
0x02 lowercase
0x03 uppercase
0x04 edgengram + min(u8) + max(u8)
0x05 ngram    + min(u8) + max(u8)
0x06 snowball + language_id(u8)  -- 0=en, 1=ru, 2=de, ...
0x07 mapper   + file_path_len(u16) + utf8_path
```

---

## 19. StatisticsStore — статистика для планировщика

Планировщик (`gql_spec.md §21.4`) использует cardinality, степени и распределения только для
выбора плана. Ошибочная или устаревшая статистика может ухудшить план, но не меняет результаты:
residual predicate остаётся обязательным, а статистика не является источником истины для данных.

`StatisticsStore` принадлежит одному MVCC-root и хранит статистику по точной ревизии графа.
Durable-представление одного графа разделено по частоте и назначению чтения на три checksummed
файла:

- `stats/graph_stats.fgb`: page 0 содержит `StatisticsRoot`, а начиная с page 1 без разрывов идут
  `DegreeHistogram`;
- `stats/label_stats.fgb`: zero-based цепочка `LabelStatistic`;
- `stats/prop_stats.fgb`: zero-based последовательность `PropertyHistogram`.

Root хранит начало и длину каждого диапазона. `degree_first_page` равен 1 для непустого диапазона,
а `label_first_page` и `property_first_page` равны 0 в своих файлах; для пустого диапазона start
равен `NULL_PAGE`, а count — нулю. Объявленные длины обязаны точно совпадать с физическими длинами
трёх файлов: `graph_stats.fgb` содержит `1 + degree_page_count` страниц, два остальных — ровно
соответствующий count. Недостижимые, переставленные и лишние страницы являются повреждением.
Physical bindings статистических файлов находятся только в catalog; `GraphSuperblock` не
дублирует их roots.

При открытии графа eager читается и проверяется только `StatisticsRoot`; degree, label и property
части загружаются независимо при первом запросе через точный storage snapshot вызывающей
транзакции. Успешно декодированная immutable-часть кешируется для этой durable revision.
Повреждение auxiliary-файла поэтому не блокирует открытие и обычное чтение данных, но возвращается
при первом обращении к соответствующей статистике. Планировщик обязан трактовать такую ошибку как
недоступную статистику и использовать корректный консервативный план; point-DML, которому нужны
точные label counters, отклоняет commit, если прочитать их безопасно невозможно.

### 19.1. GraphStatsPage

```
Offset  Size  Тип    Поле
0       32    —      PageHeader (magic=MAGIC_STATS, type=StatisticsRoot, page_no=0)
32      8     u64    node_count
40      8     u64    edge_count
48      8     f64    avg_out_degree       -- производное, сверяется с exact total
56      8     f64    avg_in_degree        -- производное, сверяется с exact total
64      8     u64    max_out_degree
72      8     u64    max_in_degree
80      8     u64    degree_first_page    -- page 1 graph_stats.fgb или NULL_PAGE
88      8     u64    label_first_page     -- page 0 label_stats.fgb или NULL_PAGE
96      8     u64    property_first_page  -- page 0 prop_stats.fgb или NULL_PAGE
104     4     u32    label_page_count
108     4     u32    degree_page_count
112     4     u32    property_page_count
116     4     u32    statistics_layout_version = 1
120     8     u64    analyzed_revision
128     8     u64    dirty_rows
136     8     u64    total_out_degree
144     8     u64    total_in_degree
152     8     u64    total_undirected_degree
160     8     u64    revision
168     ...   —      _reserved = 0
```

`node_count`, `edge_count` и три degree totals точны для `revision`. Средние выводятся из
целочисленных totals; записанные `f64` являются проверяемой избыточностью, а не authority.
`max_*` точны после bulk-пересчёта и могут быть только консервативной верхней границей после
point-DML, уменьшающего степень. `analyzed_revision == revision && dirty_rows == 0` означает, что
bulk-распределения соответствуют текущей ревизии.

Точные per-class base counts хранятся в `atoms/stats/class_stats.fgb`; graph-local class counts
строятся по occurrences и относятся к `graph_stats.fgb`. Ни один count не выводится из schema
membership: class задаёт `RecordId`, graph membership — отдельный data relation.

### 19.2. LabelStatsPage

```text
0       32    PageHeader(magic=MAGIC_STATS, type=LabelStatistic)
32      8     next_page
40      4     count
44      4     reserved = 0
48      ...   entries[24 bytes]

LabelStatisticEntry:
0       1     kind: 0=NodeLabel, 1=EdgeLabel
1       3     reserved = 0
4       4     label_id
8       8     atom_count
16      8     property_count
```

Ключ записи — точная пара `{kind:u8, label_id:u32}`. Арифметический offset edge labels и
объединённый числовой namespace запрещены: добавление node label не перенумеровывает edge
statistics. Записи строго отсортированы по `(kind,label_id)`, неизвестный kind и duplicate key
являются corruption. В payload помещается не более 680 записей. `property_count` — точная сумма
числа свойств атомов метки; point-DML сохраняет нулевой `atom_count`, а bulk ANALYZE может удалить
нулевую запись.

### 19.3. PropertyHistogramPage

Гистограмма для одного `(class_id, field_id, scope)`:

```
Offset                  Size                Тип     Содержимое
0                       32                  -       PageHeader (magic=MAGIC_STATS, type=PropertyHistogram)
32                      4                   u32     class_id
36                      4                   u32     field_id
40                      1                   u8      scope_kind      -- 0=Base, 1=Effective
41                      1                   u8      value_type
42                      1                   u8      method          -- 0=exact, 1=HLL+reservoir
43                      1                   u8      hll_precision   -- 0 или 12
44                      4                   u32     sample_count
48                      8                   u64     scope_graph     -- 0 для Base; GraphId для Effective
56                      8                   u64     distinct_count  -- NDV
64                      4                   f32     null_fraction
68                      2                   u16     bucket_count
70                      2                   u16     reserved = 0
72                      8 × bucket_count    [u64]   offsets
72 + 8 × bucket_count   -                   -       buckets: [lo_len(u16) + hi_len(u16) + lo_value + hi_value + freq(u64) + ndv(u64)] × bucket_count
```

Bucket значения кодируются compact order-preserving encoding (§20.2.1). Смещения строго
возрастают, первый bucket начинается сразу после массива offsets, а последний заканчивается до
границы страницы; неиспользованный хвост заполнен нулями. `distinct_count`, bucket `freq` и
bucket `ndv` точны для последнего bulk-пересчёта. `null_fraction` учитывает отсутствующие,
`NONE` и `NULL` значения среди atoms соответствующего class/scope. Effective scope вычисляет
overlay `Value|Hidden|absent` до добавления наблюдения.

HLL registers служат только рабочей памятью builder и в histogram page не сохраняются.
`INFO`/`EXPLAIN` показывают `method`, `sample_count`, analyzed revision и staleness.

Один histogram занимает одну страницу. Неподдерживаемый тип, значение, которое нельзя безопасно
закодировать индексным codec, либо распределение, не помещающееся в страницу, приводит к
отсутствию только этой необязательной гистограммы. Пользовательская транзакция из-за этого не
отклоняется.

### 19.4. DegreeHistogramPage

```
PageHeader(32) (magic=MAGIC_STATS, type=DegreeHistogram)
kind:      u8   -- offset 32; 0=NodeLabel, Edge запрещён format-v1
direction: u8   -- offset 33; 0=out, 1=in, 2=undir
bucket_count: u16 -- offset 34
label_id:  u32  -- offset 36; u32::MAX = весь graph
buckets:   [min_degree(u32) + max_degree(u32) + node_count(u64)] × bucket_count
```

Per-label degree histogram допустим только для node label. В распределение входят все живые
вершины соответствующего множества, включая вершины с нулевой степенью.

### 19.5. Метод построения гистограмм и NDV

**Гистограммы — equi-depth (равнонаполненные).** Границы корзин выбираются так, чтобы в каждую попадало примерно одинаковое число строк, а не чтобы корзины были равной ширины по значению. Equi-width деградирует на перекошенных распределениях: одна корзина собирает почти всё, и оценка селективности становится бессмысленной — а перекос в графовых данных норма (степени вершин, частоты меток).

Число корзин ограничено 100. Для `N > 0` создаётся ровно `min(100, N)` непустых корзин;
размеры корзин отличаются не более чем на одну запись. Для каждой корзины хранятся границы,
частота и число различных значений внутри неё (`ndv`). Текущий bulk-путь выполняет полный scan,
сортирует значения и считает общий и bucket NDV точно.

`ANALYZE ... FULL` использует полный source scan и точный NDV. `APPROXIMATE` сохраняет точные
row/null/property counters, но использует HLL `p=12` (4096 однобайтовых регистров, стандартная
ошибка около 1,6%) и детерминированный reservoir sample на 30 000 values. `SAMPLE n` меняет
размер reservoir в диапазоне `1024..=1_000_000`, но не обещает уменьшить I/O. По умолчанию
выбирается FULL для не более 32 видимых data pages и `APPROXIMATE SAMPLE 30000` иначе; выбор и
параметры видны в `EXPLAIN/INFO`.

### 19.6. Обновление статистики

- **Point-DML:** транзакционная дельта один раз при commit обновляет точные graph/class/label
  counts, label `property_count` и degree totals. Membership-only delta также увеличивает
  `dirty_rows`, поскольку меняет выборку property histogram. Page 0 `graph_stats.fgb` и полный
  `label_stats.fgb` публикуются в той же frame-WAL транзакции, что и данные. Degree pages в
  `graph_stats.fgb` и `prop_stats.fgb` не переписываются; `revision`, `dirty_rows` и
  `analyzed_revision` явно отмечают их устаревшими.
- **Bulk-путь:** создание полного graph image и schema-affected materialized rebuild выполняют
  один полный scan и атомарно публикуют все три файла с точными counts и распределениями. Если
  новый файл короче существующего in-place файла, writer выбирает новое COW-поколение вместо
  оставления недостижимого хвоста. Этот же анализатор используется RAM и local backend.
- **Планировщик:** при stale или отсутствующей гистограмме использует консервативную оценку либо
  scan, но всегда сохраняет residual predicate. Корректность данных не зависит от статистики.
- **Ошибки:** overflow точного счётчика и повреждение `StatisticsRoot` отклоняют commit/open;
  повреждение lazy auxiliary-части возвращается при её первом чтении. Невозможность построить
  отдельную optional property histogram не отклоняет пользовательскую транзакцию.

`ANALYZE (CLASS @class (BASE | EFFECTIVE IN GRAPH @graph) | GRAPH @graph)
[FULL | APPROXIMATE [SAMPLE @count]] [CONCURRENTLY]` всегда выполняет полный source scan для
одного snapshot. Foreground ждёт результат и при revision race возвращает conflict.
`CONCURRENTLY` немедленно возвращает наблюдаемый job ID; race отменяет publication и
перепланирует job. Auto-analyze не блокирует DML и запускается при
`dirty_rows >= max(1024, ceil(population × 0.10))`.

---

## 20. Индексы

### 20.1. Общий контракт Index

Format-v1 не вводит public trait с одной реализацией. Внутреннее ядро предоставляет bulk
builder, exact lookup и bounded range scan над standard/unique B+tree, immutable per-label
массивы и одно-страничный Count index. Leaf mode является частью page metadata и проверяется при
каждом чтении.

Core B+tree не получает отдельную дублирующую header page. Единственная physical authority —
`{DataFileId, root_page}`: scope определяет `DataSpaceId`, `FileCatalog` разрешает path, а
height/count выводятся при необходимости. Logical identity, kind, fields и `SchemaVersion`
принадлежат catalog definition.

Primary, Standard/Unique, Label и Count side-files сначала строятся в новом недостижимом UUID-поколении
графа и полностью синхронизируются. Затем один catalog/frame-WAL commit делает новые
`DataFileId` и roots достижимыми. Unique violation прерывает commit до публикации. Count меняется
в существующем side-file через page frame той же WAL-транзакции. Изменение label membership
помечает immutable Label generation как `stale`; Standard/Unique index при materialized update пока
перестраивается вместе с COW-поколением. Incremental scalar/primary page-WAL maintenance и
измеряемый bulk threshold принадлежат DML-этапу.

Отсутствующий, неоткрываемый или не прошедший checksum secondary index исключается из физического
registry, но его schema definition сохраняется: access path обязан перейти к корректному
primary/class scan или graph-occurrence scan с тем же scope. Недоступный primary index
остаётся фатальным повреждением. Fulltext/HNSW/
R-tree и остальные специализированные индексы ниже описывают последующие этапы и не маскируют
своё отсутствие.

### 20.2. B+tree Property Index (standard и unique)

`Standard` и `Unique` остаются разными `ClassIndexKind` и разными физическими контрактами:
Standard leaf хранит posting list, Unique leaf допускает ровно один atom и проверяет нарушение
уникальности до публикации. Общий внутренний модуль/codec B+tree может называться `scalar`, но
это не третий kind и не имя durable файла. Файлы называются `std_<ordinal:04>.fgidx` и
`uniq_<ordinal:04>.fgidx`; ordinal вычисляется по `IndexId` среди Standard/Unique текущего
поколения. Старые `scalar_*.fgidx` format-v1 не принимает.

#### 20.2.1. Key encoding (order-preserving)

── Скалярные типы ──────────────────────────────────────────────────────
```
outer     → 0x00 для none; 0x01 для null; 0x02 || payload для present value
bool      → 0x00 (false) / 0x01 (true)
int       → big-endian i64 с инвертированным MSB: v ^ 0x8000_0000_0000_0000
float     → IEEE 754 order-preserving (`-0.0` канонизируется в `+0.0`; NaN — ошибка валидации)
              if positive: flip MSB    → v ^ 0x8000_0000_0000_0000
              if negative: flip all    → v ^ 0xFFFF_FFFF_FFFF_FFFF
decimal   → см. §20.2.1.1 (наивная схема «sign + coefficient + exponent» НЕ сохраняет порядок)
string    → raw UTF-8 + 0x00 terminal sentinel
bytes     → raw + 0x00 sentinel
datetime  → ordered big-endian i64(seconds) + big-endian u32(nanos) [tz не индексируется]
duration  → нормализация из UnitSet-bitmask (формат хранения, §3.4) в 3 компонента:
             total_months = years×12 + months
             total_days   = weeks×7 + days
             total_nanos  = hours×3_600_000_000_000 + minutes×60_000_000_000
                          + seconds×1_000_000_000 + ms×1_000_000 + μs×1_000 + ns
             Encoding: (total_months ^ 0x8000_0000) big-endian 4 байт
                     + (total_days   ^ 0x8000_0000) big-endian 4 байт
                     + (total_nanos  ^ 0x8000_0000_0000_0000) big-endian 8 байт
             Семантика порядка: months доминируют над days, days над nanos.
             Примечание: "1 месяц 0 дней" > "0 месяцев 31 день" — это намеренное
             соглашение, не зависящее от длины конкретного месяца.
uuid/ulid → big-endian 16 байт (UUIDv7/ULID монотонны по времени в big-endian)
record_id → escaped canonical UTF-8 class name + terminator + tagged id_part encoding
```

Outer prefix обязателен для каждого поля, поэтому `none < null < present` совпадает с runtime
`Value::Ord`. Он также не позволяет `none`/`null` столкнуться с валидным payload. При попытке
вставить `NaN` значение в индекс происходит **ошибка валидации**.
В `PropHeapPage` `NaN` хранится как `edge weight sentinel`; в B+tree индексе он недопустим.

#### 20.2.1.1. Order-preserving ключ для `decimal`

Наивная схема «знак + big-endian мантисса + экспонента» **не сохраняет порядок** и потому непригодна. Два дефекта:

1. Численно равные значения с разным масштабом (`1.0` при `scale=1` и `1.00` при `scale=2`) дают **разные** ключи. Для unique-индекса это означает, что дубликат пройдёт проверку.
2. Сравнение сырых мантисс при разных экспонентах даёт неверный порядок: `5e-1` (мантисса 5) окажется больше `1e0` (мантисса 1), хотя `0.5 < 1.0`.

Format v1 использует одну схему независимо от schema declaration:

```
1. Нормализовать: убрать хвостовые нули → канонические (mantissa, scale).
   Ноль кодируется отдельным значением sign_byte и пустым остатком.
2. sign_byte:  0x00 — отрицательное, 0x01 — ноль, 0x02 — положительное.
3. adjusted_exponent = digits(|mantissa|) - scale - 1        (десятичный порядок числа)
   Записать как big-endian i16, смещённый в беззнаковый: (adj_exp ^ 0x8000).
4. Десятичные цифры |mantissa| без ведущих нулей, по одной на байт, дополненные
   до фиксированной длины (39 байт — максимум для i128) байтом 0x00.
5. Для отрицательных значений все байты ПОСЛЕ sign_byte инвертируются побитово.
```

Порядок при таком кодировании лексикографический: сначала различает знак, затем десятичный
порядок, затем значащие цифры слева направо. Нормализация на шаге 1 гарантирует, что численно
равные значения дают побайтово равные ключи. Альтернативного fixed-scale представления в format
v1 нет: два encodings одного логического decimal нарушили бы unique и reopen-контракт.

── Составные типы ──────────────────────────────────────────────────────
```
option<T> → отдельного ключа нет: None кодируется как none, Some(v) как encode(v)
array<T>  → escaped encode(elem[0]) + 0x00 0x01 + ... + 0x00 0x00
set<T>    → то же что array, элементы уже отсортированы (set = sorted unique)
object    → индексирование object целиком не поддерживается;
            для composite index используются отдельные поля
range<T>  → start_encoding || end_encoding:
            start: 0x00 unbounded; иначе 0x01 || escaped(value) || 0x00 || bound_kind
            end:   0x01 unbounded; иначе 0x00 || escaped(value) || 0x00 || bound_kind
            bound_kind: 0x00 included, 0x01 excluded
```

Такой range key воспроизводит runtime-порядок: start сравнивается раньше end, значение bound —
раньше included/excluded, unbounded start минимален, unbounded end максимален.

── Доменные типы ───────────────────────────────────────────────────────
```
record ref           → canonical class name + canonical RecordId part
vertex/edge/atom ref → canonical RecordId соответствующего AtomRef
```

Dictionary id и физический `DiskAtomRef` намеренно не входят в key: они зависят от порядка
интернирования/аллокации и не являются стабильным runtime-порядком. Для atom field kind уже
зафиксирован схемой `Type::Atom(kind)`; отсутствующая mapping в `EncodeContext` — ошибка.

── Неиндексируемые типы ────────────────────────────────────────────────
```
geometry  → не поддерживается в B+tree; используется R-tree index (§20.7)
vector    → не поддерживается в B+tree; используется HNSW index (§20.6)
any/number/object/regex/path/graph/union/record-shape/closure → не поддерживаются
```

`Array<T>`, `Set<T>`, `Range<T>` и `Option<T>` индексируемы только если `T` индексируем. Эта
кодировка гарантирует для одного schema type: `a < b ⟺ encode(a) < encode(b)`
лексикографически.

#### 20.2.2. BTree Internal Page

Ключи в Internal Page имеют переменную длину → binary search внутри страницы
требует offset-массива (иначе O(n) сканирование).

```
[0..32]                           PageHeader (magic=MAGIC_IDX_BTREE, type=IndexBtreeInternal; !FLAG_LEAF set)
[32..34]                          num_keys (u16)
[34..36]                          key_area_len (u16)  -- суммарная длина key_area в байтах
[36..36+2n]                       key_offsets: u16[num_keys]    
                                      -- смещения от начала key_area (= offset 36+2n) до начала ключа i
[36+2n..]                         key_area: [encoded_key(variable)] × num_keys  
                                      -- упакованные encoded_keys (self-delimiting по TypeTag); растёт вверх
[PAGE_SIZE − 8(n+1)..PAGE_SIZE]   child_pages: u64[num_keys + 1]
```

`child_pages` хранятся в конце страницы в прямом порядке:
- `child_pages[k]` = страница, куда идти если `target < key[k]`;
- `child_pages[num_keys]` = правый крайний потомок;
- смещение: `PAGE_SIZE − 8 × (num_keys + 1) + 8 × k`.
- инвариант (проверка `key_area_end < child_pages_start`): 
  `36 + 2 × num_keys + key_area_size + 8 × (num_keys + 1) ≤ PAGE_SIZE` 

Доступный для ключей объём: `PAGE_SIZE − 36 − 2n − 8(n+1)`.
Свободное место: `child_pages_start − key_area_end`.

`key_area_len` обязателен: без него протяжённость **последнего** ключа невыводима (`key_offsets` хранит только начала), а значит нельзя ни вычислить свободное место, ни корректно обойти область ключей.

```
Binary search (O(log num_keys)):
  сравнить key[mid] из key_area[key_offsets[mid]..] с искомым ключом
  сравнение — прямой memcmp, БЕЗ декодирования → сдвинуть границы
```

**Сравнение внутренних ключей — побайтовое (`memcmp`), а не через декодирование.** Кодирование ключей order-preserving по построению (§20.2.1), поэтому лексикографическое сравнение байтов семантически тождественно сравнению значений — но не требует диспетчеризации по типу и работает заметно быстрее.

Из этого следует важное для сборки индекса свойство: **разделитель можно усекать**. Между `последний_ключ(левый)` и `первый_ключ(правый)` в качестве разделителя допустим любой кратчайший префикс `P`, для которого `последний_ключ(левый) < P ≤ первый_ключ(правый)`. Для строковых и составных ключей это часто вдвое сокращает размер разделителя и добавляет целый уровень ветвления.

Вставка нового ключа `key[j]` с правым потомком `ptr`:
1. Проверить: `free_space ≥ key_len + 2 + 8`
2. Сдвинуть `child_pages` влево на 8 байт (в конце страницы)
3. Вставить `ptr` в `child_pages[j+1..]`, ключ в `key_area`, смещение в `key_offsets`
4. `num_keys += 1`

#### 20.2.3. BTree Leaf Page (Standard — non-unique)

```
[0..32]         PageHeader (magic=MAGIC_IDX_BTREE, type=IndexNonUniqueBtreeLeaf; FLAG_LEAF set)
[32..40]        next_leaf (u64)                   -- для range scan (prev_leaf не хранится)
[40..42]        num_entries (u16)
[42..44]        _pad
[44..44+2n]     entry_offsets: u16[num_entries]   -- смещения от начала entry_area (= offset 44+2n)
[44+2n..]       free/entry_area; записи плотно упакованы с конца страницы

LeafEntry (variable):
  encoded_key:     variable                     -- order-preserving (§20.2.1)
  atom_refs:       u64 × min(8, total_count)    -- первые ссылки inline
  key_len:         u16
  inline_count:    u16
  total_count:     u32
  posting_page:    u64                          -- NULL_PAGE при total_count ≤ 8
```

Свободное место: `PAGE_SIZE − 44 − 2n − Σ(entry_len_i)`.

```
Binary search: 
  entry_offsets[mid] + key_len → прямой memcmp, без decode.
```

#### 20.2.4. BTree Leaf Page (Unique)

Структура идентична Standard, но каждый LeafEntry:
```
PageHeader (magic=MAGIC_IDX_BTREE, type=IndexUniqueBtreeLeaf, FLAG_LEAF set)

encoded_key:  variable
atom_ref:     u64   -- единственная запись; нет posting_page
```

При нарушении уникальности → транзакция aborted с `ConstraintViolation`.

#### 20.2.5. PostingPage

Для non-unique index при count > 8:

```
Offset  Size   Содержимое
0       32     PageHeader (magic=MAGIC_IDX_BTREE, type=IndexPostingBtreeLeaf, FLAG_LEAF set)
32      8      next_page (u64)
40      4      count (u32)
44      4      _pad
48      16336  [DiskAtomRef(u64)] × count → (PAGE_SIZE - 48) / 8 = 2042 entries per page
```

#### 20.2.6. Composite Index

Composite index строится по нескольким полям одновременно.

**Зачем**: ускоряет запросы с фильтрацией или сортировкой по нескольким полям:
`SELECT * FROM person WHERE age > 18 AND city = 'Moscow'`
→ если есть `DEFINE INDEX ON person FIELDS age, city` → один B+tree lookup

**Определение**:
`DEFINE INDEX idx_age_city ON CLASS person FIELDS age, city SCOPE BASE`

Создаёт B+tree с составным ключом: сначала кодируется `age`, затем `city`.
Порядок полей в `DEFINE INDEX` определяет порядок сортировки в индексе.

**Свойство частичного поиска**: если `index = (A, B, C)`, то он эффективен для:
`WHERE A = x`                       → prefix scan ✓
`WHERE A = x AND B = y`             → prefix scan ✓
`WHERE A = x AND B = y AND C = z`   → точечный lookup ✓
`WHERE B = y`                       → full index scan (нет использования порядка A) ✗
`WHERE B = y AND C = z`             → full index scan ✗

Format-v1 tree поддерживает эти byte ranges. Текущий sync vertical slice выбирает single-field
index для equality/range и composite index только при equality по всем его полям. Остальные
prefix/composite формы намеренно деградируют к корректному class/graph-occurrence scan до появления cost-based
planner; полный predicate и SSI registration выполняются независимо от выбранного access path.

**Формирование ключа**:
- Ключ = конкатенация полных typed order-preserving encodings всех компонент.
- Порядок сортировки: по первому компоненту, при равенстве — по второму и т.д.
- Последний компонент записывается как есть. Каждый non-terminal компонент целиком экранируется:
  `0x00 → 0x00 0xFF`, затем добавляется `0x00 0x00`.
- B+tree никогда не декодирует component boundaries: lookup и range используют только `memcmp`.

**Кодирование строк в composite key**:

Ниже строка показывает общий механизм экранирования non-terminal компонента; те же правила
применяются к полному typed encoding любого другого переменного или фиксированного типа.

Правила:
- **Terminal-компонент** (последнее или единственное поле индекса): raw UTF-8 + 0x00 (стандартный sentinel, без экранирования).
  Безопасно: нет следующего компонента, которому 0x00 мог бы помешать.
- **Non-terminal компонент** (не последнее поле в composite index): escaped UTF-8 + 0x00 0x00 (двойной sentinel = конец строки).
  Экранирование: каждый байт 0x00 в строке заменяется на 0x00 0xFF. Терминатор: 0x00 0x00.

**Пример**:

Схема `DEFINE INDEX ON person FIELDS last_name, age`:
1. Запись: `{last_name: "O'Brien", age: 30}`: 
Ключ = `encode_nonterminal("O'Brien") + encode_terminal(30)` = `O ' B r i e n 0x00 0x00 [age_be_with_msb_flip]`.
2. Запись `{last_name: "O'\x00B", age: 30}` (строка со встроенным null): Ключ = `O ' 0x00 0xFF B 0x00 0x00 [age_be]`.
                                                                                     ─────────
                                                                                     escaped null

Доказательство ordering:
```
"O'B"       → O ' B 0x00 0x00
"O'\x00B"   → O ' 0x00 0xFF B 0x00 0x00
Pos 2: 'B' (0x42) vs 0x00 0xFF → 0x00 < 0x42 → "O'\x00B" < "O'B" ✓
(строка с null-символом в начале алфавита сортируется раньше — корректно)
```


**Пример**:
```

  Строка "ab\x00cd" в non-terminal позиции:
    0x61 0x62 0x00 0xFF 0x63 0x64 0x00 0x00
    ──── ──── ──────── ──── ──── ─────────
    'a'  'b'  escaped  'c'  'd'  terminator
                null
```

Доказательство order-preserving:
```
  "ab"     → 0x61 0x62 0x00 0x00
  "ab\x00" → 0x61 0x62 0x00 0xFF 0x00 0x00
  При сравнении на позиции 3: 0x00 < 0xFF → "ab" < "ab\x00" ✓
```

Реализация:
```rust
fn encode_string_nonterminal(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 2);
    for &b in s.as_bytes() {
        if b == 0x00 { out.extend_from_slice(&[0x00, 0xFF]); }
        else         { out.push(b); }
    }
    out.extend_from_slice(&[0x00, 0x00]);
    out
}

fn encode_string_terminal(s: &str) -> Vec<u8> {
    let mut out = s.as_bytes().to_vec();
    out.push(0x00);
    out
}
```

#### 20.2.7. Primary Index (`primary.fgidx`)

AtomStore содержит один обязательный unique B+tree
`canonical full RecordId → GlobalAtomId`. Имя class кодируется стабильным `ClassId`, а id-part
поддерживает Int, String, Uuid, Ulid, Array и Object. Graph address space не дублирует primary:
он использует `membership.fgidx` (`GlobalAtomId → graph-local DiskAtomRef`).

#### 20.2.8. Альтернативы B+tree и обоснование выбора

Рассмотренные альтернативы:

| Структура        | Write perf | Read perf | Disk-friendly | Зрелость | Вывод       |
|------------------|------------|-----------|---------------|----------|-------------|
| B+tree (выбрано) | Хорошо     | Отлично   | Да            | Высокая  | MVP         |
| Skip list        | Хорошо     | Хорошо    | Плохо         | Средняя  | Нет         |
| Bε-tree          | Отлично    | Хорошо    | Да            | Низкая   | Future      |
| ART              | Отлично    | Отлично   | Плохо         | Средняя  | In-mem only |
| LSM-tree         | Отлично    | Среднее   | Да            | Высокая  | Deferred    |
| Fractal tree     | Отлично    | Хорошо    | Да            | Низкая   | Нет         |

**B+tree**: страничный индекс с упорядоченными ключами. O(log n) для точечных
и диапазонных запросов. Отлично ложится на модель PageManager (каждый узел
дерева = одна страница). Зрелая теория splitting/merging.

**Bε-tree**: обобщение B+tree с буферами у внутренних узлов. Операции записи
накапливаются в буфере и применяются "каскадом" при его переполнении.
Снижает write amplification с O(log n) до O(log^{ε} n). Сложнее в реализации.
Подходит для workloads с интенсивными обновлениями property-индексов.

**Fractal tree** (TokuDB / PerconaFT): конкретная реализация Bε-tree с
"message injection" — изменения кодируются как сообщения, спускаемые по дереву.
Те же теоретические гарантии, что и у Bε-tree. Codebase PerconaFT открыт,
но разработка замедлилась после приобретения Percona.

**LSM-tree**: многоуровневое хранилище (L0 → L1 → ... → Lk). Отличный write
throughput, но read path требует слияния нескольких уровней. Стандарт для
write-heavy workloads (RocksDB, Cassandra, LevelDB). Для property-индексов
в graph DB добавляет read amplification без очевидного выигрыша — отложено.

**Skip list**: вероятностная in-memory структура. Хорошая конкуренция для
B+tree в памяти (используется в Redis), но страничная организация для диска
неестественна. Не рассматривается для disk-backed индексов.

**ART (Adaptive Radix Tree)**: исключительная cache-efficiency в памяти,
O(k) где k — длина ключа. Не предназначен для дисковых страниц. Перспективен
как in-memory буфер перед записью на диск (аналог MemTable в LSM).

**Вывод**: B+tree — оптимальный выбор для MVP:
- Предсказуемая производительность O(log n) для точечных и диапазонных запросов.
- Page-aligned структура идеально сочетается с PageManager.
- Нет write amplification проблем LSM (нет levels/compaction для property indexes).
- Хорошо изученная splitting/merging логика.

#### 20.2.9. Сжатие страниц индекса

Stage-7 core layout хранит полные encoded keys и требует только `FLAG_LEAF` для листьев;
`FLAG_COMPRESSED` у B+tree pages пока недопустим. Описанные ниже два механизма являются
кандидатами оптимизации до format-v1 freeze, а не альтернативной интерпретацией уже записанной
страницы. Их включение требует явного page-layout discriminator, golden vectors и сравнения
размера/CPU на representative string/composite indexes.

Два рассматриваемых механизма — структурное сжатие ключей и блочное сжатие страницы. Они
складываются и решают разные задачи.

**Префиксное сжатие ключей (предпочтительный кандидат).** Ключи в листе отсортированы, поэтому соседние обычно имеют длинный общий префикс — особенно строковые и составные (§20.2.6). В листе хранится общий префикс всех ключей страницы один раз, а каждая запись содержит только суффикс:

```
[..]        page_prefix_len (u16)
[..]        page_prefix (page_prefix_len байт)
[записи]    для каждого ключа: suffix_len (u16) + suffix_bytes
```

Это **не** LZ4 и вообще не универсальный компрессор: сжатие структурное и order-preserving, поэтому по сжатой странице по-прежнему работает бинарный поиск **без распаковки** — сравнивается только суффикс. Стоимость на чтении нулевая, выигрыш для строковых ключей часто двукратный.

Существенно: префиксное сжатие дёшево при bulk-сборке (§20.13), где лист формируется целиком и общий префикс известен сразу, и дорого при построчной вставке — вставка в середину сжатого листа может изменить общий префикс и потребовать перекодирования всей страницы. Если кандидат будет принят, естественная точка его применения — сборка и перестроение; stage-7 packer его не записывает.

**Блочное сжатие (вторичный кандидат).** Сжатие всей полезной нагрузки страницы целиком по аналогии с контрактом `PropHeapPage` (§13.3) через `FLAG_COMPRESSED`. Оно требует распаковки страницы перед любым обращением и не включено в текущий B+tree codec.

Если этот вариант будет принят, выбор алгоритма должен принадлежать конкретному class/index, а не быть глобальным: у разных данных разный профиль. Поле `block_compressor` в target `ClassDefinition` пока отсутствует.

| Значение             | Когда уместно                                                                             |
|----------------------|-------------------------------------------------------------------------------------------|
| `none`               | Данные уже плотные; приоритет — латентность                                               |
| `lz4`                | Универсальный компромисс; тот же кодек, что и в `PropHeapPage`                            |
| `zstd`               | Холодные данные и архивные графы; заметно плотнее LZ4, дороже по CPU                      |
| `snappy`             | Совместимость с внешними экосистемами                                                     |
| `fsst`               | Специализированно для коротких строк — сохраняет возможность работы без полной распаковки |

Кодеки, требующие полной распаковки страницы (`lz4`, `zstd`, `snappy`), не сочетаются с чтением произвольной записи «на месте», поэтому для горячих индексов рекомендуется ограничиваться префиксным сжатием.

### 20.3. Count Index

Хранит число живых atoms одного class в явном Base либо Effective scope.

```
CountIndexPage (единственная страница):
  PageHeader(32)  (magic=MAGIC_IDX_COUNT, type=IndexCountSingle)
  class_id:    u32
  scope_kind:  u8    -- 0=Base, 1=Effective
  _pad:        u8[3] -- нули
  scope_graph: u64   -- 0 для Base; ненулевой GraphId для Effective
  atom_count:  u64   -- число живых атомов
  last_updated_lsn: u64
  created_at_lsn: u64
  _pad:        16312
```

Обновляется атомарно при каждом INSERT/DELETE в той же транзакции.
Используется для `SELECT count() FROM @class [IN GRAPH @graph]` + `GROUP ALL` без scan.

`class_id` обязан быть ненулевым; scope согласован с `scope_graph`; `_pad` и хвост страницы
нулевые, а файл содержит ровно
одну page 0. При первоначальной COW-сборке `created_at_lsn = last_updated_lsn = 0`: side-file
становится видимым только через catalog commit, поэтому предварительно угадывать его LSN нельзя.
При последующей membership delta WAL writer присваивает `last_updated_lsn` тот же LSN, что и
page frame, и заново считает checksum до записи frame. Abort оставляет прежний Count видимым.
Недоступный Count index не используется; точный результат получается обычным class/membership scan.

### 20.4. Label Index (обязательный, неявный)

Файл: `idx/lbl_node_<label_id>.fgidx` и `idx/lbl_edge_<label_id>.fgidx`.

Sorted array `DiskAtomRef (u64)`, разбитый по страницам:

```
Offset  Size    Содержимое
0       32      PageHeader (magic=MAGIC_IDX_LABEL, type=IndexLabelArray)
32      8       next_page (u64)
40      4       count (u32)
44      4       _pad
48      16336   [DiskAtomRef(u64)] × count  → (PAGE_SIZE - 48) / 8 = 2042 per page
```

Каждый файл уже выбран точным `(AtomKind, label_id)`, поэтому equality lookup не ищет label
внутри файла: он последовательно читает плотную цепочку и выдаёт `DiskAtomRef` по возрастанию
`slot_index`. `next_page` обязан указывать на физически следующую страницу, непоследняя страница
обязана быть заполнена полностью, цикл/неверный kind/нарушение порядка являются corruption.

Stage-7 LabelIndex immutable и не имеет freelist или tombstone. Full graph generation строит его
одним проходом по уже распределённым слотам, без внешней сортировки ключей. Любое добавление,
удаление или смена label переводит readiness соответствующего catalog binding в `stale` в той
же транзакции; пока binding не `ready`, query path обязан сканировать hot slots с residual label
check. Полная COW-пересборка создаёт новое поколение файлов и атомарно публикует новый `ready`
binding. Отсутствующий, неоткрываемый или повреждённый Label side-file эквивалентен `stale`, но
не скрывает повреждение primary/base pages. `GraphSuperblock` readiness индексов не дублирует.

### 20.5. Fulltext Index (BM25)

Файл: `idx/ft_<label_id>_<key_id>.fgidx`

Инвертированный индекс: term → posting list.

#### 20.5.1. FulltextHeaderPage (Page 0)

```
PageHeader(32) (magic=MAGIC_IDX_FT, type=IndexFulltextHeader)
index_name_id:    u32
class_id:         u32
field_id:         u32
analyzer_id:      u32
avg_doc_len:      f32    -- средняя длина документа (обновляется)
_pad:             u32
doc_count:        u64    -- число проиндексированных документов
term_count:       u64    -- число уникальных термов
term_dict_page:   u64    -- первая страница TermDictionary
term_hash_page:   u64    -- первая страница TermHashIndexPage
postings_root:    u64    -- корень B+tree posting lists
```

**Параметры анализатора** (хранятся в `SchemaCatalog`, entry доступно по `analyzer_id`):
```
bm25_k1:          f32   -- default 1.2
bm25_b:           f32   -- default 0.75
highlights:       u8
language_id      u8     (0=any/default, 1=en, 2=ru, ...)
tokenizer_id     u8     (0=whitespace, 1=standard, 2=ngram)
min_gram         u8     (для ngram)
max_gram         u8
stemming         bool
stop_words_page  u64    (страница со stop-words для данного языка)
```

#### 20.5.2. TermDictPage

```
Offset  Size   Содержимое
0       32     PageHeader (magic=MAGIC_IDX_FT, type=IndexFulltextDict)
32      8      next_page (u64)
40      4      term_count (u32)
44      4      _pad
48      var    entries (sorted by term):
                  term_len(u16) + term_utf8[term_len]
                  + doc_freq(u32)             -- число документов, содержащих терм
                  + posting_page(u64)         -- первая posting page
                  + total_term_freq(u64)      -- суммарная частота терма по всем документам (для BM25)
```

Совместимо с `gql_spec.md §19` (анализаторы текста).

#### 20.5.3. TermHashIndexPage

Аналогично HashIndexPage, но с немного другим форматов записей:
```
PageHeader (magic=MAGIC_IDX_FT, type=IndexFulltextHash)

hash(u32) + term_page(u64) + term_offset(u32) -> posting_page

Entry size = 16 байт; (PAGE_SIZE − 48) / 16 = 1021 entries per page.
```

#### 20.5.4. PostingPage (fulltext)

```
Offset  Size   Содержимое
0       32     PageHeader (magic=MAGIC_IDX_FT, type=IndexFulltextPosting)
32      8      next_page (u64)
40      4      count (u32)
44      4      _pad
48      var    entries: [DiskAtomRef(u64) + term_freq(u16) + _pad(6)] × count
                  → (PAGE_SIZE - 48) / 16 = 1021 entries per page
```

Posting lists сортированы по DiskAtomRef → эффективный AND/OR через merge.

#### 20.5.5. BM25 scoring

```
score(d, q) = Σ_t IDF(t) × ((tf(t,d) × (k1+1)) / (tf(t,d) + k1 × (1 - b + b × dl/avgdl)))

IDF(t) = ln(1 + (doc_count - doc_freq(t) + 0.5) / (doc_freq(t) + 0.5))
```

Вычисляется во время query execution, хранится только `doc_freq` и `term_freq`.

### 20.6. HNSW и DiskANN vector indexes

HNSW и DiskANN имеют общий логический exact-delta contract, но разные immutable base formats,
filenames, side-format versions и implementation fingerprints:

```text
idx/hnsw_<ordinal:04>.fgidx
idx/hnsw_delta_<ordinal:04>.fgidx
idx/diskann_<ordinal:04>.fgidx
idx/diskann_delta_<ordinal:04>.fgidx
```

Base строится COW на фиксированном snapshot. WAL-versioned exact delta содержит insert/delete и
tombstones; поиск объединяет base и delta candidates и пересчитывает distance по полным vector
bytes. Rebuild запускается при delta size
`min(4096, max(1, floor(posting_count / 1000)))`. Несовместимый fingerprint или major version
помечает secondary index stale и требует rebuild; source data остаются читаемыми точным scan.

#### 20.6.1. Base header

Оба алгоритма используют `PageType::IndexVector` для page-framed header, но discriminator и
fingerprint однозначно выбирают собственный side codec:

```text
32      4     index_id
36      4     schema_version
40      2     side_format_version = 1
42      1     scope_kind
43      1     elem_type
44      1     distance
45      3     reserved = 0
48      8     scope_graph
56      4     class_id
60      4     field_id
64      4     dimensions
68      4     degree
72      4     l_build
76      4     canonical finite alpha:f32
80      8     vector_count; <= u32::MAX
88      4     entry_ordinal
92      4     record_bytes
96      2     records_per_page
98      6     reserved = 0
104     8     first_graph_page = 1
112     8     graph_page_count
120     8     build_lsn
128     8     vector_lookup_root
136     8     implementation_fingerprint
144     1     algorithm: 0=HNSW, 1=DiskANN
145     1     flags: bit0=HASHED_VECTOR
146     ...   reserved = 0
```

DiskANN base является disk-resident graph с bounded cache; HNSW base может materialize graph в
память. Ferros-Grynn сохраняет собственный versioned side-file, а не внешний изменяемый ABI.

#### 20.6.2. `HASHED_VECTOR`

Флаг разрешён только для DiskANN, входит в catalog definition и требует COW rebuild при
изменении. Без флага `vector_lookup_root` — B+tree
`canonical_vector_bytes → vector_ordinal`; слишком длинный fixed key отклоняется typed resource
error до build. С флагом B+tree хранит
`(RapidHashV3(canonical_vector_bytes, seed=0):u64, vector_ordinal:u32) → empty`.

Lookup выполняет bounded streaming prefix scan collision group и обязательно сравнивает полные
bytes из vector-data region; исчерпание budget — resource error, не ложное отсутствие. Перед
hashing dimensions и element type проверяются, float `-0.0` канонизируется в `+0.0`, NaN и
infinity запрещены. Одинаковые bytes имеют один ordinal и отдельные atom postings. Hash mode не
меняет ANN graph, distance, ranking, recall или result set; exact delta использует тот же режим.

#### 20.6.3. Query compatibility

DDL хранит `DIST @distance`; `distance` может быть built-in enum или `FunctionId +
SchemaVersion`. `<|K, DISTANCE|>` использует `DISTANCE` как metavariable выражения, а не keyword.
Planner выбирает ready ANN index только при точном совпадении distance, element type, dimensions,
scope и field. `<|K|>` имеет default EUCLIDEAN. `<|K, EF n|>` явно требует ровно один compatible
ANN index либо `WITH INDEX`; ambiguity/unavailability являются typed planning errors.

### 20.7. Geometry R-tree Index

Файл: `idx/geo_<ordinal:04>.fgidx`.

Spatial index для `geo::` операторов: `INSIDE`, `INTERSECTS`, `OUTSIDE`, `geo::within`, `geo::distance`.

**Структура**: R-tree (2D bounding rectangles).

#### 20.7.1. GeoHeaderPage

```
PageHeader(32) (magic=MAGIC_IDX_RTREE, type=IndexGeometryRtreeHeader)
index_name_id: u32
class_id:      u32
field_id:      u32
root_page:     u64   -- корень R-tree
node_count:    u64
```

#### 20.7.2. RtreeInternalPage

```
PageHeader(32) (magic=MAGIC_IDX_RTREE, type=IndexGeometryRtreeInternal)
num_entries: u16
_pad: 14
entries: [mbr_lo_lon(f64) + mbr_lo_lat(f64) + mbr_hi_lon(f64) + mbr_hi_lat(f64)
          + child_page(u64)] × num_entries
→ MBR(32 байта) + child(8 байт) = 40 байт/entry
→ (PAGE_SIZE - 48) / 40 = 408 entries per page
```

#### 20.7.3. RtreeLeafPage

```
PageHeader(32) (magic=MAGIC_IDX_RTREE, type=IndexGeometryRtreeLeaf, FLAG_LEAF set)
num_entries: u16
_pad: 6
entries: [mbr_lo_lon + mbr_lo_lat + mbr_hi_lon + mbr_hi_lat (f64×4)
          + DiskAtomRef(u64)] × num_entries
→ (PAGE_SIZE - 40) / 36 = 454 entries per page
```

### 20.8. Reachability Index

`TREE_COVER`, `GRAIL`, `FERRARI`, `BFL` и `CHAIN_COVER` — отдельные derived accelerators.
Для ориентированного graph сначала вычисляются SCC, детерминированно нумеруются по минимальному
`GlobalAtomId` и сворачиваются в condensation DAG. Одна SCC даёт `Yes`; остальные проверки
возвращают внутреннее `Yes | No | Maybe`, где `Maybe` всегда запускает bounded exact DFS/BFS.
Для `BOTH` индекс строится над симметризованным отношением. Mutation dependency делает side-file
stale; stale artifact никогда не влияет на корректность.

#### 20.8.1. ReachabilityHeaderPage

```text
0       32    PageHeader(IndexReachabilityHeader)
32      4     index_id
36      4     schema_version
40      8     graph_id
48      8     build_lsn
56      8     node_capacity
64      8     edge_capacity
72      8     component_count
80      8     condensation_edge_count
88      8     node_component_root
96      8     edge_component_root
104     8     representative_root
112     8     csr_offsets_root
120     8     csr_targets_root
128     8     algorithm_data_root
136     8     algorithm_aux_root
144     8     algorithm_aux_count
152     8     max_bytes
160     4     edge_label_id; NULL_LABEL=all, 0 is valid
164     4     algorithm_parameter
168     1     algorithm: 0=TREE_COVER, 1=GRAIL, 2=FERRARI, 3=BFL, 4=CHAIN_COVER
169     1     direction: 0=OUT, 1=IN, 2=BOTH
170     2     flags = 0
172     ...   reserved = 0
```

`ON_DEMAND` — только planner strategy и physical header не создаёт. Status
`building/ready/stale/error` хранится только в catalog. `algorithm_parameter`: 0 для TREE_COVER и
CHAIN_COVER, `k` для GRAIL, `max_ranges` для FERRARI, `bits` для BFL.

#### 20.8.2. Общие derived codecs

`PageType::IndexDerivedArray=0x003E`:

```text
32 next_page:u64; 40 section_kind:u16; 42 element_width:u16;
44 element_count:u32; 48 first_ordinal:u64; 56 used_bytes:u32;
60 reserved:u32=0; 64 payload
```

`PageType::IndexDerivedRecords=0x003F`:

```text
32 next_page:u64; 40 section_kind:u16; 42 record_count:u16;
44 used_bytes:u16; 46 reserved:u16=0; 48 first_ordinal:u64;
56 offsets:u16[record_count+1], затем records
```

Единое пространство `section_kind`:

| Код | Секция | Код | Секция |
|----:|--------|----:|--------|
| `0x0001` | NodeComponentMap | `0x0010` | PatternDescriptor |
| `0x0002` | EdgeComponentMap | `0x0011` | PatternCanonicalIr |
| `0x0003` | SccRepresentativeMap | `0x0012` | PatternBindingSchema |
| `0x0004` | CondensationCsrOffsets | `0x0013` | PatternDependencies |
| `0x0005` | CondensationCsrTargets | `0x0014` | PatternBindingRows |
| `0x0006` | PathVertexMap | `0x0100` | TreeCoverIntervals |
| `0x0007` | LandmarkRefs | `0x0101` | GrailIntervals |
| `0x0008` | LandmarkDistanceFrom | `0x0102` | FerrariIntervals |
| `0x0009` | LandmarkDistanceTo | `0x0103` | BflBitsets |
| | | `0x0104` | ChainCoordinates |
| | | `0x0105` | ChainReachabilityCells |

`0x003E` принимает только `0x0001..=0x0009`, `0x003F` — `0x0010..=0x0014`; specialized page types
`0x0027..=0x002A` принимают соответственно только `0x0100..=0x0103`, а renamed
`IndexReachabilityChainCover=0x002B` — `0x0104/0x0105`. Ноль, unknown и несовместимая пара
PageType/section_kind являются corruption.

Fixed arrays проверяют exact element width/count, checked `u64` total, непрерывность
`first_ordinal` и нулевой tail. Component/representative/CSR/path/landmark refs имеют width 8;
TREE/GRAIL intervals и chain coordinates — 16; BFL — `2*ceil(bits/8)`;
chain cells — 8. GRAIL хранит все `k` dimensions; FERRARI — bounded exact/approx interval set;
BFL — hashed reachability filters без landmarks и без false negatives; Chain-Cover хранит
отдельно coordinates и плоскую component×chain matrix. Пустая секция имеет `NULL_PAGE`.

#### 20.8.3. Алгоритмические контракты

- `TREE_COVER` разрешён только для TREE/ROOTED_TREE/FOREST. OUT/IN хранит точные ancestor
  intervals; BOTH сравнивает connected-component ordinals симметризованного отношения.
- `GRAIL(k)`, `k=1..=32`, default 2, проверяет все dimensions: нарушение containment хотя бы в
  одной даёт `No`; прохождение всех даёт `Maybe`, не `Yes`.
- `FERRARI(max_ranges)`, default 4, range `1..=255`, объединяет exact postorder intervals и
  помечает покрытые gaps как Approximate. Exact hit даёт `Yes`, miss — `No`, approximate hit —
  `Maybe` с guided traversal.
- `BFL(bits)` использует `64..=4096` bits, кратно 8, default 160. SCC хешируются portable
  RapidHash V3 seed 0, bitsets распространяются OR по topological orders. Если
  `out(target) ⊄ out(source)` или `in(source) ⊄ in(target)`, ответ `No`; иначе `Maybe`.
- `CHAIN_COVER` хранит `{chain_id,position}` и плоскую строку
  `lowest_reachable_position[chain]`; `u64::MAX` означает отсутствие. Точный ответ:
  `source_row[target.chain_id] <= target.position`.

`ALGORITHM AUTO` и explicit CHAIN_COVER требуют `WITH (MAX_BYTES=n)`. AUTO выбирает TREE_COVER
для tree families, затем для прочих graph families пробует CHAIN_COVER, FERRARI(4), BFL(160) и
ON_DEMAND по projected bytes. Explicit candidate при превышении бюджета возвращает resource
error и не заменяется молча. FERRARI/BFL также ограничены своим параметром; дополнительный
MAX_BYTES является hard cap.

### 20.9. Neighbourhood Index

**Файл:** `idx/nbh_<ordinal:04>.fgidx`

Создаётся один файл на один именованный граф.
Заголовок хранит ссылки на корни B+tree для каждого из видов индекса.

#### NeighbourhoodHeaderPage (страница 0)

```
Offset  Size  Тип    Поле               Описание
0       32    —      PageHeader (magic=MAGIC_IDX_NBH, type=IndexNeighbourhoodHeader)
32      1     u8     algorithm          0=exact, 1=sketch, 2=landmark, 3=on_demand
33      1     u8     max_k              предвычисленный радиус (1..4)
34      1     u8     direction          0=out, 1=in, 2=both
35      1     u8     status             0=building, 1=ready, 2=stale
36      4     u32    params             LANDMARK: n_landmarks
                                        SKETCH: hll_precision (4..18, default=12)
                                        остальные: 0
40      8     u64    node_count
48      8     u64    build_lsn
56      32    u64[4] exact_roots        EXACT: B+tree корни для k=1..4
88      8     u64    sketch_root        SKETCH: B+tree корень
96      8     u64    landmark_refs_page LANDMARK: список landmark DiskAtomRef;
                                        остальные: NULL_PAGE
104     8     u64    landmark_nbh_root  LANDMARK: B+tree окрестностей landmark-вершин
112     16272 —      _reserved
Total = PAGE_SIZE байт
```

#### EXACT — Neighbourhood Posting Pages

  B+tree (exact_roots[k-1]): ключ = DiskAtomRef(u64) центра, значение = page_no первой NeighbourhoodListPage.

```
NeighbourhoodListPage:
    Offset  Size    Содержимое
    0       32      PageHeader (MAGIC_IDX_NBH, type=IndexNeighbourhoodExact)
    32      8       center_ref: DiskAtomRef(u64)
    40      1       k (u8)
    41      7       _pad
    48      8       next_page (u64)
    56      4       count (u32)
    60      4       _pad
    64      16320   neighbors: DiskAtomRef(u64) × count
    -- per page: 16320 / 8 = 2040 соседей
```

#### SKETCH — HyperLogLog Pages

B+tree (sketch_root): составной ключ = k(u8) + _pad(7) + DiskAtomRef(u64) = 16 байт, значение = hll_sketch(u8[64]).

```
PageHeader (MAGIC_IDX_NBH, type=IndexNeighbourhoodSketch)
```

#### LANDMARK

LANDMARK хранит точные окрестности только для landmark-вершин.
Формат данных идентичен EXACT, но в B+tree (landmark_nbh_root) присутствуют только записи для landmark-вершин, не для всех.

```
PageHeader (MAGIC_IDX_NBH, type=IndexNeighbourhoodLandmarkList)
```

landmark_refs_page: LandmarkListPage в том же формате, что §20.8 LandmarkListPage.

Для нелэндмарк-вершин данных нет: запрос выполняется BFS с ранним отсечением при достижении landmark.

### 20.10. Path Index

**Файл:** `idx/path_<ordinal:04>.fgidx`

#### 20.10.1. PathIndexHeaderPage

```text
0       32    PageHeader(IndexPathHeader)
32      4     index_id
36      4     schema_version
40      8     graph_id
48      8     build_lsn
56      8     vertex_count
64      4     landmark_count
68      4     edge_label_id; NULL_LABEL=all, 0 is valid
72      8     vertex_map_root
80      8     landmark_refs_root
88      8     distance_from_root
96      8     distance_to_root
104     8     pattern_catalog_root
112     8     pattern_rows_root
120     8     dependency_root
128     8     max_bytes
136     4     cost_function_id
140     4     cost_function_version
144     1     algorithm: 0=LANDMARK_SSSP, 1=PATTERN_CACHE
145     1     direction: 0=OUT, 1=IN, 2=BOTH
146     1     cost_kind: 0=NONE, 1=HOP, 2=WEIGHT, 3=FUNCTION
147     1     distance_encoding: 0=NONE, 1=U64, 2=F64_INTERVAL
148     4     flags = 0
152     ...   reserved = 0
```

Readiness/staleness хранится в catalog; `ON_DEMAND` physical header не создаёт. LANDMARK_SSSP
использует roots `PathVertexMap`, `LandmarkRefs`, `LandmarkDistanceFrom/To` и NULL pattern roots.
PATTERN_CACHE использует `PatternDescriptor`, `PatternBindingRows`, `PatternDependencies` и
locators на canonical IR/schema; vertex/landmark/distance roots равны NULL_PAGE.

#### 20.10.2. LANDMARK_SSSP и directed ALT

Без modifier стоимость ребра равна 1; `WEIGHTED` использует встроенный вес; `COST fn` совместим
только с той же `PURE (edge)->number` функцией и `SchemaVersion`. Полная стоимость аддитивна.
Stateful `(edge, accumulated_cost)` остаётся только ON_DEMAND. Отрицательные, NaN и infinite
costs запрещены; встроенный NaN sentinel предварительно нормализуется в 1.

Для directed graph хранятся расстояния от и до landmarks, а A* heuristic равна:

```text
h(v,t) = max(0,
             max_l(d(l,t) - d(l,v)),
             max_l(d(v,l) - d(t,l)))
```

Участвуют только конечные terms. Это admissible ALT lower bound; A* повторно открывает
улучшенные states. Landmark count `1..=256`, selection deterministic farthest-point с tie по
меньшему `GlobalAtomId`. Hop distance — `u64` (`u64::MAX` infinity); weighted/custom — directed
rounded interval `{lower:f64,upper:f64}`, вычитание использует lower minus upper и `next_down`.
Weight/property/function dependency mutation делает индекс stale.

#### 20.10.3. PATTERN_CACHE

`PATTERNS` — конечный список parameter-free bounded path patterns с PURE functions и literal
constraints. Unbounded WALK, runtime parameters, STABLE/IMPURE functions и projected result выше
обязательного `MAX_BYTES` отклоняются до build.

Cache identity содержит alpha-normalized analyzed IR, GraphId, directions, labels, constraints,
cost model, canonical literals и versioned dependencies. Hash служит только lookup; collision
сравнивает полные canonical bytes. Cached payload — exact binding rows, а не `SubgraphDirPage`:

```text
PatternDescriptor:
  pattern_ordinal:u32; binding_count:u16; reserved:u16=0;
  dependency_count:u32; reserved:u32=0; canonical_hash:u64;
  build_lsn:u64; source_revision:u64;
  canonical_ir:Locator16; binding_schema:Locator16; dependencies:Locator16;
  first_row_ordinal:u64; row_count:u64

CachedBindingRow:
  multiplicity:u64 (>0); binding_count:u16; reserved:u16=0; canonical bindings...
```

Atom/edge bindings содержат `GlobalAtomId`; named path — start atom и ordered
`(edge GlobalAtomId, endpoint GlobalAtomId)[]`. Equal rows схлопываются с multiplicity и
сортируются lexicographically. `WHERE`, `RETURN` и `RETURN GRAPH` работают поверх восстановленных
rows; materialized graph строится обычным путём. Membership/topology/label/overlay либо видимое
base-property изменение и schema/function dependency mutation атомарно делают definition stale.

### 20.11. Совместное использование индексов

При `DEFINE INDEX ... DEFER` — индекс работает в eventually-consistent режиме:

- Pending changes накапливаются в `IndexPendingQueue` в памяти.
- Background worker применяет их с интервалом.
- `status` = `building_update` пока очередь непуста.

При `CONCURRENTLY` — начальная индексация без блокировки class source:

- Snapshot существующих записей → bulk insert.
- Параллельные changes → в PendingQueue.
- После завершения bulk → flush PendingQueue.

### 20.12. EdgeEndpointIndex

Когда атом имеет другое ребро как endpoint (V-E, E-V, E-E), стандартный adjacency index (NodeHotSlot.adj_*) 
не отслеживает такие связи — он работает только с вершинами.

**Назначение:** ускорить AtomWalk-traversal через edge-endpoints.

**Файл:** `idx/ee_endpoint.fgidx`

**Структура:** B+tree.
- Ключ: `DiskAtomRef` edge-endpoint (`QualifiedDiskAtomRef` при `CROSS_LEVEL`)
- Значение: список записей `(edge_ref, side)`

```
Header:
  PageHeader (magic=MAGIC_IDX_EE, type=IndexEdgeEndpointHeader)
```

Листовые страницы используют стандартный формат B+tree-листа (§20.2.3); собственного `PageHeader` у отдельной записи, разумеется, нет — заголовок принадлежит странице. Запись:

```
LeafEntry (24 байта):
  endpoint_ref:  u64  -- DiskAtomRef ребра, выступающего endpoint'ом (ключ)
  edge_ref:      u64  -- DiskAtomRef ребра, у которого оно endpoint
  side:          u8   -- 0=source, 1=target
  _pad:          u8[7]
```

**Создаётся автоматически** при наличии хотя бы одного ребра с is_edge(endpoint) = true.
Для каждого edge-endpoint записывается одна строка на фактическую сторону. Insert/delete
выполняется в той же транзакции, что и изменение `EdgeHotSlot`; vertex-endpoint'ы в индекс
не попадают.

**Использование при traversal:**
1. Текущий атом = ребро $e_1$ (AtomWalk-режим)
2. `EdgeEndpointIndex.lookup(e_1)` → список `(e_n, side)`
3. Для каждого $e_n$ → противоположные endpoint'ы из `EdgeHotSlot` → следующие атомы

Это единственный дополнительный access path для edge-endpoint'ов. Источником истины остаются
`source`/`target` ребра; индекс полностью перестраиваем.

### 20.13. Bulk-сборка индексов

Построчная вставка сопровождает каждый индекс отдельно: спуск по дереву за `O(log n)` **случайными** обращениями к страницам, расщепления, и WAL-запись на каждую пару (строка, индекс). При загрузке графа это доминирующая стоимость. Bulk-путь строит индекс снизу вверх, записывая каждую страницу **ровно один раз**, последовательно.

Путь состоит из четырёх переиспользуемых компонентов; они одни и те же для `DEFINE INDEX` на непустом class, `REBUILD INDEX`, `COMPACT INDEX` и режима импорта (`gql_spec` §7.11).

#### 20.13.1. Внешняя сортировка

Логический ключ сортировки — `(encoded_key, DiskAtomRef)` (сравнение raw `DiskAtomRef` как
беззнакового числа эквивалентно сравнению его big-endian bytes). Для scalar B+tree
`encoded_key` является order-preserving (§20.2.1), поэтому поиск сравнивает байты без
декодирования. AtomStore primary сохраняет отдельный canonical `RecordId` codec (§4.2):
его byte order нужен только как стабильный внутренний порядок exact-lookup B+tree и не
объявляется семантическим порядком `RecordId`.

Добавление `DiskAtomRef` делает sort order **тотальным**, поэтому устойчивая сортировка не нужна,
а posting-списки выходят уже упорядоченными по `DiskAtomRef` — ровно так, как требуется §20.5.4
для слияния AND/OR. Label-массивы строятся отдельно из монотонных generation-local slots.

Stage-7 baseline использует фиксированный budget **256 MiB** на один run: заполнить bounded
`Vec`, отсортировать `sort_unstable`, записать page-aligned run через `DirectFile`, затем выполнить
k-путёвое слияние с fan-in **64** через стандартный `BinaryHeap`. Scratch принадлежит БД и лежит
в `tmp/index-sort/<session_uuid>`; RAII удаляет session после успеха/ошибки, startup удаляет хвост
процесса, завершившегося аварийно. Финальные не более 64 runs дают replayable stream, напрямую
питающий двухпроходный standard packer или однопроходный unique/primary packer без materialized
`Vec` всех sorted records.

Имена `FG_BULK_SORT_MEM`, `FG_TEMPFILES_PATH` и `FG_BULK_SORT_MERGE_FANIN` не являются вторым
скрытым источником конфигурации: до унификации configuration authority (§27) действуют указанные
фиксированные значения и database-owned path. Loser tree и внутренняя parallel sort добавляются
только если профиль representative import покажет, что heap/sort, а не I/O или page packing,
является bottleneck.

Параллелизм имеет смысл прежде всего **по индексам** (загрузка объявляет 5–10 независимых индексов), а не внутри слияния одного.

#### 20.13.2. Упаковка снизу вверх

```
target = floor(usable_leaf_space × FILL_FACTOR)
для каждой группы записей с одинаковым ключом (уже упорядоченной по ref):
    inline_count известен ЗАРАНЕЕ → запись сразу пишется в финальной форме
    если не помещается в текущий лист — запечатать лист, начать новый
запечатать последний лист
```

Ключевое отличие от построчного пути: `inline_count` известен до записи, поэтому запись не приходится переписывать и мигрировать из inline-формы в posting-страницы при пересечении порога в 8 ссылок.

Физическая раскладка файла делает цепочку листьев **непрерывной**, поэтому
`next_leaf = page_no + 1`, а полный скан индекса становится одним последовательным чтением.
Отдельная метастраница не нужна: exact root хранится в durable catalog entry.

```
страницы 0..L-1   : листья (непрерывно)
страницы L..L+P-1 : posting-страницы standard index
далее             : внутренние уровни, снизу вверх
последняя         : корень, если дерево имеет больше одного листа
```

Внутренние уровни строятся снизу вверх из bounded списка `(page_no, min_key)` текущего уровня.
Stage-7 baseline хранит полный `min_key` как разделитель; допустимое §20.2.2 усечение является
оптимизацией размера и добавляется только после page-count/lookup профиля. Уровень,
завершившийся одной страницей, и есть корень; для одно-листового дерева root указывает на leaf 0.

#### 20.13.3. Fill factor

Stage-7 immutable generation packer заполняет страницу до её физической capacity: после
публикации он не выполняет in-place insert/split, поэтому искусственный reserve только увеличил
бы read amplification. `FG_INDEX_BTREE_FILL_FACTOR` ещё не является production setting.

Stage-9 point-DML сначала пытается обновить только затронутые leaf-страницы через page WAL.
Этот путь допустим, только если не меняются разделитель internal page, число leaf-страниц и
posting-layout Standard index. Иначе используется существующая полная перестройка текущего
mutable index image; unique validation выполняется до публикации, а recovery видит один
атомарный WAL commit.

Paired-профиль на 50 000 записях и batch `1/32/256` подтвердил, что заполненные до capacity
mutable leaf почти всегда немедленно уходят в полную перестройку. Поэтому format-v1 фиксирует
следующую writer policy:

| Путь построения                                         | Leaf fill |
|---------------------------------------------------------|-----------|
| Immutable external-sort generation                      | 1.00      |
| Mutable primary и Unique fallback image                 | 0.90      |
| Mutable Standard fallback image                         | 0.80      |

На primary при fill `0.90` leaf-only path обновлял 1 страницу вместо 100 для point mutation и
32 страницы вместо 100 для batch 32; медианное отношение времени incremental/bulk составило
соответственно `0.013` и `0.198`. На Standard при fill `0.80` отношения составили `0.005` и
`0.110`; batch 256 оставался быстрее bulk (`0.127`) на измеренном распределении. При fill `1.00`
primary batch 256 и Standard batch 256 требовали bulk fallback, который был быстрее попытки
сохранить плотную mutable раскладку.

Fill factor является политикой writer, а не новым on-disk discriminator или ограничением reader.
Пользовательская настройка и автоопределение монотонности не вводятся: immutable packer остаётся
плотным, а изменение этих констант требует нового paired workload и проверки page count, recovery
и unique semantics. Отдельный internal fill factor и настройка R-tree в core format v1 не входят.

#### 20.13.4. Протокол side-file: журнал не растёт

Тело bulk-сборки **не журналируется**. Сборка идёт в новое UUID-поколение graph directory,
недостижимое из закоммиченного состояния каталога, поэтому крах в процессе не может нарушить
консистентность — незавершённое поколение удаляется startup-GC.

```
1. Создать `graphs/<graph_id>/<generation_uuid>/`; это имя ещё отсутствует в durable catalog.
2. Последовательно записать base files и side-files напрямую через `DirectFile`; каждый файл
   завершить `sync_all`. Записей page WAL для тела индекса: **ноль**.
3. Открыть и зарегистрировать ровно эти trusted paths, но не делать их видимыми читателям.
4. В одной транзакции записать новые database/file catalog pages и root switch; `TxCommit` +
   WAL fsync атомарно публикуют schema definition, `DataFileId` и B+tree root.
5. При abort снять временные registrations; недостижимые bytes удалит startup-GC. Прежнее
   поколение удерживается активными snapshots и удаляется только после безопасного retirement.
```

Порядок величин: сборка индекса на 3 ГБ даёт **~200 байт** журнала вместо десятков гигабайт при построчном пути.

**Нумерация поколений вместо перезаписи** принципиальна для Windows (целевая платформа):
открытый читателями файл нельзя безопасно заменить как единственную identity. UUID directory и
точный `DataFileId` в каталоге оставляют старые handles на прежнем поколении и снимают этот класс
проблем на обеих платформах.

Восстановлению не нужна отдельная `IndexBuildComplete`: durability boundary уже является
database-level catalog commit. UUID directories, на которые не ссылается восстановленный
catalog, удаляются. Если опубликованный secondary side-file отсутствует, не открывается или не
проходит checksum, он исключается из physical registry и access path деградирует к scan; schema
definition сохраняется, runtime state становится `stale` и наблюдается через `INFO`; последующий
`REBUILD` может опубликовать новый binding. Та же ситуация для
primary/base file является фатальной corruption.

#### 20.13.5. Стратегия по типам индексов

| Индекс                              | Стратегия                                                                        | Нужна сортировка            |
|-------------------------------------|----------------------------------------------------------------------------------|-----------------------------|
| B+tree standard / unique, `pk_`     | Внешняя сортировка → упаковка снизу вверх                                        | да                          |
| Label                               | Слоты выделяются монотонно → разбиение по `label_id` и последовательная запись   | **нет**                     |
| Count                               | Побочный продукт скана                                                           | нет                         |
| Fulltext BM25                       | Сортировка кортежей `(term, doc_ref, tf)` → потоковая инверсия по группам термов | да                          |
| HNSW                                | Параллельная сборка в памяти, сериализация один раз                              | не применимо                |
| Geometry R-tree                     | STR-упаковка (Sort-Tile-Recursive)                                               | да, 2 сортировки на уровень |
| Reachability / Neighbourhood / Path | Вычисляются **после** загрузки; вход уже упорядочен по `DiskAtomRef`             | нет                         |
| EdgeEndpoint                        | Сортировка `(endpoint_ref, edge_ref, side)`                                      | да                          |

**Проверка уникальности бесплатна.** В отсортированном потоке нарушение уникальности — это ровно пара соседних записей с равным префиксом-ключом. Одно сравнение с предыдущей записью, `O(1)` памяти, ноль обращений к диску — вместо спуска по дереву на каждую строку только ради ответа на вопрос «есть ли дубликат».

Тем же проходом бесплатно получаются `distinct_count`, границы корзин гистограммы, `null_fraction` (§19.5) и `doc_freq` для полнотекстового индекса.

**Производные индексы (reachability, neighbourhood, path) не поддерживают построчного сопровождения в принципе:** все их алгоритмы (интервальные метки DFS, GRAIL, FERRARI, BFL, k-hop замыкание, multi-source Dijkstra) требуют глобального обзора графа, и одна вставка ребра может инвалидировать `Θ(n)` меток. Они объявляются **снимковыми артефактами**, актуальными на `build_lsn`: любая мутация топологии переводит их в `stale`, а устаревший индекс не используется молча — планировщик откатывается к `ON_DEMAND`. У них нет `insert`/`delete`, только `build(snapshot)` и `invalidate()`.

**STR-упаковка R-tree** (Sort-Tile-Recursive): отсортировать по координате X, разбить на `S = ceil(sqrt(P))` вертикальных полос, внутри каждой отсортировать по Y и нарезать узлами по `C` элементов; полученные MBR подать на следующий уровень. Даёт заполнение узлов ~100% против ~70% при поштучной вставке и существенно меньшее перекрытие MBR.

**HNSW** сортировке не поддаётся — это инкрементальное построение графа. Bulk-режим ограничивается параллельной сборкой в памяти с **перемешиванием порядка вставки**: если поток загрузки кластеризован (по class, label или исходному файлу), жадное построение HNSW деградирует по полноте выдачи, поскольку алгоритм рассчитан на случайный порядок поступления.

#### 20.13.6. Отношение к `DEFER` и `CONCURRENTLY`

`DEFER` (§20.11) **не является** заменой bulk-сборке: он переносит ту же построчную работу в фон, то есть превращает проблему латентности в проблему очереди. На загрузке в сотни миллионов строк это даёт очередь того же порядка.

Разрешение противоречия «очередь в памяти или персистентная» (§20.11 против `gql_spec` §9.6): очередь остаётся **в памяти и ограниченной** `FG_INDEX_PENDING_MAX`. При достижении порога происходит **эскалация**: очередь сбрасывается, индекс помечается `stale`, планируется полная bulk-пересборка. Это crash-safe по построению — ничего долговечного не теряется, поскольку устаревший индекс всё равно строится заново из базовых данных. Персистентная очередь потребовала бы собственного формата, вида WAL-записи и семантики восстановления — то есть незаметно превратилась бы в L0 LSM-дерева, от которого проект отказался сознательно.

Роли получаются раздельными и определёнными: `DEFER` — оптимизация **малой** дельты при редких точечных записях; bulk-сборка — путь **большой** дельты; между ними один явный переход.

`CONCURRENTLY` реализуется **поверх** bulk-сборки: снимок на `build_lsn` → внешняя сортировка и сборка в side-file → добор накопившейся (к этому моменту небольшой) очереди обычными вставками → атомарная подмена поколения.

---

## 21. Changefeed и Live-запросы

### 21.1. Changefeed Log

Требуется `CHANGEFEED @duration` в определении CLASS либо GRAPH source.

**duration** — время хранения изменений (например, `CHANGEFEED 7d` хранит 7 дней).

```
changefeed/<space_id>_<class_id>_<since_lsn>.fcf
```

#### 21.1.1. ChangefeedPage

```
PageHeader(32) (magic=MAGIC_CHANGEFEED, type=ChangefeedHeader)
space_id:      u64  -- DataSpaceId
class_id:      u32
next_page:     u64
entry_count:   u32
_pad:          4
entries: [ChangefeedEntry]×N
```

**ChangefeedEntry (variable):**

```
PageHeader (magic=MAGIC_CHANGEFEED, type=ChangefeedEntry)
lsn:           u64
versionstamp:  u64   -- монотонно растущий глобальный счётчик
event_kind:    u8    -- 0=create, 1=update, 2=delete, 3=define_class, 4=membership
global_atom:   u64   -- GlobalAtomId; generation проверяется
record_id_len: u16
record_id:     [u8]  -- encoded RecordId
[before_len:   u32]  -- если event_kind ∈ {1, 2}
[before_data:  [u8]] -- PropertyMap до изменения
[after_len:    u32]  -- если event_kind ∈ {0, 1}
[after_data:   [u8]] -- PropertyMap после изменения
```

#### 21.1.2. Changefeed purge

WAL-like rotation: файлы старше `changefeed_duration` удаляются.
Cursor `since_lsn` в имени файла позволяет быстро найти нужный диапазон.

### 21.2. Live SELECT

`LIVE SELECT` подписывается на изменения class либо graph-scoped effective source.

**Хранимое состояние live-запроса** (в SchemaCatalog, kind=LiveSelect):

```
live_id:       u128  -- UUID v7 ($id для KILL)
space_id:      u64  -- 0 для Base; GraphId для Effective
class_id:      u32
filter_ir_len: u32
filter_ir:     [u8]  -- сериализованный IR предиката WHERE
output_mode:   u8    -- 0=full_record, 1=value_fields, 2=diff, 3=patch
created_at_ms: u64
last_active_ms:u64
```

**Delivery mechanism**: на каждый DML подходящего class/graph source при наличии live-подписчиков:

1. Определить matching live queries (filter IR evaluation).
2. Добавить в `LiveEventQueue` (in-memory, per-session).
3. Session reader вычитывает из очереди и отправляет клиенту.

Live queries хранятся только в SchemaCatalog для восстановления сессии после перезапуска.
Сами очереди — in-memory.

---

## 22. WAL для дискового режима

Дисковый режим использует **физический журнал из образов страниц** (frame-WAL) с индексом версий страниц — модель «copy-on-write в логе», как в SQLite WAL и RavenDB Voron. Журнал единый на всю базу данных (§7.1).

### 22.0. Модель

Писатель, желающий изменить логическую страницу `P`, **не правит её на месте**. Он хранит
логические atom-delta в приватном write-set. Под единым DB commit lock они сливаются с актуальными
страницами, после чего WAL подряд получает полные фреймы транзакции и маркер `TxCommit`; затем
выполняются один buffered `flush` и один `sync_data`. LSN заголовка `TxCommit` является
`commit_lsn`. Снимок хранит `StorageSnapshot { commit_ts, visible_lsn }`, где `visible_lsn` —
durable граница журнала на момент начала транзакции. Фрейм видим только если `commit_lsn ≤
snapshot.visible_lsn`. Checkpoint переносит закоммиченные фреймы обратно **на их штатные места**
в `.fgb` (§23.1), кроме superblock frames: их единственная home-page публикуется COW.

Одна транзакция может содержать frames `DataSpaceId::Database` и нескольких
`DataSpaceId::Graph(_)`. Только общий `TxCommit` делает их видимыми; recovery не имеет права
публиковать base atom без соответствующей membership/overlay/topology части или наоборот.
Reverse membership, effective Unique preflight и graph CASCADE используют тот же bounded
transaction-private write-set до первой durable записи.

Что эта модель даёт по сравнению с журналом дельт:

| Проблема                        | Решение                                                                                                                                                                                    |
|---------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Незакоммиченные данные на диске | Фреймы без durable `TxCommit` невидимы и никогда не бэкфиллятся; начальный atom-delta write-set ограничен 256 МиБ                                                                          |
| Torn write                      | Фрейм **и есть** полный образ страницы; отдельный механизм full-page-write не нужен                                                                                                        |
| Отсутствие on-disk MVCC         | Снимок видит committed-фреймы с `commit_lsn ≤ visible_lsn`; версионность появляется как побочный эффект                                                                                    |
| Потолок объёма транзакции       | До commit lock вычисляется консервативный рекурсивный объём owned atom-delta; при превышении 256 МиБ commit отклоняется `FG-54F07`; transaction-private spill — отдельный последующий этап |
| Откат                           | Отбросить scratch-фреймы и записи индекса. UNDO и CLR не нужны                                                                                                                             |
| Идемпотентность replay          | Применение фрейма — `memcpy`; повторное применение того же образа ничего не меняет                                                                                                         |

Ключевое отличие от «чистого COW» (LMDB): обратная запись **на штатное место** сохраняет арифметическую адресацию §25.1 (`page_no = slot / 102`) и физическую кластеризацию SoA, на которых держится аналитический путь. Data-file COW обе эти вещи разрушает.

Лимит `FG_TX_WRITE_SET_MAX_BYTES` является лимитом логического transaction-private payload, а не
перехватчиком каждой аллокации. Оценка рекурсивно включает capacity контейнеров, graph/atom
properties, строки и составные `RecordId`, topology и adjacency representation, а также вложенные
и transition graphs; арифметическое переполнение насыщает оценку и приводит к отказу. Проверка
выполняется в начале `Transaction::commit` после проверки liveness, но до захвата commit lock,
FCW/SSI validation, page encoding, WAL I/O и публикации MVCC root. Следовательно, вызывающий код
может уже потратить память при построении крупной мутации, однако отклонённая транзакция не делает
ни одного durable или видимого изменения. Отдельный жёсткий лимит уникальных frame-WAL страниц
остаётся вторым fail-safe после кодирования. Для заранее крупных загрузок нормативный bounded путь
— сегментный импорт; allocation-time quota и private spill в format v1 не обещаются.

### 22.1. Набор WalKind

**Записи восстановления** (участвуют в replay):

```
TxBegin         = 0x30,   // Начало транзакции
TxCommit        = 0x31,   // Коммит: commit_ts + frame_count
TxAbort         = 0x32,   // Явный откат
PageFrame       = 0x33,   // Полный образ страницы
PageInit        = 0x34,   // Пустая аллокация страницы после checkpoint
CheckpointBegin = 0x35,
CheckpointEnd   = 0x36,   // generation + backfill_lsn
GraphCreate     = 0x37,   // graph_id + name
GraphDrop       = 0x38,   // graph_id
```

**Логические аннотации** (0x10–0x22 из прежней редакции: `NodeAlloc`, `AdjAppend`, `PropSet`, `IndexInsert`, `SchemaDef`, `StatsUpdate`, `ChangefeedPut` и прочие) **сохраняются, но перестают быть записями восстановления**. Они опциональны, не реплеятся и служат changefeed'у, логической репликации и диагностике. Единственный источник истины при восстановлении — фреймы.

Записи режима 1 (0x01–0x07, in-memory MVCC поверх логического WAL) не изменяются.

### 22.2. Формат записей

Заголовок записи:

```
[ FGW1:4 ][ lsn:8 ][ tx_id:8 ][ kind:1 ][ payload_len:4 ][ checksum:8 ] = 33 байта
```

`FGW1` одновременно версионирует новый `DataFileId` payload-контракт. Экспериментальный magic
среза 3a `MVCE` явно отклоняется: его `file_kind + pad[7]` нельзя двусмысленно принять за
`DataFileId(instance_id = 0)`.

`checksum` — portable RapidHash V3 (seed `0`, `RapidSecrets::seed_cpp(0)`) по
`lsn ‖ tx_id ‖ kind ‖ payload_len ‖ payload`. Алгоритм и seed совпадают с checksum страницы (§8).
`payload_len` не превышает размер `PageFrame` (16 408 байт); scanner проверяет границу до
allocation.

Payload по видам:

```
PageFrame:       space_id(u64) + file_id(u64) + page_no(u64) + page_bytes(PAGE_SIZE)
                 Total = 16408 байт
PageInit:        space_id(u64) + file_id(u64) + page_type(u16) + reserved[6] + page_no(u64)
                 Total = 32 байта; reserved = 0
TxBegin:         (пусто)
TxCommit:        commit_ts(u64) + frame_count(u32) + _pad(4)
TxAbort:         (пусто)
CheckpointBegin: (пусто)
CheckpointEnd:   generation(u64) + backfill_lsn(u64)
GraphCreate:     graph_id(u64) + name_len(u16) + name_utf8(variable)
GraphDrop:       graph_id(u64)
```

`PageFrame` допускает `AtomStoreSuperblock` и `GraphSuperblock`. Их логические версии участвуют
в snapshot visibility наравне с обычными страницами, но checkpoint не пишет их поверх home-page
in-place. В одной транзакции допустим не более чем один итоговый superblock frame на data space;
если metadata не менялась, такой frame отсутствует.

`CheckpointEnd` допустим только после хотя бы одного незакрытого `CheckpointBegin`;
`backfill_lsn` ненулевой и не превышает уже durable `TxCommit`-границу. Несколько `CheckpointBegin`
подряд разрешены: так журнал представляет повтор после ошибки backfill до commit point.

**`PageKey = (DataSpaceId, DataFileId, PageNo)`** — ключ индекса версий страниц. `space_id = 0`
адресует database-level AtomStore/catalog, ненулевое значение — соответствующий `GraphId`.
System graph не вводится. Dirty-page table не хранится: recovery строит индекс одним
streaming-проходом, а checkpoint выбирает latest eligible frame из этого индекса.

`PageInit` разрешён только для регистрации **пустой** аллокации страницы после последнего
checkpoint. Он не доказывает и не восстанавливает содержимое. Первая же содержательная мутация
этой страницы обязана записать полный `PageFrame`; заменять им фреймы массовой загрузки запрещено.

### 22.3. Индекс версий страниц

В памяти: `HashMap<PageKey, SmallVec<(FrameLsn, CommitLsn, FrameOffset)>>` — для каждой страницы
список её committed-версий в журнале. Чтение страницы на снимке `S`: взять самый свежий фрейм,
для которого `commit_lsn ≤ S.visible_lsn`; если такого нет — читать `.fgb`. Фреймы без durable
`TxCommit` в индекс видимых версий не входят.

Стоимость — один хеш-поиск на промах буфер-пула. В read-heavy фазе после бэкфилла индекс пуст,
поэтому доминирующая нагрузка не платит ничего. Одна `FrameVersion` format v1 хранит четыре
`u64` (32 байта) плюс overhead `HashMap`/`Vec`: миллион неотбэкфилленных версий требует не менее
≈ 32 МиБ до учёта ключей и capacity.

Индекс восстанавливается при старте одним проходом по журналу и не персистится отдельно.

### 22.4. WAL-first инвариант

1. Изменение страницы идёт в scratch-фрейм; исходная страница в `.fgb` не трогается.
2. Под DB commit lock: дописать фреймы транзакции → `TxCommit` → один buffered `flush` и
   **один** `sync_data` (независимо от числа затронутых графов и файлов).
3. Страница в `.fgb` по физическому адресу `L` записывается **только после** того, как все фреймы этой страницы с `lsn ≤ page_lsn` durable в журнале.
4. Журнал **никогда** не обрезается раньше `fsync` файлов данных.

Пункты 3–4 — это весь протокол долговечности; правило «не писать страницу раньше её WAL-записи» выполняется автоматически, поскольку запись в `.fgb` бывает только при бэкфилле уже durable фреймов.

---

## 23. Checkpoint и Recovery

### 23.1. Checkpoint — бэкфилл фреймов

Checkpoint переносит latest committed frame каждой страницы обратно в `.fgb` **на штатное
место**. В срезе 3b поддерживается только пассивный режим:

1. Полный checkpoint сериализуется отдельным mutex.
2. Под коротким snapshot write-gate атомарно захватываются `durable_visible_lsn` и активные
   readers: `target = min(durable_visible_lsn, oldest_reader_lsn.unwrap_or(durable_visible_lsn))`.
3. Если `target ≤ checkpoint_lsn`, операция завершается без записей.
4. Под WAL writer lock назначается следующая `generation`, добавляется `CheckpointBegin` и
   фиксируется latest complete frame каждой страницы с `commit_lsn ≤ target`.
5. Фреймы читаются в переиспользуемый aligned extent, группируются по `DataFileId` и соседним
   home-страницам; superblock frames из ordinary in-place backfill исключаются; cache/WAL
   metadata locks во время Direct I/O не удерживаются.
6. Каждый затронутый data-файл синхронизируется ровно один раз.
7. Добавляется `CheckpointEnd { generation, backfill_lsn: target }`, затем WAL выполняет
   `flush + sync_data`; LSN этой записи становится `checkpoint_record_lsn`.
8. Для каждого затронутого data space выбирается последняя logical superblock version не новее
   target. В ней меняется только checkpoint tuple, а `PageHeader.page_lsn` сохраняется. Образ
   пишется в `.tmp`, синхронизируется и заменяет main с сохранением прежнего main как `.prev`.
9. Тем же протоколом последним заменяется `db.fgb`. Только успешная замена `db.fgb` является
   commit point checkpoint.
10. Под exclusive prune-gate публикуется новая home generation, из version index удаляются версии
   с `commit_lsn ≤ target` (latest-commit metadata для FCW сохраняется), затем удаляются только
   закрытые WAL-сегменты, чей последний LSN `≤ target`.

Ошибка до замены `db.fgb` не меняет version index и не разрешает prune. Ошибка удаления сегмента
после commit point означает завершённый checkpoint с лишним, но безопасным WAL-префиксом.

`begin_tx` удерживает shared snapshot-gate на короткой границе `snapshot capture + active
registration`, поэтому reader не может потеряться между вычислением watermark и регистрацией.
После захвата `target` checkpoint не ждёт старых читателей и не блокирует регистрацию новых.
Текущая транзакция не пишет WAL до commit, поэтому отдельного
`oldest_active_tx_first_frame` в prune-контракте нет.

Старый reader ограничивает сам backfill: иначе обновлённый home-файл уничтожил бы его fallback-
версию. Обычный `CheckpointMode::Passive` сохраняет непросроченные снимки и отменяет только
достигшие `max_snapshot_age` (`FG-25F01`). Явный `CheckpointMode::Restart` под snapshot write-gate
отменяет все зарегистрированные storage snapshots (`FG-25F02`), фиксирует durable tail как target
и после выхода из gate не мешает регистрации новых readers. Transaction spill остаётся отдельным
этапом 7 и до него транзакция не пишет WAL раньше commit.

**Windows ownership и atomic replace.** Открытая локальная БД удерживает `db.lock` с
`share_mode(0)` весь срок жизни manager; повторное открытие возвращает `LocalDatabaseBusy`
(`FG-51I03`). Суперблок кодируется как одна aligned page в соседний `.tmp`, выполняет `sync_all`,
закрывает handle, сохраняет прежний main как `.prev` и заменяет main через
`MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`. Superblock files не держат
открытого handle между заменами. Unix ownership/replace в срезе 3b —
компилируемый `unsupported`; порт и нативные crash-tests остаются release-gate.

### 23.2. Recovery

Восстановление — один упорядоченный проход по журналу. Отдельных фаз analysis/redo/undo нет, каталога процедур на каждый вид записи нет.

```
1. Эксклюзивно открыть `db.lock`. `db.fgb` main — единственная checkpoint authority. Проверить
   checksum/format/clocks main; `.tmp` удалить или игнорировать и никогда не продвигать.
2. Передать persisted clocks выбранного `db.fgb` как seed в streaming WAL scanner. После
   законного prune первый retained segment может начинаться не с LSN 1; gap после
   `checkpoint_lsn` остаётся fatal.
3. Для `generation > 0` найти совпадающий
   `CheckpointEnd(checkpoint_record_lsn, generation, checkpoint_lsn)`.
4. Одним проходом построить version index, отбросить транзакции без durable `TxCommit` и
   восстановить clocks как максимум persisted seed и WAL tail. LSN, TxId, CommitTs и file
   instance id никогда не переиспользуются.
5. Для каждого AtomStore/graph superblock выбрать максимальный checksummed main/prev candidate,
   чья checkpoint tuple не опережает `db.fgb`; candidate незавершённого checkpoint игнорируется.
   Затем применить committed frames с `frame_lsn > checkpoint_lsn` строго по возрастанию frame LSN.
   Путь берётся только из sealed file registry; отсутствующий id или разрыв файла — corruption.
   Один переиспользуемый aligned buffer ограничивает память recovery.
6. Полный frame перезаписывает torn home page. После replay синхронизировать каждый затронутый
   файл один раз; любая повреждённая home-страница без более нового retained frame даёт
   `InvalidStoragePage`.
7. Отсутствие допустимого main/prev superblock и полного retained WAL frame является typed
   corruption. Выполнить свежий passive checkpoint. До его успешного commit point менеджер не публикуется
   вызывающему коду.
```

**Порядок строго по LSN, а не по коммитам.** Это принципиально. Прежняя редакция буферизовала операции по транзакциям и применяла их в момент `COMMIT`, из-за чего при пересечении двух транзакций на одной странице закоммиченное изменение могло быть **молча потеряно**:

```
lsn 100: tx A → изменение страницы P
lsn 101: tx B → изменение страницы P
lsn 200: COMMIT B → применяем (101 > page_lsn) → page_lsn = 101
lsn 300: COMMIT A → 100 ≤ 101 → ПРОПУЩЕНО, хотя A закоммичена
```

При воспроизведении в порядке LSN этой ситуации не возникает по построению.

**Память.** `O(число фреймов)`, а не `O(объём полезной нагрузки)`: индекс хранит только смещения. Прежняя схема удерживала полные payload'ы (`PropSet` несёт весь сериализованный `PropertyMap`), то есть требовала памяти порядка размера самой большой транзакции.

**Сложность.** `O(|журнал с последнего checkpoint|)`.

### 23.3. Порядок при COMMIT

```
1. Validate (SSI / SI)
2. Взять единый DB commit lock
3. Assign commit_ts (глобальный логический счётчик БД)
4. Merge atom-delta и повторно проверить graph-family invariants
5. WAL: append последовательных фреймов транзакции
6. WAL: append TxCommit { commit_ts, frame_count }; LSN записи = commit_lsn
7. WAL: buffered flush + sync_data       ← один durable barrier на всю транзакцию
8. durable_visible_lsn := commit_lsn; опубликовать commit_ts/root
9. Освободить DB commit lock; бэкфилл — позже, при checkpoint
```

Один `fsync` покрывает транзакцию целиком, сколько бы global atoms, graphs и файлов она ни затронула.

### 23.4. Torn write

```
При чтении страницы из .fgb:
  expected = rapid_hash_v3(page_bytes с обнулённым checksum, seed=0)
  if page_header.checksum != expected || page_header.page_no != ожидаемый:
    → страница повреждена
    → если в журнале есть фрейм этой страницы с lsn > checkpoint_lsn:
        применить фрейм (полный образ) — страница восстановлена
    → иначе:
        MetaGraphError::PageCorrupt
```

Восстановление всегда возможно, пока фрейм не обрезан, потому что **фрейм — это полный образ**, а не дельта. Именно поэтому обрезка журнала обязана следовать строго после `fsync` файлов данных (§22.4, п. 4): нарушение этого порядка — единственный способ получить невосстановимую рваную страницу.

### 23.5. Изоляция и чтение на снимке

Снимок транзакции — пара `StorageSnapshot { commit_ts, visible_lsn }`. `commit_ts` задаёт
логический MVCC-порядок и используется SSI/FCW; `visible_lsn` фиксирует durable-префикс WAL.
Эти величины монотонны на уровне БД, но не сравниваются друг с другом. Разрешение чтения страницы:

```
read_page(PageKey, snapshot):
    frames = page_version_index[PageKey]
    взять самый свежий committed-фрейм с commit_lsn ≤ snapshot.visible_lsn
    если найден → вернуть его образ
    иначе       → прочитать страницу из .fgb
```

Отсюда следуют свойства, которых у модели дельт не было:
- **Снимок консистентен по всей базе**, а не по одному графу: `commit_ts` упорядочивает MVCC,
  а `visible_lsn` одинаково ограничивает durable WAL-версии всех графов.
- **Читатели не блокируют писателя и наоборот**: писатель добавляет новые фреймы, читатель смотрит на префикс журнала.
- **Checkpoint учитывает читателей**: живой снимок удерживает `oldest_reader_visible_lsn`, который
  ограничивает и бэкфилл, и обрезку.

**Checkpoint-stable чтение.** Транзакция, объявленная как читающая только отбэкфилленные данные (`BEGIN READ ONLY` с соответствующим режимом), читает исключительно `.fgb`, **не удерживает ни одного фрейма** и видит состояние на момент последнего checkpoint. Это штатный режим для долгой аналитики: он полностью снимает риск разрастания журнала из-за многоминутного обхода.

---

## 24. Compaction и дефрагментация

### 24.0. Два разных сценария — и только один из них требует ремапа

Проблема ремапа ссылок возникает исключительно потому, что `DiskAtomRef` — это **позиция слота** (§9). Любое физическое перемещение слота обязано переписать все ссылки на него: индексы, overflow-цепочки, инцидентности, changefeed. Но перемещение слотов нужно далеко не всегда.

Различаются два запроса пользователя, и путать их не следует:

| Запрос                                                 | Механизм                                   | Ремап        | Блокирует |
|--------------------------------------------------------|--------------------------------------------|--------------|-----------|
| «Верните место **внутри** файла для переиспользования» | Логический реклейминг через freelist       | **не нужен** | нет       |
| «Сожмите файл **на диске**, верните место ОС»          | Полный дефраг с `slot_remap` + `ftruncate` | нужен        | да        |

**Основной путь — инкрементальный, без ремапа.** По аналогии с `SlotAllocator` освобождение слота логическое: слот помечается tombstone и возвращается во freelist. Страница, у которой доля tombstone-слотов превысила `FG_COMPACTION_TOMBSTONE_RATIO`, становится кандидатом на реклейминг, но **живые слоты на ней не двигаются** — освобождается лишь дыра внутри уже выделенного файла, через постраничный freelist (§15). Файл физически не ужимается, зато новые вставки переиспользуют освободившиеся страницы вместо роста файла. `DiskAtomRef` при этом не меняется вообще, поэтому операция не требует ни ремапа, ни остановки читателей и выполняется в фоне.

Для hot-страниц и кучи свойств (§24.2) это означает переиспользование стабильных физических
слотов. Immutable Label side-files (§24.1) вместо локального reclaim пересобираются поколением.

**Полный дефраг (§24.3) остаётся, но как редкая офлайн-операция** — ровно для второго сценария, когда нужно вернуть место операционной системе. Он требует эксклюзивного доступа и checkpoint'а с обеих сторон: ни один читатель не может удерживать снимок поперёк `slot_remap`.

### 24.1. Label Index Compaction

Stage-7 Label index immutable и tombstone не содержит, поэтому отдельной in-place compaction у
него нет. **Триггер пересборки** — catalog readiness `stale`, отсутствующий/corrupt side-file
либо явный будущий `REBUILD INDEX`.

```
1. Зафиксировать source snapshot.
2. Потоково распределить живые hot slots по (AtomKind, label_id).
3. Записать плотные sorted arrays в новое недостижимое UUID-поколение и fsync файлов.
4. Одним catalog/WAL commit опубликовать поколение и перевести его binding в ready.
5. Старое поколение удалить после завершения удерживающих его snapshots.
```

### 24.2. PropHeap и GraphOverlay compaction

**Триггер**: среднее `free_space / PAGE_SIZE > 40%`.

```
1. Allocate новые страницы в том же `DataSpaceId`.
2. В AtomStore для каждого live `AtomHotSlot` переписать canonical `RecordId` и base
   `PropertyMap`, затем обновить только его `record_id_*`/`properties_*` references.
3. В graph space переписать только live sparse overlay records и обновить
   `NodeHotSlot.overlay_*`/`EdgeHotSlot.overlay_*`; base properties не копировать.
4. Записать изменённые heap/hot pages как обычные frame-WAL `PageFrame` одного commit.
5. Обновить соответствующие freelists, reclaim старые pages и выполнить checkpoint.
```

### 24.3. Полный дефрагментатор (Manual COMPACT)

**Триггер**: `COMPACT GRAPH <name>` или tombstone ratio > 30% в hot файле (много удалений без переиспользования).

```
1. Построить отдельные node/edge `slot_remap` для живых occurrences выбранного graph;
2. Записать новые `nodes_hot.fgb` и `edges_hot.fgb`: переупорядочить occurrence slots, сохранив
   каждый `GlobalAtomId` без изменения;
3. Для каждого EdgeHotSlot:
   - source_inline[], target_inline[] — применить remap к каждому элементу;
   - source_sg_slot, target_sg_slot — это root-arena slot, не DiskAtomRef, не трогать;
4. Применить remap к AdjacencyOverflowPage и CrossLevelAdjacencyOverflowPage (local_ref);
5. NodeHotSlot adjacency (adj_out, adj_in, adj_undir) — это edge slots,
   применить remap если рёбра тоже дефрагментируются
6. EdgeEndpointIndex — перестроить полностью (endpoint_ref и edge_ref оба меняются)
7. Перестроить `membership.fgidx`, class/label и explicit effective indexes этого graph;
   global primary/base indexes не менять.
8. Обновить database `memberships.fgidx`: `GlobalAtomId` остаётся прежним, меняется только
   graph-local `DiskAtomRef` occurrence.
9. SubgraphDirPage — sg_slot это root-arena slot подграфов, не DiskAtomRef;
   если подграфы тоже дефрагментируются — отдельная процедура
10. В новых changefeed entries использовать remapped graph-local refs; уже опубликованная
    history остаётся привязана к своей generation.
11. Обновить GraphSuperblock.
12. fsync всего → atomic rename.
```

Дорогая операция; только по явному запросу или при запуске после длительной эксплуатации по расписанию.

### 24.4. Алгоритм освобождения цепочек страниц.

Цепочки страниц используются в: `PropLargeChunkPage`, `AdjacencyOverflowPage` (и cross-level варианте),
`DictEntryPage`, `SubgraphDirPage`, B+tree страницах (leaf linked list), `PostingPage`, `TermDictPage` , 
`NeighbourhoodListPage`, `LandmarkListPage`, `VectorDataPage`, `ChangefeedPage` и так далее.

Общий алгоритм освобождения цепочки:
```rust
fn free_page_chain(pm: &PageManager, file: DataFileKind, first_page: u64, wal: &WalWriter, tx: TxId) {
    let mut page_no = first_page;
    while page_no != NULL_PAGE {
        let page = pm.read_page(file, page_no);
        let next = read_next_page_ptr(&page);   // offset зависит от типа страницы
        wal.append(tx, WalKind::PageFree, &encode_page_free(file, page_no));
        pm.freelist(file).set_free(page_no);
        page_no = next;
    }
}
```

Для `PropLargeChunkPage` дополнительно:
```rust
fn free_large_object(pm: &PageManager, first_chunk: u64, wal: &WalWriter, tx: TxId) {
    free_page_chain(pm, DataFileKind::Props, first_chunk, wal, tx);
    // После этого обнулить owning reference:
    // AtomHotSlot.record_id_*/properties_* либо NodeHotSlot/EdgeHotSlot.overlay_*
}
```

Для `PropHeapPage` (normal records) освобождение отдельной записи не возвращает страницу в freelist немедленно: 
ставится tombstone в `SlotEntry`, страница помечается dirty. 
Возврат страницы в freelist — только при compaction, когда все `SlotEntry` на странице имеют tombstone.

---

## 25. Адресация и навигация

### 25.1. Формулы адресации

```
Для AtomHotPage (340 слотов × 48 байт):
  page_no          = GlobalAtomId.slot_index / 340
  slot_within_page = GlobalAtomId.slot_index % 340
  byte_offset      = PAGE_HEADER_SIZE + slot_within_page × 48

Для NodeHotPage и EdgeHotPage (102 слота × 160 байт):
  page_no          = slot_index / 102
  slot_within_page = slot_index % 102
  byte_offset      = PAGE_HEADER_SIZE + slot_within_page × 160
```

Арифметическая адресация сохраняется и в дисковом режиме именно потому, что checkpoint возвращает фреймы **на штатные места** (§23.1); это и было причиной выбрать COW-в-логе вместо COW в файле данных.

### 25.2. Чтение смежности

```rust
fn out_neighbors(disk_graph: &DiskMetaGraph, node_slot: u64) -> Vec<DiskAtomRef> {
    let hot = disk_graph.node_hot(node_slot);
    let inline = &hot.adj_out[..min(hot.adj_out_count as usize, 4)];
    let mut result: Vec<_> = inline.iter()
        .take_while(|&&s| s != NULL_SLOT)
        .map(|&&s| DiskAtomRef::edge(s))
        .collect();
    if hot.adj_out_count > 4 {
        // traverse overflow chain
        let mut page_no = hot.overflow_out;
        while page_no != NULL_PAGE {
            let page = disk_graph.pages.read_page(DataFileKind::AdjOverflow, page_no);
            let overflow_page = AdjacencyOverflowPage::parse(&page);
            result.extend(overflow_page.entries[..overflow_page.count].iter().map(|&s| DiskAtomRef::edge(s)));
            page_no = overflow_page.next_page;
        }
    }
    result
}
```

### 25.3. Загрузка в in-memory MetaGraph (Mode 3)

Mode 3 выполняет bounded двухпроходную materialization:

1. Для каждого выбранного node occurrence прочитать `NodeHotSlot`, затем разрешить
   `global_atom → AtomHotSlot → base PropertyMap`. Только при `HAS_OVERLAY` прочитать overlay и
   построить effective properties. Создать новый owning node и сохранить
   `DiskAtomRef → materialized AtomId`.
2. Для каждого выбранного edge occurrence аналогично разрешить base/overlay, remap endpoints
   через таблицу первого прохода и создать owning edge с graph-local topology/weight/label.
3. После завершения bounded окна освободить page epoch; materialized graph не удерживает
   `GlobalAtomId`, source `DiskAtomRef` или page guard.

Unattached global atom не входит в graph materialization, но остаётся доступен прямым class или
`RecordId` read из AtomStore.

### 25.4. Прямое чтение слота (без BufferPool)

```rust
const SLOTS_PER_HOT_PAGE: u64 = 102;
const HOT_SLOT_SIZE:      u64 = 160;

fn hot_page_no(slot: u64) -> u64   { slot / SLOTS_PER_HOT_PAGE }
fn hot_offset(slot: u64)  -> usize { (32 + (slot % SLOTS_PER_HOT_PAGE) * HOT_SLOT_SIZE) as usize }
```

---

## 26. Тензорные представления

Дисковый формат хранения матричных и тензорных представлений графов, описанных в
[gql_spec.md](gql_spec.md) §3.8.4.

### 26.1. Режимы хранения

| Режим          | Описание                               | Применение                     |
|----------------|----------------------------------------|--------------------------------|
| On-the-fly     | Вычисляется из NodeHotPage/EdgeHotPage | По умолчанию — без кеширования |
| Cached dense   | Хранится как бинарная матрица на диске | ML/analytics workloads         |
| Cached sparse  | CSR-формат на диске                    | Большие разреженные            |

### 26.2. TensorCachePage (для cached-режима)

```
PageHeader(32) (magic=MAGIC_TENSOR, type=TensorCache)
  tensor_kind:  u8    -- 0=endpoint_source, 1=endpoint_target
  format:       u8    -- 0=dense_u8, 1=csr_u8
  n_atoms:      u32   -- число строк (atoms)
  n_edges:      u32   -- число столбцов (edges)
  data_pages:   u64   -- первая страница данных
  build_lsn:    u64   -- LSN момента кэширования
```

- Dense-формат: packed bit-matrix, 1 бит на ячейку.
- Страница данных: PageHeader(32) + 16352 байт битовой матрицы = 16352 × 8 = 130816 бит на страницу.

Например, для n_atoms=10000, n_edges=50000: matrix size = 10000 × 50000 / 8 = 62.5 МБ ≈ 3823 страниц.
Только для специализированных ML-сценариев.

### 26.3. Инвалидация кэша

TensorCachePage инвалидируется при любом изменении топологии (NodeAlloc, EdgeAlloc, NodeFree, EdgeFree WAL entries).
Признак инвалидации: `build_lsn < current_checkpoint_lsn`.

---

## 27. Конфигурация

### 27.1. Compile-time константы формата

Следующие параметры определяют бинарный формат NodeHotSlot и EdgeHotSlot.
Они фиксируются при компиляции и не могут быть изменены для уже существующих
файлов хранилища без создания новой базы данных и полной миграции.
Их изменение инкрементирует STORAGE_FORMAT_VERSION.

| Параметр                   | Тип   | Default | Описание                                                |
|----------------------------|-------|---------|---------------------------------------------------------|
| `FG_ADJ_INLINE_OUT`        | usize | 4       | Inline out-adj per node (NodeHotSlot.adj_out[])         |
| `FG_ADJ_INLINE_IN`         | usize | 4       | Inline in-adj per node (NodeHotSlot.adj_in[])           |
| `FG_ADJ_INLINE_UNDIR`      | usize | 2       | Inline undir-adj per node (NodeHotSlot.adj_undir[])     |
| `FG_EDGE_INLINE_ENDPOINTS` | usize | 2       | Inline endpoints per edge side (EdgeHotSlot.*_inline[]) |

### 27.2. Runtime-параметры

| Параметр                             | Тип    | Default   | Описание                                                                     |
|--------------------------------------|--------|-----------|------------------------------------------------------------------------------|
| `FG_PAGE_BUFFER_PAGES`               | usize  | 4096      | Ёмкость BufferPool (4096 × 16 КиБ = 64 МиБ)                                  |
| `FG_WAL_SEGMENT_SIZE`                | u64    | 67108864  | Размер WAL сегмента до ротации (64 МиБ)                                      |
| `FG_CHECKPOINT_DIRTY_THRESHOLD`      | f64    | 0.25      | Checkpoint при ≥ N% dirty pages в pool                                       |
| `FG_CHECKPOINT_INTERVAL_SEC`         | u64    | 300       | Максимальный интервал checkpoint                                             |
| `FG_LABEL_DICT_CACHE_MB`             | usize  | 32        | In-memory LRU кэш label dictionary                                           |
| `FG_PROPS_COMPRESS_THRESHOLD`        | usize  | 512       | Сжимать props если serialized_size > N байт                                  |
| `FG_HNSW_CACHE_SIZE`                 | usize  | 268435456 | HNSW кэш (256 МиБ); §9.6 gql_spec                                            |
| `FG_COMPACTION_TOMBSTONE_RATIO`      | f64    | 0.20      | Инкрементальный реклейминг страницы при tombstone ratio > N                  |
| `FG_COMPACT_HOT_TOMBSTONE_RATIO`     | f64    | 0.30      | Полный defrag при tombstone ratio > N                                        |
| `FG_STATS_AUTO_ANALYZE_DIRTY`        | f64    | 0.10      | Авто-ANALYZE при dirty_fraction > N                                          |
| `FG_ASYNC_EVENT_PROCESSING_INTERVAL` | u64    | 5000      | Интервал обработки async событий (мс)                                        |
| `FG_CHANGEFEED_PURGE_INTERVAL_SEC`   | u64    | 3600      | Интервал очистки старых changefeed файлов                                    |
| `FG_PATH_MAX_DEPTH`                  | usize  | 30        | Максимальная глубина путей                                                   |
| `FG_TRAVERSAL_READ_YOUR_WRITES`      | bool   | false     | Traversal видит собственные мутации                                          |
| `FG_TEMPFILES_PATH`                  | String | ""        | Каталог временных файлов: спиллинг сортировок запросов **и** сборки индексов |

**Журнал и checkpoint** (§22–§23)

| Параметр                    | Тип   | Default    | Описание                                                                                          |
|-----------------------------|-------|------------|---------------------------------------------------------------------------------------------------|
| `FG_TX_WRITE_SET_MAX_BYTES` | u64   | 268435456  | Лимит atom-delta write-set sync MVP (256 МиБ); превышение отклоняет commit                        |
| `FG_WAL_CHECKPOINT_FRAMES`  | usize | 4096       | Порог автоматического запуска бэкфилла                                                            |
| `FG_WAL_MAX_TOTAL_BYTES`    | u64   | 1073741824 | Жёсткий предел размера журнала (1 ГиБ) → принудительный бэкфилл отстающего графа                  |
| `FG_WAL_SYNC_MODE`          | enum  | `full`     | `full` \| `normal` \| `off`                                                                       |
| `FG_MAX_SNAPSHOT_AGE_SEC`   | u64   | 300        | Возраст снимка, после которого он принудительно закрывается, чтобы разблокировать обрезку журнала |

**Сборка индексов и массовая загрузка** (§20.13)

Таблица ниже задаёт целевой runtime-configuration contract. В stage-7 baseline
`FG_BULK_SORT_MEM` и `FG_BULK_SORT_MERGE_FANIN` ещё не читаются из окружения:
действуют фиксированные значения 256 МиБ и 64 из §20.13.1;
`FG_BULK_SORT_PARALLELISM` и fill-factor tuning ещё не подключены.

| Параметр                              | Тип   | Default   | Описание                                                                       |
|---------------------------------------|-------|-----------|--------------------------------------------------------------------------------|
| `FG_BULK_SORT_MEM`                    | usize | 268435456 | Память на прогон внешней сортировки (256 МиБ)                                  |
| `FG_BULK_SORT_MERGE_FANIN`            | usize | 64        | Степень k-путёвого слияния                                                     |
| `FG_BULK_SORT_PARALLELISM`            | usize | ядра      | Потоки генерации прогонов                                                      |
| `FG_INDEX_BTREE_FILL_FACTOR`          | f64   | **0.90**  | Заполнение листьев B+tree (было 0.80); 1.00 автоматически при монотонном ключе |
| `FG_INDEX_BTREE_INTERNAL_FILL_FACTOR` | f64   | 0.95      | Заполнение внутренних узлов B+tree                                             |
| `FG_INDEX_RTREE_FILL_FACTOR`          | f64   | 0.95      | Заполнение узлов R-tree при STR-упаковке                                       |
| `FG_INDEX_PENDING_MAX`                | usize | 1000000   | Порог очереди `DEFER` → индекс `stale` + пересборка                            |
| `FG_HNSW_BUILD_MEM`                   | usize | 2 ГиБ     | Бюджет памяти на построение HNSW (отдельно от кэша обслуживания)               |
| `FG_IMPORT_COMMIT_ROWS`               | usize | 1000000   | Размер сегментного коммита при импорте                                         |
| `FG_IMPORT_MAX_REPORTED_VIOLATIONS`   | usize | 1000      | Сколько нарушений описывается подробно                                         |
| `FG_IMPORT_REJECT_LIMIT`              | usize | 0         | Допустимое число нарушений до прерывания                                       |
| `FG_IMPORT_NO_SLOT_REUSE`             | bool  | true      | Импорт только дописывает → возможен откат усечением                            |
| `FG_BULK_WAL_MODE`                    | enum  | `minimal` | `minimal` \| `full`; `full` станет обязательным при появлении репликации/PITR  |

**Статистика** (§19.5)

| Параметр                       | Тип   | Default | Описание                                                  |
|--------------------------------|-------|---------|-----------------------------------------------------------|
| `FG_STATS_HISTOGRAM_BUCKETS`   | usize | 100     | Число корзин equi-depth гистограммы                       |
| `FG_STATS_SAMPLE_ROWS`         | usize | 30000   | Размер выборки для `ANALYZE`, не зависит от размера class |
| `FG_STATS_FULL_SCAN_THRESHOLD` | usize | 32      | Таблицы меньше N страниц анализируются полным сканом      |

---

## 28. Эволюция формата

### 28.1. Версионирование

`STORAGE_FORMAT_VERSION` хранится в `DatabaseSuperblock` и повторяется в каждом
`AtomStoreSuperblock`/`GraphSuperblock`; несовпадение является corruption.

Пока не пройдены Windows sync performance gate и Windows GA-core gate, значение `1` обозначает
кандидат и не является обещанием замороженной совместимости. После Windows freeze Linux должен
читать и записывать те же bytes; несовместимая адаптация layout выпускается только как format v2
с миграцией. Отсутствие Linux-окружения не задерживает Windows freeze и не позволяет объявлять
Unix-поддержку проверенной.

| Условие                                       | Действие                                                                      |
|-----------------------------------------------|-------------------------------------------------------------------------------|
| `superblock.format_version > ENGINE_VERSION`  | Error: downgrade not supported                                                |
| `superblock.format_version < ENGINE_VERSION`  | Typed incompatible-format; migration для промежуточного candidate отсутствует |
| `superblock.format_version == ENGINE_VERSION` | Open normally                                                                 |

### 28.2. Правила совместимости

- `_reserved` поля и зарезервированные биты: обнулить при записи; reader той же версии обязан
  отклонить ненулевое значение как повреждённый/неподдерживаемый layout. Новое назначение требует
  увеличения `STORAGE_FORMAT_VERSION`.
- Новые `PageType` в страницах: старая версия возвращает `IoError::UnknownPageType` (не panic).
- Новые `WalKind`: старые WAL readers обязаны пропускать неизвестные entries с предупреждением (не panic).
- Новые `TypeTag`: reader format v1 возвращает `UnknownTypeTag { tag, offset }`. Пропуск
  возможен только на уровне внешней length-framed записи; подмена неизвестного значения на
  `Bytes` запрещена.

### 28.3. Migration functions

```rust
fn migrate(graph_dir: &Path, from_version: u32, to_version: u32) -> io::Result<() > {
    match (from_version, to_version) {
        (1, 2) => migrate_v1_to_v2(graph_dir),
        _ => Err(io::Error::new(io::ErrorKind::Unsupported, format!("no migration path {from_version} → {to_version}")))
    }
}
```

### 28.4. Правило: один формат на версию

При необходимости изменить размер слота или структуру страницы:
1. Инкрементировать `STORAGE_FORMAT_VERSION`.
2. Написать migration function: `migrate_v1_to_v2(graph_dir: &Path)`.
3. Migration выполняется при первом открытии новой версией.

---

## 29. Новые Rust-компоненты

> **Вынесено в [implementation_plan.md](implementation_plan.md).** Целевая структура Rust-модулей,
> trait `PageManager`, таблица новых компонентов и зависимости `Cargo.toml` перенесены в план
> реализации: §2 «Целевая структура модулей», §3 «Ключевые Rust-контракты» (§3.3 — действующая
> frame-WAL-редакция `PageManager`, заменившая раннюю STEAL-версию `write_page_dirty`/`flush_*`),
> §3.6 «Новые компоненты», §3.7 «Зависимости».

---

## 30. План доработки кодовой базы

> **Вынесено в [implementation_plan.md](implementation_plan.md).** Пофайловый план доработки
> (`value.rs`, `serial.rs`, `id.rs`, `mvcc_persist.rs`, `mvcc.rs`, `store.rs`, `graph.rs`),
> приоритизированный порядок работ и правки полной инцидентности графового ядра перенесены в план:
> §4 «План реализации по областям» (пофайловая детализация — Области A–D, N) и §5
> «Приоритизированный порядок работ».
>
> Часть ранней редакции плана отменена принятыми решениями и в план не переносилась дословно:
> `WalKind 0x10–0x22` и раздельные `MvccWal`/`DiskWal` → единый **frame-WAL** уровня БД (§22–§23);
> `DiskAtomRef = u64` (§9). Актуализированные формулировки — в таблице решений в начале плана
> (помечены 🔄).

---
