# Отчёт по этапу 20: durable maintenance и admin surface

**Дата:** 2026-09-29  
**Статус:** J0–J14 завершены; `FILL_FACTOR` получил отрицательное решение и
вынесен в отдельную открытую задачу `Q-39`.

## 1. Реализованный срез

- `FGDC v10`, `FGFC v6`, `FGJB v1` и `FGQR v1` являются единственными
  принимаемыми версиями; migration и test-only чтения v9/v5 отсутствуют.
- Один FIFO `MaintenanceCoordinator` владеет durable ledger, worker lifecycle,
  recovery/reconciliation, cooperative cancellation и publication boundary.
- Реализованы foreground/background `REBUILD INDEX`, `ANALYZE`, `COMPACT`,
  `INFO FOR JOB[S]`, `INFO FOR INDEX` и `KILL`.
- `ANALYZE` поддерживает exact `FULL`, bounded deterministic
  `APPROXIMATE SAMPLE 1024..=1_000_000` и заранее разрешаемый `AUTOMATIC`.
- `DEFER` публикует `Stale` вместе с DML, исключает индекс из planner
  candidates, отклоняет forced hint и возвращает `Ready` только после exact
  rebuild и проверки revision под commit lock.
- Atomic Import поддерживает streaming quarantine с payload, `SORT_MEM` и
  ограниченный `PARALLELISM`.
- Final build counters больше не вызывают отдельный metadata-COW/fsync:
  `stage_progress` обновляет coordinator state, а обязательный переход
  `Publishing` сохраняет counters вместе с candidate fingerprint. Crash до
  перехода и без того требует rebuild недостижимого candidate.

## 2. Correctness evidence

Целевые проверки покрывают:

- golden/round-trip и malformed `FGJB`/`FGQR`;
- несовместимые catalog versions и новые file kinds/roles;
- bounded pending queue и coalescing в full rebuild;
- FIFO worker, durable job lookup, `KILL` и recovery fingerprint;
- stale-index scan fallback и forced-hint rejection;
- scoped full/approximate statistics и sample bounds;
- quarantine publication, reject limit, cleanup и streaming reopen;
- canonical external-sort bytes при разных допустимых parallelism;
- co-persist progress counters и clean worker shutdown.

Финальная library matrix текущего checkout:

```text
cargo test --lib --no-default-features -- --test-threads=1
  531 passed; 0 failed; 9 ignored

cargo test --lib --no-default-features --features native-async-io -- --test-threads=1
  533 passed; 0 failed; 10 ignored

cargo check --lib --no-default-features
cargo check --lib --no-default-features --features native-async-io
cargo +nightly fmt --all -- --check
```

Focused worker regression после удаления отдельного progress barrier также
прошёл. Итоговый повтор полной matrix выполнялся после этой правки; команды и
результаты приведены без замены существующего workspace lint policy.

## 3. Методика J13

Baseline взят из Stage-19 closure commit
`ee7923b8af9b68ba217c0acef790ca7ee0e7c649`. Оба варианта собраны только через
`--profile profiling` с debug symbols:

```text
baseline: target/gql-stage20/baseline-target/profiling/deps/
          gql_stage_common-a951172517f1d3e9.exe/.pdb
current:  target/gql-stage20/current-target/profiling/deps/
          gql_stage_common-9a52a6fc2f80c68a.exe/.pdb
stage:    target/profiling/deps/gql_stage20_21-302c454d54725998.exe/.pdb
```

Окружение: Windows 11, Intel Core i5-12450H, 8 физических/12 logical CPUs,
Rust 1.98.1, LLVM 22.1.8. Для шумных mutation probes выполнено девять
ротируемых process pairs; в каждой стороне Criterion собирал десять samples.
Простой query и Stage-21 probes использовали по 100 samples.

## 4. Результаты J13

### 4.1. Baseline regression

| Probe | Current | Stage-19 control | Ratio | Gate | Итог |
|---|---:|---:|---:|---:|---|
| `RETURN 1` | 3,5776 µs | 3,5880 µs | 0,9971× | ≤1,05× | PASS |
| DML без индекса, paired median | — | — | 0,9913× | ≤1,05× | PASS |
| DML с Immediate index, paired median | — | — | 1,0146× | ≤1,05× | PASS |
| Foreground Standard build, 10K rows | 12,024 ms | 12,286 ms | 0,9787× | ≤1,05× | PASS |

Отдельные mutation pairs имели большой системный разброс
(`0,7308–1,1978×` без индекса и `0,7775–1,1420×` с индексом), поэтому решение
основано на заранее выбранном paired median, а не на одном процессе.

### 4.2. DEFER, resources и defaults

| Probe | Variant | Control | Result | Решение |
|---|---:|---:|---:|---|
| 2K bulk mutation | 693,54 ms Deferred | 1 058,60 ms Immediate | −34,5% | gate ≥10% пройден |
| Peak RSS того же subprocess | 15 134 720 B | 15 220 736 B | −0,6% | скрытого RSS роста нет |
| Rebuild parallelism 8 | 876,99 µs | 909,69 µs при 1 | +3,6% | ниже 15%; default остаётся 1 |
| `RETURN 1` с terminal job | 3,8114 µs | 3,8412 µs empty ledger | −0,8% | idle overhead <1% |

`DEFER` остаётся явным schema policy: измерение подтверждает его полезность, но
не меняет default `Immediate`, поскольку доступность индекса является частью
явного DDL-контракта.

Первый local end-to-end control показал 77,179 ms для concurrent rebuild против
63,108 ms foreground. Разница включает enqueue, worker handoff и все durable
phase transitions, а не только progress. Профиль кода выявил лишний отдельный
progress metadata-COW. Он удалён: теперь на job приходится ноль дополнительных
progress publications, а counters co-persist с обязательным `Publishing`.
Таким образом progress-write overhead не масштабируется с rows/runs и не может
доминировать build; общий durability/scheduling overhead background mode
остаётся наблюдаемой ценой `CONCURRENTLY`, а не скрывается как builder cost.

### 4.3. `FILL_FACTOR` и `Q-39`

Diagnostic на 50 000 entries дал:

| Index | Dense pages | Fill 0,90 | Рост | Fill 0,80 | Рост |
|---|---:|---:|---:|---:|---:|
| Primary | 90 | 100 | +11,1% | 113 | +25,6% |
| Standard | 118 | 131 | +11,0% | 147 | +24,6% |

Incremental mutations на свободных leaves ускоряются, но page-set является
постоянной ценой всех последующих чтений: каждый лишний 16-КиБ page увеличивает
cold I/O, cache pressure, storage footprint и работу reopen/checkpoint. Поэтому
лимит выбран равным общему проектному бюджету hot-path regression — `5%`.
Это не доверительный интервал: число созданных страниц детерминировано. Более
высокий лимит разрешил бы постоянный двузначный read-side налог ради условной
будущей записи и не является нейтральным default.

Оба проверенных значения gate не прошли. `FILL_FACTOR` остаётся распознаваемым,
но typed unsupported параметром `BEGIN IMPORT`; его будущий bounded/adaptive
вариант отслеживается отдельно как `Q-39`. Fixed internal policies mutable
indexes из `Q-05` не меняются.

## 5. Profiling и артефакты

Valid user-mode VTune profiles собраны установленным VTune 2026.4.0 на exact
EXE/PDB. Common query profile сохранил общий CPU time: 9,807 s current против
9,816 s baseline; `GqlSession::execute_internal` занял 2,7% против 2,4%, а
нового maintenance/query hotspot не возникло. Hardware sampling без elevated
driver не использовался как доказательство.

Артефакты:

- `target/gql-stage20/j13-summary.csv` — все девять mutation pairs, aggregate
  ratios, RSS, defaults и fill decision;
- `target/gql-stage20/*.log` — Criterion и resource probes;
- `target/gql-stage21/vtune-current-return` и
  `target/gql-stage21/vtune-baseline-return` — valid source-level profiles;
- `target/gql-stage20/fill-factor.log` — per-fill page counts и timings.

## 6. Закрытие J14

- `Q-33` сужен до `CHANGEFEED=ON`/`EVENTS=ON`; `FILL_FACTOR` вынесен в `Q-39`.
- `PARALLELISM > 1` не стал default; `DEFER` сохранил explicit schema policy.
- `FGDC v10`/`FGFC v6` повторно зафиксированы в core-v1 manifest.
- План, living specs, module AGENTS и `full_plan.md` синхронизированы.
- Graphify успешно обновлён.

Strict workspace Clippy по-прежнему блокируется зарегистрированным `Q-14`:
тысячами ранее существующих documentation/privacy-layout diagnostics. Lint
levels не ослаблялись, и этот долг не маскируется локальными `allow`.
