# Graph module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/graph`.

## Назначение и границы

`graph` — публичная runtime-модель графов. Storage layout и транзакционная
изоляция ей не принадлежат. Durable/query boundaries используют `RecordId`,
`AtomRef` и `GraphRef`; публично видимый `AtomId` остаётся snapshot-local
handle конкретного materialization и нужен в том числе ECS sidecar.

- `GraphRead` — общий read contract для in-memory, disk-bound и hybrid views.
- `Graph<Mutable>` не помещается в `Value`; graph value всегда frozen.
- `Tree`, `RootedTree`, `Forest`, `Dag` и `Hypergraph` сохраняют отдельные
  structural invariants и layouts. Не заменять их номинальными wrapper-ами.
- `Metagraph` использует general layout как явно зафиксированное исключение.
- `GraphTopologyConstraints` хранит ортогональные `SIMPLE`, `BIPARTITE` и
  `COMPLETE`; это metadata, а не дополнительные graph families. Rooted tree
  хранит durable `RecordId` root и разрешает его в materialization-local
  `AtomId` только после проверки graph scope.
- `MaterializedGraph` является полностью owned result. Вложенный child graph
  физически представлен один раз, даже если несколько container slots
  ссылаются на него; разные occurrence paths при этом не схлопываются.
- `AtomOccurrenceRef` — snapshot-relative graph occurrence. Его equality/order/hash
  определяются парой `{GraphId, AtomRef}`; graph name и storage mode в identity не
  входят.
- `PathStep` хранит occurrence endpoint и optional occurrence connector edge,
  поэтому cross-level path может иметь endpoint в дочернем графе и
  connector в родительском. `Path` не хранит snapshot-local `AtomId`.
- Crate-private Path heap estimate включает immutable steps, RecordId и graph references;
  оценка не меняет equality, snapshot или canonical encoding. Shared references учитываются
  консервативно без дополнительных allocations.
- Trace materialization с admission сначала проверяет chain и оценивает owned steps,
  затем допускает allocation/clones; cancellation проверяется в обеих фазах.
- Внутренние SoA-accessors `label`/`weight` намеренно возвращают две оси
  optionality: внешний `None` означает stale/foreign `AtomId`, внутренний
  `None` — живой atom без label/weight. Не схлопывать эти состояния.

## Карта владельцев

- `general`, `forest`, `dag`, `hypergraph`, `metagraph`, `subgraph` — graph
  families;
- `id`, `topology`, `storage` — atom identity, incidence и SoA storage;
- `read`, `view`, `materialized` — общая поверхность чтения и типизированные
  views;
- `path` — public path и внутренний trace arena;
- `traversal` — connectivity, shortest paths, spanning, cuts и LCA;
- `tensor` — owned qualified EndpointTensor, NODE/full incidence planes и sparse
  binary/Number adjacency с borrowed VV/VE/EV/EE views. LOCAL retains boundary
  endpoints; induced analytics domain не включает их. Containment хранится отдельно.
  Public CSR constructor/export принимает Copy cells; memory/work admission учитывает
  retained capacities и filtered/empty-row scans, а dense export всегда явный.
  ROOTED runtime capture и qualified endpoint routes обходятся итеративно через borrowed
  views; произвольный depth ceiling не вводится. Undirected incidence устанавливает обе
  logical planes; original endpoint roles сохраняются отдельно для adjacency.
- `analytics` — owned qualified degree/spectral/distance results, sparse motifs,
  clustering и deterministic communities. Centrality distances используют
  vertex-acyclic projected paths; connector reuse в разных projected pairs разрешён.
  Non-Int costs сохраняют все prefixes в bounded full-state fallback. LPA votes
  используют Number; Louvain принимает только положительный certified gain.
  Counts, work и scratch checked; convergence exhaustion не возвращает partial success.
- `traversal/relation` — bounded NODE/ATOM incidence projection и deterministic
  condensation CSR; общий SCC kernel в connectivity сохраняет public NODE API.
  General CSR restore допускает cycles/self arcs; condensation restore требует DAG.
- `live` — отдельный live-update contract;
- `workspace` — ECS-sidecar derived state, не часть durability/equality.

## Инварианты

- Production `AtomId`: `gen(23) | kind(1) | slot(40)`; legacy layout из
  `fg-meta` не переносить.
- Mutation проверяет generation, graph identity и structural constraints до
  публикации.
- Алгоритмы принимают `GraphRead` и дают одну семантику для всех storage modes.
- Public all-target Dijkstra и query point refinement используют один loop с тем же distance/node
  queue и strict predecessor improvement. Query callbacks остаются crate-private через re-export;
  storage/GQL types в graph kernel не входят. Refinement предварительно валидирует все weights
  и доказывает отсутствие новых arithmetic errors. Reconstruction поддерживает fallible admission.
- Optional `GraphRead::edge_endpoint_connectors` возвращает snapshot-local logical
  connector IDs по полному qualified endpoint; readers без capability возвращают
  None для exact traversal fallback. `visit_endpoint_steps` сохраняет общие
  NODE/ATOM, direction и endpoint roles без сборки всех hyperedge endpoint pairs.
- Graph equality/ordering не означает изоморфизм; не вводить скрытый deep scan.
- `Path::resolve` с одним `GraphRead` применяется только к single-graph path.
  Cross-graph consumers открывают graph по `PathStep::atom().graph()`; новый resolver
  trait для этого не нужен.
- `query_to_memory` освобождает исходный guard после копирования, а mutable
  hybrid сохраняет origin mapping и explicit dirty set.
- Graph-result assembly копирует только выбранные atoms и closure-required
  endpoints/containers; adjacency не обходится неявно. Несвязанные root scopes
  дают typed error вместо flattening.

## Проверка

Проверять конкретное graph family, одинаковые результаты алгоритма на разных
`GraphRead` implementations, stale-generation/foreign-origin ошибки и
детерминированный порядок выдачи. Затем запускать `cargo check --lib
--no-default-features` и `cargo +nightly fmt --check`.
