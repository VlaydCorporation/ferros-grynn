# Этап 19. Windows GA-core gate и freeze core v1

**Статус (2026-09-29): ✅ завершён.** W0–W9 и обязательные correctness,
performance и public-freeze gates пройдены. Единственный источник общего
прогресса — [`full_plan.md`](full_plan.md); итоговое evidence сохранено в
[`reports/windows_ga_core_stage_19_report.md`](reports/windows_ga_core_stage_19_report.md).

## 1. Граница этапа

Этап 19 не добавляет новый язык или storage feature. Он закрывает измеренные
release-blockers существующего Windows core, проверяет уже реализованные
контракты этапов 3–18 и после успешного gate фиксирует:

- `STORAGE_FORMAT_VERSION = 1`, `FGDC v9`, `FGFC v5` и page/WAL codecs;
- публичный Rust API datastore/graph/value/query core;
- исполнимый GrynnQL-profile этапов 8–18;
- Windows `Sync` и feature-gated `NativeAsync` как проверенные local modes.

Отложенные `Q-09`, `Q-30`–`Q-33` и возможности этапов 20+ не маскируются
заглушками и не входят в GA-profile. Advanced index side-files сохраняют
собственные версии и не меняют core-format.

## 2. Dependency-ordered выполнение

| Шаг                          | Зависит от | Результат                                                                                                                                                                                      |
|------------------------------|------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| W0. Release inventory        | этап 18    | Зафиксировать exact checkout, toolchain/features, открытые release-blockers, текущие format/API/GrynnQL surfaces и baseline-команды                                                            |
| W1. Runtime resources        | W0         | Закрыть `Q-04` snapshot-aware retirement; сделать `LocalOptions` единственной embeddable authority; вывести cache/WAL/retirement pressure; выполнить long-run/multi-file sweep для `Q-11/Q-13` |
| W2. Serializable concurrency | W0         | Повторить disjoint-writer probe 1/2/4/8; проверить reservation ranges и merge ownership; закрыть `Q-06` без ослабления SSI/FCW                                                                 |
| W3. Reopen/startup           | W1         | Разложить startup time на catalog validation, file open и derived rebuild; решить `Q-22/Q-36` только по 1/100 graphs и 1K/100K/1M atoms × 0/1/8 indexes                                        |
| W4. Format freeze            | W1, W3     | Сравнить full-key с prefix/block compression для `Q-20`; оставить один доказанный codec; собрать golden/malformed vectors и freeze manifest core v1                                            |
| W5. Crash/fuzz               | W2–W4      | Crash-point recovery/checkpoint/catalog/WAL matrix; bounded codec/parser fuzzing без panic/OOM; corpus и seeds сохранить в artifacts                                                           |
| W6. Differential/soak        | W5         | RAM/local, Sync/NativeAsync, reopen/checkpoint equality; долгие SSI/checkpoint/snapshot-retirement tests                                                                                       |
| W7. Performance              | W3, W6     | Storage/query paired benchmarks, one-statement `Q-34`, R8/multi-file/concurrency pressure; оптимизировать только подтверждённый hotspot                                                        |
| W8. Public freeze            | W4, W7     | Зафиксировать Rust API и supported GrynnQL matrix; удалить временные prototype hooks, объяснить либо упростить semantic double-Option и сузить lint debt `Q-14`                                |
| W9. Closure                  | W8         | Полные checks обеих feature-конфигураций, report, registry statuses, Graphify update и отметка этапа 19                                                                                        |

W2 может выполняться независимо от W1 после W0. W4 запрещено завершать до
W3: startup/recovery должны читать именно замораживаемый format.

### Текущий прогресс

- W0 завершён: release inventory и обязательные gates зафиксированы этим
  документом.
- W1 реализовал snapshot-aware retirement и runtime introspection. `Q-04` и
  `Q-13` закрыты; long-run и multi-file решение `Q-11` завершено в W7.
- W2 завершён: commit replay переводит private physical allocations в
  актуальные slots под commit lock, не ослабляя логические FCW/SSI conflicts.
  Пять paired повторов дали `0` aborts для `1/2/4/8` disjoint writers; same
  `RecordId` и same membership продолжают возвращать serialization failure.
- W3 завершён: registry валидирует catalog/path/length без eager native handle,
  statistics roots загружаются при reopen, а независимые auxiliary parts и
  explicit Standard/Unique/Count indexes декодируются только по demand. Перед
  первой транзакционной мутацией они строго гидратируются. Матрица
  1K/100K/1M × 0/1/8 подтвердила, что
  количество secondary indexes больше не масштабирует reopen; `Q-22` и `Q-36`
  закрыты. Остаточный `O(atoms)` materialization bottleneck зафиксирован, но не
  маскируется новой speculative архитектурой.
- W4 завершён: exact profiling EXE/PDB проверил full-key против page-prefix на
  100 000 canonical keys. Prefix давал `25,2–76,1%` меньше страниц, но один
  packing-layout pass был в `5,1–9,9×` дороже и не имел end-to-end read/mutation/
  recovery evidence. Full-key оставлен единственным core-v1 codec, block/prefix
  drafts и несуществующий property-LZ4 contract удалены. Freeze manifest:
  [`storage_core_v1_freeze_manifest.md`](storage_core_v1_freeze_manifest.md).
- W5 завершён: `26` WAL recovery tests, pre-publication crash points, orphan
  cleanup и оба authorized checkpoint-COW сценария прошли. Parser и canonical
  value decoder проверены по `4096` bounded proptest cases с фиксированными
  seeds `19001901` и `19001902`; corpus manifest сохранён в
  `target/windows-ga-core-stage19/w5_crash_fuzz_evidence.txt`.
- W6 завершён: RAM `3/3`, Sync local `15/15`, NativeAsync local `16/16` и
  local-store `11/11` production tests прошли. Exact lib-test binary выполнил
  `420` soak cases SSI/checkpoint/retirement без отказов. Reopen снова читает
  только statistics roots: planner допускает conservative fallback повреждённой
  optional auxiliary part, а первая mutation строго отклоняет corruption без
  публикации. Evidence:
  `target/windows-ga-core-stage19/w6_differential_soak_evidence.txt`.
- W7 завершён: long-run R8, multi-file/concurrency и общий memory budget
  прошли minimum gates без directory/epoch sharding, per-file quotas и
  auto-sizing. Повторный paired probe дал median `1,0497×` для текущего
  one-statement driver против direct control, а простой SELECT — `0,9493×`
  baseline; `Q-11` и `Q-34` закрыты. Software VTune подтвердил Rust symbols и
  source lines; hardware Hotspots помечен `SKIP`, поскольку sampling driver не
  доступен без administrative setup.
- W8 завершён: временный метод измерения удалён из публичного `Datastore`, а
  non-default prototype boundary явно исключён из GA API; семантика двойных
  `Option` документирована, stale parser constants удалены. `Q-14` сужен до
  существующего documentation/privacy-layout Clippy debt и остаётся отдельной
  maintenance-задачей.
- W9 завершён: полные library tests дали `499 passed` без optional I/O и
  `501 passed` с `native-async-io`; checks, nightly rustfmt, warning-level
  focused Clippy и prototype library check прошли. Полный gate дополнительно
  обнаружил и исправил преждевременное закрытие replayable index cursor.
  Graphify обновлён штатным module entrypoint.

## 3. Утверждённый runtime-resource contract

- `LocalOptions` — единственная authority local datastore. Environment
  variables не переопределяют переданные значения.
- `buffer_pool_pages` измеряется в 16-КиБ frames и является общим resident
  budget всех файлов, графов и версий.
- transient pages вычисляются внутренне как `min(288, floor(P / 10))`;
  пользовательского QD/prefetch/policy knob нет.
- `Datastore::options()` возвращает construction authority, а
  `Datastore::local_storage_status()` — read-only effective budgets и
  cumulative pressure. In-memory backend возвращает `None`.
- Process-wide allocator tracking является наблюдением, а не скрытым admission
  policy; прежний `SURREAL_MEMORY_THRESHOLD` не поддерживается.
- После durable COW publication прежний file registry становится retirement
  token. Exact physical snapshots удерживают его `Arc`; runtime cleanup хранит
  только `Weak` markers и удаляет UUID-generation после release.
- Ошибка удаления после публикации не превращает успешный commit в rollback:
  directory остаётся pending, failure counter увеличивается, следующая COW
  publication либо explicit checkpoint повторяет удаление.
- Crash до/после publication по-прежнему очищается startup-GC из durable
  catalog. Никакой background thread или отдельный GC log не вводится.

Жёсткая граница disk churn: текущие catalog generations плюс по одному
retired generation на реально живой exact physical snapshot и directories с
неустранённой OS cleanup error. Без живых snapshots и ошибок каждый COW commit
оставляет только текущее поколение.

## 4. Correctness gates

- snapshot pin, release/retry, deletion failure и crash orphan;
- WAL torn tail, catalog/superblock COW crash points и checkpoint watermarks;
- malformed/truncated/non-canonical page, WAL, `FGDC`, `FGFC` и value bytes;
- parser/token/depth fuzz corpus без panic и unbounded allocation;
- одинаковые committed results RAM/local Sync/NativeAsync и после reopen;
- SSI phantom/FCW, concurrent DDL/DML/import/build и snapshot expiry;
- no partial publication при cancellation, I/O, checksum или validation error;
- unsupported Stage-20+ surface остаётся typed unsupported до side effects.

## 5. Resource/performance gates

- resident + transient frame budget не превышает 110% configured pool;
- metadata, locator/source counts, WAL backlog и retirement queue выводятся
  отдельно;
- без snapshot/error generation count не растёт от числа COW commits;
- простой one-statement execute и no-index SELECT не хуже зафиксированного
  baseline более чем на 5%; `Q-34` требует ratio `<= 1.05`;
- startup regression 1/100 graphs и 1K/100K/1M atoms объяснён phase timings;
- automatic path не хуже лучшего compatible control более чем на 10%;
- instrumentation overhead не более 2%;
- memory/RSS не растёт как `O(rows)` там, где контракт обещает bounded spill.

Профилирование выполняется только `--profile profiling` с exact EXE/PDB.
CSV, seeds, crash logs и profiler artifacts сохраняются в
`target/windows-ga-core-stage19/`.

## 6. Финальный gate

Обязательны focused tests каждого W-step, полные library tests с
`--no-default-features` и `native-async-io`, `cargo check` в обеих
конфигурациях, nightly rustfmt check, focused Clippy и `graphify update .`.
Этап не закрывается по одному успешному focused-test: в отчёте должны быть
команды, результаты, SKIP с причиной и остаточные blockers.
