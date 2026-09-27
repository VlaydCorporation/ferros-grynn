# Этапы 15–16. Полный StandardPath MATCH и graph-first AtomWalk/nested MATCH

## Статус и границы

**Статус (2026-09-27): M0–M8 и N0–N6 завершены.** Correctness, RAM/local/reopen,
обе feature-конфигурации, exhaustive path oracle, paired performance и VTune/resource gates
пройдены. Evidence:
[`gql_standard_match_stage_15_report.md`](reports/gql_standard_match_stage_15_report.md) и
[`gql_nested_match_stage_16_report.md`](reports/gql_nested_match_stage_16_report.md).
`Q-10` остаётся открытым до появления настоящего physical page frontier.

Документ задаёт dependency-ordered реализацию этапов 15–16 из
[`full_plan.md`](full_plan.md). Нормативную семантику определяет
[`gql_spec.md`](gql_spec.md), точный синтаксис — [`gql_grammar.md`](gql_grammar.md),
а технические границы — [`gql_tech_spec.md`](gql_tech_spec.md).

Этап 15 завершает StandardPath MATCH. Этап 16 расширяет тот же pipeline до AtomWalk,
hyper/meta/nested-графов и cross-level incidence. Второй MATCH executor, универсальный
traversal framework, новые зависимости и новые durable storage formats не добавляются.

## 1. Результат

После этапа 15 поддерживаются:

- обязательный и `OPTIONAL MATCH`;
- общие aliases, несколько и несвязанные patterns;
- `REPEATABLE ELEMENTS`, `DIFFERENT EDGES`, `DIFFERENT ATOMS`;
- inline class/label/property/`WHERE`-ограничения;
- четыре направления incidence;
- bounded и открытые variable-length paths с нормализацией в конечный диапазон;
- `WALK`, `TRAIL`, `ACYCLIC`, `SIMPLE`;
- один канонический `SHORTEST` либо `WEIGHTED SHORTEST` для каждой пары source–target;
- именованные пути и `path::*`;
- binding-level и projection-level `DISTINCT`;
- общий optimizer/`EXPLAIN`, budgets, timeout и cancellation.

После этапа 16 тот же pipeline поддерживает:

- `ATOM` и edge-as-endpoint;
- `HYPER ... CONTAINS`;
- `META`, `METAVERTEX`, `METAEDGE`;
- `WITHIN` и ограниченное транзитивное containment;
- `EdgeEndpoint::Nested`;
- V–V, V–E, E–V и E–E incidence;
- точный graph occurrence context каждого binding и path step.

## 2. Нормативный контракт

### 2.1. Канонический синтаксис

```grynql
[OPTIONAL] MATCH
    [DISTINCT]
    [ATOM]
    [REPEATABLE ELEMENTS | DIFFERENT EDGES | DIFFERENT ATOMS]
    [HYPER | META | METAVERTEX | METAEDGE]
    [CONSTRAINTS <path_constraint ...>]
    pattern [, pattern ...]
    [CONTAINS ...]
    IN GRAPH graph_ref
    [WHERE expression]
    [MAXDEPTH depth]
    [TIMEOUT duration]
    RETURN [DISTINCT] projection
    [ORDER BY ...];
```

- Единственная optional-форма — `OPTIONAL MATCH`; `MATCH OPTIONAL` и `MATCH ... FROM graph`
  не имеют compatibility alias.
- `OPTIONAL MATCH DISTINCT` допустим: optionality и binding deduplication независимы.
- `graph_ref` — имя либо параметр типа `GraphRef`.
- `CONSTRAINTS` находится только перед pattern list.
- `MATCH DISTINCT` удаляет дубли полного набора bindings до projection.
- `RETURN DISTINCT` удаляет дубли projected values.
- `ORDER BY` выполняется после projection через Stage-13 sorter.
- `WITH INDEX`, `WITH COST`, `KEEP LEVELPATH` и `RETURN GRAPH` принадлежат этапам
  25, 21, 17 и 18 соответственно.

### 2.2. Bindings и ограничения позиций

```grynql
(person AS p LABEL Employee { age: $age })
(person AS p WHERE p.active)
(person:alice AS p)
(* AS p)
```

- selector — class, exact `RecordId` либо `*`; `AS` вводит alias;
- `LABEL` — graph-local label;
- inline object — conjunction null-safe equality над effective properties; отсутствующее
  свойство не совпадает с `null`;
- inline `WHERE` выполняется после текущего binding и может читать текущие и более ранние
  aliases; forward reference отклоняется анализатором;
- повтор alias — equality join, несовместимый node/edge domain — semantic error;
- несвязанные компоненты образуют Cartesian product.

Match mode действует и на anonymous positions:

- `REPEATABLE ELEMENTS` не вводит uniqueness;
- `DIFFERENT EDGES` (default) запрещает повтор edge-typed positions и connector edges;
- `DIFFERENT ATOMS` запрещает повтор всех endpoint и connector atom positions.

Path mode ограничивает один path и не заменяет match mode.

### 2.3. OPTIONAL и DISTINCT

`OPTIONAL MATCH` является left outer match всего clause. После patterns и `WHERE` входная
строка сохраняется при отсутствии совпадения, а только новые aliases получают `none`.
Standalone optional MATCH имеет одну пустую входную строку. Binding distinct использует
occurrence identity `{GraphId, AtomRef}`, projection distinct — canonical `Value`; оба barrier
сохраняют первое стабильное вхождение и общий Stage-13 memory/spill budget.

### 2.4. Variable paths и constraints

- `*` → `1..effective_max`;
- `*..5` → `1..5`;
- `*3..` → `3..effective_max`;
- `*3` → `3..3`;
- `*0..N` разрешает empty path;
- `effective_max` берётся из `MAXDEPTH`, иначе `FG_PATH_MAX_DEPTH = 30`;
- max больше 30 отклоняется до выполнения.

Default path mode — `TRAIL`:

- `WALK`: endpoint и connector могут повторяться;
- `TRAIL`: connector edge не повторяется;
- `ACYCLIC`: endpoint atom не повторяется;
- `SIMPLE`: endpoint не повторяется, кроме единственного совпадения последнего endpoint
  с первым.

Global и local constraints имеют три независимые оси: один mode, один objective и набор
AND-combined filters. Локальная ось полностью заменяет соответствующую global ось. Два mode
или objective в одном scope являются semantic error.

### 2.5. Shortest paths

- `SHORTEST` возвращает один канонический путь на каждую source–target пару.
- Bound target даёт максимум один путь; free target — по одному для каждого достижимого
  подходящего target.
- Равенство расстояний разрешает stable sequence occurrence identities и directions.
- Unweighted поиск использует BFS, weighted — Dijkstra.
- Search-state включает depth и mode history; endpoint-only visited недостаточен для
  `TRAIL`/`SIMPLE`/`ACYCLIC`.
- Стоимость складывается существующей exact numeric matrix; отсутствующий weight равен `1`.
- Filter `WEIGHTED` допускает только connector edges с явно заданным weight.
- Negative/non-finite weight даёт typed runtime error.
- `COST fn` распознаётся, но является typed unsupported до row pull; владелец — этап 21.

### 2.6. Path values

Именованный path охватывает всю линейную chain. Trace создаётся только если path наблюдаем
projection/`WHERE`/`ORDER`/path-function либо нужен objective.

- `path::atoms` — endpoint и connector occurrences;
- `path::nodes` — node endpoint occurrences;
- `path::edges` — connector edge occurrences; edge endpoint остаётся только в `atoms`;
- `path::length` — число connector steps;
- `path::cost` — exact `Number`, для unweighted path равный length.

Empty path содержит один endpoint occurrence, length `0`, cost `0`.

## 3. Occurrence identity и публичные типы

В `src/graph` вводится:

```rust
pub struct AtomOccurrenceRef {
	atom: AtomRef,
	graph: Arc<GraphRef>,
}
```

- `AtomRef` остаётся глобальной durable identity;
- `AtomOccurrenceRef` — snapshot-relative graph occurrence;
- `Eq`/`Ord`/`Hash` используют `{GraphId, AtomRef}`;
- statement graph cache создаёт один `Arc<GraphRef>` на граф;
- getters: `atom()`, `graph()`, `into_parts()`.

Runtime получает `Value::AtomOccurrence`, `ValueKind::AtomOccurrence` и
`Type::AtomOccurrence(AtomKind)`. Durable field/type и storage codecs occurrence не принимают.
Transient query-run codec получает versioned occurrence/path encoding.

Idioms `id`, `class`, `kind`, `graph`, `label`, `properties` работают occurrence-aware;
обычный `occurrence.field` лениво читает effective property в occurrence graph scope.
`meta::id/class/type` и atom predicates принимают `AtomRef` и occurrence.

`PathStep` хранит occurrence endpoint и optional occurrence connector. Path equality включает
occurrence sequence, snapshot и cost. Single-graph resolve требует один graph view; cross-graph
resolve следует graph каждого step без нового public resolver trait.

`QueryExecutionOptions::max_path_expansions` имеет default `1_000_000` и должен быть больше
нуля. Budget считает topology branches, hyper assignments и containment links до property
filtering; превышение возвращает generated `PathExpansionLimitExceeded`.

## 4. Stage 16

### 4.1. AtomWalk

```grynql
MATCH ATOM
  <(person AS p)>~[annotation AS r]~><[document AS d]>
IN GRAPH knowledge
RETURN p, r, d;
```

Endpoint domain расширяется до node/edge atoms; connector всегда является edge occurrence.
Поддерживаются V–V, V–E, E–V и E–E. Endpoint edge и connector edge остаются разными
positions. Match/path modes учитывают edge endpoint как atom. `ATOM` ортогонален `HYPER`.

`<(...)>` и `<[...]>` внутри AtomWalk задают endpoint kind, не meta. Смешанный meta/non-meta
AtomWalk использует `graph::inner(alias)` в inline `WHERE`; clause-level meta scope действует
на все позиции.

### 4.2. Hyper edges

```grynql
MATCH HYPER [meeting AS e]
CONTAINS HEAD [(person AS a), (person AS b)],
         TAIL [(document AS d)]
IN GRAPH knowledge
RETURN e, a, b, d;
```

- `HYPER` исключает binary edges;
- `HEAD` соответствует source, `TAIL` — target Directed/Bidirectional topology;
- unqualified `CONTAINS` принимает любую сторону;
- HEAD/TAIL не совпадают с Undirected edge;
- одна сторона использует one-to-one endpoint assignments согласно match mode;
- сохраняется исходный endpoint order, иначе canonical occurrence order;
- bag multiplicity сохраняется;
- traversal создаёт pairwise linear branches; multidimensional `HyperPath` не вводится.

### 4.3. Meta scopes и `graph::inner`

- `METAVERTEX`: каждая node endpoint position имеет inner graph;
- `METAEDGE`: каждая edge position, включая connectors и edge endpoints, имеет transition graph;
- `META`: оба правила;
- `HYPER`: отдельная arity axis.

В StandardPath локальные `<(...)>`/`<[...]>` ограничивают одну meta position.
`graph::inner(occurrence) -> GraphRef | none` принимает MATCH occurrence. Meta atom остаётся
opaque: adjacency traversal не входит во внутренний graph автоматически.

### 4.4. WITHIN

```grynql
(child AS c) WITHIN <(module AS m)>
(child AS c) WITHIN*[1..4] <[transition AS e]>
```

- `WITHIN` — один containment level;
- `WITHIN*[min..max]` требует `min >= 1`, default max — `FG_WITHIN_MAX_DEPTH = 7`;
- chain читается child→owner→root; owner может быть meta node или meta edge;
- несколько owners создают отдельную строку на chain;
- одна chain не повторяет `{GraphId, owner AtomRef}`;
- containment — join relation, не connector edge и не `PathStep`;
- path alias через WITHIN отклоняется; `LevelPath` не создаётся.

Reverse containment строится лениво. Bound owner открывает только inner graph; bound child
сканирует visible meta bindings; две свободные стороны используют catalog scan с warning.

### 4.5. Cross-level incidence

`EdgeEndpoint::Nested(QualifiedAtomId)` разрешается от текущего graph scope. Каждый scope atom
проверяется как meta owner, соответствующий inner/transition graph открывается в том же
statement snapshot, final atom и kind валидируются. Malformed scope либо отсутствующий graph/
atom — corruption/invariant error, не silent no-match.

Cross-level branch хранит endpoint occurrence дочернего graph и connector occurrence
родительского graph. `CROSS_LEVEL` проверяет выбранную branch.

## 5. Executor, optimizer и ресурсы

Внутренний binding становится occurrence-aware:

```text
BoundAtom = AtomId hot handle + AtomOccurrenceRef public identity
```

Row хранит graph context отдельно для каждой MATCH column. Concrete statement-local graph cache
лениво открывает `GraphId` в одном snapshot, разделяет property dictionary и регистрирует SSI
predicates. Storage pages/guards в GQL не выходят.

Stage 15 physical operators:

```text
MatchSeed → MatchJoin → FixedExpand/PathEnumerate
          → BreadthFirstShortest/DijkstraShortest
          → OptionalFallback → BindingDistinct
          → MatchProject → ProjectionDistinct → MatchOrder
```

Stage 16 добавляет `AtomExpand`, `HyperContains`, `HyperExpand`, `ResolveNestedEndpoint`,
`WithinJoin`, `TransitiveWithin`. Executor не выбирает algorithm: он зафиксирован plan.

Planner использует Stage-14 counters/histograms, переставляет только connected required
patterns, сохраняет error barriers и OPTIONAL boundary, оценивает path growth bounded branching
series и выдаёт warnings для degree/depth, caps, stale/missing stats, containment scan и hyper
fanout. Plan/EXPLAIN кодирует modes, normalized bounds, constraints, seed, order, algorithm,
scopes, estimates и warnings; ANALYZE добавляет фактические frontier/trace/graph metrics.

Frontier, histories, containment map и trace учитываются общим statement barrier budget и не
spill-ятся. DISTINCT/ORDER могут spill. Deadline/cancellation проверяются перед pull, на каждом
frontier pop и не реже 1024 candidate branches. Partial path после ошибки не публикуется.

Q-10 по frame-IoRing остаётся открытым: materialized `GraphRead` не предоставляет физического
page frontier. Stage 16 фиксирует logical frontier metrics; sparse physical cursor принадлежит
этапу 17 после появления реального local page window.

## 6. Dependency-ordered реализация

| Шаг | Зависит от | Результат | Статус |
|---|---|---|---:|
| M0. Нормативная синхронизация | этапы 13–14 | План, spec/grammar/tech-spec, удаление legacy MATCH forms и baseline | ✅ |
| M1. Occurrence/path contract | M0 | `AtomOccurrenceRef`, runtime Value/Type, occurrence-aware `PathStep`, idioms и transient codec | ✅ |
| M2. MATCH syntax/AST | M0 | OPTIONAL/DISTINCT/modes/constraints, inline predicates, open ranges, MAXDEPTH/ORDER/TIMEOUT и dynamic graph refs | ✅ |
| M3. Typed analysis и IR | M1–M2 | Alias unification, graph refs, constraint precedence, finite bounds и typed occurrence columns | ✅ |
| M4. Fixed pattern/cardinality executor | M3 | Repeated aliases, disconnected joins, modes, inline filters, OPTIONAL и два DISTINCT barriers | ✅ |
| M5. Bounded path enumeration | M4 | Path modes, fixed+variable named paths, lazy trace, path functions и budgets | ✅ |
| M6. Shortest objectives | M5 | Canonical BFS/Dijkstra, exact weights и errors | ✅ |
| M7. Optimizer/EXPLAIN | M4–M6 | Physical operators, estimates, plan hash, warnings, counters и MATCH ORDER | ✅ |
| M8. Stage-15 gate | M7 | Полная correctness/resource/performance матрица и отчёт | ✅ |
| N0. Multi-graph runtime | M8 | Graph cache, per-column contexts и reverse containment | ✅ |
| N1. AtomWalk/cross-level | N0 | Edge endpoints, V–E/E–V/E–E и nested endpoint resolution | ✅ |
| N2. Hyper MATCH | N1 | `HYPER ... CONTAINS`, HEAD/TAIL assignments и pairwise branches | ✅ |
| N3. META/WITHIN | N0–N2 | Meta scopes, `graph::inner`, containment и cycle/depth protection | ✅ |
| N4. Stage-16 planner/EXPLAIN | N1–N3 | Conservative estimates, warnings и graph/frontier metrics | ✅ |
| N5. Stage-16 gate | N4 | Graph-family, cross-level, snapshot, SSI, corruption, memory и performance tests | ✅ |
| N6. Closure | N5 | Отчёт, статусы `full_plan`, deferred registry, AGENTS и Graphify | ✅ |

Parser размещается в bounded MATCH submodules; executor — отдельно для enumeration, shortest,
hyper и nesting. Универсальный многотысячестрочный `match.rs` и traversal trait не создаются.

## 7. Correctness gate

Stage 15 проверяет canonical syntax и отказ legacy forms; static/dynamic graph refs; selectors,
labels/effective properties; repeated aliases/disconnected products; все match modes; OPTIONAL;
оба DISTINCT; directions/topology; range boundaries; four path modes; local/global constraints;
named paths; `path::*`; canonical shortest; numeric weights/errors; inline predicates/error order;
budgets/cancellation; spill equality; optimizer/EXPLAIN; SSI; RAM/local/reopen/native-async.
Small generated graphs сверяются с exhaustive brute-force oracle.

Stage 16 проверяет V–V/V–E/E–V/E–E; directions; HYPER binary rejection и HEAD/TAIL;
assignments/multiplicity; META scopes и local filters; `graph::inner`; один global atom в разных
occurrences; direct/transitive/multi-parent/cyclic WITHIN; depths 1/2/4/7; nested resolution и
corruption; cross-level direction/role/scopes/properties; отсутствие implicit descent и LevelPath;
snapshot/SSI; RAM/local/reopen result и PlanHash equality.

## 8. Performance gate и отложенная поверхность

Stage 15: one-hop/class/label и bounded TRAIL regression не более 5%; unused path alias не
создаёт trace; shortest не enumerate-all; planner p95 сохраняет Stage-14 gate; accounted memory
не превышает budget; instrumentation overhead не более 2%.

Stage 16: StandardPath regression не более 5%; memory зависит от bounded frontier/history;
измеряются branching 2/8/32, depth 1/4/7/30, hyper arity 2/8/32 и nesting fanout 1/4/16.
Scan fallback допустим, но rows/CPU/warnings фиксируются. Профилирование использует только
`--profile profiling` и exact EXE/PDB pair.

Отложены traversal VM/`FAST`/`KEEP LEVELPATH`/map-reduce (этап 17), graph assembly (18), UDF
cost (21), graph indexes и Pattern Cache (25), bidirectional BFS/A*/WCOJ/parallel frontier до
измеренного bottleneck, all-source hyper activation и multidimensional `HyperPath`.
