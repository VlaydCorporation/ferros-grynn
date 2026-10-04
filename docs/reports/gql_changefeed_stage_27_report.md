# Этап 27 — отчёт evidence Durable Changefeed

## Статус

Этап в реализации. C0–C9 реализованы и прошли focused correctness gates;
C10 прошёл итоговый full/backend correctness gate; performance/tooling matrix
сохраняет открытые проверки, перечисленные ниже.
Stage closure остаётся OPEN до полного прохождения acceptance gates.

## C0 baseline — 2026-10-03

- Исходный commit: `3d1679f1b88a93ee979d9203d19f4d6000758eab`.
- Pre-existing dirty changes сохранены: README, disconnected collections/async,
  staged prototype deletions, graphify output и backup files.
- `cargo test --lib --no-default-features datastore:: -- --test-threads=1`:
  **PASS: 90 tests, 0 failures, 1.88 s**. Лог: `target/stage27-baseline-datastore.log`.
- Exact pre-production-edit source snapshot для paired measurement сохранён в
  `target/stage27-baseline-source`, включая current dirty production source,
  Cargo/config и actual workspace members; reference trees не использованы.
- DML/query/import paired performance baseline и полная 10K/100K/1M matrix:
  **OPEN**, correctness duration не подменяет latency evidence.

## Подтверждённые integration points

Existing commit_ts — durable глобальный counter; process-local transaction id
не является persisted batch identity. Один TxHandle/TxCommit barrier обслуживает
primary/catalog. Feed-only publication должна учитывать FULL targeted no-op,
writer lease и Serializable validation, а не только mutations.is_empty().

Historical named properties и qualified endpoint routes должны быть owned.
PropertyMap без dictionary и EdgeTopology с local AtomId для этого не подходят.
Import schema DDL запрещён; accepted-item history должна переживать seal_import_segment.

## Открытые gates

C10 paired performance matrix, global Clippy и just all-check.
Итоговые full/backend checks после segment replay и purge accounting PASS.
Исторический lint/tooling debt отделяется от новых
signatures; focused PASS не объявляется полным stage closure.

## C1 — definitions, identities и durable metadata

- Public `changefeed` содержит owned SDK types, normalized fixed retention, exact
  Versionstamp и versioned 48-byte cursor. Public UUID — существующий `values::Uuid`.
- Root/catalog — единственная authority source configuration; `ClassSchema` и
  `GraphSchema` не содержат вторую копию policy. Historical capture epochs сохраняют
  final policy commit; DROP/REMOVE не стирают descriptor.
- FGDC v10 development header расширен на месте до 128 bytes: database UUID, UTC
  time и retained feed count. Checked descriptor/epoch codecs проверяют reserved
  bytes, bounds и allocation clocks retired sources. Reopen сохраняет origin.
- `configure_changefeed`/`drop_changefeed`/`changefeed_info` и
  `CommitResult::versionstamp` доступны без GQL. Private removals и BUILD GRAPH
  обновляют lifecycle атомарно; exhausted revision failure откатывает private state.
- `cargo test --lib --no-default-features changefeed -- --test-threads=1`:
  **PASS: 7 tests** (`target/stage27-c1-tests.log`).
- `cargo test --doc --no-default-features changefeed -- --test-threads=1`:
  **PASS: 3 executed examples** (`target/stage27-c1-rustdoc.log`).
- Public RAM target: **PASS: 1 test**. Local Sync checkpoint/reopen:
  **PASS: 1 test** (`target/stage27-c1-local-sync.log`). NativeAsync target:
  **PASS: 1 test** (`target/stage27-c1-local-native.log`).
- `cargo check --lib --no-default-features`: **PASS**, existing warnings.
- Initial Clippy: **FAIL: 7037 diagnostics**, включая existing repository debt и
  исправленные затем новые private-doc/wildcard/parameter findings. Повторная
  slice classification остаётся OPEN; весь набор не объявлен pre-existing.

Эти результаты подтверждают metadata/API, а не доставку событий. Images, history
files, reader, retention/import и GQL consumers относятся к C2–C10.

## C2 — logical operation journal

- Компактные intents записываются перед equality exits и первичной allocation;
  SDK/GQL/cascade/writeback/import boundaries не копируют primary images.
- Savepoints возвращают operation ordinals и journal reservations. Metadata входит
  в transaction resource admission; buffers имеют отдельный checked scratch cap.
- `ReplayRemap` общий для всех operation ranges одного commit, включая rebased CREATE.
- Focused journal/capture checks: **PASS: 10 tests**. Расширенный datastore gate:
  **PASS: 98 tests, 2.55 s** (`target/stage27-c2-datastore.log`).
- Nightly rustfmt для изменённых файлов выполнен. Clippy: **FAIL: 6960 errors**;
  новые catalog wildcard findings исправлены, общая tooling debt остаётся OPEN.

## C3 — exact images, NET/FULL и bounded import spool

- Runtime images владеют named values, overlays и qualified endpoint routes.
  NET сравнивает numeric storage variants/decimal scales; masked inherited fanout
  не создаёт события. Captured groups пока находятся только в RAM root: durable
  event delivery после reopen этими тестами не подтверждена.
- Финальная validated policy применяется к replay, включая late enable, DROP и
  owner retirement. FULL-only no-op проходит writer/SSI/target validation и
  получает whole-commit stamp. Mixed GQL и writeback проверяют каждый no-op target.
- `ImportOptions.changefeed` — typed ON/OFF, default OFF. ON сохраняет accepted
  item groups через seals; END coalesces NET и публикует FULL с одним time/stamp.
  OFF использует compact targets и borrowed representation comparison, без owned
  images. Inherited FULL fanout сравнивается на outer item boundary; failed
  setter/Skip/rollback не оставляет suppression history.
- Review findings R1–R8 исправлены и покрыты regressions; read-only scoped review:
  **PASS**. Temporary record/membership без before/after на accepted item boundary
  не создаёт ложный OFF GAP.
- Focused tests после R8: **PASS: 26 tests** (`target/stage27-c3-review5-tests.log`).
  Расширенный datastore gate: **PASS: 112 tests, 1.91 s**
  (`target/stage27-c3-datastore.log`), до добавления последнего R8 regression.
- `cargo check --lib --no-default-features`: **PASS**, existing warnings
  (`target/stage27-c3-final-check.log`). Nightly rustfmt выполнен для changed slice.
- Clippy: **FAIL: 6959 errors** (`target/stage27-c3-clippy.jsonl`). Один новый
  `unused_self` в capture helper исправлен. Private-doc findings старого writeback
  существуют в baseline source; полный набор diagnostics не объявлен pre-existing.

Local capture/recovery, independent generated-graph image oracle и paired
performance matrix остаются открытыми до соответствующих C4–C10 checks.

## C4 — explicit commit groups и continuation codecs

- `DataFileKind::Changefeed = 0x17`, canonical `.fcf` paths и роль `WalBackfilled`
  зарегистрированы; reserved-kind WAL test использует следующий незанятый tag.
- FCFS/FCFE и FGCB кодируют whole-commit bounds, historical dictionaries,
  NET/FULL, exact images и обратимый before-diff. Primary records для decode
  не нужны. Многостраничные groups сохраняют один stamp и checked ordinals.
- Admission выполняется до buffer growth, dictionary intern, runtime tree
  conversion и geometry validation. Проверены complete initial B-tree nodes,
  named time zones, canonical nesting/text limits и reserved bytes.
- Focused gate: **PASS: 43 tests, 0.14 s**
  (`target/stage27-c4-tests6.log`). `cargo check --lib --no-default-features`:
  **PASS** (`target/stage27-c4-check2.log`). Nightly rustfmt выполнен.
- Read-only scoped codec/resource review после исправлений: **PASS**.
  Clippy остаётся **FAIL** по общей repository debt; новый visibility finding
  устранён точечным allow для crate-private re-exported admission helper.
  Existing `multiple_inherent_impl`/`needless_pass_by_ref_mut` в DTO не изменены.

Эти проверки подтверждают физические codecs; evidence публикации приведён ниже.

## C5 — единая WAL publication и восстановление

- Primary, feed pages, dictionaries и оба каталога публикуются одним `TxHandle`.
  Shared WAL cap проверяется до создания новых `.fcf`; normal, catalog-only,
  FULL-only no-op и final import используют общий barrier. Feed registrations
  сохраняются при последующих generation builds.
- Logical root удерживает только immutable segment descriptors; local images
  восстанавливаются из pinned snapshot pages. Старые readers видят прежние page
  ceilings после append и checkpoint. Reopen не загружает всю историю в RAM.
- Missing/malformed `.fcf` не стирает доверенные catalog bounds. Recovery отдельно
  проверяет все source clocks, включая sources без сегментов. Catalog corruption
  не подменяется optional-segment degradation.
- Известные ошибки до WAL удаляют новые unpublished feed files. Неопределённый
  append/flush/sync сохраняет feed, primary и quarantine до recovery reachability;
  poisoned writer отвергает повторные writes до side-file creation. Orphan cleanup
  работает только после recovery и по canonical file identities.
- Focused correctness gate: **PASS: 51 tests, 7.17 s**
  (`target/stage27-c5-tests4.log`), включая checkpoint/reopen, private old snapshots,
  import ON/OFF, четыре known publication failures и четыре WAL failure boundaries.
- Read-only scoped publication/resource review: **PASS** после исправлений cleanup,
  future clocks и poisoned retries. Nightly rustfmt выполнен для changed slice.
- `cargo check --lib --no-default-features`: **PASS**, 10.91 s
  (`target/stage27-c5-check3.log`). Clippy: **FAIL: 6961 errors, 591 warnings**
  (`target/stage27-c5-clippy2.jsonl`); общая tooling debt остаётся открытой.

## C6 — snapshot reader, resume и GAP

- Rust SDK предоставляет `ChangefeedReadOptions`, `ChangefeedSince` и fallible
  `ChangefeedReader`. LIMIT считает целые commits/GAP; images/cursors владеют
  данными. FULL coverage проверяется по историческим flags всего выбранного
  интервала до выдачи первого элемента, независимо от LIMIT.
- Local reader использует отдельный constant-size history pin: TxHandle,
  visibility и shared file handles. Begin не сканирует graph/index bindings.
  Metadata preflight не декодирует dictionaries/images; private writes не
  создают неопубликованную историю. Trusted segment loss даёт GAP с tail.
- Timestamp cursor остаётся None, пока inclusive boundary не доказан; future
  EOF и resource failure до boundary не превращают timestamp в unsafe resume.
  Проверены stamps выше 2^53, clock rollback, corruption последнего payload
  первого сегмента с доступным следующим сегментом, checkpoint/reopen и pins.
- Focused gate после lint fixes: **PASS: 57 tests, 7.00 s**
  (`target/stage27-c6-tests5.log`). Public API: RAM **3 PASS**
  (`target/stage27-c6-api4.log`), Sync **2 PASS**
  (`target/stage27-c6-sync2.log`), NativeAsync **2 PASS**
  (`target/stage27-c6-async2.log`). Rustdoc **4 PASS**
  (`target/stage27-c6-rustdoc1.log`). `cargo check --lib --no-default-features`:
  **PASS: 14.78 s** (`target/stage27-c6-check3.log`).
- Scoped source review: **PASS** после R1/R2 regressions и dedicated-pin review.
  Nightly rustfmt выполнен. Clippy: **FAIL: 6964 errors, 539 warnings**
  (`target/stage27-c6-clippy2.jsonl`); выделенные changefeed modules не имеют
  diagnostics. Общий tooling gate остаётся OPEN; все diagnostics репозитория
  не объявляются pre-existing без сравнения с baseline.

## C7 — rotation, retention и pins

- Purge публикует logical prefix floor и позиции первого доступного group;
  partial segment сохраняет исходные physical bounds. Reader/reopen используют
  эти позиции. Active source сохраняет floor при пустом списке segments;
  closed source удаляется только после TTL от durable UTC close time.
- Единственный maintenance worker имеет timed idle wake. Проход ограничен 1024
  removals и 1M metadata operations, scratch проверяется до COW allocations.
  Purge и database compaction coalesce отдельно. Publication повторно проверяет
  cancellation, root revision и import writer lease под commit lock.
- `.fcf` unreference публикуется одной WAL transaction с floor; unlink ждёт pins
  всех заменённых generations. Cleanup bounded и повторяется после освобождения
  readers; uncertain WAL outcome сохраняет файлы до recovery reachability.
- Focused gate: **63 PASS, 7.44 s** (`target/stage27-c7-tests5.log`), включая
  eight WAL boundary cases для partial/whole purge, terminal GAP после reopen,
  multi-generation pins, idle TTL, cancellation и revision race.
- Fresh read-only C7 review: **PASS** после R1 (last-source registrations) и R2
  (graphs/name scratch admission). Nightly rustfmt выполнен для changed slice.
  `cargo check --lib --no-default-features`: **PASS, 5.49 s**
  (`target/stage27-c7-check3.log`). Clippy: **FAIL: 6964 errors, 547 warnings**
  (`target/stage27-c7-clippy2.jsonl`); общая tooling debt остаётся OPEN.

## C8 — language/SDK vertical slice

- DEFINE/ALTER CLASS и GRAPH принимают validated fixed retention, INCLUDE ORIGINAL
  и FULL; DROP останавливает захват. SHOW/INFO проходят parser, typed analysis, IR
  и обычный transaction adapter, используя самостоятельный Rust SDK reader.
- SHOW возвращает owned whole-commit/GAP objects с exact u64 Number, historical
  before/after, qualified graph identities/overlay decisions и opaque Bytes cursor.
  Исторический lookup, FULL coverage и import ON/OFF используют тот же контракт SDK.
- Review исправления: labels и INFO capture epochs admitted до копирования;
  target/SINCE/LIMIT effects участвуют в transitive UDF analysis; scalar evaluator
  использует общий bounded admission protocol до cloning cursor/selector payloads.
- Focused gate: **72 PASS, 7.21 s** (`target/stage27-c8-tests4.log`), включая восемь
  end-to-end GQL regressions и parser boundaries. После этого выполнена малая
  extraction DEFINE dispatch и исправлены новые private-doc/module-layout findings.
  `cargo check --lib --no-default-features`: **PASS, 9.46 s**, 210 warnings
  (`target/stage27-c8-check3.log`). Полный lib gate: **773 PASS, 10 ignored,
  74.47 s** (`target/stage27-c8-full-lib3.log`); rustdoc: **10 PASS**,
  включая прежний compile-fail CSR constructor (`target/stage27-c8-doc1.log`).
- Initial C8 Clippy: **FAIL: 6981 errors, 539 warnings**
  (`target/stage27-c8-clippy1.jsonl`). Новые findings выделены сравнением primary
  source snippets с C7 и исправлены. Latest Clippy: **6965 errors, 539 warnings**
  (`target/stage27-c8-clippy3.jsonl`); один differing diagnostic — прежняя parser
  complexity, сниженная с 80/30 до 69/30. Новых scoped findings не осталось.
- Fresh review: **PASS** после Range-bound depth/work admission regression.
  Три full-gate fixtures синхронизированы с FileKind 0x17, ALTER GRAPH grammar
  и retirement unpinned atom generations; graph binding identity проверяется точно.

## C9 — logical read plans и observations

- Rust reader предоставляет owned `ChangefeedReadBinding` и отдельные counters
  inspected groups, whole commits/entries/GAP, page requests/bytes, work и admitted
  scratch. Local reader считает реальные запросы к существующему read engine,
  включая FULL preflight; повторное чтение имеет независимый observation scope.
- Datastore process counters учитывают capture attempts и группы, опубликованные
  после существующего WAL barrier. Catalog gauges показывают retained sources,
  physical segments/bytes и bounded TTL backlog; open сбрасывает process counters.
- EXPLAIN использует `ChangefeedHistoryRead`/`ChangefeedInfo`, committed source
  binding, historical read mode, bounds/resume и whole-commit limit. JSON/TEXT
  ANALYZE содержат девять payload-free counters. Каждый ANALYZE имеет свой peak;
  read operator не отмечается как publication и не исполняет UDF для planning.
- PlanHash version 7 включает bounded analyzer-owned operand spelling, runtime
  slots, committed origin/bounds и private function catalog descriptor. Spelling
  чувствителен к formatting. Полные function versions/bodies различают private
  ALTER и rollback/redefine, при этом тексты не выводятся в telemetry.
- Review fixes: preserved left-to-right operand errors, independent retired cursor
  validation, correct source provenance в saved UDF/session closure, checked
  Path/Geometry/Error depth/work перед Hash и admission canonical buffers.
  Existing UDF compiler генерировал zero-parameter `||`, которое lexer читает как
  OR; synthetic closure delimiters теперь разделены пробелами.
- Focused gate: **80 PASS, 7.02 s** (`target/stage27-c9-tests3.log`);
  fresh bounded source review: **PASS**. Nightly rustfmt выполнен для changed slice.
  Check: **PASS, 26.40 s, 210 warnings** (`target/stage27-c9-check4.log`).
  Clippy: **FAIL: 6964 errors, 539 warnings** (`target/stage27-c9-clippy2.jsonl`).
  Новые scoped findings исправлены; единственный differing signature к C8 —
  существующий `redundant_pub_crate` у изменённой константы encoding version 6→7.

- Rustdoc: **5 PASS** (`target/stage27-c9-doc1.log`), включая configure, cursor,
  whole-batch reader с binding/statistics и purge. Focused doc filter не выбирает
  compile-fail example; полный rustdoc gate остаётся частью C10.

## C10 — closure evidence, 2026-10-04

- До позднего исправления общего meta-owner validator полный lib gate:
  **781 PASS, 10 ignored, 65.23 s** (`target/stage27-c10-full-lib1.log`),
  API **3 PASS**, local Sync **2 PASS**, NativeAsync **2 PASS**,
  full rustdoc **10 PASS**, включая compile-fail CSR constructor.
  NativeAsync использовал `--features native-async-io`; no-default target без
  этой feature выбрал ноль tests и помечен SKIP, а не PASS.
- Независимый public SDK state machine покрывает 256 length-three sequences
  create/set0/set1/delete, present/absent record и оба INCLUDE ORIGINAL flags.
  NET/FULL сравниваются с моделью optional integer state, без production roots/
  coalescing helpers. Present images проверяются также по RecordId/kind,
  FULL operations — по execution order. Strengthened execution: **1 PASS,
  256 scenarios, 0.18 s** (`target/stage27-c10-integration3.log`).
- Lifecycle fixture: **8 PASS, 0.20 s** (`target/stage27-c10-integration1.log`):
  actual hyperedge endpoint order, shared nested graphs, edge-owned routes,
  successful writeback/BUILD GRAPH, graph-local cascade, handled TRY/abort/
  zero matches, current-root membership fanout, ReadCommitted reader bounds и
  Serializable FULL-only no-op cycle. Base-property no-op не делает неявный
  graph fanout; графовый no-op должен иметь explicitly targeted occurrence.
- Первый fixture обнаружил прежний NODE-only guard общего metagraph validator.
  Dispatch теперь проверяет child/transition graph соответствующего owner kind.
  Actual qualified images дополнительно проверяются в отдельных Sync/NativeAsync
  reopen targets, включая expected parent/child GraphId. Sync **3 PASS, 1.11 s**
  (`target/stage27-c10-integration3.log`), NativeAsync **3 PASS, 0.79 s**
  (`target/stage27-c10-native-async2.log`). Эти gates предшествуют segment replay.
- Focused gate после дополнений: **82 PASS, 8.36 s**
  (`target/stage27-c10-focused3.log`). Новый normal FULL-only fault matrix покрывает
  BeforeAppend/AfterAppend/AfterFlush/AfterSync, poisoned retries, old-or-whole
  commit, exact present images/value/stamp и повторный checkpoint/reopen.
  Это uncertain transport outcome с Drop/reopen, не power-loss process crash.
- Shared WAL admission проверена с primary/catalog/feed keys и simulated already
  admitted 256 MiB boundary: replacement не увеличивает reservation, новый frame
  отклоняется без staged-map/publication changes. Test не выделяет 256 MiB и не
  является измерением RSS или фактически записанного большого WAL.
- Maintained Criterion targets `gql_stage27_common` и `gql_stage27` не добавляют
  dependencies. Common исходник одинаков для C0/current: background 10K/100K/1M,
  RETURN, 16 point reads и 64 alternating real mutations. Профиль `profiling`,
  один toolchain/config/lockfile; probes запускаются последовательно без Cargo
  builds во время измерения. Raw logs/estimates сохраняются в target.
- Предварительное 10K/100K сравнение имеет широкие confidence intervals:
  10K point reads delta +13.825% (CI −5.1517..+35.388%, p=0.18),
  100K mutation +4.4001% (CI −1.5361..+10.658%, p=0.18).
  `No change detected` не доказывает gate ≤5%; performance остаётся OPEN.
  Первые 10K current samples до baseline build не использованы как paired gate.
- 1M до COW optimization: C0 mutation64 **12.728 s**, current **14.616 s**,
  delta **+14.829%**, CI **+9.9003..+19.676%**, p=0.00. Это выявленная регрессия,
  а не пройденный ≤5% gate. Process PeakWorkingSet **1245569024 bytes**, включая
  fixture/import и весь запуск (`target/stage27-c10-common-current-1m1.process.json`).
- VTune 2026.4.0 SW Hotspots собран последовательно для exact current/C0 EXE/PDB;
  full Rust symbols/source paths подтверждены. Оба profiles показывают дорогой
  unconditional AtomStoreRoot clone на каждом setter. Direct diff по разным source
  roots не считается сравнением одинаковых symbols. Внесена малая перестановка
  fallible derived-state preparation перед primary COW reuse; shared snapshots и
  savepoints продолжают отделяться. После неё focused **82 PASS, 7.86 s**
  (`target/stage27-c10-focused4.log`). После diagnostic fixes full lib:
  **783 PASS, 10 ignored, 69.29 s** (`target/stage27-c10-full-lib2.log`).
  Перед integration retry возник disk-full error112; удалены только три compiler
  incremental cache dirs этого C10 run. Исходники, baseline EXE/PDB и raw evidence
  сохранены. Retry API **3 PASS**, lifecycle **8 PASS**, Sync **3 PASS**, oracle
  **1 PASS** (`target/stage27-c10-integration3.log`). Segment replay требует нового
  итогового full gate.
- Benchmark review исправил rejection masking: только 100K/1M typed resource statuses
  могут стать ожидаемым rejection, после проверки всех records, полного unchanged
  feed info/stamp и отсутствия history/GAP. ON/OFF читаются как exact whole commit
  или ImportSuppressed GAP с source/stamp, ordinals и images. Capture sample history
  растёт и выводит start/end groups/bytes; это не fixed-history latency. Retained
  WAL footprint и admitted scratch не подменяют cumulative WAL/RSS. TTL timing
  начинается до configuration publication, включая DDL/job polling.
- `just all-check` выполнен с byte guard для 551 Rust files; cargo check PASS,
  formatter восстановил один rewritten benchmark. Global fmt остановился на
  Windows mapped-section write error **1224** для transaction/tests.rs; Clippy
  в этом запуске не исполнялся. Log: `target/stage27-c10-all-check1.log`.
- COW paired observations: 10K mutation **8.2033 ms** против fresh C0 **96.182 ms**,
  100K **131.13 ms** против **1.4004 s**, 1M **2.0751 s** против **12.728 s**.
  1M delta **−83.697%** (CI −84.270..−83.078%, p=0.00); peak process working set
  **1123598336 bytes**, включая dataset setup и весь запуск. Logs:
  `target/stage27-c10-common-cow-{10k2,100k1,1m1}.log`.
  10K RETURN confidence interval ещё допускает >5%; 1M RETURN latest delta
  **+30.940%** (CI +4.0475..+66.666%, p=0.04). Query performance gate OPEN.
  Отдельные SW profiles RETURN показывают общие memcpy/parser/analyzer/transaction
  hotspots; они не установили отдельный changefeed-specific источник регрессии.
- Atomic byte-identical replacement сняла mapped-file obstruction без изменения
  source contents; SHA256 проверен (`target/stage27-c10-mapped-file-replace1.log`).
  Повторный all-check после этой операции pending.
- Strict ON/OFF import/read/purge probes **10K PASS** для RAM и local Sync:
  RAM OFF/ON import **19.8675 ms / 7.08935 s**, Sync **65.5447 ms / 5.99698 s**.
  Sync ON first/warm validated read **10.8437 / 7.451 ms**, **119 page requests**,
  feed footprint **1867776 bytes**, retained WAL **2104612 bytes**. Sync ON
  reader admitted scratch peak **122158611 bytes**, capture **33432120 bytes**,
  process peak **110186496 bytes**. Суммы admission не являются measured RSS.
  TTL-to-purge-complete ON **13.931 ms** включает DDL/worker polling. Raw logs:
  `target/stage27-c10-probe-{ram,sync}-10k1.log`. Это single observations,
  а не пройденный statistical paired performance gate или OS-cold I/O.
- До segment replay RAM100K FULL ON закончил atomic resource rejection за
  **1235.5053336 s**, capture cap **268435456 bytes**, attempted **268435738 bytes**.
  Проверены unchanged complete feed info/stamp, отсутствие всех primary records
  и history/GAP. Process peak **281448448 bytes**. Probe завершился сам, не был
  прерван (`target/stage27-c10-probe-ram-100k1.log`). Это PASS атомарного отказа,
  но неприемлемое время, не performance PASS. В raw error parameters были
  перепутаны: исправлены changefeed constructors, shared query usage diagnostics
  и exact maximum/actual assertions; focused **82 PASS, 8.36 s**
  (`target/stage27-c10-focused6.log`), bounded read-only rereview PASS.
- Bounded import refactoring удерживает один admitted segment before-root;
  ordered replay выполняется до очистки accepted-item journal/mutations. Primary,
  journal capacity, root metadata и retained spool делят ON admission cap.
  Transient replay charges не накапливаются в spool; empty capture освобождает
  их. Intermediate seal failure отравляет session даже при swallowed error,
  END не публикует private state. OFF и Skip/Quarantine savepoints сохраняются.
  Focused **84 PASS, 8.25 s** (`target/stage27-c10-focused8.log`), bounded rereview
  PASS. Расширенный import gate **9 PASS, 0.04 s**
  (`target/stage27-c10-import-tests9.log`): segment1/3/64, rejected item, no-op,
  delete/recreate, intermediate seal/END admission, 200 non-feed no-ops под4KiB.
  Новые comparative timings и итоговый full/backend gate pending.
- `graphify update .`: **PASS** (`target/stage27-c10-graphify3.log`), 56213 nodes,
  150089 edges. Installed launcher не видит sandboxed uv Python; первый module
  run прошёл AST, но inplace `.sig` write получил Errno22 и оставил partial update.
  Curated dated backup и signatures сохранены в target; повторный run использовал
  существующий Graphify `write_json_atomic` только для `.sig`, без package edits,
  installs или semantic API calls. Known empty config.json/context7.json warnings
  не являются evidence извлечения этих файлов.

### Итоговый correctness gate после segment replay — 2026-10-04

- `cargo test --lib --no-default-features -- --test-threads=1`: **785 PASS,
  10 ignored, 69.14 s**, 795 discovered; без фильтра
  (`target/stage27-c10-full-lib3.log`).
- Separate integration targets: Rust API **3 PASS**, lifecycle **8 PASS**,
  local Sync **3 PASS**, independent oracle **1 PASS / 256 scenarios**,
  RAM datastore **4 PASS**, local datastore **15 PASS**
  (`target/stage27-c10-integration4.log`). NativeAsync с явным
  `--features native-async-io`: **3 PASS, 0.97 s**
  (`target/stage27-c10-native-async3.log`).
- Full rustdoc: **11 PASS** — 9 runtime examples и два compile-fail примера
  (`target/stage27-c10-doc3.log`). Теперь отдельный E0597 example действительно
  запрещает reader пережить transaction; прежний compile-fail проверял CSR.
  Owned batch/cursor остаются доступны после освобождения reader/transaction.
- `cargo check --lib --no-default-features`: **PASS, 20.87 s**
  (`target/stage27-c10-check1.log`). No-default Clippy: **FAIL, 6964 errors**.
  Machine comparison с C9: одинаковые counts, **0 новых source diagnostic
  signatures**; новый pathless linker message содержит имя DLL
  (`target/stage27-c10-clippy1.jsonl`, `stage27-c10-new-clippy1.json`).
  Это не PASS global lint gate.
- `just all-check` retry дошёл до cargo check, nightly fmt и default Clippy:
  **FAIL, 7002 errors / 540 warnings** (`target/stage27-c10-all-check2.log`).
  Default features отличаются от no-default, разность counts не объявляется
  новыми errors. In-memory formatter guard остановился при восстановлении
  mapped transaction/tests.rs. Original восстановлен atomic replacement с
  проверкой SHA256; три несвязанных graph_index files восстановлены из C0 bytes,
  совпадающих с HEAD. Следующий guard сохраняет originals и hashes на диск,
  восстанавливает каждый changed file atomically и продолжает после ошибки.
- Segment replay RAM probes: 10K OFF/ON **19.0031 / 55.7837 ms**;
  100K **202.4735 ms / 1.0213713 s**, ON теперь успешно публикует complete FULL
  history. 100K first/warm validated read **11.5261 / 10.3657 ms**,
  capture admitted peak **177732664 bytes**, process observed high-water
  **269586432 bytes**. Logs `target/stage27-c10-probe-segment-ram-{10000,100000}.log`.
  200 ms polling в быстром 10K запуске пропустил peak; его process result
  не используется как надёжное измерение peak memory. Новые probes опрашивают
  high-water каждые 10 ms, но последний exit peak всё равно может быть пропущен.
- Первые 1M probes выявили завышенный purge admission: typed diagnostic
  actual **640048896**, maximum **268435456 bytes**. Version tree содержит
  fixed Copy keys/stamps, поэтому estimator теперь использует
  `size_of::<(ConflictKey, u64)>()` с прежним conservative tree overhead вместо
  произвольной ширины 128. Caps не повышены, admission остаётся до COW.
  Исправление прошло bounded review и итоговый lib/backend gate выше;
  старый failed probe сохраняется отдельно (`stage27-c10-probe-segment-ram-1m-diagnostic.log`).

### Последовательная performance matrix — 2026-10-04

Profiling build7 PASS; exact measured EXE/PDB и SHA256 manifest сохранены в
`target/stage27-perf-before-root-group` перед следующей пересборкой.
Таблица измеряет source после segment/purge fixes, до root-handle grouping.
Во время измерений Cargo не запускался.

| Backend | Rows | NET capture64 | FULL capture64 | NET read64 | FULL read64 |
|---------|------|---------------|----------------|------------|-------------|
| RAM     | 10K  | 11.410 ms     | 12.478 ms      | 2.1681 µs  | 2.2098 µs   |
| RAM     | 100K | 215.23 ms     | 212.99 ms      | 2.5475 µs  | 2.6574 µs   |
| RAM     | 1M   | 2.7493 s      | 2.7699 s       | 2.2839 µs  | 1.9926 µs   |
| Sync    | 10K  | 52.888 ms     | 56.965 ms      | 91.692 µs  | 124.77 µs   |
| Sync    | 100K | 501.06 ms     | 553.87 ms      | 92.825 µs  | 145.94 µs   |
| Sync    | 1M   | 5.6707 s      | 6.0916 s       | 93.670 µs  | 134.41 µs   |

Это Criterion point estimates, 10 samples; intervals и raw observations находятся
в `target/stage27-c10-matrix-{ram,sync}-{10000,100000,1000000}.log` и Criterion
estimates. В 1M Criterion продлил измерение до десяти полных samples. History
capture растёт во время каждого запуска; read проверяет cardinality 64 entries
в неизменной history. RAM read использует owned shared runtime batches;
Sync read проходит physical codec. Ни один замер не обозначен OS-cold.
Это CLASS workload; graph fanout performance отдельно остаётся OPEN.

Observed process high-water включает setup и все workloads: RAM10K/100K/1M
**124059648 / 289243136 / 1917796352 bytes**, Sync
**233816064 / 501309440 / 2177490944 bytes**. Это largest live OS high-water query
при 10 ms polling, не allocation-tracked scratch; exit peak может быть пропущен.
Capture admitted peak **154382 NET / 160270 FULL bytes** не включает весь process
и не подменяет эти measurements. Feed/page/WAL observations сохраняются raw;
WAL footprint означает retained files, а не cumulative bytes written.

Final strict import/read/purge probes:

| Backend/rows | OFF import  | FULL ON import | Результат ON                    |
|--------------|-------------|----------------|---------------------------------|
| RAM 1M       | 2.2222 s    | 2.0749 s       | atomic capture-budget rejection |
| Sync 10K     | 68.2235 ms  | 118.8369 ms    | complete batch PASS             |
| Sync 100K    | 544.5912 ms | 2.0433 s       | atomic codec-budget rejection   |
| Sync 1M      | 5.8242 s    | 1.8812 s       | atomic capture-budget rejection |

Every expected rejection проверяет отсутствие всех primary records, unchanged
полные info/stamp и history EOF без GAP. Capture actual **268435522 > 268435456**;
Sync100K codec actual **879778292 > 268435456**. Caps не повышены; resource
rejection не является успешным throughput sample. Logs:
`target/stage27-c10-probe-final-{ram-1m1,sync-10000,sync-100000,sync-1000000}.log`.
Sync10K ON first/warm validated read **16.7844 / 7.1077 ms**, 119 page requests,
feed **1867776 bytes**, retained WAL **2104612 bytes**; observed process
high-water **118525952 bytes**. TTL-to-purge-complete OFF1M
**45.5231 ms RAM / 48.9224 ms Sync**, включая DDL и worker/job polling.

### Controlled query pair и root-handle experiment

- На одной проверенной CPU affinity mask1, одинаковом common source и 1M fixture,
  три последовательные C0/current RETURN1 pairs дали delta **+4.9920 / +4.8970 /
  +6.7762%**; CI upper **+6.7368 / +7.6146 / +8.6372%**. Это сохраняет gate >5%
  OPEN, хотя устраняет прежнюю неопределённость размещения процесса. Mask1 не
  объявляется конкретным P/E core. Logs `stage27-c10-return-affinity-{c0,current}-{1,2,3}.log`.
- Bounded internal refactor группирует три root handles одним outer Arc; inner
  catalog/history/segments Arc остаются независимыми. Read accessors не выполняют
  COW; mutable accessors отделяют outer и затем только выбранную map. Storage
  bytes/DTOs, dedicated file/WAL pins, UUID/clocks и SDK неизменны. Новая небольшая
  allocation покрывается fixed reserves; отдельного retained-spool charge нет.
  Bounded review PASS. Это проверяемая гипотеза, причина RETURN regression пока
  не объявлена единственной установленной причиной. Refactor сохранён после
  следующего paired run и подтверждённого улучшения 1M.
- После grouping focused **85 PASS, 7.09 s**; full lib **786 PASS, 10 ignored,
  69.49 s**. API3/lifecycle8/Sync3/oracle1/RAM4/local15 PASS; NativeAsync3 PASS;
  rustdoc11 PASS; no-default check PASS19.16s. Logs
  `target/stage27-c10-root-group-{focused2,full-lib1,integration1,native1,doc1,check3}.log`.
- No-default Clippy после grouping: **FAIL, 6964 errors**, **0 новых source
  signatures** относительно C9, root warning count снизился на один; единственная
  новая pathless signature — linker DLL filename
  (`stage27-c10-root-group-clippy1.jsonl`, `stage27-c10-root-group-clippy-compare1.log`).
- Disk-backed formatter guard3 защитил **551 files**, restore failures **0**.
  `just all-check` cargo check PASS, fmt опять FAIL1224 mapped transaction/tests.rs;
  rewritten files **0**, Clippy этим запуском не исполнялся. Original bytes/hash
  остаются сохранёнными (`stage27-c10-all-check3.log`, `stage27-c10-all-check-result3.txt`).

### Финальный performance decision

После root grouping свежие 1M pairs дали **+1.5807 / +2.9969 / +2.5840%**, CI upper
**+2.8605 / +4.5865 / +3.8102%** относительно C0 на той же CPU mask1.
Все три находятся в пределах 5%. Перед grouping corresponding controlled pairs
имели upper bounds 6.7–8.6%; сокращение handles оставлено. Это не доказательство
того, что весь прежний RETURN hotspot объяснялся только Arc operations.
Measured short EXE/PDB сохранены в `target/stage27-perf-root-group-short` и
`target/stage27-perf-c0-short`; capture/read matrix — в archive before-root-group.

Для precision добавлен opt-in `FG27_LONG=1`: 50 samples, 2 s warmup, 10 s measurement.
Одинаковый harness current/C0 имеет SHA256
`64296E1E9967CA013FD49FEFF33562A20145D3914FAAAED690E019AFDB3063B7`;
production C0 sources не менялись. Extended пары:

| Rows | C0 point time | Current point time | Criterion change | Change CI         |
|------|---------------|--------------------|------------------|-------------------|
| 10K  | 2.3645 µs     | 2.5066 µs          | +8.4412%         | +4.9768..+12.201% |
| 100K | 2.3330 µs     | 2.4312 µs          | +2.0400%         | +1.0388..+3.1059% |
| 1M   | 2.3699 µs     | 2.4701 µs          | +3.6460%         | +2.4885..+4.8691% |

Point-time и normalized change — разные Criterion estimates; столбец change не
пересчитан как простое отношение point times. Raw intervals/outliers сохраняются
в `target/stage27-c10-return-long-{c0,current}-{10000,100000,1000000}.log`.
100K/1M подтверждают предел; **10K не прошёл gate**, а не объявлен noise/PASS.
No-feed local DML после grouping также требует собственного paired evidence:
post-WAL mut accessors имеют новую fixed outer allocation. Graph fanout, NativeAsync
performance и реально cold storage states остаются непроверенными performance gates.

Первый extended C0 build из вложенного cwd удвоил Rust flags из parent config;
этим EXE измерения не выполнялись. По machine fingerprint удалены только 948
generated artifacts/fingerprint paths с подтверждённым duplicate flag list,
проверенными внутри named baseline target; source и measured artifacts сохранены
(`stage27-c10-invalid-config-cleanup1.txt`). Повторный build из workspace root через
`--manifest-path` PASS9.87s; exact flags/profile совпадают с current
(`stage27-c10-long-config-parity1.txt`). Default/common workload body сохранён.

Финальный `just all-check` после последних source/benchmark changes: cargo check
PASS, nightly fmt выполнен, default Clippy **FAIL: 7002 errors / 539 warnings**.
Disk-backed byte guard защитил551 Rust files, восстановил4 legacy formatter
changes (transaction/tests.rs и три graph_index files), restore failures0.
Logs `target/stage27-c10-all-check4.log`, `stage27-c10-all-check-result4.txt`,
`stage27-c10-fmt-restored4.txt`; persisted originals/hashes оставлены в target.
Mapped-file failure в этом запуске не повторился. Default/no-default counts
не сравниваются как одинаковый feature target. Commit не создавался.

Финальный `graphify update .` завершился с exit code 0: **56257 nodes,
150206 edges, 1859 communities**. Windows multiprocessing pool перешёл на
последовательный AST fallback; все 48 uncached files обработаны. Сохранены
известные warnings для `context7.json`/`config.json` и изменившихся community
labels. LLM labeling не запускался. Evidence:
`target/stage27-c10-graphify-final1.log`.

После финального formatter guard проверены все **551 protected files**:
изменённых bytes относительно originals **0**
(`target/stage27-c10-final-source-protection1.json`). Несвязанные изменения
сохранены; commit не создавался.

C10 performance/tooling gates остаются открытыми. Stage readiness не заявляется.
