# Этап 25 — производные графовые индексы

Статус: D0–D6 core correctness gates пройдены; D7 реализуется (HOP/WEIGHT/PURE COST MATCH, global MATCH/TRAVERSE WITH COST, custom path::cost, scoped exact fallback и HOP/WEIGHT traversal: lib gate 682 pass / 10 ignored). Общие Clippy/performance gates, PURE COST runtime resource gate и подключение остальных query consumers остаются открытыми. Correctness gate этапа 24 пройден 2026-10-01 (RAM/local Sync), performance gate остаётся открытым. Зависимости: 16, 17, 20 и versioned PURE UDF этапа 21
для COST/PATTERN_CACHE. Открытые gates этапов 22–23 не являются условием запуска.

## Контракт

Graph-owned Path/Reachability/Neighbourhood definitions используют общие IndexId/version/state,
maintenance worker, bounded external sort, PageManager, WAL и COW publication. DEFINE/REMOVE/INFO/
REBUILD и durable job targets получают graph definitions; transaction API — define_graph_index/
remove_graph_index. Fingerprints включают MODE, direction, labels, budgets и точные версии функций.
MODE NODE|ATOM, default NODE; один artifact хранит одну проекцию. Development layout изменяется
на месте по Q-40. Status хранится только в catalog, не в physical headers.

EdgeEndpoint — engine-owned synchronous capability при первом edge-endpoint, публикуется вместе
с topology; qualified occurrence → connector и source/target side. DEFINE/REMOVE запрещены,
INFO/REBUILD доступны; materialized/physical AtomWalk сохраняют hyper/cross-level semantics.
Прочие derived indexes — immutable snapshot artifacts без построчного сопровождения.

## Порядок и условия готовности

| Шаг | Обязательный результат                                                                           |
|-----|--------------------------------------------------------------------------------------------------|
| D0  | GraphIndex definitions/API/DDL/catalog/jobs и fingerprints                                       |
| D1  | Exact transactional EdgeEndpoint для обеих traversal surfaces                                    |
| D2  | Checked array/record codecs, ordinals/references/reserved bytes, CHAIN_COVER PageType            |
| D3  | NODE/ATOM incidence relation, direction/labels, общий SCC kernel, deterministic condensation CSR |
| D4  | Все пять Reachability algorithms с Yes/No/Maybe и bounded exact fallback                         |
| D5  | Exact/Sketch/Landmark Neighbourhood, переиспользуемый mergeable HLL                              |
| D6  | Явно approximate tv::kHopEstimate, exact обычные kHop/count/enumeration                          |
| D7  | Directed LANDMARK_SSSP и ALT/A* с сохранением path constraints/canonical ties                    |
| D8  | Exact PATTERN_CACHE binding rows и canonical descriptors                                         |
| D9  | Dependency invalidation/private writes/old snapshots/revision-checked publication                |
| D10 | MATCH/traversal consumers, compatibility, EXPLAIN/PlanHash/counters                              |
| D11 | Differential/publication/recovery/resource/paired performance gates и отчёт                      |

TREE_COVER только tree families; GRAIL k=2, 1..32; FERRARI ranges=4;
BFL 160 bits (64..4096, кратно 8); CHAIN_COVER требует MAX_BYTES. GRAIL/BFL возвращают No
при провале фильтра, иначе Maybe; прохождение не доказывает Yes. SCC identity даёт Yes.
AUTO выбирает по projected bytes: tree, CHAIN_COVER, FERRARI, BFL, ON_DEMAND. Explicit превышение
budget — resource error. Graph-local artifact не применяется к cross-level запросу.

Neighbourhood k=1 использует adjacency. Materialized EXACT/SKETCH/LANDMARK только k≤4,
с обязательным MAX_BYTES (pages+metadata). Explicit k>4 отклоняется, AUTO выбирает ON_DEMAND.
EXACT хранит distinct members по радиусам; LANDMARK дополняет traversal оставшимся радиусом.
HLL p=12 содержит 4096 registers, merge — registerwise max. SKETCH используется только planner
и tv::kHopEstimate; без совместимого Ready sketch estimate считает exact traversal.
Seed inclusion и обещанный порядок существующего API сохраняются. Scratch имеет отдельный budget.

LANDMARK_SSSP: deterministic selection, directed from/to distances; hop u64,
weighted/custom outward-rounded intervals. ALT lower bounds admissible, улучшенные states
переоткрываются; depth/trail/simple constraints и canonical ties сохраняются.
Versioned PURE additive COST допускает artifact; stateful COST остаётся ON_DEMAND.
Локальный MATCH `COST fn::name` принимает unary PURE COST либо `(edge, prefix:number)`;
вторая возвращает вклад hop с exact prefix текущего segment и не materialize-ится.
Scoped weighted traversal использует общий conservative numeric completion bound и pending
terminal rows; qualified occurrences одного RecordId в разных graph scopes не объединяются.
Global MATCH WITH COST задаёт default linear segments; fixed/variable local overrides
используют один PURE evaluator с qualified connector scope. Fixed stateful segment начинает prefix с нуля.
Cost identity/version/stateful contract включён в physical PlanHash encoding version 5.
`path::cost(path, fn::name)` повторно считает qualified connector contributions через общий
PURE resolver после origin/snapshot/membership validation. Stateful prefix относится ко всему
Path; scalar reduction потоковый и использует query expansion/cancellation/SSI budgets.
TRAVERSE WITH COST применяется к direct navigation и custom weighted seed operations.
Unary additive Ready binding требует NODE/OUT/function-version без labels; stateful search
сохраняет full path states и применяет mode/filters. OUT сохраняет undirected incidence.
Seed-local NODE domain не расширяется в TRAVERSE ATOM. Resource error не выдаёт partial path.

PATTERN_CACHE: bounded parameter-free patterns, PURE versioned dependencies; alpha-normalized
canonical descriptor включает literals/mode/graph/functions. Hash collision проверяет полные bytes.
Хранятся exact binding rows, multiplicity/named paths; atom/connector/endpoint сохраняет
{GraphId, GlobalAtomId}. WHERE/RETURN/RETURN GRAPH исполняются обычным pipeline, typed executable
IR не сериализуется. Unbounded/parameterized/stateful patterns отклоняются до publication.

Topology/membership/labels/relevant properties/weights/schema/functions атомарно делают definition
Stale. Private mutation немедленно исключает artifact; старый snapshot сохраняет старый binding.
Worker проверяет definition/dependency revisions под commit lock перед Ready publication.
Reachability Yes не заменяет построение пути или bounded-length proof; Neighbourhood не заменяет
path bindings. Недоступные/stale/incompatible artifacts сохраняют exact fallback.

## Gates

Generated small graphs: all-pairs против exact traversal для пяти algorithms, NODE/ATOM,
OUT/IN/BOTH, cycles/disconnected/hyperedges/edge-endpoints/cross-level. ALT vs BFS/Dijkstra для
hop/zero/weighted/PURE costs, больших integers/float boundaries, bounds/ties.
Cache collision/bag multiplicity/alias renaming/qualified occurrences. Commit/abort/SSI,
private/old snapshots, concurrent DML/build, cancel/overflow, crash до/после publication,
reopen/checkpoint/retirement. Hard caps до allocation/write, malformed pages без panic,
dense/star neighbourhoods и oversized cache без partial result. Раздельные backend targets,
focused checks и полный datastore correctness gate. Paired 10K/100K/1M warm/cold/query/DML/build,
latency/page reads/expansions/WAL/peak memory, непрофильные regressions ≤5%.
Обновить specs, Q-31b, AGENTS и graphify; evidence report — `reports/gql_graph_indexes_stage_25_report.md`.
