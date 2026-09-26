# Отчёт этапа 14: Rule/CBO optimizer, `DETERMINISTIC` и `EXPLAIN`

**Дата:** 2026-09-26

**Статус:** O0–O12 завершены; production pipeline использует один concrete optimizer.

## 1. Итог

Между typed logical IR и sync pull executor добавлен обязательный физический planning layer:

```text
parse -> structural validation -> typed analysis -> logical IR
      -> canonical rules -> physical alternatives -> fixed-point cost selection
      -> physical plan -> sync pull execution -> QueryResult<Value>
```

Реализация не добавляет публичный planner API, runtime registry, planner traits с одной
реализацией, новые зависимости или новый storage format. `gql` получает только read-only
`PlannerSnapshot`; `DatabaseRoot`, `DataFileId`, страницы и storage DTO за границу datastore
adapter не выходят.

Обязательные результаты этапа достигнуты:

- scan, point, scalar-index и Count paths выбираются до исполнения;
- forced hints проверяются до первого row pull, optional недоступность деградирует в scan с
  наблюдаемой причиной;
- INNER equality joins получают bounded DP/greedy enumeration и конкретный алгоритм;
- ORDER может использовать forward/reverse scalar index и exact keyset seek;
- текущий MATCH использует общий estimator/cost/tie-break;
- `DETERMINISTIC` фиксирует snapshot, seed, result ordering и `PlanHash`;
- prefix `EXPLAIN`, `EXPLAIN FULL` и реально исполняющий `EXPLAIN ANALYZE` работают для
  SELECT, текущего MATCH, DML и DDL;
- correctness, local reopen, обе feature-конфигурации и profiling gates пройдены.

## 2. Реализованная структура

Optimizer декомпозирован по одному владельцу ответственности:

| Модуль | Ответственность |
|---|---|
| `optimizer/plan.rs` | physical nodes, warnings, write phases и plan hash input |
| `optimizer/snapshot.rs` | безопасная projection catalog/statistics snapshot |
| `optimizer/demand.rs` | lazy demand classes, graphs, property/degree histograms |
| `optimizer/estimate.rs` | cardinality intervals и histogram estimates |
| `optimizer/cost.rs` | integer/fixed-point cost components |
| `optimizer/rules.rs` | canonical logical rules в фиксированном порядке |
| `optimizer/select_plan.rs` | orchestration SELECT planning |
| `optimizer/select_plan/access.rs` | access paths, hints, ORDER и residual contracts |
| `optimizer/select_plan/join_order.rs` | DP/greedy join enumeration и join costs |
| `optimizer/match_plan.rs` | текущий MATCH seed order |
| `optimizer/write_plan.rs` | DDL/DML physical phase sequence |
| `optimizer/deterministic.rs` | purity checks, replay context и statement seed |
| `optimizer/encode.rs` | versioned canonical hashes без platform-dependent encoding |

После финального разбиения самые крупные SELECT-файлы содержат 256, 684 и 327 строк;
прежнего 1219-строчного `select_plan.rs` больше нет.

### 2.1. Physical alternatives

| Область | Реализованные alternatives | Зафиксированная граница |
|---|---|---|
| Source | `PointGet`, `ClassScan`, `GraphClassScan`, `ScalarIndexScan`, `CountLookup`, `GraphCatalogScan`, `Derived` | Нет bitmap merge, skip-scan и index intersection |
| Scalar index | exact, finite `IN`, equality prefix + один range, forced/full ordered scan | Всегда остаётся residual predicate |
| Join | hash, partitioned hash, block nested-loop, index nested-loop | DP до 8 sources, deterministic greedy после 8 |
| ORDER | external sort, exact unique-index order, Standard tie-group sort, reverse scan, exact keyset seek, seeded RAND | Несовместимые null/none semantics сохраняют sort |
| Write | target selection, mutation, constraint validation, catalog mutation, index build, publication | EXPLAIN не исполняет phases, ANALYZE исполняет ровно один раз |

### 2.2. Canonical rules

Rules применяются в нормативном порядке: constant folding, conjunction decomposition,
predicate pushdown, projection pruning, late materialization, safe LIMIT pushdown, exact count
rewrite и sort elimination. Rule-pass не переставляет выражения, способные вернуть ошибку, не
меняет LEFT/CROSS barriers, bag multiplicity, effective-property view или порядок floating-point
reduction.

## 3. Statistics и cost model

`PlannerSnapshot` содержит только востребованные metadata и distributions:

- exact base/graph class counters;
- graph node/edge/label/degree totals;
- transaction-visible index identity/state/scope/version/physical binding;
- только те property/degree histograms, которые запросил текущий plan demand.

Cardinality хранится как `{lower, expected, upper}`. Cost не использует `f64`: startup,
sequential rows, random probes, expression CPU, peak memory, spill и uncertainty/resource risk
суммируются saturating integer arithmetic и имеют canonical tie-break.

Range-estimator использует canonical literal bytes и границы equi-depth bucket. Исправленная
модель считает ordered range как один seek плюс последовательный leaf walk, а не как random
lookup на каждую строку. Именно прежняя неправильная стоимость скрывала выгодный index path при
selectivity 0–1%.

Fresh, approximate, stale и missing statistics проверены дифференциально: состояние может менять
оценку, warning и physical choice, но не результат. После порога
`max(1024, ceil(population * 0.10))` dirty rows distribution не используется как точная;
`statistics_stale`/`statistics_missing` выводятся в FULL EXPLAIN.

## 4. Performance screening

Артефакт: `target/gql-stage14/results.csv`. Все числа ниже — median paired rotated runs из
`--profile profiling`; planning — p95. `bps` означает basis points, то есть 100 = 1%.

| Rows | Selectivity | Choice | Scan, ms | Auto, ms | Ускорение auto | Planning p95 |
|---:|---:|---|---:|---:|---:|---:|
| 100 000 | 0% | ScalarIndexScan | 57,807 | 6,398 | 9,04x | 35,8 us |
| 100 000 | 0,01% | ScalarIndexScan | 60,038 | 6,371 | 9,42x | 36,8 us |
| 100 000 | 0,1% | ScalarIndexScan | 48,296 | 6,530 | 7,40x | 36,1 us |
| 100 000 | 1% | ScalarIndexScan | 59,406 | 7,216 | 8,23x | 44,4 us |
| 100 000 | 10% | ScalarIndexScan | 58,517 | 16,134 | 3,63x | 37,3 us |
| 100 000 | 50% | ClassScan | 62,998 | 63,043 | 1,00x | 38,3 us |
| 100 000 | 100% | ClassScan | 55,726 | 46,996 | 1,19x | 39,6 us |
| 1 000 000 | 1% | ScalarIndexScan | 519,149 | 32,989 | 15,74x | 47,4 us |
| 1 000 000 | 100% | ClassScan | 688,561 | 660,055 | 1,04x | 44,5 us |

Шумовая точка 10k/100% была повторена на 15 runs: auto `ClassScan` 6,184 ms против forced scan
6,048 ms, отклонение +2,24%, то есть ниже 10%-gate. Для boundary 100k/50% 15 runs дали +5,55%
к лучшему compatible forced candidate. Автоматический выбор не проиграл gate более чем на 10%.

Stage-13 baseline простого 10k no-index SELECT составлял 5,944 ms; текущий forced scan —
6,048 ms (+1,75%), весь automatic path — 6,184 ms (+4,02%). Gate общей регрессии +5%
выполнен. Instrumentation counter включается только для `EXPLAIN ANALYZE`; обычный pull делает
предсказуемую false-проверку без atomic increment. Отдельного hotspot этой проверки VTune не
показал, а совокупная регрессия остаётся ниже общего gate.

Index ORDER/LIMIT устраняет full spool:

| Rows | External ORDER, ms | Index ORDER, ms | Ускорение |
|---:|---:|---:|---:|
| 100 000 | 206–242 | 4,84–7,30 | 30–45x |
| 1 000 000 | 10 794–11 732 | 30,67–38,08 | 283–356x |

На двух источниках automatic join дал 1,291 s против forced-right baseline 1,371 s, то есть
примерно на 5,8% быстрее. Differential tests покрывают bounded DP на 8 и deterministic greedy на
10 sources.

Plan cache не добавлен: наблюдаемый planning p95 равен 10,5–47,4 us, существенно ниже gate 1 ms
и ниже 5% длительных запросов.

## 5. Resources и spill

Stage-14 reuse-ит общий Stage-13 barrier budget и run codec. Physical plan выбирает
`PartitionedHash` при resource risk; warning виден в EXPLAIN.

| Workload | Budget | Elapsed | RSS delta | Temp peak |
|---|---:|---:|---:|---:|
| ORDER 100k | 1 MiB | 1,751 s | 1,22 MiB | 47,04 MB |
| ORDER 100k | 32 MiB | 1,159 s | 36,18 MiB | 41,44 MB |
| ORDER 100k | 256 MiB | 0,294 s | 195,72 MiB | 0 |
| JOIN 100k | 1 MiB | 7,685 s | 1,20 MiB | 91,56 MB |
| JOIN 100k | 256 MiB | 1,637 s | 289,03 MiB | 68,10 MB |

RSS включает source/result rows и allocator arenas и поэтому не является одной только barrier
memory. Внутренний accounting и тесты проверяют именно shared barrier limit; малые запросы temp
files не создают. Exact index keyset cursor прошёл с budget 1 byte и не создал spool.

## 6. `DETERMINISTIC`

Effective seed включает version, normalized query fingerprint, canonical parameter fingerprint,
snapshot и user seed. `PlanHash` — RapidHash V3 над versioned canonical physical-plan bytes;
`Debug`, native `usize` encoding и hash-map iteration order не участвуют.

Проверено:

- 20 повторов `ORDER BY RAND()` с одним replay tuple дают один результат;
- 20 FULL EXPLAIN дают один `PlanHash`;
- RAM/local и reopen сохраняют result и hash;
- `time::now()` отклоняется до чтения;
- `READ COMMITTED + DETERMINISTIC` отклоняется структурно;
- `FAST` остаётся typed unsupported до этапа 17.

## 7. `EXPLAIN`

Поддерживается только prefix syntax:

```grynql
EXPLAIN SELECT id FROM person WHERE properties.age = 30;
EXPLAIN FULL FORMAT JSON
SELECT id FROM person WITH INDEX by_age WHERE properties.age = 30;
EXPLAIN ANALYZE FORMAT JSON CREATE person:published;
EXPLAIN FULL FORMAT JSON DEFINE INDEX by_age ON CLASS person FIELDS age;
```

Logical TEXT показывает normalized logical statement. FULL JSON содержит version/hash, snapshot,
seed, stable node IDs, physical operator, estimates/costs, required/provided ordering, index
identity/state/scope/bounds, residual, statistics и warnings. Write-plan показывает
`TargetSelection`, `Mutation`, `ConstraintValidation`, `CatalogMutation`, `IndexBuild` и
`Publication` в применимом порядке.

Plain/FULL не получают writer lease, не выделяют durable ID и не читают rows. ANALYZE исполняет
statement один раз, полностью потребляет SELECT, удаляет continuation и возвращает actual rows,
pulls, elapsed, spill, peak accounted memory, affected rows и publication state. Autocommit DML/DDL
публикуется; explicit transaction сообщает `deferred`. Ошибка использует обычный rollback.

## 8. Профилирование

Финальный exact artifact pair:

```text
target/profiling/deps/ferros_grynn-2170861548b6ea93.exe  13 213 696 bytes
target/profiling/deps/ferros_grynn-2170861548b6ea93.pdb 194 113 536 bytes
timestamp: 2026-09-26 19:59:22
```

Сборка выполнена `cargo test --locked --profile profiling --lib --no-default-features
gql_stage_14_optimizer_probe --no-run`. VTune Hotspots запускал именно этот EXE; артефакты:

- `target/gql-stage14/vtune-hotspots-20260926-final3/`;
- `target/gql-stage14/vtune-hotspots.csv`;
- `target/gql-stage14/vtune-hotspots-lines.csv`.

Rust symbols и source lines разрешились. Верхние user-mode точки:

| Hotspot | CPU | Доля/наблюдение |
|---|---:|---|
| `memcmp` | 0,231 s | 10,0% |
| `mi_page_free_list_extend` | 0,155 s | 6,7% |
| `memcpy` | 0,140 s | 6,0% |
| `mi_page_malloc_zero` | 0,088 s | 3,8% |
| `layout_row`, `select.rs:1152` | 0,063 s | 2,7%; видимый query hotspot |

Профиль подтверждает, что planning не является bottleneck. Основная оставшаяся стоимость относится
к materialization/copy/ordering общего query path. VTune предупредил об отсутствии admin-доступа к
microarchitecture sampling driver; source-level user-mode Hotspots и symbols при этом получены,
поэтому capture не классифицируется как SKIP.

## 9. Correctness evidence

Пройдены:

- scan/index equality, composite prefix/range, finite IN, Count и residual;
- Ready/stale/prepared/missing/wrong-scope hints и optional fallback;
- base/effective scope, Standard duplicates и public `RecordId` ties;
- SSI range parity между index и scan;
- INNER reorder/build side, неизменность LEFT/CROSS, DP/greedy equality;
- index ORDER forward/reverse, null/ties/keyset и spool parity;
- fresh/approximate/stale/missing statistics без изменения результата;
- current MATCH plan integration;
- deterministic 20-run replay и local reopen;
- no-effect plain/FULL EXPLAIN и real ANALYZE publication/rollback;
- spill/cursor cleanup и cancellation.

Финальные library gates после последней декомпозиции:

```text
cargo test --locked --lib --no-default-features -- --test-threads=1
425 passed; 0 failed; 6 ignored

cargo test --locked --lib --no-default-features --features native-async-io -- --test-threads=1
427 passed; 0 failed; 7 ignored
```

Local regression `optimizer_plan_and_deterministic_result_survive_reopen` также прошла.

Targeted nightly rustfmt check для всех затронутых Rust-файлов прошёл. Repository-wide
`cargo +nightly fmt --all -- --check` по-прежнему показывает только ранее существовавший drift в
`src/memory/notrack.rs` и нескольких `src/storage/*` файлах. Strict workspace Clippy остаётся
заблокирован `Q-14`; диагностический запуск с `--cap-lints warn` завершился и показал 4591
накопленный warning. Эти долги не маскируются как зелёный gate и не исправлялись массово в
optimizer-срезе.

## 10. Известные границы и следующие минимальные эксперименты

1. Filtered keyset пока использует корректный spool fallback: direct seek включается только для
   unfiltered single-source exact index ORDER, потому что LIMIT нельзя считать до residual.
2. Очень большая duplicate group Standard index материализуется и сортируется внутри tie group.
   Upgrade нужен только при измеренном high-skew memory/latency bottleneck.
3. Exact cursor удерживает read-only transaction/MVCC root до close/expiry. Это сохраняет snapshot,
   но long-lived cursor pressure должен измеряться на этапе 19.
4. Local open сейчас вызывает eager `rebuild_derived_state`, хотя durable indexes/statistics уже
   существуют. Это не нарушает correctness этапа 14, но скрывает startup scaling и не позволяет
   считать side-files единственным runtime source. Проблема зарегистрирована как `Q-36`.
5. Plan cache, adaptive re-planning, WCOJ, bitmap intersection, skip-scan и vectorized/parallel
   executor не добавлены: текущие измерения не дают основания для этой сложности.

Следующий минимальный optimizer-эксперимент нужен только при изменении workload: для filtered
keyset — сохранять residual-aware accepted-row anchor; для duplicate Standard groups — bounded
tie-group spill. До измеренного trigger production path остаётся текущим.
