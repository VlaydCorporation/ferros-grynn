# Отчёт этапа 8.1: переход на graph-first модель атомов

Дата завершения: 2026-09-22.

## Итог

Этапы A5–A16 завершили полный переход без compatibility path:

- `RecordId = class:id` адресует один database-global atom;
- `CLASS` владеет namespace, immutable `NODE|EDGE` kind, полями и Base indexes;
- AtomStore хранит `RecordId`, class, kind и base properties один раз;
- graph хранит только occurrence, local `AtomId`, label, topology/weight,
  nested bindings и sparse overlay;
- reverse memberships связывают global atom со всеми graph occurrences;
- `GraphId` всегда ненулевой, database files адресуются через
  `DataSpaceId::Database`;
- `TableSchema`, `AtomTableMemberships`, system graph, runtime selector и
  test-only compatibility grammar отсутствуют в собираемом коде.

Текущий page/WAL format не мигрирует промежуточные кандидаты. Исторические
отчёты этапов 5–9 остаются снимками прежней TABLE-модели и не являются описанием
текущего checkout.

## Реализованные шаги

### A5–A10: модель, MVCC и durable layout

- Введены `ClassId`, `ClassSchema`, `GlobalAtomId`, database-global
  `AtomStoreRoot`, graph-local `GraphOccurrence`, exact reverse memberships и
  ленивые overlays.
- FCW/SSI разделяют `BaseAtom`, `GraphMembership`, `GraphOverlay`,
  `GraphIncidence` и range predicates. Fanout base mutation вычисляется только
  по reverse memberships изменяемого atom и повторно проверяется при commit.
- `AtomStoreSuperblock`, `AtomHotPage`, property heap/dictionary, global primary,
  graph occurrence pages и оба membership indexes публикуются одной WAL
  транзакцией и читаются по одной snapshot boundary.
- Base/Effective Standard, Unique и Count, class/label indexes и scoped
  statistics используют typed scope; Effective Unique повторно проверяется до
  публикации.

Подробный byte/recovery результат A8 сохранён отдельно в
[`atom_store_stage_a8.md`](atom_store_stage_a8.md).

### A11: DDL

Вертикальная цепочка parser → analysis → typed IR → execution реализует
`CLASS`, `FIELD ON CLASS`, scope-aware `INDEX` и `INFO FOR DB|CLASS|INDEX`.
Class kind после публикации неизменяем. `TABLE` отвергается parser regression,
а system-table data path удалён.

### A12: direct и effective DML

`CREATE`, `UPDATE`, `UPSERT`, `DELETE` и `SELECT` адресуют global atoms напрямую
по `RecordId`. Graph scope читает и изменяет effective view; base non-member
остаётся доступен при record dereference. Удаление attached atom по умолчанию
использует `RESTRICT`, а `CASCADE` проходит полный preflight до изменения
private root.

### A13: graph DML

Реализованы `ATTACH`, `DETACH`, class-aware `RELATE ... IN GRAPH`, независимые
alias/label, graph-local label update и `RESET`. Один edge RecordId может иметь
разные topology, weight и label в разных графах. Family validation выполняется
до commit; ошибка DAG cycle не оставляет частичного statement effect.

### A14: references и storage modes

- Mode 1 использует тот же normalized MVCC root без storage path.
- Mode 2 восстанавливает AtomStore, occurrences, overlays и memberships из
  local WAL/checkpoint без изменения публичного результата.
- Mode 3 создаёт owned effective graph, не удерживающий source transaction.
  Mutable materialization хранит origin mapping и выполняет только явный
  delta write-back; `Drop` не пишет.

## Correctness gate A15

Финальная матрица на Windows MSVC:

| Проверка | Результат |
|---|---:|
| `cargo test --locked --lib --no-default-features -- --test-threads=1` | 307 passed, 1 ignored |
| `cargo test --locked --lib --no-default-features --features native-async-io -- --test-threads=1` | 309 passed, 2 ignored |
| Integration tests, sync | 3 in-memory + 4 local + 1 public-path passed |
| Integration tests, native async | 3 in-memory + 5 local + 1 public-path passed |
| Intermittent combined AtomStore/graph WAL regression | 100/100 process runs passed после исправления |
| `cargo check --locked --lib --no-default-features` | PASS |
| Та же проверка с `native-async-io` | PASS |
| `cargo +nightly fmt --all -- --check` | PASS |
| `cargo clippy --locked --lib --no-default-features` | BLOCKED: 3074 известных workspace lint errors, `Q-14` |

Матрица покрывает shared base/isolated overlay, `RESET`, разные labels и edge
topology в двух graphs, unattached direct records, exact/class/label/intersection
MATCH, Base/Effective indexes, Unique conflicts, graph removal, stale IDs,
FCW/SSI phantoms, bounded fanout, WAL reopen, checkpoint recovery и Mode 3
write-back. Native IoRing tests проходят; hardware QD calibration остаётся
явно ignored host-calibration test, как и до этого этапа.

### Исправленная MSVC-проблема over-aligned pages

Полная native-async матрица обнаружила прежний класс `Q-27` на ещё одном
call site: первый byte magic `AtomStoreSuperblock` или `GraphSuperblock`
иногда становился нулём до первого Direct-I/O write и до создания WAL.
Проверка после каждого шага локализовала изменение между валидным `encode()` и
входом в первый catalog loop, то есть в долгоживущем перемещаемом
`#[repr(align(16384))]` stack value.

Обе страницы test generation builder теперь находятся в стабильных `Box`;
аналогичные production builders уже кодируют populated pages в окончательном
`Vec`-slot/arena. Page codec, checksum и I/O contract не менялись. После
удаления диагностики exact regression прошла 100 отдельных процессов, затем
прошла вся native-async матрица.

Уточнение от 2026-09-23: формулировка про перемещаемое stack value была сильнее имеющихся
доказательств. Последующее исследование подтвердило место симптома до I/O и пользу стабильного
конечного storage, но не нарушение alignment и не универсальный сбой перемещения. Текущая
классификация и матрица анализаторов приведены в
`aligned_page_corruption_investigation.md`.

## Performance gate A15

Измерения выполнены Criterion в release/profiling codegen на том же Windows
host (Intel Core i5-12450H, 8 cores / 12 logical CPUs, 16 874 201 088 bytes
RAM). Во время финальных прогонов использовался подтверждённый пользователем
maximum-performance mode, parking отключён, питание от сети. Значения ниже —
central regression-slope estimates; legacy получен из сохранённого исходного
среза до cutover.

| Workload | Legacy | Graph-first | Изменение | Gate |
|---|---:|---:|---:|---:|
| Graph point read | 77,317 ns | 62,142 ns repeated base | −19,6% | PASS, предел +5% |
| Graph no-overlay projection | 77,317 ns | 26,769 ns | −65,4% | PASS, предел +10% |
| Overlay projection | — | 103,23 ns | отдельная owned merge cost | diagnostic |
| Class scan, 1024 atoms | 652,694 µs | 427,03 µs | −34,6% | PASS, предел +5% |
| Label scan, 512 из 1024 | 752,654 µs | 721,035 µs | −4,2% | PASS, предел +5% |

No-overlay `TransactionGraphView` возвращает `Cow::Borrowed` на base
`PropertyMap`; regression проверяет Borrowed до появления overlay и Owned после
него. Поэтому point path не создаёт effective property copy и не обращается к
отсутствующей overlay map.

Fanout base update:

| Memberships изменяемого atom | Время |
|---:|---:|
| 1 | 4,5847 µs |
| 8 | 11,031 µs |
| 32 | 40,558 µs |

Рост зависит от memberships одного atom, а не от общего числа graphs:
reverse-membership regression добавляет посторонние пустые graphs и получает
тот же выбранный fanout. Shared payload хранится как один `Arc<BaseAtom>`;
каждый graph добавляет только occurrence/reverse-membership metadata и
необязательный overlay.

Mode 1 class/label query workloads не показали регрессии, поэтому предел 3%
пройден. Этот этап не менял `HybridReadEngine`, transport routing или page hot
loop; их production correctness покрыта обеими feature-конфигурациями, а
performance evidence остаётся в
[`storage_engine_hybrid_integration_report.md`](storage_engine_hybrid_integration_report.md).
Повторный полный storage-engine sweep без изменения его кода не выполнялся.

## Профилирование

Профилируемый benchmark собран с `--profile profiling`; использована точная
пара:

- `target/profiling/deps/atom_model-86256ed21e76f1a1.exe`;
- `target/profiling/deps/atom_model-86256ed21e76f1a1.pdb`.

VTune 2026.4 Hotspots разрешил Rust function names и source lines. Финальный
label-scan capture и экспорт находятся в:

- `target/graph-first-atom-model/profilers/vtune-final-label-scan-rapidhash-profiling`;
- `target/graph-first-atom-model/profilers/vtune-final-label-scan-rapidhash-source-stack.csv`.

После замены transaction SSI read-set с `BTreeMap` на `RapidHashMap` прежний
tree insertion/allocation subtree исчез из hot path. Оставшиеся основные
samples относятся к созданию owned query rows/results и их property maps.
Оптимизировать эту следующую границу в A16 не требуется: label gate уже пройден,
а смена публичного result ownership затронула бы отдельный query-streaming
контракт. Hardware microarchitecture counters были недоступны без elevated
VTune driver session; user-mode Hotspots и source resolution прошли.

## Cutover A16

- Production и test paths используют только graph-first layout; fallback и
  selector отсутствуют.
- Поиск по `src/` и `tests/` не находит `TableSchema`,
  `AtomTableMemberships`, system graph или `GraphId=0`.
- Единственное упоминание `DEFINE TABLE` в коде — отрицательный parser test.
- Финальный review выявил обход `LABEL` в exact-record ветке MATCH. Проверка
  перенесена в общий `query_match_candidates`; regression test проходит в
  sync и `native-async-io` конфигурациях.
- Модульные `AGENTS.md` синхронизированы с Class/AtomStore/occurrence моделью.
- Оба compile checks и nightly rustfmt проходят. Полный Clippy остаётся явно
  зарегистрированным общим долгом `Q-14`, а не скрытым успешным gate.
- Реестр закрывает `Q-08`, `Q-23`–`Q-29` в реализованной части; отложенные
  transaction semantics, GC, configuration и advanced indexes не исполнялись
  наперёд.

## Оставшиеся ограничения

- Нет class inheritance, multi-class atom и автоматической смены class.
- Один occurrence имеет не более одного optional label.
- Нет отдельного durable `InstanceId`: instance — membership + sparse overlay.
- Cross-database identity/bridges, canonical columnar property storage и
  автоматический index-layout selection принадлежат последующим этапам.
- `ReadCommitted` statement refresh/savepoint semantics остаются `Q-07`.
- Mutable Mode 3 write-back покрывает текущие graph-local deltas; расширение
  conflict UX выполняется только при появлении соответствующего публичного API.
