# Этап 25 — производные графовые индексы: evidence checkpoint

Дата: 2026-10-02. Статус: **D0–D6 core correctness gates пройдены; D7 реализуется; этап не закрыт**.
Контракт: [план этапа](../gql_graph_indexes_stage_25_plan.md).

## D0 — текущий срез

GraphIndex принадлежит GraphSchema; используются существующие IndexId, SchemaVersion,
ClassIndexState и IndexMaintenanceMode. SDK define_graph_index/remove_graph_index/
overwrite_graph_index проходят обычный private root, FCW/SSI replay и commit publication.
MODE и direction используют существующие TraversalSemantics/TraversalDirection.
Параметры algorithms, radius, labels и обязательные MAX_BYTES проверяются до publication.
Canonical definition fingerprint включает graph/identity/version/projection/direction/
algorithm/cost/labels/budget/maintenance и versioned dependencies; availability и build revision
хранятся отдельно. Материализованный индекс не становится Ready без artifact.

FGDC v10 development layout дополнен graph user index count и checked descriptors без
compatibility decoder. Decoder ограничивает descriptor bytes, labels, dependencies и patterns,
проверяет reserved bytes, UTF-8, canonical ordering и полные descriptor bytes вместе с hash.
Graph-owned durable maintenance target имеет отдельный tag 5; graph-wide target сохранён.
DEFINE/REMOVE/INFO проходят grammar → AST → analysis → IR → execution. REBUILD ON_DEMAND
использует существующий worker/publication. Materialized builders и их publication ещё открыты.

## Полученные проверки

`cargo check --lib --no-default-features --locked --offline` после SDK/catalog — pass.
`test-graph-definitions.log`: graph definition parameter/fingerprint unit check, существующие
graph-index storage tests и public local Sync integration — pass. Последний проверяет abort,
commit, конфликт двух definitions, old snapshot, reopen, stable fingerprint и removal.
`test-graph-ddl-v5.log`: **24 lib tests pass, 3 diagnostic tests ignored; 1 local Sync integration
pass**. Включены DDL/INFO/REBUILD ON_DEMAND, MODE/direction/labels, overwrite identity/version,
отклонение invalid parameters/budgets и engine-owned DEFINE/REMOVE. Golden descriptor bytes,
все truncated prefixes, reserved bytes, oversized lengths, неверный fingerprint и неканонический
порядок labels отклоняются. Job GraphIndex target tag 5 round-trip и zero owner rejection — pass.
PURE COST dependency mutation в private transaction немедленно делает definition Stale;
CANCEL ничего не публикует, commit обновляет captured function/definition versions, старый snapshot
сохраняет Ready definition, competing graph definition write получает FCW conflict. Function
removal оставляет dependent definition Stale. `correctness-gate-d0.log`: **611 lib tests pass,
10 diagnostics ignored; RAM 4, local 15, geometry Sync 1, graph definitions Sync 1 — pass**.
NativeAsync после D0 выполняется отдельно.

## D1 — EdgeEndpoint

Добавлены lazy qualified endpoint postings в GraphRoot, общая engine-owned identity в
GraphSchema и FGDC binding. Attach, topology replacement, delete/cascade и BUILD GRAPH
публикуют postings вместе с topology. Shared Unique B+tree/streaming writer переиспользован
с concrete Endpoint leaf type, page-0 ownership/revision header и bounded external sorting.
GraphRead отдаёт logical connector identities; materialized MATCH/AtomWalk и physical
local hyperedge frontier используют общий endpoint incidence kernel. Cross-level physical
shape возвращает exact scoped traversal до выдачи partial rows. INFO/REBUILD доступны,
DEFINE/REMOVE engine capability запрещены. Snapshot pinning расширено на EdgeEndpoint.

`check-endpoint-physical.log` — lib check pass. `test-endpoint-kernels.log` — 3 focused tests pass:
direction/mode kernel differential, qualified scope leaf lookup, malformed header/key rejection.
`test-endpoint-lifecycle.log` — 6 focused tests pass, включая private topology replacement/abort,
old snapshot, connector обеих сторон и разные containment paths одного child graph.
`test-endpoint-physical-v4.log` — 7 focused tests pass, включая physical hyperedge frontier
для OUT/IN/BOTH, checkpoint при малом resident pool, old generation и reopen. Выявлены и
исправлены две общие stats delta ошибки: conservative degree maximum теперь ограничен новым
exact total после удаления; topology replacement пересчитывает totals, сверенные с bulk ANALYZE.
`test-native-d1-v2.log` — geometry NativeAsync 1 и EdgeEndpoint NativeAsync 1 pass.
Первый Native прогон обнаружил устаревший captured checkpoint ceiling: pinned reader после
passive backfill/prune теперь ограничивает home page LSN своим snapshot.visible_lsn. Transient
Direct/IoRing routes проверяют тот же ceiling. `correctness-gate-d1-lib.log` — **616 lib tests
pass, 10 diagnostic tests ignored**. `correctness-gate-d1-backends.log`: RAM 4, local 15,
geometry Sync 1, graph definitions Sync 1 — pass. Nightly fmt owned Rust files pass
(`fmt-d2-v2.log`); первая попытка наткнулась на временные Windows mapped-file handles,
повтор после завершения компилятора прошёл. Clippy остаётся общим открытым gate.

## D2 — общие section codecs

Добавлены нормативные PageType 0x003E/0x003F с FID v1 magic и renamed CHAIN_COVER 0x002B.
Array/record views заимствуют payload, не выделяют память из on-disk count. Проверяются
family/kind/width, counts/used bytes, offset directory, checked ordinal end, forward extent
links, flags и весь reserved tail. Chain visitors удерживают одну aligned page, проверяют
ordinal continuity и exact total; пустая секция использует NULL root. Ref bounds проверяются
отдельно в контексте captured extent.
`test-derived-d2.log` — 3 focused tests pass: все нормативные widths, golden header/directory,
пустые records, oversized/malformed metadata, reserved bytes, reference bounds, ordinal overflow,
truncated chains и backward links. `check-d2.log` — lib check pass. `clippy-d2.jsonl` фиксирует
6972 errors / 434 warnings общего дерева; единственное новое codec finding
`chunks_exact_to_as_chunks` исправлено. Повторная проверка changed slice ещё выполняется.

## D3 — incidence relation и общий SCC

Добавлен bounded projection adapter над текущим GraphRoot: exact connector label filter,
NODE/ATOM и OUT/IN/BOTH проходят общий incidence kernel. Vertex order и component numbering
используют stable GlobalAtomId; snapshot-local AtomId остаётся только handle в том же graph.
Qualified endpoints не схлопываются, artifact отмечает cross-level shape для exact fallback.
Condensation CSR содержит distinct sorted arcs и deterministic Kahn order.
Scratch admission до роста структур использует conservative bound
`1024 + 512*vertices + 128*incidences` bytes, отдельно от artifact MAX_BYTES. Это admission
estimate, не измеренный RSS. Нехватка scratch и cancellation возвращают typed error.

Выделенный iterative Kosaraju kernel используется также существующим public NODE-only SCC.
До extraction регрессионный граф A→B, A→C, B→C ошибочно объединял B/C: DFS отмечал queued
siblings visited до завершения активной ветви. `test-scc-repro.log` фиксирует failure (2 вместо 3),
kernel теперь хранит итератор активного DFS frame и завершает дочернюю ветвь перед следующей.
Public API не изменён. `test-relation-d3-v5.log` — **11 traversal tests pass**, включая 64 generated
graphs × 2 filters × 2 modes × 3 directions, all-pairs SCC differential, permutation invariance,
hyper/edge incidence, disconnected atoms, qualified fallback, resource/cancellation checks.
`test-relation-root-d3.log` — snapshot adapter test pass: actual definition labels/direction/mode
и GlobalAtomId order, отличающийся от local slot order.
`correctness-gate-d3-lib.log` — **623 lib tests pass, 10 diagnostics ignored**.
`fmt-check-d3.log` — nightly check owned Rust files pass. D1/D2 additions к исходному Clippy
baseline исправлены; окончательный Clippy срез проверяется повторно.

## D4 — Reachability artifacts

Реализованы TREE_COVER, GRAIL, FERRARI, BFL и CHAIN_COVER над общей SCC condensation.
TREE_COVER проверяет tree family и направление forest relation. GRAIL/BFL дают только No/Maybe
для разных SCC; FERRARI сохраняет exact/approximate tags. Maybe уточняется bounded exact DFS,
с cancellation и отдельными expansion/scratch caps. AUTO сравнивает полную page projection,
включая карты, CSR, labels, canonical descriptor и header; explicit overflow возвращает error.
GRAIL использует deterministic dimensions; FERRARI применяет greedy gap compression,
описанную авторами как допустимый вариант
[FERRARI](https://arxiv.org/pdf/1211.3375). Optimal interval dynamic programming не реализована;
её необходимость должна подтверждаться refinement workload.

Immutable artifact публикуется существующим maintenance worker после commit-lock revision
recheck. Private membership/topology/edge-label mutation делает свои definitions Stale и удаляет
artifact из private root; старый Arc snapshot сохраняет labels. Base/overlay property mutation
без изменения relation не инвалидирует Reachability. Neighbourhood использует тот же relation
invalidation в D5; Path dependency сопровождение ещё не реализовано. Local generation сохраняет graph-owned binding в FGDC, переиспользует
совместимый immutable файл при независимом commit и загружает checked sections через snapshot
PageManager при reopen. Полные descriptor bytes проверяются наряду с fingerprint.
Недоступный/повреждённый secondary становится Stale; partial artifact не публикуется.

`test-reachability-d4-v11.log`: **9 focused tests pass**. Проверены all-pairs для 96 generated
graphs × NODE/ATOM × OUT/IN/BOTH и всех пяти algorithms, extreme parameter values, tree families
в обеих ориентациях, precise Yes/No/Maybe, exact fallback и resource/cancellation errors.
Page tests проверяют exact MAX_BYTES, full canonical packing, malformed ownership/roots/counts/
reserved bytes/maps/CSR/representatives, descriptor collisions и admission до disk allocation.
Regression фиксирует последовательную запись страниц с page 0: первый local прогон обнаружил
ошибку writer, пытавшегося начать расширение Direct-I/O файла со страницы 1.

`test-reachability-local-d4-v2.log`: **2 local Sync integrations pass**; все пять algorithms
публикуются и переживают reopen, NODE/ATOM и разные graph owners сохраняются.
Focused local test также проверяет четыре prepublication failure points, отсутствие unreachable
generation/root publication, сохранение предыдущего Ready binding при failed rebuild, неизменный
DataFileId при независимом commit и Stale после checksum corruption/reopen.
`correctness-gate-d4-lib.log` — **633 lib tests pass, 10 diagnostics ignored**.
`correctness-gate-d4-backends.log` — RAM 4, local 15, geometry Sync 1, graph indexes Sync 2 pass.
`test-native-d4-v2.log` — geometry NativeAsync 1, graph indexes NativeAsync 2 pass, включая
publication/checkpoint/abort/reopen. `check-d4-v3.log` и owned nightly fmt — pass.
`clippy-d4-v3.jsonl` — 6974 errors / 450 warnings дерева; сравнение с исходным stage-24 baseline
не показывает новых error signatures, но число повторений старых diagnostics выросло.
Новые warnings включают ещё не подключённые D10 consumers, complexity и impl-trait style;
общий Clippy gate остаётся открытым. После исправлений focused reachability повторён:
`test-reachability-d4-v12.log` — 9 pass. D4 core correctness подтверждён; query/performance
доказательства остаются частью D10/D11.

## D5 — Neighbourhood artifacts

HLL вынесен из ANALYZE в `math::HyperLogLog`: прежний p=12 hash/rounding сохраняется,
precision 4..18, fallible allocation, checked restored registers и registerwise-max merge.
Coefficients для 16/32/64 registers и small-range correction сверены с
[первичной публикацией HLL](https://algo.inria.fr/flajolet/Publications/FlFuGaMe07.pdf).
Это переиспользование существующего kernel; новая dependency не добавляется.

EXACT сохраняет distinct members для каждого центра и радиуса, включая seed. SKETCH
propagates отдельные sketches по радиусам; LANDMARK хранит exact sets для deterministic
degree-selected centers и дополняет traversal cached members для оставшегося радиуса.
Cached membership не подавляет последующее обычное посещение той же вершины по более короткому
пути. MAX_BYTES считает полные packed pages и common metadata, scratch отдельно ограничивает
живую relation, workspace и overlap при росте member buffer. Explicit overflow — typed error;
AUTO fallback применяется только по artifact/degree policy. k=1 и большие AUTO радиусы используют
adjacency/ON_DEMAND. Query consumers ещё открыты.

Immutable header и восемь sections сохраняют canonical vertex map, general CSR, centers,
radius offsets, members/registers и full definition bytes. Status остаётся в catalog.
Общий `derived_index::ArraySection` переиспользуется Reachability и Neighbourhood для
полных contiguous pages, resource admission, stream/visit. Restore проверяет живые occurrences
того же snapshot, general CSR, sorted distinct sets, seed inclusion, monotone radii и HLL ranks.
Публикация использует существующий maintenance/WAL/COW writer; независимый commit сохраняет
DataFileId, relation mutation исключает private artifact и сохраняет старый Arc snapshot.
Повреждённый optional secondary при reopen становится Stale, base data остаются доступны.

`test-hll-d5.log` — 1 test pass: merge равен sketch точного distinct union, precision boundaries,
4096 registers для p=12, register round-trip и invalid precision/length/rank rejection.
`test-neighbourhood-d5.log` — 2 tests pass: 16 generated graphs × NODE/ATOM × OUT/IN/BOTH,
все centers/radii 1..4, EXACT/LANDMARK vs независимый minimum-hop oracle, SKETCH register bytes
vs exact member union, dense hyperedge, budgets/scratch/cancellation и seed at radius zero.
`correctness-gate-d5-kernels.log` — 636 lib tests pass, 10 diagnostic tests ignored после HLL
extraction. `test-neighbourhood-codec-d5-v2.log` — 4 focused tests pass: kernels и checked page
codecs всех трёх algorithms, exact file length, последовательная запись с page 0, malformed
headers/sections и scratch rejection до чтения.
`test-neighbourhood-artifact-d5-v4.log` — 5 focused tests pass: 36 snapshot artifacts для
EXACT/SKETCH/LANDMARK × NODE/ATOM × OUT/IN/BOTH × empty/nonempty graph, section round-trip,
malformed payloads, AUTO budget fallback, cancellation, private invalidation/abort, старые
snapshots и сохранение Arc при base/overlay property mutation. Тест выявил отсутствие cancellation
при empty restore; добавлена проверка на входе restore/shared CSR validation.
`correctness-gate-d5-lib.log` — **640 lib tests pass, 10 diagnostic tests ignored**. General CSR
restore принимает cycles/self arcs и отклоняет malformed references/offsets/duplicates; local
failure/reuse/corruption test теперь проходит Reachability и три Neighbourhood algorithms.
`correctness-gate-d5-backends.log` — RAM 4, local 15, Geometry Sync 1, graph artifacts Sync 2
pass; `correctness-gate-d5-native.log` — Geometry NativeAsync 1 и graph artifacts NativeAsync 2
pass. CHECK/no-default-features и nightly owned-file fmt/check прошли.
`clippy-d5-publication.jsonl` — 6970 errors / 454 diagnostics уровня warning (включая macro crate);
сравнение с D4 baseline не добавляет error signatures. Новый unused import перенесён в tests;
три dead-code warnings относятся к будущим D10 consumers. Общий Clippy gate открыт.
`graphify-update-d5.log` — AST-only update: 54248 nodes, 144128 edges, 1835 communities.
D5 core correctness подтверждён; query/performance evidence остаётся в D6/D10/D11.

## D6 — явно приблизительный count через общий pipeline

`tv::kHopEstimate(k)` проходит analysis → typed IR → physical traversal planner → adapter.
Planner запрашивает sketch metadata только для этого шага и выбирает Ready unfiltered OUT
SKETCH с совпадающим MODE и достаточным radius. При нескольких bindings предпочитает p,
затем bytes/IndexId. Graph/index/version/build revision/projection/p/bytes участвуют в PlanHash
и EXPLAIN. Executor проверяет captured binding; другого индекса скрыто не выбирает.
Без binding строит bounded local incidence projection и считает distinct members exact,
включая seed. k=0 возвращает 1. Private mutation исключает artifact, старый snapshot сохраняет
его; unrelated property mutation сохраняет binding. Обычный kHop/BFS и counts sketch не читают.

Exact projection/traversal ограничены текущим statement scratch и отдельным cumulative work
ceiling `max_path_expansions`. При исчерпании результат error, не partial count. Cancellation
проверяется до чтения binding и в точном kernel. ANALYZE показывает sketch reads, exact
fallbacks/work, consulted artifact bytes. Peak accounted memory отражает scratch admission
reservation; это не измеренный RSS. Новый шаг сохраняет bulk multiplicity.

`test-estimate-d6-v3.log` — 3 focused tests pass: compatible/incompatible bindings, p selection,
NODE/ATOM, k=0..5, BFS order, multiplicity, invalid types/radii, scratch/work/cancellation,
shared work cap, private invalidation/abort, old snapshot и SSI conflict.
Первый прогон выявил reset bulk у нового шага; исправлен до полного gate. Ошибочные тестовые
ожидания ATOM уточнены по существующей incidence projection: connector не становится
промежуточной вершиной без edge-endpoint связи.

`correctness-gate-d6-final-lib.log` — **643 lib tests pass, 10 diagnostic tests ignored**.
`correctness-gate-d6-backends.log` — Geometry Sync 1, graph artifacts Sync 2, RAM 4, local 15
pass; после demand-only metadata change `correctness-gate-d6-final-sync.log` — graph artifacts
Sync 2 pass. `correctness-gate-d6-native.log` — Geometry NativeAsync 1 и graph artifacts
NativeAsync 2 pass. Backend tests исполняют estimate после reopen, private invalidation и в old
snapshot; NativeAsync также после passive checkpoint. CHECK/no-default-features прошёл.

Read-only review D6: typed errors/checked sizes, immutable snapshot binding, dependency
direction и cancellation/resource paths проверены; blocking correctness findings не выявлены.
Правила `num-overflow-explicit`, `err-result-over-panic`, `err-no-unwrap-prod` применены к новым
веткам. Обнаруженный ненужный сбор sketch metadata в непрофильных plans ограничен PlannerDemand.
Clippy и performance evidence фиксируются отдельно; этап не закрывается по focused tests.
`clippy-d6-last.jsonl` — 6970 errors / 446 warnings с macro crate: число errors совпадает с
D5 baseline; `clippy-d6-last-diff.log` — ноль новых signatures. Новые visibility/wildcard lints
исправлены, большой ANALYZE observation передаётся по ссылке. Owned nightly fmt/check прошли.

## D7 — directed landmarks, immutable PATH publication и MATCH ALT

Добавлено общее ядро directed landmarks над NODE/ATOM CSR. Deterministic farthest-point
selection использует canonical ordinal, OUT/IN distances и стабильные ties; disconnected
vertices выбираются первыми. HOP distances сохраняются как u64; WEIGHT/PURE COST как
outward-rounded lower/upper intervals. Недостижимая distance не участвует в ALT subtraction.
Large integers/Decimal преобразуются с проверкой enclosure; zero/tiny positive weights
сохраняют monotone relaxation. Формулы directed ALT сверены с
[Goldberg/Harrelson, Computing the Shortest Path: A* Search Meets Graph Theory](https://www.microsoft.com/en-us/research/publication/computing-the-shortest-path-a-search-meets-graph-theory/).

PATH page 0 и пять shared array sections хранят canonical DiskAtomRef vertex map,
landmark ordinals, from/to distance cells и полный definition descriptor. Header magic FIP,
page type 0x0030; status остаётся в catalog. Requested/actual landmarks, MODE/direction,
cost function/version, revision, roots, extents и reserved bytes проверяются перед decode.
Full-page MAX_BYTES учитывает page 0 и metadata. Общий bounded section loader теперь
переиспользован Reachability/Neighbourhood/PATH. Sealed AlignedPage остаётся на стабильном
адресе; streaming/WAL/COW writer и локальное recovery переиспользованы.

Worker строит HOP/WEIGHT/PURE COST generations и обновляет transitive function dependencies
через существующий analyzer перед публикацией. Parallel connectors сворачиваются по least
exact compatible cost. PURE COST получает owned occurrence identity и effective fields из
того же immutable root; nested named calls сохраняют resolver. Weight mutation делает WEIGHT
Stale; ordinary property mutation сохраняет HOP/WEIGHT, консервативно инвалидируя PURE COST.
Abort ничего не публикует, старый snapshot удерживает прежнюю generation.

`test-landmark-d7.log` — 5 focused tests pass, включая independent all-pairs oracle,
admissibility/enclosure на generated directed graphs, large integers, floating extremes,
cycles/zero edges, cancellation и budgets.
`test-path-d7-v5.log` — 5 focused tests pass: 12 MODE/direction/cost projections,
empty/multi-page codecs, malformed headers/sections, complete-page/scratch caps,
snapshot/abort/weight invalidation, PURE COST overlay и nested calls. После смены body rebuild
захватывает новую transitive dependency; её отрицательный return завершает job ошибкой без
partial Ready generation. `correctness-gate-d7-lib-v1.log` — 652 pass / 10 diagnostic ignored
до подключения HOP consumer.

HOP `LandmarkShortest` выбирает compatible Ready MODE/direction/label binding. Adapter отдаёт
только logical metadata и Number bound, сохраняя SSI relation reads. Heap ранжирует полные
path states по admissible cost; bounded depth и uniqueness не теряются, улучшенные prefixes
не отсекаются visited-set. Все states с bound ≤ best cost проходят exact refinement до
выдачи canonical result. Unbound targets используют прежний exact cursor. Scoped/cross-level
paths artifact не читают. EXPLAIN JSON и PlanHash включают captured generation, mode/cost,
landmarks/bytes, refinement/fallback; ANALYZE считает lookups/fallbacks/bytes и expansions.

AtomWalk диапазоны используют общий parser/analysis/PathExpand. Intermediate kind не
ограничивается final binding: пути через edge endpoints проходят NODE/ATOM incidence kernel.
`test-d7-alt-v4.log` — 2 differential tests pass: public atoms/cost/length, all pairs на cyclic
graph, NODE/ATOM, bounds, WALK/TRAIL/ACYCLIC/SIMPLE, canonical ties, edge endpoints, snapshot/
private invalidation/fallback и меньшие expansions на detour workload. Последующий полный
gate дополнительно проверяет BOTH/undirected/bidirectional topology.

`check-d7-alt-v2.log` — lib/no-default check pass; owned nightly fmt прошёл.
`correctness-gate-d7-lib-v2.log` — 654 pass / 10 ignored, один recovery fixture failure до
создания PATH. Его isolated повтор `test-d7-maintenance-recovery-repro.log` прошёл; generic
fixture message дополнен исходными errors для повторного полного gate. Это не объявляется
закрытым correctness gate по isolated pass.

Повторный полный `correctness-gate-d7-lib-v3.log` прошёл: **655 pass / 10 ignored**.
`correctness-gate-d7-backends.log`: Geometry Sync 1, graph indexes Sync 2, public RAM 4,
public local 15 — все pass. `correctness-gate-d7-native.log`: Geometry NativeAsync 1 и
graph indexes NativeAsync 2 — все pass. PATH HOP/WEIGHT generations переживают reopen и
checkpoint; MATCH использует recovered HOP generation в NODE/ATOM, private mutation
выключает lookup. Эти результаты закрывают текущий correctness срез HOP consumer,
а не оставшиеся resource/performance gates этапа.

Последующий срез подключает WEIGHT `LandmarkShortest` к MATCH. Exact terminal rows хранятся
до proof против pending completion bounds; all-target fallback также учитывает zero-weight
ties и смешанные reductions. Для prefix `g`, exact suffix lower `h` и оставшегося радиуса `r`
используется downward-rounded `(g_lower + h_lower) * max(0, 1 - 3r * EPSILON)` с вычетом
`3r` smallest subnormals. Три события на шаг покрывают binary64 casts/addition; integer
addition exact, 96-bit Decimal addition имеет меньшую relative loss. Bernoulli inequality
даёт conservative bound. Этот bound не clamps к `g`: Int `9007199254740993` плюс Float `0`
даёт Float `9007199254740992`. Float и casting-exact/upward Int prefixes дополнительно
позволяют безопасный canonical dominance; остальные проходят общий bound proof.
Правила casts проверены по [Rust Reference](https://doc.rust-lang.org/reference/expressions/operator-expr.html#numeric-cast),
Decimal addition — по установленному `rust_decimal 1.43.0/src/ops/add.rs` и
[исходному коду проекта](https://github.com/paupino/rust-decimal/blob/master/src/ops/add.rs).

`test-d7-weighted-v3.log`: 12 focused tests pass, включая 1152 independent bounded NODE
oracle comparisons до/после Ready, zero costs, >2^53 integers, float/Decimal boundaries,
all-target fallback и факт выбранного WEIGHT binding. Расширенная NODE/ATOM/direction матрица
и numeric-bound property check вошли в `correctness-gate-d7-lib-v4.log`: **659 pass / 10 ignored**.
3076 paired NODE/ATOM queries покрывают HOP/WEIGHT, OUT/IN/BOTH, bounds, четыре path modes,
edge endpoint terminals и canonical ties. Numeric-bound check сравнивает bounded runtime
reductions до 30 шагов для 1000 комбинаций prefix/weights: i64::MAX, >2^53, smallest
subnormal, MIN_POSITIVE, MAX binary64 и Decimal::MAX/minimal scale. Overflow/incompatible
numeric paths возвращают прежний typed error.

PURE builders теперь наследуют maintenance cancellation во всех typed expressions, nested
calls и child blocks. Retained cache и live compilation frames делят caller-owned cap;
conservative source admission выполняется до source/token/AST allocation, guard освобождается
на error/cancellation. Runtime slot frame и captured/argument clones проходят admission до
allocation. PATH build учитывает отдельную долю evaluator scratch в relation/live budget.
`test-d7-pure-resources-v2.log` — 5 pass; `test-d7-compiler-resources.log` — 1 pass:
deterministic cancellation внутри миллионного loop, oversized argument/frame и aggregate
compiler frames. Growing collections, builtin temporaries и compiler peak-memory proof
ещё не покрыты полным hard-cap gate. Compiler source reserve является conservative estimate.
`clippy-d7-core-v2.jsonl`: errors **6970**, как D6 baseline; новые error instances устранены.
Остаётся warning для ещё не используемых PATTERN_CACHE section tags.
`clippy-d7-weighted-v1.jsonl` также даёт **6970 errors** без новых error instances;
owned nightly fmt и lib/no-default check прошли.

`correctness-gate-d7-backends-v2.log`: Geometry Sync 1, graph indexes Sync 2, public RAM 4,
public local 15 — pass. `correctness-gate-d7-native-v2.log`: Geometry NativeAsync 1,
graph indexes NativeAsync 2 — pass. Recovered WEIGHT binding используется после reopen
в MODE ATOM на Sync и MODE NODE на NativeAsync; NativeAsync также проходит checkpoint.

Следующий срез подключил локальный MATCH `COST fn::name` через parser/AST/analysis/IR и
обычный cached versioned PURE evaluator. Unary COST получает qualified connector occurrence
и effective fields; совместимый function/version artifact используется в ALT. Двухаргументный
`(edge, prefix:number)` возвращает вклад текущего hop с exact segment prefix и остаётся ON_DEMAND;
такой materialized definition отвергается и в GQL, и в SDK. Negative/non-finite contributions
дают typed error. Enumeration и shortest path используют одни cost helpers; повторный подсчёт
cost при materialization одного row удалён. Physical plans сохраняют identity/version/stateful
flag и используют PlanHash encoding version 3.

Scoped weighted fallback использует общий numeric bound и terminal-row heap. Нулевые edges
не завершают поиск преждевременно; canonical ties и numeric reductions проходят pending-bound
proof. Graph-local artifact не используется для cross-level; targets остаются qualified
GraphId/AtomId и разные occurrences одного RecordId дают отдельные exact rows. PURE artifact
lookup регистрирует SSI reads всех captured property class ranges и function dependencies:
pruning непросмотренных connectors не исключает dependency tracking.

`test-d7-custom-focus-v3.log` — 8 focused tests pass. Дополнительные 2304 custom COST
comparisons с independent bounded DFS покрывают local/scoped, OUT/IN, четыре path modes,
zero/integer/float/Decimal costs до/после Ready. Ещё 1152 comparisons проверяют scoped WEIGHT.
Stateful prefix check сравнивает enumeration и shortest, проверяет ON_DEMAND/EXPLAIN и
rejection incompatible definitions. Qualified cross-level test возвращает два target scopes
одного RecordId и сохраняет rows после build. Lifecycle test проверяет private overlay/abort,
old snapshots, commit invalidation, transitive UDF alteration/rebuild, PlanHash и SSI conflict.
В предыдущем запуске cross-level fixture ошибочно вызывал REBUILD через Transaction;
исправлено на production Datastore maintenance entrypoint.

Финальный `correctness-gate-d7-lib-v7.log` — **664 pass / 10 ignored**;
`check-d7-custom-v5.log` — lib/no-default-features pass; owned nightly rustfmt и
changed-file `git diff --check` прошли. Stateful/fallback plans дополнительно сохраняют
schema epoch function resolution: смена transitive UDF меняет PlanHash и без artifact;
старый snapshot сохраняет прежний hash. Scoped fallback counter проверен отдельным assertion.

`correctness-gate-d7-custom-backends-v3.log` — graph indexes Sync 2, public RAM 4,
public local 15 tests pass; `correctness-gate-d7-custom-native-v3.log` — 2 NativeAsync
tests pass. Public backend targets строят nested PURE COST с effective overlay,
проверяют actual lookup и exact cost 4 после Sync reopen; NativeAsync проверяет
exact cost 2 и lookup после checkpoint/reopen. Первоначальный integration setup
использовал private intern_property; исправлен на public GQL UPDATE без расширения SDK.
`clippy-d7-custom-v4.jsonl`: **6968 errors / 445 warnings** с macro crate против D6 baseline
6970 / 446. Multiset diagnostic delta не содержит новых errors; известный warning относится
к ещё неподключённым PATTERN_CACHE tags. General Clippy/performance gates не пройдены.
`graphify-update-d7-custom.log` завершился; заключительный update
(`graphify-update-d7-custom-final.log`) получил Windows errno 22 при записи signature.
Последовательный повтор `graphify-update-d7-custom-retry.log` завершился успешно:
54456 nodes, 144811 edges, 1835 communities. Код возврата CLI сам по себе не является
подтверждением успешной записи графа.

### D7 — HOP traversal consumer и memory accounting

`tv::shortestPath` подключён к demanded Ready NODE/OUT/HOP metadata без edge-label restriction.
PlanHash и EXPLAIN фиксируют logical generation; runtime исключает binding чужого GraphId.
A* вычисляет только exact длину с reopening. Отдельный BFS сохраняет прежний FIFO/connector
порядок и отсекает только strictly greater completion bounds. NODE domain существующих
seed-local algorithms сохраняется и внутри TRAVERSE ATOM. Path удерживает собственный SnapshotId;
дифференциальное сравнение разных commits проверяет steps/cost, а не равенство snapshots.

Общий RecordId membership resolver используется source и target. Первый test обнаружил старое
несоответствие target API спецификации: literal RecordId отклонялся как `record`; root fix подключён
к reachable/shortestPath/weightedShortestPath/distanceTo/bellmanFord/lca. Seed algorithms больше
не сбрасывают bulk: повторные seeds сохраняют bag multiplicity.

Shared TraversalIndex получает checked preflight admission adjacency, включая hyperedge pairs
и qualified scopes. A* frontier и BFS state buffers ограничены statement budget. Read-only review
обнаружил недоучёт `Value::Path`: учитывался только boxed header. Теперь общий estimator включает
steps, public identifiers и graph references; existing VM queue accounting использует ту же оценку.
Retained algorithm batch получает admission до append/growth. Итоговый owned traversal array
сохраняет прежний контракт вне barrier scratch: перенос этого лимита на final output нарушил
existing sort/dedup spill gate с бюджетом 1 byte и был устранён. kHopEstimate освобождает временный
projection scratch guard перед retained batch admission; иначе он удерживал весь available budget.
Shared references учитываются консервативно. Это не закрывает общий compiler/runtime resource gate.

`test-d7-traversal-hops-v5.log`: 1296 paired NODE-domain source/target cases на Graph/Metagraph,
с TRAVERSE/ATOM, unreachable/zero-length paths, cycles, equal routes и old/private snapshots — pass.
Отдельный test проверяет repeated seeds, точные 64 path rows и fail-closed budget 96 KiB.
`test-d7-traversal-reopen-v3.log`: admissible inconsistent heuristic с необходимым reopening — pass.
`test-d7-traversal-path-memory-v1.log`: steps/long public identifier heap accounting — pass.
Первый backend checkpoint (`correctness-gate-d7-traversal-sync-v1.log`, native-v1) прошёл:
Sync graph indexes 2, RAM 4, local 15, NativeAsync 2; HOP lookup/cost проверены после reopen/checkpoint.
После memory/bulk fixes `correctness-gate-d7-traversal-lib-v3.log` прошёл: **668 pass / 10 ignored**.
`correctness-gate-d7-traversal-sync-v3.log`: graph indexes 2, public RAM 4, public local 15 — pass;
`correctness-gate-d7-traversal-native-v3.log`: NativeAsync 2 — pass. `check-d7-traversal-v5.log`,
owned nightly `fmt-d7-traversal-v8.log` и changed-file `diff-check-d7-traversal-v3.log` прошли.
Все 23 traversal regressions прошли после исправления scratch lifetime и final output contract.
Normalized multiset delta `clippy-d7-traversal-delta-v3.log` относительно custom v4 пуст:
новых diagnostics нет; общий Clippy gate по-прежнему не пройден из-за прежнего долга.
`graphify-update-d7-traversal-v1.log` подтверждает успешное обновление: 54468 nodes,
144864 edges, 1833 communities.

D7 на этом checkpoint оставался открытым: weighted/custom traversal consumers, полный compiler/runtime
scratch gate PURE COST и paired performance evidence.

### D7 — WEIGHT traversal consumer

`tv::weightedShortestPath` и `tv::distanceTo` подключены к Ready NODE/OUT/WEIGHT binding.
Physical `LandmarkWeighted` содержит kind и logical generation; EXPLAIN показывает refinement,
cost/mode/revision и fallback, PlanHash различает path/distance consumers. ATOM artifact не
подменяет существующий NODE domain этих seed algorithms.

Shared graph Dijkstra loop сохраняет distance/node queue и strict predecessor improvement.
ALT completion bound отсекает только strictly greater states против уже обнаруженной target cost.
Предварительный numeric guard исключает пропуск арифметических ошибок: all-edge validation
сохраняется, int/decimal суммы ограничены conservative i64 ceiling, float prefixes дополнительно
проверяются на exact integer casts и outward-rounded overflow. При отсутствии доказательства
работает полный прежний loop, включая unrelated reachable arithmetic при zero-length target.
Adjacency, weights, cache, frontier и reconstruction получают checked admission до роста;
typed errors/cancellation/branch budget проходят общие callbacks без зависимости graph от GQL.

`test-d7-weighted-traversal-v4.log`: **3 pass**, включая 1500 paired path/distance cases для
Graph/Metagraph/Hypergraph, TRAVERSE/ATOM, int/float/decimal/zero/large-integer costs, canonical
steps/type, incompatible artifact и old/private/abort snapshots. Отдельные assertions сохраняют
overflow и float/decimal errors и fail-closed query limits. Detour fixture подтверждает меньше
expansions при неизменном пути. Первый запуск fixture ошибочно использовал Transaction для
REBUILD; исправлен на production Datastore entrypoint. Check/no-default и owned nightly fmt
прошли. `correctness-gate-d7-weighted-traversal-lib-v1.log`: **671 pass / 10 ignored**.
`correctness-gate-d7-weighted-traversal-sync-v1.log`: graph indexes Sync 2, public RAM 4,
public local 15 — pass; `correctness-gate-d7-weighted-traversal-native-v1.log`: NativeAsync 2 — pass.
WEIGHT traversal lookup и exact cost проверены после reopen/checkpoint в обоих backend targets.
`clippy-d7-weighted-traversal-delta-v2.log` относительно HOP checkpoint не содержит новых
diagnostics. Общий Clippy остаётся FAIL из-за прежнего долга; первоначальные новые diagnostics
(visibility/imports/pattern parentheses/docs/needless borrow) исправлены в текущем срезе.
`check-d7-weighted-traversal-v4.log`, owned nightly `fmt-d7-weighted-traversal-v7.log` и
changed-file `diff-check-d7-weighted-traversal-v1.log` прошли. Performance gate не закрыт.
`graphify-update-d7-weighted-traversal-v1.log`: успешное обновление 54483 nodes,
144949 edges, 1824 communities.

### D7 — global MATCH WITH COST и fixed contributions

`MATCH ... IN GRAPH ... WITH COST fn::name` проходит owned AST → analysis → существующий
LogicalPathCost. Default распространяется на linear connector segments; local COST переопределяет
его. Global function разрешается и проверяется даже при override; clause без connector, wrong
order/keyword, missing/ambiguous function и HOP objective с COST отвергаются. WHERE и RETURN GRAPH
используют прежний pipeline. Отдельный parser test проверяет Unicode byte span.

Fixed Expand сохраняет edge aliases, prebound alias reuse, mode/uniqueness и обычные cursors.
Все три fixed surfaces (edge scan, traversal index, scoped expansion) вызывают общий qualified
PURE contribution helper, включая запрос без named path. Stateful prefix относится к segment
и начинает fixed segment с нуля. Обычный trace суммирует fixed/variable contributions.
Fixed/Atom/Hyper physical operators содержат cost identity/version/schema epoch/stateful flag;
canonical PlanHash encoding повышен до **4**. Global/local normalization даёт одинаковый plan hash.

`test-d7-global-cost-v4.log`: **4 pass** — parser, defaults/overrides/WHERE/RETURN GRAPH,
fixed aliases и повторное использование alias, mixed segments, stateful fallback, AtomWalk,
hyperedge и nested graph occurrence с разными overlays (exact cost 7+11=18).
Existing private writes/abort/old snapshot/dependency version/SSI test выполняется для local
и global COST. Global additive Ready lookup проверен после reopen/checkpoint в backend targets.
Две промежуточные fixture ошибки исправлены: variable-only semantic guard для fixed connector
и `as_graph`, ограниченный general family, вместо `as_materialized_graph` для RETURN GRAPH.

`correctness-gate-d7-global-cost-lib-v1.log`: **674 pass / 10 ignored**.
`correctness-gate-d7-global-cost-sync-v1.log`: graph indexes 2, public RAM 4, public local 15 — pass.
`correctness-gate-d7-global-cost-native-v1.log`: NativeAsync 2 — pass.
`check-d7-global-cost-v2.log` и owned nightly `fmt-d7-global-cost-v5.log` прошли.
`clippy-d7-global-cost-delta-v1.log` относительно WEIGHT traversal checkpoint пуст;
общий Clippy — FAIL (6966 errors, 437 warnings), прежний долг не правился.
Changed-file `diff-check-d7-global-cost-v1.log` прошёл. Graphify update
`graphify-update-d7-global-cost-v1.log`: 54488 nodes, 144984 edges, 1834 communities.
TRAVERSE WITH COST, PURE growing temporary resource gate и paired performance остаются открытыми.

### D7 — custom path::cost

`path::cost(path, fn::name)` использует общий versioned PURE COST resolver, named-function
cache и contribution evaluator. Unary additive и stateful формы выполняют exact numeric
reduction в порядке connector occurrences. Stateful prefix начинается с нуля для всего
переданного Path; одноаргументная форма продолжает читать stored cost без property reads.
Новый consumer не выбирает artifact для пересчёта scalar.

Query resolver проверяет origin первого graph, SnapshotId и все qualified occurrences до
contribution reads. Он сохраняет overlays и SSI. Суммирование не удерживает промежуточные
contributions; один connector расходует один общий expansion, cancellation проверяется
при validation и reduction. Пустой connector sequence возвращает 0. Runtime closures,
неоднозначные/не-COST функции, malformed scopes, detached connector, negative/non-finite
contributions и overflow отвергаются typed errors. Static calls внутри компилируемой UDF
используют обычное candidate merging и MAXDEPTH cycle validation; рекурсивный ALTER без
MAXDEPTH отклоняется без повторной компиляции собственного body.

Focused tests `test-d7-path-cost-v4.log` и `test-d7-path-cost-global-v1.log` прошли (2+3).
Review выявил пропущенный reader в traversal source/arguments/final RETURN: regression
`test-d7-path-cost-review-v1.log` воспроизвёл ошибку, исправление передаёт общий RecordFields
также этим evaluation sites и RETURN GRAPH. `test-d7-path-cost-v5.log`: 2 pass; финальный
lib gate включает прямые source, step argument и final RETURN checks, SELECT и traversal closure.
Проверены старые snapshots, private overlay/abort, foreign origin/snapshot, missing occurrence,
SSI, checked arithmetic и typed expansion exhaustion. Nested path сохраняет разные graph
occurrences одного connector base record и пересчитанную стоимость 7+11=18.

`correctness-gate-d7-path-cost-lib-v2.log`: **676 pass / 10 ignored**.
`correctness-gate-d7-path-cost-sync-v2.log`: graph indexes 2, public RAM 4, public local 15 — pass.
`correctness-gate-d7-path-cost-native-v2.log`: NativeAsync 2 — pass. Stored/recomputed custom
cost сравнивается после reopen/checkpoint в обоих backend targets.
Первый backend запуск остановился до исполнения tests из-за неверного имени target; повтор
использует отдельный `graph_indexes_local_sync` target.
`check-d7-path-cost-v4.log`, owned nightly `fmt-d7-path-cost-v8.log` и changed-file
`diff-check-d7-path-cost-v1.log` прошли. PURE growing temporary resource gate остаётся открытым.
`clippy-d7-path-cost-v2.jsonl`: общий FAIL (6966 errors, 437 warnings);
`clippy-d7-path-cost-delta-v2.log` относительно global COST checkpoint пуст.

### D7 — TRAVERSE WITH COST

Owned AST → analysis → logical/physical plan → execution проводит статический versioned
PURE COST key после optional outer graph scope. Navigation использует qualified connector
и exact accumulated traverser prefix даже без path projection. Seed-local custom weighted
path/distance начинает отдельный segment с prefix 0 и сохраняет NODE domain внутри ATOM.
Совместимый Ready NODE/OUT/function-version LANDMARK_SSSP без labels даёт lower bounds;
stateful COST остаётся exact full-state search. Повторно используется ShortestPathCursor,
с сохранением OUT incidence, включая undirected edges, mode/filters и canonical path ties.
SHORTEST/HOP shortestPath, BellmanFord и clause без connector consumer отвергаются при analysis.

TRAIL/ACYCLIC/SIMPLE получают structural bounds. WALK использует checked u32 depth и
общие work/memory caps; structurally unreachable target проверяется bounded BFS. Zero-cost
cycles и неразрешимые filters могут исчерпать бюджет: результатом является typed error.
Trace output допускается до allocation/clones. Bulk различает exact numeric prefix
representation (type/float bits/decimal scale), поскольку stateful UDF может его наблюдать.
PlanHash encoding повышен до **5**: captured cost и constraints входят в canonical bytes;
EXPLAIN показывает function/version/schema epoch/stateful, refinement и fallback.

Read-only review выявил различие OUT incidence и MATCH arrow filtering, а также пропуск
не-WEIGHTED filters при адаптации kernel. Cursor теперь сохраняет traversal relation и
переиспользует общий scoped filter matcher. Fixtures исправлены: bounded WALK oracle,
fresh transaction после resource error, SDK Serializable без вложенного BEGIN; Sync
reopen получает отдельный NODE custom index вместо попытки использовать ATOM generation.

`test-d7-traverse-cost-v5.log`: 4 pass; differential matrix — **960** query comparisons
(Graph/Metagraph/Hypergraph, NODE seed domain в TRAVERSE/ATOM, Ready/fallback, unary/stateful,
TRAIL/ACYCLIC/SIMPLE/WALK, reachable/unreachable/zero-length, bag duplicates, path/distance).
Полный gate дополнительно проверяет function dependency/version/PlanHash, private writes,
old snapshots/abort/SSI, nested connector scopes с overlays 7+11=18, ATOM edge-endpoint,
connector filters, typed invalid costs/work/memory limits и trace admission до cloning.

`correctness-gate-d7-traverse-cost-lib-v2.log`: **681 pass / 10 ignored**.
`correctness-gate-d7-traverse-cost-sync-v2.log`: graph indexes 2, public RAM 4, public local 15 — pass.
`correctness-gate-d7-traverse-cost-native-v2.log`: NativeAsync 2 — pass; custom path/distance
проверены после reopen/checkpoint в обоих отдельных backend targets.
`check-d7-traverse-cost-v3.log`, owned nightly `fmt-d7-traverse-cost-v8.log` и
`fmt-check-d7-traverse-cost-v1.log` прошли.
Changed-file `diff-check-d7-traverse-cost-v1.log` прошёл.
`clippy-d7-traverse-cost-v1.jsonl`: общий FAIL (6966 errors, 445 warnings);
`clippy-d7-traverse-cost-delta-v1.log` относительно path::cost v2 пуст для production `src`.
Новые allocations не являются доказательством закрытия общего PURE growing temporary
или всех full-state cursor resource gates: эти gates сохраняются открытыми.
`graphify-update-d7-traverse-cost-v1.log`: успешное AST update — 54511 nodes,
145111 edges, 1841 communities. Semantic community labeling отдельно не запускался.

### D7 — admission перед клонированием path buffers

Review обоих canonical shortest cursors нашёл два источника allocation до admission:
клонирование hop/canonical vectors перед `push_state` и создание boxed Values в heap
estimator. Общий `extend_path_buffers` проверяет размеры, допускает полный новый state,
выполняет fallible exact reserve для двух vectors и лишь затем клонирует prefix.
Pre-admitted state использует существующий cursor account без повторного начисления;
начальный пустой state сохраняет обычный admission. Identity estimator читает RecordId
по ссылке и не создаёт временные Values. Scope/constraints/cost/tie порядок не менялся.

`test-d7-path-buffers-v1.log`: 1 pass. Clone counter подтверждает ноль вызовов Clone
при отказе admission; успешное расширение сохраняет обе последовательности, checked
overflow происходит до admission. Этот check применяется к общему helper обоих cursors.
`correctness-gate-d7-path-buffers-lib-v1.log`: **682 pass / 10 ignored**.
`correctness-gate-d7-path-buffers-sync-v1.log`: graph indexes 2, public RAM 4, public local 15 — pass.
`correctness-gate-d7-path-buffers-native-v1.log`: NativeAsync 2 — pass.
`check-d7-path-buffers-v1.log`, owned nightly `fmt-d7-path-buffers-v1.log` прошли.
`fmt-check-d7-path-buffers-v1.log` и changed-file `diff-check-d7-path-buffers-v1.log` прошли.
`clippy-d7-path-buffers-v1.jsonl`: общий FAIL (6966 errors, 445 warnings);
`clippy-d7-path-buffers-delta-v1.log` относительно TRAVERSE COST checkpoint пуст.
Полный resource gate остаётся открытым: candidate construction/ordering, queue capacities,
terminal row cloning и PURE growing temporaries не закрываются этим исправлением.
`graphify-update-d7-path-buffers-v1.log`: успешное AST update — 54518 nodes,
145130 edges, 1800 communities.

## Открытая работа (после текущего checkpoint)

D7–D11: PURE runtime/full-state cursor resource gates, PATTERN_CACHE,
полный dependency invalidation, MATCH/traversal consumers, EXPLAIN/counters и
correctness/recovery/resource/performance matrix. Query consumers Reachability пока не подключены;
готовые labels не объявляются доказательством bounded path или способом восстановить path.
