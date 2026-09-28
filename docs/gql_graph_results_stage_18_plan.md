# Этап 18. Самодостаточные graph-results и Atomic `BUILD GRAPH`

**Статус (2026-09-28): ✅ завершён.** G0–G14 реализованы; correctness,
RAM/local/reopen/NativeAsync и profiling gates пройдены. Итоговое evidence:
[`reports/gql_graph_results_stage_18_report.md`](reports/gql_graph_results_stage_18_report.md).

## 1. Результат и основные решения

После этапа 18:

- `MATCH` и `TRAVERSE` поддерживают терминальный `RETURN GRAPH`;
- анонимный `BUILD GRAPH` возвращает owned `MaterializedGraph`;
- именованный `BUILD GRAPH` атомарно создаёт либо заменяет catalog graph и возвращает `GraphRef`;
- существующий `Path` остаётся единственным публичным path-представлением;
- graph result не удерживает transaction, page guard или storage snapshot;
- `graph::inner(...)` при обычном `SELECT`/`RETURN` возвращает `GraphRef`; materialization выполняется только явным `RETURN GRAPH` или анонимным `BUILD GRAPH`;
- выбранное ребро автоматически добавляет необходимые endpoints, но не втягивает постороннюю adjacency;
- вложенная структура сохраняется; несвязанные graph roots не сплющиваются и дают typed error;
- один source-child, доступный через нескольких owners, превращается в один target child `GraphId`, на который ссылаются несколько scope slots;
- именованный build использует только уже существующие database-global атомы;
- `OVERWRITE` сохраняет root `GraphId`, graph properties, family и system-index identities;
- `SIMPLE`, `BIPARTITE`, `COMPLETE` остаются ортогональными durable validators, а не новыми graph families.

Итоговые артефакты:

- этот план;
- `docs/reports/gql_graph_results_stage_18_report.md`;
- benchmark/profiler artifacts в `target/gql-stage18/`.

Новые зависимости и второй query executor не добавляются.

## 2. Нормативный синтаксис

```ebnf
graph_contract ::= "<" [ graph_family ] { graph_validator } ">"

graph_family ::=
    "GRAPH"
  | "TREE"
  | "ROOTED_TREE"
  | "FOREST"
  | "DAG"
  | "HYPER"
  | "META"

graph_validator ::= "SIMPLE" | "BIPARTITE" | "COMPLETE"

return_graph ::=
    "RETURN" "GRAPH"
    [ graph_contract ]
    [ "ROOT" expr ]
    "{"
        "nodes" ":" expr ","
        "edges" ":" expr
    "}"

build_graph ::=
    "BUILD" "GRAPH"
    [ "OVERWRITE" | "IF" "NOT" "EXISTS" ]
    [ graph_contract ]
    [ graph_name ]
    "FROM" build_source
    [ "ROOT" expr ]
    [ "NODES" expr ]
    [ "EDGES" expr ]
    [ "LIMIT" "NODES" unsigned_integer ]
    [ "LIMIT" "EDGES" unsigned_integer ]
    [ "TIMEOUT" duration_lit ]

build_source ::=
    match_stmt
  | select_stmt
  | traverse_stmt
  | "(" query_stmt ")"
  | graph_value_expr
```

Правила:

- contract содержит не более одной family и одного экземпляра каждого validator;
- IR канонизирует validators в порядке `SIMPLE`, `BIPARTITE`, `COMPLETE`;
- без family анонимный результат остаётся `Subgraph<Frozen>`, новый именованный graph получает `GraphKind::Graph`;
- `ROOT` допустим только для `ROOTED_TREE` или `OVERWRITE` существующего rooted tree;
- `LIMIT` является жёстким ceiling: превышение отменяет операцию;
- `RETURN GRAPH` является альтернативой обычному `RETURN` у `MATCH` и `TRAVERSE` и допускает session/block bindings;
- `BUILD GRAPH` допустим как statement и как весь RHS `LET`, но не как произвольное nested scalar expression;
- обе конструкции запрещены в Atomic Import;
- `graph::inner(meta)` остаётся обычным `GraphRef`.

Тот же нормализованный graph contract используется `DEFINE GRAPH TYPE`. Этап активирует `TREE`, `ROOTED_TREE`, `FOREST`, durable validators и `ROOT`. `VALIDATE ON_MUTATION|DEFERRED` остаётся unsupported; контракт всегда проверяется перед commit.

## 3. Семантика assembly

Добавляется один concrete `GraphAssemblyAccumulator`, без framework или trait.

```text
MATCH / SELECT / TRAVERSE / direct value
        -> sync pull
        -> selector evaluation
        -> deduplication по source occurrence identity
        -> endpoint + containment closure
        -> owned graph assembly
        -> family/root/validator checks
        -> MaterializedGraph | transaction-private built graph tree
```

Source rows потребляются потоково; полный промежуточный `Vec<Value>` не создаётся. Для query-source `NODES`/`EDGES` вычисляются для каждой строки, `SELECT VALUE` предоставляет `$value`, а отсутствие обоих selectors включает рекурсивную классификацию graph-domain values. Для прямого `GraphRef`, `MaterializedGraph`, `Path`, occurrence или коллекции применяется type-directed extraction; отдельные selectors там запрещены.

Selectors рекурсивно принимают arrays/sets, node `RecordId`/`AtomRef`/`AtomOccurrenceRef`, edge occurrence, `Path`, `GraphRef`, `MaterializedGraph` и пропускают `none`. `null` и неподходящий тип дают typed error. Node-selector пути получает node endpoint steps; edge-selector — connector edges и edge-as-endpoint atoms. Bare database-global edge без occurrence не предоставляет topology.

Topology closure:

- выбранное edge добавляет все endpoints;
- endpoint-edge рекурсивно добавляет собственную topology;
- cross-level endpoint добавляет необходимые owner occurrences и scope path;
- containment closure добавляет только containers выбранной nested occurrence;
- adjacency выбранных nodes не обходится;
- visited-set ключуется source graph scope и atom identity;
- containment cycle даёт typed corruption/constraint error без partial result.

Deduplication использует `{GraphId, AtomRef}` для scoped occurrence и `AtomRef` для graphless atom. Один atom в разных scopes остаётся разными occurrences; конфликтующие данные одной scoped identity ошибочны.

### 3.1. Иерархия и shared child

Root scope задаётся прямым `GraphRef`/`MaterializedGraph`, statement root у graph-scoped query либо единственным общим containment root; graphless atoms относятся к result root. Несколько несвязанных roots дают `GraphAssemblyScopeAmbiguous`.

Один source child `GraphId` отображается в один target child; selected subset объединяется по всем путям; сохраняются все минимальные containment chains; разные owners имеют разные slots к одному child. Anonymous result переиспользует одну frozen child identity, named build создаёт один внутренний child `GraphId` и одну физическую копию файлов.

Копируются snapshot-visible `RecordId`, kind, class, effective properties, graph-local label, weight, topology/roles, nested bindings и child properties. Anonymous/new named root копирует properties единственного source root, graphless root получает empty map, `OVERWRITE` сохраняет target properties. Storage identities в result не попадают.

## 4. Family, root и validators

Без внешней зависимости добавляются:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub enum GraphTopologyConstraint {
	Simple,
	Bipartite,
	Complete,
}

#[repr(transparent)]
pub struct GraphTopologyConstraints(u16);
```

Коллекция предоставляет `empty`, `contains` и stable iteration, хранится в `GraphSchema`/`GraphMetadata`; `GraphRead` получает default-empty getter.

- `GRAPH`, `TREE`, `ROOTED_TREE`, `FOREST`, `DAG`, `HYPER`, `META` строят существующие точные Rust-типы;
- без family anonymous result — `Subgraph<Frozen>`;
- `Subgraph` не становится durable family;
- narrowing переиспользует существующие fallible conversions.

Rooted tree:

- `ROOT` возвращает ровно одну root-level node occurrence и автоматически включает её;
- explicit root authoritative и для directed arborescence совпадает со structural root;
- inference допускается только для единственного directed root или singleton;
- undirected multi-node tree без explicit root отклоняется;
- durable root хранится как `RecordId`, при materialization разрешается в local `AtomId`;
- child-scoped, edge или missing root отклоняется.

Validation order: reference/topology integrity, family/root, `SIMPLE`, `BIPARTITE`, `COMPLETE`. `SIMPLE` запрещает self-loop и повторную unordered binary pair. `BIPARTITE` применяет BFS-coloring underlying undirected binary node projection. `COMPLETE` требует каждую unordered distinct node pair; empty/singleton считаются complete. Комбинация — conjunction. Несовместимый hyper/meta/edge-endpoint input даёт constraint error.

Durable contract проверяется для каждого touched graph после transaction replay под commit lock. Это закрывает `Q-37`: прямой Rust mutation path не обходит family/root/validator checks.

## 5. Именованный `BUILD GRAPH`

### 5.1. Create/overwrite

- normal mode требует отсутствующего имени;
- `IF NOT EXISTS` возвращает существующий `GraphRef` без source evaluation и writer work;
- `OVERWRITE` создаёт отсутствующий graph;
- существующий graph сохраняет root `GraphId`, properties, family и system `IndexId`;
- explicit family обязана совпасть;
- без annotation сохраняются validators, с annotation exact set заменяется;
- rooted-tree root определяется заново.

Named build не создаёт base atoms: каждый `RecordId` должен существовать с совпадающими class/kind. Target effective view воспроизводится overlay-правилами: равное base наследуется, отличающееся становится overlay, отсутствующий source key при существующем base скрывается; labels/weights/topology остаются graph-local.

Nested subset получает один internal catalog graph на source child. Его family — generic `Graph`, validators empty, properties копируются. Internal graph имеет `owner_root`, зарезервированное имя `$fg$build_<root-id>_<child-id>`, скрыт из `GRAPHS`/name lookup/normal `INFO`, но доступен по `GraphRef`. Graph-local mutation видна всем owners; direct remove/overwrite/reparent запрещены. Root remove/overwrite каскадно удаляет owned children. Старые escaped child refs после overwrite invalid. Overwrite public graph повторно проверяет referring scopes и cross-level endpoints.

Добавляется одна concrete bulk mutation `InstallBuiltGraphTree` с prepared schemas/roots и replaced children. Она атомарно применяется к private `DatabaseRoot`, учитывает payload в write-set, регистрирует conflict keys, использует savepoint и публикуется обычным commit. Local backend переиспользует существующий full COW generation writer и одну catalog/WAL publication.

## 6. `FGDC v9` и multi-path GraphArena

`DatabaseCatalog` принимает только `FGDC v9`; миграция v8 отсутствует.

```text
0   8   graph_id
8   4   schema_version
12  8   published_at
20  8   generation
28  1   graph_kind
29  1   storage_mode
30  2   topology_constraint_bits
32  8   label IndexId + SchemaVersion
40  8   adjacency IndexId + SchemaVersion
48 16   membership physical binding
64  8   owner_root_graph_id; 0 = public graph
72  4   root_value_len; 0 = absent
76  4   property_count
80  4   arena_slot_count
84 ... canonical graph name
...     canonical StorageValue::Record root bytes
...     sorted graph properties
...     arena slot records
```

Unknown bits запрещены; root допустим только у `ROOTED_TREE` и обязан существовать в root graph; internal owner существует и public; owned descendants имеют один owner root; internal graph не доступен из чужого root arena; names canonical/unique; name map содержит только public graphs.

Wire shape `GraphArenaBindings` сохраняется, но разные slots могут содержать один child `GraphId`; каждый slot имеет exact scope; slots dense; `(scope, child)` уникальна; containment DAG допустим, cycle запрещён. Runtime использует `parent_slot + container AtomId -> child_slot + child GraphId`; queue хранит `(graph_id, arena_slot, ancestry)`. Повтор graph в другой branch допустим, в текущей ancestry — cycle. Page layouts, `GraphSuperblock`, `FGFC`, WAL и checksums не меняются.

## 7. Query pipeline, API и ресурсы

Публичные изменения:

- `QueryStatementKind::BuildGraph`;
- standalone `RETURN GRAPH` имеет kind `ReturnGraph`, terminal MATCH/TRAVERSE сохраняет исходный kind;
- `GraphTopologyConstraint(s)`;
- getters `GraphSchema::topology_constraints/root`;
- validators у `MaterializedGraph`/`GraphRead`;
- формы `GraphRef`, `Path`, `Value::Graph`, `QueryResult` и storage DTO не меняются.

`QueryExecutionOptions` получает positive defaults `max_graph_nodes = 1_000_000`, `max_graph_edges = 5_000_000`; effective ceiling — minimum option/query limit, explicit query limit может быть zero.

Unique target nodes/edges считаются по всему child tree; scope slots учитываются barrier budget; bytes проверяются перед insertion. Assembly не spill-ится. Timeout/cancellation проверяются на pull и не реже 1024 closure/validation operations. Ошибка не публикует partial graph. `DETERMINISTIC` каноничен; `FAST` не меняет итоговую canonical graph identity/topology.

Optimizer получает concrete `GraphAssemble`/`BuildGraph` nodes без alternatives framework. Plan включает source, mode, family/validators/root, selectors/limits/hierarchy и estimates. Plain/FULL EXPLAIN не читает rows, не выдаёт IDs и не берёт writer lease; ANALYZE исполняет один раз и показывает source/selected/closure counts, scopes, dedup, memory, validation timing, files/pages и publication. Continuation не создаётся.

## 8. Dependency-ordered реализация

| Шаг | Зависит от | Результат |
|---|---|---|
| G0 | Этап 17 | Нормативная синхронизация, stage-plan, `Q-37`, baseline |
| G1 | G0 | Runtime graph contract, constraints, explicit rooted-tree constructor |
| G2 | G1 | `FGDC v9`, root/owner/validators, hidden graphs, arena DAG |
| G3 | G0–G1 | Syntax, AST, typed IR и special LET RHS |
| G4 | G3 | Pull-source integration без промежуточного result array |
| G5 | G1, G4 | Selectors, dedup, endpoint closure, properties, root-scoped assembly |
| G6 | G2, G5 | Hierarchy, shared child, multi-owner scopes, qualified topology, cycles |
| G7 | G5–G6 | Family/root/validators и commit-time validator; `Q-37` |
| G8 | G3, G7 | Anonymous results и owned-after-drop |
| G9 | G2, G7 | Named build, overlays, internal children, bulk mutation |
| G10 | G2, G9 | Local COW durability, reopen/checkpoint/recovery/cascade |
| G11 | G8–G10 | Optimizer/EXPLAIN/resources/modes |
| G12 | G11 | Correctness gate |
| G13 | G12 | Performance gate |
| G14 | G13 | Report, status, registry, AGENTS, Graphify |

G2/G3 независимы после G1; G8/G9 независимы после G7.

## 9. Correctness gate

Обязательны canonical syntax и отказ legacy `@constraint`; duplicate/conflicting contracts; standalone/MATCH/TRAVERSE/LET; empty graphs; selectors и invalid values; exact endpoint closure; scope identity; unrelated roots; shared child 2/4/16 owners; subset union; DAG/deep/cycle; owned lifetime; `graph::inner -> GraphRef`; all families; root cases; validator combinations; direct Rust commit gate; create/IFNE/overwrite preservation; hidden/cascaded children; no implicit atoms; exact overlays; FCW/SSI; read-only/import rejection; EXPLAIN behavior; bounded failure; `FGDC v9` golden/corruption cases; RAM/local/reopen/checkpoint/recovery/Sync/NativeAsync parity; no storage DTO/snapshot leakage.

## 10. Performance gate

Матрица: 1K/10K/100K/1M atoms, edge ratio 0/1/4/16, duplicate ratio 1/4/32, nesting depth 1/4/7, owners 1/2/8/32, local files 1/4/8, validators separately/together.

Gates:

- обычные MATCH/SELECT/TRAVERSE ≤5% regression;
- full anonymous copy ≤5% относительно `query_to_memory`;
- нет `O(source rows)` промежуточного `Value` array;
- closure линейна по добавленным atoms/topology/scopes;
- shared child физически записывается один раз;
- query-owned memory укладывается в barrier budget + bounded codec buffers;
- named build имеет одну publication;
- unconstrained DML overhead ≤5%; instrumentation ≤2%.

Профилируемые тесты используют только `--profile profiling` и exact EXE/PDB. Артефакты сохраняются в `target/gql-stage18/`. Финальный gate: focused parser/GQL/graph/datastore/catalog, полные library tests в обеих feature-конфигурациях, оба `cargo check`, nightly fmt check, focused Clippy и `graphify update .`.

`Q-09` и runtime generation retirement `Q-04` остаются этапу 19.
