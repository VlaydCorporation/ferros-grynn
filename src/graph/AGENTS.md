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
- `Path` хранит стабильные `AtomRef`, а не snapshot-local `AtomId`.

## Карта владельцев

- `general`, `forest`, `dag`, `hypergraph`, `metagraph`, `subgraph` — graph
  families;
- `id`, `topology`, `storage` — atom identity, incidence и SoA storage;
- `read`, `view`, `materialized` — общая поверхность чтения и типизированные
  views;
- `path` — public path и внутренний trace arena;
- `traversal` — connectivity, shortest paths, spanning, cuts и LCA;
- `live` — отдельный live-update contract;
- `workspace` — ECS-sidecar derived state, не часть durability/equality.

## Инварианты

- Production `AtomId`: `gen(23) | kind(1) | slot(40)`; legacy layout из
  `fg-meta` не переносить.
- Mutation проверяет generation, graph identity и structural constraints до
  публикации.
- Алгоритмы принимают `GraphRead` и дают одну семантику для всех storage modes.
- Graph equality/ordering не означает изоморфизм; не вводить скрытый deep scan.
- `query_to_memory` освобождает исходный guard после копирования, а mutable
  hybrid сохраняет origin mapping и explicit dirty set.

## Проверка

Проверять конкретное graph family, одинаковые результаты алгоритма на разных
`GraphRead` implementations, stale-generation/foreign-origin ошибки и
детерминированный порядок выдачи. Затем запускать `cargo check --lib
--no-default-features` и `cargo +nightly fmt --check`.
