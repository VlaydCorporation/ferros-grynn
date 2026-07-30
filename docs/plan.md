# План реализации `values`

## 1. Архитектура значений

### Границы модулей

- `values::runtime` — публичная система типов Rust SDK.
- `values::storage` — внутренние дисковые DTO, преобразования, теги и codec.
- `values::columnar` — адаптеры пакетной материализации в Apache Arrow/DataFusion.
- `src/storage` — перенесённый в основную кодовую базу `crates/storage_engine`: страницы, I/O, buffer pool, WAL и recovery.
- `runtime` не зависит от Arrow, DataFusion или storage engine.

### `Value` и типы

- Использовать `CompactString` напрямую для строк, имён таблиц, меток и публичных ключей. Собственные `StringValue` и SSO не реализовывать. `compact_str` хранит короткие строки inline и поддерживает `rkyv`. [Документация](https://docs.rs/compact_str/latest/compact_str/).
- Публичные типы именовать `Array`, `Bytes`, `Set`, `Object`, `Range`, `Duration`, `DateTime`, `Uuid`, `Ulid`, `RecordId`, `Geometry`, `Regex`, `Path`, `Graph`, без суффикса `Value`.
- Хранить `rust_decimal::Decimal` непосредственно внутри `Number::Decimal`.
- `Uuid`/`Ulid` сделать newtype обёртками с `as_inner`, `into_inner`, `From`/`Into`, parsing и Ferros-Grynn API.
- `option<T>` оставить только статическим `Type`: `Some(v)` представлен самим `v`, отсутствие — `Value::None`. Вариант `Value::Option` не вводить.
- Все варианты `Value` реализуют согласованные `Eq`, `Ord` и `Hash`; `OrdValue` удалить.
- Mutable-графы не помещать в `Value`: запросы возвращают только immutable `Graph<Frozen>`, `Tree<Frozen>`, `Path` и другие frozen-типы. Для ручной мутации пользователь явно вызывает `thaw`.
- Ввести три независимых описания типа:
    - `ValueKind` — конкретный runtime-вариант (`Int`, `Float`, `Decimal`, `String`, …);
    - `Type` — тип GQL со структурой `array<T>`, `set<T>`, `option<T>`, `range<T>`, `record<T>`, union и сигнатурами closure;
    - `TypeTag` — стабильный внутренний байт дискового формата.
- Расширять `#[non_exhaustive] Value` поэтапно, не добавляя ссылки на ещё не реализованные типы:
    - базовые: `None`, `Null`, `Bool`, `Number`, `String`, `Bytes`;
    - хранимые: `DateTime`, `Duration`, `Uuid`, `Ulid`, `RecordId`, `Array`, `Set`, `Object`, `Range`, `Geometry`, `Vector`, atom/graph references;
    - runtime-only: `Closure`, `Regex`, `Matcher`, `Computed`, `Formatter`, `Cursor`, `Error`;
    - графовые результаты: атом, путь и граф/подграф с различением reference/materialized состояния внутри соответствующих типов.
- Все сужающие и потенциально неточные преобразования сделать `TryFrom`; удалить преобразования, молча возвращающие `0`/default.
- `CompactString`, `Bytes`, `Array` и `Object` предоставляют `as_str`/`as_ref`, итераторы, `len`, `is_empty`, consuming-конверсии и контролируемые методы изменения. Не использовать `DerefMut`, если он обходит инварианты.
- `Set` хранить как `BTreeSet<Value>`, `Range` валидировать на совместимость границ, а `RecordId` разделить на публичные строковые `table:id` и внутренние идентификаторы словаря.

### Sentinel-семантика и порядок

`none` и `null` — обычные разные sentinel:

```text
none = none   → true
null = null   → true
none = null   → false
null = 1      → false
null != 1     → true
bool(none)    → false
bool(null)    → false
```

`IS NULL` и `IS NONE` остаются удобными проверками, но не имеют отдельной трёхзначной семантики. `AND`, `OR`, `NOT`, сравнения и фильтры всегда возвращают `bool`.

Стабильный порядок вариантов:

```text
none < null < bool < number < string < duration < datetime
     < ulid < uuid < array < set < object < geometry < bytes
     < table < record < regex < range
     < atom < path < subgraph < graph < closure
```

- Ранг задаётся явно через `ValueKind`, а не порядком вариантов enum.
- `Number` сравнивается точно между `Int`, `Float` и `Decimal`.
- `-0.0` равен `0.0`; все NaN сравниваются через каноническое представление и равны друг другу для `Eq`/`Ord`/`Hash`. B+tree по-прежнему отклоняет NaN.
- `Array`, `Set`, `Object`, `Range` и `Path` сравниваются лексикографически.
- Графы сравниваются по identity/revision, а не по содержимому или изоморфизму. Изоморфизм проверяется отдельной функцией.
- Runtime-only объекты вроде closure сравниваются по стабильному в рамках процесса `RuntimeId` и не преобразуются в `StorageValue`.

### Коллекции и свойства

- `Array` хранит `Vec<Value>`.
- `Set` хранит `BTreeSet<Value>`.
- Публичный `Object` хранит `BTreeMap<CompactString, Value>` ради канонического порядка, стабильного вывода и дешёвого `Ord`.
- `PropertyMap` не является alias для `Object`. Внутри графа ключи представлены `PropertyKeyId(u32)` и разрешаются через `PropertyDictionary`.
- Начальное представление `PropertyMap`:

```text
до 8 свойств   → sorted SmallVec<[(PropertyKeyId, Value); 8]>
больше 8       → RapidHashMap<PropertyKeyId, Value>
```

- Порядок свойств семантически незначим. При дисковом кодировании heap-вариант сортируется по `PropertyKeyId`.
- Benchmark может изменить константу `INLINE_PROPERTIES`, но не публичный API или disk format.

### Column-store

`PropHeapPage` остаётся row-oriented OLTP-хранилищем. `PropertyColumnCachePage` и Arrow используются для аналитической материализации.

`Arrow::MapArray` не заменяет `PropertyMap`: это колонка однотипных map-значений, физически являющаяся list из key/value struct. [MapArray](https://docs.rs/arrow/latest/arrow/array/struct.MapArray.html).

`PropertyBatch` содержит:

```text
RecordBatch
presence bitmap для каждой property-column
mapping PropertyKeyId → Arrow field
```

Состояния:

```text
present=0             → none
present=1, valid=0    → null
present=1, valid=1    → значение
```

- Schemafull-свойства материализуются в нативные Arrow arrays.
- Schemaless-колонка получает вычисленный общий тип; несовместимые значения кодируются как dense union либо `Binary(StorageValue)` по явно зафиксированному fallback.
- DataFusion подключается только к batch-слою. Его `ScalarValue` не используется внутри `Value`: nested scalar фактически хранится как одноэлементный Arrow array, а документация рекомендует пакетные arrays для производительности. [ScalarValue](https://docs.rs/datafusion/latest/datafusion/common/scalar/enum.ScalarValue.html).


## 2. Графовый домен и пути

### Режимы хранения

```text
Mode 1 — Full in-memory
Graph<Mutable> / специализированный T<Mutable>
MVCC + WAL + snapshot durability

Mode 2 — Disk-backed
GraphRef + DiskGraphView<'tx>
PageManager + BufferPool + SnapshotId

Mode 3 — Hybrid
query_to_memory<T, State>()
State ::= Mutable | Frozen
дисковый подграф материализуется в T<State>
исходный disk snapshot освобождается после загрузки

Отдельные:
LiveGraph<Frozen> для immutable read-optimized графа с live-обновлением
LiveGraph<Mutable> полученный явно через конвертацию и потенциально небезопасный из-за коллизий dirty-set с явным fallback для live-запроса
```

`GraphSnapshot` как самостоятельный возвращаемый объект не вводить.

- `SnapshotId` — идентификатор MVCC-версии.
- `GraphReadView<'tx>` — transaction-bound read guard.
- В Mode 1 view ссылается на MVCC version root in-memory графа.
- В Mode 2 view ссылается на `GraphRef`, `PageManager` и `SnapshotId`; это не срез памяти и не копия графа.
- View удерживает историю только в пределах транзакции/guard.
- `RETURN GRAPH` и `query_to_memory` не возвращают snapshot-bound значения.

### Type-state API

```rust
pub struct Mutable;
pub struct Frozen;

pub struct Graph<State = Frozen>;
pub struct Tree<State = Frozen>;
pub struct RootedTree<State = Frozen>;
pub struct Forest<State = Frozen>;
pub struct Dag<State = Frozen>;
pub struct Hypergraph<State = Frozen>;
pub struct Metagraph<State = Frozen>;
```

Предоставить aliases `MutableTree`, `FrozenTree` и аналогичные.

- `GraphRead` реализован всеми состояниями и backend-режимами.
- `GraphMut` реализован только для `Mutable`.
- `freeze(self)` строит read-optimized представление.
- `thaw(self)` строит mutable arena-представление.
- Обе конверсии явно документируются как потенциально `O(V+E)`.
- Query results и обычный `LIVE SELECT` используют `Frozen`. При ручной конвертации в `Mutable` - явный fallback `LIVE SELECT` при конфликте dirty-set.
- Manual SDK и write-back `query_to_memory` могут сразу запрашивать `Mutable`.

### Storage families

Не использовать один универсальный backing для всех узких типов. Разделить storage по структуре:

1. `GeneralStorage`
    - Универсальный multi-hyper-meta graph.
    - Отдельные SoA-пулы атомов, endpoint- и participation-incidence, вложенные графы.
    - Основа `Graph` и `Metagraph`; `Metagraph` добавляет invariant/API, но первоначально разделяет физический layout с `Graph`.

2. `ForestStorage`
    - Общая основа `RootedTree`, `Tree`, `Forest`.
    - Parent, children, roots/components, edge pool, optional depth/LCA cache.
    - Tree/Forest не допускают self-loop, parallel edges, hyper/meta/cross-level incidence.
    - `TreeOrientation`: `Undirected`, `OutArborescence`, `InArborescence`.

3. `DagStorage`
    - Только vertices и обычные directed edges.
    - Outgoing/incoming adjacency и topological order/rank.
    - Self-loop запрещён, parallel edges разрешены, если они не создают цикл.
    - Mutable-вариант поддерживает topological invariant при каждой мутации.
    - Frozen-вариант использует packed adjacency/CSR-подобные offsets.

4. `HyperStorage`
    - Vertices, hyperedges и incidence-oriented layout.
    - Meta-атомы и cross-level incidence не допускаются: для них используется `GeneralStorage`.

Специализация оправдана различием layout: arena-based tree может хранить parent/children непосредственно, тогда как DAG и general graph требуют иных adjacency structures.
Существующие Rust-библиотеки также разделяют adjacency-list, stable-arena и CSR по различным компромиссам. [petgraph graph types](https://docs.rs/petgraph/latest/petgraph/), [arena tree](https://docs.rs/indextree/latest/indextree/).

### Преобразования типов

Дешёвые read-only views без перестроения:

```text
RootedTree → Tree → Forest → Graph
RootedTree/Tree/Forest directed → DagView → GraphView
Dag → GraphView
Hypergraph → GraphView
Metagraph → GraphView
```

Owned-преобразования:

- `RootedTree → Tree → Forest` — `O(1)`, общий `ForestStorage`.
- Directed `Tree/Forest → Dag` — `O(V+E)`, строится `DagStorage`.
- Любой специализированный тип → owned `Graph` — `O(V+E)`.
- `Graph/Dag/Forest → Tree`, `Graph → Dag/Hypergraph/Metagraph` — только `TryFrom`, с полной валидацией и перестроением.
- Undirected `Tree → Dag` требует явных root и orientation; обычный `TryFrom` не реализуется.
- `Dag → Tree` разрешён, если есть один root, каждая прочая вершина имеет indegree=1, граф связен и `|E|=|V|-1`.
- GQL не выполняет неявных преобразований. Constraint у `RETURN GRAPH <type>` валидирует результат и строит точное целевое storage family.

### Контракты операторов

| Операция                             | Результат                                                         |
|--------------------------------------|-------------------------------------------------------------------|
| `db.graph(name)`                     | `GraphRef` с `GraphKind` и storage mode                           |
| `graph_ref.read(&tx)`                | `GraphReadView<'tx>`; snapshot-bound                              |
| `MATCH ... RETURN atom`              | стабильный `AtomRef`/`RecordId`, не внутренний `AtomId`           |
| `MATCH path = ... RETURN path`       | self-contained structural `Path`                                  |
| `RETURN GRAPH { ... }`               | self-contained `Subgraph<Frozen>`                                 |
| `RETURN GRAPH <tree> { ... }`        | `Tree<Frozen>`; аналогично остальным constraints                  |
| `BUILD GRAPH <type> name ...`        | после commit — `GraphRef` с зафиксированным `GraphKind`           |
| анонимный `BUILD GRAPH <type>`       | session-owned `T<Frozen>`                                         |
| `graph::inner(meta)` внутри запроса  | transaction-bound `GraphReadView`/`SubgraphView`                  |
| возврат `graph::inner(meta)` клиенту | материализованный `Subgraph<Frozen>`                              |
| `query_to_memory::<T, Frozen>`       | read-optimized hybrid graph без dirty/flush API                   |
| `query_to_memory::<T, Mutable>`      | mutable hybrid graph с source mapping, dirty-set и explicit flush |

Backend-dispatch остаётся внутренним. Пользователь работает через конкретный return type и общие traits. Публичный `GraphHandle`, требующий угадывать ветку `match`, не используется как результат операторов с фиксированным контрактом. Если транспортный `Value` требует sum-type для графовых значений, его discriminant однозначно выводится из оператора и доступен через `kind()` и типизированные `TryFrom`.

`RETURN GRAPH` полностью материализует выбранную топологию и значения до возврата и сразу освобождает source snapshot. Полученный frozen-граф независим от дальнейших изменений БД.

### Path

Во время исполнения используется внутренний `PathTraceArena`:

```text
PathTraceNode {
    previous: Option<PathTraceId>,
    atom: AtomRef,
    via_edge: Option<AtomRef>,
    direction/role,
    cumulative_cost
}
```

- Добавление шага — `O(1)`.
- Prefixes разделяются между traverser-ами.
- Planner включает tracking с начала обхода, если path именован, возвращается, используется `tv::path()` или path-функцией.
- Если path нигде не используется, trace не создаётся.
- Barrier не обязан материализовывать path; materialization выполняется на `RETURN`, `collect` или при операторе, требующем сравнения полного пути.

Публичный `Path` содержит:

```text
origin graph identity
query SnapshotId
ordered Arc<[PathStep]>
length
optional cumulative cost
```

`PathStep` хранит стабильные atom/edge refs и направление/роль. Внутренний `AtomId` наружу не попадает.

- `nodes`, `edges`, `atoms`, `length`, `cost` работают без доступа к графу.
- Свойства разрешаются явно через `Path::resolve(&impl GraphRead)`.
- Resolver обязан соответствовать origin и snapshot; переход к latest-version молча запрещён.
- Если историческая версия недоступна, возвращается `SnapshotUnavailable`.
- Для сохранения path после окончания snapshot пользователь материализует нужные свойства или выполняет path над `query_to_memory`.
- Path immutable; изменение исходного графа не переписывает уже возвращённый Path.

### LIVE-графы

Статический `Graph<Frozen>` никогда не меняется. Автообновление выражается отдельным типом:

```rust
pub struct LiveGraph<State = Frozen>;
```

`LiveGraph<Frozen>`:

- хранит атомарно заменяемый `Arc` на текущую frozen-версию;
- changefeed delta применяется к copy-on-write chunks;
- после валидации новая версия публикуется одним atomic swap;
- `read()` возвращает guard конкретной revision, поэтому один traversal всегда видит согласованную версию;
- старые guards продолжают читать прежнюю immutable-версию;
- пользователь может получить отдельный static snapshot текущей версии.

`LiveGraph<Mutable>`:

- поддерживает локальный dirty-set и очередь входящих changefeed events;
- непересекающиеся события применяются автоматически;
- при пересечении с локальными изменениями подписка приостанавливается;
- конфликтующее обновление не применяется и не коммитится автоматически;
- event stream возвращает отдельную ветку:

```rust
LiveEvent::Conflict {
    incoming_changes,
    local_changes,
    conflicts,
    remote_revision,
}
```

- API совместим с `tokio::select!`;
- пользователь явно выбирает `accept_remote`, `keep_local`, `discard_local`, `rebase`, `flush` или `resume`;
- после разрешения применяется выбранный набор изменений и повторно проверяются graph invariants;
- документация предупреждает, что одновременная локальная и query-based мутация одного live-графа требует ручного conflict resolution.


## 3. Дисковое представление и storage engine

### `StorageValue`

- `StorageValue` содержит только persistable data: dictionary IDs, `DiskAtomRef`, decimal coefficient+scale и нормализованные коллекции.
- Runtime-only, mutable graph, live graph и transaction-bound view отклоняются через `TryFrom<Value>` с доменным статусом.
- Добавить borrowed `StorageValueRef<'a>` для чтения без лишних копий.
- Преобразования получают encode/decode context для `key_id`, `table_id`, `tz_id`, graph refs и snapshot refs.

### Кодеки

Зафиксировать гибрид:

- Канонический формат `StorageValue` в `PropHeapPage` и WAL payload реализуется явно по `disk_storage_spec`: `TypeTag`, canonical LEB128, little-endian, length framing и decode limits. B+tree использует отдельный order-preserving codec.
- Фиксированные page headers/slots реализуются через `zerocopy` и endian-aware поля.
- `rkyv 0.8 + bytecheck` используется для Mode 1 graph snapshots, rebuildable caches и временных query archives, но не как вечный canonical storage format.
- Каждый rkyv archive получает envelope `magic + format_version + schema_id + length + checksum`.
- `bincode 2` и `binrw` остаются benchmark/reference tooling, не production format.
- Учитывать, что rkyv сохраняет совместимость только при неизменных schema и format features, а major-форматы могут быть несовместимы. [rkyv compatibility](https://docs.rs/rkyv/latest/rkyv/).

Нужны бенчмарки бинарных сериализаторов, анализ производительности кодеков, схем.
Желательно zerocopy десериализация, чтобы не терять преимущества от Direct I/O режима работы с файлами.

### Перенос `storage_engine`

- Перенести `crates/storage_engine` в `src/storage`.
- Использовать рабочие Direct I/O, BufferPool, graph store и persistence-механизмы из `fg-meta` как референс.
- Удалить дублирующий `PropertyValue`; storage engine принимает encoded `StorageValue`.
- Порядок переноса: I/O → buffer pool → page layouts → PageManager → store → WAL/checkpoint/recovery.
- Удалить отдельный crate только после parity-тестов.


## 4. Этапы реализации

1. **Синхронизация спецификаций — ✅ выполнено**
    - Обновить sentinel-логику и удалить все остатки `UNKNOWN`.
    - Зафиксировать operator return matrix, snapshot lifetime, graph conversions, path и live semantics.
    - Сохранить `PropHeapPage` row-oriented и Arrow materialization отдельным слоем.
    - Исправить текущие противоречия `TypeTag`, `DiskAtomRef`, option и старые `Value`-имена. Зафиксировать `disk_storage_spec.md §5.2` как источник `TypeTag`; исправить конфликт тега `option`, ширину `DiskAtomRef` на `u64`, противоречие `float → decimal`, обработку object-ID и устаревший `Tuple`.
    - Для неизвестного тега в codec v1 возвращать структурированную ошибку: сохранить неизвестное значение как `Bytes` невозможно без внешнего length framing.
    - Синхронизированы `gql_spec.md`, `gql_tech_spec.md`, `disk_storage_spec.md`, `graphs.md`, `implementation_plan.md` и этот план.

2. **Runtime-фундамент**
    - Исправить `Array::new`, `Number::Hash` и некорректный `Value::Ord`.
    - Реализовать scalars, безопасные конверсии, `Display`, parsing и introspection, `CompactString`, `Number`, temporal, UUID/ULID, `RecordId`.
    - Добавить `DateTime`/`Duration` поверх `jiff`, UUID/ULID как непрозрачные 128-битные значения, GeoJSON-совместимую геометрию и типизированные vector-варианты.
    - Реализовать полный `ValueKind`, `Eq`/`Ord`/`Hash`.
    - Реализовать `Array`, `Set`, `Object`, `Range`.
    - Реализовать `RecordId`, включая int/string/uuid/ulid/array/object части и рекурсивную проверку допустимости.
    - Затем перенести графовые типы из `fg-meta`, адаптировать их `PropertyMap` и реэкспортировать как корневой `graph` API.
    - Runtime-only типы добавить последними; попытка сохранить их должна возвращать диагностическую ошибку, а не panic.
    - После каждого варианта сохранять успешный `cargo check`.

3. **PropertyMap и columnar**
    - Реализовать dictionary и hybrid small/hash representation.
    - Реализовать `PropertyBatch`, presence bitmaps и Arrow conversion.
    - Подключить DataFusion только после проверки Arrow round-trip и `none`/`null`.

4. **Графовое ядро**
    - Перенести универсальное поведение `fg-meta`.
    - Ввести type-state API и storage families.
    - Реализовать mutable/frozen conversions и conversion matrix.
    - Реализовать три storage modes и `query_to_memory`.
    - Затем добавить PathTrace/Path и LiveGraph.

5. **StorageValue и storage engine**
    - Ввести внутренний `StorageValue`, содержащий дисковые формы: `key_id`, `table_id`, `tz_id`, `DiskAtomRef`, decimal `i128+scale` и нормализованные структуры коллекций.
    - Преобразование разделить на:
        - `Value → StorageValue` с mutable encode-контекстом интернирования;
        - `StorageValue → Value` с decode-контекстом разрешения строк, таблиц, часовых поясов и ссылок.
    - Преобразования должны потреблять `StorageValue`, чтобы переносить объекты кучи без повторных аллокаций, где представления совпадают.
    - Реализовать ручной versioned codec для формата из `disk_storage_spec.md`: canonical LEB128, little-endian payload, UTF-8 validation, отсортированные set и object по `key_id`, проверка duplicate keys и всех размерностей.
    - Реализовать canonical codec, decode limits и rkyv archive envelope.
    - Добавить `DecodeLimits` для глубины, длины коллекций, строк и общего allocation budget; повреждённые данные никогда не должны приводить к panic или неконтролируемому выделению памяти.
    - Отделить обычный value codec от order-preserving кодирования B+tree. Для object-части `RecordId` канонический порядок задаётся лексикографически по UTF-8 ключу; полное индексирование произвольного object остаётся запрещённым.
    - Перенести `storage_engine` в основную кодовую базу, удалить его дублирующий `PropertyValue`, передавать между слоями encoded bytes.
    - Ошибки codec/conversion оформить новыми статусами через `define_status_codes!`.
    - Соединить Mode 1, Mode 2 и Mode 3 через общий `GraphRead`.

6. **Интеграция**
    - Подключить graph/query return matrix.
    - Реализовать BUILD/RETURN GRAPH, path results, live conflict stream.
    - Зафиксировать disk format v1 только после fuzzing и cross-mode тестов.


## 5. Тесты, бенчмарки и критерии готовности

### Тесты значений

- Законы `Eq`/`Ord`/`Hash` для всех вариантов.
- Cross-number comparisons, NaN, infinities, `-0`, decimal boundaries.
- Sentinel truth tables.
- Вложенные коллекции, duplicate keys/elements, canonical order.
- Runtime ↔ StorageValue ↔ bytes round-trip.
- Повреждённые теги, LEB128, UTF-8, depth/allocation limits.
- Fuzzing codec и recursive values.

### Графовые тесты

- Каждая mutable-операция сохраняет заявленный invariant.
- Freeze/thaw и все допустимые конверсии сохраняют topology, IDs и свойства.
- Недопустимые преобразования возвращают структурированную ошибку.
- Directed/undirected Tree → Dag policies.
- Mode 1/2/3 возвращают одинаковые результаты `GraphRead`.
- `RETURN GRAPH` не удерживает disk snapshot.
- `query_to_memory` освобождает snapshot после загрузки.
- Path tracking включается только при статически обнаруженной необходимости.
- Path materialization сохраняет порядок, роли и стоимость.
- Live frozen guards видят одну revision.
- Mutable live conflict не применяет remote delta до решения пользователя.
- После conflict resolution повторно проверяются Tree/Dag invariants.

### Проверка и критерии готовности

- Unit-тесты всех конструкторов, fallible-конверсий, `none`/`null`, числовых границ, decimal normalization, temporal timezone/duration и RecordId.
- Property-based тесты законов: рефлексивность, транзитивность, согласованность `Eq`/`Hash`/`Ord`, одинаковый результат для объектов с разным порядком вставки.
- Round-trip тест каждого `TypeTag`, глубоко вложенных коллекций, Unicode/NUL в строках, пустых и максимальных значений.
- Negative-тесты: неизвестный/обрезанный тег, non-canonical LEB128, invalid UTF-8, переполнение decimal, неизвестный `key_id`/`tz_id`, duplicate object keys, неотсортированный set, превышение лимитов.
- Проверить, что runtime-only значения стабильно отклоняются storage-конвертером.

### Бенчмарки

Строковый benchmark не добавлять.

Сравнить:

- `Set`: `BTreeSet`, sorted `Vec` (`SmallVec`), `RapidHashSet`;
- `Object`: `BTreeMap`, `RapidHashMap`, `IndexMap`;
- `PropertyMap`: sorted `SmallVec`, `RapidHashMap`, hybrid;
- mutable/frozen representations каждой storage family;
- traversal general Graph против Tree/Forest/Dag;
- freeze/thaw и cross-family conversion;
- disk-backed traversal против `query_to_memory`;
- PathTrace arena против копирования полного path;
- live COW delta и atomic publish;
- canonical codec, rkyv, bincode и binrw на проектных данных.

Размеры PropertyMap: `0, 1, 4, 8, 16, 32, 128`. Измерять latency, throughput, bytes/atom и число аллокаций.

### Готовность

- Все публичные graph/query operators имеют однозначный return type.
- Mutable и Frozen разделены на уровне типов.
- Snapshot-bound views не выходят за lifetime транзакции.
- Graph/Path results не зависят от живого disk snapshot без явного resolver.
- Все модули покрыты unit, property-based и integration tests.
- Нет `todo!`, `unimplemented!`, пустых trait-реализаций, silent-default conversions и panic на повреждённых данных.
- Проходят `cargo +nightly fmt --check`, `cargo check`, `cargo clippy`, `cargo test` и отдельные benchmark smoke tests.

### Принятые допущения

- Обратная совместимость с удалённой реализацией `AnyValue` и старыми snapshot не требуется; codec начинается с версии 1.
- Строки и значения считаются преимущественно неизменяемыми после построения; hash memoization откладывается до появления подтверждённой необходимости и должен быть `Sync`.


В конце реализации добавить файл `AGENTS.md` для модуля `values` и сослаться на него в корневом `AGENTS.md`.
Обновлять соответствующие разделы [implementation_plan.md](implementation_plan.md) и отмечать выполненные этапы.
