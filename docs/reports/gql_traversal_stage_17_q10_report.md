# Этап 17 — закрытие Q-10: physical sparse frontier

**Дата:** 2026-09-27
**Решение:** `Q-10` закрыт положительно; frame-IoRing QD32 включён в production routing для
подходящего `NativeAsync` sparse window.

## 1. Проверяемая граница

Измерение проходит через production path:

```text
Traversal VM
→ transaction query adapter
→ snapshot-exact PhysicalTraversalCursor
→ HybridReadEngine router
→ sync Direct либо frame-IoRing QD32
```

Cursor фиксирует storage snapshot, generation и physical bindings. В одном окне допускается не
более 256 logical operations и 32 cold home pages. До выбора transport выполняются exact R8 и
visible-WAL probes; WAL page не заменяется home page. Результат декодируется только после
полной checksum validation и выходит в GQL как owned occurrence/property/topology.

Production router выбирает IoRing только при одновременном выполнении условий:

- `IoMode::NativeAsync`;
- Windows build с `native-async-io` и доступной IoRing capability;
- known sparse window с `cold_pages > 1`;
- не более 256 logical operations и не более 32 cold frames.

`Sync`, singleton miss и неподходящее окно выбирают sync Direct до начала I/O. Ошибка после
IoRing submission не повторяется другим transport. Пользовательской настройки transport нет.

## 2. Сборка и символы

Использована обязательная profiling-сборка:

```powershell
cargo test `
  --locked `
  --profile profiling `
  -p fg-storage-engine-prototypes `
  --lib `
  --features native-async-io `
  storage_engine_prototype_traversal_frontier_probe `
  -- `
  --ignored --nocapture --test-threads=1
```

Точная пара текущего прогона:

```text
target/profiling/deps/fg_storage_engine_prototypes-d28d7023897fe0fc.exe
target/profiling/deps/fg_storage_engine_prototypes-d28d7023897fe0fc.pdb
```

Оба файла имеют один stem и timestamp `2026-09-27 22:42:23`. Короткий VTune Hotspots capture
сохранён в `target/gql-stage17/vtune-q10-physical-frontier-20260927`; экспорт разрешает Rust
symbol и source `PhysicalTraversalCursor::read_pages` в `physical_traversal.rs`.

## 3. Нагрузка

- один local graph;
- четыре target occurrence и четыре connector edge, разнесённые по hot-page slot ranges;
- `buffer_pool_pages = 1024`;
- по 64 запроса на sample с очисткой resident frames перед каждым запросом;
- datastore переоткрывается для каждого кандидата, поэтому resident state между вариантами не
  переносится;
- 7 paired repetitions с чередованием порядка `Sync`/`NativeAsync`;
- одинаковые logical values проверяются на каждом запросе.

CSV:
`target/gql-stage17/q10-traversal-frontier-01a0e463-da2a-73c2-8b73-5b48102e0f5e.csv`.

## 4. Результаты

| Метрика | sync Direct | frame-IoRing QD32 | Отношение IoRing / Sync |
|---|---:|---:|---:|
| Median wall | 1 343 950 ns | 868 610 ns | 0,646 |
| p99 wall | 2 450 029 ns | 1 290 679 ns | 0,527 |
| CPU total семи samples | 1 708 982 ns | 732 421 ns | 0,429 |

IoRing уменьшил median wall time на `35,4%`, превысив обязательный gate `20%`; p99 и CPU также
улучшились. CPU timer Windows имеет грубую дискретность, поэтому CPU используется только как
проверка отсутствия регрессии, а не как самостоятельная оценка стоимости.

RSS delta был шумным из-за reopen/allocator lifecycle, но положительный максимум IoRing составил
только `28 672` bytes. Transport остаётся внутри существующего transient budget: не более 32
frames, без admission cold pages в resident arena.

EXPLAIN ANALYZE подтвердил фактический production route:

```text
physical_windows = 1
physical_resident_pages = 0
physical_wal_pages = 0
physical_sync_pages = 1
physical_ioring_pages = 8
```

То есть измерение не подменено materialized `GraphRead` или synthetic transport probe: реальный
cursor собрал mixed bounded physical window и отправил cold sparse frames через IoRing.

## 5. Вывод

`Q-10` закрыт. `NativeAsync` получает frame-IoRing QD32 для доказуемого sparse physical
frontier; sync Direct сохраняется для остальных маршрутов. Отдельный async query API,
эвристическое угадывание страниц, runtime transport selector и parallel traversal runtime не
добавлены.

Отдельный gate закрывает V11 и транспортную часть V14. Последующие V13–V15 также завершены;
итог этапа зафиксирован в [`gql_traversal_stage_17_report.md`](gql_traversal_stage_17_report.md).

## 6. Проверки и tooling

- `cargo test --lib --no-default-features local_physical_traversal -- --test-threads=1` —
  `3 passed`;
- та же команда с `--features native-async-io` — `3 passed`;
- profiling probe `storage_engine_prototype_traversal_frontier_probe` — `1 passed`;
- targeted nightly rustfmt check для изменённых traversal-файлов — PASS;
- `graphify update .` — `SKIP`: launcher завершился сообщением
  `Failed to canonicalize script path`; это известная проблема локального Graphify launcher,
  code/test evidence ею не заменяется.

Первый profiling-вызов без `--lib` дополнительно попытался собрать устаревший независимый
`mode1_query_probe` и остановился на удалённом table API. Нормативный library probe после
добавления `--lib` собран и выполнен успешно; obsolete binary не изменялся в рамках Q-10.
