# Этап 26 — EndpointTensor, аналитика и Rust API

**Статус:** реализация; этап не закрыт. Нормативный контракт принят пользователем 2026-10-02.

## 1. Контракт и границы

Зависимости: этапы 16–17; versioned PURE COST использует этап 21. Незавершённые
общие gates 24–25 не блокируют запуск. Сохраняются GraphRead, parse → analyze → IR →
execute, snapshot/SSI, Value/RecordId/AtomOccurrenceRef/Path. Development layout
изменяется на месте по Q-40, без compatibility decoders.

Публичные runtime поверхности: `graph::tensor`, `graph::analytics`. Все views,
алгоритмы и cache maintenance доступны из Rust без GQL. Storage DTO, pages,
GlobalAtomId и сторонние matrix types не входят в public API. Новые dependencies,
worker, function registry и WAL не вводятся.

## 2. Представления

- EndpointTensor: atom × connector × Source/Target, binary membership.
- IncidenceTensor: NODE-only borrowed view; IncidenceMatrix: Source/Target/Both.
- Binary и weighted adjacency разделены; weighted cells сохраняют Number.
- VV/VE/EV/EE — borrowed block views. Shape, axis dictionaries, checked cells,
  ordinal lookup, nonzero iteration, CSR и bounded dense export доступны публично.
- Standalone GraphRead без GraphRef использует runtime axis identity {Uuid, AtomRef};
  datastore использует существующий AtomOccurrenceRef. GraphId не синтезируется.
- Undirected endpoints устанавливают обе стороны; bidirectional direction и
  ordered endpoint metadata сохраняются. Adjacency использует endpoint kernel,
  не слепое произведение planes. Каждый connector даёт один вклад на допустимую
  endpoint pair; parallel connectors остаются разными. Реальный undirected loop
  требует repeated endpoint, а не просто попадания в обе tensor planes.
- Weighted zero/cancellation не удаляет binary adjacency. Missing weight = 1.
- NODE включает node endpoints, ATOM также edge endpoints. Connector не становится
  промежуточным vertex автоматически. Hyperedge pairs ограничены budgets.
- LOCAL по умолчанию: columns принадлежат local graph; boundary rows сохраняются,
  но исключаются из local analytics domain. ROOTED включает containment closure
  и cross-level incidence. Containment сам по себе не создаёт arcs.
- Canonical axis ordering сохраняет qualified identity; один base atom в разных
  GraphId не схлопывается. Shared child GraphId читается один раз; containment
  descriptors сохраняют разные owner paths.
- Пустые axes допустимы. Invalid ordinals/origin/snapshot/data дают typed errors.

## 3. Алгоритмы

| Семейство          | Контракт                                                                                                                |
|--------------------|-------------------------------------------------------------------------------------------------------------------------|
| PageRank           | Sparse power iteration, damping .85, uniform teleport/dangling redistribution; nonnegative finite strengths, sum=1      |
| Degree/strength    | IN/OUT/total, raw и normalized; parallel connectors и doubled undirected self-loop                                      |
| Harmonic closeness | sum(1/d)/(n−1), unreachable=0; hop или strictly positive weighted distances                                             |
| Betweenness        | Exact all-source Brandes, parallel shortest-path multiplicity, positive weighted costs, checked path counters           |
| Eigenvector        | Uniform start, shifted sparse iteration, L2 normalization, original eigenproblem residual                               |
| Katz               | x=α Aᵀx+β, β=1, default α=.5/max(1,κ), explicit ακ<1 по conservative norm bound                                         |
| Triangles          | Undirected, directed cycles/transitive; binary topology, self-loops excluded                                            |
| Clustering         | Local/mean/global transitivity; directed Fagiolo и normalized weighted extension; zero denominator=0                    |
| LPA                | IN/OUT/BOTH votes, default BOTH; canonical sequential sweeps, retain current maximal label on ties                      |
| Louvain            | Local moves/aggregation, undirected/directed modularity, γ=1, canonical community identity; no global-optimum promise   |
| All-shortest       | SDK/MATCH/TRAVERSE hop/weight/additive/stateful COST; exact Number ties, constraints, canonical order, bag multiplicity |

PageRank/eigenvector/Katz: tolerance 1e-10, 1000 iterations. LPA: 1000 sweeps.
Louvain: 100 sweeps/level, 64 levels. Exhausted convergence limit = typed error.
Empty domain = empty scores/communities and zero counts. No sampling substitutes
ordinary exact metrics. Iterative scores use f64; topology/counts/distances retain
their exact domain and checked arithmetic.

All-shortest defaults to TRAIL. Zero costs are allowed; ALL WEIGHTED SHORTEST WALK
requires finite depth. Stateful COST is full-state ON_DEMAND. Existing ONE shortest
and Stage-17 algorithms retain behavior. Landmarks supply only compatible bounds.
Fallible enumeration reports failure; collect_all returns complete result or error.

Defaults: 256 MiB scratch, 100M work, 1M enumerated paths, available to SDK/query
callers. Admission precedes allocation/write; work/cancellation is checked during
build, refinement, iteration and enumeration.

## 4. Durable cache

GraphIndexDefinition::Tensor / GraphIndex::new_tensor reuse IndexId/version/state,
definition fingerprints, maintenance worker and job targets. Full ATOM incidence
is cached; NODE is a view. Formats CSR/DENSE/AUTO; MAX_BYTES required. AUTO chooses
the smaller admitted encoding, otherwise resource error. Query auto lookup falls
back exact; forced incompatible/stale generation errors before row pull.

TensorCache FileKind/PageType remain the existing constants. Header page zero:
descriptor/version/fingerprint, shape, checked section roots, axes and dependency
revision vector. Data includes Source/Target membership and original topology
metadata. Dense has two row-major LSB-first bit planes; CSR has checked offsets
and sorted distinct ordinals. Canonical integer/Number bytes use existing codecs.
Reserved bytes, tails, counts and references are validated before allocation.

Budget includes pages/dictionaries/metadata; scratch/temporary space are separate.
Build uses bounded external sort, streamed pages, existing WAL/COW and commit-lock
dependency recheck. Membership/topology/containment/relevant labels and captured
weights/properties invalidate atomically; checkpoint LSN does not establish validity.
Private writes exclude old cache; old snapshots retain their binding. Recovery,
cancellation and retirement use existing lifecycle machinery.

## 5. Срезы и gates

| Срез | Результат                                                                            |
|------|--------------------------------------------------------------------------------------|
| T0   | Normative sync, scan/DML baselines, rule-to-test matrix                              |
| T1   | Qualified LOCAL/ROOTED incidence, common endpoint kernel and bounded CSR             |
| T2   | All tensor/matrix Rust views/access/export; direct SDK tests                         |
| T3   | Degree/PageRank/closeness/Brandes/eigenvector/Katz                                   |
| T4   | Triangles/clustering/LPA/undirected+directed Louvain                                 |
| T5   | Shared ONE/ALL shortest kernels; full-state costs/constraints                        |
| T6   | Tensor definition and checked dense/CSR codecs                                       |
| T7   | Snapshot publication/invalidation/recovery/retirement                                |
| T8   | GQL grammar→AST→analysis→IR→execution and traversal adapters                         |
| T9   | Planner/EXPLAIN/PlanHash/counters                                                    |
| T10  | Full correctness/resources/paired performance; report/specs/AGENTS/registry/graphify |

Следующий срез начинается после correctness gate предыдущего. Отчёт хранится в
`docs/reports/gql_tensor_analytics_stage_26_report.md`; отчёт отделяет passed,
pre-existing и OPEN gates. API examples покрывают каждую поверхность без GQL.
Differential oracles покрывают NODE/ATOM, directions, loops/multiedges/hyperedges,
qualified scopes, same base in different graphs, numeric/convergence boundaries,
ALL paths, snapshot/SSI/crash/cancellation и malformed pages без panic.

Performance: paired 10K/100K/1M для scalable kernels, warm/cold/cache/mutation/rebuild;
superlinear families отдельно измеряют admitted domain и budget rejection. Metrics:
latency/page reads/work/WAL/peak memory; unrelated query/DML regression ≤5%.
RAM/local Sync/NativeAsync — отдельные integration targets.

После срезов: focused tests, cargo check --lib --no-default-features, nightly fmt
и Clippy. Closure: full datastore gate и affected integrations. Перед commit:
just all-check с сохранением unrelated dirty work. OPEN gate оставляет этап открытым.

## 6. ProfileStack текущего среза

T0 normative contract: owner specs. T1–T2 caller contract: owner rust-api-design;
CSR/bit-plane layout: owner rust-data. Разные decisions одного представления;
durability, concurrency, GQL и algorithms пока deferred. Rules: api-common-traits
(immutable owned public values), api-default-impl (options), api-parse-dont-validate
(checked constructors), num-overflow-explicit (shape/offset/budget arithmetic).
Coverage: own/public types → API owner; borrowed views → API owner; sparse arrays
и memory/work admission → data owner. Acceptance: public SDK differential tests,
compile/rustdoc, check/fmt/Clippy. Gaps: none for T1–T2.

T3: public parameters/results → rust-api-design; distance state and sparse
reductions → rust-data; overflow/nonconvergence → rust-errors. Rules remain
api-common-traits, api-default-impl and num-overflow-explicit. Pure integer
positive costs admit Dijkstra/Brandes. Other Number representations use bounded
full-state vertex-acyclic enumeration: unequal prefixes may round to equal final
costs. Centrality uses projected vertex-acyclic paths; a connector may contribute
to several projected pairs in a path. The All-shortest default TRAIL is a separate
binding contract. No epsilon or prefix dominance is introduced. A positive addition that cannot advance its
Number distance produces a typed numeric error. Exponential work and quadratic
Brandes fallback scratch are charged before work/allocation; no partial success.

T4: motif/community parameters and owned qualified results → rust-api-design;
sparse intersections, community reduction and aggregation → rust-data;
numeric/convergence failures → rust-errors. Dense multiplication is deferred
entirely; no dependency is needed. Acceptance: independent motif/modularity
oracles, direction/weight/loop tests, budgets and direct SDK/rustdoc examples.

T4 numeric fit: gamma=1 и целочисленные strengths с exact total <=2^53 допускают
точные checked i128 products. Остальные uncertain Louvain gains проверяются
через уже подключённый num::BigRational после admission scratch. Floating scores
только ранжируют доказанно положительные candidates; округлённый 0 не означает
convergence. Общий рациональный numeric framework не вводится.

T5: owned logical paths и существующий Path adapter → rust-api-design;
bounded full-state enumeration и безопасный hop lower bound → rust-data;
numeric/admission failures → rust-errors. Standalone runtime identities остаются
TensorAtom; материализация Path принимает явные GraphRef, как существующий
ShortestPaths::materialize_path. Datastore paths используют реальные occurrences.
Stateful COST не получает prefix dominance или predecessor DAG.

T6: public tensor-cache definition → rust-api-design; explicit header/section bytes,
canonical metadata и aggregate decode admission → rust-serialization. Existing derived
ArraySection/page checksum/extent codecs принимаются по fit: bounded streaming, canonical
roots и final aligned destination уже используются тремя graph artifact families.
Полная metadata проверяется против incidence oracle без hash-only trust. No new dependencies.
T6 focused correctness gate пройден; общий closure gate остаётся открытым.

T7: publication/invalidation protocol → rust-concurrency; physical binding and
recovery validation → rust-serialization; automatic/forced snapshot API → rust-api-design.
Existing coordinator owns one bounded FIFO worker, cancellation and shutdown.
Candidate captures immutable root вне commit lock; worker проверяет тот же commit revision
под lock, затем публикует catalog + physical files + immutable runtime artifact одним COW.
Private writes инвалидируют dependencies до reads; old roots удерживают прежние Arc generations.
Lock order и durable cancellation/retirement сохраняются существующими maintenance/WAL paths.
Новых atomics, threads, queues или async runtimes нет; concurrency rulebook overlay
не требуется для новых constructs. Incidence plane packing → rust-data: существующий
ExternalIndexSorter заменяет resident pair sort; callbacks сохраняют dependency direction
graph → runtime, datastore → storage. Dictionaries/topology остаются bounded resident
scratch; scaling ceiling проверяется в T10.

T8: grammar/AST/typed IR и закрытый vocabulary steps → specs; qualified snapshot
adapter и resolved result options → rust-api-design; induced projection, ALL branch/
terminal hooks и bounded retained buffers → rust-data; statement-wide atomic
work/path admission → rust-concurrency; numeric/resource diagnostics → rust-errors.
rust-design-protocol использован только для discovery MATCH через layers. Общий
kernel фильтрует branches/terminal до minimum; ONE cursors сохраняются. GQL использует
обычную versioned PURE invocation и существующие MATCH predicates, без второй registry.
Rules: api-parse-dont-validate, api-default-impl, num-overflow-explicit. Failed graph
invocations удерживают worst-case work admission; unused gas возвращается только
после полного успешного результата, включая nested controls. Новые dependencies нет.
Focused correctness T8 пройден; cache planner/observability T9 остаётся отдельным срезом.

T9: logical Ready metadata и generation compatibility → rust-api-design;
physical operator/PlanHash/EXPLAIN contract → specs; shared counters и accounted
scratch → rust-observability. Runtime lookup проверяет выбранный immutable binding
тем же descriptor/dependency validator, что planner, и использует exact fallback.
Полная incidence generation совместима с downstream NODE/ATOM/direction/labels
views; named COST не является tensor cache payload. Нового worker/artifact слоя нет.

T9 rules: obs-structured-fields и obs-no-sensitive-data применяются к существующему
EXPLAIN/counter contract. Counters имеют фиксированные поля без identifiers в labels;
generation metadata остаётся в запросе EXPLAIN, properties и query values не логируются.
Runtime Source/Target packing использует один logical membership selector с двумя
реальными consumers (resident и external sort). Focused correctness T9 пройден.

T10: runtime containment frontier и qualified owner chains → rust-data;
admission/numeric failures → rust-errors; paired workloads и measured ceilings →
rust-performance; final command matrix → rust-verify. Iterative borrowed runtime
walker переиспользует inherent NodeView/EdgeView lifetimes; GraphRead не изменяется,
properties не копируются, новые dependencies/handles не вводятся. Depth задаётся
memory/work budget вместо произвольного порога. Performance claims требуют измерений.

T10 cold recovery finding: потеря LocalOptions maintenance authority → rust-debugging
и rust-concurrency (existing scoped TLS/RAII); duplicate encoding work reservation →
rust-serialization и rust-errors. Используется существующий external-sort options
context, без нового budget API. Sync/Native normal→1 KiB→normal tests проверяют
resource fallback и сохранение durable Ready generation без промежуточных writes.
