# Этап 24 — Geometry R-tree: evidence checkpoint

Дата: 2026-10-01. Статус: **реализуется; этап не закрыт**.
Нормативный контракт: [план этапа](../gql_geometry_stage_24_plan.md),
[GQL](../gql_spec.md), [storage](../disk_storage_spec.md).

Повторный correctness gate после Cow statistics и ограничения eager physical snapshot
локальным backend пройден: `correctness-gate-v4.log`. Непрофильный steady SELECT на RAM
100K (100000 observations) дал 20,0 → 20,6 мкс (+3%). Семь DML observations дали
18,97 → 24,41 мс; этот замер превышает 5% и требует отдельного controlled повторения.
Performance gate не закрыт. Stage 25 D0 начат после correctness gate согласно плану.

## Реализованный срез

Собственные public Geometry/Value/RecordId сохранены. `geo = 0.33.1` подключён без
default features для validation, OGC kernels и WGS-84 Point distance. Входные кольца
проверяются до Polygon conversion; validation применяется к GeoJSON/Value conversion,
Rust SDK writes, storage codec, predicates и index builds. Eq/Ord/Hash и canonical
geometry encoding не заменены библиотечными типами.

Geometry DDL проходит lexer/parser/AST → analysis/IR → execution/catalog/API.
Spatial operators и quantifiers используют exact kernels; empty/missing/null и
boundary behavior проверены. Для overlapping GeometryCollection используется bounded
arrangement без quantization. Read-only review нашло две ошибки (line boundary и
budget обход через повторённые координаты); исправления закреплены regression tests.

STR base: page-backed потоковый writer, leaf/internal record 40 bytes, capacity 408,
fill 387/408, stable identity ties, bounded external identity/X/Y sorts. Run metadata
ограничена 256 KiB на sorter; равные поколения runs сливаются online. RAM удерживает
тот же page format в окончательных aligned allocations. Local publication использует
существующие COW/WAL jobs. Delta — exact replacement MBR/tombstone; старый snapshot
закрепляет прежнюю generation до первого index query. Deferred definitions используют
существующий atomic Stale lifecycle. Private writes исключают artifact из своих reads.

Planner использует candidates только для положительных INSIDE/INTERSECTS с обязательным
exact refinement. Отрицательные predicates/distance сохраняют scan. Geometry sample
ANALYZE ограничен 1024 MBR и отдельно хранит nonempty population. EXPLAIN показывает
binding/scope/state/estimate/post-filter/delta/fallback; ANALYZE — actual candidates,
refinements, fallbacks, tree-node pages и delta records. Tree-node pages включают
resident hits; это не счётчик всех native I/O, metadata/delta decode измеряются отдельно.

## Correctness evidence

Toolchain: rustc/cargo 1.98.1, Windows MSVC, edition 2024,
`--no-default-features --locked --offline`, serial test execution.

| Команда/target                                         |                                                                       Результат | Raw log                                   |
|--------------------------------------------------------|--------------------------------------------------------------------------------:|-------------------------------------------|
| `cargo test --lib ... datastore:: -- --test-threads=1` |                                                                       72 passed | `target/stage24/datastore-gate-v2.log`    |
| Full lib + RAM/local integration gate                  | 605 lib, 4 RAM, 15 local, 1 geometry Sync passed; 10 existing lib tests ignored | `target/stage24/correctness-gate-v3.log`  |
| Geometry-focused lib after work counters               |                                                                       23 passed | `target/stage24/test-geometry-v8.log`     |
| Separate NativeAsync geometry target                   |                                                                        1 passed | `target/stage24/test-geometry-native.log` |

Проверены holes/boundaries/empty/depth/large MultiPoint, malformed rings/pages,
MBR false positives, eager quantifier validation, scan/index error order, BASE/EFFECTIVE
overlays/RESET, delta replacement/tombstone/compaction, old snapshots/private writes,
query memory/cancellation, checkpoint/reopen. Общий prepublication failure matrix
включает geometry side-file для AfterBuild/AfterFileSync/AfterCatalogStage/BeforeWalCommit.
Последующие изменения ещё требуют повторного полного gate перед закрытием.

## Предварительные performance observations

Публичный paired probe: `tests/geometry_performance_probe.rs`, `--profile profiling`,
same population, exact scan oracle, alternating query samples. Baseline — текущее
состояние без Geometry definition; это не восстановленный historical baseline.

| RAM rows | Scan median | R-tree median |     Build |   Rebuild |
|---------:|------------:|--------------:|----------:|----------:|
|      10K |    17.77 ms |      0.215 ms |  25.60 ms |  10.91 ms |
|     100K |   163.19 ms |      0.225 ms | 182.13 ms | 121.46 ms |
|       1M |     11.67 s |      0.302 ms |    3.80 s |    2.87 s |

Raw logs: `target/stage24/perf-ram-10k.log`, `perf-ram-large.log`.
Local Sync 10K: warm scan 14.80 ms, index 0.219 ms; after reopen query 18.24 ms
для scan и 1.38 ms для index. Geometry mutation записала 1,463,413 WAL bytes в данном
workload, вместе с существующей primary publication. Это не isolated delta-only bytes.

На 100K выявлено превышение +5% в непрофильном point query. Повтор с 51 samples:
11.4 → 12.6 µs (+10.5%); DML 24.18 → 23.08 ms. Local Sync 10K point query:
13.3 → 14.1 µs (+6.0%). Эти результаты **не проходят performance gate**.
Идёт VTune investigation на точном EXE/PDB; преждевременного статуса Ready этапа нет.

## Открытые gates

- Довести непрофильные regressions до ≤5% по воспроизводимым paired measurements.
- Завершить Local Sync/NativeAsync 100K/1M, repeated cold measurements и peak-memory evidence.
- Повторить full correctness/resource/integration gate после последних изменений.
- Nightly rustfmt/check и Clippy: targeted formatting выполнялся; full Clippy пока
  не проходит из-за repository-wide diagnostics, преимущественно private docs/layout.
  Новые geometry diagnostics исправляются отдельно; full Clippy success не заявляется.
- Выполнить финальный `graphify update .`, актуализировать registry/AGENTS и статус по evidence.

Этап 25 допускается после correctness gate; открытый performance gate этапа 24
сохраняет этап 24 незавершённым. Этапы 22–23 не добавляются к prerequisites этапа 25.
