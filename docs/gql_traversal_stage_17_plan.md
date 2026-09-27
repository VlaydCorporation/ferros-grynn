# Этап 17. Traversal VM, полноценные closures, `FAST` и physical sparse frontier

## Статус и границы

**Статус (2026-09-28): ✅ V0–V15 завершены; этап 17 закрыт.** Language/VM,
LevelPath, correctness и performance gates пройдены. `Q-10` закрыт положительным transport
решением. Итоговое evidence:
[`gql_traversal_stage_17_report.md`](reports/gql_traversal_stage_17_report.md); отдельное
evidence `Q-10`:
[`gql_traversal_stage_17_q10_report.md`](reports/gql_traversal_stage_17_q10_report.md).

Документ задаёт dependency-ordered реализацию этапа 17 из
[`full_plan.md`](full_plan.md). Нормативную семантику определяет
[`gql_spec.md`](gql_spec.md), синтаксис — [`gql_grammar.md`](gql_grammar.md), технические
границы — [`gql_tech_spec.md`](gql_tech_spec.md). Stage 17 не создаёт второй MATCH executor,
универсальный traversal trait, отдельную async runtime или новые зависимости.

## 1. Результат

После этапа GrynnQL получает отдельный от MATCH потоковый traversal executor:

```text
source
→ Traverser stream
→ map / flatMap / filter / navigation
→ sack / side effects / path propagation
→ repeat / branch / choose / coalesce
→ barriers / aggregates / graph algorithms
→ RETURN / Value
```

Сохраняется единый pipeline:

```text
parse → structural validation → typed analysis → logical IR
      → optimizer → physical plan → sync pull execution
```

Реализация переиспользует Stage-16 MATCH, `TraversalIndex`, `PathTraceArena`, statement-local
graph cache, Stage-13 spill и Stage-14 optimizer/EXPLAIN. Одновременно:

- session-scoped GrynnQL closures становятся исполнимыми;
- `FAST` становится exact execution mode без ослабления ACID и семантики;
- `KEEP LEVELPATH` переносится только в graph-local DML;
- `Q-10` проверяется настоящим snapshot-exact local physical cursor;
- несуществующие analytics-алгоритмы и `tv::algo` остаются будущим этапам.

## 2. Traversal contract

### 2.1. Синтаксис и источники

```grynql
TRAVERSE [ATOM]
    [CONSTRAINTS <path_constraints>]
    FROM source
    [IN GRAPH graph_ref]
    |> tv::step(...)
    |> tv::step(...)
    [RETURN projection]
    [TIMEOUT duration];
```

`|>` является левоассоциативным expression operator. Источники:

- `GraphRef`: первым генерирующим шагом обязан быть `tv::V()` либо `tv::E()`;
- `RecordId`/`AtomRef`: требуется `IN GRAPH`;
- `AtomOccurrenceRef`: использует собственный graph scope;
- `MATCH`: один traverser на binding row, последний endpoint alias — `current`, остальные
  aliases сохраняются в `env`;
- `SELECT VALUE`: один traverser на value;
- обычный `SELECT`: один traverser на object row с aliases в `env`;
- array, set и конечный range: encounter/canonical/range order соответственно;
- parameter или idiom: после вычисления применяется одно из правил выше.

Bare atom без occurrence scope и без `IN GRAPH` отклоняется до чтения. Без `RETURN` выдаётся
`array<Value>` текущих значений. `RETURN GRAPH` и graph assembly принадлежат этапу 18.

### 2.2. Traverser и bulking

```rust
struct Traverser {
	current: Value,
	trace: Option<PathTraceId>,
	sack: Value,
	named_sacks: NamedSacks,
	env: BindingSlots,
	bulk: NonZeroU64,
}
```

Публичный `Traverser` не добавляется. Слияние допустимо только при полном равенстве
`current`, `trace`, `sack`, `named_sacks` и `env`. Path tracking отключает слияние ветви;
overflow `bulk` является typed error. `count`/`sum`/`avg` и side effects учитывают logical
multiplicity; `limit`/`skip` могут делить bulk. `dedup` оставляет первое стабильное вхождение с
`bulk = 1`. IMPURE side effect разворачивает bulk и вызывается на каждый logical traverser.

Однопоточный baseline использует adjacent merge и bounded hash merge только на естественных
frontier/barrier boundaries. При memory pressure merge отключается, а поток продолжается без
сжатия. `PathTraceArena` остаётся единственным path representation и включается только когда
путь наблюдаем.

### 2.3. Steps

Исполнимый набор:

- navigation: `V`, `E`, `out`, `in`, `both`, `outE`, `inE`, `bothE`, `inV`, `outV`,
  `hyper`, `meta_inner`;
- filter: `hasattr`, `has`, `hasLabel`, `where`, `dedup`, `dedup_by`, `simplePath`,
  `cyclicPath`;
- transform: `map`, `flatMap`, `values`, `select`, `match`, `as`, `project`, `unfold`;
- state: `init_sack`, `sack`, `get_sack`, `init_named_sack`, `named_sack`,
  `get_named_sack`, `side_effect`, `path`, `emit`;
- control: `limit`, `skip`, `repeat(...).times(n)`, `repeat(...).until(closure)`,
  `repeat(...).emit()`, `branch`, `choose`, `coalesce`;
- barriers: `order`, `group`, `fold`, `count`, `sum`, `min`, `max`, `avg`.

`flatMap`/`unfold` принимают array, set или конечный range. `tv::match` вызывает существующий
MATCH executor как correlated pull source. `coalesce` использует savepoint для каждой
альтернативы: пустая альтернатива откатывается, первая непустая сохраняется, ошибка не
маскируется. Barriers используют общий statement budget и Stage-13 spill; runtime closure не
spill-ится. Публичный spilled handle не возвращается.

## 3. Language closures

Поддерживаются expression- и block-формы:

```grynql
|$x| $x.properties.age > 18

|$x, $acc| -> number {
    LET $next = $acc + $x.properties.weight;
    RETURN $next;
}
```

Closure захватывает lexical values по значению и принадлежит создавшей её `GqlSession`.
Session хранит concrete registry `RuntimeId → typed body + captures + signature + effect +
SessionId` с лимитом 256 МиБ retained IR/captures и без скрытого eviction. Closure может жить в
`LET`, быть аргументом и результатом persistent session, но не переносится в другую session, не
переживает `GqlSession::drop`, не сериализуется и не spill-ится. Temporary session
`Datastore::execute` не позволяет closure покинуть вызов.

`Closure::call` сохраняет native Rust contract; language closure отвечает typed
`ClosureRequiresSession`. GrynnQL evaluator разрешает её через текущую session. Equality/order/
hash остаются по `RuntimeId`.

Closure body может использовать уже исполнимые statements, кроме transaction boundaries,
`BEGIN IMPORT` и вложенного `EXPLAIN ANALYZE`. Каждый вызов выполняется под transaction
savepoint, который при ошибке восстанавливает private root, conflict/read/write sets, scopes и
mutation log. `BREAK`/`CONTINUE` не пересекают boundary closure.

Effect classes:

- `PURE`;
- `STABLE_READ`;
- `DATA_MUTATION`;
- `SCHEMA_MUTATION`.

Predicate/map/control positions принимают только `PURE`/`STABLE_READ`. `DATA_MUTATION`
разрешена только в `tv::side_effect`; `SCHEMA_MUTATION` запрещена внутри активного traversal,
но closure может быть вызвана как самостоятельное expression statement.

`tv::side_effect` даёт step-boundary read-your-writes: downstream читает новую private-root
epoch, upstream cursor не перезапускается, новые atoms не входят в поток без нового `V/E`, а
затронутый local graph до конца statement переключается на materialized private root. Порядок
side effects последовательный и в `FAST`.

## 4. Algorithms

Stage 17 адаптирует только уже существующие реализации:

- BFS/DFS-based `kHop` и `reachable`;
- `shortestPath`, `weightedShortestPath`, `distanceTo`, `bellmanFord`;
- `topologicalSort`, SCC/WCC, condensation;
- minimum spanning forest;
- articulation points/bridges;
- LCA и cycle detection.

Seed-local algorithms используют current occurrence. Graph-wide algorithms работают над
induced graph текущего потока: node occurrences одного `GraphId`, duplicates удалены, binary
connector включается только если оба endpoint входят в selected set. Неподдерживаемые
edge-as-endpoint/cross-graph/hyper inputs дают typed domain error. Исходный graph не копируется:
используется filtered `GraphRead` view.

Weighted core переводится с `f64` на существующий `Number`. Dijkstra отклоняет negative и
non-finite weights; Bellman-Ford принимает negative и диагностирует negative cycle;
отсутствующий weight равен `1`. Один exact core используется graph API, MATCH и TRAVERSE.

PageRank, centralities, triangles, clustering, label propagation, Louvain и all-shortest
enumeration принадлежат этапу 26. `tv::algo` и UDF/algorithm registries принадлежат этапу 21.

## 5. `KEEP LEVELPATH`

`MATCH` и `TRAVERSE` read-only и не принимают `KEEP LEVELPATH`. Сохранение допустимо только в:

- `ATTACH ... KEEP LEVELPATH`;
- `RELATE ... KEEP LEVELPATH`;
- graph-scoped `UPDATE ... KEEP LEVELPATH`.

`DETACH` удаляет occurrence вместе с overlay. Base mutations и `UPDATE GRAPH` ничего не
записывают. `levelpath` — system-managed graph-local overlay property; прямой пользовательский
`SET/UNSET properties.levelpath` запрещён.

Reserved object:

```grynql
{ kind: "atom", chains: [[{ graph: GraphRef, owner: AtomRef }, ...], ...] }

{
    kind: "binary_edge",
    source: { atom: AtomRef, chains: [...] },
    target: { atom: AtomRef, chains: [...] }
}
```

Top-level occurrence имеет `chains: [[]]`. Сохраняются все containment chains, они
дедуплицируются и сортируются по `{GraphId, AtomRef}`; `GraphRef` фиксирует revision вычисления.
Cycle/corruption fail-closed. Hyperedge с явным `KEEP` даёт typed unsupported. Dynamic
`graph::level_path(occurrence)` вычисляет тот же object read-only.

## 6. `FAST`

Внутренний boolean заменяется enum:

```rust
enum ExecutionMode {
	BestEffort,
	Deterministic,
	Fast,
}
```

`FAST` сохраняет ACID, FCW/SSI, constraints, exact predicates, result multiset, explicit
`ORDER BY`, budgets, cancellation и error behavior. Он может ослабить только неоговорённый
порядок, plan/replay stability, pure branch/hash merge order и schedule float reductions.
Int/decimal reductions exact. Approximation, sampling, early cutoff и пропуск validation
запрещены. IMPURE closures и DML side effects всегда source-ordered.

Baseline остаётся однопоточным pull. Parallel path добавляется только после measured CPU hotspot
не менее 20% и paired gain не менее 15% на 4/8 cores при приемлемых p99/RSS.

## 7. Physical sparse frontier и `Q-10`

Граница остаётся внутри datastore:

```text
Traversal VM
→ Transaction query adapter
→ PhysicalTraversalCursor
→ HybridReadEngine
```

Local `QuerySnapshot` фиксирует storage commit/visible LSN, graph generation/bindings, registry
handles и exact home/WAL source identity. Physical path допустим только при отсутствии
transaction-private mutations, точном совпадении generation и однозначном page mapping.

Cursor предоставляет owned logical операции `expand`, `resolve_fields` и
`resolve_inner_graph`; GQL не получает pages, guards, `DataFileId` или `StorageValue`.
Окно ограничено 256 logical operations и 32 cold home pages. Порядок source resolution:
resident R8 → exact visible WAL/staged → cold home. После полной checksum validation pages
decode-ятся, а logical order/multiplicity восстанавливаются отдельно. Mixed WAL/home batch не
подменяет WAL home-страницей; ошибка после IoRing submission не retry-ится через sync.

Transport selector `Sync`/`IoRing` доступен только prototype crate. IoRing попадает в production
router только при median gain ≥20%, p99 regression ≤5%, CPU regression ≤10% и соблюдении memory
budget. Иначе sync Direct остаётся production route, а отрицательное измеренное решение также
закрывает `Q-10`.

**Решение (2026-09-27): `Q-10` закрыт положительно.** Реальный local
`PhysicalTraversalCursor` дал для frame-IoRing median `0,646×` sync Direct, улучшил p99 и не
ухудшил CPU/RSS gates. Production router поэтому использует frame-IoRing для подходящего
`NativeAsync` sparse window; `Sync`, singleton miss, недоступная capability и неподходящее окно
детерминированно остаются на sync Direct. Пользовательского transport knob не добавлено.

## 8. Dependency order

| Шаг | Зависит от | Результат |
|---|---|---|
| V0 | 14–16 | Нормативная синхронизация, план и baseline |
| V1 | V0 | Language closures, registry, effects, savepoints |
| V2 | V0 | `TRAVERSE`, generic `|>`, sources, steps и logical IR |
| V3 | V1–V2 | Physical plan, pull VM skeleton, estimates и базовый EXPLAIN |
| V4 | V3 | Navigation/filter/map/flatMap/project и correlated MATCH |
| V5 | V4 | Bulk, sacks, env, side effects, path и emit |
| V6 | V1, V5 | repeat/branch/choose/coalesce и downstream read-your-writes |
| V7 | V5–V6 | Barrier/aggregate budget, spill и cleanup |
| V8 | V4, V7 | Induced graph и adapters существующих algorithms |
| V9 | V4 | Mutation-only LevelPath, dynamic read, WAL/reopen |
| V10 | V3, V6–V8 | `ExecutionMode` и exact `FAST` |
| V11 | V4 | Snapshot-exact `PhysicalTraversalCursor` и transport selector |
| V12 | V7–V11 | Optimizer/EXPLAIN completion и counters |
| V13 | V12 | Correctness gate |
| V14 | V13 | Performance gate и решение `Q-10` |
| V15 | V14 | Report, статусы, module guidance, checks и Graphify |

V1 и V2 независимы. V9 и V11 независимы от V5–V8 после появления V4.

Все шаги V0–V15 завершены 2026-09-28. Speculative parallel runtime не добавлен: обязательный
профильный gate не показал одновременно CPU-hotspot не менее 20% и paired gain не менее 15%
на 4/8 cores. Однопоточный pull остаётся production baseline.

## 9. Gates

Correctness gate проверяет syntax/source scope, closure lifetime/effects/savepoints, expanded
`bulk=1` oracle, все steps/control paths, spill/no-spill parity, graph-algorithm parity, exact
weights, LevelPath shapes/recovery, `FAST` multiset parity и exact physical snapshot/WAL/home
precedence. RAM/local/reopen и обе I/O feature-конфигурации обязательны.

Performance gate фиксирует Stage-16 baseline и измеряет 1K–1M starts, branching 2/8/32,
depth 1/4/7/30, bulk 1/4/32/1024, path on/off, 1/4/8 files, resident ratio 0/50/90/100% и
barrier budgets 1/32/256 МиБ. Без-traversal SELECT/MATCH/DML и Stage-16 MATCH могут
регрессировать не более чем на 5%; one-hop TRAVERSE — не более чем на 5% относительно MATCH;
instrumentation ≤2%; `FAST` ≤3% к BEST_EFFORT и должен выиграть ≥10% хотя бы на одном
representative unordered workload. Path state и query memory обязаны оставаться bounded.

Профилирование выполняется только с `--profile profiling` и exact Cargo EXE/PDB pair. CSV,
логи и profiler artifacts сохраняются в `target/gql-stage17/`. Ошибки symbols/ETW фиксируются
как `SKIP`, но не заменяют paired benchmark.

## 10. Намеренно отложено

- `RETURN GRAPH`/graph assembly — этап 18;
- UDF/catalog/overloads/security/recursion и `tv::algo` — этап 21;
- новые traversal indexes — этап 25;
- analytics и all-shortest enumeration — этап 26;
- approximate FAST, публичный `Traverser`, второй MATCH engine, универсальный graph algorithm
  registry и speculative parallel runtime не добавляются.
