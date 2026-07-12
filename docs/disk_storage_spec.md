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
| Стабильный дисковый ID          | `DiskAtomRef = u32` (kind-бит + slot-индекс, без поколения)                       |
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

| Тег  | GQL тип                     | Описание                             | Размер на диске            |
|------|-----------------------------|--------------------------------------|----------------------------|
| 0x00 | `none`                      | Поле отсутствует                     | 1 байт (тег)               |
| 0x01 | `null`                      | Поле есть, значение пусто            | 1 байт                     |
| 0x02 | `bool (false)`              | Логическое                           | 1 байт                     |
| 0x03 | `bool (true)`               | Логическое                           | 1 байт                     |
| 0x04 | `int`                       | i64                                  | 2..11 байт (LEB128 zigzag) |
| 0x05 | `float`                     | f64 IEEE 754                         | 9 байт                     |
| 0x06 | `decimal`                   | Произвольная точность (128-bit)      | 17 байт                    |
| 0x07 | `string`                    | UTF-8                                | 1+len(uLEB128)+N байт      |
| 0x08 | `bytes`                     | Произвольные байты                   | 1+len(uLEB128)+N байт      |
| 0x09 | `datetime`                  | RFC 3339 + timezone                  | 1+8+4+tz байт              |
| 0x0A | `duration`                  | Временной интервал (знаковый)        | 3..18 байт (переменная)    |
| 0x0B | `uuid`                      | UUID v7 (128 бит)                    | 17 байт                    |
| 0x0C | `ulid`                      | ULID (128 бит)                       | 17 байт                    |
| 0x10 | `record_id`                 | RecordId (table:id)                  | 1+переменная               |
| 0x11 | `array<T>`                  | Типизированный массив                | 1+4+Σ(elements)            |
| 0x12 | `set<T>`                    | Множество уникальных                 | 1+4+Σ(elements)            |
| 0x13 | `tuple<...>`                | Кортеж                               | 1+1+Σ(elements)            |
| 0x14 | `object`                    | Map ключ→значение                    | 1+4+Σ(key_id+value)        |
| 0x15 | `option<T>`                 | Some(T) или None                     | 1+[value]                  |
| 0x16 | `range<T>`                  | Диапазон [lo..hi] с флагами          | 1+flags+[lo]+[hi]          |
| 0x21 | `geometry::Point`           | Точка (lon, lat)                     | 17 байт                    |
| 0x22 | `geometry::LineString`      | Ломаная                              | 1+4+N×16 байт              |
| 0x23 | `geometry::Polygon`         | Многоугольник + holes                | переменная                 |
| 0x24 | `geometry::MultiPoint`      |                                      | переменная                 |
| 0x25 | `geometry::MultiLineString` |                                      | переменная                 |
| 0x26 | `geometry::MultiPolygon`    |                                      | переменная                 |
| 0x27 | `geometry::Collection`      | GeometryCollection                   | переменная                 |
| 0x31 | `vector_f64`                | Вектор f64 (для HNSW)                | 1+2+N×8 байт               |
| 0x32 | `vector_f32`                | Вектор f32                           | 1+2+N×4 байт               |
| 0x33 | `vector_i64`                | Вектор i64                           | 1+2+N×8 байт               |
| 0x34 | `vector_i32`                | Вектор i32                           | 1+2+N×4 байт               |
| 0x35 | `vector_i16`                | Вектор i16                           | 1+2+N×2 байт               |
| 0x36 | `atom_ref`                  | disk_atom_ref(u32 LE)                | 1 + 4 байт                 |
| 0x37 | `graph_ref`                 | sg_slot(u32) + sg_gen(u32)           | 1 + 8 байт                 |
| 0x38 | `qualified_atom_ref`        | sg_slot(u32 LE) + local_slot(u32 LE) | 1 + 8 байт                 |

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

Используется формат **IEEE 754-2008 decimal128** (16 байт): coefficient + exponent + sign.
Это даёт:
- Точность: 34 значимых десятичных цифры.
- Диапазон: ±9.999...×10^6144.
- Поддержку NaN, ±Infinity, ±0.

На диске: тег(1) + 16 байт little-endian = 17 байт.

Реализация в Rust: крейт `rust_decimal` или `bigdecimal` для расширенной точности.

### 3.4. DateTime и Duration

**DateTime** хранится как:

- `seconds: i64` — Unix timestamp в секундах (UTC).
- `nanos: u32` — субсекундная точность `[0 .. 999_999_999]`.
- `tz_id: u32`  -- кодирование часового пояса (см. ниже)

Кодирование tz_id (u32):
- `0` = UTC (по умолчанию)
- `1 .. 999_999` = ID пояса в IANA timezone dictionary (§16, отдельная секция).
Хватит на любые пополнения tzdb (2024 год: ~600 поясов).
- `1_000_000 .. 1_172_799` = Фиксированное UTC-смещение.
Декодирование: offset_seconds = tz_id − 1_000_000 − 86_399 (86_399 — сдвиг для представления отрицательных смещений).
Диапазон: `−86_399 .. +86_399` секунд (≈ ±24 ч с запасом). Реальный диапазон UTC-смещений: `−43_200 .. +50_400` с.

Итоговый размер DateTime на диске: `тег(1) + header(2) + [uLEB128 per unit × 0..10] = 3..22 байт`.

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

На диске: DateTime = тег(1) + i64 + u32 + u32 = 17 байт; Duration = тег(1) + i32 + i32 + i64 = 17 байт.

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
- `atom → tag(u8) + DiskAtomRef(u32)`
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
- Если bit 31 = 0 (DiskAtomRef.kind=Node) или 1 (kind=Edge) без флага `HAS_QUALIFIED` — обычный DiskAtomRef.
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
decimal             →  0x06 + 16 байт (decimal128 LE)
string              →  0x07 + len(uLEB128) + utf8_bytes
bytes               →  0x08 + len(uLEB128) + raw_bytes
datetime            →  0x09 + i64(seconds LE) + u32(nanos LE) + u32(tz_id LE)
duration            →  0x0A + header(u16 LE) + [uLEB128 per unit × count]
uuid                →  0x0B + 16 байт (raw big-endian)
ulid                →  0x0C + 16 байт (raw big-endian)
record_id           →  0x10 + table_id(u32 LE) + id_kind(u8) + [id_payload]
array               →  0x11 + count(uLEB128) + [Value]×count
set                 →  0x12 + count(uLEB128) + [Value sorted]×count
tuple               →  0x13 + arity(u8) + [Value]×arity
object              →  0x14 + count(uLEB128) + [(key_id u32 LE + Value)]×count
option              →  0x15 + 0x00 (none) | 0x15 + 0x01 + Value
range               →  0x16 + flags(u8) + [lo: Value] + [hi: Value]
point               →  0x21 + f64(lon LE) + f64(lat LE)
linestr             →  0x22 + count(u32 LE) + [f64 lon + f64 lat]×count
polygon             →  0x23 + ring_count(u16 LE) + [count(u32 LE) + [f64×2]×count]×ring_count
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
        wal/
          <lsn_hex>.wal                 -- WAL сегменты
        snapshot/
          <lsn_hex>.snap                -- MVCC snapshots
        mvcc.chk                        -- CheckpointHeader
```

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
0x0008  EdgeIncidencePage
0x0009  CrossLevelEdgeIncidencePage
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

`AtomId` (u64 из `id.rs`) содержит поколение (gen, биты 63..32) — runtime-концепция для генерационной инвалидации.
На диске поколение избыточно: оно хранится внутри hot-слота и восстанавливается при загрузке.

**DiskAtomRef = u32**

```
бит  31       kind        0=Node, 1=Edge
биты 30..0    slot_index  u31 (до 2 147 483 648 на каждый kind)
```

Null-значение: `NULL_DISKATOMREF = u32::MAX = 0xFFFF_FFFF`

```rust
// AtomId → DiskAtomRef
fn to_disk_ref(id: AtomId) -> DiskAtomRef {
    let slot = id.slot() as u32;        // bits 30..0
    let kind = id.is_edge() as u32;     // bit 31
    (kind << 31) | slot
}

// DiskAtomRef → AtomId (требует восстановленного поколения из hot-слота)
fn from_disk_ref(r: DiskAtomRef, gen: u32) -> AtomId {
    let slot = r & 0x7FFF_FFFF;
    if (r >> 31) == 0 { AtomId::new_node(slot, gen) } else { AtomId::new_edge(slot, gen) }
}
```

---

## 10. NodeHotPage — горячие данные вершин

### 10.1. NodeHotSlot (256 байт, фиксированный)

```
Offset  Size  Тип        Поле             Описание
0       4     u32        gen              Поколение для восстановления AtomId
4       4     u32        subgraph_slot    GraphId.index() если is_meta; NULL_SLOT иначе
8       4     u32        label_id         Интернированный id метки; NULL_LABEL = нет
12      4     u32        table_id         Таблица, которой принадлежит запись; NULL_TABLE = нет
16      2     u16        adj_out_count    Общее кол-во исходящих рёбер (inline + overflow)
18      2     u16        adj_in_count     Общее кол-во входящих рёбер
20      2     u16        adj_undir_count  Общее кол-во неориентированных рёбер
22      1     u8         kind_flags       Биты: [7]=is_meta; [6]=tombstone; [5]=visited;
                                          [4]=port; [3]=in_result; [2]=cut_vertex; [1]=reserved
23      1     u8         _pad                                     
24      8     u64        overflow_out     page_no overflow для out; NULL_PAGE = нет
32      8     u64        overflow_in      page_no overflow для in
40      8     u64        overflow_undir   page_no overflow для undirected
48      8     u64        props_page       page_no в props.fgb; NULL_PAGE = нет
56      4     u32        props_slot       Индекс слота в SlotDirectory PropHeapPage
60      4     u32        props_len        Длина сериализованного PropertyMap
64      64    [u32; 16]  adj_out          Inline исходящие: edge slot indices; NULL_SLOT = конец
128     64    [u32; 16]  adj_in           Inline входящие
192     32    [u32;  8]  adj_undir        Inline неориентированные
224     32    —          _reserved        Зарезервировано (выравнивание до 256 байт); обнулить при записи
Total = 256 байт
```

**Инварианты:**

- Если `kind_flags.tombstone = 1`, остальные поля не гарантированы (кроме `gen`).
- Если `adj_out_count > 16`, первая overflow-страница: `overflow_out`.
- `adj_out[]` заполняется с индекса 0; `NULL_SLOT` — конец inline части.
- `table_id` используется планировщиком для быстрой фильтрации по таблице.

### 10.2. NodeHotPage layout

```
Offset   Size   Содержимое
0        32     PageHeader (magic=MAGIC_NODE_HOT, type=NodeHot)
32       16128  63 × NodeHotSlot (63 × 256 = 16128)
16160    224    _padding
Total = PAGE_SIZE байт
```

**Слотов на страницу:** 63  
**Покрытие:** NodeHotPage[k] содержит слоты `[63k .. 63k+62]`.  
**Смещение слота:** `page_header_size + slot_within_page * 256`

**Адресация:**

```
page_no          = slot_index / 63
slot_within_page = slot_index % 63
byte_offset      = 32 + slot_within_page × 256
```

---

## 11. EdgeHotPage — горячие данные рёбер

### 11.1. EdgeHotSlot (128 байт, фиксированный)

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
2       1     u8         inv_count        Кол-во invertex конечных точек (total, ≤4 inline)
3       1     u8         out_count        Кол-во outvertex конечных точек (total, ≤4 inline)
4       4     u32        gen              Поколение
8       4     u32        label_id         Интернированный id метки; NULL_LABEL = нет
12      4     u32        table_id         Таблица (для RELATION-таблиц)
16      8     f64        weight           NaN = нет веса
24      4     u32        transition_gid   GraphId.index() для MetaEdge; NULL_SLOT = нет
28      1     u8         edge_kind_ext    Расширенная классификация:
                                            bit 0: HAS_TRANSITION   -- transition_gid != NULL_SLOT
                                            bit 1: HAS_INCIDENCES   -- inc_page != NULL_PAGE
                                            bits 2-7: reserved (0)
29      3     u8[3]      _pad
32      16    [u32; 4]   inv_inline       Inline invertex: DiskAtomRef (QualifiedAtomRef при CROSS_LEVEL)
48      16    [u32; 4]   out_inline       Inline outvertex: DiskAtomRef (QualifiedAtomRef при CROSS_LEVEL)
64      8     u64        inv_overflow     page_no overflow для invertex
72      8     u64        out_overflow     page_no overflow для outvertex
80      8     u64        inc_page         page_no EdgeIncidence list; NULL_PAGE = нет
88      8     u64        props_page       page_no в props.fgb
96      4     u32        props_slot
100     4     u32        props_len
104     4     u32        inv_sg_slot      GraphId.index() для inv_inline[0]; NULL_SLOT = тот же граф (не cross-level)
108     4     u32        out_sg_slot      GraphId.index() для out_inline[0]; NULL_SLOT = тот же граф
112     8     u64        in_sg_overflow   page_no overflow для inv_sg_slot
120     8     u64        out_sg_overflow  page_no overflow для out_sg_slot
Total = 128 байт
```

**Инварианты:**
- При `topo ∈ {Directed, Undirected, Bidirectional, Loop}`: `inv_count + out_count = 2`.
- При `topo ∈ {HyperDirected, HyperUndirected}`: `inv_count + out_count ≥ 2`.
- При `topo ∈ {Undirected, HyperUndirected}`: все вершины в `inv_inline`; `out_count = 0`.
- При `inv_count > 4` или `out_count > 4` — соответствующий overflow активен.
- `inc_page != NULL_PAGE` означает MetaEdge (edge-to-edge инциденции в отдельной странице).

**Хранение cross-level endpoints:**
- Для CROSS_LEVEL=0: inv_inline/out_inline содержат DiskAtomRef (slot в пространстве текущего графа) - стандартное поведение.
- Для CROSS_LEVEL=1: inv_inline/out_inline содержат DiskAtomRef атомов **в пространстве подграфа, которому они принадлежат**. Чтобы получить полный QualifiedAtomRef, нужен sg_slot их подграфа.
  inv_sg_slot/out_sg_slot покрывают простейший случай: одно inv и одно out с разными уровнями.
  Для гиперрёбер с несколькими cross-level endpoints — полные QualifiedAtomRef хранятся в overflow-странице in_sg_overflow/out_sg_overflow (CrossLevelAdjacencyOverflowPage).

### 11.2. EdgeHotPage layout

```
Offset   Size   Содержимое
0        32     PageHeader (magic=MAGIC_EDGE_HOT, type=EdgeHot)
32       16256  127 × EdgeHotSlot (127 × 128 = 16256)
16288    96     _padding
Total = PAGE_SIZE байт
```

**Слотов на страницу:** 127  
**Смещение слота:** `32 + slot_within_page * 128`

**Адресация:**

```
page_no          = slot_index / 127
slot_within_page = slot_index % 127
byte_offset      = 32 + slot_within_page × 128
```

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
48      16336   —      entries    Массив u32 (edge slot index или DiskAtomRef)
                                  → 16336 / 4 = 4084 entries per page
Total = PAGE_SIZE байт
```

**Важно:** entries в overflow-странице являются **продолжением** inline-массива из hot-слота.
Читать: сначала inline из hot-слота, затем последовательно все страницы цепочки.

**Ёмкость:** Для узла со степенью 100,000:
- Inline: 16 рёбер в NodeHotSlot
- Overflow: ceil(99984 / 4084) = 25 страниц цепочки

### 12.1. CrossLevelAdjacencyOverflowPage

То же самое, что и AdjacencyOverflowPage, но имеет другой вид entries для overflow-страницы с несколькими cross-level endpoints:

```
PageHeader: (magic=MAGIC_ADJ_OVER_CL, type=CrossLevelAdjacencyOverflow)
```

```
overflow_entry_cl (12 байт):
      sg_slot:    u32   -- GraphId.index() подграфа
      local_slot: u32   -- DiskAtomRef в пространстве подграфа
      role:       u8    -- 0=inv, 1=out
      _pad:       u8[3]
```

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
40      16336 —      [SlotDirectory ... GAP ... records]
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
цепочка `PropLargePage` с `next_page` указателями.

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
48      16328   bits   Bitmap               1 бит = 1 страница; 1=свободна, 0=занята
  → 16328 × 8 = 130,624 страницы покрыто одной FreelistPage
  → 130,624 × PAGE_SIZE = 2 ТиБ покрытия на один freelist
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

### 16.2. LabelDictHeader (Page 0, offset 32..160)

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
304     80    —      _reserved
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
                   -- 4=HNSW, 5=Geometry, 6=Reachability, 7=Neighbourhood
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

### 19.5. Обновление статистики

- **Инкрементально**: при каждом INSERT/DELETE обновляются счётчики в LabelStatsPage.
- **Полный пересчёт**: `ANALYZE TABLE @name` — полное сканирование + пересчёт гистограмм.
- **Автоматический trigger**: при `dirty_row_fraction > 0.10` (10% изменений с последнего ANALYZE).

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
decimal   → sign_byte(u8: 0x00=neg,0x01=pos) + 16 байт big-endian coefficient + exponent (order-preserving)
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

── Составные типы ──────────────────────────────────────────────────────
```
option<T> → 0x00 (None, сортируется первым) | 0x01 + encode(T)
array<T>  → element-wise: encode(elem[0]) + 0x01 + encode(elem[1]) + ... + 0x00
set<T>    → то же что array, элементы уже отсортированы (set = sorted unique)
tuple     → encode(field[0]) + encode(field[1]) + ...
            (каждый компонент self-delimiting через TypeTag-декодирование)
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
[34..36]                          _pad
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
  `44 + 2 × num_keys + key_area_size + 8 × (num_keys + 1) ≤ PAGE_SIZE` 

Доступный для ключей объём: `PAGE_SIZE − 36 − 2n − 8(n+1)`.
Свободное место: `child_pages_start − key_area_end`.

```
Binary search (O(log num_keys)):
  decode key[mid] из key_area[key_offsets[mid]..] → compare → сдвинуть границы
```

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
48      16304  [DiskAtomRef(u32)] × (count) → (PAGE_SIZE - 48) / 4 = 4084 entries per page
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
- Составные типы (array, set, tuple): self-delimiting через TypeTag + length prefix.

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
48      16304   [DiskAtomRef(u32)] × count  → (PAGE_SIZE - 48) / 4 = 4084 per page
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
48      var    entries: [DiskAtomRef(u32) + term_freq(u16) + _pad(2)] × count
                  → (PAGE_SIZE - 48) / 8 = 2042 entries per page
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
data:       [u8] × 16304   -- сырые байты сериализованного HNSW
```

**Memory management**: HNSW целиком в памяти до `FG_HNSW_CACHE_SIZE` (default 256 МиБ).
При превышении — LRU eviction по индексам (не узлам).

**Поддерживаемые метрики**: EUCLIDEAN, COSINE, MANHATTAN, MINKOWSKI, пользовательская функция.

**Supported element types**: F64, F32, I64, I32, I16 (совпадает с `gql_spec.md §9.6`).

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
          + DiskAtomRef(u32) + _pad(4)] × num_entries
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
Ключ leaf-записи = DiskAtomRef(u32) вершины. Значение зависит от алгоритма:
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
  48      16336   landmarks: DiskAtomRef(u32) × count
  -- per page: 16336 / 4 = 4084 landmark-вершины
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

  B+tree (exact_roots[k-1]): ключ = DiskAtomRef(u32) центра, значение = page_no первой NeighbourhoodListPage.

```
NeighbourhoodListPage:
    Offset  Size    Содержимое
    0       32      PageHeader (MAGIC_IDX_NBH, type=IndexNeighbourhoodExact)
    32      4       center_ref: DiskAtomRef(u32)
    36      1       k (u8)
    37      3       _pad
    40      8       next_page (u64)
    48      4       count (u32)
    52      4       _pad
    56      16328   neighbors: DiskAtomRef(u32) × count
    -- per page: 16328 / 4 = 4082 соседей
```

#### SKETCH — HyperLogLog Pages

B+tree (sketch_root): составной ключ = k(u8) + DiskAtomRef(u32) = 5 байт, значение = hll_sketch(u8[64]).

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
    ключ: landmark_idx(u8) + DiskAtomRef(u32) = 5 байт
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

```
LeafEntry:
  PageHeader (magic=MAGIC_IDX_EE, type=IndexEdgeEndpointEntry)
  endpoint_ref:  u32  -- DiskAtomRef ребра, выступающего endpoint'ом
  edge_ref:      u32  -- DiskAtomRef ребра, у которого оно endpoint
  role:          u8   -- 0=inv (Source), 1=out (Target)
  _pad:          u8[3]
```

**Создаётся автоматически** при наличии хотя бы одного ребра с is_edge(endpoint) = true.

**Использование при traversal:**
1. Текущий атом = ребро $e_1$ (AtomWalk-режим)
2. $`EdgeEndpointIndex.lookup(e_1)$` → список ($e_n$, role)
3. Для каждого $e_n$ → противоположные endpoint'ы из `EdgeHotSlot` → следующие атомы

**Participation-инцидентность (EdgeIncidence)** хранится в `EdgeIncidencePage` (уже определённой в §11.3), не в `EdgeEndpointIndex`. 
Два разных индекса для двух разных видов инцидентности.

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

### 22.1. Расширенный набор WalKind

Дисковый режим добавляет новые `WalKind`. Существующие (0x01–0x07) не изменяются.

```
// Добавить в WalKind:
NodeAlloc      = 0x10,   // Выделен slot вершины
EdgeAlloc      = 0x11,   // Выделен слот ребра
NodeFree       = 0x12,   // Tombstone вершины
EdgeFree       = 0x13,
NodeHotUpdate  = 0x14,   // Обновление NodeHotSlot (delta)
EdgeHotUpdate  = 0x15,   // Обновление EdgeHotSlot (delta)
AdjAppend      = 0x16,   // Добавление ребра в adjacency list
AdjRemove      = 0x17,   // Удаление ребра из adjacency list
PropSet        = 0x18,   // Установить / обновить PropertyMap
PropDelete     = 0x19,   // Удалить PropertyMap
IndexInsert    = 0x1A,   // Вставка в индекс
IndexDelete    = 0x1B,   // Удаление из индекса
IndexCompact   = 0x1C,   // Сжатие индекса
SchemaDef      = 0x1D,   // DDL: DEFINE
SchemaRemove   = 0x1E,   // DDL: REMOVE
PageAlloc      = 0x1F,   // Аллокация страницы в файле
PageFree       = 0x20,   // Освобождение страницы
StatsUpdate    = 0x21,   // Обновление статистики
ChangefeedPut  = 0x22,   // Запись в changefeed
```

### 22.2. Payload для каждого вида

```
NodeAlloc:     slot(u32) + gen(u32) + kind_flags(u8) + label_id(u32) + table_id(u32)
EdgeAlloc:     slot(u32) + gen(u32) + topo(u8) + label_id(u32) + table_id(u32)
NodeFree:      slot(u32) + gen(u32)
EdgeFree:      slot(u32) + gen(u32)
NodeHotUpdate: slot(u32) + field_mask(u32) + changed_bytes(variable)
               field_mask bits: 0=kind_flags, 1=label_id, 2=table_id,
                                3=adj_out_count, 4=adj_in_count, 5=adj_undir_count,
                                6=overflow_out, 7=overflow_in, 8=overflow_undir,
                                9=props_ref
EdgeHotUpdate: slot(u32) + field_mask(u32) + changed_bytes(variable)
AdjAppend:
  kind:       u8   -- 0=node_adj (V-E-V), 1=edge_endpoint (A-I-A)
  direction:  u8   -- 0=out/inv, 1=in/out, 2=undir
  new_count:  u16  -- итоговое значение adj_*_count после вставки
  atom_slot:  u32  -- слот атома-endpoint'а
  edge_slot:   u32  -- слот ребра, реализующего инцидентность
  Total = 12 байт
AdjRemove:
  kind:       u8   -- 0=node_adj (V-E-V), 1=edge_endpoint (A-I-A)
  direction:  u8   -- 0=out/inv, 1=in/out, 2=undir
  new_count:  u16  -- итоговое значение adj_*_count после удаления
  atom_slot:  u32  -- слот атома-endpoint'а
  edge_slot:   u32  -- слот удаляемого ребра
  Total = 12 байт
PropSet:       disk_atom_ref(u32) + props_len(u32) + encoded_props(variable)
PropDelete:    disk_atom_ref(u32)
IndexInsert:   index_id(u32) + key_len(u16) + encoded_key(variable) + disk_atom_ref(u32)
IndexDelete:   index_id(u32) + key_len(u16) + encoded_key(variable) + disk_atom_ref(u32)
SchemaDef:     kind(u8) + schema_id(u32) + def_len(u32) + def_bytes(variable)
SchemaRemove:  kind(u8) + schema_id(u32)
PageAlloc:     file_kind(u8) + page_no(u64)
PageFree:      file_kind(u8) + page_no(u64)
StatsUpdate:   stats_kind(u8) + table_id(u32) + delta_bytes(variable)
ChangefeedPut: table_id(u32) + entry_len(u32) + entry_bytes(variable)
```

### 22.3. WAL-first инвариант

Для каждой мутации:
1. WAL: append запись с operation details
2. BufferPool: обновить страницу (dirty), установить page_lsn = entry_lsn
3. При COMMIT: WAL fsync → данные durable
4. Страницы flush: при eviction или checkpoint
   (только если wal_lsn для страницы ≤ persisted_wal_lsn — LSN последнего fsync'нутого WAL сегмента)

**Нарушение**: страница **никогда** не должна быть записана на диск раньше соответствующей WAL-записи.
Это обеспечивает `page_lsn ≤ persisted_wal_lsn` на диске.

### 22.4. Формат заголовка WAL-записи

Расширение существующего формата (без изменения). Текущий формат (`mvcc_persist.rs`):

```
[MVCE:4][lsn:8][tx_id:8][kind:1][payload_len:4][crc32:4] = 29 байт
```

Этот формат достаточен для дискового режима с новыми `WalKind` значениями.
`crc32` покрывает `lsn || tx_id || kind || payload_len || payload`.

---

## 23. Checkpoint и Recovery

### 23.1. Checkpoint (дисковый режим)

Checkpoint синхронизирует состояние BufferPool с WAL и обновляет суперблок.

Алгоритм disk-backed checkpoint:

1. Flush all dirty pages для каждого .fgb файла:
   для каждого файла: BufferPool::flush_all(file_id, backend)
2. fsync каждого .fgb файла (в порядке: props → hot → adj → labels → schema → stats → idx)
3. WAL: append CheckpointEnd { snapshot_lsn: current_lsn, dirty_pages: [] }
4. WAL: fsync активного сегмента
5. Атомарно обновить GraphSuperblock:
   superblock.checkpoint_lsn = current_lsn
   atomiс write (tmp → rename)
6. WAL rotation: start_new_segment()
7. prune_before(checkpoint_lsn): удалить старые WAL сегменты
8. (опционально) purge старых changefeed файлов

**Fuzzy checkpoint (non-blocking):**

- Шаги 1–2 выполняются фоново (dirty flush через BufferPool eviction clock).
- Транзакции продолжают работать; новые WAL-записи идут в активный сегмент.
- Шаги 3–8 выполняются после подтверждения flush всех страниц с `page_lsn ≤ checkpoint_lsn`.

### 23.2. Recovery (дисковый режим)

```
Алгоритм disk-backed recovery:

1. Открыть GraphSuperblock → checkpoint_lsn
   Если суперблок повреждён (checksum mismatch или page_no mismatch) →
   попытаться прочитать резервную копию superblock.fgb.tmp;
   если и она повреждена → FATAL ERROR (нет точки восстановления).
   
2. Загрузить все WAL сегменты через MultiSegmentReader после checkpoint_lsn.

3. Single-Pass REDO с inline torn-write detection:
   
   per_tx_ops: HashMap<TxId, Vec<DiskPageOp>>
   
   for (lsn, entry) in wal.iter_from(checkpoint_lsn):
     match entry:
       COMMIT(tx, commit_ts) →
         for each op in per_tx_ops[tx] (в порядке LSN):
           page = read_page(op.file_kind, op.page_no)

           -- Inline torn-write detection:
           if checksum(page) != stored_checksum(page)
              OR page.page_no != op.page_no:
             -- Страница повреждена (torn write).
             -- REDO применяем безусловно, игнорируем page_lsn.
             apply_op(op, page)
             set_page_lsn(page, lsn)
           else if lsn > page.page_lsn:
             -- Страница цела, но не содержит эту операцию.
             apply_op(op, page)
             set_page_lsn(page, lsn)
           -- else: lsn ≤ page_lsn → операция уже на диске, пропускаем.

         per_tx_ops.remove(tx)

       ABORT(tx) →
         per_tx_ops.remove(tx)    -- uncommitted → discard

       other(tx, op) →
         per_tx_ops[tx].push(op)  -- накапливаем до COMMIT/ABORT

4. Flush всех dirty страниц → fsync всех .fgb файлов.

5. Write fresh checkpoint.
```

Порядок применения WAL-записей при recovery имеет значение. 
Если в одной транзакции были `PageAlloc → NodeAlloc → AdjAppend`, то при recovery они применяются строго в порядке LSN. 
`PageAlloc` должен быть обработан раньше `NodeAlloc` — иначе freelist окажется в рассинхронизированном состоянии. 
Это гарантируется тем, что WAL пишется последовательно и итерируется по возрастанию LSN.

**Гарантии:**

- Uncommitted транзакции отбрасываются (нет UNDO фазы, нет CLR).
- `page_lsn` обеспечивает idempotency: повторный REDO не портит данные.
- torn write страницы обязательно восстанавливаются через REDO.

REDO-семантика AdjAppend (зависит от kind bit):
- Если `kind = 0`:
  1. Читаем `NodeHotSlot[node_slot]`
  2. Если `edge_slot` уже есть в `adj_*[]` или overflow — пропускаем (idempotent)
  3. Иначе вставляем: в inline-массив если место есть, иначе в overflow-цепочку
  4. Устанавливаем `adj_*_count = new_count`
- Если `kind = 1`:
  1. Прочитать `EdgeHotSlot[edge_slot]`
  2. Определить сторону: `direction=0 → inv_inline, direction=1 → out_inline`
  3. Проверить idempotency: если `ref_slot` уже присутствует в inline-массиве или в цепочке overflow для выбранного `direction` → пропустить (уже применено)
  4. Если место в inline-массиве (`inv_count < 4` или `out_count < 4`):
     a. Записать `ref_slot` в следующую свободную позицию inline-массива
     b. Инкрементировать `inv_count` или `out_count`
  5. Иначе (overflow):
     a. Найти последнюю страницу overflow-цепочки (`inv_overflow` или `out_overflow`)
     b. Если страница полна — аллоцировать новую `AdjacencyOverflowPage`, обновить `next_page`
     c. Записать `ref_slot` в entries новой/текущей overflow-страницы, инкрементировать count
  6. Установить `inv_count` или `out_count` = new_count из WAL-записи
  7. Пометить `EdgeHotSlot` страницу dirty, обновить `page_lsn`
  8. Если `ref_slot` является атомом типа Edge (bit31=1):
     a. Обновить `EdgeEndpointIndex`: вставить запись `{endpoint_ref=ref_slot, edge_ref=edge_slot, role=direction}`

REDO-семантика AdjRemove (зависит от kind bit):
- Если `kind = 0`:
  1. Читаем `NodeHotSlot[node_slot]`
  2. Если `edge_slot` не найден в `adj_*[]` и overflow — пропускаем (idempotent)
  3. Иначе удаляем: из inline swap-remove или из overflow
  4. Устанавливаем `adj_*_count = new_count`
- Если `kind = 1`:
  1. Прочитать `EdgeHotSlot[edge_slot]`
  2. Определить сторону: `direction=0 → inv_inline, direction=1 → out_inline`
  3. Проверить idempotency: если `ref_slot` отсутствует в inline и overflow → пропустить
  4. Если `ref_slot` в inline-массиве:
     a. swap_remove: заменить `ref_slot` на последний элемент inline-массива
     b. Декрементировать `inv_count` или `out_count`
  5. Иначе (в overflow):
     a. Найти страницу цепочки, содержащую `ref_slot`
     b. swap_remove внутри страницы: заменить `ref_slot` на последний entry страницы
     c. Если страница стала пустой — исключить из цепочки, освободить страницу в freelist
     d. Декрементировать count на overflow-странице
  6. Установить `inv_count` или `out_count` = new_count из WAL-записи
  7. Пометить dirty, обновить page_lsn
  8. Если `ref_slot` типа Edge:
     a. Удалить запись из `EdgeEndpointIndex`

Примечание: AdjAppend и AdjRemove содержат `new_count`, поэтому отдельный NodeHotUpdate для счётчика НЕ нужен.
Операция атомарна в рамках одной WAL-записи.

#### 23.2.1. Алгоритмы Recovery для всех `WalKind`

Базовый принцип для всех: проверка `entry_lsn > page_lsn` перед применением (idempotency guard).
Если `entry_lsn ≤ page_lsn` — операция уже применена, пропустить.

###### NodeAlloc

```
1. Прочитать страницу nodes_hot.fgb, содержащую slot
   (page_no = slot / 63)
2. Idempotency: если NodeHotSlot[slot].gen == gen И tombstone=0 → пропустить
3. Инициализировать NodeHotSlot[slot]:
   - gen = payload.gen
   - kind_flags = payload.kind_flags (tombstone=0)
   - label_id = payload.label_id
   - table_id = payload.table_id
   - все adj_* = 0, overflow_* = NULL_PAGE
   - props_page = NULL_PAGE
4. Обновить freelist: установить бит slot в 0 (занят)
5. Пометить страницу dirty, page_lsn = entry_lsn
```

###### EdgeAlloc

```
1. Прочитать страницу edges_hot.fgb (page_no = slot / 127)
2. Idempotency: если EdgeHotSlot[slot].gen == gen И tombstone=0 → пропустить
3. Инициализировать EdgeHotSlot[slot]:
   - gen = payload.gen
   - topo = payload.topo
   - flags = 0 (tombstone=0)
   - label_id = payload.label_id
   - table_id = payload.table_id
   - weight = NaN
   - все inline-массивы обнулить, overflow = NULL_PAGE
   - props_page = NULL_PAGE
4. Обновить freelist edges: бит slot = 0
5. Dirty, page_lsn = entry_lsn
```

###### NodeFree

```
1. Прочитать NodeHotSlot[slot]
2. Idempotency: если tombstone=1 → пропустить
3. Проверить gen совпадает (защита от применения к переиспользованному слоту)
4. Установить kind_flags.tombstone = 1
5. Обнулить adj_*, overflow_*, props_page (опционально — для чистоты)
6. Обновить freelist: бит slot = 1 (свободен)
7. Dirty, page_lsn = entry_lsn
```

###### EdgeFree

```
1. Прочитать EdgeHotSlot[slot]
2. Idempotency: tombstone=1 → пропустить
3. Проверить gen
4. Установить flags.tombstone = 1
5. Обнулить inline-массивы, overflow, props_page
6. Freelist: бит slot = 1
7. Dirty, page_lsn = entry_lsn
```

###### NodeHotUpdate

```
1. Прочитать NodeHotSlot[slot]
2. Idempotency: page_lsn ≥ entry_lsn → пропустить
3. Декодировать field_mask из payload
4. Для каждого установленного бита маски — применить соответствующее поле:
   - bit0 (kind_flags): записать 1 байт
   - bit1 (label_id): записать u32
   - bit2 (table_id): записать u32
   - bit3 (adj_out_count): записать u16
   - bit4 (adj_in_count): записать u16
   - bit5 (adj_undir_count): записать u16
   - bit6 (overflow_out): записать u64
   - bit7 (overflow_in): записать u64
   - bit8 (overflow_undir): записать u64
   - bit9 (props_ref): записать props_page(u64) + props_slot(u32) + props_len(u32)
5. Dirty, page_lsn = entry_lsn
```

###### EdgeHotUpdate

Аналогично NodeHotUpdate, но для EdgeHotSlot с соответствующим полевым маппингом.

###### AdjAppend

kind=0 (node-adjacency, V-E-V):
```
1. Прочитать NodeHotSlot[node_slot]
2. Idempotency: если edge_slot уже есть в adj_*[direction] inline или overflow → пропустить
3. Если место в inline (счётчик < 16 для out/in, < 8 для undir):
   a. Записать edge_slot в следующую свободную позицию
   b. Инкрементировать adj_*_count
4. Иначе:
   a. Пройти overflow-цепочку до последней страницы
   b. Если последняя страница полна (count = 4076):
      - Аллоцировать новую AdjacencyOverflowPage (freelist adj_over)
      - WAL: PageAlloc уже должен быть в логе до этой записи
      - Установить next_page предыдущей страницы
   c. Записать edge_slot в entries, инкрементировать count страницы
5. Установить adj_*_count = new_count
6. Dirty NodeHotSlot и overflow-страница, page_lsn = entry_lsn
```

kind=1 (edge-adjacency, A-I-A):
```
1. Прочитать EdgeHotSlot[atom_slot] (atom_slot = edge_slot в WAL)
2. Idempotency: если ref_slot уже в inv_inline или out_inline или overflow → пропустить
3. direction=0 → inv_inline (inv_count), direction=1 → out_inline (out_count)
4. Если место в inline (count < 4):
   a. Записать ref_slot
   b. Инкрементировать inv_count / out_count
5. Иначе:
   a. Если CROSS_LEVEL=1: пройти in_sg_overflow / out_sg_overflow цепочку
      (CrossLevelAdjacencyOverflowPage, entries = 12 байт)
   б. Иначе: пройти inv_overflow / out_overflow (обычный AdjacencyOverflowPage)
   в. Аллоцировать новую страницу если нужно
   г. Записать ref_slot, обновить count
6. Если ref_slot является Edge (bit31=1):
   Обновить EdgeEndpointIndex: вставить {endpoint_ref=ref_slot, edge_ref=atom_slot, role=direction}
7. Установить inv_count / out_count = new_count
8. Dirty, page_lsn = entry_lsn
```

###### AdjRemove

kind=0 (node-adjacency, V-E-V):
```
1. Прочитать NodeHotSlot[node_slot]
2. Idempotency: edge_slot отсутствует в adj_* → пропустить
3. Если в inline: swap_remove (заменить на последний элемент)
4. Если в overflow:
   a. Найти страницу и позицию
   b. swap_remove: заменить на последний entry последней страницы цепочки
   c. Если последняя страница стала пустой: исключить из цепочки, освободить в freelist
5. Установить adj_*_count = new_count
6. Dirty, page_lsn = entry_lsn
```

kind=1 (edge-adjacency, A-I-A):
```
1. Прочитать EdgeHotSlot[atom_slot]
2. Idempotency: ref_slot отсутствует → пропустить
3. Аналогичный swap_remove из inline или overflow
4. Если ref_slot типа Edge: удалить из EdgeEndpointIndex
5. Установить inv_count / out_count = new_count
6. Dirty, page_lsn = entry_lsn
```

###### PropSet

```
1. Декодировать disk_atom_ref → определить это Node или Edge (bit31)
2. Прочитать соответствующий hot-слот → получить props_page, props_slot, props_len
3. Idempotency: если props_len == payload.props_len И page_lsn ≥ entry_lsn → пропустить
4. Если props_page == NULL_PAGE (новая запись):
   a. Найти страницу PropHeapPage с достаточным местом (free_bytes ≥ props_len + 4)
      или аллоцировать новую (freelist props)
   b. Вставить запись: new_record_start = free_end - props_len
   c. Записать данные, добавить SlotEntry
   d. Обновить free_start, free_end
   e. Обновить hot-слот: props_page, props_slot, props_len
5. Если props_page != NULL_PAGE (обновление):
   a. Если новый размер ≤ старого: перезаписать на месте (в пределах record_len)
   б. Если новый размер > старого:
      - Tombstone старый SlotEntry (record_offset = 0)
      - Вставить новую запись (как шаг 4a-4d)
      - Обновить hot-слот
6. Если props_len > PAGE_PAYLOAD_SIZE / 2:
   Large Object: аллоцировать цепочку PropLargeChunkPage
   - props_slot = 0xFFFF_FFFE (Large Object sentinel)
   - Записать данные в цепочку, установить props_page на первый chunk
7. Dirty все затронутые страницы, page_lsn = entry_lsn
```

###### PropDelete

```
1. Прочитать hot-слот → props_page, props_slot, props_len
2. Idempotency: props_page == NULL_PAGE → пропустить
3. Если Large Object (props_slot == 0xFFFF_FFFE):
   a. free_page_chain(props.fgb, props_page)
4. Иначе:
   a. Прочитать PropHeapPage[props_page]
   б. SlotEntry[props_slot].record_offset = 0 (tombstone)
   в. Dirty PropHeapPage
5. Обнулить в hot-слоте: props_page = NULL_PAGE, props_slot = NULL_SLOT, props_len = 0
6. Dirty hot-слот, page_lsn = entry_lsn
```

###### IndexInsert

```
1. Определить тип индекса по index_id (lookup в SchemaCatalog)
2. Для B+tree (standard, unique, primary, ee_endpoint):
   a. Найти leaf-страницу для данного ключа: спуск от root по internal pages
   б. Idempotency: если ключ+DiskAtomRef уже в leaf → пропустить
   в. Вставить entry в leaf:
      - Если место есть: записать entry, обновить num_entries
      - Если нет места (overflow): B+tree split
        * Аллоцировать новую leaf-страницу (freelist индекса)
        * Разделить entries поровну
        * Обновить next_leaf у обеих страниц
        * Поднять median-ключ в parent internal page
        * Если parent тоже переполнен → рекурсивный split вверх
        * Если split дошёл до root → аллоцировать новый root
   г. Dirty все затронутые страницы, page_lsn = entry_lsn
3. Для LabelIndex (sorted array):
   a. Найти позицию вставки (binary search)
   б. Idempotency: DiskAtomRef уже есть → пропустить
   в. Вставить в отсортированную позицию
   г. Если страница полна: аппендировать в следующую страницу (next_page)
```

###### IndexDelete

```
1. Тип индекса по index_id
2. Для B+tree:
   a. Спуск от root до leaf
   б. Idempotency: запись отсутствует → пропустить
   в. Удалить entry из leaf
   г. Если leaf опустел: рассмотреть merge с соседним leaf
      (если сосед имеет достаточно записей — rebalance, иначе merge)
   д. Если merge — удалить разделительный ключ из parent
   е. Dirty, page_lsn = entry_lsn
3. Для LabelIndex:
   a. Записать NULL_SLOT tombstone на позиции DiskAtomRef
   б. Dirty, page_lsn = entry_lsn
```

###### SchemaDef

```
1. Прочитать SchemaCatalogPage
2. Idempotency: schema_id уже существует с тем же version → пропустить
3. Найти страницу с достаточным местом или аллоцировать новую
4. Вставить SchemaEntry: schema_id, kind, version, name_id, parent_id, def_len, definition
5. Если schema_id уже существует с меньшим version (OVERWRITE):
   а. Tombstone старый entry (def_len = 0)
   б. Вставить новый
6. Dirty, page_lsn = entry_lsn
```

###### SchemaRemove

```
1. Найти SchemaEntry по schema_id
2. Idempotency: не найден → пропустить
3. Tombstone: установить def_len = 0 (или специальный флаг DELETED)
4. Dirty, page_lsn = entry_lsn
```

###### PageAlloc

```
1. Прочитать FreelistPage для file_kind
   (найти нужную FreelistPage: first_page_covered ≤ page_no < first_page_covered + 130624)
2. Idempotency: бит page_no уже = 0 (занят) → пропустить
3. Установить бит page_no = 0 в bitmap
4. Dirty FreelistPage, page_lsn = entry_lsn
5. Примечание: сама целевая страница инициализируется последующей операцией
   (NodeAlloc, EdgeAlloc и т.д.) — PageAlloc только резервирует место
```

###### PageFree

```
1. Прочитать FreelistPage для file_kind
2. Idempotency: бит page_no уже = 1 (свободен) → пропустить
3. Установить бит page_no = 1
4. Dirty FreelistPage, page_lsn = entry_lsn
```

###### StatsUpdate

```
1. По stats_kind определить целевую страницу:
   - 0=GraphStats: graph_stats_page из суперблока
   - 1=LabelStats: найти entry по table_id в LabelStatsPage
   - 2=PropertyHistogram: найти страницу по (table_id, field_id)
   - 3=DegreeHistogram: найти по label_id + direction
2. Idempotency: page_lsn ≥ entry_lsn → пропустить
3. Декодировать delta_bytes и применить к соответствующим счётчикам
4. Dirty, page_lsn = entry_lsn
```

###### ChangefeedPut

```
1. Найти активный changefeed файл для table_id
   (по имени <table_id>_<since_lsn>.fcf)
2. Idempotency: запись с данным lsn уже есть → пропустить
3. Найти последнюю ChangefeedPage или аллоцировать новую
4. Вставить ChangefeedEntry: lsn, versionstamp, event_kind, atom_ref, данные
5. Dirty, page_lsn = entry_lsn
6. Проверить TTL: если текущее время > changefeed_duration от самого старого entry
   удалить старые сегменты (purge по файловому имени)
```

### 23.3. Порядок WAL-first при COMMIT

```
1. Validate (SSI/SI check)
2. Assign commit_ts
3. WAL: append COMMIT record          ← сначала log
4. WAL: fsync (fdatasync)             ← сначала persist
5. MvccManager: commit(tx)            ← только потом версии видимы
6. Dirty pages остаются в BufferPool  ← flush при eviction/checkpoint
```

### 23.4. Torn write handling

```
При чтении страницы:
  expected_checksum = xxhash3(page_bytes with checksum field = 0)
  if page_header.checksum != expected_checksum:
    → страница повреждена
    → если есть WAL-записи для этой страницы с lsn > checkpoint_lsn:
        восстановить через REDO (страница будет перезаписана)
    → иначе:
        страница считается потерянной → MetaGraphError::PageCorrupt
```

---

## 24. Compaction и дефрагментация

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
9. Обновить GraphSuperblock
10. fsync всего → atomic rename
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
Для NodeHotPage (63 слота × 256 байт):
  page_no          = slot_index / 63
  slot_within_page = slot_index % 63
  byte_offset      = PAGE_HEADER_SIZE + slot_within_page × 256

Для EdgeHotPage (127 слотов × 128 байт):
  page_no          = slot_index / 127
  slot_within_page = slot_index % 127
  byte_offset      = PAGE_HEADER_SIZE + slot_within_page × 128
```

### 25.2. Чтение смежности

```rust
fn out_neighbors(disk_graph: &DiskMetaGraph, node_slot: u32) -> Vec<DiskAtomRef> {
    let hot = disk_graph.node_hot(node_slot);
    let inline = &hot.adj_out[..min(hot.adj_out_count, 16)];
    let mut result: Vec<_> = inline.iter()
        .take_while(|&&s| s != NULL_SLOT)
        .map(|&&s| DiskAtomRef::edge(s))
        .collect();
    if hot.adj_out_count > 16 {
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
fn node_hot_page_no(slot: u32) -> u64    { (slot / 63) as u64 }
fn node_hot_offset(slot: u32)  -> usize  { 32 + (slot % 63) as usize * 256 }

fn edge_hot_page_no(slot: u32) -> u64    { (slot / 127) as u64 }
fn edge_hot_offset(slot: u32)  -> usize  { 32 + (slot % 127) as usize * 128 }
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

Например, для n_atoms=10000, n_edges=50000: matrix size = 10000 × 50000 / 8 = 62.5 МБ ≈ 3908 страниц.
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

| Параметр                             | Тип    | Default   | Описание                                           |
|--------------------------------------|--------|-----------|----------------------------------------------------|
| `FG_PAGE_BUFFER_PAGES`               | usize  | 4096      | Ёмкость BufferPool (4096 × 16 КиБ = 64 МиБ)        |
| `FG_WAL_SEGMENT_SIZE`                | u64    | 67108864  | Размер WAL сегмента до ротации (64 МиБ)            |
| `FG_CHECKPOINT_DIRTY_THRESHOLD`      | f64    | 0.25      | Checkpoint при ≥ N% dirty pages в pool             |
| `FG_CHECKPOINT_INTERVAL_SEC`         | u64    | 300       | Максимальный интервал checkpoint                   |
| `FG_LABEL_DICT_CACHE_MB`             | usize  | 32        | In-memory LRU кэш label dictionary                 |
| `FG_PROPS_COMPRESS_THRESHOLD`        | usize  | 512       | Сжимать props если serialized_size > N байт        |
| `FG_HNSW_CACHE_SIZE`                 | usize  | 268435456 | HNSW кэш (256 МиБ); §9.6 gql_spec                  |
| `FG_INDEX_BTREE_FILL_FACTOR`         | f64    | 0.80      | Заполнение B+tree листьев при bulk insert          |
| `FG_COMPACTION_TOMBSTONE_RATIO`      | f64    | 0.20      | Compaction label index при tombstone ratio > N     |
| `FG_COMPACT_HOT_TOMBSTONE_RATIO`     | f64    | 0.30      | Полный defrag при tombstone ratio > N              |
| `FG_STATS_AUTO_ANALYZE_DIRTY`        | f64    | 0.10      | Авто-ANALYZE при dirty_fraction > N                |
| `FG_ASYNC_EVENT_PROCESSING_INTERVAL` | u64    | 5000      | Интервал обработки async событий (мс)              |
| `FG_CHANGEFEED_PURGE_INTERVAL_SEC`   | u64    | 3600      | Интервал очистки старых changefeed файлов          |
| `FG_PATH_MAX_DEPTH`                  | usize  | 30        | Максимальная глубина путей                         |
| `FG_TRAVERSAL_READ_YOUR_WRITES`      | bool   | false     | Traversal видит собственные мутации                |
| `FG_TEMPFILES_PATH`                  | String | ""        | Путь для SELECT TEMPFILES                          |

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

### 29.1. Структура модулей

```
src/
  types/
    mod.rs            -- Value enum (расширенный), TypeTag, кодирование
    value.rs          -- расширенный Value + все варианты
    record_id.rs      -- RecordId, RecordIdPart
    datetime.rs       -- DateTime, Duration (обёртки над jiff)
    decimal.rs        -- Decimal (обёртка крейтом)
    geometry.rs       -- Geometry variants
    vector.rs         -- VectorValue

  storage/
    mod.rs            -- PageManager trait, DataFileKind, constants
    superblock.rs     -- StorageSuperblock, NamespaceSuperblock, DatabseSuperblock, GraphSuperblock
    node_pages.rs     -- NodeHotSlot, NodeHotPage
    edge_pages.rs     -- EdgeHotSlot, EdgeHotPage
    adj_overflow.rs   -- AdjacencyOverflowPage, CrossLevelAdjacencyOverflowPage
    edge_incidence.rs -- EdgeIncidencePage, CrossLevelEdgeIncidencePage
    prop_heap.rs      -- PropHeapPage, slotted page
    subgraph_dir.rs   -- SubgraphDirPage
    freelist.rs       -- FreelistPage
    label_dict.rs     -- LabelDictionary
    disk_graph.rs     -- DiskMetaGraph (главный фасад)

  schema/
    mod.rs            -- SchemaCatalog
    table.rs          -- TableDefinition
    field.rs          -- FieldDefinition
    index_def.rs      -- IndexDefinition metadata
    event.rs          -- EventDefinition
    function.rs       -- FunctionDefinition
    analyzer.rs       -- AnalyzerDefinition
    param.rs          -- ParamDefinition

  stats/
    mod.rs            -- StatisticsStore
    graph_stats.rs    -- GraphStatsPage
    label_stats.rs    -- LabelStatsPage
    prop_hist.rs      -- PropertyHistogramPage
    degree_hist.rs    -- DegreeHistogramPage

  index/
    mod.rs            -- Index trait, IndexStatus
    label_index.rs    -- LabelIndex (sorted u32 array)
    btree.rs          -- BTreeIndex (B+tree)
    fulltext.rs       -- FulltextIndex (inverted + BM25)
    hnsw.rs           -- HnswIndex
    rtree.rs          -- RtreeIndex (geometry)
    count.rs          -- CountIndex

  changefeed/
    mod.rs            -- ChangefeedStore
    log.rs            -- ChangefeedPage, ChangefeedEntry
    live_query.rs     -- LiveQueryRegistry
```

### 29.2. PageManager trait

```rust
pub trait PageManager: Send + Sync {
    /// Выделяет новую страницу типа `file` и возвращает её page_no.
    fn alloc_page(&self , file: DataFileKind) -> io::Result<u64>;

    /// Освобождает страницу (обновляет freelist).
    fn free_page(&self , file: DataFileKind, page_no: u64) -> io::Result<()>;

  /// Читает страницу в буфер; использует BufferPool (Clock-Pro).
    fn read_page(&self , file: DataFileKind, page_no: u64, buf: & mut [u8; PAGE_SIZE]) -> io::Result<()>;

    /// Записывает страницу; обновляет BufferPool (dirty).
    /// Возвращает LSN записи для установки page_lsn.
    fn write_page_dirty(&self , file: DataFileKind, page_no: u64, buf: &[u8; PAGE_SIZE], lsn: Lsn) -> io::Result<()>;

    /// Сбрасывает все dirty страницы заданного файла.
    fn flush_file(&self , file: DataFileKind) -> io::Result<() >;

    /// Сбрасывает все dirty страницы всех файлов.
    fn flush_all(&self ) -> io::Result<() >;

    /// Текущий page_count для файла.
    fn page_count(&self , file: DataFileKind) -> io::Result<u64>;
}
```

### 29.3. Новые компоненты

| Компонент                     | Где                                      | Что делает                     |
|-------------------------------|------------------------------------------|--------------------------------|
| `PageManager` trait           | `storage/mod.rs`                         | Абстракция page-level I/O      |
| `DiskMetaGraph`               | `storage/disk_graph.rs`                  | Disk-backed MetaGraph          |
| `NodeHotSlot` / `EdgeHotSlot` | `storage/node_pages.rs`, `edge_pages.rs` | Encode/decode hot-слотов       |
| `PropHeapPage`                | `storage/prop_heap.rs`                   | Slotted-page для PropertyMap   |
| `LabelDictionary`             | `storage/label_dict.rs`                  | Интернирование строк           |
| `LabelIndex`                  | `index/label_index.rs`                   | Sorted array index по меткам   |
| `BTreeIndex`                  | `index/btree.rs`                         | B+tree property index          |
| `FulltextIndex`               | `index/fulltext.rs`                      | Inverted index + posting lists |
| `HnswIndex`                   | `index/hnsw.rs`                          | In-memory HNSW с checkpoint    |

### 29.4. Зависимости (добавить в Cargo.toml)

```toml
jiff = { version = "0.2", features = ["serde"] }   # DateTime, Duration
# ... Some Decimal Implementation here ... 
xxhash-rust = { version = "0.8", features = ["xxh3"] }     # Page checksum
geo = { version = "0.33" }                              # planar geospatial geometries and algorithms
geojson = { version = "1.0.0" }                         # GeoJSON
```

---

## 30. План доработки кодовой базы

### 30.1. value.rs — Критический приоритет

**Проблема**: текущий `Value` не поддерживает `None` (distinct from `Null`), `Decimal`, `DateTime`, `Duration`,
`RecordId`, `Geometry`, `Set`, `Tuple`, `Range`, `Vector`, `Uuid`, `Ulid`.

**Действие**: Полная замена `Value` enum.

```rust
// Новый Value enum (src/types/value.rs):
pub enum Value {
    None,                               // 0x00 — поле отсутствует
    Null,                               // 0x01 — поле есть, значение null
    Bool(bool),                         // 0x02
    Int(i64),                           // 0x03
    Float(f64),                         // 0x04
    Decimal(SomeDecimalImpl),           // 0x05 (d128)
    String(Box<str>),                   // 0x06
    Bytes(Vec<u8>),                     // 0x07
    DateTime(crate::types::DateTime),   // 0x08
    Duration(crate::types::Duration),   // 0x09
    Uuid(u128),                         // 0x0A
    Ulid(u128),                         // 0x0B
    RecordId(Box<RecordId>),            // 0x0C
    Array(Vec<Value>),                  // 0x10
    Set(BTreeSet<OrdValue>),            // 0x11 (ordered for encoding)
    Tuple(Vec<Value>),                  // 0x12
    Object(PropertyMap),                // 0x13
    Option(Option<Box<Value>>),         // 0x14
    Range(Box<RangeValue>),             // 0x15
    Geometry(Box<Geometry>),            // 0x20–0x26
    Vector(VectorValue),                // 0x30–0x34
}
```

Обратная совместимость на этапе MVP не требуется.

**Обратная совместимость** (future): старые `serial.rs` снимки читаются через версионированный decoder.
Новые теги добавляются с новой версией CODEC_VERSION.

### 30.2. serial.rs — Высокий приоритет

**Проблема**: кодек не поддерживает новые типы.

**Действие**: Обновить `write_value` / `read_value` для всех новых TypeTag.
`CODEC_VERSION` = 1 на всём этапе MVP. На этапе MVP совместимость со снимками других форматов не поддерживается.

Ключевые изменения:

- Добавить ветки для TypeTag.
- Обновить `write_props` / `read_props` для использования `key_id (u32)` вместо строк в disk mode.

### 30.3. id.rs — Средний приоритет

**Проблема**: `AtomId` и `GraphId` — только внутренние. Нет публичного `RecordId`.

**Действие**: Добавить `RecordId` как отдельный публичный тип (не в `id.rs` — в `src/types/record_id.rs`).
`AtomId` и `GraphId` остаются внутренними.

Добавить `DiskAtomRef (u32)` как newtype в `src/storage/mod.rs`.

### 30.4. mvcc_persist.rs — Средний приоритет

**Проблема**: `WalKind` не покрывает дисковые операции; `WalEntry::Write` использует `atom_id: u64` (внутренний AtomId),
а не `DiskAtomRef`.

**Действие**:

- Добавить новые `WalKind` значения 0x10–0x21 (§22.1).
- Добавить соответствующие `WalEntry` варианты.
- Разделить логику: `MvccWal` (текущий, Mode 1) и `DiskWal` (расширенный, Mode 2).
  Оба используют `WalWriter` / `WalSegmentManager` как transport layer.
- `PersistLayer::checkpoint()` расширить для flush .fgb файлов.

### 30.5. mvcc.rs — Низкий приоритет

**Проблема**: `VersionChain<T>` хранит typed T. Для дискового режима версии — bytes.

**Действие**: Для Mode 2 `MvccStore<Vec<u8>, BytesCodec>` используется как есть (bytes = сериализованный `PropertyMap`).
Не требует изменений, но нужно убедиться, что `MvccManager` корректно работает с `DiskMetaGraph`.

### 30.6. store.rs — Низкий приоритет

**Проблема**: `GraphStore` управляет только Mode 1 графами (`.fgr` + `.wal`).

**Действие**: Добавить `GraphStore::open_disk_graph(name: &str) -> io::Result<DiskMetaGraph>`.
Существующие методы `create_graph`, `open_graph` для Mode 1 — без изменений.

### 30.7. graph.rs — Отложено

`MetaGraph` остаётся in-memory (Mode 1 и загруженные subgraphs в Mode 3).

Единственное необходимое дополнение: метод `MetaGraph::from_disk_batch(...)` для
эффективной загрузки множества атомов из `DiskMetaGraph` без повторного сканирования.

### 30.8. Приоритизированный порядок работ

| Приоритет       | Компонент                | Что делать                                     |
|-----------------|--------------------------|------------------------------------------------|
| 1 (Блокирующий) | `src/types/value.rs`     | Новый `Value` enum с полной системой типов     |
| 1 (Блокирующий) | `src/types/record_id.rs` | `RecordId`, `RecordIdPart`                     |
| 1 (Блокирующий) | `serial.rs`              | Новые TypeTag                                  |
| 1 (Блокирующий) | `src/types/datetime.rs`  | Полная интеграция Jiff                         |
| 1 (Блокирующий) | `src/types/geometry.rs`  | GeoJSON parsing + encoding                     |
| 2 (Высокий)     | `src/storage/`           | `PageManager`, `DiskMetaGraph`, all page types |
| 2 (Высокий)     | `mvcc_persist.rs`        | Новые WalKind 0x10–0x21                        |
| 3 (Средний)     | `src/schema/`            | `SchemaCatalog`                                |
| 3 (Средний)     | `src/index/`             | `LabelIndex`, `BTreeIndex`, `CountIndex`       |
| 4 (Нормальный)  | `src/stats/`             | `StatisticsStore`                              |
| 4 (Нормальный)  | `src/index/fulltext.rs`  | `FulltextIndex` (BM25)                         |
| 5 (Низкий)      | `src/index/hnsw.rs`      | `HnswIndex`                                    |
| 5 (Низкий)      | `src/index/rtree.rs`     | `RtreeIndex`                                   |
| 5 (Низкий)      | `src/changefeed/`        | `ChangefeedStore`, `LiveQueryRegistry`         |
| 6 (MVP+)        | `store.rs`               | `open_disk_graph`                              |

### 30.9. Исправления для полной инцидентности

| Компонент                         | Что исправить                                                                                                                          |
|-----------------------------------|----------------------------------------------------------------------------------------------------------------------------------------|
| `graph.rs::register_adjacency()`  | Добавить путь для edge-endpoints: регистрировать в EdgeEndpointIndex вместо игнорирования                                              |
| `graph.rs::EdgeFlags`             | Добавить `CROSS_LEVEL = 0b0100_0000`                                                                                                   |
| `graph.rs::add_cross_meta_edge()` | Установить `CROSS_LEVEL` на ребре; убрать запись `__from_inner`/`__to_inner`; использовать `source_atom_ref`/`target_atom_ref` в props |
| `node.rs::NodeFlags`              | Убрать `PORT` бит или оставить deprecated                                                                                              |
| `traversal.rs::neighbors()`       | Параметризовать: `NodeOnly` (текущий) vs `AtomWalk` (новый); AtomWalk использует EdgeEndpointIndex и EdgeIncidence                     |
| `traversal.rs::TarjanCuts`        | Параметризовать тип обхода; `n.is_node()` — только в NodeOnly-режиме                                                                   |
| `matrix.rs`                       | Добавить `EndpointTensor` и `ParticipationTensor`                                                                                      |
| `id.rs`                           | Добавить `QualifiedAtomRef { sg_slot: u32, local_slot: u32 }`                                                                          |

---

*Конец спецификации. Версия 0.1.*