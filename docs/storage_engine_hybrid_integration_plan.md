# Этап 3.2. Внедрение гибридного storage engine

## Статус и нормативные границы

**Статус на 2026-09-04:** H0-H13 выполнены, production cutover подтверждён и
проведён, отчёт и компактный CSV созданы, Graphify обновлён. H14 остаётся
отдельной зависимой работой после delta-oriented foundation этапа 4.

Этап следует после завершённой test-only кампании 3.1 и переносит в production
только подтверждённую Pareto-комбинацию из
`reports/storage_engine_prototype_report.md`. Нормативными источниками остаются
`disk_storage_spec.md`, `gql_spec.md` и `graphs.md`; `gql_tech_spec.md`
используется только как ненормативный ориентир будущего physical cursor.

Формат страниц, checksum, порядок видимости `staged -> WAL -> home`, WAL,
checkpoint/recovery, write path и публичные query-result типы не меняются.
После прохождения correctness и performance gates `SyncPageManager` был
переключён на новый engine. Старый Clock-Pro исключён из production-сборки и
компилируется только для тестов и measurement API прототипного крейта;
постоянного runtime-переключателя между engines нет.

## Выбранная комбинация

| Физический паттерн                                    | Production-механизм                                         | Причина выбора                                                                                                             |
|-------------------------------------------------------|-------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------|
| Hot point/root/adjacency внутри bounded cursor window | R8 atomic arena, source-bound locator и один read epoch     | Убирает lookup, pin/drop и notification из каждой логической операции; достиг 93,9–100,5% mmap на общем 1/4/8-file hot set |
| Одиночный непредсказуемый miss                        | Sync validated Direct с admission для point/index intent    | Batching невозможен, а дополнительная очередь только увеличивает latency                                                   |
| B+tree leaf/range, one-shot scan, bulk build          | Rocks-like bypass, `ReadFileScatter`, prefetch depth 1      | Не загрязняет resident cache и выигрывает у mmap на последовательных и превышающих RAM наборах                             |
| Известный sparse graph/property frontier              | R8 для resident страниц и frame-IoRing QD32 для cold window | Подтверждённый выигрыш 457,6–506,3% mmap при заранее известном bounded окне                                                |
| mmap                                                  | Только benchmark-control                                    | Силен на resident bytes, но не даёт заданного bounded RSS и проигрывает Direct при подтверждённых page faults              |

R2, R9, VM cache, DashMap, отдельная lock-free directory, hazard pointers,
custom allocator и InnoDB-like cold admission не переносятся. Cold admission
может вернуться только после paired-теста реального повторного leaf reuse с
выигрышем не менее 5% при том же общем memory budget.

## Внутренняя архитектура

В `src/storage` создаётся один конкретный `HybridReadEngine`; trait, factory и
пользовательская настройка engine kind не нужны. Поток чтения:

```text
storage cursor
  -> source resolution: staged -> visible WAL -> home
  -> exact-version resident probe
  -> route(ReadIntent, ReadWindow, IoMode, platform capabilities)
  -> I/O and checksum validation
  -> page decoder producing owned values
```

Минимальные внутренние типы:

- `HybridReadEngine` владеет resident arena, mapping directory, replacement и
  transport resources;
- `ReadIntent`: `Point`, `IndexTraversal`, `SequentialScan`, `BulkBuild`;
- `ReadWindow`: одна страница, contiguous run одного source или bounded sparse
  set;
- `ReadRoute`: выбранный resident/Direct/scatter/IoRing path;
- `ReadScope`/`PhysicalReadCursor`: read epoch, resolved table, временные
  buffers, cancellation и join;
- `SourceState`: `(GraphId, DataFileId)` и точный `PageCacheVersion`;
- `TransientArena`: ограниченная память transports вне resident frames.

`GraphRead`, `RowCursor`, `IoMode`, `GraphStorageMode`, `StoreBackend` и
публичные query results сохраняют текущий контракт.

### R8 resident core

- Страницы размещаются в стабильном contiguous aligned arena; metadata
  хранится отдельно и выравнивается по cache line.
- Lifecycle, generation, pin и usage упакованы в атомарное слово.
- Общий resolve использует шардированный `RapidHashMap` под
  `parking_lot::RwLock`; replacement lock берётся только на miss/eviction.
- Одинаковые concurrent misses коалесцируются frame-local signal.
- Обычный `read_page` использует atomic pin и guard. Physical cursor входит в
  один bounded read epoch и обращается через cursor-local resolved table без
  per-operation pin/drop.
- Evicted frame сначала становится retired и не переиспользуется, пока жив
  защищающий epoch.
- Read epoch ограничен 256 логическими page operations и ротируется перед
  внешним ожиданием, при смене source epoch, cancellation/error либо
  exhausted retired capacity.
- Page reference не покидает scope; decoder возвращает owned row/record/value.
- Counters агрегируются на границе scope, а не обновляются глобальным атомиком
  на каждом hit.

### Source identity и budget

Ключ всегда содержит `GraphId`, `DataFileId` и точную home/WAL version.
`buffer_pool_pages` — один общий resident budget для всех файлов и версий, без
per-file quota.

Home source получает ленивый двухуровневый locator. Leaf chunk создаётся только
для затронутого диапазона и удаляется, когда больше не содержит resident
entries. WAL использует bounded cursor-local resolved table: file-sized table
на каждый LSN запрещена. Смена checkpoint generation создаёт новый
`SourceState`; старая версия живёт до закрытия использующих её scopes.

При `P = buffer_pool_pages` транспортный бюджет равен:

```text
transient_pages = min(288, floor(P / 10))
```

Scatter использует два reusable набора до 128 страниц каждый; IoRing — до 32
cold frames. При малом бюджете окна уменьшаются. Если минимальное полезное окно
невозможно выделить, sync Direct выбирается до submission. Resident и
transient bytes вместе не превышают 110% frame budget; metadata показывается
отдельно.

### Детерминированный router

1. Exact resident hit всегда обслуживается R8 независимо от scan policy.
2. Staged page имеет приоритет и не заменяется home page.
3. Видимая WAL version обслуживается только как точная WAL version.
4. Point/index single home miss читается sync Direct, проверяется и admitted.
5. Sequential/bulk contiguous home misses идут через bypass + scatter + P1 и
   не меняют resident count.
6. Известное sparse-окно использует R8 для hot и IoRing для не более 32 cold
   pages; cold результат не admitted.
7. Непредсказуемый singleton использует sync validated Direct.
8. Неприменимость native transport определяется до submission. После начала
   I/O ошибка, short read или checksum failure возвращается без retry другим
   transport.

`IoMode::Sync` разрешает R8, sync Direct и ожидаемый текущим потоком Windows
scatter. `IoMode::NativeAsync` добавляет frame-IoRing. Недоступный явно
запрошенный NativeAsync остаётся типизированной ошибкой. Физическую policy
выводит storage cursor, а не GQL logical IR.

### Bypass и prefetch

Bypass сначала проверяет resident cache. Cold extent читается во временный
buffer, целиком валидируется и только затем передаётся consumer; partial extent
никогда не публикуется. P1 использует один scoped worker, два reusable buffers
и bounded `sync_channel`. Порядок сохраняется, а drop/LIMIT/error выполняет
cancellation и обязательный join.

## Режимы хранения

### Mode 1 — Full In Memory

`StoreBackend::InMemory` не создаёт physical read intents и не обращается к
arena, Direct I/O или prefetch. Backend-neutral query cursor читает immutable
in-memory root. Режим остаётся volatile; добавление snapshot/WAL durability в
этот этап не входит. Допустимая регрессия query throughput — не более 3%.

### Mode 2 — Full On Disk

Local datastore использует `HybridReadEngine` для всех физических чтений:
hot roots/point/adjacency — R8, leaf/table/bulk ranges — bypass + scatter + P1,
known sparse frontier — R8 + IoRing QD32, unpredictable miss — sync Direct.
Transaction snapshot и `staged -> WAL -> home` остаются неизменными, а наружу
выдаются owned domain/query values, не page identities.

### Mode 3 — Query To Memory

Mode 3 остаётся явной materialization operation, а не новым `StoreBackend`.
Source фиксируется на одном `SnapshotId`; disk source читается через Mode 2,
memory source — напрямую. Материализация идёт bounded окнами: сначала nodes и
properties, затем edges/incidence через source-to-target mapping. После успеха
source snapshot освобождается; при ошибке или cancellation partial target
отбрасывается.

Результат владеет данными, хранит origin, не удерживает page guards и сообщает
`GraphStorageMode::Hybrid`, после чего читается как Mode 1. Frozen не получает
flush. Mutable сохраняет source mapping и dirty state; explicit delta flush
реализуется только после delta-oriented mutation foundation этапа 4. Flush не
выполняется из `Drop`, работает в новой транзакции и очищает dirty state только
после успешного commit.

## Dependency-ordered реализация

| ID  | Зависимость     | Результат                                                                                                            |
|-----|-----------------|----------------------------------------------------------------------------------------------------------------------|
| H0  | Этап 3.1        | План подключён к `full_plan.md`; prototype-крейт входит в workspace; baseline и неизменяемые contracts зафиксированы |
| H1  | H0              | Минимальный R8 core перенесён в `src/storage`, старый production path не изменён                                     |
| H2  | H1              | Exact source lifecycle, sparse locators и единый multi-file budget                                                   |
| H3  | H2              | Bounded read scope, router и transient budget                                                                        |
| H4  | H3              | Point path нового engine проверен через временный test-only selector с сохранением staged/WAL/home                   |
| H5  | H4              | Rocks-like bypass, Windows scatter и P1 pipeline                                                                     |
| H6  | H4              | Sparse frame-IoRing QD32; H5 и H6 независимы друг от друга                                                           |
| H7  | H5, H6          | Physical B+tree/table/graph/property cursors формируют bounded windows без полного page plan                         |
| H8  | H7              | Eager `Vec<QueryStorageRow>` заменён внутренним memory/local stream; SSI lifecycle сохранён                          |
| H9  | H8              | Bounded Mode 3 materialization с origin и гарантированным snapshot cleanup                                           |
| H10 | H9              | Correctness/concurrency/platform gate пройден в `fg-storage-engine-prototypes`                                       |
| H11 | H10             | Production-API performance gate и source-level profiling пройдены                                                    |
| H12 | H11             | `SyncPageManager` переключён на `HybridReadEngine`; старый Clock-Pro и selector удалены                              |
| H13 | H12             | Финальная проверка, отчёт, CSV/logs и `graphify update .`                                                            |
| H14 | H9, H12, этап 4 | Explicit Mutable delta flush отдельным изменением                                                                    |

### Фактическое выполнение

- H0-H6 завершены: production R8, exact source lifecycle, router, point path,
  scatter/P1 и frame-IoRing перенесены в `src/storage`.
- H7 завершён для существующих физических table/B+tree sequential cursors:
  они формируют ограниченные окна и не строят полный page plan. Транспорт
  known-sparse готов и проверен, но текущий query/graph cursor ещё не выдаёт
  доказуемое sparse lookahead-окно; до появления такого окна router его не
  выбирает и использует sync Direct.
- H8-H9 завершены: query rows выдаются backend-neutral stream, а Mode 3
  материализует owned graph двумя проходами с origin metadata.
- H10-H11 пройдены. Результаты и оговорки зафиксированы в
  `reports/storage_engine_hybrid_integration_report.md` и
  `reports/storage_engine_hybrid_integration_results.csv`.
- H12 выполнен после явного подтверждения: selector удалён, production
  `SyncPageManager` всегда владеет `HybridReadEngine`.
- H13 выполнен; H14 намеренно не входит в этот cutover.

## Correctness gate

Проверяются byte parity с raw Direct, corruption/short read/EOF, all-or-none
publication, staged/WAL/home visibility, concurrent exact versions, checkpoint
generation change, stale handles, ABA, epoch/eviction, coalesced miss,
backpressure, order и duplicates, 1/4/8-file budget, bypass pollution,
prefetch/IoRing cancellation и join, early cursor drop, Mode 1/2/3 parity и
SSI/FCW при streaming. Atomic state machine проверяется Shuttle-сценариями;
unsafe остаётся в минимальных arena/Windows wrappers с локальными `SAFETY`
доказательствами.

## Performance gate

- Generic R8 hit: цель `<=70 ns`, предел `<=80 ns`.
- Epoch/resolved hit: цель `<=35 ns`, предел `<=45 ns`.
- Resident throughput: цель `>=95% mmap`, обязательный floor `>=90%`.
- Resident p99: цель `mmap + 0,1 us`, предел `mmap + 0,2 us`.
- Масштабирование 2–12 потоков: не менее 80% mmap.
- Sequential: не менее 90% raw Direct и 95% лучшего validated transport.
- Fixed hot set: не менее 99% hit ratio.
- B+tree mixed: не хуже mmap более чем на 3%.
- Known sparse IoRing: не хуже mmap и цель не менее 2x sync QD1.
- Mode 1 regression: не более 3%.
- Resident + transient bytes: не более 110% pool budget.

Resident тесты используют не менее 9 paired runs, финальные — 15; I/O тесты
не менее 5, финальные — 7. Проверяются 1/4/8 файлов, 1/2/4/8/12 потоков,
hot sets 1/64/1024/4096 и pool 0,5–1,5x общего hot set. Наборы 0,9x/1,15x/2x
доступной RAM принимаются как mmap-eviction evidence только при подтверждённых
ETW transition/hard faults и измеренном working set.

Если обязательный gate не пройден, H12 не выполняется. Отчёт называет
остаточный bottleneck и один следующий минимальный эксперимент.

## Профилирование и артефакты

Profiler executable всегда собирается `cargo test --locked --profile
profiling -p fg-storage-engine-prototypes ... --no-run`. Для VTune используется
ровно напечатанный Cargo EXE и same-stem PDB из effective target directory;
перед анализом проверяются Rust function/source names в exported report. WPR/WPA
используется для I/O, waits и page faults. Ошибка инструмента оформляется как
`SKIP` с командой и логом.

Итоговые данные сохраняются в `target/storage-engine-prototype/`, компактный
CSV и `reports/storage_engine_hybrid_integration_report.md` — в `docs`.
После изменения production-кода выполняется `graphify update .`.
