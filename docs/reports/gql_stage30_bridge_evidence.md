# Этап 30 — evidence реализации Bridges

Статус: **IMPLEMENTED**, собственные correctness/recovery/resource/performance
gates проверены в указанном ниже scope. Этот документ не закрывает performance
gate этапа 29 и общий conformance gate этапа 31.

## Область и исходное состояние

Checkout: `mvp`, HEAD `2a7f46570fda31438ce2c9857657480e29ec886b`. В checkout
были unrelated staged/unstaged изменения и reference trees; они сохраняются.
Исходный root-library gate без defaults: 1334 PASS, 10 ignored; лог
`target/gql-stage30-spike/root-baseline.log`. Результаты ниже относятся к текущему
изменённому срезу, а не к чистоте всего рабочего дерева.

API/async граница — `src/bridge`, registry/MVCC — `src/datastore`, immutable
generation/codec — `src/storage`; parser → analysis → IR → существующий executor
и assembler сохраняются. Typed SDK conversions не используют Display/JSON.
SurrealDB 3.3.4 отключает defaults, Mem/Rocks включаются отдельно. Workspace geo
зафиксирован на 0.32.0: SDK и прежний geo 0.33.1 требовали несовместимые minor
ranges одного `i_overlay`; геометрические проверки входят в root-library gate.

Draft cutover: FGDC 15 / FGFC 7, bridge records FGBR 1, file kind 0x19.
Старые каталоги отклоняются; миграция или переписывание при open отсутствуют.
Одно authoritative mapping содержит immutable intent; два uniqueness indexes
возвращают его ID. Большие записи живут в независимом slotted heap и B+trees,
не в ограниченном общем catalog payload. Immutable generation синхронизируется
до WAL publication её reachability. Writer заранее проверяет тот же maintenance
budget, который нужен recovery, включая 64-МиБ codec scratch.

## VerificationRecord

Все Cargo commands используют stable 1.98.1 и MSVC static-CRT flags проекта.
Для native SDK используются `AWS_LC_SYS_PREBUILT_NASM=1`,
`LIBCLANG_PATH=D:/Programming/SDKs/LLVM/bin`, `CARGO_BUILD_JOBS=1`.

| Gate | Статус | Доказательство и предел |
|---|---|---|
| `cargo +stable check -p ferros-grynn --tests --no-default-features` | PASS | `bridge-final-core-check.log`; frontend/test slice до последних fixture edits |
| `cargo +stable test -p ferros-grynn --lib --no-default-features bridge -- --test-threads=1` | PASS | `bridge-final-operators3.log`: 19 tests. Nested FETCH, two databases, cache reuse, EXPLAIN без native IO, loops/families, duplicate recipes и atomic BUILD failure |
| Root-library no-default | PASS | `root-stage30-closure.log`: 1355 PASS, 10 ignored, включая REG-08/09 admission и catalog-only fault hooks. Feature matrix runs приведены отдельно |
| Registry publication failures | PASS | `bridge-publication-faults2.log` и final root gate: AfterBuild/AfterFileSync/AfterCatalogStage/BeforeWalCommit для reservation и Ready; no leaked generation, Pending/Ready reopen, no duplicate atom changefeed entries |
| Pending overflow/import regression | PASS | Full root gate: 3-МиБ Pending→Ready reopen, отказ второго большого row при прежнем 72-МиБ budget, import CASCADE→catalog-only→reopen tombstone |
| Mem consumer runtime/link | PASS | `bridge-mem-consumer5.log`: оба направления, none/null/large int, read-only CREATE/SLEEP, namespace isolation, реальный mirror/unknown commit/resume/FETCH |
| RocksDB production check | PASS | `bridge-rocks-check.log`; type checking, без consumer-link вывода |
| RocksDB consumer с полной debug information | FAIL (environmental) | `bridge-rocks-consumer2.log`: Windows mmap/pagefile error 1455 при чтении почти 2-ГиБ root rlib |
| RocksDB consumer runtime/link/reopen | PASS | `bridge-rocks-consumer5.log`, `--config 'profile.dev.package.ferros-grynn.debug=0'`; assertions/debug behavior сохранены, меньше только symbols |
| RocksDB shutdown boundary | PASS | Reopen идёт на одном runtime; после drop клиента bounded probe ждёт освобождения native LOCK. SDK завершает session tasks асинхронно |
| Stable Clippy core library | PASS (slice), FAIL (repository debt) | `bridge-clippy-final2.jsonl`: 6874 existing diagnostics. `bridge-changed-lints-final.json`: 0 errors на всех изменённых production строках, включая additions в существующие modules |
| Stable SDK Clippy slice | PASS (slice) | `bridge-clippy-sdk-final.jsonl` и `bridge-sdk-changed-lints-final.json`: финальный Mem+RocksDB run, проверка всех изменённых production строк; общий lint debt учитывается отдельно |
| Combined Mem+Rocks features | PASS | `bridge-combined-consumer.log`: 2 runtime/link/reopen tests |
| Rustdoc двух extension traits | PASS (slice) | `bridge-doctest-slice.log`: 2 PASS отдельно. Общий `bridge-doctests.log`: 31 PASS/1 ignored + 3 compile-fail PASS; standalone existing LIVE example не нашёл transitive `librocksdb_sys` rlib (environmental link failure) |
| Root defaults | PASS | `root-public-stage30-default.log`: 1360 library PASS/10 ignored, public RAM 4 PASS, local 15 PASS |
| Root/public native-async-io | PASS | `root-public-stage30-native-io.log`: 1369 library PASS/11 ignored, RAM 4 PASS, local 16 PASS, включая native model reopen |
| Public no-default RAM/local | PASS | `public-stage30-no-default.log`: 4 RAM + 15 local PASS |
| SDK private conversions | PASS | `bridge-conversion-unit-tests.log`: scalar/array/object/set/range/geometry/UUID/composite RecordId, nanosecond datetime, Decimal MAX; unsupported duration/ULID/geometry и NaN set collapse rejection |
| Registry 10K/100K/1M | PASS (scope) | `target/stage30/registry-{10000,100000,1000000}.csv`, `registry-processes.json`: indexed hit/miss, Pending reservation+abort, own refs/state assertions; setup исключён из latency |
| 300-МиБ registry generation/reopen | PASS | `bridge-large-generation-gate.log`: 100 Pending records с payload3MiB, heap>256MiB и public reopen |
| Ordinary query regression ≤5% | PASS (scope/model) | `target/stage30/paired-hierarchical-summary.json`: RETURN1 ratio1.01715/upper1.03895; class_1024 ratio1.01369/upper1.03833. Workload, artifact и статистические пределы указаны ниже |
| Nightly scoped rustfmt check | PASS | `scoped-fmt-delivery.log`: все затронутые production files, registry benchmark и новый fault test. Общий `full-fmt-check.log` фиксирует unrelated tests/B+tree форматный долг |
| Changed-file diff check | PASS | `changed-diff-check-default.log`: repository EOL configuration; проба с forced autocrlf=false дала false CRLF whitespace diagnostics и не используется как gate |
| Graphify update | PASS | `graphify-update-delivery.log`: AST-only incremental update, 60288 nodes / 163055 edges; две config JSON дали zero AST nodes, semantic labels не обновлялись |

Логи находятся в `target/gql-stage30-spike/`. Native SDK lib-test run ранее
упёрся в LLVM OOM; native runtime tests вынесены в public consumer target.
Результат SDK check не объявляется runtime/link PASS.

## Performance evidence — текущий проход

Baseline — чистый HEAD archive `2a7f465`, без изменения dirty checkout. Две
benchmark программы совпадают по исходному тексту после нормализации CRLF/LF;
добавленные unrelated collections/asynchronous modules отключены от production,
а прежний private `siv` не имеет consumers на выбранных paths. Это ограничивает
сравнение двумя workload paths, не всем языком и не performance gate этапа 29.

Оба варианта собраны stable 1.98.1 под `profiling`: opt3, codegen16, LTO=false,
debug2, static CRT. Baseline и current используют отдельные Cargo target directories;
первая попытка общего target была INVALID из-за reused stale status artifact.
Exact EXE/PDB copies и SHA256 сохранены в `target/stage30/artifacts/` и
`provenance.json`. Source snapshot проверен `runtime-source-reconciliation.json`:
52 из 63 files неизменны; все остальные hashes восстанавливаются удалением только
добавленных docs, re-export formatting/effective visibility внутри private storage
parent и cfg(test)-only fault hooks. Production runtime behavior после snapshot не
менялось.

Первый неприкреплённый к CPU проход показал высокий scheduler variance. Повторный
проход использует verified P-core logical6/mask64 и process-only HighQoS для native
benchmark children; host power scheme не менялся. Общий Python QoS self-check
не подтвердил persistence после interpreter initialization; у действующего native
benchmark child during-run наблюдались control_mask=1/state_mask=0 и affinity64.

`paired-summary.json` и `paired-pinned-summary.json` сохраняют первоначальный
ad hoc diagnostic: отношение средних и усреднённых endpoints индивидуальных mean
CIs. Последнее не является calibrated aggregate 95% ratio CI. Правильная отдельная
оценка `paired-hierarchical-summary.json` использует canonical Criterion0.8.2
Typical/Slope для Linear sampling и ресэмплирует paired blocks вместе с исходными
(iterations,time) pairs. Point estimates воспроизводят Criterion до 1e-12 relative;
read-only reviewer воспроизвёл JSON побитно. Seed104729, 20000 bootstrap draws,
upper two-sided 95% endpoint должен быть <=1.05; это model-conditional local gate.

RETURN1: четыре paired blocks, ratio1.01715, CI[0.99651,1.03895] — PASS.
Class_1024: исходные четыре blocks были недостаточны (upper1.07242); заранее
зафиксированный batch из12 дополнительных pairs выполнен целиком, без остановки
по промежуточным результатам. Итоговые16 blocks: ratio1.01369,
CI[0.99094,1.03833] — PASS. Все runs/outliers сохранены. Четыре RETURN blocks
ограничивают доказательство run variability; вывод локален выбранной машине/model.

Registry probe выполнен для10K/100K/1M. Fixture включает столько же local atoms,
поэтому process RSS не является памятью только mappings. Mean latency (µs):

| Mappings | Direct hit | Reverse hit | Direct miss | Reverse miss | Pending reserve+abort | Max reserve admission bytes | Peak working set |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 10K | 0.499 | 0.512 | 0.422 | 0.460 | 42.44 | 98378 | 26.3 MiB |
| 100K | 0.534 | 0.547 | 0.446 | 0.521 | 103.26 | 634673 | 175.3 MiB |
| 1M | 0.598 | 0.842 | 13.027 | 0.673 | 1351.72 | 5816200 | 1.546 GiB |

1M direct miss содержит scheduler outlier: CI95 half-width24.532µs, tail rank39/40
1.100µs; mean не скрыт и не объявляется steady-state bound. Hit/other-miss paths
используют indexed BTreeMap get, а не полный scan. Reservation копирует затронутые
COW shards и дорожает с registry. Setup:0.463/20.433/2411.091s; он исключён из latency,
но это существенная текущая стоимость массовой регистрации. Peak commit1M —1.685GiB;
transaction write budget fixture —2GiB. Measurement утверждает этот scope, не
абсолютный latency/RSS SLA или bounded growth массовой записи.

SW VTune2026.4 capture15s во время setup исключён из timed measurements;
process report подтвердил exact EXE и PID22200. Allocation stacks проходят через
BTreeMap clone → RegistryBucket COW → register_bridge_mapping. Profile объясняет
рост стоимости заполнения; он не доказывает throughput gain. Peak process memory
этого 1M прохода включает краткую profiler injection; admission bytes независимы
от sampling. Windows/system PDB warnings и folded generic symbol names ограничивают
source attribution. Raw capture и CSV exports сохранены в `target/stage30/`.
Captured registry probe source сохранён рядом с EXE/PDB. Его дополнительный tail
column был ошибочно подписан p95: rank39 из40 соответствует p97.5. В текущем
benchmark source исправлены только название column и doc comment; измеряемые
операции, mean и CI не менялись. Исходные CSV сохранены без переписывания.

## Finding ledger

| ID | Решение/изменение | Доказательство закрытия |
|---|---|---|
| REG-01 | Import после mapping tombstone удерживал старый physical binding; publication теперь передаёт новый binding | Public import→catalog-only→reopen regression PASS |
| REG-02 | Native record namespace admission симметричен decoder: bounded и non-NUL | Focused rejection test PASS |
| REG-03 | Write bytes больше не схлопываются по одному ConflictKey domain | Cumulative admission; role/database/SSI tests добавлены |
| REG-04 | Mapping pages prewritten и публикуются reachability; borrowed payload preflight до owning clone | Fresh source review PASS; 300-МиБ gate PASS |
| REG-05 | Recovery count/visitor bounded; writer резервирует aggregate recoverability и codec scratch | Fresh source review PASS; 72-МиБ/3-МиБ regression PASS |
| REG-06 | DELETE/DETACH/REMOVE GRAPH/BUILD заранее учитывают implicit tombstones; retained BUILD edges сохраняют mapping | Read-only re-review PASS; removed occurrence tests и full gate |
| REG-07 | Retirement pins ограничены заменяемыми directories | Статическая повторная проверка PASS; старый общий marker helper удалён |
| REG-08 | Encoded direct/reverse keys проверяются общим production B+tree validator до registry install | 16330/16331-byte boundary и oversized composite identity tests PASS; fresh source re-review PASS |
| REG-09 | Database descriptor проверяется тем же bounded envelope codec до registry install | Oversized class/graph table descriptor test PASS; fresh source re-review PASS |
| B3-TEST-01 | Existing generation fault hooks подключены к catalog-only пути под cfg(test); первый тест ранее не инжектировал отказ | Reservation/Ready failure matrix PASS, весь final root gate PASS, no duplicate atom changefeed entries |
| BR-SDK-01 | Native source USE изолирован клоном SDK session | Mem consumer PASS |
| BR-SDK-02 | Missing foreign property соответствует NONE, NULL остаётся отдельным | Реальный mirror/resume Mem и Rocks PASS |
| BR-QUERY-01 | Mirrors исполняются на каждом local commit barrier до следующего statement | Multi-statement RELATE→FETCH fake test PASS |
| BR-QUERY-02 | Ready confirmation выполняется уже работающим GQL worker; nested blocking worker отсутствует | One blocking-thread fake test PASS; fresh review PASS |
| BR-CONV-01 | Set cardinality changes при canonical equality отклоняются | Feature-specific conversion gate PASS |
| BR-CONV-02 | Native geometry containers проверяются до owning copies | Статическая повторная проверка PASS; root geometry gate и native typed tests |
| BR-READ-01 | Read-only ceiling применяется и к top-level statement effects | Native InternalQuery CREATE/SLEEP rejection PASS |

## Неизменяемая граница частичного успеха

Локальный commit и внешняя запись независимы. Возвращаемый QueryResult сохраняет
состоявшуюся локальную работу, external effects содержат owned mapping reference
и отдельный исход. Ошибка/timeout оставляет Pending; explicit resume проверяет
сохранённую identity, не повторяет local RELATE/Events/changefeed и не создаёт
endpoints. Tombstones не удаляют внешние данные. Автоматических фоновых retries,
distributed transactions и bridge dependencies в durable LIVE/EVENT recipes нет.
