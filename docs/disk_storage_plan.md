# Локальное дисковое хранилище Ferros-Grynn: sync-first

## Коррекция этапа 8.1

[`rework_plan.md`](rework_plan.md) заменяет graph-owned atom/TABLE layout на целевую
graph-first модель. Документационные A0–A4/S0–S6 и кодовые A5–A8 завершены;
A9–A16 остаются открытыми. Ранее выполненные этапы 1–7 ниже остаются историческим evidence,
но не target format.

Целевые storage-границы:

- `atoms/<generation>` хранит global atoms, base properties, primary, reverse
  memberships, base indexes и class statistics;
- `graphs/<graph_id>/<generation>` хранит occurrences, topology, labels, sparse
  overlays, membership index и explicit effective indexes;
- `AtomHotPage` хранит только global base atom; отдельные `NodeHotPage`/`EdgeHotPage` и файлы
  `nodes_hot.fgb`/`edges_hot.fgb` остаются target occurrence storage и не являются legacy;
- `RecordId = class:id` разрешается в `GlobalAtomId`; graph-local `DiskAtomRef`/
  `AtomId` не заменяет global identity;
- `PageKey`, WAL и `FileCatalog FGFC v5` используют `DataSpaceId::Database`
  или `DataSpaceId::Graph(nonzero GraphId)`; system graph не существует;
- `DatabaseCatalog` повышается до FGDC v7; FGDC v5/v6 и FGFC v4 отклоняются без
  migration, так как core format ещё не frozen;
- один database WAL атомарно публикует base, memberships, graph topology,
  overlays, indexes и statistics.

## Итоговая архитектура

- Production-код размещается в `src/storage`; незавершённый `crates/storage_engine` удаляется после сверки его незакоммиченного diff. `fg-meta` — сырой пример для проверки отдельных гипотез и сценариев, не источник требований или готовой реализации.
- `.fgb`/`.fgidx` используют Direct I/O: `O_DIRECT` на Linux, `FILE_FLAG_NO_BUFFERING` на Windows (macOS в поддержку не входит). WAL остаётся последовательным buffered I/O: commit записывает фреймы и `TxCommit`, затем выполняет один `flush + sync_data`.
- `PAGE_SIZE = 16384`, `PageHeader = 32` байта. Checksum format v1 — portable `RapidHash V3`, seed `0`, с нормативными тестовыми векторами. `xxhash-rust` и `zerocopy` не добавляются; фиксированные LE-layout кодируются безопасными stdlib-функциями.
- Sync — обязательный и стандартный режим. `mmap` существует только в бенчмарках через согласованный dev-only `memmap2`.
- Нормальный release-gate требует сначала пройти полный sync-gate. Этап 6 начат отдельным явным решением владельца проекта при зафиксированных регрессиях этапа 5; это не фиксирует format v1 и не отменяет sync-gate. Публичная query-модель остаётся синхронной pull-моделью; пользователь выбирает I/O для экземпляра хранилища через настройки.
- Linux/NVMe и Unix soak блокируют только объявление Unix-поддержки. Они не блокируют Windows
  GA или freeze core format v1; после freeze несовместимый Linux layout требует v2 и миграции.

## Контракты и публичный API

- Добавить `Datastore::create_local(path, LocalOptions)` и `Datastore::open_local(path, LocalOptions)`. Создание никогда не перезаписывает существующую БД; открытие никогда не усекает файлы.
- Ввести `DatastoreOptions::{InMemory, Local}`, сохранив `Datastore::in_memory*`. Значения `LocalOptions` по умолчанию:
    - `io_mode = IoMode::Sync`;
    - buffer pool — 4096 страниц / 64 МиБ;
    - WAL segment target — 64 МиБ с ротацией только между транзакциями;
    - checkpoint — 25% необэкфилленных страниц или 300 секунд;
    - write-set — 256 МиБ, snapshot age — 300 секунд.
	- `LocalOptions` экспонирует `io_mode`, buffer pool, write-set, snapshot age и рабочую `CheckpointPolicy { unbackfilled_page_percent, interval }`. WAL segment target остаётся фиксированной внутренней константой 64 МиБ.
- `IoMode::{Sync, NativeAsync}` доступен независимо от feature. Выбор недоступного режима возвращает типизированную ошибку, без скрытого fallback. Async-код собирается feature `native-async-io`, не входящим в default.
- Исправить `GraphRead`:
    - GAT-типы `Node<'a>: NodeRead` и `Edge<'a>: EdgeRead`;
    - `node`, `edge`, свойства и итераторы становятся fallible;
    - in-memory views сохраняют zero-copy borrow, disk views удерживают `PageGuard` и живут не дольше транзакции;
    - свойства загружаются отдельно и возвращаются как borrowed/owned `Cow<PropertyMap>`, поэтому чтение hot-полей не материализует property heap;
    - вложенные графы представлены `GraphRef`, а не внутренним дисковым адресом.
- Добавить backend-neutral point-mutation view для `Transaction`: создание/изменение/удаление атомов, свойств и incidence по `RecordId`/`AtomRef`. Текущий семейный `GraphWriteView` остаётся in-memory convenience API.
- `AtomId` остаётся transaction-local handle для графовых алгоритмов; GQL и внешние результаты используют `RecordId`. `StorageValue`, `TypeTag`, `DiskAtomRef` и адреса страниц наружу не выходят.
- Crate-private concrete `SyncPageManager` возвращает `PageRead<'tx>` и принимает `&TxHandle`;
  snapshot-регистрация не отделяется от транзакции. Одиночное и пакетное чтение идут в порядке
  staged → WAL → home. WAL сам назначает LSN и записывает `page_lsn`/checksum.

## Этапы реализации

1. **Этап 0 — синхронизация контрактов**
    - Одновременно исправить четыре спецификации: закрепить `src/storage`, RapidHash V3, buffered-WAL exception, sync-first gate и актуальный `PageManager`.
    - Устранить смешение `CommitTs` и `LSN`: снимок хранит durable `visible_lsn`; версия страницы видима только если её `commit_lsn ≤ snapshot.visible_lsn`.
    - Checkpoint может бэкфиллить не дальше самого старого живого reader watermark; иначе обновлённый `.fgb` нарушит старый снимок.
    - `PageInit` разрешить только для пустой аллокации; первая содержательная мутация обязана иметь полный `PageFrame`.
    - Зафиксировать начальный лимит atom-delta 256 МиБ и убрать обещание неограниченной транзакции из sync MVP.
    - Устранить конфликт P2/P6: Mode 2 становится текущей работой, advanced storage-функции остаются последующими этапами.

2. **Этап 1 — безопасный Sync Direct I/O**
    - Реализовать `AlignedPage` через `#[repr(align(16384))]`, RAII-файлы и позиционные exact-read/exact-write без общего file cursor.
    - Проверять смещение, размер файла, кратность странице, short I/O, format version, `page_no`, checksum и reserved bytes.
    - Не использовать `O_SYNC`/`WRITE_THROUGH` на каждой записи; долговечность задаётся только явными sync-точками.
    - Неподдерживаемая файловая система или платформа возвращает статус Direct I/O unavailable; buffered data-file fallback запрещён.

3. **Этап 2 — BufferPool и ранний I/O-gate**
	- Переписать Clock-Pro из `fg-meta` как read-only version-aware cache: фиксированные переиспользуемые `Arc<Box<AlignedPage>>`, RAII pinning, без копирования 16 КиБ на hit и без dirty flush при eviction.
	- Не держать metadata lock во время I/O; одинаковые concurrent misses объединять. При всех pinned/loading frames применять backpressure, не превышать бюджет и освобождать все reservations после ошибки batch.
	- Добавить raw Direct, Direct+BufferPool, обычный buffered positional и benchmark-only mmap backends; каждый repetition использует отдельные fresh-файлы и ротацию порядка.
	- Реализовать calibrated batch Direct I/O и `SyncPageManager::read_pages`: соседние страницы читаются одним syscall, порядок и дубликаты сохраняются, checksum проверяется до публикации.
	- Состояние на 2026-08-25: нативный Windows gate пройден. По явному решению для текущей Windows-разработки Linux/NVMe реализация и прогон, а также soak на dataset `≥ 1,25 × physical RAM`, отложены и больше не блокируют начало frame-WAL или будущую фиксацию Windows format v1. Они остаются обязательным release-gate выпуска Unix-поддержки. Результаты Windows: [disk_storage_stage2_report.md](reports/disk_storage_stage2_report.md).

4. **Этап 3 — frame-WAL, MVCC и recovery**
    - Транзакция хранит логические atom-delta до 256 МиБ. Под единым DB commit lock выполняются SSI/FCW, merge поверх последних страниц и повторная проверка graph-family invariants.
    - Финальные фреймы одного commit записываются подряд, по одному на изменённую страницу, затем `TxCommit`, `flush` и один `sync_data`; только после этого версии публикуются читателям.
    - Реализовать DB-level version index, read-your-writes, segment rotation, safe checkpoint/backfill, atomic superblock update и prune только после fsync всех затронутых data-файлов.
    - Recovery сканирует WAL один раз, отбрасывает незавершённый/повреждённый хвост, применяет только committed frames в порядке LSN и остаётся идемпотентным.
    - Единственный WAL writer и commit lock — осознанный MVP-предел; group commit добавляется только при измеренном bottleneck.
	- Состояние на 2026-08-25: срез 3a реализован в `src/storage/wal.rs` — точный WAL codec, commit-batch, ротация только между транзакциями, durable publication, version index, snapshot/read-your-writes и streaming recovery. `TxBegin` записывается в том же commit-batch, а не при создании приватного write-set. Этот срез не зависит от layout конкретных графовых страниц.
	- Исторический срез 3b реализовал typed file/page identity, Windows lock и frame-WAL. Target
	  после S2 логически версионирует AtomStore/graph superblocks полным `PageFrame`, исключает их
	  только из in-place backfill и публикует home через `.tmp/main/.prev`; `.tmp` не authority.
	- WAL format v1 использует magic `FGW1`; экспериментальный 3a magic `MVCE` отклоняется без миграции, поскольку прежний `file_kind + pad[7]` байтово неоднозначен с `DataFileId(instance_id = 0)`.
	- Unix runtime-реализация 3b намеренно отсутствует: non-Windows ownership/atomic replace возвращают typed `unsupported`. Linux/NVMe crash/performance tests и Unix release-gate обязательны до объявления Unix-поддержки, но не блокируют Windows-first этап 4.
	- Перенесённые в этап 4 `Datastore::create_local/open_local`, durable file-catalog discovery, alloc/free, конкретные graph pages, atom-level merge/SSI wiring, `CheckpointMode::Restart` и snapshot timeout/cancellation реализованы в Windows-first срезе.

5. **Этап 4 — минимальный disk-backed vertical slice**
    - Подключить уже реализованные DB/graph superblocks к durable-каталогам и реализовать freelist, hot node/edge pages, adjacency overflow, property heap/large objects, label dictionary и subgraph directory.
    - Переиспользовать существующие `StorageValue` conversion contexts и canonical codec; decode limits проверять до allocation.
    - Исторически этот срез добавил graph-wide primary `RecordId → DiskAtomRef`, TABLE
      membership и graph-owned properties. A8–A13 заменяют их global primary
      `RecordId → GlobalAtomId`, class scan и graph-occurrence/effective read.
	- Состояние на 2026-08-25: этап 4 завершён для Windows sync. Durable DB/file catalogs связывают concrete graph pages, label dictionary, primary B+tree и минимальную статистику с публичными `Datastore::create_local/open_local`; publication следует только после WAL durability.
	- Transaction-bound `LocalGraphView` и его table scan — historical implementation oracle.
      Target `GraphRead` разрешает graph-local occurrence и читает наследуемый base через
      AtomStore, а Mode 3 материализует effective view без удержания page guard.
	- `GraphMutationView` предоставляет backend-neutral create/update/delete по `RecordId`/`AtomRef`. Datastore выполняет atom-level FCW/SSI merge, после чего stable-layout commit пишет только изменившиеся страницы; изменение layout безопасно переключает новое COW-поколение. Tombstone хранит generation и переиспользуется без ABA, а startup-GC удаляет недостижимые поколения после захвата `db.lock`.
	- Публичные `CheckpointMode::{Passive, Restart}` и `CheckpointPolicy` подключены. Passive отменяет только просроченные снимки и сохраняет старых readers; Restart кооперативно отменяет зарегистрированные storage snapshots и продвигает backfill к durable tail.
	- Windows reopen-тесты проходят через публичный API и отдельно покрывают large-property chain, adjacency overflow, abort без публикации, slot-generation reuse и Restart checkpoint.

6. **Этап 5 — завершение и полный sync-gate**
    - Довести scalar B+tree indexes, generic incidence, nested graphs и `query_to_memory`; отсутствие специализированного индекса деградирует к корректному scan.
    - Проверить multi-graph atomic commit, same-atom FCW, merge разных атомов одной страницы, точные SSI predicates и graph-family constraints.
    - Зафиксировать format v1 только после прохождения correctness и performance gate; результаты и окружение сохранить в русскоязычном benchmark-отчёте.
	- Состояние функционального среза на 2026-08-26: scalar standard/unique B+tree, canonical order keys, postings, exact/range lookup, generic incidence, nested/transition graphs, recursive `query_to_memory`, scan fallback и E2E atomicity/FCW/SSI/family checks реализованы для Windows sync.
	- Correctness gate пройден: `cargo test --lib --no-default-features` — 299 passed, 2 native benchmark tests ignored; check, nightly fmt и changed-surface Clippy проходят.
	- Полный Windows performance gate повторно не пройден: sparse miss throughput составляет 77,5% raw Direct при пороге 90%, scan-resistance — 88,2% при пороге 90%. Остальные критерии, включая p99, memory, calibration и durable writes, проходят. Поэтому format v1 остаётся кандидатом; этап 6 начат только по последующему явному решению владельца проекта и не снимает эту блокировку выпуска.
	- Результаты, исправления benchmark-а и ETW/flamegraph stacks: [disk_storage_stage5_report.md](reports/disk_storage_stage5_report.md). Linux/NVMe и soak `≥ 1,25 × physical RAM` остаются отдельно отложенными release-gates.

7. **Этап 6 — optional Native Async I/O**
    - Внутренний `PageIo` реализует единый sync/native batch-контракт без отдельного async query API. `IoMode::Sync` остаётся default; `NativeAsync` собирается только с feature `native-async-io`.
    - Windows использует IoRing напрямую через сгенерированные bindings уже подключённого `windows::Win32::Storage::FileSystem`: `QueryIoRingCapabilities`, `CreateIoRing`, `BuildIoRingReadFile`/`WriteFile`, `SubmitIoRing`, `PopIoRingCompletion` и `CloseIoRing`. Самописная загрузка символов, IOCP fallback и новая зависимость не нужны.
    - Data-файл получает companion handle с `FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED`; один ring с глубиной до 128 сериализован mutex-ом. Файлы и aligned buffers удерживаются до извлечения всех CQE. Один extent читается через основной synchronous handle, поскольку профиль подтвердил накладные расходы очереди на QD1; независимые extents, batch checkpoint и writes идут через IoRing.
    - IoRing-режим требует Windows build 22000 или новее и runtime-поддержку read/write. Недоступный явно выбранный режим возвращает `FG-51I04`, без переключения режима.
    - Linux/tokio-uring полностью отложен до появления нативного тестового окружения: зависимость и непроверенный runtime-код не добавляются.
    - Buffered WAL и commit protocol не меняются; native queue ускоряет batch reads и checkpoint backfill.
    - Приёмочный QD-профиль на Windows/KIOXIA проверяет 1/8/32/128 с чередованием порядка backend-ов. Результаты, flamegraph и ограничения зафиксированы в [отчёте этапа 6](reports/disk_storage_stage6_report.md).

8. **Этап 7 — неограниченный spill**
    - Benchmark-first оценка выполнена на Windows. Production spill отложен: полный sync-gate этапа 5 не пройден и пока нет подтверждённого неделимого workload, для которого один commit обязан превышать 256 МиБ.
    - Исправлены обязательные prerequisites: property-aware оценка write-set возвращает `FG-54F07` до WAL/публикации; COW `RapidHashSet<RecordId>` устраняет O(n²) uniqueness scan; после local commit публичный root содержит disk-backed descriptor, а не полную materialized-копию графа.
    - Повторный профиль: 1 048 577 узлов создаются за 0,80 с; доля `ensure_unique_record` снизилась с 96,47% до 8,35%. При `MIMALLOC_PURGE_DELAY=0` RSS после пяти commit снизился с прежних 321,9 до 2,4 МиБ, что подтверждает устранение долгоживущего materialized root. Aggressive purge не становится default: workload замедляется примерно на 50%.
    - Оставшийся пик около 450 МиБ и рост segmented staging вызваны полной materialization внутри каждого `edit_graph`. Следующий prerequisite — delta-oriented mutation/import, а также учёт крупной adjacency, составных идентификаторов и nested graph payload в лимите.
    - Только после этих prerequisites отдельно проектируется transaction-private spill с bounded RAM, crash cleanup, atom-level rebase и неизменной commit visibility. Обычные и `BEGIN IMPORT` batch-транзакции не должны регрессировать; до этого bulk import коммитится порциями.
    - Методика, медианы, allocator experiment и flamegraph: [disk_storage_stage7_report.md](reports/disk_storage_stage7_report.md).

## Проверки и критерии приёмки

- Unit/component: golden RapidHash vectors, page codecs, malformed headers, Clock-Pro transitions, concurrent positional I/O, version resolution и WAL parser.
- Crash matrix: сбой до WAL sync, после sync до publication, во время backfill, после data fsync до prune, truncated/corrupt WAL tail и torn data page.
- MVCC: старый reader не видит поздний commit даже после checkpoint; read-your-writes; disjoint atom merge; same-atom conflict; phantom conflict; атомарность нескольких графов.
- End-to-end: все persistable Value tags, overflow/large properties, reopen, rollback, point `RecordId`,
  class/base scan, graph-occurrence/effective scan, nested graph и recovery без утечки storage DTO.
- Performance profile: dataset `max(1 GiB, 4 × pool)`, фиксированный seed и три fresh-file repetition. Single-page miss Direct+BufferPool должен дать не менее 90% raw Direct throughput при p99 не выше 125%; resident-hit p99 может превышать mmap не более чем на 0,5 мкс.
- Sequential batch должен дать не менее 90% fresh-file mmap first-touch throughput. Scan-resistance требует не менее 99% hits по заранее прогретому hot set и не менее 90% raw batched Direct throughput. Durable Direct writes сравниваются с mmap при одном одинаковом durability barrier.
- Storage RSS Direct не превышает 110% buffer-pool budget. Random/oversized mmap остаются информационными: его mapped RSS и системный file cache не входят в Direct memory gate.
- Текущая разработка и этап 3 проверяются нативно на Windows. Linux/NVMe реализация и прогон явно отложены из-за отсутствия тестового окружения; до их отдельного выполнения Windows-результаты не объявляются эквивалентными Unix и Unix-поддержка не считается готовой к выпуску.
- На каждом этапе: targeted tests/checks, `cargo +nightly fmt -- --check`, Clippy по затронутым targets/features. В финале — полный workspace check с отдельной фиксацией несвязанных ошибок и обновление Graphify.

## Явно вне начального scope

Production mmap, public async query API, direct-I/O WAL framing, group commit, sharded BufferPool, SPDK, changefeed и Fulltext/HNSW/R-tree/reachability/path indexes. Они добавляются только после измеримой необходимости.
