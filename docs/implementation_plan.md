# Ferros-Grynn — План реализации (Implementation Plan)

*Версия 0.1-draft.*

Этот файл — **производный** документ. Он собирает весь материал, относящийся к реализации на
Rust и к планированию работ, который ранее был рассеян по спецификациям. Спецификации фиксируют
**контракт** (язык, формат, семантику); этот файл описывает, **как** и **в каком порядке** его
строить.

## Назначение и границы

Что здесь есть:

- соответствие спецификаций текущей кодовой базе (что уже реализовано, что каркас, что новое);
- целевая структура Rust-модулей и ключевые контракты (trait'ы, типы, зависимости);
- пообластной план реализации с приоритетами, статусами и зависимостями;
- приоритизированный порядок работ, разбитый на фазы;
- отложенное / вне MVP (сводно из всех спецификаций).

Чего здесь нет: формат данных на диске, байтовые раскладки страниц, грамматика, семантика
запросов и алгоритмы — они остаются в спецификациях и являются контрактом.

**Источники извлечения.** Материал перенесён из: `disk_storage_spec.md` §29–§30;
`gql_tech_spec.md` §17–§18 и trait `PageManager` из §10.2; `gql_spec.md` §31, §32.3 и
Rust-иллюстрации тензоров из §3.8. `enum BridgeData` (§30 `gql_spec.md`) намеренно оставлен в
спецификации как часть публичного контракта мостов.

**Приведение к принятым решениям.** Часть извлечённого материала была написана до фиксации
ряда архитектурных решений и противоречила им. Такие места приведены в соответствие и помечены
🔄. Действующие решения:

| Решение                                                                                          | Действует                                              |
|--------------------------------------------------------------------------------------------------|--------------------------------------------------------|
| Журнал — **frame-WAL / COW-в-логе** (образ страницы + индекс версий), не ARIES, не data-file COW | `disk_storage_spec.md` §22–§23, `gql_tech_spec.md` §12 |
| **Единый журнал уровня БД** (не пожурнально на граф); глобальные часы коммитов                   | `disk_storage_spec.md` §7                              |
| `AtomId = gen(23) \| kind(1) \| slot(40)`, `DiskAtomRef = u64`                                   | `disk_storage_spec.md` §9                              |
| **SSI по умолчанию** (SI/RC — opt-in)                                                            | `gql_spec.md` §17.1                                    |
| `decimal` — `rust_decimal` в рантайме, Arrow-форма `i128 + scale` (18 байт) на диске             | `disk_storage_spec.md` §3.3, §5.2                      |
| Массовая загрузка — `BEGIN IMPORT … END IMPORT` + bulk-сборка индексов (без LSM/delta-store)     | `gql_spec.md` §7.11, `disk_storage_spec.md` §20.13     |

---

## 1. Соответствие кодовой базе

Объединяет `gql_spec.md` §31 и `gql_tech_spec.md` §17.

Легенда статуса: 🟢 реализовано (в `fg-meta` как референс или в `src`); 🟡 каркас (есть модуль,
тело неполное); 🔴 новое (требует реализации с нуля).

### 1.1. Согласовано / реализовано / каркас

| Область                                                                                                                              | Контракт (спека)                       | Модуль                                                                                                       | Статус                                                      |
|--------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------|--------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------|
| Модель данных `G = ⟨V, MV, E, ME⟩`                                                                                                   | `gql_spec` §3                          | `fg-meta` `graph.rs` → `src/values/runtime/graph`                                                            | 🟢 референс                                                 |
| `AtomId` (генерационный, kind-бит, u64)                                                                                              | `disk` §9                              | `id.rs`                                                                                                      | 🟢 (🔄 `slot(40)`)                                          |
| Значения (`Value`, storable/virt)                                                                                                    | `gql_spec` §5                          | `src/values` (`runtime/storable`, `runtime/virt`, `runtime/graph`, `storage`)                                | 🟡 активный, требует расширения                             |
| Диагностика (`StatusDefinition + Status + DiagnosticRecord → StatusObject → FerrosGrynnError`)                                                            | —                                      | `src/gql_status`                                                                                             | 🟢                                                          |
| Изоляция SSI по умолчанию, SI/RC — opt-in                                                                                            | `gql_spec` §17.1                       | `fg-meta` `mvcc.rs`                                                                                          | 🟢                                                          |
| MVCC + снимки, O(1)-индексы конфликтов, predicate locking                                                                            | `gql_spec` §17                         | `fg-meta` `mvcc.rs`, `mvcc_persist.rs`                                                                       | 🟢 режим 1 / 🔴 режим 2                                     |
| Журнал (frame-WAL, уровень БД)                                                                                                       | `disk` §22–§23                         | `mvcc_persist.rs` → `crates/storage_engine`                                                                  | 🟡 логический WAL режима 1 есть; frame-WAL режима 2 — новое |
| Snapshot / store, сегментная ротация                                                                                                 | `disk` §7                              | `fg-meta` `store.rs`                                                                                         | 🟢                                                          |
| Direct/Async I/O, BufferPool (Clock-Pro)                                                                                             | `disk` §2                              | `fg-meta` `io_sync`/`io_async`/`async_io`, `buffer_pool.rs`; `crates/storage_engine/io`                      | 🟢 / 🟡                                                     |
| Подграфы (`SubGraph`, `TreeSubGraph`, `DagSubGraph`, …)                                                                              | `gql_spec` §16                         | `fg-meta` `subgraph.rs`                                                                                      | 🟢 референс                                                 |
| Алгоритмы traversal (BFS/DFS/Dijkstra/Bellman-Ford/SCC/LCA/MST/cuts)                                                                 | `gql_spec` §13.4                       | `fg-meta` `traversal.rs`                                                                                     | 🟢 референс                                                 |
| Лексер / парсер (reblessive, `TokenBuffer<4>`)                                                                                       | `gql_grammar`                          | `src/syn` (`lexer`/`parser`/`token`/`error`)                                                                 | 🟡 каркас                                                   |
| AST / `Expr`                                                                                                                         | `gql_spec` §26                         | `src/gql/ast.rs`, `src/gql/expression.rs`                                                                    | 🟡 каркас                                                   |
| `Datastore::execute → Vec<QueryResult>`                                                                                              | `gql_tech` §2                          | `src/gql/ds.rs`                                                                                              | 🟡 тело TODO                                                |
| Traversal VM (`TraversalMachine`, `Traverser`, `TraversalContext`)                                                                   | `gql_tech` §8                          | `src/gql/traversal`                                                                                          | 🟡 каркас                                                   |
| `PageManager` / `DiskMetaGraph` / disk-страницы                                                                                      | `gql_tech` §10; формат — `disk` §9–§21 | `crates/storage_engine` (`store`, `layout`): `NodeRecord`/`EdgeRecord`/`PropertyRecord`, `FerrosGrynnLayout` | 🟡 каркас                                                   |
| `RecordId` / `DiskAtomRef` / `QualifiedAtomRef`                                                                                      | `disk` §4, §9                          | `src/types` (нов.)                                                                                           | 🔴 новое                                                    |
| `SchemaCatalog`                                                                                                                      | `disk` §18                             | `src/schema` (нов.)                                                                                          | 🔴 новое                                                    |
| `StatisticsStore`                                                                                                                    | `disk` §19                             | `src/stats` (нов.)                                                                                           | 🔴 новое                                                    |
| Индексы (Label/BTree/Count/Fulltext/HNSW/R-tree/…)                                                                                   | `disk` §20                             | `src/index` (нов.)                                                                                           | 🔴 новое                                                    |
| Changefeed / Live                                                                                                                    | `gql_spec` §21, `disk` §21             | `src/changefeed` (нов.)                                                                                      | 🔴 новое                                                    |
| Query-слой: binder / typechecker, lowering AST→IR, RuleEngine/CostEngine, физ. операторы, стыковка Traversal VM ↔ реляционный движок | `gql_tech` §4–§9                       | `src/gql`                                                                                                    | 🔴 новое                                                    |

### 1.2. Требует согласования / открытые вопросы

Из `gql_spec.md` §31.2.

| Вопрос               | Описание                                                                                                                                                        |
|----------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Формат хранилища     | Ранние документы описывали LSM (L0/L1/compaction). Действует WAL + snapshot без LSM; GQL-спека внутреннее хранилище не регламентирует — противоречие устранено. |
| WAL recovery         | Ранний документ описывал ARIES 3-phase. Действует **frame-WAL** с single-pass REDO по LSN; ARIES/UNDO не нужен (см. таблицу решений).                           |
| `PropertyMap`        | В коде `PropertyMap = FxHashMap<Box<str>, Value>`; GQL `object` — схожий тип. Нужен маппинг `Value` → GQL-типы при возврате результата.                         |
| `GraphId` / `AtomId` | Внутренние идентификаторы не экспонируются в GrynQL; публичная форма — `table:id` (`RecordId`).                                                                 |
| Гиперрёбра           | Реализованы (`HyperDirected`, `HyperUndirected` в `edge.rs`). Синтаксис `RELATE HYPER` / `MATCH HYPER` — новый, требует согласования.                           |
| Метаатомы в GQL      | `MetaGraph.promote_to_meta()` реализован; GrynQL-синтаксис (`graph::inner`/`graph::nodes`) требует реализации функций.                                          |
| ECS-компоненты       | `ecs.rs` (SparseSet, WorldComponents) — внутренняя система ML-пайплайна; в GrynQL напрямую не экспонируется, доступ через `tv::algo()` — задел.                 |

---

## 2. Целевая структура модулей

Из `disk_storage_spec.md` §29.1.

> 🔄 **Замечание о раскладке.** Текущая кодовая база использует `src/values/runtime/*` +
> `src/values/storage` и отдельный крейт `crates/storage_engine`; дисковый слой мигрирует именно
> туда. Дерево ниже — **целевой ориентир** по составу компонентов; финальные пути (плоский
> `src/storage/*` против `crates/storage_engine/*`) согласуются при переносе. Состав важнее путей.

```
src/
  types/
    mod.rs            -- Value enum (расширенный), TypeTag, кодирование
    value.rs          -- расширенный Value + все варианты
    record_id.rs      -- RecordId, RecordIdPart
    datetime.rs       -- DateTime, Duration (обёртки над jiff)
    decimal.rs        -- Decimal (обёртка над rust_decimal)
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

---

## 3. Ключевые Rust-контракты

Контракты, которые предстоит написать. Байтовые раскладки соответствующих структур — в
`disk_storage_spec.md`; здесь только Rust-API.

### 3.1. `Value` — система типов значений (🔴 P1)

Из `disk_storage_spec.md` §30.1. Текущий `Value` не поддерживает `None` (в отличие от `Null`),
`Decimal`, `DateTime`, `Duration`, `RecordId`, `Geometry`, `Set`, `Tuple`, `Range`, `Vector`,
`Uuid`, `Ulid`. Требуется полная замена enum.

```rust
// Новый Value enum (src/types/value.rs):
pub enum Value {
    None,                               // 0x00 — поле отсутствует
    Null,                               // 0x01 — поле есть, значение null
    Bool(bool),                         // 0x02
    Int(i64),                           // 0x03
    Float(f64),                         // 0x04
    Decimal(rust_decimal::Decimal),     // 0x05  🔄 (рантайм-decimal; на диске — Arrow-форма i128+scale)
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

Обратная совместимость на этапе MVP не требуется. В будущем — старые снимки читаются
версионированным decoder'ом; новые теги добавляются с новой `CODEC_VERSION`.

### 3.2. Идентификаторы — `RecordId` / `DiskAtomRef` / `QualifiedAtomRef`

Три уровня (`disk_storage_spec.md` §4, §9):

- **`RecordId`** (`table:id`) — публичный, единственный видимый в GrynQL; хранится как свойство `id`.
  Отдельный публичный тип в `src/types/record_id.rs` (не в `id.rs`).
- **`DiskAtomRef`** (`u64`: бит 40 = kind, биты 39..0 = slot) — стабильный дисковый id без
  generation. 🔄 newtype в `src/storage/mod.rs` (ранее планировался `u32`).
- **`AtomId`** (`u64`: slot + generation) — внутренний in-memory handle; **никогда** не
  экспонируется в GrynQL. `AtomId` и `GraphId` остаются внутренними.
- **`QualifiedAtomRef { sg_slot: u32, local_slot: … }`** — кросс-уровневый адрес (см.
  `disk_storage_spec.md` §3.9; согласовать ширину `local_slot` с `DiskAtomRef` = u40).

Разрешение при выполнении: `RecordId --(pk B+tree)--> DiskAtomRef --(hot slot → gen)--> AtomId`;
`from_disk_ref(r, gen)` восстанавливает `AtomId`. Материализация результата делает обратный
маппинг.

### 3.3. `PageManager` — страничный доступ (frame-WAL) (🔴 P2)

🔄 **Действующий контракт** — из `gql_tech_spec.md` §10.2. Он заменяет более раннюю редакцию
(`alloc/free/read/write_page_dirty/flush_*/page_count`), которая предполагала STEAL и запись
страниц на месте: под frame-WAL запись — это добавление фрейма в журнал, а чтение
параметризовано снимком.

```rust
pub trait PageManager: Send + Sync {
    fn begin_tx(&self) -> TxHandle;
    fn alloc_page(&self, tx: &TxHandle, key: PageKey) -> io::Result<u64>;
    fn free_page(&self, tx: &TxHandle, key: PageKey) -> io::Result<()>;
    /// Чтение всегда параметризовано снимком: сначала индекс версий, затем .fgb
    fn read_page(&self, key: PageKey, snapshot: Lsn, buf: &mut [u8; PAGE_SIZE]) -> io::Result<()>;
    /// Запись — новый фрейм в журнал; LSN назначает журнал, не вызывающий
    fn write_frame(&self, tx: &TxHandle, key: PageKey, buf: &[u8; PAGE_SIZE]) -> io::Result<Lsn>;
    fn commit_tx(&self, tx: TxHandle) -> io::Result<CommitTs>;
    fn abort_tx(&self, tx: TxHandle);
    fn checkpoint(&self, mode: CheckpointMode) -> io::Result<()>;
}
```

`PAGE_SIZE = 16384`; `PageHeader` (32 байта) хранит `page_lsn` и `checksum` (xxHash3-64).
`PageKey = (graph_id, file_kind, page_no)`; `graph_id = 0` — системное пространство (каталог схемы
и графов). При рассинхроне страница восстанавливается применением фрейма из журнала.

### 3.4. `Operator` — модель исполнения

Гибрид vectorized + push-based (`gql_tech_spec.md` §9): оператор реализует
`fn next_batch(&mut self) -> Option<Batch>`, где `Batch = { columns: Vec<Array>, size: usize }`,
`Array<T> = { data: Vec<T>, validity: Bitmap }`, а `Sel = { indices: Vec<u32> }` — selection
vector для фильтрации без копирования. Подробности — в спецификации.

### 3.5. Тензорные представления — `EndpointTensor` / `ParticipationTensor` (🔴 P5)

Из `gql_spec.md` §3.8 (математика — `IncidenceTensor`, матрицы `B_ep`, `D` — остаётся в спеке).
`IncidenceMatrix`/`IncidenceTensor` сохраняются для обратной совместимости; эти два — новые
компоненты для полной инцидентности (все атомы, не только вершины).

```rust
/// Endpoint-тензор: atom × edge × 2 (Source/Target).
/// Обобщение IncidenceTensor на все атомы.
pub struct EndpointTensor {
    pub matrix:     DMatrix<u8>,               // shape: (|A|, |E| × 2)
    pub atom_index: FxHashMap<AtomId, usize>,  // nodes + edges
    pub edge_index: FxHashMap<AtomId, usize>,  // только edges
    pub index_atom: Vec<AtomId>,
}

impl EndpointTensor {
    /// matrix[(atom_i, edge_j * 2 + role)] = 0 или 1
    /// role: 0=Source (∈ inv), 1=Target (∈ out)
    pub fn from_graph(g: &MetaGraph) -> Self { ... }
    pub fn get(&self, atom_i: usize, edge_j: usize, role: usize) -> u8 { ... }
    /// Atom adjacency: C = B[:,:,0] × B[:,:,1]^T (binary)
    pub fn atom_adjacency_binary(&self) -> DMatrix<u8> { ... }
    /// Weighted atom adjacency с весами рёбер
    pub fn atom_adjacency_weighted(&self, g: &MetaGraph) -> DMatrix<f64> { ... }
    /// Подматрица для конкретного вида инцидентности
    pub fn submatrix_vv(&self, g: &MetaGraph) -> DMatrix<u8> { ... }
    pub fn submatrix_ve(&self, g: &MetaGraph) -> DMatrix<u8> { ... }
    pub fn submatrix_ev(&self, g: &MetaGraph) -> DMatrix<u8> { ... }
    pub fn submatrix_ee(&self, g: &MetaGraph) -> DMatrix<u8> { ... }
}

/// Participation-тензор: edge × edge × 3 (Source/Target/Control).
pub struct ParticipationTensor {
    pub matrix:     DMatrix<u8>,               // shape: (|E|, |E| × 3)
    pub edge_index: FxHashMap<AtomId, usize>,
    pub index_edge: Vec<AtomId>,
}

impl ParticipationTensor {
    pub fn from_graph(g: &MetaGraph) -> Self { ... }
    pub fn get(&self, e1_i: usize, e2_j: usize, role: usize) -> u8 { ... }
    /// Edge participation adjacency (Source+Target роли, без Control)
    pub fn participation_adjacency(&self) -> DMatrix<u8> { ... }
}
```

### 3.6. Новые компоненты (сводно)

Из `disk_storage_spec.md` §29.3.

| Компонент                     | Где                                      | Что делает                                  |
|-------------------------------|------------------------------------------|---------------------------------------------|
| `PageManager` trait           | `storage/mod.rs`                         | Абстракция page-level I/O (frame-WAL, §3.3) |
| `DiskMetaGraph`               | `storage/disk_graph.rs`                  | Disk-backed MetaGraph                       |
| `NodeHotSlot` / `EdgeHotSlot` | `storage/node_pages.rs`, `edge_pages.rs` | Encode/decode hot-слотов                    |
| `PropHeapPage`                | `storage/prop_heap.rs`                   | Slotted-page для PropertyMap                |
| `LabelDictionary`             | `storage/label_dict.rs`                  | Интернирование строк                        |
| `LabelIndex`                  | `index/label_index.rs`                   | Sorted array index по меткам                |
| `BTreeIndex`                  | `index/btree.rs`                         | B+tree property index                       |
| `FulltextIndex`               | `index/fulltext.rs`                      | Inverted index + posting lists              |
| `HnswIndex`                   | `index/hnsw.rs`                          | In-memory HNSW с checkpoint                 |

### 3.7. Зависимости (добавить в `Cargo.toml`)

Из `disk_storage_spec.md` §29.4 (🔄 decimal уточнён).

```toml
jiff = { version = "0.2", features = ["serde"] }        # DateTime, Duration
rust_decimal = { version = "1.42" }                     # 🔄 decimal (рантайм; диск — Arrow-форма)
xxhash-rust = { version = "0.8", features = ["xxh3"] }  # Page checksum
geo = { version = "0.33" }                              # planar geospatial geometries and algorithms
geojson = { version = "1.0.0" }                         # GeoJSON
```

---

## 4. План реализации по областям

Каждая область: краткое описание, статус (см. легенду §1), приоритет (P1 блокирующий … P6 MVP+),
зависимости и состав работ. Детализация по существующим файлам (`fg-meta` / `src`) взята из
`disk_storage_spec.md` §30.1–§30.7.

### Область A. Система типов и значения — P1 (блокирующая)

Фундамент, от которого зависит сериализация, хранилище, индексы и исполнитель.

- **`src/types/value.rs`** 🔴 — новый `Value` enum (§3.1). Полная замена; обратная совместимость
  не требуется.
- **`src/types/record_id.rs`** 🔴 — `RecordId`, `RecordIdPart` (§3.2).
- **`serial.rs`** 🟡 — обновить `write_value`/`read_value` под все новые `TypeTag`; `write_props`/
  `read_props` перевести на `key_id (u32)` вместо строк в disk-режиме; `CODEC_VERSION = 1` на
  всём этапе MVP (совместимость со снимками других форматов не поддерживается).
- **`src/types/datetime.rs`** 🔴 — полная интеграция `jiff` (`DateTime`, `Duration`).
- **`src/types/decimal.rs`** 🔴 — обёртка над `rust_decimal`; на диске — Arrow-форма `i128+scale`.
- **`src/types/geometry.rs`** 🔴 — парсинг/кодирование GeoJSON (`geo`, `geojson`).
- **`id.rs`** 🟡 — `AtomId`/`GraphId` остаются внутренними; добавить `DiskAtomRef (u64)` newtype
  (🔄) и `QualifiedAtomRef` (согласовать ширину `local_slot`).

### Область B. Страничное хранилище и I/O — P2

Disk-backed режим 2. Зависит от A.

- **`PageManager`** 🔴 (§3.3) — реализация поверх `BufferPool` (Clock-Pro) и `SyncIo`/`AsyncIo`.
- **`DiskMetaGraph`** 🔴 — фасад над страницами (`storage/disk_graph.rs`).
- **Disk-страницы** 🟡→🔴 — `NodeHotSlot`/`EdgeHotSlot`, `AdjacencyOverflowPage`, `EdgeIncidence`,
  `PropHeapPage` (slotted), `SubgraphDirPage`, `FreelistPage`, `LabelDictionary`. Каркас есть в
  `crates/storage_engine` (`store`, `layout`).
- **`store.rs`** 🟢/🔴 — существующие `create_graph`/`open_graph` (режим 1) без изменений;
  добавить `GraphStore::open_disk_graph(name) -> io::Result<DiskMetaGraph>` (P6, MVP+).
- **`graph.rs`** 🟢 — `MetaGraph` остаётся in-memory; единственное дополнение —
  `MetaGraph::from_disk_batch(...)` для эффективной пакетной загрузки атомов (P4, отложено).

### Область C. Журнал (frame-WAL), checkpoint, recovery — P2

Зависит от B. 🔄 Полностью переопределено под frame-WAL — ранняя редакция (`WalKind 0x10–0x22`,
раздельные `MvccWal`/`DiskWal`, per-`WalKind` idempotent-redo) **отменена**.

- **frame-WAL** 🔴 — `WalKind` для frame-модели (`disk_storage_spec.md` §22.1: `TxBegin`,
  `TxCommit`, `TxAbort`, `PageFrame`, `PageInit`, `CheckpointBegin`, `CheckpointEnd`,
  `GraphCreate`, `GraphDrop`); 33-байтовый заголовок записи с xxHash3-64; индекс версий страниц.
- **Единый журнал уровня БД** 🔴 — глобальные часы коммитов; каталог графов в `db.fgb`.
- **checkpoint** 🔴 — бэкфилл фреймов в `.fgb` по home-адресам; prune с
  `prunable_prefix = min(active_tx_start)`.
- **recovery** 🔴 — один проход строго по возрастанию LSN (порядок обязателен: применение в
  порядке коммитов вместо LSN давало потерю обновлений); torn-write закрывается фреймами.
- **`mvcc_persist.rs`** 🟡 — переиспользовать `WalWriter`/`WalSegmentManager` как транспорт;
  `PersistLayer::checkpoint()` расширить на flush `.fgb`.

### Область D. MVCC и транзакции (SSI) — P2

Зависит от C. Референс — `fg-meta` `mvcc.rs`.

- **SSI по умолчанию** 🟢 — уже реализован; SI/RC — opt-in (`ISOLATION LEVEL …`).
- **Снимки** 🟢/🔴 — снимок на весь запрос; чтение параметризовано `snapshot: Lsn` (§3.3).
- **`mvcc.rs` для режима 2** 🟡 — `VersionChain<T>` хранит typed T; для диска версии — bytes:
  использовать `MvccStore<Vec<u8>, BytesCodec>` как есть (bytes = сериализованный `PropertyMap`);
  убедиться, что `MvccManager` корректно работает с `DiskMetaGraph`. Изменения кода минимальны.
- **Graph-специфичная консистентность** 🔴 — при явном понижении до SI обязателен
  `ASSERT INVARIANTS` (структурные инварианты `DAG`/`TREE`/`BIPARTITE`).

### Область E. Схема и каталог — P3

- **`src/schema/`** 🔴 — `SchemaCatalog`; `TableDefinition`, `FieldDefinition`, `IndexDefinition`,
  `EventDefinition`, `FunctionDefinition`, `AnalyzerDefinition`, `ParamDefinition`
  (`disk_storage_spec.md` §18). Хранится в системном пространстве (`graph_id = 0`).

### Область F. Статистика — P3/P4

Без статистики графовый планировщик не работает — обязательна для MVP.

- **`src/stats/`** 🔴 — `StatisticsStore`: `GraphStatsPage`, `LabelStatsPage`,
  `PropertyHistogramPage` (equi-depth), `DegreeHistogramPage`; NDV через HyperLogLog; сбор при
  загрузке и инкрементально (per-transaction delta-счётчики), выборка фиксированного размера.

### Область G. Индексы (+ bulk-сборка) — P3–P5

Зависит от A, B, E.

- **`LabelIndex`** 🔴 (обязательный, неявный), **`BTreeIndex`** (standard/unique), **`CountIndex`**
  — P3.
- **Bulk-сборка** 🔴 — P3, критично: внешняя сортировка (memcmp по order-preserving ключам) +
  bottom-up packer + side-file / одна WAL-запись / смена поколения + import-эпоха
  (`BEGIN IMPORT`). Планировщик обязан задействовать bulk-путь автоматически при `DEFINE INDEX` на
  непустой таблице, `REBUILD INDEX`, `COMPACT INDEX`; `CONCURRENTLY` — поверх (снимок → side-file →
  добор небольшой очереди → атомарная подмена). `DEFER` — **не** замена bulk: переносит ту же
  построчную работу в фон (проблема латентности → проблема очереди), несовместим с `UNIQUE`.
- **`FulltextIndex`** (BM25, inverted + posting lists) — P4.
- **`HnswIndex`** — P5; durability через журналирование вставок, replay, пометка stale при разрыве.
- **`RtreeIndex`** (geometry, STR-packing) — P5.
- **`EdgeEndpointIndex`**, **`NeighbourhoodIndex`**, **`PathIndex`** — P4–P5.

### Область H. Query frontend (лексер / парсер) — P2

- **`src/syn`** 🟡 — довести лексер/парсер (reblessive, stackless, `TokenBuffer<4>`) до полной
  грамматики (`gql_grammar.md`) → `AST`; ошибки с рендерингом позиции.

### Область I. Семантический анализ (binding / typing / lowering) — P2

Зависит от H, A.

- **Binder** 🔴 — scope/alias (`gql_tech_spec.md` §4.1).
- **Typechecker** 🔴 — type inference, разрешение перегрузки `number` (`int`/`float`/`decimal`) на
  этапе анализа, трёхзначная логика `null`/`none`, classification purity, проверка computed-полей.
- **Lowering AST → IR** 🔴 — логический IR (§5), две под-алгебры (join engine ↔ traversal VM).

### Область J. Оптимизатор — P3

Зависит от I, F.

- **RuleEngine** 🔴 — rule-based правила (`gql_tech_spec.md` §6.1).
- **CostEngine** 🔴 — cost-based выбор по `StatisticsStore`; штрафы и деградация плана.
- **Plan cache** 🔴; физические альтернативы; `WITH`-подсказки.

### Область K. Физическое исполнение — P3

Зависит от J, B.

- **Физические операторы** 🔴 — Scan, Filter, Expand/PathExpand, Join (Index NL для MVP; Hash),
  Project; late materialization и доступ к свойствам; vectorized (`next_batch`) + push-based
  (§3.4); pipeline-breakers (Join build, GROUP BY, SORT, DISTINCT).
- **`Datastore::execute`** 🟡 — собрать тело: план → операторы → `Vec<QueryResult>`.
- Кооперативная отмена и таймауты (`TIMEOUT`/`statement_timeout_ms`).

### Область L. Traversal VM — P3

- **`src/gql/traversal`** 🟡 — довести `TraversalMachine`/`Traverser`/`TraversalContext`; классы
  шагов, sack, path-propagation, control flow, барьеры материализации; ключевая оптимизация —
  bulk; стыковка с декларативной ветвью (MATCH → join engine).

### Область M. Changefeed и Live — P5

- **`src/changefeed/`** 🔴 — `ChangefeedStore`, `ChangefeedPage`/`ChangefeedEntry`,
  `LiveQueryRegistry`. Событие ставится в очередь в той же транзакции; at-least-once с `ack`;
  порядок по versionstamp; `SINCE`-resume; `ON OVERFLOW GAP|DISCONNECT|BLOCK`; очереди in-memory,
  восстановление через `SINCE`.

### Область N. Полная инцидентность (граф-ядро) — P4

Из `disk_storage_spec.md` §30.9. Правки референс-ядра (`fg-meta`) для vertex–edge / edge–edge
инцидентности и кросс-уровневых рёбер.

| Компонент                         | Что исправить                                                                                                       |
|-----------------------------------|---------------------------------------------------------------------------------------------------------------------|
| `graph.rs::register_adjacency()`  | Путь для edge-endpoints: регистрировать в `EdgeEndpointIndex`, а не игнорировать                                    |
| `graph.rs::EdgeFlags`             | Добавить `CROSS_LEVEL = 0b0100_0000`                                                                                |
| `graph.rs::add_cross_meta_edge()` | Ставить `CROSS_LEVEL`; убрать `__from_inner`/`__to_inner`; использовать `source_atom_ref`/`target_atom_ref` в props |
| `node.rs::NodeFlags`              | Убрать бит `PORT` или пометить deprecated                                                                           |
| `traversal.rs::neighbors()`       | Параметризовать: `NodeOnly` (текущий) vs `AtomWalk` (новый, через `EdgeEndpointIndex` + `EdgeIncidence`)            |
| `traversal.rs::TarjanCuts`        | Параметризовать тип обхода; `n.is_node()` — только в `NodeOnly`                                                     |
| `matrix.rs`                       | Добавить `EndpointTensor` и `ParticipationTensor` (§3.5)                                                            |
| `id.rs`                           | Добавить `QualifiedAtomRef` (согласовать ширину с `DiskAtomRef` = u40)                                              |

### Область O. Мосты (bridges) — MVP+

- **Bridge trait** и реализации (`SurrealBridge`, …) — `gql_spec.md` §30. Публичный контракт
  `enum BridgeData` остаётся в спецификации. Реализация — после MVP.

---

## 5. Приоритизированный порядок работ (фазы)

Расширяет таблицу `disk_storage_spec.md` §30.8 работами query-слоя (`gql_tech_spec.md`).

| Приоритет            | Область | Компоненты                                | Что делать                                                          |
|----------------------|---------|-------------------------------------------|---------------------------------------------------------------------|
| **P1** (блокирующий) | A       | `src/types/value.rs`                      | Новый `Value` enum с полной системой типов                          |
| **P1**               | A       | `src/types/record_id.rs`                  | `RecordId`, `RecordIdPart`                                          |
| **P1**               | A       | `serial.rs`                               | Новые `TypeTag`, `key_id` в disk-режиме                             |
| **P1**               | A       | `src/types/datetime.rs`                   | Полная интеграция `jiff`                                            |
| **P1**               | A       | `src/types/decimal.rs`                    | `rust_decimal` + Arrow-форма на диске                               |
| **P1**               | A       | `src/types/geometry.rs`                   | GeoJSON parsing + encoding                                          |
| **P2** (высокий)     | B       | `src/storage/`                            | `PageManager`, `DiskMetaGraph`, все типы страниц                    |
| **P2**               | C       | frame-WAL                                 | `WalKind` frame-модели (§22.1), индекс версий, checkpoint, recovery |
| **P2**               | H, I    | `src/syn`, binder, typechecker, lowering  | Фронтенд → AST → IR                                                 |
| **P3** (средний)     | E       | `src/schema/`                             | `SchemaCatalog`                                                     |
| **P3**               | G       | `src/index/`                              | `LabelIndex`, `BTreeIndex`, `CountIndex`, bulk-сборка               |
| **P3**               | J, K, L | оптимизатор, физ. операторы, Traversal VM | `Datastore::execute` end-to-end (режим 1)                           |
| **P4** (нормальный)  | F       | `src/stats/`                              | `StatisticsStore`                                                   |
| **P4**               | G       | `src/index/fulltext.rs`                   | `FulltextIndex` (BM25)                                              |
| **P4**               | N       | `fg-meta` граф-ядро                       | Полная инцидентность                                                |
| **P5** (низкий)      | G       | `src/index/hnsw.rs`, `rtree.rs`           | `HnswIndex`, `RtreeIndex`                                           |
| **P5**               | M       | `src/changefeed/`                         | `ChangefeedStore`, `LiveQueryRegistry`                              |
| **P5**               | 3.5     | `matrix.rs`                               | `EndpointTensor`, `ParticipationTensor`                             |
| **P6** (MVP+)        | B       | `store.rs`                                | `open_disk_graph` (полный режим 2)                                  |
| **MVP+**             | O       | bridges                                   | `Bridge` trait, `SurrealBridge`                                     |

**Фазы (укрупнённо):**

- **Фаза 0 — фундамент типов** (P1): Область A целиком. Блокирует всё остальное.
- **Фаза 1 — страничное хранилище** (P2): Область B + интеграция BufferPool.
- **Фаза 2 — журнал и восстановление** (P2): Область C + D (снимки, чтение на снимке).
- **Фаза 3 — query-конвейер MVP** (P2–P3): H → I → J/K/L, доведение `Datastore::execute`
  сначала для in-memory режима 1.
- **Фаза 4 — схема, статистика, индексы** (P3–P4): E, F, G (Label/BTree/Count + bulk),
  подключение к оптимизатору.
- **Фаза 5 — продвинутые индексы и Live** (P4–P5): Fulltext, HNSW, R-tree; N (полная
  инцидентность); M (changefeed/live).
- **Фаза 6 — полный disk-backed режим 2 и мосты** (P6, MVP+): `open_disk_graph`, O.

---

## 6. Отложено / вне MVP

Сводно из `gql_tech_spec.md` §18, `gql_spec.md` §32.3 и заметок `disk_storage_spec.md`.

| Область                                                        | Решение                                                                              |
|----------------------------------------------------------------|--------------------------------------------------------------------------------------|
| JIT-компиляция (LLVM: expression/operator-fusion/pipeline JIT) | Не входит в контракт языка; implementation detail; после стабилизации интерпретатора |
| Worst-Case Optimal Join (Leapfrog TrieJoin)                    | Базовый Index NL достаточен для MVP; WCOJ — для паттернов с циклами позже            |
| Полная реализация disk-backed graph-native pages (режим 2)     | Каркас есть; полная реализация — большая работа, для in-memory MVP не нужна          |
| Physical-WAL + ARIES (Undo/CLR)                                | Не требуется: frame-WAL даёт те же гарантии без UNDO/CLR                             |
| Параллельный recovery                                          | Отложено; single-pass достаточен для MVP                                             |
| SPDK / NVMe kernel-bypass                                      | Задел (`spdk.rs`), Linux-only, не развивается                                        |
| SMT-верификация оптимизатора                                   | Long-term; property-based тесты — практический инструмент                            |
| Тензорные кеши (dense/CSR на диске)                            | `TensorCachePage` — для ML/аналитики позже                                           |
| Graph-специфические индексы (adjacency, reachability)          | Future                                                                               |
| Path-tree cover / Chain-Cover                                  | Future (read amplification без очевидного выигрыша)                                  |
| Distributed execution                                          | Вне scope; движок embedded-only                                                      |

---

*Синхронизируется с `gql_spec.md`, `gql_grammar.md`, `disk_storage_spec.md`, `gql_tech_spec.md`.
При расхождении приоритет — у спецификаций (они фиксируют контракт); этот файл описывает
реализацию и порядок работ.*
