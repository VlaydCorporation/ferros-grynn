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

| Область                                                                                                                              | Контракт (спека)                       | Модуль                                                                                | Статус                                           |
|--------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------|---------------------------------------------------------------------------------------|--------------------------------------------------|
| Модель данных `G = ⟨V, MV, E, ME⟩`                                                                                                   | `gql_spec` §3                          | `fg-meta` `graph.rs` → `src/values/runtime/graph`                                     | 🟢 референс                                      |
| `AtomId` (генерационный, kind-бит, u64)                                                                                              | `disk` §9                              | `fg-meta/id.rs` → `src/values/runtime/graph`                                          | 🟢 foundation (`gen(23) \| kind(1) \| slot(40)`) |
| Значения (`Value`, runtime/storage/columnar)                                                                                         | `gql_spec` §5–§6; `disk` §5            | `src/values` (`runtime`, `storage`, `columnar`)                                       | 🟡 активный, этап спецификаций завершён          |
| Диагностика (`StatusDefinition + Status + DiagnosticRecord → StatusObject → FerrosGrynnError`)                                       | —                                      | `src/gql_status`                                                                      | 🟢                                               |
| Изоляция Serializable по умолчанию, SI/RC — opt-in                                                                                   | `gql_spec` §17.1                       | `src/datastore.rs`; `fg-meta` используется только как критически проверяемый референс | 🟢 Rust API режима 1 / 🔴 синтаксис `BEGIN`      |
| MVCC, COW-снимки, atom-level FCW и точные predicate reads                                                                            | `gql_spec` §17                         | `src/datastore.rs`, `src/collections/cow_vec.rs`, `src/values/runtime/graph`          | 🟢 RAM и Windows local                           |
| Журнал (frame-WAL, уровень БД)                                                                                                       | `disk` §22–§23                         | `src/storage`                                                                         | 🟢 Windows-first frame-WAL/checkpoint/recovery   |
| Snapshot / store, сегментная ротация                                                                                                 | `disk` §7                              | `fg-meta` `store.rs`                                                                  | 🟢                                               |
| Sync Direct I/O, затем BufferPool и optional Native Async                                                                            | `disk` §2                              | production `src/storage`; `fg-meta` — только референс сценариев                       | 🟢 Windows sync + IoRing / 🟡 Linux+soak         |
| Подграфы (`SubGraph`, `TreeSubGraph`, `DagSubGraph`, …)                                                                              | `gql_spec` §16                         | `fg-meta` `subgraph.rs`                                                               | 🟢 референс                                      |
| Алгоритмы traversal (BFS/DFS/Dijkstra/Bellman-Ford/SCC/LCA/MST/cuts)                                                                 | `gql_spec` §13.4                       | `fg-meta` `traversal.rs`                                                              | 🟢 референс                                      |
| Лексер / парсер (reblessive, `TokenBuffer<4>`)                                                                                       | `gql_grammar`                          | `src/syn` (`lexer.rs`, `parser.rs`, `ast.rs`)                                         | 🟡 read-only срез                                |
| AST / `Expr`                                                                                                                         | `gql_spec` §26                         | `src/syn/ast.rs`; публичный вход `gql::parse`                                         | 🟡 read-only срез                                |
| `Datastore::execute → Vec<QueryResult>`                                                                                              | `gql_tech` §2                          | `src/datastore.rs` → `src/gql`                                                        | 🟢 read-only `SELECT`/`LET`/`RETURN`             |
| Traversal VM (`TraversalMachine`, `Traverser`, `TraversalContext`)                                                                   | `gql_tech` §8                          | `src/gql` (будущий вертикальный этап)                                                 | 🔴 новое                                         |
| `PageManager` / disk-страницы                                                                                                        | `gql_tech` §10; формат — `disk` §9–§21 | `src/storage`                                                                         | 🟢 Windows manager/codecs/lazy graph adapter     |
| `RecordId` / `DiskAtomRef` / `QualifiedAtomRef`                                                                                      | `disk` §4, §9                          | `src/values/runtime`, `src/values/storage`, `src/storage`                             | 🟢 идентификаторы и primary lookup первого среза |
| `SchemaCatalog`                                                                                                                      | `disk` §18                             | `src/storage/catalog.rs`; полный DDL — будущий `src/schema`                           | 🟡 durable tables/fields первого среза           |
| `StatisticsStore`                                                                                                                    | `disk` §19                             | `src/storage/graph_pages.rs`; полный store — будущий `src/stats`                      | 🟡 минимальная graph/count statistics            |
| Индексы (Label/BTree/Count/Fulltext/HNSW/R-tree/…)                                                                                   | `disk` §20                             | `src/storage/graph_index.rs`; advanced — будущий `src/index`                          | 🟡 label dictionary + primary B+tree             |
| Changefeed / Live                                                                                                                    | `gql_spec` §21, `disk` §21             | `src/changefeed` (нов.)                                                               | 🔴 новое                                         |
| Query-слой: binder / typechecker, lowering AST→IR, RuleEngine/CostEngine, физ. операторы, стыковка Traversal VM ↔ реляционный движок | `gql_tech` §4–§9                       | `src/gql` (`analysis.rs`, `ir.rs`, `execution.rs`)                                    | 🟡 read-only срез; optimizer/traversal впереди   |

### 1.2. Требует согласования / открытые вопросы

Из `gql_spec.md` §31.2.

| Вопрос               | Описание                                                                                                                                                        |
|----------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Формат хранилища     | Ранние документы описывали LSM (L0/L1/compaction). Действует WAL + snapshot без LSM; GQL-спека внутреннее хранилище не регламентирует — противоречие устранено. |
| WAL recovery         | Ранний документ описывал ARIES 3-phase. Действует **frame-WAL** с single-pass REDO по LSN; ARIES/UNDO не нужен (см. таблицу решений).                           |
| `PropertyMap`        | Решено по benchmark evidence: отдельный от `Object` pointer-sized sorted `ThinVec` по `PropertyKeyId`; Arrow используется только в `PropertyBatch`.             |
| `GraphId` / `AtomId` | Внутренние идентификаторы не экспонируются в GrynQL; публичная форма — `table:id` (`RecordId`).                                                                 |
| Гиперрёбра           | Попарная traversal-семантика и ортогональные `ATOM HYPER` согласованы; прототипные поля endpoint'ов требуют миграции на `source`/`target`.                      |
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
    mod.rs            -- crate-private storage boundary
    page.rs           -- AlignedPage, typed PageHeader fields, portable checksum codec
    direct.rs         -- sync positional Direct I/O (Linux/Windows)
    buffer_pool.rs    -- Clock-Pro после раннего I/O-gate
    types.rs          -- Lsn/TxId/GraphId/DataFileId/PageKey/StorageSnapshot
    file_registry.rs  -- sealed trusted data-file routing
    local.rs          -- Windows db.lock + atomic superblock replace
    page_manager.rs   -- SyncPageManager, PageRead, checkpoint/recovery
    superblock.rs     -- DatabaseSuperblock, GraphSuperblock codecs
    node_pages.rs     -- NodeHotSlot, NodeHotPage
    edge_pages.rs     -- EdgeHotSlot, EdgeHotPage
    adj_overflow.rs   -- AdjacencyOverflowPage, CrossLevelAdjacencyOverflowPage
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

Детальный порядок работ — [values_plan.md](values_plan.md). Публичный runtime использует типы без суффикса
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
    // graph/domain/runtime-only variants добавляются по этапам values_plan.md
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

### 3.3. `SyncPageManager` — страничный доступ (frame-WAL) (🟢 P2, Windows-first)

🔄 **Действующий контракт** — из `gql_tech_spec.md` §10.2. Он заменяет более раннюю редакцию
(`alloc/free/read/write_page_dirty/flush_*/page_count`), которая предполагала STEAL и запись
страниц на месте: под frame-WAL запись — это добавление фрейма в журнал, а чтение
параметризовано снимком.

```rust
enum PageRead<'tx> {
    Staged(&'tx AlignedPage),
    Cached(PageGuard),
}

impl SyncPageManager {
    fn begin_tx(&self) -> FerrosGrynnResult<TxHandle>;
    fn read_page<'tx>(&self, tx: &'tx TxHandle, key: PageKey)
        -> FerrosGrynnResult<PageRead<'tx>>;
    fn read_pages<'tx>(&self, tx: &'tx TxHandle, keys: &[PageKey])
        -> FerrosGrynnResult<Vec<PageRead<'tx>>>;
    fn stage_frame(&self, tx: &mut TxHandle, key: PageKey, page: &AlignedPage)
        -> FerrosGrynnResult<()>;
    fn commit_tx(&self, tx: TxHandle) -> FerrosGrynnResult<CommitOutcome>;
    fn abort_tx(&self, tx: TxHandle);
    fn checkpoint(&self) -> FerrosGrynnResult<bool>;
    fn restart_checkpoint(&self) -> FerrosGrynnResult<bool>;
}
```

Оставлен один concrete crate-private manager: trait с одной реализацией не нужен. `TxHandle`
удерживает регистрацию snapshot; bare `StorageSnapshot` наружу не передаётся. Durable file
catalog, atom-slot alloc/free и `CheckpointMode::Restart` подключены этапом 4.

`PAGE_SIZE = 16384`; `PageHeader` (32 байта) хранит `page_lsn` и portable RapidHash V3 checksum
(seed `0`, checksum-поле обнулено при вычислении).
`PageKey = (GraphId, DataFileId, PageNo)`; `graph_id = 0` — системное пространство (каталог схемы
и графов). Версия фрейма доступна только при `commit_lsn ≤ tx.snapshot.visible_lsn`; `commit_ts`
используется отдельно для MVCC/SSI. Checkpoint не продвигает `.fgb` дальше самого старого живого
reader watermark. При рассинхроне страница восстанавливается применением фрейма из журнала.

### 3.4. `Operator` — модель исполнения

Гибрид vectorized + push-based (`gql_tech_spec.md` §9): оператор реализует
`fn next_batch(&mut self) -> Option<Batch>`, где `Batch = { columns: Vec<Array>, size: usize }`,
`Array<T> = { data: Vec<T>, validity: Bitmap }`, а `Sel = { indices: Vec<u32> }` — selection
vector для фильтрации без копирования. Подробности — в спецификации.

### 3.5. Тензорное представление — `EndpointTensor` (🔴 P5)

Из `gql_spec.md` §3.8. `IncidenceMatrix`/`IncidenceTensor` сохраняются как vertex-only views;
`EndpointTensor` охватывает все атомы единого endpoint-механизма. `from_graph` логически
помещает endpoint неориентированного ребра в обе стороны тензора, даже если storage хранит
единый массив `source`.

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
    /// matrix[(atom_i, edge_j * 2 + side)] = 0 или 1
    /// side: 0=Source, 1=Target
    pub fn from_graph(g: &MetaGraph) -> Self { ... }
    pub fn get(&self, atom_i: usize, edge_j: usize, side: usize) -> u8 { ... }
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
```

### 3.6. Новые компоненты (сводно)

Из `disk_storage_spec.md` §29.3.

| Компонент                       | Где                                      | Что делает                                    |
|---------------------------------|------------------------------------------|-----------------------------------------------|
| `SyncPageManager`               | `storage/page_manager.rs`                | WAL/home page-level I/O (frame-WAL, §3.3)     |
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
rapidhash.workspace = true                              # registries + portable V3 disk checksum
rkyv = { workspace = true, features = ["bytecheck"] }   # snapshots/caches, не canonical codec
arrow = { workspace = true, optional = true }           # PropertyBatch; workspace 58.4
datafusion = { workspace = true, optional = true }      # adapter; workspace 54.1
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

**Этапы 1–5 плана `values`: ✅ завершены на уровне runtime и value-codec контрактов.** Спецификации синхронизированы; реализованы
runtime scalars и collections, `Value`/`ValueKind`/`Type`, exact `Number`, temporal,
UUID/ULID, `RecordId`, geometry/vector, runtime-only identity, graph foundation,
generation-aware ECS, Arrow/DataFusion property materialization, специализированные graph
storage families, `GraphWorkspace`, three-mode `GraphRead`, Path и LiveGraph, а также внутренние
`StorageValue`/контексты, canonical codec v1, `DecodeLimits` и rkyv envelope. Следующий
исполняемый этап — интеграция query/columnar adapters; полноценные страницы, I/O и перенос
storage engine относятся к отдельной области B.

### Область A. Система типов и значения — P1 (блокирующая)

Фундамент, от которого зависит сериализация, хранилище, индексы и исполнитель.

- **`src/values/runtime`** 🟢 — `Value`, `ValueKind`, `Type`, scalars и коллекции;
  `Text(CompactString)`, `Number::Decimal(Decimal)`, согласованные `Eq`/`Ord`/`Hash`; без
  `OrdValue`, `Tuple` и `Value::Option`. Fallible public API возвращает `FerrosGrynnResult`;
  value-диагностики принадлежат `Domain::Value`, external parser errors остаются redacted
  `technical_source`.
- **`src/values/runtime/record_id.rs`** 🟢 — `RecordId`, `RecordIdPart`; object-ID хранит
  строковые ключи и имеет канонический UTF-8 порядок.
- **`src/values/runtime/property_map.rs`** 🟢 — реализованы `PropertyKeyId`, двунаправленный
  `PropertyDictionary` и однословный sorted
  `ThinVec`. Benchmark отверг boxed/tagged `SmallVec`/`RapidHashMap`: первая вставка выделяла
  656 B, а настоящий inline union занимал 672 B на каждом атоме. Empty не аллоцирует, non-empty
  имеет один contiguous allocation и canonical order без временной сортировки.
- **`src/ecs`** 🟢 — реализованы paged generation-aware `SparseSet<AtomId>` с отдельными
  node/edge slot spaces, `AnyPool`, `WorldComponents`, `(PropertyKeyId, TypeId)` property pools,
  independent presence/validity и smallest-pool read-only joins. Независимого entity allocator
  нет; revision-bound `GraphWorkspace` разделяет persistent и derived sidecars и инвалидирует
  производные pools после graph mutation.
- **`src/values/storage`** 🟢 — реализованы `StorageValue`, borrowed `StorageValueRef`,
  encode/decode contexts, полный `TypeTag` из `disk_storage_spec.md` §5.2, canonical codec v1,
  `DecodeLimits` и проверяемый rkyv archive envelope. Codec не зависит от storage engine.
- **`src/values/columnar`** 🟢/🟡 — реализованы `PropertyBatch`, presence bitmaps,
  `PropertyKeyId ↔ Arrow field`, metadata validation, native scalar arrays, dense union,
  Arrow round-trip и DataFusion `MemTable` со скрытыми presence-полями. Модули feature-gated,
  runtime от них не зависит. Nested `Binary(StorageValue)` fallback подключается на этапе
  интеграции вместе с владельцем decode-контекста batch.
- **`src/values/runtime/graph`** 🟢 — реализованы production `AtomId`, general SoA с едиными
  `source`/`target` endpoint'ами, отдельные Forest/Dag/Hyper storage families, type-state API,
  widening views и проверяемые owned conversions. `GraphRead` покрывает in-memory,
  transaction-bound disk adapter и hybrid `query_to_memory`; mutable hybrid хранит origin/
  dirty-set для explicit write-back. Добавлены `GraphWorkspace`, NodeWalk/AtomWalk traversal,
  основные алгоритмы `fg-meta`, SCC condensation, PathTrace/Path, exact graph transport и
  LiveGraph conflict protocol. PageManager-backed local adapter подключён в области B; дальнейшая
  data-layout оптимизация остаётся benchmark-driven [plan.md](values_plan.md).
- **`src/storage`** 🟢 для Windows этапа 6 — `DiskAtomRef(u64)`,
  `QualifiedAtomRef { sg_slot, local_ref: DiskAtomRef }`, Direct I/O/BufferPool, frame-WAL,
  checkpoint/recovery, durable catalogs, scalar B+tree, generic incidence, nested graph address
  spaces и lazy graph adapter находятся в production `src/storage`. Optional Windows IoRing
  подключён тем же внутренним page-source контрактом; Unix runtime-port/async остаются отдельными
  gates.

### Область B. Страничное хранилище и I/O — P2

Disk-backed режим 2. Зависит от A.

- **Sync Direct I/O + `BufferPool`** 🟡 — `AlignedPage`, безопасный LE page codec, RAII
  positional exact-read/exact-write и calibrated batch в `src/storage`; Windows использует
  `FILE_FLAG_NO_BUFFERING`, без `WRITE_THROUGH` и buffered fallback. Windows gate пройден,
  Linux/NVMe реализация/прогон и soak явно отложены до появления тестового окружения и остаются
  release-gate перед выпуском Unix-поддержки.
- **`SyncPageManager`** 🟢 (§3.3, Windows-first) — snapshot-aware concrete manager объединяет staged/WAL/home
  read path, versioned `BufferPool`, sealed `DataFileId` registry, passive checkpoint и recovery.
  Durable catalog population и Windows local composition подключены; `NativeAsync` использует
  IoRing для независимых extents и checkpoint batch, не меняя synchronous pull API. Linux async
  отложен до нативного тестового окружения.
- **Disk graph adapter** 🟢 для этапа 5 — `storage/store.rs` открывает immutable поколения как
  transaction-bound `LocalGraphView`: hot-поля читаются через `PageGuard`, primary lookup и scan
  остаются потоковыми, property heap загружается поздно, nested/transition graphs разрешаются
  рекурсивно. Материализация выполняется только перед явной мутацией или `query_to_memory`.
- **Disk-страницы** 🟢 для Windows sync среза — `NodeHotSlot`/`EdgeHotSlot`, local и cross-level overflow adjacency,
  slotted property heap/large objects, subgraph directory, freelist, label dictionary, primary
  и scalar B+tree, posting pages и graph statistics имеют checksummed codecs в `src/storage`.
- **`store.rs`** 🟢 для этапа 5 — публичные `create_local/open_local`, backend-neutral
  `GraphMutationView`, stable-layout page delta, безопасный COW fallback при смене layout и
  startup-GC старых поколений подключены и покрыты reopen-тестами.
- **`graph.rs`** 🟢 — `MetaGraph` остаётся in-memory; единственное дополнение —
  `MetaGraph::from_disk_batch(...)` для эффективной пакетной загрузки атомов (P4, отложено).

### Область C. Журнал (frame-WAL), checkpoint, recovery — P2

Зависит от B. 🔄 Полностью переопределено под frame-WAL — ранняя редакция (`WalKind 0x10–0x22`,
раздельные `MvccWal`/`DiskWal`, per-`WalKind` idempotent-redo) **отменена**.

- **frame-WAL** 🟢 — `WalKind` для frame-модели (`disk_storage_spec.md` §22.1: `TxBegin`,
  `TxCommit`, `TxAbort`, `PageFrame`, `PageInit`, `CheckpointBegin`, `CheckpointEnd`,
  `GraphCreate`, `GraphDrop`); magic `FGW1`, 33-байтовый заголовок записи с portable RapidHash V3,
  seed 0; экспериментальный 3a magic `MVCE` не мигрируется и отклоняется;
  индекс версий хранит `(frame_lsn, commit_lsn, offset)`. `PageInit` описывает только пустую
  аллокацию; первая содержательная мутация всегда пишет полный `PageFrame`.
- **Единый журнал уровня БД** 🟢 — global clocks, generation и checkpoint authority закреплены в
  checksummed `db.fgb`; WAL открывается с persisted seed и допускает законный prefix prune.
- **WAL transport** 🟢 — последовательный buffered `File`/`BufWriter`; под DB commit lock фреймы и
  `TxCommit` идут подряд, затем один `flush + sync_data`. LSN маркера становится durable
  `visible_lsn` только после barrier; Direct I/O для WAL не используется.
- **checkpoint** 🟢 — reader-safe passive target, aligned extent backfill, один sync на touched
  file, durable `CheckpointEnd`, graph superblocks и последний commit point `db.fgb`; prune только
  после него под exclusive gate. Этап 4 добавил публичные Passive/Restart, автоматическую policy и
  кооперативную отмену просроченных/Restart-снимков.
- **recovery** 🟢 — main/tmp selection, persisted clock seed, один streaming scan, replay строго
  по frame LSN через sealed registry, torn-page repair и свежий passive checkpoint до publication.

Срез 3b реализован Windows-first в `src/storage`: typed format v1, `db.lock`, atomic replace,
sealed registry, WAL/home manager, passive checkpoint/prune и seeded recovery. Unix runtime-port и
нативный Linux/NVMe crash/performance gate явно отложены; non-Windows путь компилируется как
`unsupported` и не объявляется готовым к выпуску.
- **`mvcc_persist.rs`** 🟡 — переиспользовать `WalWriter`/`WalSegmentManager` как транспорт;
  `PersistLayer::checkpoint()` расширить на flush `.fgb`.

### Область D. MVCC и транзакции (SSI) — P2

RAM-подэтап режима 1 не зависит от C: отсутствие WAL означает отсутствие durability, но не
должно блокировать корректные атомарность и изоляцию в пределах процесса. `fg-meta/mvcc.rs`
остаётся референсом сценариев, а не кодом для копирования: прототип назначает version по
`start_ts`, публикует незакоммиченные записи и имеет окно между validation и publication.

- **`Datastore` режима 1** 🟢 — единый immutable root базы, глобальные `commit_ts`, одна короткая
  commit-секция и приватный write-set. Изменения нескольких графов, таблиц и каталога публикуются
  одной заменой root; поздний commit не виден более старому snapshot.
- **COW-снимки** 🟢 — `CowVec` делит chunks колонок, allocator и adjacency; immutable snapshot не
  клонирует всю SoA. Atom-version index также chunked и различает поколения переиспользованного
  slot. `incidence` general-графа делится целиком и копируется только при структурной записи.
- **Уровни изоляции** 🟢 в Rust API — `Serializable` по умолчанию, явные `Snapshot` и
  `ReadCommitted`; один вызов `Transaction::execute` всегда удерживает один immutable root, а RC
  меняет root между вызовами. Текущий вертикальный срез считает весь переданный `Program` одной
  execution unit; refresh между несколькими top-level statements одного `Program` и синтаксис
  GrynQL `BEGIN ISOLATION LEVEL` остаются 🔴.
- **Serializable validation** 🟢 — консервативный SSI-вариант: любая исходящая
  rw-антизависимость завершает транзакцию `FG-40001`. Это корректнее прототипа и может давать
  больше abort, чем детекция только dangerous structures; переход нужен лишь по измеренной доле
  abort. Read-only snapshot не участвует в проверке и не получает serialization abort.
- **Точные предикаты** 🟢 для исполняемого среза `WHERE` — matcher использует тот же typed
  expression evaluator; commit проверяет только атомы из COW-журнала изменений после snapshot.
  Scan идёт в стабильном порядке `RecordId`. Property-изменение строки, которая не входила и не
  вошла в предикат, конфликт не создаёт.
- **Fine-grained merge** 🟢 — first-committer-wins на одном atom/graph-properties; изменения
  разных атомов одного графа сливаются поверх последнего root. После merge family заново
  валидируется, поэтому `Tree`/`RootedTree`/`Forest`/`Dag`/`Hypergraph` не могут опубликовать
  нарушающий тип результат даже при явных SI/RC. `GraphWriteView` является blind-write API:
  прикладное чтение, влияющее на решение, обязано пройти через `GraphRef::read(&tx)` до mutable
  borrow, чтобы попасть в serializable read-set; tracked family guard нужен только если этот
  контракт окажется неудобным на практике.
- **Ограничения ресурсов** 🟡 — логическая оценка write-set ограничена 256 MiB, возраст snapshot —
  300 секунд по умолчанию; структурированные статусы `FG-54F07` и `FG-25F01`. Точный
  allocation-time учёт крупных значений требует tracked write guards и остаётся 🔴; текущая
  проверка commit не является heap quota.
- **Durability режима 2** 🟢 для этапа 4 — Windows local backend сохраняет graph generations,
  переключает durable catalogs через frame-WAL и публикует новый MVCC root только после commit.
  Datastore сливает atom-level delta поверх последнего root; неизменный layout записывает только
  отличающиеся страницы, а структурный рост использует безопасный COW fallback. Чистый in-memory
  режим 1 по-прежнему не обещает durability.

### Область E. Схема и каталог — P3

- **In-memory `TableSchema`** 🟢 — имя таблицы, `GraphRef`, `TableKind`, упорядоченные typed-поля и
  validated standard/unique scalar indexes;
  `id: record` неявно добавляется query adapter, а scan режима 1 временно использует совпадающую
  часть `RecordId.table` как proxy признака принадлежности таблице; отдельный membership
  component/primary index относится к полному `src/schema/`. Имена полей заранее интернируются
  append-only методом `Datastore::intern_property`, поэтому конкурентный commit не может переназначить
  `PropertyKeyId`; schema и graph catalog коммитятся атомарно. Удалить graph, пока на него
  ссылается таблица, нельзя без удаления этой таблицы в той же транзакции.
- **Полный `src/schema/`** 🔴 — `SchemaCatalog`; `TableDefinition`, `FieldDefinition`, `IndexDefinition`,
  `EventDefinition`, `FunctionDefinition`, `AnalyzerDefinition`, `ParamDefinition`
  (`disk_storage_spec.md` §18). Хранится в системном пространстве (`graph_id = 0`).

### Область F. Статистика — P3/P4

Без статистики графовый планировщик не работает — обязательна для MVP.

- **`src/stats/`** 🔴 — `StatisticsStore`: `GraphStatsPage`, `LabelStatsPage`,
  `PropertyHistogramPage` (equi-depth), `DegreeHistogramPage`; NDV через HyperLogLog; сбор при
  загрузке и инкрементально (per-transaction delta-счётчики), выборка фиксированного размера.

### Область G. Индексы (+ bulk-сборка) — P3–P5

Зависит от A, B, E.

- **Local scalar `BTreeIndex`** 🟢 для Windows sync — standard/unique, order-preserving
  scalar/composite keys, posting pages, exact/range reads, catalog/reopen и scan fallback.
  Primary graph-wide B+tree также 🟢. **`LabelIndex`** и **`CountIndex`** остаются 🔴.
- **Bulk-сборка** 🟡 — реализованы in-memory sort, bottom-up packer и durable side-file/COW
  generation. Внешняя сортировка +
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

- **`src/syn`** 🟡 — реализован fail-fast lexer/parser (reblessive, stackless,
  `TokenBuffer<4>`) для `LET`, `RETURN`, read-only `SELECT`, выражений, массивов/объектов,
  postfix/cast и record target. AST владеет строками и `TextSpan`, но не source/runtime values.
  Известные неподдержанные statements возвращают `QueryFeatureNotSupported`; следующие семьи
  добавляются только вместе с analysis/IR/execution-потребителем.
- **Проверки** 🟢 — Unicode byte spans, greedy operators, gap-sensitive `table:id`, приоритеты,
  незакрытый delimiter с related location, лимит глубины и proptest `parser never panics`.

### Область I. Семантический анализ (binding / typing / lowering) — P2

Зависит от H, A.

- **Binder** 🟡 — реализованы slots для входных параметров/`LET` и позиционные slots колонок
  источника; unknown field/parameter завершается в `SemanticAnalysis` до запуска cursor.
- **Typechecker** 🟡 — реализован направленный вывод для первого среза и локальная унификация
  `number`, `option` и union. Общий HM-solver откладывается до closures/UDF.
- **Lowering AST → IR** 🟡 — arena `Vec<LogicalNode>` + `NodeId`; узлы `Scan`, `Filter`,
  `Project`, `Limit`, отдельные logical statements для `LET`/`RETURN`. IR хранит типы и spans,
  но не токены и lexemes.

### Область J. Оптимизатор — P3

Зависит от I, F.

- **RuleEngine** 🔴 — rule-based правила (`gql_tech_spec.md` §6.1).
- **CostEngine** 🔴 — cost-based выбор по `StatisticsStore`; штрафы и деградация плана.
- **Plan cache** 🔴; физические альтернативы; `WITH`-подсказки.

### Область K. Физическое исполнение — P3

Первый эталонный read-only путь не зависит от optimizer или незрелого storage engine; реальная
storage-интеграция зависит от B.

- **Физические операторы** 🟡 — эталонный sync pull `RowCursor::next`; реализованы
  `ScanCursor`, `FilterCursor`, `ProjectCursor`, `LimitCursor`, позиционный `Row<Vec<Value>>`.
- **Storage boundary** 🟢 для режимов 1 и 2 — crate-private `QueryRead` (`snapshot`, `catalog`,
  `scan`, `get`) подключён к `Transaction`; local adapter выполняет primary point lookup,
  потоковый scan и позднюю загрузку свойств. Наружу идут только `Value` и `RecordId`, не
  `TypeTag`/`AtomId`.
- **`QueryResult`** 🟢 для read-only среза — `Datastore::execute` и `Transaction::execute`
  выполняют `LET`/`RETURN`/`SELECT` на одном snapshot; `SELECT` сканирует по `RecordId`.
- **Batch/Arrow/async/parallel** 🔴 — только после измерений; row pull остаётся эталоном
  корректности.
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

Production `src/values/runtime/graph` и Windows `src/storage` уже сохраняют V–V/V–E/E–V/E–E,
hyper- и cross-level endpoints едиными `source/target`, включая root-wide nested slots и
qualified overflow. Таблица ниже теперь относится к оставшейся аналитической/derived-index
работе и к необязательному выравниванию референсного `fg-meta`, а не к correctness local store.

| Компонент                         | Что исправить                                                                                                       |
|-----------------------------------|---------------------------------------------------------------------------------------------------------------------|
| `graph.rs::register_adjacency()`  | Путь для edge-endpoints: регистрировать в `EdgeEndpointIndex`, а не игнорировать                                    |
| `graph.rs::EdgeFlags`             | Добавить `CROSS_LEVEL = 0b0100_0000`                                                                                |
| `graph.rs::add_cross_meta_edge()` | Ставить `CROSS_LEVEL`; убрать `__from_inner`/`__to_inner`; использовать `source_atom_ref`/`target_atom_ref` в props |
| `node.rs::NodeFlags`              | Убрать legacy-бит `PORT`; в disk format v1 значение `0x10` зарезервировано                                          |
| `edge.rs`                         | Переименовать legacy endpoint-поля в `source/target`; удалить role-enum и второй edge/edge incidence path           |
| `traversal.rs::neighbors()`       | Параметризовать: `NodeOnly` (текущий) vs `AtomWalk` (через единые endpoint'ы и `EdgeEndpointIndex`)                 |
| `traversal.rs::TarjanCuts`        | Параметризовать тип обхода; `n.is_node()` — только в `NodeOnly`                                                     |
| `matrix.rs`                       | Добавить `EndpointTensor` (§3.5)                                                                                    |
| `id.rs`                           | Добавить `QualifiedAtomRef { sg_slot: u32, local_ref: DiskAtomRef }`                                                |

### Область O. Мосты (bridges) — MVP+

- **Bridge trait** и реализации (`SurrealBridge`, …) — `gql_spec.md` §30. Публичный контракт
  `enum BridgeData` остаётся в спецификации. Реализация — после MVP.

---

## 5. Приоритизированный порядок работ (фазы)

Расширяет таблицу `disk_storage_spec.md` §30.8 работами query-слоя (`gql_tech_spec.md`).

| Приоритет            | Область | Компоненты                               | Что делать                                                          |
|----------------------|---------|------------------------------------------|---------------------------------------------------------------------|
| **P1** (блокирующий) | A       | `src/values/runtime`                     | ✅ Runtime-фундамент, `ValueKind`, scalars и collections            |
| **P1**               | A       | `src/values/storage`                     | ✅ `StorageValue`, `TypeTag`, canonical codec и decode limits       |
| **P1**               | A       | `src/values/runtime/record_id.rs`        | ✅ `RecordId`, `RecordIdPart`                                       |
| **P2**               | A       | `src/values/columnar`                    | ✅ `PropertyBatch`, Arrow/presence и DataFusion adapter             |
| **P2**               | A       | `src/values/runtime/graph`               | Graph family/type-state, Path и query return contracts              |
| **P2**               | A       | `src/ecs`                                | ✅ ECS foundation; далее revision-bound `GraphWorkspace`            |
| **P2** (высокий)     | B       | `src/storage/`                           | Sync Direct I/O → BufferPool gate → `PageManager` → vertical slice  |
| **P2**               | C       | frame-WAL                                | `WalKind` frame-модели (§22.1), индекс версий, checkpoint, recovery |
| **P2**               | H, I, K | `src/syn`, analysis, IR, sync pull       | ✅ read-only `LET`/`RETURN`/`SELECT`; далее clauses/MATCH           |
| **P3** (средний)     | E       | `src/schema/`                            | `SchemaCatalog`                                                     |
| **P3**               | G       | `src/index/`                             | `LabelIndex`, `BTreeIndex`, `CountIndex`, bulk-сборка               |
| **P3**               | J, K, L | optimizer, storage adapter, Traversal VM | `Datastore::execute` поверх утверждённого `PageManager`             |
| **P4** (нормальный)  | F       | `src/stats/`                             | `StatisticsStore`                                                   |
| **P4**               | G       | `src/index/fulltext.rs`                  | `FulltextIndex` (BM25)                                              |
| **P4**               | N       | `fg-meta` граф-ядро                      | Полная инцидентность                                                |
| **P5** (низкий)      | G       | `src/index/hnsw.rs`, `rtree.rs`          | `HnswIndex`, `RtreeIndex`                                           |
| **P5**               | M       | `src/changefeed/`                        | `ChangefeedStore`, `LiveQueryRegistry`                              |
| **P5**               | 3.5     | `matrix.rs`                              | `EndpointTensor`                                                    |
| **P2–P3**            | B, L    | `store.rs`, storage adapter              | Минимальный `open_disk_graph` и disk-backed vertical slice          |
| **MVP+**             | O       | bridges                                  | `Bridge` trait, `SurrealBridge`                                     |

**Фазы (укрупнённо):**

- **Фаза 0 — синхронизация спецификаций**: ✅ завершена; нормативные контракты values,
  graph/path/live и storage codec согласованы.
- **Фаза 1 — фундамент типов** (P1): ✅ runtime и `StorageValue`/codec части Области A.
- **Фаза 1.5 — свойства и ECS sidecar** (P2): ✅ PropertyMap benchmarks/dictionary,
  generation-aware SparseSet и PropertyColumn; `GraphWorkspace` продолжается в этапе 4.
- **Фаза 2a — sync Direct I/O** (P2): ✅ aligned page codec, RAII positional I/O, native проверки
  и calibrated batch reads/writes.
- **Фаза 2b — ранний I/O-gate** (P2): 🟡 read-only Clock-Pro + benchmark-only mmap; Windows gate
  пройден, Linux/NVMe реализация/прогон и soak отложены до доступного окружения и остаются
  release-gate перед Unix/format-v1 выпуском, но не блокируют Windows frame-WAL срез.
- **Фаза 3a — in-memory MVCC** (P2): ✅ RAM-часть Области D, COW-снимки, atom-level merge,
  serializable validation и точные предикаты.
- **Фаза 3b — журнал и восстановление** (P2): ✅ Windows-first Область C; Unix release-gate
  остаётся отложенной границей.
- **Фаза 4a — local disk vertical slice** (P2–P3): ✅ Windows public create/open, durable
  catalogs, concrete graph pages, lazy page-backed adapter, point mutations, page delta,
  checkpoint policy/Restart и read/write/reopen E2E.
- **Фаза 4b — query-конвейер MVP** (P2–P3): 🟡 H → I → J/K/L; read-only
  `Datastore::execute` подключён для RAM и materialized local root, optimizer/traversal остаются.
- **Фаза 5a — Windows sync storage completion** (P3–P4): ✅ scalar B+tree, generic incidence,
  nested graphs, recursive `query_to_memory`, atomicity/FCW/SSI/family E2E. Correctness gate
  пройден, но повторный Windows performance gate этапа 5 не прошёл sparse/scan throughput;
  format v1 остаётся candidate также до устранения этой регрессии, Linux/NVMe и soak release-gate.
- **Фаза 5b — полная схема/статистика/optimizer indexes** (P3–P4): E, F, оставшаяся G
  (Label/Count, external bulk и cost-based planner).
- **Фаза 6 — продвинутые индексы и Live** (P4–P5): Fulltext, HNSW, R-tree,
  derived incidence indexes и M (changefeed/live).
- **Фаза 7 — optional Native Async I/O и мосты**: ✅ Windows IoRing через единый внутренний
  page-source контракт; GrynQL pull API не меняется. Linux/tokio-uring отложен до нативного
  окружения. Unlimited transaction spill проектируется отдельно после crash/performance tests.

---

## 6. Отложено / вне MVP

Сводно из `gql_tech_spec.md` §18, `gql_spec.md` §32.3 и заметок `disk_storage_spec.md`.

| Область                                                        | Решение                                                                              |
|----------------------------------------------------------------|--------------------------------------------------------------------------------------|
| JIT-компиляция (LLVM: expression/operator-fusion/pipeline JIT) | Не входит в контракт языка; implementation detail; после стабилизации интерпретатора |
| Worst-Case Optimal Join (Leapfrog TrieJoin)                    | Базовый Index NL достаточен для MVP; WCOJ — для паттернов с циклами позже            |
| Advanced disk indexes/pages сверх минимального режима 2        | После disk-backed vertical slice и полного sync-gate                                 |
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
