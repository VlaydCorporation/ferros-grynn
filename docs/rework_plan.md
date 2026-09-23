# Этап 8.1. Возврат к graph-first модели атомов

## 1. Архитектурное решение

SQL-like таблицы как самостоятельные хранилища не нужны. Их полезные обязанности разделяются:

```text
Database
├── ClassCatalog
│   └── CLASS: namespace RecordId + NODE/EDGE kind + schema + base indexes
├── AtomStore
│   └── RecordId → global atom kind + base properties
└── GraphCatalog
    └── Graph
        ├── memberships: global atom → graph-local AtomId
        ├── optional labels
        ├── edge topology / weight / nested-graph bindings
        └── sparse property overlays
```

Основные правила:

- Каждый `RecordId = class:id` идентифицирует ровно один глобальный атом БД.
- `CLASS` фиксирует `NODE` или `EDGE`; `ANY` удаляется.
- Атом может не принадлежать ни одному графу, например быть property-record.
- Один атом может входить в несколько графов без копирования base properties.
- Один глобальный edge может иметь независимые topology, label, weight и transition graph в каждом графе.
- Инстанс — не новый атом и не новый ID. Это graph membership плюс sparse overlay.
- Optional label остаётся graph-local категоризацией и не меняет class.
- Граф — отдельный каталожный объект. `GraphRef` больше не является фиктивным `graph:<name>` RecordId.
- Каноническое хранение свойств остаётся row-oriented. ECS/Arrow/columnar layouts являются только производными аналитическими представлениями.

Эффективное значение поля:

```text
overlay value  → вернуть override
Hidden         → поля нет
overlay absent → вернуть base value
```

`Hidden` является внутренним tombstone и отличается от пользовательских `NONE` и `NULL`. `RESET field` удаляет override и снова включает наследование base value.

### Осознанные слабые места

| Риск                                                               | Решение                                                                                               |
|--------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------|
| Base update влияет на все наследующие graph views                  | Database-wide reverse membership index; обход только memberships данного атома                        |
| Большое число graph memberships делает base write дорогим          | Полный preflight до мутации и существующий write-set limit; никакого скрытого частичного commit       |
| Effective indexes размножают derived data                          | Они создаются только явным `SCOPE EFFECTIVE IN GRAPH`; хранится полный индекс только выбранного графа |
| Graph read получает дополнительную индирекцию к base atom          | Graph slot хранит внутренний `GlobalAtomId`; бит наличия overlay исключает лишнее чтение и allocation |
| Массовый `DELETE CASCADE` может затронуть много графов             | `RESTRICT` по умолчанию; explicit `CASCADE` проходит bounded preflight                                |
| Schema change CLASS влияет на все графы                            | Kind и имя immutable; изменение fields проверяет base и затронутые effective views                    |
| Один class не выражает разные роли атома в графах                  | Для этого сохраняется отдельный optional graph-local label                                            |
| Property-record не обязан быть участником графа                    | Ссылка не создаёт membership; в graph scope member читается как effective, non-member — как base      |
| Один edge имеет разные topology в разных графах                    | Это явно считается разными graph occurrences одной общей relation entity                              |
| Columnar storage ускорило бы аналитику, но ухудшило point mutation | Columnar cache строится позднее как revision-bound derived data                                       |
| RecordId не глобален между БД                                      | Он остаётся database-local; cross-database qualification принадлежит этапу Bridges                    |

## 2. Нормативные контракты и API

### 2.1. Типы

Ввести или скорректировать:

```rust
pub struct ClassId(SchemaId);

pub struct RecordId {
    class: Text,
    id: RecordIdPart,
}

pub struct GraphId(NonZeroU64);

pub struct GraphRef {
    id: GraphId,
    name: Text,
    kind: GraphKind,
    storage_mode: GraphStorageMode,
}

pub enum IndexScope {
    Base,
    Effective { graph: GraphId },
}
```

- `RecordId::table*` заменяются на `class()` и `class_text()`.
- `TableSchema`, `TableField`, `TableIndex`, `TableSchemaMode` заменяются на `ClassSchema`, `ClassField`, `ClassIndex`, `ClassSchemaMode`.
- Class kind использует существующий `AtomKind::{Node, Edge}`.
- `ClassId`, как и `IndexId`, получает стабильную schema identity.
- Имя CLASS immutable: переименование не поддерживается.
- `REMOVE CLASS` разрешён только при отсутствии атомов и зависимостей.
- `GraphRef` идентифицируется через `GraphId`; имя является каталожным отображением.
- Добавляются `Value::GraphRef`, `ValueKind::GraphRef` и `Type::GraphRef(GraphKind)`, отдельно от owning `Value::Graph`.
- `AtomRef { kind, RecordId }` сохраняется.
- Внутренний `GlobalAtomId` отделяется типом от graph-local `AtomId`.
- Внутренний `QueryStorageRow` заменяется на atom row, которому graph-local `AtomId` требуется только для graph-scoped источника.
- Compatibility aliases для `TABLE` не сохраняются.

### 2.2. GrynQL

DDL:

```gql
DEFINE CLASS person TYPE NODE SCHEMAFULL;
DEFINE CLASS friendship TYPE EDGE SCHEMALESS;

DEFINE FIELD email ON CLASS person TYPE string;

DEFINE INDEX by_email ON CLASS person
  FIELDS email UNIQUE SCOPE BASE;

DEFINE INDEX by_email_social ON CLASS person
  FIELDS email UNIQUE SCOPE EFFECTIVE IN GRAPH social;

INFO FOR CLASS person;
ALTER CLASS person ...;
REMOVE CLASS person;
```

- `TYPE NODE|EDGE` обязателен.
- Каждый пользовательский Standard/Unique/Count index обязан явно указывать `SCOPE`.
- Primary RecordId index один на AtomStore и пользователем не объявляется.
- Graph class-membership, label и adjacency indexes остаются engine-owned.

DML:

```gql
CREATE person:alice SET name = "Alice";

ATTACH person:alice TO GRAPH social LABEL Employee;

UPDATE person:alice SET name = "Alice Smith";
UPDATE person:alice IN GRAPH social
  LABEL Moderator
  SET nickname = "Ali";

UPDATE person:alice IN GRAPH social LABEL NONE;
UPDATE person:alice IN GRAPH social UNSET private_note;
UPDATE person:alice IN GRAPH social RESET private_note;

DETACH person:alice FROM GRAPH social;
DETACH person:alice FROM GRAPH social CASCADE;

DELETE person:alice;
DELETE person:alice CASCADE;
```

- `CREATE`, unscoped `UPDATE`, `UPSERT` и `DELETE` работают с base atom.
- `UPDATE ... IN GRAPH` требует существующий membership и изменяет только overlay/label.
- Graph-scoped `UPSERT` и `DELETE` не вводятся: используются явные `ATTACH`/`DETACH`.
- `ATTACH` создаёт node membership; edge occurrence создаётся через `RELATE`.
- Обычный `DETACH` запрещён при graph-local incidence. `CASCADE` удаляет затронутые edge occurrences, но не глобальные edge-атомы.
- Обычный `DELETE` запрещён при memberships/references. `CASCADE` удаляет memberships, incidences и overlays атомарно и применяет объявленные `ON DELETE` policies.

`RELATE`:

```gql
RELATE
  (person:alice)
  -[friendship AS e LABEL CloseFriend]->
  (person:bob)
IN GRAPH social;

RELATE
  (person:alice)
  -[friendship:alice_bob AS e LABEL CloseFriend]->
  (person:bob)
IN GRAPH social;
```

- `[friendship]` генерирует новый edge RecordId.
- `[friendship:id]` создаёт отсутствующий либо переиспользует существующий глобальный edge-атом.
- Endpoints и edge автоматически получают membership, если его ещё нет.
- `SET`, `CONTENT`, `WEIGHTED` и `LABEL` у `RELATE` всегда graph-local.
- Если edge уже имеет occurrence в этом же графе, `RELATE` возвращает конфликт. Изменение topology выполняется явной парой `DETACH` + `RELATE` в одной транзакции.
- Ошибка topology/family validation не оставляет созданный base edge.

`MATCH`:

```gql
MATCH
  (person AS v1 LABEL Employee)
  -[friendship AS e LABEL CloseFriend]->
  (* AS v2 LABEL Moderator)
IN GRAPH social;
```

- Первая позиция — class, exact `RecordId` либо `*`.
- `AS` всегда вводит alias.
- `LABEL` всегда задаёт graph-local label predicate.
- `* ... LABEL X` означает label-only lookup.
- Class + label используют пересечение либо меньший posting stream с residual check.

Direct reads:

```gql
SELECT * FROM person;
SELECT * FROM person IN GRAPH social;
SELECT * FROM person:alice;
SELECT * FROM person:alice IN GRAPH social;
```

Unscoped форма читает base registry. Graph-scoped форма требует membership и возвращает effective properties.

### 2.3. Физическое хранение

В [disk_storage_spec.md](D:/Programming/Rust/ferros-grynn/docs/disk_storage_spec.md) закрепить:

```text
<database>/
  atoms/<generation_uuid>/
    superblock.fgb
    atoms_hot.fgb
    properties.fgb
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

  graphs/<graph_id>/<generation_uuid>/
    superblock.fgb
    nodes_hot.fgb
    edges_hot.fgb
    adjacency_overflow.fgb
    adj_over_cl.fgb
    overlays.fgb
    subgraphs.fgb
    labels.fgb
    freelist_*.fgb
    membership.fgidx
    stats/
      graph_stats.fgb
      label_stats.fgb
      prop_stats.fgb
    idx/
      cls_<class_id>.fgidx
      lbl_node_<label_id>.fgidx
      lbl_edge_<label_id>.fgidx
      std_<ordinal:04>.fgidx
      uniq_<ordinal:04>.fgidx
      cnt_<class_id>.fgidx
```

- `atoms_hot.fgb` использует 48-байтовый `AtomHotSlot`: generation, kind, `ClassId`, canonical
  RecordId reference, base PropertyMap reference и flags; он не содержит graph-local state.
- `nodes_hot.fgb` и `edges_hot.fgb` остаются обязательными target-файлами, а их 160-байтовые
  `NodeHotSlot`/`EdgeHotSlot` хранят occurrence: `GlobalAtomId`, local generation, optional
  label, overlay reference и graph-local structural state. Legacy является только прежний
  graph-owned набор полей с `table_id`/base properties и durable query-scratch flags, а не эти
  page families.
- `membership.fgidx` отображает `GlobalAtomId → graph-local DiskAtomRef`.
- `memberships.fgidx` является обратным database index `GlobalAtomId → graph occurrences`.
- Global primary отображает canonical `RecordId → GlobalAtomId`.
- Base secondary indexes находятся в AtomStore.
- Explicit effective indexes являются полными graph-local B+tree над effective values.
- Graph property histogram строится по effective view и помечается stale при релевантном base/overlay изменении.
- Отсутствующий overlay не вызывает чтение `overlays.fgb` и не создаёт merged `PropertyMap`.
- `buffer_pool_pages` остаётся общим бюджетом всех database/graph files.

Устранить типовую ложь `GraphId=0`:

```text
DataSpaceId::Database = 0
DataSpaceId::Graph(GraphId) > 0
```

- `PageKey`, WAL и `FileCatalog` используют crate-private `DataSpaceId`.
- `GraphId` всегда ненулевой.
- Database files не называются system graph и не имеют `GraphSuperblock`.
- Добавляется логически WAL-versioned `AtomStoreSuperblock`; его home-page публикуется
  checkpoint metadata COW через `.tmp/main/.prev` и не мутируется in-place.
- Один database WAL атомарно публикует base, memberships, graph topology, overlays и indexes.

Так как core format v1 ещё не заморожен:

- `STORAGE_FORMAT_VERSION` остаётся candidate v1;
- `DatabaseCatalog` повышается до `FGDC v7`; property-key strings вынесены в append-only
  `property_keys.fgb`, а catalog хранит только `PropertyKeyId`;
- `FileCatalog` повышается до `FGFC v5` с `space_id`;
- новые page/file kinds получают новые discriminants и golden vectors до написания их codecs;
- текущие `FGDC v5`/`FGFC v4` и graph-owned atom layout явно отклоняются;
- миграция промежуточного формата не реализуется.

### 2.4. Режимы хранения

- Mode 1: `DatabaseRoot` также нормализуется на global AtomStore + graph memberships; standalone SDK graph остаётся owning materialization.
- Mode 2: AtomStore и graph-local stores читаются через существующий `HybridReadEngine`; его ключ меняется с `GraphId` на `DataSpaceId`.
- Mode 3: материализует effective graph view. Результат владеет копиями данных и origin mapping; mutable flush пишет graph-local topology/overlay, но никогда не меняет base неявно.
- Удаление графа удаляет memberships, overlays, topology и effective indexes, но сохраняет глобальные атомы.
- Импорт owning `MaterializedGraph` создаёт отсутствующие base atoms, а для существующих RecordId сохраняет отличия как overlay нового графа.

## 3. Сверка с текущим кодом и документами

| Требование               | Текущее состояние                                                         | Исправление                                        |
|--------------------------|---------------------------------------------------------------------------|----------------------------------------------------|
| Global atom registry     | В `DatabaseRoot` его нет                                                  | Добавить отдельный MVCC-owned AtomStore            |
| Shared base properties   | Каждый `StoredGraph` владеет полной копией атома                          | Вынести base properties из graph storage           |
| Class задаётся RecordId  | `TableSchema` и `AtomTableMemberships` независимы от `RecordId.table`     | Удалить data membership из SchemaCatalog           |
| Атом в нескольких графах | Возможны только независимые копии                                         | Reverse membership + graph-local occurrences       |
| Sparse instancing        | Отсутствует                                                               | Overlay `Value/Hidden`, lazy allocation            |
| GraphRef                 | Использует `graph:<name>` RecordId                                        | Перейти на `GraphId + name`                        |
| Direct class row source  | System-table cursor пуст                                                  | Читать global AtomStore                            |
| RELATE identity          | Edge body задаёт alias/label, но не class/id                              | Новая class/id/AS/LABEL grammar                    |
| Property-record          | `Value::Record` есть, но row должен находиться в graph                    | Разрешить atom с нулём memberships                 |
| Base/effective indexes   | Только graph-owned table indexes                                          | Разделить explicit scopes                          |
| Label и class            | Смешиваются через двоеточие                                               | Class в первой позиции, label только через `LABEL` |
| Полезная основа          | RecordIdPart, PropertyMap, B+tree, WAL, MVCC, GraphRead, HybridReadEngine | Переиспользовать без нового storage framework      |

Нормативно обновить [gql_spec.md](D:/Programming/Rust/ferros-grynn/docs/gql_spec.md), [disk_storage_spec.md](D:/Programming/Rust/ferros-grynn/docs/disk_storage_spec.md), `graphs.md`, `gql_grammar.md` и ненормативный `gql_tech_spec.md`.

Производные документы:

- этот `docs/rework_plan.md` является отдельным authority-планом коррекции; дублирующий
  `graph_first_atom_model_plan.md` не создаётся;
- синхронизировать `implementation_plan.md`, `disk_storage_plan.md`, `values_plan.md` и `storage_engine_hybrid_integration_plan.md`;
- в [full_plan.md](D:/Programming/Rust/ferros-grynn/docs/full_plan.md) вставить этап 8.1 как обязательную зависимость этапов 9–31;
- исторические отчёты этапов 5–9 не переписывать; итоговый отчёт A16 должен явно указать,
  какие их выводы superseded.

Реестр `full_plan.md`:

- `Q-08` переоткрыть: explicit table membership признан неверной моделью;
- `Q-23` заменить на удаление system-table/system-graph data model и закрыть только после RAM/local gate;
- `Q-24` закрыть после class/id-aware `RELATE`;
- добавить `Q-25`: graph-owned atoms и отсутствие global AtomStore;
- добавить `Q-26`: bounded fanout base mutations, scoped effective indexes и cascade preflight;
- прежнюю закрытую задачу о Label-index page relocation перенумеровать из `Q-25`
  в `Q-27`, не меняя её исторический статус;
- этап 8 пометить частично superseded: GRAPH DDL остаётся, TABLE DDL заменяется CLASS;
- этап 9 оставить незавершённым до полного cutover.

## 4. Dependency-ordered реализация

**Статус на 2026-09-22: ✅ A0–A16 выполнены.** Production и test paths используют
только graph-first Class/AtomStore/occurrence model; TABLE/system-graph fallback
и selector удалены. Correctness/performance evidence и ограничения зафиксированы
в [`reports/graph_first_atom_model_report.md`](reports/graph_first_atom_model_report.md).

**Результат A8:** реализованы exact `AtomStoreSuperblock`, database-global append-only
`PropertyDictionary`, `FGFC v5` binding, единая WAL-публикация data+metadata, WAL-first reopen,
checkpoint backfill и metadata COW `tmp/main/prev`. Golden/corruption, old-snapshot,
abort/commit/reopen и checkpoint-recovery проверки пройдены. Подробности:
[`reports/atom_store_stage_a8.md`](reports/atom_store_stage_a8.md).

**Результат A9:** graph-space хранит только occurrences, topology, labels и sparse overlays;
`membership.fgidx` и database-global `memberships.fgidx` проверяются в обе стороны. AtomStore,
graph files и membership indexes публикуются одним WAL transaction, открываются по одной snapshot
границе и совместно backfill'ятся до COW-публикации обоих superblock. Focused A8/A9 gates пройдены.

|   | Шаг                                  | Зависит от | Работа                                                                                                                                   |
|:--|--------------------------------------|------------|------------------------------------------------------------------------------------------------------------------------------------------|
|   | A0. Зафиксировать коррекцию ✅       | —          | Использовать этот план, обновить реестр Q-08/Q-23–Q-26 и остановить этапы 9–31 до кодового cutover                                       |
|   | A1. Семантические спецификации ✅    | A0         | Переписать glossary, formal model, CLASS, RecordId, GraphRef, base/effective view, labels, references и DML                              |
|   | A2. Grammar и примеры ✅             | A1         | Утвердить `CLASS`, `IN GRAPH`, `ATTACH/DETACH`, `RESET`, class/id/AS/LABEL pattern grammar                                               |
|   | A3. Disk specification ✅            | A1         | Зафиксировать AtomStore, memberships, overlays, DataSpaceId, catalog versions, exact Atom/Node/Edge hot layouts, WAL и recovery          |
|   | A4. Скорректировать планы ✅         | A2, A3     | Обновить зависимости и статусы всех активных планов; исторические reports оставить                                                       |
|   | A5. Типы и schema API ✅             | A2         | Ввести ClassId/GraphId/DataSpaceId, новый GraphRef и ClassSchema; убрать TABLE compatibility surface                                     |
|   | A6. In-memory logical model ✅       | A5         | Добавить global AtomStoreRoot, graph occurrences, reverse memberships и lazy overlays                                                    |
|   | A7. MVCC и conflicts ✅              | A6         | Разделить BaseAtom, GraphMembership, GraphOverlay и GraphIncidence conflict keys; добавить fanout preflight                              |
|   | A8. Durable AtomStore ✅             | A3, A5     | Реализовать AtomStoreSuperblock, atom/property pages, global primary, file registry, WAL/recovery                                        |
|   | A9. Graph storage refactor ✅        | A7, A8     | Превратить graph slots в occurrences, вынести properties в base store, добавить overlays и membership index                              |
|   | A10. Indexes и statistics ✅         | A9         | Перенести base indexes, добавить full effective indexes, class/label indexes и scope-aware statistics                                    |
|   | A11. DDL frontend ✅                 | A5, A10    | Parser → analysis → IR → execution для CLASS/FIELD/INDEX/INFO; удалить system-table path                                                 |
|   | A12. Direct DML ✅                   | A7, A11    | Перевести CREATE/UPDATE/UPSERT/DELETE/SELECT на global AtomStore и effective graph view                                                  |
|   | A13. Graph DML ✅                    | A9, A12    | Реализовать ATTACH, DETACH, RELATE, graph LABEL updates, RESET и family validation                                                       |
|   | A14. References и modes ✅           | A12, A13   | Scope-aware record dereference, Mode 1/2 parity, Mode 3 materialization/write-back                                                       |
|   | A15. Correctness/performance gate ✅ | A14        | Полная RAM/local/native-async/recovery/concurrency/benchmark матрица                                                                     |
|   | A16. Cutover и очистка ✅            | A15        | Удалить TableSchema/AtomTableMemberships/system graph paths, временный selector, обновить AGENTS/report/Graphify и завершить этапы 8.1/9 |

Runtime fallback после cutover не сохранён. Единственное упоминание `DEFINE TABLE`
в собираемом коде является отрицательным parser regression.

## 5. Проверка и критерии завершения

Обязательные сценарии:

- два графа используют один node RecordId и видят общую base property;
- overlay одного графа не виден другому;
- base update виден обоим, кроме явно overridden/Hidden fields;
- `RESET` возобновляет наследование актуального base value;
- optional labels одного атома различаются между графами;
- один edge RecordId имеет разные topology/weight/label в двух графах;
- unattached property-record читается напрямую и через RecordId reference;
- ссылка в graph scope возвращает effective member либо base non-member;
- exact/class/label/class+label MATCH выбирают правильные indexes;
- Standard/Unique/Count проверяются отдельно для Base и Effective scope;
- base update повторно проверяет затронутые effective Unique indexes;
- `DETACH` и `DELETE` соблюдают RESTRICT/CASCADE и reference policies;
- graph removal не удаляет global atoms;
- stale `GlobalAtomId` не проходит generation check;
- старый snapshot видит согласованную пару base + membership + overlay;
- FCW/SSI обнаруживают base/overlay/membership phantoms;
- WAL recovery и checkpoint атомарно восстанавливают несколько data spaces;
- старый candidate layout возвращает typed incompatible-format error;
- Mode 1, Mode 2 и materialized Mode 3 дают одинаковые effective результаты.

Performance gates:

- unscoped RecordId point read не хуже текущего graph point baseline более чем на 5%;
- graph-scoped no-overlay point read — не хуже более чем на 10%;
- no-overlay read не делает allocation и не читает overlay page;
- class и label scans не регрессируют более чем на 5%;
- base update выполняет `O(memberships изменяемого атома)`, без scan всех графов;
- graph traversal разрешает base pages через bounded read epoch/resolved table;
- shared payload хранится один раз плюс `O(number of memberships)` metadata;
- Mode 1 query throughput регрессирует не более чем на 3%;
- существующие HybridReadEngine memory и I/O gates сохраняются.

После реализации выполнить targeted tests, обе feature-конфигурации, полный library gate, nightly rustfmt check, focused Clippy, profiling для lookup/overlay/index fanout и `graphify update .`. Итог сохранить в `docs/reports/graph_first_atom_model_report.md`.

Зафиксированные ограничения:

- нет class inheritance, multi-class atoms и автоматического изменения class;
- один graph-local label на occurrence;
- нет полного физического копирования атома и отдельного `InstanceId`;
- нет автоматического выбора index layout;
- нет canonical columnar property store;
- нет миграции промежуточного disk candidate;
- cross-database identity и bridge qualification остаются этапу Bridges;
- unrelated dirty checkout сохраняется.
