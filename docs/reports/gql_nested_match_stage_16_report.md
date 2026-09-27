# Отчёт этапа 16: graph-first AtomWalk, hyper/meta и nested `MATCH`

**Дата:** 2026-09-27

**Статус:** N0–N6 завершены; Stage-16 формы используют тот же MATCH pipeline и
occurrence-aware runtime, что StandardPath.

## 1. Итог

Реализованы:

- `MATCH ATOM` с node/edge endpoints и V–V, V–E, E–V, E–E incidence;
- `HYPER ... CONTAINS` с `HEAD`/`TAIL`/unqualified assignments;
- clause-level `META`, `METAVERTEX`, `METAEDGE` и local meta wrappers;
- `graph::inner(occurrence)`;
- direct и bounded transitive `WITHIN`;
- разрешение `EdgeEndpoint::Nested(QualifiedAtomId)`;
- cross-level path step, где endpoint принадлежит child graph, а connector — parent graph;
- physical planning, EXPLAIN warnings и logical frontier metrics.

Новых dependencies, durable formats, общего traversal trait и отдельного executor не добавлено.

## 2. Multi-graph runtime

Statement-local graph cache лениво открывает каждый `GraphId` один раз в том же immutable
snapshot. Binding хранит hot `AtomId` вместе с публичным `AtomOccurrenceRef`; graph context
принадлежит каждой MATCH column, а не одной строке целиком. Lazy field resolution использует
effective properties именно того occurrence.

SSI predicates регистрируются для каждого открытого graph, graph-catalog scan и containment
relation. Storage pages/guards не выходят в GQL.

## 3. AtomWalk и nested incidence

AtomWalk расширяет endpoint domain, не смешивая edge endpoint с connector edge. Direction и
topology-kind проверяются для каждого branch. Nested endpoint разрешается последовательно по
scope owners; malformed scope, отсутствующий graph/atom и kind mismatch являются corruption,
а не silent no-match.

Cross-level trace хранит child endpoint occurrence, parent connector occurrence и исходные
direction/role. `CROSS_LEVEL` проверяет выбранный branch. Обычный adjacency traversal не
проваливается внутрь meta atom неявно.

## 4. Hyper и containment

`HYPER` исключает binary edges. `HEAD` и `TAIL` отражают source/target directed topology;
undirected edge допускает только unqualified `CONTAINS`. Endpoint assignments one-to-one,
сохраняют bag multiplicity и подчиняются match mode. Traversal создаёт pairwise linear branches,
не новый `HyperPath`.

`WITHIN` является join relation и не добавляет `PathStep`, length или cost. Transitive lookup
ограничен `FG_WITHIN_MAX_DEPTH = 7`, хранит visited `{GraphId, owner AtomRef}` и не зацикливается
на повреждённой/cyclic catalog topology. Reverse relation строится лениво только для реально
используемого `WITHIN`.

## 5. Correctness evidence

Focused tests проверяют:

- все четыре AtomWalk domain combinations и направления;
- binary rejection, directed/bidirectional HEAD/TAIL, undirected CONTAINS;
- hyper assignment multiplicity и modes;
- all-position META/METAVERTEX/METAEDGE semantics и local filters;
- одинаковый global atom в нескольких graph occurrences;
- direct/transitive/multi-parent/cyclic WITHIN и bounds 1/2/4/7;
- qualified nested endpoint, direction, role и connector scope;
- отсутствие implicit meta descent и отсутствие `LevelPath` в traversal `Path`;
- local reopen в sync и native-async configurations;
- physical operators и actual graph/frontier/hyper/containment counters в EXPLAIN ANALYZE.

Эти tests входят в полные library gates: 443/445 passed соответственно без и с
`native-async-io`; failures отсутствуют.

## 6. Resource/performance evidence

Probe: `target/gql-stage15-16/match_resource.csv`. Значения являются actual
`EXPLAIN ANALYZE` counters; memory — внутренний accounted peak, а не process RSS.

| Workload | Width | Depth | Rows | Expansions | Frontier peak | Accounted peak | Graph opens |
|---|---:|---:|---:|---:|---:|---:|---:|
| path | 32 | 1 | 32 | 32 | 32 | 34 852 B | 1 |
| path | 8 | 4 | 4 096 | 4 680 | 4 096 | 8 437 836 B | 1 |
| path | 2 | 7 | 128 | 254 | 128 | 492 718 B | 1 |
| path | 1 | 30 | 1 | 30 | 1 | 34 205 B | 1 |
| hyper | 2 | 1 | 2 | 2 | 0 | 0 | 5 |
| hyper | 8 | 1 | 8 | 8 | 0 | 0 | 17 |
| hyper | 32 | 1 | 32 | 32 | 0 | 0 | 65 |
| within | 1 | 1 | 1 | 2 | 0 | 691 B | 3 |
| within | 4 | 1 | 4 | 8 | 0 | 2 620 B | 6 |
| within | 16 | 1 | 16 | 32 | 0 | 10 342 B | 18 |

Hyper assignments равны 2/8/32; containment links — 2/8/32. Path memory следует активному
frontier/history, containment memory — materialized bounded relation, а не всем возможным
traversal paths. StandardPath regression после Stage-16 integration находится между −6,3% и
−28,0% относительно Stage 14, то есть Stage-16 ≤5% regression gate выполнен.

## 7. Planner и наблюдаемость

Physical plan добавил `AtomExpand`, `HyperContains`, `HyperExpand`,
`ResolveNestedEndpoint`, `WithinJoin` и `TransitiveWithin`. Plan encoding фиксирует scopes,
bounds и algorithms; EXPLAIN ANALYZE выводит graph opens, containment links, hyper assignments,
expansions, frontier peak, trace nodes и cancellation checks. При отсутствии специализированного
индекса planner использует корректную conservative scan estimate и явный warning.

## 8. Открытая граница `Q-10`

`Q-10` не закрыт. Текущий `GraphRead` читает materialized transaction root и не предоставляет
физическое bounded page window, поэтому подключение frame-IoRing QD32 было бы эвристикой, а не
доказанным sparse physical cursor. Stage 16 сохранил необходимые logical frontier metrics;
реальный local page window и paired transport gate остаются этапу 17.

Также отложены traversal VM/`FAST`/map-reduce (этап 17), graph assembly (этап 18), custom cost
(этап 21) и PATH/EdgeEndpoint/Reachability/Chain-Cover/FERRARI/BFL/Pattern Cache indexes
(этап 25).

