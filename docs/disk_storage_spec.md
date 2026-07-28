# Ferros-Grynn — Спецификация дискового хранилища

**Версия:** 0.1
**Статус:** проектирование

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
9. [DiskAtomRef — стабильный дисковый идентификатор](#9-diskatomref--стабильный-дисковый-идентификатор)
10. [NodeHotPage — горячие данные вершин](#10-nodehotpage--горячие-данные-вершин)
11. [EdgeHotPage — горячие данные рёбер](#11-edgehotpage--горячие-данные-рёбер)
12. [AdjacencyOverflowPage](#12-adjacencyoverflowpage)
13. [PropHeapPage — куча свойств](#13-propheappage--куча-свойств)
14. [SubgraphDirPage — директория подграфов](#14-subgraphdirpage--директория-подграфов)
15. [FreelistPage — управление свободными страницами](#15-freelistpage--управление-свободными-страницами)
16. [LabelDictionary — интернирование строк](#16-labeldictionary--интернирование-строк)
17. [GraphSuperblock — суперблок графа](#17-graphsuperblock--суперблок-графа)
18. [SchemaCatalog — хранилище схем](#18-schemacatalog--хранилище-схем)
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

- Поддержать **Режим 2** (`gql_tech_spec.md §2.2`): граф не помещается в RAM; горячие страницы в Clock-Pro buffer pool.
- Поддержать **Режим 3** (гибридный): загрузка подграфа из диска в in-memory MetaGraph через `query_to_memory`.
- Обеспечить хранение **полной системы типов GQL** (`gql_spec.md §5`): все примитивы, составные типы, доменные типы.
- Обеспечить хранение **схемы** (таблицы, поля, индексы, события, функции, анализаторы, параметры).
- Обеспечить хранение **статистики** для cost model планировщика (степени, гистограммы, selectivity).
- Поддержать **все виды индексов**: standard, unique, count, fulltext (BM25), HNSW vector, path, reachability,
  neighbourhood.
- Поддержать **changefeed** и инфраструктуру live-запросов.
- Сохранить **SoA-дизайн** кодовой базы: горячие данные обхода (`flags`, `kinds`, `adjacency`) — отдельно от холодных (
  `props`).
- Обеспечить **Direct I/O** через существующий `DirectFile` без промежуточного page-cache ОС.

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
| Direct I/O                      | Все файлы открываются с O_DIRECT/F_NOCACHE/FILE_FLAG_NO_BUFFERING                 |
| Атомарность суперблоков         | Запись через tmp + rename                                                         |
| Стабильный дисковый ID          | `DiskAtomRef = u64` (kind-бит + 40-битный slot-индекс, без поколения)             |
| RecordId                        | Публичный идентификатор записи отделён от внутреннего AtomId                      |

### 1.3. Что требует изменения в кодовой базе

Принципиально: `value.rs`, `serial.rs`, `mvcc_persist.rs`, `id.rs` не удовлетворяют полной системе типов GQL.
Детальный план — в §29.

### 1.4. Что остаётся без изменений

- `PAGE_SIZE = 16384` — используется без изменений.
- `DirectFile`, `BufferPool` (Clock-Pro), `StorageBackend` — используются напрямую.
- `store.rs` — по-прежнему управляет именованными графами и WAL Mode 1.
- `AtomId` — без изменений; поколение (`gen`) хранится в hot-слоте и восстанавливается при загрузке.

---

## 2. Режимы хранилища

```
Mode 1: Full In-Memory                          (текущее, без изменений)
  MetaGraph (in-memory SoA)
  ↓ checkpoint
  WAL → Snapshot (.fgr + .wal через store.rs)
  ↓ recovery
  load snapshot → replay WAL

Mode 2: Disk-Backed                             (ЭТА СПЕЦИФИКАЦИЯ)
  DiskMetaGraph
    ├── superblock.fgb      (GraphSuperblock)
    ├── nodes_hot.fgb       (NodeHotPage × N)
    ├── edges_hot.fgb       (EdgeHotPage × N)
    ├── adj_over.fgb        (AdjacencyOverflowPage)
    ├── adj_over_cl.fgb     (CrossLevelAdjacencyOverflowPage)
    ├── props.fgb           (PropHeapPage)
    ├── subgraphs.fgb       (SubgraphDirPage)
    ├── labels.fgb          (LabelDictionary)
    ├── freelist_*.fgb      (FreelistPage per file)
    ├── stats/              (StatisticsStore)
    ├── idx/                (Index files)
    ├── changefeed/         (Changefeed logs)
    └── wal/                (WAL segments)

Mode 3: Hybrid                                  (поверх Mode 2)
  DiskMetaGraph (disk backend)
  ↓ query_to_memory
  MemoryGraphHandle { inner: MetaGraph, source: DiskMetaGraph, dirty_set, snapshot_ts }
  ↓ flush
  DiskMetaGraph (записать dirty_set)
```

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
| 0x10 | `record_id`                 | RecordId (table:id)                      | 1+переменная               |
| 0x11 | `array<T>`                  | Типизированный массив                    | 1+4+Σ(elements)            |
| 0x12 | `set<T>`                    | Множество уникальных                     | 1+4+Σ(elements)            |
| 0x13 | `object`                    | Map ключ→значение                        | 1+4+Σ(key_id+value)        |
| 0x14 | `option<T>`                 | Some(T) или None                         | 1+[value]                  |
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
| 0x36 | `atom_ref`                  | disk_atom_ref(u32 LE)                    | 1 + 4 байт                 |
| 0x37 | `graph_ref`                 | sg_slot(u32) + sg_gen(u32)               | 1 + 8 байт                 |
| 0x38 | `qualified_atom_ref`        | sg_slot(u32 LE) + local_slot(u32 LE)     | 1 + 8 байт                 |

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

Крейт: `rust_decimal 1.42` (пин на минорную версию: в `master` идёт несовместимая разработка 2.0). Публичный API движка не должен раскрывать типы крейта наружу — тонкий newtype оставляет замену дешёвой.

### 3.4. DateTime и Duration

**DateTime** хранится как:

- `seconds: i64` — Unix timestamp в секундах (UTC).
- `nanos: u32` — субсекундная точность `[0 .. 999_999_999]`.
- `tz_id: u32`  -- кодирование часового пояса (см. ниже)

Кодирование tz_id (u32):
- `0` = UTC (по умолчанию)
- `1 .. 999_999` = ID пояса в IANA timezone dictionary (§16, отдельная секция).
Хватит на любые пополнения tzdb (2024 год: ~600 поясов).
- `1_000_000 ..1_172_798` = Фиксированное UTC-смещение.
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
Не добавляют накладных расходов: тип хранится в одном байте тега, данные — в u32 слоте.

| Тег  | GQL тип          | Описание                              | Размер на диске |
|------|------------------|---------------------------------------|-----------------|
| 0x36 | `atom`           | Ссылка на любой атом (vertex \| edge) | 1 + 4 байт      |
| 0x37 | `graph`          | Ссылка на подграф                     | 1 + 8 байт      |

Кодирование:
- `atom → tag(u8) + DiskAtomRef(u64)`
- `graph → tag(u8) + sg_slot(u32) + sg_gen(u32)`

**Семантика**: тег кодирует тип (атом или граф), разделение атомов происходит на уровне валидации схемы по kind-биту; 
generic-параметр T (например, `vertex<Person>`) является schema-level ограничением; проверяемым при записи, и не хранится в значении — только в FieldDefinition (§18.4).

#### 3.8.2. Runtime / interpretation-time типы (не хранимые)

| GQL тип          | Причина                                                                |
|------------------|------------------------------------------------------------------------|
| `metavertex<T?>` | Хранится как обычная вершина с NodeHotSlot.kind=Meta; тип=schema       |
| `metaedge<T?>`   | Хранится как ребро с EdgeHotSlot.inc_page != NULL_PAGE                 |
| `path`           | Результат запроса; не хранится как значение свойства                   |
| graph subtypes   | Интерпретационное ограничение на граф; не самостоятельный тип хранения |

Примечание: `metavertex` и `metaedge` — не отдельные хранимые типы. 
Они отличаются от обычных vertex/edge только флагом `is_meta` в hot-слоте и наличием subgraph_slot / inc_page. 
Пользовательский код работает с ними через те же теги 0x36/0x37; meta-семантика определяется схемой и флагами.

### 3.9. QualifiedAtomRef — составной идентификатор кросс-уровневого атома

Когда endpoint ребра принадлежит вложенному подграфу, простого `DiskAtomRef (u32)` недостаточно — нужно указать и подграф.

```rust
// DiskAtomRef — для endpoints в том же графе (u32)
// QualifiedAtomRef — для cross-level endpoints
struct QualifiedAtomRef {
    sg_slot:    u32,   // GraphId.index() подграфа, содержащего атом
    local_slot: u32,   // DiskAtomRef — slot в координатах подграфа
}
```

Обычная ссылка `atom_ref` не изменяется — используется для атомов в том же графе; `qualified_atom_ref` — явный маркер кросс-уровневого адреса.

При чтении `EdgeHotSlot.inv_inline[i]`:
- Если bit 31 = 0 (DiskAtomRef.kind=Node) или 1 (kind=Edge) без флага `CROSS_LEVEL` — обычный DiskAtomRef.
- Если `EdgeHotSlot.flags.CROSS_LEVEL = 1` — inv/out inline хранят QualifiedAtomRef своих endpoint'ов.

---

## 4. RecordId — публичный идентификатор

`RecordId` — публичный идентификатор записи вида `table:id`.
Он строго отделён от внутреннего `AtomId` (генерационного u64).

### 4.1. Структура

```rust
struct RecordId {
    table: TableId,   // u32: интернированное имя таблицы
    id: RecordIdPart,
}

enum RecordIdPart {
    Int(i64),
    String(Box<str>),
    Uuid(u128),           // UUIDv7
    Ulid(u128),           // ULID
    Array(Vec<Value>),    // composite key
    Object(PropertyMap),  // object key
}
```

### 4.2. Кодирование RecordId на диске

```
tag(u8) = 0x10
table_id(u32)
id_kind(u8):
  0x00 = Int
  0x01 = String
  0x02 = Uuid
  0x03 = Ulid
  0x04 = Array
  0x05 = Object
[id_bytes: variable encoding per kind]
```

### 4.3. Отображение RecordId ↔ AtomId

Каждая запись в таблице имеет:

- `RecordId` — публичный (хранится как свойство `id` в PropHeapPage).
- `AtomId` — внутренний (slot + generation, только in-memory).
- `DiskAtomRef` — стабильный дисковый (slot без generation, u32).

Маппинг `RecordId → DiskAtomRef` хранится в B+tree индексе
(`idx/pk_<table_id>.fgidx`), который является **первичным индексом** каждой таблицы.

### 4.4. Генерация ID по умолчанию

При `CREATE person CONTENT {...}` без явного id:

- Генерируется ULID (криптографически случайный, монотонный по времени).
- `rand::id()` генерирует алфавитно-цифровую строку 20 символов.
- `uuid()` генерирует UUIDv7 (time-ordered).

---

## 5. Бинарное кодирование значений

Единый формат кодирования для **PropHeapPage**, **WAL payload**, **serial.rs snapshot**, **индексов**.

### 5.1. Принципы

- Первый байт — **тег типа** (TypeTag, см. §3.1).
- Числа без знака кодируются **LEB128 unsigned** (переменная длина).
- Числа со знаком — **LEB128 zigzag**.
- f64 — 8 байт little-endian.
- Строки — `len(LEB128) + UTF-8 bytes` (без null-terminator).
- Коллекции — `count(LEB128) + [elements]`.

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
record_id           →  0x10 + table_id(u32 LE) + id_kind(u8) + [id_payload]
array               →  0x11 + count(uLEB128) + [Value]×count
set                 →  0x12 + count(uLEB128) + [Value sorted]×count
object              →  0x13 + count(uLEB128) + [(key_id u32 LE + Value)]×count
option              →  0x14 + 0x00 (none) | 0x15 + 0x01 + Value
range               →  0x15 + flags(u8) + [lo: Value] + [hi: Value]
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
atom_ref            →  0x36 + disk_atom_ref(u32)
graph_ref           →  0x37 + sg_slot(u32) + sg_gen(u32)
qualified_atom_ref  →  0x38 + sg_slot(u32 LE) + local_slot(u32 LE)
```

### 5.3. Кодирование object-ключей

В `object` ключи хранятся как `key_id (u32)` из `LabelDictionary` (§16).
Это устраняет повторение строк-ключей в PropHeapPage.

---

## 6. Константы и магические числа

```
PAGE_SIZE             = 16384     -- байт; совпадает с io/mod.rs
PAGE_HEADER_SIZE      = 32        -- байт у всех страниц
PAGE_PAYLOAD_SIZE     = 16352     -- PAGE_SIZE - PAGE_HEADER_SIZE

NULL_PAGE             = 0xFFFF_FFFF_FFFF_FFFF  -- u64::MAX: нет страницы
NULL_SLOT             = 0xFFFF_FFFF            -- u32::MAX: нет слота
NULL_LABEL            = 0xFFFF_FFFF            -- u32::MAX: нет метки
NULL_TABLE            = 0xFFFF_FFFF            -- u32::MAX: нет таблицы

-- Магические числа страниц (4 байта, little-endian в поле header.magic)
MAGIC_NODE_HOT        = 0x01_4E_48_46   -- b"FHN\x01"
MAGIC_EDGE_HOT        = 0x01_45_48_46   -- b"FHE\x01"
MAGIC_ADJ_OVER        = 0x01_4F_41_46   -- b"FAO\x01"
MAGIC_ADJ_OVER_CL     = 0x01_43_41_46   -- b"FAC\x01"
MAGIC_EDGE_INC        = 0x01_49_45_46   -- b"FEI\x01"
MAGIC_EDGE_INC_CL     = 0x01_43_45_46   -- b"FEC\x01"
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

-- Версия формата
STORAGE_FORMAT_VERSION = 1
```

---

## 7. Файловая организация

```
<store_dir>/
  store.fgb                             -- глобальный суперблок хранилища (engine version, magic)
  <namespace>/
    ns.fgb                              -- суперблок namespace
    <database>/
      db.fgb                            -- суперблок БД: format_version, last_back_compat_version,
                                        --               глобальные LSN/TxId/CommitTs, каталог графов
      wal/
        <lsn_hex>.wal                   -- ЕДИНЫЙ журнал уровня БД (frame-WAL, §22)
      snapshot/
        <lsn_hex>.snap                  -- снимки MVCC (режим 1)
      mvcc.chk                          -- CheckpointHeader
      schema/
        catalog.fgb                   -- SchemaCatalog root
        tables.fgb                    -- DEFINE TABLE definitions
        fields.fgb                    -- DEFINE FIELD definitions
        indexes.fgb                   -- DEFINE INDEX metadata
        events.fgb                    -- DEFINE EVENT definitions
        functions.fgb                 -- DEFINE FUNCTION bodies
        analyzers.fgb                 -- DEFINE ANALYZER definitions
        params.fgb                    -- DEFINE PARAM values
      <graph_name>/
        superblock.fgb                  -- GraphSuperblock (page 0 этого пространства)
        nodes_hot.fgb                   -- NodeHotPage: flags, kind, gen, label_id, adjacency inline
        edges_hot.fgb                   -- EdgeHotPage: topo, flags, gen, label_id, weight, endpoints inline
        adj_over.fgb                    -- AdjacencyOverflowPage: для вершин с degree > 16/8
        adj_over_cl.fgb                 -- CrossLevelAdjacencyOverflowPage
        edge_inc_over.fgb               -- EdgeIncidencePage: для атомов с degree > 16/8
        edge_inc_over_cl.fgb            -- CrossLevelEdgeIncidencePage
        props.fgb                       -- PropHeapPage: PropertyMap в slotted-page layout
        subgraphs.fgb                   -- SubgraphDirPage: арена подграфов
        labels.fgb                      -- LabelDictionary: node/edge labels + prop keys
        freelist_nodes.fgb              -- FreelistPage для nodes_hot.fgb
        freelist_edges.fgb              -- FreelistPage для edges_hot.fgb
        freelist_props.fgb              -- FreelistPage для props.fgb
        freelist_adj.fgb                -- FreelistPage для adj_over.fgb
        freelist_adj_cl.fgb             -- FreelistPage для adj_over_cl.fgb
        freelist_edge_inc.fgb           -- FreelistPage для edge_inc_over.fgb
        freelist_edge_inc_cl.fgb        -- FreelistPage для edge_inc_over_cl.fgb
        freelist_subgraphs.fgb          -- FreelistPage для subgraphs.fgb
        stats/
          graph_stats.fgb               -- node/edge counts, degree histograms
          label_stats.fgb               -- per-label cardinality
          prop_stats.fgb                -- per-property histograms + selectivity
        idx/
          pk_<table_id>.fgidx           -- primary B+tree (RecordId → DiskAtomRef)
          lbl_node_<id>.fgidx           -- label index (nodes)
          lbl_edge_<id>.fgidx           -- label index (edges)
          std_<name>.fgidx              -- standard B+tree property index
          uniq_<name>.fgidx             -- unique B+tree property index
          cnt_<table_id>.fgidx          -- count index
          ft_<label_id>_<key_id>.fgidx  -- fulltext inverted index
          vec_<label_id>_<key_id>.fgidx -- HNSW vector index
          geo_<name>.fgidx              -- geometry R-tree index
          reach_<name>.fgidx            -- reachability index
          nbh_<name>.fgidx              -- neighbourhood index
          ee_endpoint.fgidx             -- edge endpoint index
          freelists/
            freelist_pk_<table_id>.fgb
            freelist_std_<name>.fgb
            freelist_uniq_<name>.fgb
            freelist_ft_<id>.fgb
            freelist_geo_<name>.fgb
            freelist_ee_endpoint.fgb
        changefeed/
          <table_id>_<since_lsn>.fcf    -- changefeed log segments
```

### 7.1. Домен журналирования — база данных, а не граф

Журнал (`wal/`), снимки и `CheckpointHeader` лежат на уровне **базы данных**, а не отдельного графа. Это следствие двух независимых требований.

**1. Глобальные часы обязательны для кросс-граф чтения.** Видимость версии — `version.commit_ts ≤ snapshot_ts`. Если бы `commit_ts` выдавался локально для графа, счётчики двух графов были бы попарно **несравнимы**, и предикат видимости для версии из другого графа не имел бы смысла. Кросс-граф чтение (`MATCH ... FROM g1, g2`, `BUILD GRAPH x FROM MATCH ... FROM y`) — штатная возможность языка, поэтому глобальные `LSN` / `TxId` / `CommitTs` нужны в любом случае. А раз единая точка сериализации уже оплачена, per-graph журналы не дают ничего — они лишь отбирают атомарность, которая при едином журнале достаётся бесплатно.

**2. Каталоги уровня БД не помещаются ни в один per-graph журнал.** `schema/*`, список графов и сам `db.fgb` общие для всех графов. `DEFINE GRAPH`, `BUILD GRAPH <name>`, `REMOVE GRAPH` создают или удаляют каталог графа и правят `db.fgb`: в момент создания журнала графа ещё нет, в момент удаления — уже нет. Атомарно это выразимо только в журнале уровня БД.

Следствия:
- Транзакция атомарна над любым числом графов и таблиц одной БД; один коммит — один `fsync` независимо от числа затронутых графов ([gql_spec §17.7](gql_spec.md)).
- Транзакция **не может** охватывать две БД или два namespace — отвергается статически.
- `SYSTEM_GRAPH_ID = 0` — системное пространство: каталог схемы, каталог графов, параметры, функции и таблицы, объявленные без указания графа.
- Бэкфилл (перенос фреймов в `.fgb`) остаётся **пографовым** и планируется независимо; глобальной является только обрезка журнала (§23.1).
- Отделение графа в самостоятельный набор файлов (`DETACH GRAPH`) требует предварительного бэкфилла до состояния покоя — это административная операция, не бесплатная.

### 7.2. Каталог графов

`db.fgb` содержит не только счётчик `graph_count`, но и сам список графов: `(graph_id: u64, name: [u8; 128], flags: u32, created_lsn: u64)`. Создание и удаление графа журналируются записями `GraphCreate` / `GraphDrop` (§22.1).

Запись `GraphDrop` обязательна: при восстановлении фреймы, адресующие удалённый `graph_id`, должны отбрасываться, иначе воспроизведение упрётся в отсутствующий файл. Физическое удаление каталога графа откладывается до момента, когда запись `GraphDrop` durable и старше самого раннего живого читателя.

**Расширения файлов:**

- `.fgb` — Ferros-Grynn Binary (данные хранилища).
- `.fgidx` — Ferros-Grynn Index.
- `.wal` — Write-Ahead Log.
- `.snap` — Snapshot.
- `.fcf` — Ferros-Grynn Changefeed.
- `.chk` — Checkpoint header.

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
0       32    -     PageHeader (magic=MAGIC_SUPERBLOCK, type=DatabseSuperblock)
32      32    [u8]  db_name                     -- имя database, макс. 32 символа в ASCII кодировке
64      4     u32   format_version              -- версия формата хранилища (§6: STORAGE_FORMAT_VERSION)
68      4     u32   last_back_compat_version    -- последняя версия, совместимая с текущей
72      4     u32   graph_count                 -- количество графов в БД
```

## 8. Универсальный заголовок страницы

Все страницы всех файлов начинаются с идентичного 32-байтового заголовка.

```
PageHeader:
Offset  Size  Тип   Поле        Описание
0       4     u32   magic       Тип страницы (§6)
4       2     u16   page_type   Тип страницы
6       2     u16   flags       Per-page flags
8       8     u64   page_lsn    LSN последней успешной записи на страницу
16      8     u64   checksum    xxHash3-64 всего PAGE_SIZE (с checksum=0)
24      8     u64   page_no     Самоссылка (для обнаружения torn write)
Total = 32 байт
```

**PageType:**

```
0x0001  StorageSuperblock             
0x0002  NamespaceSuperblock             
0x0003  DatabseSuperblock             
0x0004  NodeHot             
0x0005  EdgeHot             
0x0006  AdjacencyOverflow
0x0007  CrossLevelAdjacencyOverflow
0x0008  EdgeIncidence
0x0009  CrossLevelEdgeIncidence
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
0x0027  IndexReachabilitTreeCover
0x0028  IndexReachabilitGrail
0x0029  IndexReachabilitFerrari
0x002A  IndexReachabilitBfl
0x002B  IndexReachabilityLandmarkList
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
```

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
            OR xxhash3(bytes with checksum=0) != stored_checksum
         → страница повреждена → REDO из WAL
```

---

## 9. DiskAtomRef — стабильный дисковый идентификатор

`AtomId` (u64 из `id.rs`) содержит поколение (gen) — runtime-концепция для генерационной инвалидации.
На диске поколение избыточно: оно хранится внутри hot-слота и восстанавливается при загрузке.

### 9.1. Разрядность

**AtomId = u64**

```
биты 63..41   gen         u23  (8 388 608 поколений на слот)
бит  40       kind        0=Node, 1=Edge
биты 39..0    slot_index  u40  (1 099 511 627 776 слотов на каждый kind)
```

**DiskAtomRef = u64** — то же, без поколения:

```
бит  40       kind        0=Node, 1=Edge
биты 39..0    slot_index  u40
биты 63..41   зарезервированы, при записи нули
```

Null-значения: `NULL_DISKATOMREF = u64::MAX`, `NULL_SLOT = 0xFF_FFFF_FFFF` (все 40 бит slot_index).

**Почему u40, а не u31.** Прежний `u31` давал потолок ~2.1 млрд атомов каждого вида на граф — мало для движка, заявленного как универсальный метаграфовый на масштабе. u40 даёт ~1.1 трлн, что заведомо перекрывает любой граф, помещающийся на одну машину, и при этом `AtomId` остаётся ровно `u64`: расширение до `u128` удвоило бы размер handle и ударило по кэш-локальности SoA — той самой, ради которой выбрана раскладка «горячее отдельно от холодного».

**Адресация не переполняется.** При `slot_index` до 2⁴⁰ номер страницы `page_no = slot / 102` не превосходит ≈1.08·10¹⁰, что укладывается в `u64` с запасом (≈285 ТБ при 16 КиБ на страницу). Пространства имён `label_id`, `table_id`, `key_id`, `sg_slot` остаются `u32` — они нумеруют метки, таблицы, ключи и подграфы, а не атомы, и с числом атомов не связаны.

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

## 10. NodeHotPage — горячие данные вершин

### 10.1. NodeHotSlot (160 байт, фиксированный)

```
Offset  Size  Тип        Поле             Описание
0       4     u32        gen              Поколение для восстановления AtomId
4       4     u32        subgraph_slot    GraphId.index() если is_meta; NULL_SLOT иначе
8       4     u32        label_id         Интернированный id метки; NULL_LABEL = нет
12      4     u32        table_id         Таблица, которой принадлежит запись; NULL_TABLE = нет
16      4     u32        adj_out_count    Общее кол-во исходящих рёбер (inline + overflow)
20      4     u32        adj_in_count     Общее кол-во входящих рёбер
24      4     u32        adj_undir_count  Общее кол-во неориентированных рёбер
28      1     u8         kind_flags       Биты: [7]=is_meta; [6]=tombstone; [5]=visited;
                                          [4]=port; [3]=in_result; [2]=cut_vertex; [1]=reserved
29      3     u8[3]      _pad
32      8     u64        overflow_out     page_no overflow для out; NULL_PAGE = нет
40      8     u64        overflow_in      page_no overflow для in
48      8     u64        overflow_undir   page_no overflow для undirected
56      8     u64        props_page       page_no в props.fgb; NULL_PAGE = нет
64      4     u32        props_slot       Индекс слота в SlotDirectory PropHeapPage
68      4     u32        props_len        Длина сериализованного PropertyMap
72      8     —          _reserved        Выравнивание массивов смежности на 16 байт
80      32    [u64; 4]   adj_out          Inline исходящие: DiskAtomRef рёбер; NULL_SLOT = конец
112     32    [u64; 4]   adj_in           Inline входящие
144     16    [u64; 2]   adj_undir        Inline неориентированные
Total = 160 байт
```

**Почему инлайн 4, а не 16.** Прежние `[u32;16]` были перенесены из in-memory представления (`SmallVec<[u32;16]>`), где размер подобран под кэш-линию. Для диска оптимум другой: степенны́е распределения означают, что у подавляющего большинства вершин степень 1–3, поэтому при инлайне 16 бо́льшая часть слота — пустые `NULL_SLOT`, а хабы всё равно уходят в overflow. Инлайн 4 покрывает типичную вершину целиком и втрое уплотняет страницу (102 слота вместо 63) при том же поведении на хабах.

**Почему счётчики u32, а не u16.** `u16` ограничивал степень вершины 65 535, что противоречит §12, где разбирается пример со степенью 100 000, и заведомо мало для хабов.

**Инварианты:**

- Если `kind_flags.tombstone = 1`, остальные поля не гарантированы (кроме `gen`).
- Если `adj_out_count > 4`, первая overflow-страница: `overflow_out`.
- `adj_out[]` заполняется с индекса 0; `NULL_SLOT` — конец inline части.
- `table_id` используется планировщиком для быстрой фильтрации по таблице.

### 10.2. NodeHotPage layout

```
Offset   Size   Содержимое
0        32     PageHeader (magic=MAGIC_NODE_HOT, type=NodeHot)
32       16320  102 × NodeHotSlot (102 × 160 = 16320)
16352    32     _padding
Total = PAGE_SIZE байт
```

**Слотов на страницу:** 102  
**Покрытие:** NodeHotPage[k] содержит слоты `[102k .. 102k+101]`.

**Адресация:**

```
page_no          = slot_index / 102
slot_within_page = slot_index % 102
byte_offset      = 32 + slot_within_page × 160
```

---

## 11. EdgeHotPage — горячие данные рёбер

### 11.1. EdgeHotSlot (160 байт, фиксированный)

```
Offset  Size  Тип        Поле             Описание
0       1     u8         topo             EdgeTopology: 0=Directed,1=Undirected,
                                          2=Bidirectional,3=HyperDirected,
                                          4=HyperUndirected,5=Loop
1       1     u8         flags            EdgeFlags: 
                                            bit 0: TOMBSTONE
                                            bit 1: VISITED
                                            bit 2: META          -- = HAS_TRANSITION | HAS_INCIDENCES
                                            bit 3: MULTI
                                            bit 4: BRIDGE
                                            bit 5: IN_RESULT
                                            bit 6: CROSS_LEVEL   -- endpoint в другом подграфе
                                            bit 7: _reserved
2       1     u8         edge_kind_ext    Расширенная классификация:
                                            bit 0: HAS_TRANSITION   -- transition_gid != NULL_SLOT
                                            bit 1: HAS_INCIDENCES   -- inc_page != NULL_PAGE
                                            bits 2-7: reserved (0)
3       1     u8         _pad
4       4     u32        gen              Поколение
8       4     u32        inv_count        Кол-во invertex конечных точек (total, ≤2 inline)
12      4     u32        out_count        Кол-во outvertex конечных точек (total, ≤2 inline)
16      4     u32        label_id         Интернированный id метки; NULL_LABEL = нет
20      4     u32        table_id         Таблица (для RELATION-таблиц)
24      8     f64        weight           NaN = нет веса
32      4     u32        transition_gid   GraphId.index() для MetaEdge; NULL_SLOT = нет
36      4     u32        inv_sg_slot      GraphId.index() для inv_inline[0]; NULL_SLOT = тот же граф
40      4     u32        out_sg_slot      GraphId.index() для out_inline[0]; NULL_SLOT = тот же граф
44      4     u32        props_slot       Индекс слота в SlotDirectory PropHeapPage
48      4     u32        props_len        Длина сериализованного PropertyMap
52      4     u32        _pad
56      8     u64        props_page       page_no в props.fgb
64      8     u64        inv_overflow     page_no overflow для invertex
72      8     u64        out_overflow     page_no overflow для outvertex
80      8     u64        inc_page         page_no EdgeIncidence list; NULL_PAGE = нет
88      8     u64        inv_sg_overflow  page_no overflow для inv_sg_slot
96      8     u64        out_sg_overflow  page_no overflow для out_sg_slot
104     8     —          _reserved        Выравнивание массивов endpoint'ов на 16 байт
112     16    [u64; 2]   inv_inline       Inline invertex: DiskAtomRef
128     16    [u64; 2]   out_inline       Inline outvertex: DiskAtomRef
144     16    —          _reserved        Зарезервировано под эволюцию формата; обнулить при записи
Total = 160 байт
```

**Почему инлайн 2.** Бинарное ребро — подавляющее большинство — имеет ровно `inv_count = 1` и `out_count = 1`, поэтому двух inline-ячеек достаточно с запасом; гиперрёбра уходят в overflow в любом случае. Инлайн 4 при 8-байтовых ссылках стоил бы лишних 32 байта на каждое ребро ради редкого случая.

**Почему счётчики u32, а не u8.** Прежний `u8` ограничивал гиперребро 255 конечными точками; для гиперграфов это произвольное и слишком низкое ограничение.

**Инварианты:**
- При `topo ∈ {Directed, Undirected, Bidirectional, Loop}`: `inv_count + out_count = 2`.
- При `topo ∈ {HyperDirected, HyperUndirected}`: `inv_count + out_count ≥ 2`.
- При `topo ∈ {Undirected, HyperUndirected}`: все вершины в `inv_inline`; `out_count = 0`.
- При `inv_count > 2` или `out_count > 2` — соответствующий overflow активен.
- `inc_page != NULL_PAGE` означает MetaEdge (edge-to-edge инциденции в отдельной странице).

**Хранение cross-level endpoints:**
- Для CROSS_LEVEL=0: inv_inline/out_inline содержат DiskAtomRef (slot в пространстве текущего графа) - стандартное поведение.
- Для CROSS_LEVEL=1: inv_inline/out_inline содержат DiskAtomRef атомов **в пространстве подграфа, которому они принадлежат**. Чтобы получить полный QualifiedAtomRef, нужен sg_slot их подграфа.
  inv_sg_slot/out_sg_slot покрывают простейший случай: одно inv и одно out с разными уровнями.
  Для гиперрёбер с несколькими cross-level endpoints — полные QualifiedAtomRef хранятся в overflow-странице inv_sg_overflow/out_sg_overflow (CrossLevelAdjacencyOverflowPage).

### 11.2. EdgeHotPage layout

```
Offset   Size   Содержимое
0        32     PageHeader (magic=MAGIC_EDGE_HOT, type=EdgeHot)
32       16320  102 × EdgeHotSlot (102 × 160 = 16320)
16352    32     _padding
Total = PAGE_SIZE байт
```

**Слотов на страницу:** 102

**Адресация:**

```
page_no          = slot_index / 102
slot_within_page = slot_index % 102
byte_offset      = 32 + slot_within_page × 160
```

Слоты вершин и рёбер имеют одинаковый размер (160 байт) и одинаковую плотность (102 на страницу), поэтому арифметика адресации у них общая — это упрощает и код, и дефрагментацию.

### 11.3. EdgeIncidencePage

Для MetaEdge (edge-to-edge инциденции).

```
PageHeader: (magic=MAGIC_EDGE_INC, type=EdgeIncidence)
```

Формат AdjacencyOverflowPage (§12), но записи имеют другой формат:

```
entry (8 байт): peer_disk_ref(u32) + role(u8) + _pad(3)
```

где `role` = 0=Source, 1=Target, 2=Control (совпадает с `EdgeRole` из `edge.rs`).

### 11.4. CrossLevelEdgeIncidencePage

То же самое, что и EdgeIncidencePage, но имеет другой вид entries для overflow-страницы с несколькими cross-level endpoints:

```
PageHeader: (magic=MAGIC_EDGE_INC_CL, type=CrossLevelEdgeIncidence)
```

```
entry (12 байт):
      sg_slot:    u32   -- GraphId.index() подграфа
      local_slot: u32   -- DiskAtomRef в пространстве подграфа
      role:       u8    -- 0=Source, 1=Target, 2=Control
      _pad:       u8[3]
```

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
      sg_slot:    u32   -- GraphId.index() подграфа
      role:       u8    -- 0=inv, 1=out
      _pad:       u8[3]
      local_slot: u64   -- DiskAtomRef в пространстве подграфа
→ 16336 / 16 = 1021 entries per page
```

`QualifiedAtomRef` = `{ sg_slot: u32, local_slot: u64 }` — 16 байт с учётом выравнивания.

---

## 13. PropHeapPage — куча свойств

Хранит сериализованные `PropertyMap` в **slotted page** формате: записи растут с конца к началу,
SlotDirectory — с offset 48 к концу.

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
  key_id:  u32 LE       -- интернированный id ключа из LabelDictionary
  value:   encoded Value  -- §5
```

### 13.3. Сжатие

Если `FLAG_COMPRESSED = 1` в PageHeader, bytes `[free_end..PAGE_SIZE]` сжаты LZ4 frame.
Байты `[32..40]` сжимать нельзя, так как это метаданные, и их сжатие не позволит прочитать их быстро без декомпрессии всей страницы.
Применяется при `serialized_size > FG_PROPS_COMPRESS_THRESHOLD` (default 512 байт).
Сжатие опциональное; решение принимается при записи страницы. 
Сжатие также применимо к PropLargeChunkPage.

### 13.4. Мультистраничные записи (Large Objects)

Если PropertyMap > `PAGE_PAYLOAD_SIZE / 2` (>8176 байт), он хранится как Large Object:
цепочка `PropLargeChunkPage` с `next_page` указателями.

```
PropLargeChunkPage:
  PageHeader (magic=MAGIC_PROP_HEAP, type=PropLargeHeap; FLAG_OVERFLOW set)
  next_page: u64
  data_len:  u32   -- байт данных на этой странице
  _pad:      u32
  data:      [u8]  -- raw bytes кодированного PropertyMap
```

В `NodeHotSlot.props_slot = 0xFFFF_FFFE (один ниже NULL_SLOT)` означает Large Object; `props_page` указывает на первый чанк.

### 13.5. Ссылка на запись — PropertyRef

```rust
struct PropertyRef {
    page_no:    u64,  // props_page из hot-слота
    slot_index: u16,  // индекс в SlotDirectory (не смещение!)
    _len:       u32,  // props_len из hot-слота (для быстрой проверки)
}
```

**Замечание:** Для прямого доступа без чтения `SlotDirectory` — использовать смещение из `SlotEntry`: `SlotEntry[props_slot].record_offset`.

### 13.6. Свойства и аналитика: OLTP row-store + колоночный кэш

`PropHeapPage` — **намеренно строково-ориентированное OLTP-хранилище**: все свойства атома лежат одной сериализованной записью. Это оптимально для доминирующего сценария (взять атом — получить все его свойства) и плохо для аналитического скана одного поля по миллионам атомов, где приходится читать целые записи ради одного значения.

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
80      8     u64   validity_page page_no битовой карты валидности; NULL_PAGE = нет пропусков
88      16264 —     data          Плотный массив [value; elem_count] фиксированной ширины
Total = PAGE_SIZE байт
```

Свойства и ограничения:
- Кэшируются **только поля фиксированной ширины** (числа, `datetime`, `duration`, `decimal`, векторы фиксированной размерности, `bool`). Строки и вложенные структуры не кэшируются.
- Элементы упорядочены по `DiskAtomRef.slot`, поэтому доступ по атому — арифметика, а не поиск. Пропуски (у атома нет этого свойства) отмечаются в битовой карте валидности.
- Инвалидация — как у `TensorCachePage`: `build_lsn < текущий checkpoint_lsn` при любой мутации соответствующего поля делает кэш устаревшим. Устаревший кэш не используется молча: планировщик либо перестраивает его, либо идёт по обычному пути.
- Раскладка `decimal` (§3.3) выбрана так, что колонка `decimal` побайтово совместима с Arrow `Decimal128Array`.

#### Продвинутая аналитика — через Arrow, а не своим кодом

Собственные реализации dictionary/RLE/bitpacking-кодирования и колоночного исполнителя сознательно **не разрабатываются**: это отдельная колоночная СУБД внутри графовой, с несоразмерной стоимостью сопровождения.

Вместо этого колонка из кэша отдаётся наружу как **Arrow-массив** (нулевое копирование для типов фиксированной ширины), после чего доступны готовые инструменты экосистемы: Arrow — как in-memory представление, Parquet — как формат выгрузки, DataFusion — как исполнитель аналитических запросов поверх выгруженных колонок. Все три написаны на Rust и пригодны и для памяти, и для файлов.

Граница ответственности: движок отвечает за граф, топологию, транзакции и материализацию колонок; тяжёлая колоночная аналитика — за пределами его ядра.

---

## 14. SubgraphDirPage — директория подграфов

Хранит информацию о каждом вложенном подграфе (arena subgraphs).

### 14.1. SubgraphDirEntry (32 байт)

```
Offset  Size  Тип    Поле           Описание
0       4     u32    sg_slot        GraphId.index()
4       4     u32    gen            Поколение
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

---

## 15. FreelistPage — управление свободными страницами

Каждый `.fgb` файл имеет сопутствующий `freelist_*.fgb` для отслеживания свободных страниц.

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
4. Установить bit=0 (занять), записать WAL `PageAlloc { file_kind, page_no }`.

---

## 16. LabelDictionary — интернирование строк

Интернирует: метки вершин/рёбер, имена таблиц, ключи свойств, имена полей.

### 16.1. Структура files labels.fgb

```
Page 0:      LabelDictHeader
Page 1..N:   DictEntryPage    (NodeLabel entries)
Page N+1..:  DictEntryPage    (EdgeLabel entries)
...          DictEntryPage    (PropKey entries)
...          DictEntryPage    (TableName entries)
...          HashIndexPage    (string → id lookup)
...          TzHashIndexPage  (IANA name → tz_id)
...          DictEntryPage    (TzName entries)
```

Формат `TzName entries` идентичен DictEntryPage: `tz_id(u32) + name_len(u16) + utf8_name`.
Lookup через отдельную HashIndexPage с ключом = `xxHash64(iana_name) → tz_id`. 
Диапазон `tz_id` для именованных зон: `1..999_999` (как в §3.4).

### 16.2. LabelDictHeader (Page 0)

```
Offset  Size  Тип    Поле                                 Описание
0       32    —      PageHeader (magic=MAGIC_LABEL_DICT, type=LabelDict)
32      4     u32    node_label_count                     Число зарегистрированных node-меток
36      4     u32    edge_label_count                     Число зарегистрированных edge-меток
40      4     u32    prop_key_count
44      4     u32    table_name_count
48      8     u64    node_entries_first_page              Первая страница с NodeLabel entries
56      8     u64    edge_entries_first_page
64      8     u64    prop_key_first_page
72      8     u64    table_name_first_page
80      8     u64    hash_index_first_page                Первая страница hash-индекса
88      8     u64    tz_hash_index_first_page             Первая страница TzName hash-индекса
96      8     u64    tz_entries_first_page                Первая страница TzName entries
104     16280 —      _reserved
```

### 16.3. DictEntryPage

```
Offset  Size   Содержимое
0       32     PageHeader (magic=MAGIC_LABEL_DICT, type=LabelDictEntry)
32      8      next_page (u64)
40      4      count (u32)
44      4      _pad
48      var    Entries: [id(u32) + name_len(u16) + utf8_bytes[name_len]]×count
```

**Минимальный размер entry:** 4+2+1 = 7 байт → max ~2300 single-char labels per page.  
**Типичный размер label** (10 chars): 4+2+10 = 16 байт → ~1000 labels per page.

`DictEntryPage` используется только для чтения при загрузке (bulk load в memory), а runtime lookup идёт через HashIndexPage.

### 16.4. Hash Index (string → id)

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

- Коллизии разрешаются линейным пробированием внутри страницы. 
- Load factor 0.75 → max 612 entries per page без overflow.

**Инварианты:** 
- id значения монотонно растут по секциям (node, edge, propkey);  
- для обратного lookup по id → entry: использовать DictEntryPage (записи идут по возрастанию id, бинарный поиск работает).

---

## 17. GraphSuperblock — суперблок графа

Точка входа для каждого именованного графа. Атомарно обновляется (tmp → rename).

```
Offset  Size  Тип    Поле                
0       32    —      PageHeader (magic=MAGIC_SUPERBLOCK, type=GraphSuperblock)
32      8     u64    graph_id_raw               GraphId.raw() для внешних ссылок
40      8     u64    created_at_ms              Unix ms
48      8     u64    updated_at_ms              Unix ms
56      8     u64    checkpoint_lsn             LSN последнего checkpoint
64      8     u64    wal_lsn                    LSN последней WAL-записи
72      8     u64    node_capacity              Текущая ёмкость node pool
80      8     u64    edge_capacity              Ёмкость edge pool
88      8     u64    node_count                 Живых вершин
96      8     u64    edge_count                 Живых рёбер
104     8     u64    subgraph_capacity          Ёмкость subgraph arena
112     8     u64    subgraph_count             Число живых подграфов
120     8     u64    node_hot_pages             Страниц в nodes_hot.fgb
128     8     u64    edge_hot_pages             Страниц в edges_hot.fgb
136     8     u64    props_pages                Страниц в props.fgb
144     8     u64    labels_size_bytes          Размер labels.fgb в байтах
152     8     u64    flags                      bit0=encrypted, bit1=lz4_props, bit2=read_only
160     8     u64    schema_root_page           Первая страница в schema/catalog.fgb
168     8     u64    stats_root_page            Первая страница в stats/graph_stats.fgb
176     128   u8[]   graph_name                 UTF-8 null-terminated, max 127 символов
304     16080 —      _reserved
```

---

## 18. SchemaCatalog — хранилище схем

Хранит все DDL-объекты: таблицы, поля, индексы, события, функции, анализаторы, параметры.

### 18.1. Общая структура

Каждый DDL-объект имеет:

- Уникальный `schema_id: u32` (монотонно растущий).
- `kind: SchemaObjectKind`.
- `version: u32` (при OVERWRITE инкрементируется).
- `definition: Vec<u8>` — сериализованное определение (формат §18.7).

```rust
enum SchemaObjectKind {
    Table = 0x01,
    Field = 0x02,
    Index = 0x03,
    Event = 0x04,
    Function = 0x05,
    Analyzer = 0x06,
    Param = 0x07,
    Changefeed = 0x08,
    LiveSelect = 0x09,
}
```

### 18.2. SchemaCatalogPage

```
Offset  Size  Тип    Поле                
0       32    —      PageHeader (magic=MAGIC_SCHEMA, type=SchemaRoot)
32      8     u64    next_page
40      4     u32    entry_count
44      4     u32    _pad
48      var   —      entries: [SchemaEntry]×N
```

**SchemaEntry (variable):**

```
Offset  Size  Тип    Поле  
0       4     u32    schema_id
4       1     u8     kind
5       4     u32    version
9       4     u32    name_id
13      4     u32    parent_id
17      4     u32    def_len
21      var   -      definition[def_len]
```

Где `parent_id` = table schema_id для Field, Index, Event, Changefeed.

### 18.3. TableDefinition (def format)

```
Offset  Size  Тип    Поле  
0       1     u8     type_flags                                 -- ANY=0, NORMAL=1, RELATION=2, SCHEMALESS|SCHEMAFULL bits
1       1     u8     drop_flag                                  -- DROP = 1
2       1     u8     has_as_select
3       1     u8    _pad
4       8     u64    changefeed_duration_ns                     -- 0 = нет changefeed; >0 = длительность хранения в нс
12      var   -      [as_select_query: len(u32) + utf8_bytes]   -- если has_as_select
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
index_kind:    u8  -- 0=Standard, 1=Unique, 2=Count, 3=Fulltext,
                   -- 4=HNSW, 5=DiskANN, 6=Geometry, 7=Reachability, 8=Neighbourhood, 9=Path
field_count:   u8
field_ids:     [u32] × field_count
flags:         u8  -- bit0=CONCURRENT, bit1=DEFERRED
status:        u8  -- 0=building_initial, 1=building_update, 2=ready, 3=error
[fulltext_params]  -- если kind=3: analyzer_id(u32) + bm25_k1(f32) + bm25_b(f32) + highlights(u8)
[hnsw_params]      -- если kind=4: dimensions(u32) + elem_type(u8: F64/F32/I64/I32/I16)
                   --              dist_metric(u8) + efc(u32) + m(u32) + m0(u32) + lm_scaled(f32)
[geo_params]       -- если kind=5: geometry_field_type(u8)
```

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

Планировщик (`gql_spec.md §21.4`) требует степени, гистограммы, selectivity.
Статистика обновляется инкрементально при DML-операциях и при явном `ANALYZE`.

### 19.1. GraphStatsPage

```
PageHeader(32) (magic=MAGIC_STATS, type=StatisticsRoot)
node_count:         u64
edge_count:         u64
avg_out_degree:     f64
avg_in_degree:      f64
max_out_degree:     u64
max_in_degree:      u64
degree_histogram_page: u64  -- страница с гистограммой out-degree
```

### 19.2. LabelStatsPage

```
PageHeader(32) (magic=MAGIC_STATS, type=LabelStatistic)
next_page: u64
count:     u32
_pad:      u32
entries: [label_id(u32) + atom_count(u64) + avg_prop_count(f32)] × count
```

### 19.3. PropertyHistogramPage

Гистограмма для одного `(table_id, field_id)`:

```
Offset                  Size                Тип     Содержимое
0                       32                  -       PageHeader (magic=MAGIC_STATS, type=PropertyHistogram)
32                      4                   u32     table_id
36                      4                   u32     field_id
40                      1                   u8      type_tag
41                      1                   u8      _pad
42                      8                   u64     distinct_count  -- NDV (number of distinct values)
50                      4                   f32     null_fraction   -- доля NULL/NONE значений
54                      2                   u16     bucket_count
56                      8 × bucket_count    [u64]   offsets         -- массив смещений бакетов
56 + 8 × bucket_count   -                   -       buckets: [lo_value + hi_value + freq(u64) + ndv(u64)] × bucket_count
```

Bucket значения кодируются compact order-preserving encoding (§20.2.1).

### 19.4. DegreeHistogramPage

```
PageHeader(32) (magic=MAGIC_STATS, type=DegreeHistogram)
label_id:  u32
direction: u8   -- 0=out, 1=in, 2=undir
_pad:      u8 × 2
bucket_count: u16
buckets:   [min_degree(u32) + max_degree(u32) + node_count(u64)] × bucket_count
```

### 19.5. Метод построения гистограмм и NDV

**Гистограммы — equi-depth (равнонаполненные).** Границы корзин выбираются так, чтобы в каждую попадало примерно одинаковое число строк, а не чтобы корзины были равной ширины по значению. Equi-width деградирует на перекошенных распределениях: одна корзина собирает почти всё, и оценка селективности становится бессмысленной — а перекос в графовых данных норма (степени вершин, частоты меток).

Число корзин — `FG_STATS_HISTOGRAM_BUCKETS` (по умолчанию 100). Для каждой корзины хранятся границы, частота и число различных значений внутри неё (`ndv`), что позволяет оценивать и точечные предикаты, и диапазонные.

**NDV — HyperLogLog.** Точный подсчёт различных значений требует памяти порядка их количества. Вместо этого используется тот же компонент HLL, что уже применяется в neighbourhood-индексе (§20.9): фиксированные 64 байта регистров на поле, погрешность ~2%. Итог записывается в `distinct_count`.

### 19.6. Обновление статистики

- **Инкрементально**: счётчики (`node_count`, `edge_count`, per-label `atom_count`) обновляются **дельтой на транзакцию** — накапливаются в транзакции и применяются один раз при коммите. Обновление на каждую строку превращало бы одну страницу статистики в точку сериализации всех писателей.
- **Полный пересчёт**: `ANALYZE [ TABLE @name | GRAPH @name ]`.
- **Автоматический trigger**: при `dirty_row_fraction > FG_STATS_AUTO_ANALYZE_DIRTY` (0.10).

**Выборка вместо полного скана.** `ANALYZE` не читает таблицу целиком, если она больше порога `FG_STATS_FULL_SCAN_THRESHOLD` (по умолчанию 32 страницы): для маленьких таблиц полный скан дешевле и точнее, для больших берётся **выборка фиксированного размера** `FG_STATS_SAMPLE_ROWS` (по умолчанию 30 000 случайных строк) **независимо от размера таблицы**.

Фиксированный, а не пропорциональный размер выборки — принципиально: иначе `ANALYZE` растёт линейно с графом и сам становится узким местом, что прямо противоречит автоматическому триггеру при 10% изменений. Точность equi-depth гистограммы определяется размером выборки, а не долей таблицы, поэтому нескольких десятков тысяч строк достаточно и для миллиарда атомов.

Алгоритм: собрать выборку → отсортировать → расставить границы корзин так, чтобы в каждой было ≈ `sample_size / bucket_count` строк → масштабировать частоты на `row_count / sample_size`. Границы корзин записываются в том же order-preserving кодировании, что и ключи индексов (§20.2.1), поэтому сравнение с предикатом не требует декодирования.

**Статистика как побочный продукт bulk-сборки.** При массовой загрузке (§20.13) ключи каждого индекса и так проходят через полную сортировку, поэтому точные `distinct_count`, `null_fraction` и границы корзин вычисляются одним и тем же проходом — отдельный `ANALYZE` после импорта не нужен и выборка не требуется.

---

## 20. Индексы

### 20.1. Общий контракт Index

Каждый индекс реализует:

```rust
trait Index {
    fn insert(&mut self , tx: &MvccTransaction, key: &IndexKey, atom: DiskAtomRef) -> io::Result<()>;
    fn delete(&mut self , tx: &MvccTransaction, key: &IndexKey, atom: DiskAtomRef) -> io::Result<()>;
    fn lookup(&self , key: &IndexKey) -> io::Result<Vec<DiskAtomRef>>;
    fn range(&self , lo: &IndexKey, hi: &IndexKey, inclusive: (bool, bool)) -> io::Result<Box<dyn Iterator<Item=DiskAtomRef>>>;
    fn rebuild(&mut self , source: &DiskMetaGraph) -> io::Result<()>;
    fn status(&self ) -> IndexStatus;
}
```

**Транзакционная консистентность**: standard, unique, count, fulltext — обновляются в той же транзакции (WAL + страница).  
**Eventually consistent**: HNSW (по умолчанию) или транзакционный (при `CONCURRENTLY` = false).

### 20.2. B+tree Property Index (standard и unique)

#### 20.2.1. Key encoding (order-preserving)

── Скалярные типы ──────────────────────────────────────────────────────
```
none      → 0xFF (sorts last)
null      → 0xFE
bool      → 0x00 (false) / 0x01 (true)
int       → big-endian i64 с инвертированным MSB: v ^ 0x8000_0000_0000_0000
float     → IEEE 754 order-preserving: (v - битовое представление f64, NaN в индексе - ошибка валидации)
              if positive: flip MSB    → v ^ 0x8000_0000_0000_0000
              if negative: flip all    → v ^ 0xFFFF_FFFF_FFFF_FFFF
decimal   → см. §20.2.1.1 (наивная схема «sign + coefficient + exponent» НЕ сохраняет порядок)
string    → raw UTF-8 (encoding зависит от позиции в composite key, см. §20.2.6)
bytes     → raw + 0x00 sentinel
datetime  → big-endian i64(seconds) + u32(nanos) [tz не индексируется]
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
record_id → table_id(4 байта BE) + id_part_encoding(variable)
```

При попытке вставить `NaN` значение в индекс происходит **ошибка валидации**.
В `PropHeapPage` `NaN` хранится как `edge weight sentinel`; в B+tree индексе он недопустим.

#### 20.2.1.1. Order-preserving ключ для `decimal`

Наивная схема «знак + big-endian мантисса + экспонента» **не сохраняет порядок** и потому непригодна. Два дефекта:

1. Численно равные значения с разным масштабом (`1.0` при `scale=1` и `1.00` при `scale=2`) дают **разные** ключи. Для unique-индекса это означает, что дубликат пройдёт проверку.
2. Сравнение сырых мантисс при разных экспонентах даёт неверный порядок: `5e-1` (мантисса 5) окажется больше `1e0` (мантисса 1), хотя `0.5 < 1.0`.

**Схема для schemaless-полей** (масштаб заранее не известен):

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

Порядок при таком кодировании лексикографический: сначала различает знак, затем десятичный порядок, затем значащие цифры слева направо. Нормализация на шаге 1 гарантирует, что численно равные значения дают побайтово равные ключи.

**Схема для schemafull-полей** (в `DEFINE FIELD` объявлен фиксированный масштаб): значение приводится к каноническому масштабу столбца, после чего ключ — просто

```
(mantissa as i128) ^ 0x8000_0000_0000_0000_0000_0000_0000_0000, big-endian, 16 байт
```

Это тот же приём, что уже применяется к `int`, он тривиально сохраняет порядок и вдвое короче. Схема выбирается по индексу: при объявленном масштабе — вторая, иначе — первая.

── Составные типы ──────────────────────────────────────────────────────
```
option<T> → 0x00 (None, сортируется первым) | 0x01 + encode(T)
array<T>  → element-wise: encode(elem[0]) + 0x01 + encode(elem[1]) + ... + 0x00
set<T>    → то же что array, элементы уже отсортированы (set = sorted unique)
object    → индексирование object целиком не поддерживается;
            для composite index используются отдельные поля
```

── Доменные типы ───────────────────────────────────────────────────────
```
vertex/edge/atom ref → big-endian u32 (DiskAtomRef; порядок по slot_index)
                       (семантический порядок вставки, не семантический)
```

── Неиндексируемые типы ────────────────────────────────────────────────
```
geometry  → не поддерживается в B+tree; используется R-tree index (§20.7)
vector    → не поддерживается в B+tree; используется HNSW index (§20.6)
range<T>  → bounds_byte(u8) + [encode(lo)] + [encode(hi)]
            bounds_byte: bit0=lo_inclusive, bit1=hi_inclusive,
                         bit2=lo_unbounded, bit3=hi_unbounded
            Unbounded lo: lo-часть опускается, prefix 0x00 (sorts first)
            Unbounded hi: hi-часть опускается, suffix 0xFF (sorts last)
            Порядок: по lo, затем по hi; None lo < любого значения
graph_ref → big-endian u64 (sg_slot(u32 BE) + sg_gen(u32 BE))
            Полезен для equality lookup; диапазонные запросы семантически
            не осмысленны (порядок аллокации ≠ смысловой порядок).

```

Эта кодировка гарантирует: `a < b ⟺ encode(a) < encode(b)` лексикографически.

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
[44+2n..]       entry_area: [LeafEntry] × num_entries

LeafEntry (variable):
  encoded_key:     variable                     -- order-preserving (§20.2.1)
  inline_count:    u32                          -- общее число DiskAtomRef в индексе для этого ключа (inline + posting_pages)
  atom_refs:       u32 × min(8, inline_count)   -- DiskAtomRef inline, только первые 8 (или меньше) хранятся inline
  posting_page:    u64                          -- только если inline_count > 8; первая PostingPage
```

Свободное место: `PAGE_SIZE − 44 − 2n − Σ(entry_len_i)`.

```
Binary search: 
  entry_offsets[mid] → decode encoded_key → compare.
```

#### 20.2.4. BTree Leaf Page (Unique)

Структура идентична Standard, но каждый LeafEntry:
```
PageHeader (magic=MAGIC_IDX_BTREE, type=IndexUniqueBtreeLeaf, FLAG_LEAF set)

encoded_key:  variable
atom_ref:     u32   -- единственная запись; нет posting_page
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
48      16336  [DiskAtomRef(u64)] × (count) → (PAGE_SIZE - 48) / 16 = 1021 entries per page
```

#### 20.2.6. Composite Index

Composite index строится по нескольким полям одновременно.

**Зачем**: ускоряет запросы с фильтрацией или сортировкой по нескольким полям:
`SELECT * FROM person WHERE age > 18 AND city = 'Moscow'`
→ если есть `DEFINE INDEX ON person FIELDS age, city` → один B+tree lookup

**Определение**:
`DEFINE INDEX idx_age_city ON TABLE person FIELDS age, city`

Создаёт B+tree с составным ключом: сначала кодируется `age`, затем `city`.
Порядок полей в `DEFINE INDEX` определяет порядок сортировки в индексе.

**Свойство частичного поиска**: если `index = (A, B, C)`, то он эффективен для:
`WHERE A = x`                       → prefix scan ✓
`WHERE A = x AND B = y`             → prefix scan ✓
`WHERE A = x AND B = y AND C = z`   → точечный lookup ✓
`WHERE B = y`                       → full index scan (нет использования порядка A) ✗
`WHERE B = y AND C = z`             → full index scan ✗

**Формирование ключа**:
- Ключ = конкатенация order-preserving encodings всех компонент.
- Порядок сортировки: по первому компоненту, при равенстве — по второму и т.д.
- Каждый компонент кодируется по правилам §20.2.1.

Для определения длины каждого компонента при декодировании:
- Скалярные типы (int, float, decimal, datetime, uuid, ulid): фиксированная длина.
- String, bytes: до первого терминирующего sentinel (правила ниже).
- Составные типы (array, set): self-delimiting через TypeTag + length prefix.

**Кодирование строк в composite key**:

Строки в non-terminal позиции composite key требуют экранирования,
потому что сырой UTF-8 + 0x00 sentinel конфликтует с байтами следующего компонента.

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

#### 20.2.7. Primary Index (pk_<table_id>.fgidx)

Обязательный unique B+tree: `RecordId.id_part → DiskAtomRef`.
Ключ = order-preserving encoding `id_part` (Int, String, Uuid, Array, Object).
Создаётся автоматически для каждой таблицы при `DEFINE TABLE`.

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

Два независимых механизма: структурное сжатие ключей и блочное сжатие страницы. Они складываются и решают разные задачи.

**Префиксное сжатие ключей (первично, применяется по умолчанию).** Ключи в листе отсортированы, поэтому соседние обычно имеют длинный общий префикс — особенно строковые и составные (§20.2.6). В листе хранится общий префикс всех ключей страницы один раз, а каждая запись содержит только суффикс:

```
[..]        page_prefix_len (u16)
[..]        page_prefix (page_prefix_len байт)
[записи]    для каждого ключа: suffix_len (u16) + suffix_bytes
```

Это **не** LZ4 и вообще не универсальный компрессор: сжатие структурное и order-preserving, поэтому по сжатой странице по-прежнему работает бинарный поиск **без распаковки** — сравнивается только суффикс. Стоимость на чтении нулевая, выигрыш для строковых ключей часто двукратный.

Существенно: префиксное сжатие дёшево при bulk-сборке (§20.13), где лист формируется целиком и общий префикс известен сразу, и дорого при построчной вставке — вставка в середину сжатого листа может изменить общий префикс и потребовать перекодирования всей страницы. Поэтому оно применяется при сборке и перестроении, а при точечных вставках лист может временно храниться без него до ближайшего уплотнения.

**Блочное сжатие (вторично, опционально).** Сжатие всей полезной нагрузки страницы целиком, как уже сделано для `PropHeapPage` (§13.3) через `FLAG_COMPRESSED`. Требует распаковки страницы перед любым обращением, поэтому применяется только при выигрыше — порог `FG_PROPS_COMPRESS_THRESHOLD`.

Алгоритм настраивается **на таблицу** (`block_compressor` в `TableDefinition`), а не глобально: у разных данных разный профиль.

| Значение             | Когда уместно                                                                             |
|----------------------|-------------------------------------------------------------------------------------------|
| `none`               | Данные уже плотные; приоритет — латентность                                               |
| `lz4` (по умолчанию) | Универсальный компромисс; тот же кодек, что и в `PropHeapPage`                            |
| `zstd`               | Холодные данные и архивные графы; заметно плотнее LZ4, дороже по CPU                      |
| `snappy`             | Совместимость с внешними экосистемами                                                     |
| `fsst`               | Специализированно для коротких строк — сохраняет возможность работы без полной распаковки |

Кодеки, требующие полной распаковки страницы (`lz4`, `zstd`, `snappy`), не сочетаются с чтением произвольной записи «на месте», поэтому для горячих индексов рекомендуется ограничиваться префиксным сжатием.

### 20.3. Count Index

Хранит единственное значение — число живых записей в таблице.

```
CountIndexPage (единственная страница):
  PageHeader(32)  (magic=MAGIC_IDX_COUNT, type=IndexCountSingle)
  table_id:    u32
  _pad:        u32
  atom_count:  u64   -- число живых атомов
  last_updated_lsn: u64
  created_at_lsn: u64
  _pad:        16320
```

Обновляется атомарно при каждом INSERT/DELETE в той же транзакции.
Используется для `SELECT count() FROM @table` + `GROUP ALL` без scan.

### 20.4. Label Index (обязательный, неявный)

Файл: `idx/lbl_node_<label_id>.fgidx` и `idx/lbl_edge_<label_id>.fgidx`.

Sorted array DiskAtomRef (u32), разбитый по страницам:

```
Offset  Size    Содержимое
0       32      PageHeader (magic=MAGIC_IDX_LABEL, type=IndexLabelArray)
32      8       next_page (u64)
40      4       count (u32)
44      4       _pad
48      16336   [DiskAtomRef(u64)] × count  → (PAGE_SIZE - 48) / 8 = 2042 per page
```

Массив отсортирован по slot_index. Binary search + страничная навигация.

**Lookup (equality):** Binary search по pages → O(log n) disk reads.

**Insert**: append к последней странице (слоты монотонно растут для новых атомов);
для переиспользованных слотов — insertion в sorted позицию.

**Delete**: tombstone-маркер (NULL_SLOT вместо DiskAtomRef) или физическое удаление при compaction (при fragmentation > threshold).

LabelIndex не имеет freelist, только tombstone + ручной compaction.
При компакции label index нужно создавать новый файл (tmp → rename) без freelist.

### 20.5. Fulltext Index (BM25)

Файл: `idx/ft_<label_id>_<key_id>.fgidx`

Инвертированный индекс: term → posting list.

#### 20.5.1. FulltextHeaderPage (Page 0)

```
PageHeader(32) (magic=MAGIC_IDX_FT, type=IndexFulltextHeader)
index_name_id:    u32
table_id:         u32
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

### 20.6. HNSW Vector Index

Файл: `idx/vec_<label_id>_<key_id>.fgidx`

**HNSW** — иерархически управляемый малый мир (Hierarchical Navigable Small World).

**MVP (persistence as snapshot):**
- HNSW граф строится и хранится полностью в памяти.
- При checkpoint сериализуется как opaque blob на VectorPages.
- При открытии загружается в память целиком и восстанавливается.

#### 20.6.1. VectorHeaderPage (Page 0)

```
PageHeader(32) (magic=MAGIC_IDX_VEC, type=IndexVector)
index_name_id:    u32
table_id:         u32
field_id:         u32
elem_type:        u8   -- 0=F64, 1=F32, 2=I64, 3=I32, 4=I16
dist_metric:      u8   -- 0=EUCLIDEAN, 1=COSINE, 2=MANHATTAN, 3=MINKOWSKI, 4=CUSTOM
_pad:             u16
dimensions:       u32
m:                u32  -- max connections per layer (default 12)
m0:               u32  -- connections at base layer (default 24 = 2×M)
efc:              u32  -- ef_construction (default 150)
lm:               f32  -- малое вещественное число
entry_point_ref:  u32  -- DiskAtomRef точки входа
max_level:        u32
node_count:       u64
data_start_page:  u64  -- первая страница blob-данных
data_byte_len:    u64  -- общий размер serialized HNSW
custom_fn_id:     u32  -- NULL_LABEL = нет кастомной метрики
```

#### 20.6.2. VectorDataPage (opaque blob)

```
PageHeader(32) (magic=MAGIC_IDX_VEC, type=IndexVectorData)
next_page:  u64
data_len:   u32
_pad:       4
data:       [u8] × 16336   -- сырые байты сериализованного HNSW
```

**Memory management**: HNSW целиком в памяти до `FG_HNSW_CACHE_SIZE` (default 256 МиБ).
При превышении — LRU eviction по индексам (не узлам).

**Поддерживаемые метрики**: EUCLIDEAN, COSINE, MANHATTAN, MINKOWSKI, пользовательская функция.

**Supported element types**: F64, F32, I64, I32, I16 (совпадает с `gql_spec.md §9.6`).

#### 20.6.3. Долговечность и восстановление

Сериализация графа HNSW блобом только при checkpoint означала бы, что крах между checkpoint'ами теряет все вставки с момента последнего из них, а восстановление требует полной пересборки индекса — операции, стоимость которой растёт с размером набора векторов и на больших индексах измеряется десятками минут. Это неприемлемо как единственная стратегия.

**Двухуровневая схема:**

1. **Журналирование операций.** Каждая вставка и удаление вектора логируется записью `IndexInsert` / `IndexDelete` с `index_id` вектора, `DiskAtomRef` атома и самим вектором. Это логические аннотации (§22.1) — они не участвуют в восстановлении страниц, но образуют полный журнал изменений вектор-индекса начиная с `build_lsn` последнего сохранённого блоба.
2. **Периодическая сериализация.** Блоб `VectorDataPage` пишется при checkpoint и хранит `build_lsn` — момент, на который он актуален.

**Восстановление:** загрузить блоб → проиграть записи `IndexInsert`/`IndexDelete` с `lsn > build_lsn` как обычные вставки в память. Стоимость ограничена числом векторных операций **с последнего checkpoint**, а не размером индекса.

**Граница деградации.** Если журнал с `build_lsn` недоступен (обрезан, повреждён) — индекс помечается `stale`. Устаревший вектор-индекс **не используется молча**: планировщик либо отказывается от него (точный перебор, если это допустимо по стоимости), либо возвращает ошибку, требуя `REBUILD INDEX`. Молчаливая выдача неполных результатов ANN-поиска недопустима — она неотличима от нормальной работы.

**Память при сборке.** `FG_HNSW_CACHE_SIZE` (256 МиБ) — бюджет **обслуживания**, а не построения. Для построения выделяется отдельный `FG_HNSW_BUILD_MEM` (по умолчанию 2 ГиБ). При нехватке бюджета сборка завершается явной ошибкой ресурса; для наборов, не помещающихся в память, предназначен `DISKANN` (§20.6.4 `gql_spec` §9.6) — он изначально спроектирован как out-of-core и в этом сценарии предпочтителен шардированию HNSW.

### 20.7. Geometry R-tree Index

Файл: `idx/geo_<name>.fgidx`.

Spatial index для `geo::` операторов: `INSIDE`, `INTERSECTS`, `OUTSIDE`, `geo::within`, `geo::distance`.

**Структура**: R-tree (2D bounding rectangles).

#### 20.7.1. GeoHeaderPage

```
PageHeader(32) (magic=MAGIC_IDX_RTREE, type=IndexGeometryRtreeHeader)
index_name_id: u32
table_id:      u32
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

**Файл:** `idx/reach_<name>.fgidx`

Создаётся один файл на один именованный граф.
Заголовок хранит ссылки на корни B+tree для каждого из видов индекса.

#### ReachabilityHeaderPage
```
Offset  Size  Тип    Поле
  0       32    —      PageHeader (magic=MAGIC_IDX_REACH, type=IndexReachabilityHeader)
  32      1     u8     algorithm      -- 0=tree_cover, 1=grail, 2=ferrari,
                                      --   3=bfl, 4=on_demand
  33      1     u8     direction      -- 0=out, 1=in, 2=both
  34      1     u8     status         -- 0=building, 1=ready, 2=stale
  35      1     u8     k_param        -- GRAIL: число интервалов k;
  36      4     u32    n_landmarks    -- BFL: число landmark-вершин
  40      8     u64    node_count
  48      8     u64    build_lsn      -- LSN момента построения
  56      8     u64    tc_root        -- TREE_COVER: B+tree корень
  64      8     u64    grail_root     -- GRAIL: B+tree корень
  72      8     u64    ferrari_root   -- FERRARI: B+tree корень
  80      8     u64    bfl_root       -- BFL: B+tree корень
  88      8     u64    landmarks_page -- BFL: страница с landmark DiskAtomRef[]
```

#### Формат label-записей (хранятся в B+tree с ключом DiskAtomRef)

Label-записи хранятся в стандартных B+tree leaf-страницах (§20.2.3).
Ключ leaf-записи = DiskAtomRef(u64) вершины. Значение зависит от алгоритма:
```
TREE_COVER:
PageHeader (magic=MAGIC_IDX_REACH, type=IndexReachabilitTreeCover)
value = count(u32) + [in_ts(u32 LE) + out_ts(u32 LE)] × count   -- 8 × count байт

GRAIL(k):
PageHeader (magic=MAGIC_IDX_REACH, type=IndexReachabilitGrail)
value = [ in_ts(u32 LE) + out_ts(u32 LE) ] × k   -- 8×k байт

FERRARI:
PageHeader (magic=MAGIC_IDX_REACH, type=IndexReachabilitFerrari)
value = count(u32) + [in_ts(u32 LE) + out_ts(u32 LE) + extra(u32 LE)] × count   -- 12 × count байт

BFL:
PageHeader (magic=MAGIC_IDX_REACH, type=IndexReachabilitBfl)
out_bitmap: u64[ ceil(n_landmarks/64) ]   -- landmark'ы, достижимые из v
in_bitmap:  u64[ ceil(n_landmarks/64) ]   -- landmark'ы, из которых v достижима
итого: 2 × ceil(n_landmarks/64) × 8 байт на вершину
```

#### LandmarkListPage (только для BFL)

```
  Offset  Size    Содержимое
  0       32      PageHeader (magic=MAGIC_IDX_REACH, type=IndexReachabilityLandmarkList)
  32      8       next_page (u64)
  40      4       count (u32)
  44      4       _pad
  48      16336   landmarks: DiskAtomRef(u64) × count
  -- per page: 16336 / 8 = 2042 landmark-вершины
```

### 20.9. Neighbourhood Index

**Файл:** `idx/nbh_<name>.fgidx`

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

**Файл:** `idx/path_<name>.fgidx`

#### PathIndexHeaderPage (страница 0)

```
Offset  Size  Тип    Поле
  0       32    —      PageHeader (magic=MAGIC_IDX_PATH, type=IndexPathHeader)
  32      1     u8     algorithm         -- 0=landmark_sssp, 1=pattern_cache,
                                         --   2=on_demand
  33      1     u8     direction         -- 0=out, 1=in, 2=both
  34      1     u8     status            -- 0=building, 1=ready, 2=stale
  35      1     u8     _pad
  36      4     u32    n_landmarks       -- LANDMARK_SSSP: число landmark-вершин
  40      8     u64    node_count
  48      8     u64    build_lsn
  56      8     u64    landmark_refs_page -- список DiskAtomRef landmark-вершин
  64      8     u64    sssp_root          -- B+tree расстояний (LANDMARK_SSSP)
  72      8     u64    patterns_root      -- B+tree паттернов (PATTERN_CACHE)
  80      16304 —      _reserved
```

#### LANDMARK_SSSP — хранение дистанций

LandmarkListPage: аналог §20.8 LandmarkListPage.

```
PageHeader (magic=MAGIC_IDX_PATH, type=IndexPathLandmarkList)

B+tree (sssp_root):
    ключ: landmark_idx(u8) + _pad(7) + DiskAtomRef(u64) = 16 байт
    значение: distance(u32)   -- u32::MAX = недостижимо
    
Пространство: n_landmarks × n_nodes × 4 байт.
```

#### PATTERN_CACHE — хранение кэшированных результатов

```
PageHeader (magic=MAGIC_IDX_PATH, type=IndexPathPatternCatalog)

PatternCatalogPage (patterns_root → B+tree leaf):
    pattern_hash: u64          -- xxHash3 строки паттерна
    pattern_len:  u16
    pattern_str:  u8[pattern_len]   -- GQL-строка паттерна
    result_page:  u64          -- первая страница кэшированного subgraph
```

Кэшированный subgraph: SubgraphDirPage-совместимый формат (§14), содержит атомы и рёбра результата паттерна.
Устанавливаем status = stale при любом изменении топологии.

### 20.11. Совместное использование индексов

При `DEFINE INDEX ... DEFER` — индекс работает в eventually-consistent режиме:

- Pending changes накапливаются в `IndexPendingQueue` в памяти.
- Background worker применяет их с интервалом.
- `status` = `building_update` пока очередь непуста.

При `CONCURRENTLY` — начальная индексация без блокировки таблицы:

- Snapshot существующих записей → bulk insert.
- Параллельные changes → в PendingQueue.
- После завершения bulk → flush PendingQueue.

### 20.12. EdgeEndpointIndex

Когда атом имеет другое ребро как endpoint (V-E, E-V, E-E), стандартный adjacency index (NodeHotSlot.adj_*) 
не отслеживает такие связи — он работает только с вершинами.

**Назначение:** ускорить AtomWalk-traversal через edge-endpoints.

**Файл:** `idx/ee_endpoint.fgidx`

**Структура:** B+tree.
- Ключ: DiskAtomRef ребра-endpoint (QualifiedAtomRef при CROSS_LEVEL)
- Значение: список записей (edge_ref, role)

```
Header:
  PageHeader (magic=MAGIC_IDX_EE, type=IndexEdgeEndpointHeader)
```

Листовые страницы используют стандартный формат B+tree-листа (§20.2.3); собственного `PageHeader` у отдельной записи, разумеется, нет — заголовок принадлежит странице. Запись:

```
LeafEntry (24 байта):
  endpoint_ref:  u64  -- DiskAtomRef ребра, выступающего endpoint'ом (ключ)
  edge_ref:      u64  -- DiskAtomRef ребра, у которого оно endpoint
  role:          u8   -- 0=inv (Source), 1=out (Target)
  _pad:          u8[7]
```

**Создаётся автоматически** при наличии хотя бы одного ребра с is_edge(endpoint) = true.

**Использование при traversal:**
1. Текущий атом = ребро $e_1$ (AtomWalk-режим)
2. $`EdgeEndpointIndex.lookup(e_1)$` → список ($e_n$, role)
3. Для каждого $e_n$ → противоположные endpoint'ы из `EdgeHotSlot` → следующие атомы

**Participation-инцидентность (EdgeIncidence)** хранится в `EdgeIncidencePage` (уже определённой в §11.3), не в `EdgeEndpointIndex`. 
Два разных индекса для двух разных видов инцидентности.

### 20.13. Bulk-сборка индексов

Построчная вставка сопровождает каждый индекс отдельно: спуск по дереву за `O(log n)` **случайными** обращениями к страницам, расщепления, и WAL-запись на каждую пару (строка, индекс). При загрузке графа это доминирующая стоимость. Bulk-путь строит индекс снизу вверх, записывая каждую страницу **ровно один раз**, последовательно.

Путь состоит из четырёх переиспользуемых компонентов; они одни и те же для `DEFINE INDEX` на непустой таблице, `REBUILD INDEX`, `COMPACT INDEX` и режима импорта (`gql_spec` §7.11).

#### 20.13.1. Внешняя сортировка

Ключ сортировки — `encoded_key ‖ DiskAtomRef` (big-endian). Два следствия, оба важны:
- кодирование ключей order-preserving (§20.2.1), поэтому компаратор — **`memcmp`**, без декодирования и диспетчеризации по типу;
- добавление `DiskAtomRef` делает порядок **тотальным**, поэтому устойчивая сортировка не нужна, а posting-списки выходят уже упорядоченными по `DiskAtomRef` — ровно так, как требуется §20.5.4 для слияния AND/OR и §20.4 для label-массивов.

Алгоритм: генерация прогонов (заполнить `FG_BULK_SORT_MEM`, отсортировать `sort_unstable`, параллельно по потокам) → спиллинг прогонов в `FG_TEMPFILES_PATH` через собственный слой Direct I/O → k-путёвое слияние (`FG_BULK_SORT_MERGE_FANIN`, по умолчанию 64) деревом проигравших. Выход слияния — итератор, напрямую питающий упаковщик страниц; промежуточная материализация не нужна.

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

Физическая раскладка файла делает цепочку листьев **непрерывной**, поэтому `next_leaf = page_no + 1`, а полный скан индекса становится одним последовательным чтением:

```
страница 0        : метастраница
страницы 1..L     : листья (непрерывно)
страницы L+1..L+P : posting-страницы
далее             : внутренние уровни, снизу вверх
последняя         : корень (фиксируется в метастранице)
```

Внутренние уровни строятся потоково: при запечатывании потомка в родительский строитель отдаётся разделитель (с усечением, §20.2.2); когда родитель заполняется, он рекурсивно отдаёт свой разделитель выше. Уровень, завершившийся единственной страницей, и есть корень.

#### 20.13.3. Fill factor

`FG_INDEX_BTREE_FILL_FACTOR` = **0.90** (было 0.80). Плотность здесь защищает не столько от расщеплений, сколько от **физической фрагментации цепочки листьев**: первая вставка в полный лист и расщепляет его, и вытягивает физически случайную страницу из freelist в логически последовательную цепочку — после чего последовательность скана восстанавливается только полной пересборкой.

| Случай                                                                 | Fill factor                                                        |
|------------------------------------------------------------------------|--------------------------------------------------------------------|
| По умолчанию                                                           | 0.90                                                               |
| Монотонно возрастающий ключ (`pk_` над ULID/UUIDv7/int, `DiskAtomRef`) | 1.00 — рост идёт по правому краю, внутренних расщеплений не бывает |
| Индекс объявлен только для чтения                                      | 1.00                                                               |
| Заведомо случайные вставки после загрузки                              | 0.75–0.80                                                          |

Монотонность определяется автоматически: если первый ключ отсортированного потока не меньше текущего максимума индекса (или индекс пуст, а тип ключа `uuid`/`ulid`/`record_id`), fill factor повышается до 1.00.

Отдельно: `FG_INDEX_BTREE_INTERNAL_FILL_FACTOR` = 0.95 (внутренних страниц мало, они кэш-резидентны — плотнее упаковать почти бесплатно), `FG_INDEX_RTREE_FILL_FACTOR` = 0.95.

#### 20.13.4. Протокол side-file: журнал не растёт

Тело bulk-сборки **не журналируется**. Сборка идёт в новый файл, недостижимый из закоммиченного состояния каталога, поэтому крах в процессе не может нарушить консистентность — незавершённый файл просто удаляется.

```
1. Собрать в idx/.build/<name>.<gen>.fgidx.tmp — последовательная запись
   мимо BufferPool и мимо WAL. Записей журнала: НОЛЬ.
2. fsync(tmp), fsync(каталог)
3. WAL: одна запись в той же транзакции, что и обновление каталога:
     IndexBuildComplete { index_id, generation, root_page, leaf_count,
                          key_count, source_snapshot_lsn, file_checksum }
     SchemaDef { IndexDefinition: status = ready, file_generation = <gen> }
4. WAL: fsync
5. rename(tmp → idx/<name>.<gen>.fgidx), fsync(каталог)
6. Удалить прежнее поколение, когда на него не осталось ссылок
```

Порядок величин: сборка индекса на 3 ГБ даёт **~200 байт** журнала вместо десятков гигабайт при построчном пути.

**Нумерация поколений вместо перезаписи** принципиальна для Windows (целевая платформа): открытый читателями файл нельзя удалить, а `MoveFileEx` с заменой оставляет существующие хендлы указывающими на прежний inode. Поколение в имени и указатель в каталоге снимают весь этот класс проблем на обеих платформах.

Восстановление: незавершённые файлы в `idx/.build/` собираются сборщиком мусора; REDO записи `IndexBuildComplete` идемпотентен (переименовать, если источник есть, а цели нет; иначе — ничего). Если ни временного, ни финального файла нет, индекс помечается `stale` — **никогда** не `ready` молча.

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
| EdgeEndpoint                        | Сортировка `(endpoint_ref, edge_ref, role)`                                      | да                          |

**Проверка уникальности бесплатна.** В отсортированном потоке нарушение уникальности — это ровно пара соседних записей с равным префиксом-ключом. Одно сравнение с предыдущей записью, `O(1)` памяти, ноль обращений к диску — вместо спуска по дереву на каждую строку только ради ответа на вопрос «есть ли дубликат».

Тем же проходом бесплатно получаются `distinct_count`, границы корзин гистограммы, `null_fraction` (§19.5) и `doc_freq` для полнотекстового индекса.

**Производные индексы (reachability, neighbourhood, path) не поддерживают построчного сопровождения в принципе:** все их алгоритмы (интервальные метки DFS, GRAIL, FERRARI, BFL, k-hop замыкание, multi-source Dijkstra) требуют глобального обзора графа, и одна вставка ребра может инвалидировать `Θ(n)` меток. Они объявляются **снимковыми артефактами**, актуальными на `build_lsn`: любая мутация топологии переводит их в `stale`, а устаревший индекс не используется молча — планировщик откатывается к `ON_DEMAND`. У них нет `insert`/`delete`, только `build(snapshot)` и `invalidate()`.

**STR-упаковка R-tree** (Sort-Tile-Recursive): отсортировать по координате X, разбить на `S = ceil(sqrt(P))` вертикальных полос, внутри каждой отсортировать по Y и нарезать узлами по `C` элементов; полученные MBR подать на следующий уровень. Даёт заполнение узлов ~100% против ~70% при поштучной вставке и существенно меньшее перекрытие MBR.

**HNSW** сортировке не поддаётся — это инкрементальное построение графа. Bulk-режим ограничивается параллельной сборкой в памяти с **перемешиванием порядка вставки**: если поток загрузки кластеризован (по таблице, по метке, по исходному файлу), жадное построение HNSW деградирует по полноте выдачи, поскольку алгоритм рассчитан на случайный порядок поступления.

#### 20.13.6. Отношение к `DEFER` и `CONCURRENTLY`

`DEFER` (§20.11) **не является** заменой bulk-сборке: он переносит ту же построчную работу в фон, то есть превращает проблему латентности в проблему очереди. На загрузке в сотни миллионов строк это даёт очередь того же порядка.

Разрешение противоречия «очередь в памяти или персистентная» (§20.11 против `gql_spec` §9.6): очередь остаётся **в памяти и ограниченной** `FG_INDEX_PENDING_MAX`. При достижении порога происходит **эскалация**: очередь сбрасывается, индекс помечается `stale`, планируется полная bulk-пересборка. Это crash-safe по построению — ничего долговечного не теряется, поскольку устаревший индекс всё равно строится заново из базовых данных. Персистентная очередь потребовала бы собственного формата, вида WAL-записи и семантики восстановления — то есть незаметно превратилась бы в L0 LSM-дерева, от которого проект отказался сознательно.

Роли получаются раздельными и определёнными: `DEFER` — оптимизация **малой** дельты при редких точечных записях; bulk-сборка — путь **большой** дельты; между ними один явный переход.

`CONCURRENTLY` реализуется **поверх** bulk-сборки: снимок на `build_lsn` → внешняя сортировка и сборка в side-file → добор накопившейся (к этому моменту небольшой) очереди обычными вставками → атомарная подмена поколения.

---

## 21. Changefeed и Live-запросы

### 21.1. Changefeed Log

Требуется `DEFINE TABLE ... CHANGEFEED @duration`.

**duration** — время хранения изменений (например, `CHANGEFEED 7d` хранит 7 дней).

```
<graph_name>/changefeed/<table_id>_<since_lsn>.fcf
```

#### 21.1.1. ChangefeedPage

```
PageHeader(32) (magic=MAGIC_CHANGEFEED, type=ChangefeedHeader)
table_id:      u32
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
event_kind:    u8    -- 0=create, 1=update, 2=delete, 3=define_table
atom_ref:      u32   -- DiskAtomRef
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

`LIVE SELECT` подписывается на изменения таблицы.

**Хранимое состояние live-запроса** (в SchemaCatalog, kind=LiveSelect):

```
live_id:       u128  -- UUID v7 ($id для KILL)
table_id:      u32
filter_ir_len: u32
filter_ir:     [u8]  -- сериализованный IR предиката WHERE
output_mode:   u8    -- 0=full_record, 1=value_fields, 2=diff, 3=patch
created_at_ms: u64
last_active_ms:u64
```

**Delivery mechanism**: на каждый DML в таблице, если таблица имеет live-подписчиков:

1. Определить matching live queries (filter IR evaluation).
2. Добавить в `LiveEventQueue` (in-memory, per-session).
3. Session reader вычитывает из очереди и отправляет клиенту.

Live queries хранятся только в SchemaCatalog для восстановления сессии после перезапуска.
Сами очереди — in-memory.

---

## 22. WAL для дискового режима

Дисковый режим использует **физический журнал из образов страниц** (frame-WAL) с индексом версий страниц — модель «copy-on-write в логе», как в SQLite WAL и RavenDB Voron. Журнал единый на всю базу данных (§7.1).

### 22.0. Модель

Писатель, желающий изменить логическую страницу `P`, **не правит её на месте**. Он копирует `P` в scratch-фрейм и меняет копию. Коммит — дописать фреймы транзакции и маркер `TxCommit`, затем один `fdatasync`. Чтение страницы на снимке `S` — самый свежий фрейм с `lsn ≤ S`, а если такого нет — страница из `.fgb`. Checkpoint переносит закоммиченные фреймы обратно **на их штатные места** в `.fgb` (§23.1).

Что эта модель даёт по сравнению с журналом дельт:

| Проблема                        | Решение                                                                                                                                                                     |
|---------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Незакоммиченные данные на диске | Фреймы незавершённой транзакции лежат в журнале, но без маркера коммита невидимы и никогда не бэкфиллятся. NO-STEAL на уровне `.fgb` **без** ограничения размера транзакции |
| Torn write                      | Фрейм **и есть** полный образ страницы; отдельный механизм full-page-write не нужен                                                                                         |
| Отсутствие on-disk MVCC         | Снимок = «фреймы с `lsn ≤ S`». Версионность появляется как побочный эффект                                                                                                  |
| Потолок объёма транзакции       | Фреймы спиллятся в журнал; в памяти — только индекс (≈16 байт на фрейм)                                                                                                     |
| Откат                           | Отбросить scratch-фреймы и записи индекса. UNDO и CLR не нужны                                                                                                              |
| Идемпотентность replay          | Применение фрейма — `memcpy`; повторное применение того же образа ничего не меняет                                                                                          |

Ключевое отличие от «чистого COW» (LMDB): обратная запись **на штатное место** сохраняет арифметическую адресацию §25.1 (`page_no = slot / 102`) и физическую кластеризацию SoA, на которых держится аналитический путь. Data-file COW обе эти вещи разрушает.

### 22.1. Набор WalKind

**Записи восстановления** (участвуют в replay):

```
TxBegin         = 0x30,   // Начало транзакции
TxCommit        = 0x31,   // Коммит: commit_ts + frame_count
TxAbort         = 0x32,   // Явный откат
PageFrame       = 0x33,   // Полный образ страницы
PageInit        = 0x34,   // Страница создана после checkpoint (образ не нужен)
CheckpointBegin = 0x35,
CheckpointEnd   = 0x36,   // checkpoint_lsn + dirty page table
GraphCreate     = 0x37,   // graph_id + name
GraphDrop       = 0x38,   // graph_id
```

**Логические аннотации** (0x10–0x22 из прежней редакции: `NodeAlloc`, `AdjAppend`, `PropSet`, `IndexInsert`, `SchemaDef`, `StatsUpdate`, `ChangefeedPut` и прочие) **сохраняются, но перестают быть записями восстановления**. Они опциональны, не реплеятся и служат changefeed'у, логической репликации и диагностике. Единственный источник истины при восстановлении — фреймы.

Записи режима 1 (0x01–0x07, in-memory MVCC поверх логического WAL) не изменяются.

### 22.2. Формат записей

Заголовок записи:

```
[ MVCE:4 ][ lsn:8 ][ tx_id:8 ][ kind:1 ][ payload_len:4 ][ checksum:8 ] = 33 байта
```

`checksum` — xxHash3-64 по `lsn ‖ tx_id ‖ kind ‖ payload_len ‖ payload`. Переход с CRC32 на xxHash3-64 согласует контрольную сумму записи с контрольной суммой страницы (§8) и заметно дешевле на 16-КиБ полезной нагрузке.

Payload по видам:

```
PageFrame:       graph_id(u64) + file_kind(u8) + _pad(7) + page_no(u64) + page_bytes(PAGE_SIZE)
                 Total = 16408 байт
PageInit:        graph_id(u64) + file_kind(u8) + _pad(1) + page_type(u16) + _pad(4) + page_no(u64)
TxBegin:         (пусто)
TxCommit:        commit_ts(u64) + frame_count(u32) + _pad(4)
TxAbort:         (пусто)
CheckpointBegin: (пусто)
CheckpointEnd:   checkpoint_lsn(u64) + dpt_len(u32) + [ (graph_id u64, file_kind u8, _pad u8[7],
                                                         page_no u64, rec_lsn u64) × N ]
GraphCreate:     graph_id(u64) + name_len(u16) + name_utf8(variable)
GraphDrop:       graph_id(u64)
```

**`PageKey = (graph_id: u64, file_kind: u8, page_no: u64)`** — ключ индекса версий страниц. `graph_id = SYSTEM_GRAPH_ID = 0` адресует системное пространство (каталог схемы, каталог графов, параметры, функции), поэтому DDL и данные покрываются одним механизмом.

`PageInit` вместо `PageFrame` для страниц, выделенных **после** последнего checkpoint: у такой страницы нет предыдущего содержимого, которое стоило бы сохранять, а восстановить её можно из логики аллокации. Это убирает практически всю стоимость фреймов при массовой загрузке, где страницы создаются и заполняются один раз.

### 22.3. Индекс версий страниц

В памяти: `HashMap<PageKey, SmallVec<(Lsn, FrameOffset)>>` — для каждой страницы список её версий в журнале. Чтение страницы на снимке `S`: взять самый свежий фрейм с `lsn ≤ S`; если такого нет — читать `.fgb`.

Стоимость — один хеш-поиск на промах буфер-пула. В read-heavy фазе после бэкфилла индекс пуст, поэтому доминирующая нагрузка не платит ничего. Память — порядка 16 байт на фрейм: миллион неотбэкфилленных страниц ≈ 16 МБ.

Индекс восстанавливается при старте одним проходом по журналу и не персистится отдельно.

### 22.4. WAL-first инвариант

1. Изменение страницы идёт в scratch-фрейм; исходная страница в `.fgb` не трогается.
2. Коммит: дописать фреймы транзакции → `TxCommit` → **один** `fdatasync` (независимо от числа затронутых графов и файлов).
3. Страница в `.fgb` по физическому адресу `L` записывается **только после** того, как все фреймы этой страницы с `lsn ≤ page_lsn` durable в журнале.
4. Журнал **никогда** не обрезается раньше `fsync` файлов данных.

Пункты 3–4 — это весь протокол долговечности; правило «не писать страницу раньше её WAL-записи» выполняется автоматически, поскольку запись в `.fgb` бывает только при бэкфилле уже durable фреймов.

---

## 23. Checkpoint и Recovery

### 23.1. Checkpoint — бэкфилл фреймов

Checkpoint переносит закоммиченные фреймы из журнала обратно в `.fgb` **на их штатные места** и затем обрезает журнал.

```
1. WAL: append CheckpointBegin
2. Для каждого графа (независимо, в любом порядке):
     для каждого фрейма с lsn ≤ commit_watermark и lsn > page_lsn страницы:
         записать образ страницы по адресу page_no в соответствующий .fgb
         page_lsn := lsn фрейма
3. fsync каждого затронутого .fgb
4. Обновить пографовые водяные знаки: GraphSuperblock.backfilled_lsn
5. WAL: append CheckpointEnd { checkpoint_lsn, dirty_page_table }
6. WAL: fsync
7. Атомарно обновить db.fgb (глобальный checkpoint_lsn), tmp → rename
8. prune_before(prunable_prefix) — обрезать журнал
```

**Что копируется.** Только фреймы транзакций, у которых есть маркер `TxCommit`. Фрейм незавершённой транзакции физически не может попасть в файл данных — отсюда свойство NO-STEAL на уровне `.fgb` без каких-либо ограничений на размер транзакции.

**Что можно делать пографово, а что нельзя.** Бэкфилл — пографовая работа и планируется независимо для каждого графа. Глобальна только **обрезка**:

```
prunable_prefix = min( min по живым графам (backfilled_lsn),
                       oldest_reader_mark,
                       oldest_active_tx_first_frame )
```

Это стандартное ограничение любого общего журнала: PostgreSQL, InnoDB и RocksDB держат общий лог до минимума по всем объектам. Отсюда же следует патология: один граф, в который давно не было checkpoint'а, удерживает журнал целиком. Защита — `FG_WAL_MAX_TOTAL_BYTES`: при превышении принудительно запускается бэкфилл отстающего графа (аналог `max_total_wal_size` в RocksDB).

**Идемпотентность и прерываемость.** Шаг 2 — чистый `memcpy` под защитой `page_lsn`, поэтому checkpoint можно прервать и перезапустить в любой момент без последствий. Обрезка (шаг 8) выполняется строго после `fsync` файлов данных: пока файл не синхронизирован, фреймы обязаны оставаться в журнале.

**Долгие читатели.** Читатель удерживает `oldest_reader_mark` и тем самым тормозит обрезку — журнал растёт. Меры: `FG_MAX_SNAPSHOT_AGE_SEC` принудительно закрывает устаревшие снимки; режим checkpoint `Restart` блокирует новых читателей, чтобы дать журналу схлопнуться; для долгой аналитики предусмотрен режим 3 (`query_to_memory`), который материализует подграф в память и **освобождает снимок**, вместо того чтобы держать его открытым весь расчёт.

### 23.2. Recovery

Восстановление — один упорядоченный проход по журналу. Отдельных фаз analysis/redo/undo нет, каталога процедур на каждый вид записи нет.

```
1. Прочитать db.fgb → checkpoint_lsn (при повреждении — из .tmp, иначе FATAL).
2. Проход по журналу с checkpoint_lsn:
     построить индекс версий страниц: PageKey → [(lsn, offset)]
     зафиксировать множество транзакций с маркером TxCommit
     отметить graph_id, удалённые записями GraphDrop
3. Отбросить фреймы транзакций без маркера коммита (это и есть весь «откат»).
4. Применить закоммиченные фреймы в порядке возрастания LSN:
     пропустить фреймы для удалённых graph_id
     применить, если lsn > page_lsn страницы (или страница повреждена — см. §23.4)
5. fsync файлов данных.
6. Записать свежий checkpoint.
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
2. Assign commit_ts (глобальный счётчик БД)
3. WAL: append фреймы транзакции
4. WAL: append TxCommit { commit_ts, frame_count }
5. WAL: fsync (fdatasync)               ← один на всю транзакцию
6. MvccManager: commit(tx)              ← версии становятся видимы
7. Бэкфилл — позже, при checkpoint
```

Один `fsync` покрывает транзакцию целиком, сколько бы графов, таблиц и файлов она ни затронула.

### 23.4. Torn write

```
При чтении страницы из .fgb:
  expected = xxhash3_64(page_bytes с обнулённым полем checksum)
  if page_header.checksum != expected || page_header.page_no != ожидаемый:
    → страница повреждена
    → если в журнале есть фрейм этой страницы с lsn > checkpoint_lsn:
        применить фрейм (полный образ) — страница восстановлена
    → иначе:
        MetaGraphError::PageCorrupt
```

Восстановление всегда возможно, пока фрейм не обрезан, потому что **фрейм — это полный образ**, а не дельта. Именно поэтому обрезка журнала обязана следовать строго после `fsync` файлов данных (§22.4, п. 4): нарушение этого порядка — единственный способ получить невосстановимую рваную страницу.

### 23.5. Изоляция и чтение на снимке

Снимок транзакции — значение `snapshot_lsn` из глобальной последовательности БД. Разрешение чтения страницы:

```
read_page(PageKey, snapshot_lsn):
    frames = page_version_index[PageKey]
    взять самый свежий фрейм с lsn ≤ snapshot_lsn
    если найден → вернуть его образ
    иначе       → прочитать страницу из .fgb
```

Отсюда следуют свойства, которых у модели дельт не было:
- **Снимок консистентен по всей базе**, а не по одному графу: `snapshot_lsn` сравним с `commit_ts` любой транзакции любого графа.
- **Читатели не блокируют писателя и наоборот**: писатель добавляет новые фреймы, читатель смотрит на префикс журнала.
- **Обрезка учитывает читателей**: живой снимок удерживает `oldest_reader_mark`.

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

Это распространение на hot-страницы того, что уже описано для label-индекса (§24.1) и кучи свойств (§24.2).

**Полный дефраг (§24.3) остаётся, но как редкая офлайн-операция** — ровно для второго сценария, когда нужно вернуть место операционной системе. Он требует эксклюзивного доступа и checkpoint'а с обеих сторон: ни один читатель не может удерживать снимок поперёк `slot_remap`.

### 24.1. Label Index Compaction

**Триггер**: tombstone ratio (fragmentation) > 20% (более 20% записей в label index — tombstone-маркеры).

```
1. Создать tmp файл
2. Потоково скопировать только живые DiskAtomRef (без tombstone-маркеров)
3. fsync tmp → rename
4. WAL: IndexCompact { index_id }
```

### 24.2. PropHeap Compaction

**Триггер**: среднее `free_space / PAGE_SIZE > 40%`.

```
1. Allocate новые страницы
2. Для каждого живого атома:
   a. Read PropertyMap из старой страницы
   b. Write в новую страницу (компактно)
   c. Update props_page + props_slot в NodeHotSlot / EdgeHotSlot
   d. WAL: PropSet + NodeHotUpdate (new props_ref)
3. Update freelist
4. Truncate / reclaim старые страницы
5. Checkpoint
```

### 24.3. Полный дефрагментатор (Manual COMPACT)

**Триггер**: `COMPACT GRAPH <name>` или tombstone ratio > 30% в hot файле (много удалений без переиспользования).

```
1. Построить slot_remap: HashMap<old_slot, new_slot> для живых атомов;
2. Записать новый nodes_hot.fgb в tmp: переупорядочить слоты;
3. Для каждого EdgeHotSlot:
   - inv_inline[], out_inline[] — применить remap к каждому элементу;
   - inv_sg_slot, out_sg_slot — это GraphId, не DiskAtomRef, не трогать;
4. Применить remap к AdjacencyOverflowPage, CrossLevelAdjacencyOverflowPage (local_slot), EdgeIncidencePage (peer_disk_ref), CrossLevelEdgeIncidencePage;
5. NodeHotSlot adjacency (adj_out, adj_in, adj_undir) — это edge slots,
   применить remap если рёбра тоже дефрагментируются
6. EdgeEndpointIndex — перестроить полностью (endpoint_ref и edge_ref оба меняются)
7. LabelIndex pages — применить remap к каждому DiskAtomRef
8. Все B+tree индексы (primary, standard, unique):
   - leaf values (DiskAtomRef) — применить remap
   - ee_endpoint.fgidx — перестроить
9. SubgraphDirPage — sg_slot это GraphId арены подграфов, не DiskAtomRef;
   если подграфы тоже дефрагментируются — отдельная процедура
10. Changefeed entries — atom_ref применить remap
11. Обновить GraphSuperblock
12. fsync всего → atomic rename
```

Дорогая операция; только по явному запросу или при запуске после длительной эксплуатации по расписанию.

### 24.4. Алгоритм освобождения цепочек страниц.

Цепочки страниц используются в: `PropLargeChunkPage`, `AdjacencyOverflowPage` (и cross-level варианте), `EdgeIncidencePage`,
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
    // После этого обновить NodeHotSlot/EdgeHotSlot:
    // props_page = NULL_PAGE, props_slot = NULL_SLOT, props_len = 0
}
```

Для `PropHeapPage` (normal records) освобождение отдельной записи не возвращает страницу в freelist немедленно: 
ставится tombstone в `SlotEntry`, страница помечается dirty. 
Возврат страницы в freelist — только при compaction, когда все `SlotEntry` на странице имеют tombstone.

---

## 25. Адресация и навигация

### 25.1. Формулы адресации

```
Слоты вершин и рёбер имеют одинаковый размер, поэтому формула общая:

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

```rust
fn load_subgraph(disk: &DiskMetaGraph, tx: &MvccTransaction, atom_slots: &[u32]) -> io::Result<MetaGraph> {
    let mut g = MetaGraph::with_capacity(atom_slots.len(), atom_slots.len() * 4);
    let mut slot_to_atom: HashMap < u32, AtomId > = HashMap::new();
    
    // Pass 1: load nodes
    for & slot in atom_slots {
        let hot = disk.node_hot(slot)?;
        if hot.kind_flags & TOMBSTONE != 0 { continue; }
      
        let id = AtomId::new_node(slot, hot.gen);
        let label = if hot.label_id != NULL_LABEL {
            Some(disk.label_dict.lookup_node(hot.label_id) ? )
        } else { 
            None 
        };
      
        let props = if hot.props_page != NULL_PAGE {
            disk.read_props(hot.props_page, hot.props_slot) ?
        } else { 
            PropertyMap::default() 
        };
        // ... add node to g ...
        slot_to_atom.insert(slot, id);
    }
    
    // Pass 2: load edges between loaded nodes
    // ... similar pattern ...
    
    Ok(g)
}
```

### 25.4. Прямое чтение слота (без BufferPool)

```rust
const SLOTS_PER_HOT_PAGE: u64 = 102;
const HOT_SLOT_SIZE:      u64 = 160;

fn hot_page_no(slot: u64) -> u64   { slot / SLOTS_PER_HOT_PAGE }
fn hot_offset(slot: u64)  -> usize { (32 + (slot % SLOTS_PER_HOT_PAGE) * HOT_SLOT_SIZE) as usize }
```

---

## 26. Тензорные представления

Дисковый формат хранения матричных и тензорных представлений графов, описанных в [gql_spec.md](gql_spec.md) §5.11.

### 26.1. Режимы хранения

| Режим          | Описание                               | Применение                     |
|----------------|----------------------------------------|--------------------------------|
| On-the-fly     | Вычисляется из NodeHotPage/EdgeHotPage | По умолчанию — без кеширования |
| Cached dense   | Хранится как бинарная матрица на диске | ML/analytics workloads         |
| Cached sparse  | CSR-формат на диске                    | Большие разреженные            |

### 26.2. TensorCachePage (для cached-режима)

```
PageHeader(32) (magic=MAGIC_TENSOR, type=TensorCache)
  tensor_kind:  u8    -- 0=endpoint_source, 1=endpoint_target,
                      -- 2=participation_source, 3=participation_target,
                      -- 4=participation_control
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
| `FG_ADJ_INLINE_OUT`        | usize | 16      | Inline out-adj per node (NodeHotSlot.adj_out[])         |
| `FG_ADJ_INLINE_IN`         | usize | 16      | Inline in-adj per node (NodeHotSlot.adj_in[])           |
| `FG_ADJ_INLINE_UNDIR`      | usize | 8       | Inline undir-adj per node (NodeHotSlot.adj_undir[])     |
| `FG_EDGE_INLINE_ENDPOINTS` | usize | 4       | Inline endpoints per edge side (EdgeHotSlot.*_inline[]) |

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

| Параметр                   | Тип   | Default    | Описание                                                                                          |
|----------------------------|-------|------------|---------------------------------------------------------------------------------------------------|
| `FG_WAL_FRAME_SPILL_PAGES` | usize | 1024       | Порог грязных фреймов транзакции, после которого они спиллятся в журнал                           |
| `FG_WAL_CHECKPOINT_FRAMES` | usize | 4096       | Порог автоматического запуска бэкфилла                                                            |
| `FG_WAL_MAX_TOTAL_BYTES`   | u64   | 1073741824 | Жёсткий предел размера журнала (1 ГиБ) → принудительный бэкфилл отстающего графа                  |
| `FG_WAL_SYNC_MODE`         | enum  | `full`     | `full` \| `normal` \| `off`                                                                       |
| `FG_MAX_SNAPSHOT_AGE_SEC`  | u64   | 300        | Возраст снимка, после которого он принудительно закрывается, чтобы разблокировать обрезку журнала |

**Сборка индексов и массовая загрузка** (§20.13)

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

| Параметр                        | Тип   | Default | Описание                                                    |
|---------------------------------|-------|---------|---------------------------------------------------------------|
| `FG_STATS_HISTOGRAM_BUCKETS`    | usize | 100     | Число корзин equi-depth гистограммы                          |
| `FG_STATS_SAMPLE_ROWS`          | usize | 30000   | Размер выборки для `ANALYZE`, не зависит от размера таблицы  |
| `FG_STATS_FULL_SCAN_THRESHOLD`  | usize | 32      | Таблицы меньше N страниц анализируются полным сканом         |

---

## 28. Эволюция формата

### 28.1. Версионирование

`STORAGE_FORMAT_VERSION` хранится в `GraphSuperblock.format_version`.

| Условие                                       | Действие                       |
|-----------------------------------------------|--------------------------------|
| `superblock.format_version > ENGINE_VERSION`  | Error: downgrade not supported |
| `superblock.format_version < ENGINE_VERSION`  | Run migration routine          |
| `superblock.format_version == ENGINE_VERSION` | Open normally                  |

### 28.2. Правила совместимости

- `_reserved` поля: обнулить при записи, игнорировать при чтении.
- Новые `PageType` в страницах: старая версия возвращает `IoError::UnknownPageType` (не panic).
- Новые `WalKind`: старые WAL readers обязаны пропускать неизвестные entries с предупреждением (не panic).
- Новые TypeTag для Value: при чтении неизвестного тега возвращать `Value::Bytes(raw_bytes)`.

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
> `DiskAtomRef (u32)` → `u64` (§9). Актуализированные формулировки — в таблице решений в начале
> плана (помечены 🔄).

---
