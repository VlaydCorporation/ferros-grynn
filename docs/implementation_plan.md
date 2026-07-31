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

| Решение                                                                                           | Действует                                              |
|---------------------------------------------------------------------------------------------------|--------------------------------------------------------|
| Журнал — **frame-WAL / COW-в-логе** (образ страницы + индекс версий), не ARIES, не data-file COW  | `disk_storage_spec.md` §22–§23, `gql_tech_spec.md` §12 |
| **Единый журнал уровня БД** (не пожурнально на граф); глобальные часы коммитов                    | `disk_storage_spec.md` §7                              |
| `AtomId = gen(23) \| kind(1) \| slot(40)`, `DiskAtomRef = u64`                                    | `disk_storage_spec.md` §9                              |
| **SSI по умолчанию** (SI/RC — opt-in)                                                             | `gql_spec.md` §17.1                                    |
| `decimal` — `rust_decimal` в рантайме, Arrow-форма `i128 + scale` (18 байт) на диске              | `disk_storage_spec.md` §3.3, §5.2                      |
| `none`/`null` — разные sentinel, но вся логика двухзначная; `option<T>` не имеет runtime-варианта | `gql_spec.md` §5–§6                                    |
| `PropHeapPage` — row-store; аналитика — `PropertyColumnCachePage`/Arrow batch                     | `disk_storage_spec.md` §13.6                           |
| Graph results — frozen owned values; disk views ограничены lifetime транзакции                    | `gql_spec.md` §16–§17, `graphs.md`                     |
| Массовая загрузка — `BEGIN IMPORT … END IMPORT` + bulk-сборка индексов (без LSM/delta-store)      | `gql_spec.md` §7.11, `disk_storage_spec.md` §20.13     |

---

## 1. Соответствие кодовой базе

Объединяет `gql_spec.md` §31 и `gql_tech_spec.md` §17.

Легенда статуса: 🟢 реализовано (в `fg-meta` как референс или в `src`); 🟡 каркас (есть модуль,
тело неполное); 🔴 новое (требует реализации с нуля).

### 1.1. Согласовано / реализовано / каркас

| Область                                                                                                                              | Контракт (спека)                       | Модуль                                                                                                       | Статус                                                      |
|--------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------|--------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------|
| Модель данных `G = ⟨V, MV, E, ME⟩`                                                                                                   | `gql_spec` §3                          | `fg-meta` `graph.rs` → `src/values/runtime/graph`                                                            | 🟢 референс                                                 |
| `AtomId` (генерационный, kind-бит, u64)                                                                                              | `disk` §9                              | `fg-meta/id.rs` → `src/values/runtime/graph`                                                                 | 🟢 foundation (`gen(23) \| kind(1) \| slot(40)`)            |
| Значения (`Value`, runtime/storage/columnar)                                                                                         | `gql_spec` §5–§6; `disk` §5            | `src/values` (`runtime`, `storage`, `columnar`)                                                              | 🟡 активный, этап спецификаций завершён                     |
| Диагностика (`StatusDefinition + Status + DiagnosticRecord → StatusObject → FerrosGrynnError`)                                       | —                                      | `src/gql_status`                                                                                             | 🟢                                                          |
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
| `RecordId` / `DiskAtomRef` / `QualifiedAtomRef`                                                                                      | `disk` §4, §9                          | `src/values/runtime`, `src/values/storage`, `src/storage`                                                    | 🟡 runtime `RecordId` готов; disk refs — новые              |
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
| `PropertyMap`        | Решено по benchmark evidence: отдельный от `Object` pointer-sized sorted `ThinVec` по `PropertyKeyId`; Arrow используется только в `PropertyBatch`.             |
| `GraphId` / `AtomId` | Внутренние идентификаторы не экспонируются в GrynQL; публичная форма — `table:id` (`RecordId`).                                                                 |
| Гиперрёбра           | Реализованы (`HyperDirected`, `HyperUndirected` в `edge.rs`). Синтаксис `RELATE HYPER` / `MATCH HYPER` — новый, требует согласования.                           |
| Метаатомы в GQL      | `MetaGraph.promote_to_meta()` реализован; GrynQL-синтаксис (`graph::inner`/`graph::nodes`) требует реализации функций.                                          |
| ECS-компоненты       | `src/ecs` — AtomId-keyed runtime/ML sidecar; `GraphWorkspace` доступен Rust SDK, но не является `Value` и не экспонирует произвольные компоненты в GrynQL.      |

---

## 2. Целевая структура модулей

Из `disk_storage_spec.md` §29.1.

> 🔄 **Замечание о раскладке.** `src/values` — система значений, `src/storage` — перенесённый в
> основную кодовую базу `crates/storage_engine`. Runtime не зависит от Arrow/DataFusion или
> storage engine. Отдельный crate удаляется только после parity-тестов.

```
src/
  ecs/
    mod.rs             -- paged SparseSet, AnyPool, WorldComponents
    sparse_set.rs      -- node/edge sparse pages + full AtomId generation check
    workspace.rs       -- GraphWorkspace и revision-bound invalidation
    property_column.rs -- (PropertyKeyId, TypeId) materialized columns
    join.rs            -- read-only smallest-pool joins
  values/
    mod.rs
    runtime/
      mod.rs          -- Value, ValueKind, Type; без TypeTag
      number.rs       -- Number с rust_decimal::Decimal
      text.rs         -- публичный Text над CompactString
      array.rs
      set.rs
      object.rs       -- публичный BTreeMap<Text, Value>
      range.rs
      record_id.rs
      temporal.rs
      ids.rs          -- Uuid/Ulid newtype с inner API
      property_map.rs -- PropertyKeyId + sorted ThinVec
      graph/          -- graph family, Mutable/Frozen, Path, LiveGraph
    storage/
      mod.rs          -- StorageValue, StorageValueRef, TypeTag
      codec.rs        -- canonical value codec v1 + DecodeLimits
      context.rs      -- dictionaries и ref resolution
    columnar/
      mod.rs          -- PropertyBatch
      arrow.rs        -- Arrow arrays + presence bitmap
      datafusion.rs   -- batch integration

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

### 3.1. `Value` — система типов значений (🟢 P1)

Детальный порядок работ — [plan.md](plan.md). Публичный runtime использует типы без суффикса
`Value`: `Array`, `Bytes`, `Set`, `Object`, `Range`, `Duration`, `DateTime`, `Path`, `Graph`
и т. д. Строки — доменный `Text` над `CompactString`; decimal — `rust_decimal::Decimal` внутри `Number`.

```rust
#[non_exhaustive]
pub enum Value {
    None,
    Null,
    Bool(bool),
    Number(Number),
    String(Text),
    Bytes(Bytes),
    DateTime(DateTime),
    Duration(Duration),
    Uuid(Uuid),
    Ulid(Ulid),
    Record(RecordId),
    Array(Array),
    Set(Set),
    Object(Object),
    Range(Range),
    Geometry(Geometry),
    Vector(Vector),
    // graph/domain/runtime-only variants добавляются по этапам plan.md
}
```

`ValueKind` задаёт runtime rank, `Type` — статические GQL-типы, `TypeTag` живёт только в
`values::storage` и соответствует `disk_storage_spec.md` §5.2. `option<T>` — только `Type`:
`Some(v) = v`, `None = Value::None`. `OrdValue` и `Value::Option` не вводятся.

`Value` реализует согласованные `Eq`/`Ord`/`Hash`; rank не зависит от порядка enum-вариантов.
Mutable/live/snapshot-bound graph objects не помещаются в `Value`. Storage conversion
отклоняет runtime-only значения структурированной диагностикой.

### 3.2. Идентификаторы — `RecordId` / `DiskAtomRef` / `QualifiedAtomRef`

Три уровня (`disk_storage_spec.md` §4, §9):

- **`RecordId`** (`table:id`) — публичный, единственный видимый в GrynQL; хранится как свойство `id`.
  Отдельный публичный тип в `src/values/runtime/record_id.rs`.
- **`DiskAtomRef`** (`u64`: бит 40 = kind, биты 39..0 = slot) — стабильный дисковый id без
  generation. 🔄 newtype в `src/storage/mod.rs` (ранее планировался `u32`).
- **`AtomId`** (`u64`: slot + generation) — внутренний in-memory handle; **никогда** не
  экспонируется в GrynQL. `AtomId` и `GraphId` остаются внутренними.
- **`QualifiedAtomRef { sg_slot: u32, local_ref: DiskAtomRef }`** — кросс-уровневый адрес
  (`disk_storage_spec.md` §3.9); payload занимает 12 байт, Rust-структура с выравниванием — 16.

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

| Компонент                       | Где                                      | Что делает                                    |
|---------------------------------|------------------------------------------|-----------------------------------------------|
| `PageManager` trait             | `storage/mod.rs`                         | Абстракция page-level I/O (frame-WAL, §3.3)   |
| `DiskMetaGraph`                 | `storage/disk_graph.rs`                  | Disk-backed MetaGraph                         |
| `NodeHotSlot` / `EdgeHotSlot`   | `storage/node_pages.rs`, `edge_pages.rs` | Encode/decode hot-слотов                      |
| `PropHeapPage`                  | `storage/prop_heap.rs`                   | Slotted-page для PropertyMap                  |
| `LabelDictionary`               | `storage/label_dict.rs`                  | Интернирование строк                          |
| `LabelIndex`                    | `index/label_index.rs`                   | Sorted array index по меткам                  |
| `BTreeIndex`                    | `index/btree.rs`                         | B+tree property index                         |
| `FulltextIndex`                 | `index/fulltext.rs`                      | Inverted index + posting lists                |
| `HnswIndex`                     | `index/hnsw.rs`                          | In-memory HNSW с checkpoint                   |
| `SparseSet` / `WorldComponents` | `ecs/`                                   | AtomId-keyed typed component columns          |
| `GraphWorkspace`                | `ecs/workspace.rs`                       | Revision-bound graph + ECS sidecar            |
| `PropertyColumn<T>`             | `ecs/property_column.rs`                 | Typed property materialization for algorithms |

### 3.7. Зависимости (добавить в `Cargo.toml`)

Из `disk_storage_spec.md` §29.4 (🔄 decimal уточнён).

```toml
compact_str = { workspace = true, features = ["rkyv"] } # inner storage for runtime Text
jiff = { version = "0.2", features = ["serde"] }        # DateTime, Duration
rust_decimal = { version = "1.42" }                     # Number::Decimal
thin-vec = "0.2.19"                                    # pointer-sized sorted PropertyMap
smallvec.workspace = true                               # adjacency/traversal buffers
rapidhash.workspace = true                              # registries, indexes and ECS pools
rkyv = { workspace = true, features = ["bytecheck"] }   # snapshots/caches, не canonical codec
zerocopy.workspace = true                               # fixed page headers/slots
arrow.workspace = true                                  # PropertyBatch
datafusion.workspace = true                             # только columnar adapter
xxhash-rust = { version = "0.8", features = ["xxh3"] }  # Page checksum
geo = { version = "0.33" }                              # planar geometry
geojson = { version = "1.0.0" }                         # GeoJSON
```

### 3.8. ECS и `GraphWorkspace` (🟡 P2)

`src/ecs` развивается из прототипа `fg-meta/ecs.rs` и идей
[EnTT](https://github.com/skypjack/entt/blob/main/single_include/entt/entt.hpp), но не переносит
его entity layout буквально:

```rust
pub struct SparseSet<T> {
    node_sparse_pages: Vec<Option<Box<[SparseEntry; SPARSE_PAGE]>>>,
    edge_sparse_pages: Vec<Option<Box<[SparseEntry; SPARSE_PAGE]>>>,
    dense_atoms: Vec<AtomId>,
    dense_components: Vec<T>,
}

pub struct WorldComponents {
    components: RapidHashMap<TypeId, Box<dyn AnyPool>>,
    property_columns: RapidHashMap<(PropertyKeyId, TypeId), Box<dyn AnyPool>>,
}

pub struct GraphWorkspace<G> {
    graph: G,
    components: WorldComponents,
    bound_identity: Uuid,
    bound_revision: u64,
}
```

Инварианты:

- ECS не создаёт `Entity`: источником идентичности является graph `SlotAllocator`.
- Sparse lookup разделяет node/edge slot spaces и после dense-index проверяет полный `AtomId`,
  поэтому stale generation не получает компонент нового атома.
- `remove_atom` выполняется во всех pools при освобождении slot.
- Read-only join выбирает наименьший pool фактически, а не требует от caller поставить его первым.
- `PropertyColumn<T>` отделён от обычного TypeId-component pool и сохраняет различие
  `none`/`null` через presence/validity.
- `GraphWorkspace` — sidecar, не поле `Graph`: arbitrary components не обязаны быть `Clone`, не
  участвуют в `Value::Graph`, `Eq`/`Hash`, canonical codec или durability.
- Workspace связывает derived pools с `(identity, revision)`; graph mutation инвалидирует
  затронутые pools. Persisted/query-visible компонент сначала конвертируется в `PropertyMap`.
- `query_to_memory` продолжает возвращать точный `T<State>`; пользователь создаёт workspace через
  `into_workspace`, а query engine может использовать тот же тип внутри аналитического pipeline.

---

## 4. План реализации по областям

Каждая область: краткое описание, статус (см. легенду §1), приоритет (P1 блокирующий … P6 MVP+),
зависимости и состав работ. Детализация по существующим файлам (`fg-meta` / `src`) взята из
`disk_storage_spec.md` §30.1–§30.7.

**Этапы 1–2 плана `values`: ✅ завершены.** Спецификации синхронизированы; реализованы
runtime scalars и collections, `Value`/`ValueKind`/`Type`, exact `Number`, temporal,
UUID/ULID, `RecordId`, geometry/vector, runtime-only identity и graph foundation.
Следующий исполняемый этап — `PropertyMap` dictionary/benchmarks и columnar materialization.

### Область A. Система типов и значения — P1 (блокирующая)

Фундамент, от которого зависит сериализация, хранилище, индексы и исполнитель.

- **`src/values/runtime`** 🟢 — `Value`, `ValueKind`, `Type`, scalars и коллекции;
  `Text(CompactString)`, `Number::Decimal(Decimal)`, согласованные `Eq`/`Ord`/`Hash`; без
  `OrdValue`, `Tuple` и `Value::Option`. Fallible public API возвращает `FerrosGrynnResult`;
  value-диагностики принадлежат `Domain::Value`, external parser errors остаются redacted
  `technical_source`.
- **`src/values/runtime/record_id.rs`** 🟢 — `RecordId`, `RecordIdPart`; object-ID хранит
  строковые ключи и имеет канонический UTF-8 порядок.
- **`src/values/runtime/property_map.rs`** 🟡 — реализованы `PropertyKeyId` и однословный sorted
  `ThinVec`. Benchmark отверг boxed/tagged `SmallVec`/`RapidHashMap`: первая вставка выделяла
  656 B, а настоящий inline union занимал 672 B на каждом атоме. Empty не аллоцирует, non-empty
  имеет один contiguous allocation и canonical order без временной сортировки. Dictionary
  относится к следующему этапу.
- **`src/ecs`** 🟡 — перенесённый `Registry<Entity>` является временным каркасом. Заменить на
  paged generation-aware `SparseSet<AtomId>`, `WorldComponents`, property pools и
  `GraphWorkspace`; не подключать независимый entity allocator к графу.
- **`src/values/storage`** 🔴 — `StorageValue`, borrowed `StorageValueRef`, encode/decode
  contexts, `TypeTag` из `disk_storage_spec.md` §5.2, canonical codec v1 и `DecodeLimits`.
- **`src/values/columnar`** 🔴 — `PropertyBatch`, presence bitmaps, Arrow conversion и
  DataFusion adapter; runtime от этого слоя не зависит.
- **`src/values/runtime/graph`** 🟡/🟢 reference — реализованы general SoA foundation,
  bit-packed `AtomId`, node/edge и edge/edge incidence, identity/revision,
  `Graph<Mutable>`/`Graph<Frozen>` и корневой re-export; специализированные storage families,
  conversions, PathTrace/Path и LiveGraph остаются этапом 4 [plan.md](plan.md).
- **`src/storage`** 🟡 — `DiskAtomRef(u64)` и
  `QualifiedAtomRef { sg_slot, local_ref: DiskAtomRef }`; перенос `crates/storage_engine`
  выполняется после стабилизации value codec.

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
  этапе анализа, двухзначная sentinel-семантика `null`/`none`, classification purity, проверка
  computed-полей; `option<T>` стирается в `T | none`.
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
- **`LiveGraph<Frozen>`** 🔴 — COW delta, atomic publish frozen revision и revision guard.
- **`LiveGraph<Mutable>`** 🔴 — dirty-set, incoming queue, pause-on-conflict и явные
  `accept_remote`/`keep_local`/`discard_local`/`rebase`/`flush`/`resume`; ни remote delta, ни
  local changes не коммитятся при конфликте автоматически.

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
| `id.rs`                           | Добавить `QualifiedAtomRef { sg_slot: u32, local_ref: DiskAtomRef }`                                                |

### Область O. Мосты (bridges) — MVP+

- **Bridge trait** и реализации (`SurrealBridge`, …) — `gql_spec.md` §30. Публичный контракт
  `enum BridgeData` остаётся в спецификации. Реализация — после MVP.

---

## 5. Приоритизированный порядок работ (фазы)

Расширяет таблицу `disk_storage_spec.md` §30.8 работами query-слоя (`gql_tech_spec.md`).

| Приоритет            | Область | Компоненты                                | Что делать                                                          |
|----------------------|---------|-------------------------------------------|---------------------------------------------------------------------|
| **P1** (блокирующий) | A       | `src/values/runtime`                      | ✅ Runtime-фундамент, `ValueKind`, scalars и collections            |
| **P1**               | A       | `src/values/storage`                      | `StorageValue`, `TypeTag`, canonical codec и decode limits          |
| **P1**               | A       | `src/values/runtime/record_id.rs`         | ✅ `RecordId`, `RecordIdPart`                                       |
| **P2**               | A       | `src/values/columnar`                     | `PropertyBatch`, Arrow/presence, затем DataFusion                   |
| **P2**               | A       | `src/values/runtime/graph`                | Graph family/type-state, Path и query return contracts              |
| **P2**               | A       | `src/ecs`                                 | SparseSet, WorldComponents, PropertyColumn и GraphWorkspace         |
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

- **Фаза 0 — синхронизация спецификаций**: ✅ завершена; нормативные контракты values,
  graph/path/live и storage codec согласованы.
- **Фаза 1 — фундамент типов** (P1): runtime-часть Области A, затем StorageValue/codec.
- **Фаза 1.5 — свойства и ECS sidecar** (P2): PropertyMap benchmarks/dictionary,
  generation-aware SparseSet, PropertyColumn и GraphWorkspace foundation.
- **Фаза 2 — страничное хранилище** (P2): Область B + интеграция BufferPool.
- **Фаза 3 — журнал и восстановление** (P2): Область C + D (снимки, чтение на снимке).
- **Фаза 4 — query-конвейер MVP** (P2–P3): H → I → J/K/L, доведение `Datastore::execute`
  сначала для in-memory режима 1.
- **Фаза 5 — схема, статистика, индексы** (P3–P4): E, F, G (Label/BTree/Count + bulk),
  подключение к оптимизатору.
- **Фаза 6 — продвинутые индексы и Live** (P4–P5): Fulltext, HNSW, R-tree; N (полная
  инцидентность); M (changefeed/live).
- **Фаза 7 — полный disk-backed режим 2 и мосты** (P6, MVP+): `open_disk_graph`, O.

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
