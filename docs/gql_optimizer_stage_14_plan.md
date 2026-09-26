# Этап 14. Rule/CBO optimizer, `DETERMINISTIC` и `EXPLAIN`

## 1. Результат и зафиксированные решения

После этапа 14 pipeline становится явным:

```text
parse → structural validation → typed analysis → logical IR
      → canonical rules → physical alternatives → cost selection
      → physical plan → sync pull execution → QueryResult<Value>
```

Утверждаются следующие решения:

- `WITH INDEX` задаёт набор разрешённых кандидатов; optimizer обязан выбрать один
  совместимый Ready-index для каждого явно указанного source.
- `DETERMINISTIC` является строгим:
  - одинаковые snapshot, параметры, seed и engine build дают одинаковые plan/result;
  - `time::now()` и другие недетерминированные функции отклоняются до чтения данных;
  - `READ COMMITTED + DETERMINISTIC` запрещается, поскольку не фиксирует snapshot всей
    транзакции.
- `EXPLAIN` и `EXPLAIN FULL` поддерживают DDL, DML, SELECT и текущее исполнимое MATCH без
  выполнения.
- `EXPLAIN ANALYZE` реально выполняет обёрнутый statement:
  - в autocommit успешная DDL/DML публикуется;
  - внутри explicit transaction изменяется private root до последующего `COMMIT`;
  - ошибка использует обычные rollback/abort rules;
  - пользовательский результат statement заменяется планом и метриками.
- Отсутствующий, неподходящий или `stale` индекс без forced hint приводит к корректному scan
  с наблюдаемой причиной.
- Plan cache, adaptive re-planning, WCOJ, vectorized executor и новые path-алгоритмы не входят.

## 2. Контракты и архитектура

### 2.1. Один concrete optimizer

Добавить crate-private `gql::optimizer`, без публичного planner API, registry правил или traits с
одной реализацией:

- orchestration и plan types;
- канонические logical rules;
- estimates и fixed-point cost model;
- physical alternatives;
- explain projection и runtime counters.

Typed logical IR остаётся источником семантики. Executor больше не выбирает scan/join algorithm
самостоятельно, кроме заранее предусмотренного resource fallback, например hash join →
partitioned hash join.

Минимальные physical operators:

```text
Source:
  PointGet
  ClassScan
  GraphClassScan
  ScalarIndexScan
  CountLookup
  GraphCatalogScan
  Derived

Join:
  HashJoin
  PartitionedHashJoin
  BlockNestedLoop
  IndexNestedLoop

Write:
  TargetSelection
  Mutation
  ConstraintValidation
  CatalogMutation
  IndexBuild
  Publication
```

### 2.2. Граница с datastore

Transaction adapter строит из того же immutable statement snapshot read-only
`PlannerSnapshot`:

- class/field/index metadata;
- `IndexId`, kind, state, scope и schema version;
- наличие transaction-visible physical binding;
- exact base/graph/label counters;
- только востребованные property/degree histograms;
- statistics revision, analyzed revision, dirty rows и method.

`gql` не получает `DatabaseRoot`, `DataFileId`, page addresses или storage DTO.

Текущий игнорируемый список `ScalarIndexConstraint` заменить явным planned access path. Index
scan регистрирует те же conservative `BaseClassRange`/`GraphClassRange`, что scan, поэтому смена
плана не ослабляет SSI/FCW.

### 2.3. Канонические rules

Rules выполняются в неизменном порядке:

1. Constant folding только для уже типизированных pure expressions.
2. Разложение conjunction без перестановки expressions, способных вернуть ошибку.
3. Predicate pushdown к единственному source.
4. Projection pruning с учётом joins, group, order, fetch и output.
5. Existing late materialization.
6. Safe LIMIT pushdown при отсутствии join/group/split/order/correlated subquery/ONLY.
7. Count rewrite для точной формы `count()` над одним source.
8. Sort elimination только при полностью совпадающем index ordering, null placement и stable tie
   key.

Rules не могут менять LEFT JOIN, bag multiplicity, effective-property semantics, порядок ошибок
или floating-point reduction order.

### 2.4. Statistics и cost model

Cardinality представляется диапазоном:

```text
{ lower, expected, upper }
```

Источники:

- exact base/graph class counters;
- exact graph node/edge/label/degree totals;
- property histograms точного class/field/scope;
- degree histograms для уже реализованного MATCH.

Histogram policy:

- fresh: `revision == analyzed_revision && dirty_rows == 0`;
- до `max(1024, ceil(population × 0.10))` dirty rows — использовать с расширенным uncertainty;
- после порога — conservative estimate и warning `statistics_stale`;
- отсутствие — conservative estimate и warning `statistics_missing`;
- approximate sample увеличивает uncertainty, но не влияет на correctness.

План выбирается integer/fixed-point стоимостью, а не platform-sensitive `f64`. Компоненты:

- startup;
- sequential rows;
- random lookups;
- expression CPU;
- peak memory;
- expected spill;
- uncertainty/resource risk.

Веса калибруются microprobes. Residual predicate выполняется после любого index access.

### 2.5. Index access и hints

Поддержать:

- equality по leftmost composite prefix;
- equality prefix плюс один range следующего field;
- конечный constant/parameter `IN` как несколько exact probes;
- full scalar-index scan при forced hint или exact ORDER satisfaction;
- Count index только для доказанно точной aggregate-формы.

Не добавлять skip-scan, bitmap merge или пересечение secondary indexes.

Синтаксис:

```grynql
SELECT ...
FROM person AS p
JOIN department AS d ON ...
WITH INDEX p.by_age, d.by_name
WHERE ...;

SELECT ...
FROM person
WITH NOINDEX
WHERE ...;
```

Правила:

- неqualified index name допустим только при однозначном source;
- несколько индексов одного source — candidate set, optimizer выбирает один;
- каждый явно упомянутый source обязан использовать один индекс из своего набора;
- missing/stale/prepared/wrong-scope forced index даёт typed planning error до row pull;
- `WITH NOINDEX` запрещает user secondary indexes, но не primary/class/membership structures;
- без hint недоступный индекс даёт scan fallback и explain warning;
- index ordering ties упорядочиваются по public `RecordId`, не по `AtomId`.

### 2.6. Join и ORDER planning

Join enumeration:

- переставляются только consecutive INNER equality joins с pure predicates;
- LEFT, CROSS, correlated subquery и semantic barriers фиксируют границы;
- до 8 sources применяется bounded Selinger-style DP;
- свыше 8 — deterministic greedy, cross product рассматривается последним;
- ограничения задаются числом states, не wall-clock;
- INNER hash join строит меньшую сторону;
- LEFT сохраняет left side и right build;
- small outer + Ready right index допускает IndexNestedLoop;
- arbitrary predicate и CROSS используют block nested-loop.

ORDER:

- Unique index может полностью удовлетворить matching ORDER;
- Standard index требует bounded stable sorting внутри одинакового key;
- reverse scan разрешён только при exact reverse range;
- несовпадающие null/none semantics сохраняют external sort;
- index-ordered `START AFTER` выполняет range seek по ORDER key и stable key;
- arbitrary ORDER сохраняет existing spool cursor.

`ORDER BY RAND()` реализовать как специальный ordering operator:

- ключ — versioned RapidHash V3 от statement seed и canonical stable row key;
- в `DETERMINISTIC` результат воспроизводим;
- в `BEST_EFFORT` используется новый process-local statement seed;
- scalar `rand()/uuid()`, `COLLATE` и `NUMERIC` остаются этапу 21.

### 2.7. Строгий `DETERMINISTIC`

Добавить:

```rust
QueryExecutionOptions {
    deterministic_seed: u64, // default 0
}
```

Seed фиксируется при `BEGIN EXECUTION MODE DETERMINISTIC`. Effective seed включает:

```text
version
+ normalized query fingerprint
+ canonical parameter fingerprint
+ SnapshotId
+ user seed
```

`PlanHash` вычисляется RapidHash V3 по versioned canonical physical-plan bytes. Не использовать
`DefaultHasher`, Debug output или iteration order hash maps.

Гарантии:

- только `SNAPSHOT` и `SERIALIZABLE`;
- final stable ordering без явного ORDER BY;
- canonical join/index tie-break;
- фиксированный input order floating aggregates;
- одинаковый plan/result для одного replay tuple;
- `time::now()` и будущие IMPURE functions отклоняются до чтения;
- `FAST` остаётся unsupported до этапа 17.

`EXPLAIN ANALYZE` сохраняет стабильные plan/hash/counts, но elapsed timings являются наблюдением
и не входят в детерминистическую гарантию.

### 2.8. `EXPLAIN`

Единственная форма:

```grynql
EXPLAIN [ ANALYZE | FULL ] [ FORMAT TEXT | JSON ] statement
```

Suffix `... EXPLAIN` удалить из спецификаций.

Поддерживаемые statements:

- SELECT;
- текущее исполнимое MATCH;
- CREATE/UPDATE/UPSERT/DELETE;
- ATTACH/DETACH/RELATE/UPDATE GRAPH;
- DEFINE/ALTER/REMOVE.

Transaction statements, import, control flow, вложенный EXPLAIN и unsupported future statements
отклоняются структурно.

Семантика:

- `EXPLAIN` — normalized/re-written logical plan;
- `FULL` — physical plan, estimates, costs, indexes, fallbacks и resource warnings;
- `ANALYZE` — physical plan плюс actual execution;
- plain/FULL не берут writer lease, не выделяют durable IDs и не читают rows;
- ANALYZE выполняет statement ровно один раз;
- autocommit ANALYZE включает validation/commit/publication timings;
- explicit transaction ANALYZE отмечает publication как deferred;
- DML `RETURN BEFORE/AFTER/DIFF` вычисляется по обычным правилам, но значения не выдаются наружу;
- notifications сохраняются в `Outcome`, payload содержит explain result;
- SELECT ANALYZE полностью потребляет семантический result и не создаёт continuation.

Default — TEXT. `FORMAT JSON` возвращает structured `Value::Object`.

JSON содержит:

- plan version/hash, statement kind, mode, snapshot и seed;
- stable preorder node IDs;
- logical/physical operator и children;
- estimates и cost components;
- required/provided ordering;
- выбранный `IndexId`, name/state/scope и bounds;
- residual predicates;
- statistics revision/method/staleness;
- fallback/resource warnings;
- для ANALYZE: actual rows/pulls, elapsed, spill bytes/runs, peak accounted memory, affected rows и
  publication state.

Добавить `QueryStatementKind::Explain`; публичный Rust plan type не добавлять.

## 3. Dependency-ordered реализация

| Шаг | Зависит от | Результат |
|---|---|---|
| O0. Нормативная синхронизация | этапы 6, 7, 13 | Синхронизировать `gql_spec`, grammar и tech-spec; исправить текущий черновик stage-14 на полный DDL/DML EXPLAIN; подключить план из `full_plan`; снять scan/join/order/MATCH/DML baseline |
| O1. Plan contracts | O0 | Добавить concrete optimizer, physical nodes и canonical query/parameter/order/plan encoder |
| O2. Planner snapshot | O1 | Передавать catalog/index metadata, exact counters и demanded lazy histograms через adapter |
| O3. Estimator и costs | O2 | Реализовать fresh/stale/missing estimates, fixed-point cost и canonical tie-break |
| O4. Rule passes | O1, O3 | Constant fold, pushdown, pruning, late materialization, safe limit/count/order rules с equivalence tests |
| O5. Index paths и hints | O2–O4 | Parser/AST/IR hints, exact/range/IN/full scans, CountLookup, residual predicates, SSI и stale fallback |
| O6. Join alternatives | O3–O5 | DP/greedy ordering, build-side choice, hash/partitioned/block/index nested-loop |
| O7. ORDER и cursors | O5, O6 | Index ordering, tie groups, reverse/range seek, CountLookup и RAND ordering |
| O8. Current MATCH integration | O3, O4 | Заменить локальную heuristic общим estimator/cost/tie-break без stage-15 syntax |
| O9. `DETERMINISTIC` | O1, O6–O8 | Seed/context, stable result/aggregates, strict function/isolation checks и replay tuple |
| O10. `EXPLAIN` | O1–O9 | Prefix AST→IR, read/write plans, TEXT/JSON, real ANALYZE и phase/operator instrumentation |
| O11. Correctness gate | O10 | Differential scan/index/plans, DDL/DML effects, RAM/local/reopen/SSI/determinism/explain matrix |
| O12. Performance и закрытие | O11 | Calibration, profiling, report, status update, `src/gql/AGENTS.md` и `graphify update .` |

## 4. Проверка и критерии завершения

### Correctness

Проверить:

- equivalence каждого rule с optimizer-disabled oracle;
- forced scan против exact/range/IN/composite/Count index;
- Base и exact graph-effective scope;
- null/none/missing property и Standard duplicates;
- Ready/stale/prepared/missing/corrupt index;
- residual predicate после любого probe;
- qualified, unqualified, ambiguous и multi-candidate hints;
- SSI phantom/conflict parity scan/index;
- INNER reorder/build swap bag semantics;
- неизменность LEFT и CROSS;
- DP и greedy result equality;
- ORDER/null/ties/reverse/keyset parity с external sort;
- stale/missing/approximate statistics меняют только plan;
- текущий MATCH до/после optimizer возвращает те же bindings;
- одинаковые PlanHash/result в 20 deterministic runs, RAM/local и после reopen;
- strict rejection `READ COMMITTED`, `time::now()` и IMPURE functions;
- plain/FULL EXPLAIN не вызывают scan/get/next и не получают writer lease;
- ANALYZE DML/DDL действительно публикует в autocommit и остаётся private в explicit transaction;
- ANALYZE failure использует обычный rollback;
- ANALYZE выполняет statement один раз и не создаёт cursor;
- cleanup spill/temp state после timeout/cancellation/planning/runtime errors.

### Performance

Матрица:

- 1K/10K/100K/1M rows;
- selectivity 0%, 0.01%, 0.1%, 1%, 10%, 50%, 100%;
- uniform/skew, fresh/stale/missing histograms;
- Standard/Unique/composite/Count, base/effective;
- joins 2–10 sources;
- ORDER/LIMIT/keyset с unique и duplicate keys;
- low/high-degree current MATCH;
- barrier budget 1/32/256 MiB.

Gates:

- простой no-index SELECT и point DML не хуже baseline более чем на 5%;
- planning p95 для запросов до 8 sources — не более 1 ms и не более 5% wall time для запросов
  дольше 20 ms;
- при selectivity ≤1% и population ≥100K Ready index даёт минимум 3× scan throughput либо отчёт
  доказывает иной bottleneck;
- automatic plan не хуже лучшего compatible forced candidate более чем на 10% median;
- join planning не хуже fixed-right baseline более чем на 5% и выигрывает на skew cases;
- index-ordered LIMIT/keyset не строит full spool;
- обычный execution не получает более 2% instrumentation overhead;
- barrier memory остаётся в установленном shared budget.

Профилирование выполнять только через `--profile profiling` с exact EXE/PDB. CSV, логи и
profiler artifacts сохранять в `target/gql-stage14/`.

Финальные команды включают focused parser/GQL/datastore tests, полные library tests в обеих
feature-конфигурациях, `cargo check`, nightly rustfmt check, focused Clippy и Graphify update.
Существующий workspace-wide lint debt фиксируется отдельно и не маскируется локальными gates.

## 5. Артефакты и отложенная поверхность

Отчёт `docs/reports/gql_optimizer_stage_14_report.md` должен содержать:

- rule/alternative matrix;
- estimator accuracy;
- automatic против forced plans;
- index/stats fallback reasons;
- PlanHash/replay evidence;
- EXPLAIN examples для SELECT, DML и DDL;
- CPU/RSS/spill/planning/execution timings;
- отклонённые alternatives и следующий минимальный эксперимент.

Отложено:

- plan cache — только если O12 докажет значимый planning overhead;
- adaptive re-planning и runtime algorithm switching;
- parallel/vectorized execution;
- WCOJ, bitmap intersection и skip-scan;
- полный MATCH/path cost и algorithms — этапы 15–17;
- `FAST` — этап 17;
- scalar randomness, `time::now()` snapshot semantics, UDF purity, `COLLATE`/`NUMERIC` — этап 21;
- fulltext/vector/spatial/derived indexes — этапы 22–25;
- публичный Rust planner API — не вводится.

Форматы storage/statistics/index pages не меняются, новых зависимостей нет, а публичный
результат остаётся `QueryResult<Value>` без storage DTO.

## 6. Статус выполнения

**Завершено 2026-09-26:** O0–O12.

- Один concrete optimizer подключён между typed IR и sync pull executor; O1–O10 покрыты
  focused и differential tests.
- Полные library gates после финальной декомпозиции: 425/425 без default features и 427/427 с
  `native-async-io`; local deterministic reopen regression прошла.
- Paired profiling подтвердил index choice от 0 до 10% selectivity, scan choice на 50–100%,
  planning p95 10,5–47,4 us и ускорение index ORDER до 356x.
- Финальный profiling EXE/PDB имеет общий stem `ferros_grynn-2170861548b6ea93` и timestamp
  `2026-09-26 19:59:22`; VTune разрешил Rust symbols и source lines.
- Graphify обновлён: 51 548 nodes, 133 507 edges, 1 804 communities.

Полное evidence, CSV/resource paths, rejected alternatives и известные потолки приведены в
[`gql_optimizer_stage_14_report.md`](reports/gql_optimizer_stage_14_report.md). Eager local
rebuild derived state не маскируется завершением optimizer и зарегистрирован как `Q-36` этапа 19.
