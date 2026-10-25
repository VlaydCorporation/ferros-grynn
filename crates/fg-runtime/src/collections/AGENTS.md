# Collections module guidance

Этот файл дополняет корневой `AGENTS.md` для `crates/fg-runtime/src/collections`.

## Назначение и границы

`collections` — общий дом переиспользуемых коллекций и структур данных
Ferros-Grynn. Здесь размещаются контейнеры, не принадлежащие только одному
предметному модулю и имеющие либо несколько потребителей, либо обоснованную
будущую область применения. Public value collections (`Object`, `Set`,
`PropertyMap`) по-прежнему принадлежат `values`, а graph adjacency/layout —
`graph` или `storage`.

Наличие реализации здесь — повод рассмотреть её до написания аналога, но не
автоматическое доказательство пригодности. Непроверенные и отключённые
структуры сначала получают аудит инвариантов, ошибок, unsafe и тестов, затем
подключаются минимальным production-срезом.

## Карта подмодулей

Только `cow_vec.rs` перенесён в runtime. Остальные перечисленные реализации
остаются в отключённом корневом дереве `src/collections`.

- `cow_vec.rs` — copy-on-write vector для дешёвых immutable snapshots и дельт;
  текущие consumers — SoA layouts runtime graph families;
- `src/collections/siv.rs` — stable-index vector для ephemeral registries/arenas; перед
  использованием проверить generation и stale-handle semantics; это не замена
  нормативному `AtomId`;
- `src/collections/borrow_bag/mod.rs` — typed heterogeneous append-only bag;
  `append.rs` описывает type-level добавление, `handle.rs` — handles,
  `lookup.rs` — типобезопасный поиск;
- `src/collections/radixdb/mod.rs` — radix-tree facade и bulk/prefix операции;
  `node/` — tree representation и алгоритмы, `store/` — blob backends,
  `util.rs` — локальное форматирование/служебный код;
- `src/collections/roaring_graphs/` — отключённые bitmap-ориентированные DAG/digraph и
  triangular-matrix структуры, включая delta-debugging bitmap.

Будущие области выбора: immutable catalogs и snapshots, plan/job registries,
term/path dictionaries, autocomplete, bitmap frontiers/visited sets,
posting/reachability/neighborhood indexes и differential test oracles.

## Правила

- Выбор существующей коллекции, внешнего crate и ручной реализации одинаково
  требует описанного access pattern и вывода текущего `rust-workflow` owner.
- Сначала сравнить `std` и уже принятый domain type, затем подходящие реализации
  из этого модуля и зависимостей.
- Не обобщать container backend через trait с одной реализацией.
- Public value contracts (`Object`, `Set`, `PropertyMap`) меняются в
  `crates/fg-runtime/src/values`, а graph adjacency — в `crates/fg-runtime/src/graph`, не здесь.
- Unsafe ownership tricks требуют отдельного safety contract, Miri/property
  tests и доказательства, что более простой owning layout недостаточен.
- Ошибки production-пути должны быть `FerrosGrynnError`; `anyhow` из
  `radixdb` не распространять за пределы прототипа.

## Проверка

Перед production-использованием проверить формат/ошибки, сложность, память,
panic/drop paths и совместимость с предметными ID/codec contracts. Запускать
tests конкретной структуры и всех затронутых consumers. Для handles проверять
stale/reuse; для persistent radix/bitmap кода — malformed input и property
oracles; для concurrent/unsafe кода — отдельный Miri/Loom/Shuttle этап, а не
только обычный unit test.
