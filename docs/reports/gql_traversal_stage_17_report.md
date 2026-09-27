# Отчёт этапа 17: Traversal VM, closures, `FAST` и physical sparse frontier

**Дата:** 2026-09-28
**Статус:** V0–V15 завершены; этап 17 закрыт.

## 1. Итог

Реализованы:

- session-scoped GrynnQL closures с captures, effect analysis, savepoint и лимитом retained
  plan/captures;
- `TRAVERSE` и generic `|>` в общем parse → analysis → logical IR → optimizer → physical
  plan → sync pull pipeline;
- navigation, transform/filter, sacks, side effects, bulking, path propagation, control steps,
  barriers и aggregates;
- adapters существующих graph algorithms с exact `Number` для weighted core;
- mutation-only `KEEP LEVELPATH` и read-only `graph::level_path`;
- exact `ExecutionMode::Fast` без ослабления ACID, constraints и result multiset;
- snapshot-exact local `PhysicalTraversalCursor` и положительный frame-IoRing QD32 route;
- traversal physical plans, canonical hash и runtime/transport counters в EXPLAIN.

Второй MATCH executor, публичный `Traverser`, универсальный traversal trait, новая async runtime,
новые зависимости и speculative parallel executor не добавлены.

## 2. Архитектурные границы

Language closure хранится в concrete registry своей `GqlSession`. Registry учитывает captures и
conservative retained typed-plan bytes в лимите 256 МиБ, не выполняет скрытый eviction и не
позволяет closure пережить либо сменить session. Вызов использует существующий transaction
savepoint; ошибка восстанавливает root, conflict/read/write sets, scopes и mutation log.

Traversal VM хранит `current`, optional trace, sacks, bindings и logical bulk. Merge допустим
только при полном равенстве observable state; path tracking и impure side effects отключают
опасное слияние. `PathTraceArena` остаётся единственным path representation. Barriers используют
общий Stage-13 budget и spill lifecycle; frontier/path state при превышении бюджета fail-closed.

Local physical path проходит только через datastore adapter. Cursor фиксирует точный snapshot,
generation и bindings, разрешает resident/WAL/home версии до I/O и возвращает GQL owned
occurrences/properties/topology. После private mutation затронутый graph до конца statement
переключается на materialized private root.

## 3. Correctness evidence

Focused traversal suite проверяет все public step adapters, closures/effects/savepoints,
expanded-bulk oracle, sacks, control flow, barriers, algorithms, LevelPath, FAST parity,
EXPLAIN и physical fallback: `19 passed`, failures отсутствуют.

Финальные library gates после последней правки resource accounting:

| Конфигурация | Passed | Ignored | Failed |
|---|---:|---:|---:|
| `--no-default-features` | 474 | 6 | 0 |
| `--no-default-features --features native-async-io` | 476 | 7 | 0 |

Проверены RAM/local/reopen paths, exact staged/WAL/home precedence, private-mutation fallback,
SSI parity, timeout/cancellation и отсутствие partial output. Runtime-only closure/path values
не проходят durable codecs; transient spill использует явный query format.

## 4. Performance evidence

Финальный paired probe собран только с `--profile profiling`. Точная Cargo-пара:

```text
target/profiling/deps/traversal_performance_probe-abc53d4da17c4a1a.exe
target/profiling/deps/traversal_performance_probe-abc53d4da17c4a1a.pdb
```

Stem и timestamp обоих файлов совпадают: `2026-09-28 00:54:08`. CSV:
`target/gql-stage17/traversal-vm-01a0e4dd-a8dd-7b22-ad45-8e3689792e9a.csv`.

| Workload | Control median | Candidate median | Paired median ratio | Решение |
|---|---:|---:|---:|---|
| unordered fanout, 15 × 256 | BEST `2 431 099 ns` | FAST `1 940 107 ns` | `0,8224×` | FAST быстрее на 17,8% |
| one-hop, 15 × 256 | MATCH `61 448 ns` | TRAVERSE `57 128 ns` | `0,9284×` | regression отсутствует |

Стабильный Stage-16 regression probe на 1024 строках и 2000 samples дал class/label/one-hop/
TRAIL `971 900 / 548 200 / 612 400 / 730 200 ns`. Относительно Stage-14 comparator это
`+1,56% / −0,92% / −4,31% / −4,84%`; обязательный предел +5% выполнен. Диагностический
4096-row прогон имел разброс выше 5% после длительных LTO-сборок и не использовался как
acceptance evidence.

Resource matrix подтвердила bounded growth: path workloads `(branching, depth)` 32×1, 8×4,
2×7 и 1×30 имели frontier peaks 32/4096/128/1; hyper arity 2/8/32 дал столько же assignments;
containment fanout 1/4/16 дал 2/8/32 links. Неиспользуемый path не создаёт trace arena.

Speculative parallel path не добавлен: обязательная комбинация measured CPU hotspot ≥20% и
paired gain ≥15% на 4/8 cores не доказана. Однопоточный pull уже проходит Stage-17 gates.

## 5. `Q-10`

`Q-10` закрыт положительно отдельным paired physical-frontier gate. Frame-IoRing QD32 показал
median `868 610 ns` против sync Direct `1 343 950 ns` (`0,646×`), улучшил p99 и CPU и сохранил
bounded RSS. Production router выбирает его только для подходящего `NativeAsync` sparse window;
sync, singleton miss и неподходящие окна остаются на sync Direct. Полное evidence:
[`gql_traversal_stage_17_q10_report.md`](gql_traversal_stage_17_q10_report.md).

## 6. Профилирование и tooling

VTune software Hotspots captures для traversal VM, one-hop и physical frontier разрешили Rust
symbols и source lines. Короткий final MATCH capture оказался dominated построением fixture и
не интерпретировался как query hotspot. Hardware sampling недоступен без административно
загруженного VTune driver; это зафиксировано как environment `SKIP`, а не ошибка symbols и не
замена paired benchmark.

Финальные команды:

```text
cargo test --locked --lib --no-default-features -- --test-threads=1
cargo test --locked --lib --no-default-features --features native-async-io -- --test-threads=1
cargo check --locked --lib --no-default-features
cargo check --locked --lib --no-default-features --features native-async-io
cargo +nightly fmt --all -- --check
cargo clippy --locked --lib --no-default-features --features native-async-io --no-deps
```

Итог gate:

| Проверка | Результат |
|---|---|
| Library tests без default features | `PASS`: 474 passed, 6 ignored |
| Library tests с `native-async-io` | `PASS`: 476 passed, 7 ignored |
| `cargo check` в обеих конфигурациях | `PASS` |
| Nightly rustfmt затронутых Stage-17 файлов | `PASS` |
| Focused Clippy затронутых Stage-17 файлов | `PASS`: diagnostics отсутствуют |
| Workspace-wide rustfmt | `SKIP`: единственный оставшийся diff относится к не затронутому `src/memory/notrack.rs` |
| Workspace-wide Clippy | `SKIP`: команда завершилась с кодом 101 из-за существующего lint debt вне Stage 17 |
| `graphify update .` | `SKIP`: launcher вернул `Failed to canonicalize script path` |

Workspace-wide debt и отказ Graphify не маскируются успешными focused gates и не относятся к
реализации Stage 17. Затронутые файлы проходят `git diff --check`; trailing whitespace в новом
отчёте отсутствует.

## 7. Отложенная поверхность

- `RETURN GRAPH` и graph assembly — этап 18;
- UDF/catalog/overloads/security/recursion и `tv::algo` — этап 21;
- traversal indexes — этап 25;
- PageRank, centralities, triangles, clustering, label propagation, Louvain и all-shortest —
  этап 26;
- approximate FAST, публичный Traverser и speculative parallel runtime не вводятся без нового
  измеренного bottleneck.
