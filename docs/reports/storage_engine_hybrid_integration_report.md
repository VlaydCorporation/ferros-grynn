# Отчёт по этапу 3.2: внедрение гибридного storage engine

Дата: 2026-09-04.

## 1. Итог

Production read path переключён с Clock-Pro `BufferPool` на единый
`HybridReadEngine`. Переключателя engine kind в runtime нет: обычная сборка
содержит только новый путь, а прежний Clock-Pro компилируется лишь в тестах и
под non-default feature `storage-engine-prototype-api` как benchmark-control.

В production перенесены четыре подтверждённых физических пути:

| Паттерн                                | Путь                                                                            |
|----------------------------------------|---------------------------------------------------------------------------------|
| Resident point/root/adjacency          | R8 atomic arena; bounded read epoch и source-bound resolved table внутри cursor |
| Непредсказуемый одиночный miss         | Sync validated Direct с admission в R8                                          |
| Последовательный leaf/table/bulk range | Resident probe, затем Rocks-like bypass + `ReadFileScatter` + prefetch depth 1  |
| Заранее известное sparse-окно          | Resident probe, затем frame-IoRing QD32 для cold pages без admission            |

Формат страницы, checksum, `staged -> visible WAL -> home`, exact WAL/home
version semantics, checkpoint/recovery и write path не изменены. mmap остаётся
только контрольной реализацией benchmark.

Все обязательные correctness gates и минимальные performance floors, на
которых было основано решение о cutover, пройдены. Две более строгие stretch
цели resident microcost не достигнуты: generic hit равен `74,898 ns` при цели
`<=70 ns`, epoch/resolved hit — `40,613 ns` при цели `<=35 ns`. Оба результата
укладываются в утверждённые cutover-пределы `80 ns` и `45 ns`.

Одна интеграционная граница оставлена намеренно: frame-IoRing реализован,
проверен и измерен, но существующий physical graph/property cursor пока не
может доказать bounded sparse lookahead. До появления такого окна production
router не выбирает IoRing и детерминированно использует sync Direct. Это не
подменяется эвристическим предугадыванием страниц.

## 2. Что перенесено в production

### 2.1. R8 Atomic Arena

`HybridReadEngine` владеет одной стабильной выровненной ареной 16-КиБ frames.
Payload и cache-line-aligned metadata разделены. Lifecycle, generation, pin и
usage упакованы в атомарное состояние. Общий resolve выполняется через
шардированный `RapidHashMap` под `parking_lot::RwLock`; replacement lock нужен
только на miss/eviction.

Обычный `read_page` сохраняет guard-контракт и атомарный pin. Физический cursor
использует более дешёвый R8-путь:

1. окно не более 256 логических page operations разрешается один раз;
2. один read epoch защищает адреса frames внутри окна;
3. повторные обращения идут через cursor-local resolved table без hash lookup
   и pin/drop на каждую операцию;
4. вытесненный frame остаётся retired до завершения защищающего epoch;
5. page reference не выходит из scope, декодер возвращает owned значение.

Одинаковые concurrent misses коалесцируются. Обязательный глобальный hit
counter из hot path удалён; диагностические counters доступны только в
measurement surface.

### 2.2. Source lifecycle и общий бюджет

Ключ resident-страницы различает `GraphId`, `DataFileId` и точную
`PageCacheVersion`: home checkpoint generation либо WAL LSN. При обходе home
source `SyncPageManager` получает exact source key, создаёт ленивый locator и
удаляет устаревшие home generations только после завершения активных scopes.

Home locator двухуровневый и создаёт chunks только для затронутых диапазонов.
Авторитетом остаётся arena directory: если point path admitted страницу до
создания locator entry, последующий resolve находит её в arena и обновляет
locator. Пустые chunks удаляются. Поэтому последовательный scan не оставляет
file-sized таблицу адресов, а point и cursor paths видят одну и ту же exact
version.

`buffer_pool_pages` — общий resident budget для всех графов, файлов и версий;
per-file quota нет. WAL-exact страницы не создают file-sized locator на каждый
LSN и разрешаются ограниченной cursor-local таблицей.

### 2.3. Router и transports

Router — чистая таблица по `ReadIntent`, `ReadWindow`, `PageIoMode` и заранее
известным platform capabilities. Неприменимый transport заменяется sync Direct
до submission. После начала I/O ошибка, short read или checksum failure
возвращается вызывающему коду; повтор другим transport запрещён.

Последовательный path соответствует RocksDB `fill_cache=false`: точный
resident hit всё равно используется, но cold extent проходит через transient
buffers и не участвует в replacement. `ReadFileScatter` читает extent в
выровненную память; весь extent валидируется до выдачи первой страницы.
Prefetch P1 держит два reusable buffer-набора и один bounded worker. Drop,
LIMIT, error и cancellation прекращают выдачу, отменяют неопубликованное и
выполняют обязательный join.

Frame-IoRing принимает не более 32 cold frames в окне до 256 операций,
сохраняет порядок и duplicates, валидирует каждую страницу и не admitted cold
результат. В `PageIoMode::Sync` этот маршрут недоступен; явно выбранный
`NativeAsync` сохраняет типизированную ошибку при отсутствии platform support.

### 2.4. Transient memory

Для `P = buffer_pool_pages` применяется:

```text
transient_pages = min(288, floor(P / 10))
```

Scatter получает до двух наборов по 128 страниц, IoRing — до 32 страниц.
Размеры уменьшаются при малом pool; если полезное окно не помещается, fallback
выбирается до I/O. При `P=4096` это 4096 resident + 288 transient frames, или
`107,03125%` frame budget. Metadata учитывается отдельно.

## 3. Query cursor и режимы хранения

Eager цепочка `Transaction::query_rows -> Vec<QueryStorageRow>` заменена
backend-neutral streaming cursor. Memory source итерирует immutable root,
local source владеет `LocalTableCursor` и bounded physical scope, индексный
source использует уже ограниченный набор candidates. Наружу по-прежнему
выдаются owned `QueryStorageRow` и существующий `RowCursor`.

Predicate регистрируется при создании cursor; фактически выданные atoms
учитываются по мере чтения. При раннем LIMIT, drop или ошибке консервативный
predicate остаётся частью транзакции. Snapshot, FCW/SSI и публичные query
типы не менялись.

| Режим                    | Фактическое поведение после cutover                                                                                                                                           |
|--------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Mode 1 — Full In Memory  | `HybridReadEngine` не вызывается; cursor читает immutable in-memory root. В paired-тесте регрессии нет                                                                        |
| Mode 2 — Full On Disk    | Все физические чтения проходят через новый resident/router/transport контур при прежнем snapshot и WAL precedence                                                             |
| Mode 3 — Query To Memory | Source фиксируется на одном snapshot, local source читается как Mode 2, результат двумя проходами становится самостоятельным owned graph с origin и затем читается как Mode 1 |

Mode 3 сначала материализует nodes/properties, затем edges/incidence через
source-to-target mapping. Bounded scopes закрываются между окнами. Успешный
результат не удерживает source transaction или page guards; при ошибке
частичный target уничтожается. Frozen не получает flush API. Mutable dirty
state и origin подготовлены, но explicit delta flush остаётся H14 после
delta-oriented mutation foundation этапа 4.

## 4. Выполнение dependency-ordered этапов

| Этап | Статус                        | Результат                                                                                              |
|------|-------------------------------|--------------------------------------------------------------------------------------------------------|
| H0   | PASS                          | Контракты и H11 baseline зафиксированы, план подключён к `full_plan.md`                                |
| H1   | PASS                          | Минимальный R8 core перенесён в `src/storage`                                                          |
| H2   | PASS                          | Exact source identity, lazy locators, generation retirement и общий multi-file budget                  |
| H3   | PASS                          | `ReadIntent`, `ReadWindow`, `ReadRoute`, bounded scope и transient budget                              |
| H4   | PASS                          | Point path сохраняет staged/WAL/home precedence и checksum contract                                    |
| H5   | PASS                          | Rocks-like bypass, Windows scatter и P1 pipeline                                                       |
| H6   | PASS с границей потребителя   | Frame-IoRing QD32 реализован и проверен; production cursor пока не создаёт known-sparse window         |
| H7   | PASS для существующих cursors | Table/B+tree sequential cursors используют bounded windows; speculative sparse planner не добавлен     |
| H8   | PASS                          | Query path переведён с eager `Vec` на memory/local/index stream                                        |
| H9   | PASS                          | Owned двухпроходная Mode 3 materialization с origin и cleanup                                          |
| H10  | PASS                          | Correctness/concurrency/platform матрица прошла в prototype crate                                      |
| H11  | PASS                          | Все обязательные performance floors пройдены; stretch microcost отмечен отдельно                       |
| H12  | PASS                          | После явного подтверждения `SyncPageManager` переключён на `HybridReadEngine`; runtime selector удалён |
| H13  | PASS                          | Проверки, профиль, отчёт, компактный CSV и Graphify обновлены                                          |
| H14  | DEFER                         | Зависит от этапа 4 и не входит в текущий cutover                                                       |

`storage-engine-prototype-api` не выбирает production engine и не меняет
поведение. Скрытый non-default feature сохранён только потому, что утверждённый
внешний workspace-крейт должен продолжать проверять production primitives.
Полное удаление этого measurement API сделало бы
`fg-storage-engine-prototypes` некомпилируемым; selector и альтернативный
production path при этом действительно удалены.

## 5. Correctness gate

Финальные команды после cutover:

```powershell
cargo check --locked --lib --no-default-features
cargo check --locked --lib --features native-async-io
cargo test --locked --lib --features native-async-io
cargo test --locked -p fg-storage-engine-prototypes --features native-async-io
```

Результат:

- production tests: `314 passed`, `0 failed`, `8 ignored` native benchmark
  probes;
- prototype tests: `93 passed`, `0 failed`, `14 ignored` explicit profiling
  probes;
- оба `cargo check` завершены успешно.

Матрица покрывает byte parity, checksum corruption, EOF/short read, rollback
reservation, all-or-none publication, exact home/WAL versions, checkpoint при
активном scope, stale handle и ABA, pin/epoch во время eviction, coalesced
miss, backpressure, 1/4/8 файлов, duplicate keys и order, bounded locator и
transient memory, bypass pollution, scatter/prefetch cancellation и join,
IoRing completion/error, early streaming drop, SSI predicate lifetime и owned
Mode 3 result. Критические interleavings pin/eviction и capacity wakeup
проверяются Shuttle-сценариями.

Unsafe ограничен выровненной arena-address арифметикой и Windows wrappers.
Адрес payload выдаётся только после успешной generation/lifecycle проверки;
epoch запрещает reuse; scatter/IoRing buffers, segment arrays, `OVERLAPPED` и
events живут до completion; публикация возможна только после полной checksum
validation.

## 6. Методика performance gate

Финальные resident серии содержат 15 runs для isolated costs и multi-file,
12 paired observations на каждое значение concurrency. I/O и mixed серии
содержат по 7 paired runs с ротацией порядка. CPU affinity фиксировалась для
microprobes.

Машина по сообщению пользователя работала от сети, с максимальным режимом
производительности и отключённой парковкой ядер. Harness одновременно записал
Windows power-scheme GUID с подписью `Balanced`. Настройки повторно не
изменялись: результаты считаются по paired ratios одного запуска, а это
расхождение сохранено как атрибут воспроизводимости.

### 6.1. Resident microcost

| Кандидат             |  Median mean |       Min..max |   p50 |    p99 | Cutover gate | Результат                          |
|----------------------|-------------:|---------------:|------:|-------:|-------------:|------------------------------------|
| Generic R8 guard hit | 74,898 ns/op | 73,427..76,929 | 72 ns | 126 ns |      <=80 ns | PASS; stretch <=70 ns не достигнут |
| Epoch/resolved hit   | 40,613 ns/op | 39,090..42,140 | 39 ns |  66 ns |      <=45 ns | PASS; stretch <=35 ns не достигнут |

### 6.2. Concurrency относительно mmap

| Threads | Paired median R8/mmap |       Min..max |
|--------:|----------------------:|---------------:|
|       1 |                99,64% | 97,79..100,42% |
|       2 |                96,68% | 93,42..118,28% |
|       4 |                95,88% |  87,17..96,86% |
|       8 |                94,75% | 84,55..101,69% |
|      12 |                94,10% |  87,53..98,48% |

Минимальный gate `>=80% mmap` пройден во всех точках. Падения до текущей
глобальной lock-сериализации Clock-Pro нет; median остаётся выше 94% даже при
12 threads.

### 6.3. Общий hot set нескольких файлов

Общий hot set и pool равны 4096 страниц независимо от числа files:

| Files | Paired median R8/mmap | R8 p99 | mmap p99 |
|------:|----------------------:|-------:|---------:|
|     1 |               116,06% |  21 ns |    22 ns |
|     4 |               115,50% |  20 ns |    22 ns |
|     8 |               115,01% |  20 ns |    22 ns |

Это подтверждает именно общий, а не per-file budget и отсутствие эффекта
«pool помнит только последний файл».

### 6.4. Sequential transport

| Кандидат                | Median MiB/s | p50 extent | p99 extent |   RSS delta |
|-------------------------|-------------:|-----------:|-----------:|------------:|
| Raw Direct              |     2311,170 |   840,4 us |  1387,5 us | 2 121 728 B |
| Validated Direct        |     1794,692 |  1070,5 us |  1693,7 us | 2 117 632 B |
| Production Scatter + P1 |     2270,067 |   851,6 us |  1457,3 us |   106 496 B |

Scatter P1 даёт paired median `98,22%` raw Direct и `127,54%` validated
Direct. Обязательные `>=90% raw` и `>=95% best validated` пройдены. Это
устраняет прежнюю потерю около 500–1000 MiB/s: checksum перекрывается с
чтением следующего extent, cold pages не копируются в replacement pool.

### 6.5. Scan с fixed hot set

| Кандидат            | Median MiB/s | p50 page | p99 page | Hit ratio |       RSS delta |
|---------------------|-------------:|---------:|---------:|----------:|----------------:|
| Raw Direct          |     1924,758 |  8923 ns | 11482 ns |        0% |     2 109 440 B |
| mmap                |     1264,125 |   314 ns | 18315 ns |      100% | 1 073 750 016 B |
| R8 hot + Scatter P1 |     1870,134 |    59 ns | 13800 ns |      100% |    17 858 560 B |

Production path достигает paired median `98,05%` raw и `148,15%` mmap при
`100%` resident hit ratio. В одной из семи пар было падение до `65,87% raw`;
остальные пары и median gate проходят. Это остаётся наблюдаемым I/O outlier,
а не скрывается усреднением.

### 6.6. DB-specific mixed workloads

| Workload                               |    Production |          mmap | Production/mmap | Production p99 |  mmap p99 |
|----------------------------------------|--------------:|--------------:|----------------:|---------------:|----------:|
| B+tree hot roots + disjoint leaf scan  | 151 581 ops/s |  62 422 ops/s |         232,93% |      16,067 us | 17,955 us |
| Graph hot frontier + sparse cold pages | 334 176 ops/s | 125 648 ops/s |         273,54% |       3,606 us | 16,711 us |
| Property hot window + sparse overflow  | 401 976 ops/s | 127 108 ops/s |         300,50% |       3,383 us | 15,980 us |

Sparse IoRing даёт `418,54%` соответствующего sync-QD1 path для graph и
`465,73%` для properties, поэтому gate `>=2x sync` также пройден. Эти цифры
доказывают ценность транспорта при реальном lookahead, но не разрешают
применять его к непредсказуемому singleton miss.

### 6.7. Mode 1

15 paired runs на 4096 rows x 1000 repetitions дали median
`Hybrid/legacy = 1,310026`, min `1,044043`, max `1,428298`. Регрессии Mode 1
нет; обязательный предел `<=3%` выполнен.

### 6.8. Файлы около и больше RAM

| Размер / RAM |     Scatter P1 | mmap first touch | Ускорение | mmap process faults | mmap working-set delta |
|-------------:|---------------:|-----------------:|----------:|--------------------:|-----------------------:|
|        0,90x | 1739,235 MiB/s |    490,076 MiB/s |     3,55x |           3 714 989 |         около 6,57 GiB |
|        1,15x | 2403,795 MiB/s |    696,721 MiB/s |     3,45x |           4 746 924 |         около 8,38 GiB |
|        2,00x | 2315,182 MiB/s |    734,797 MiB/s |     3,15x |           8 255 506 |         около 8,31 GiB |

Scatter working-set delta составила лишь 114 688, 118 784 и 122 880 B для
трёх размеров. Для 1,15x-RAM WPR/WPA независимо классифицировал 4 737 628
unique mapped faults: 4 702 941 transition и 34 687 hard faults; hard-fault
I/O — 19 643 416 576 B. mmap создаёт четыре 4-КиБ OS faults на одну 16-КиБ
страницу БД. Следовательно, это подтверждённый fault-heavy workload, а не
предположение только по размеру файла.

## 7. VTune после cutover

Профильная сборка выполнена только с `--profile profiling`:

```powershell
cargo test --locked --profile profiling `
  -p fg-storage-engine-prototypes `
  --features native-async-io `
  storage_engine_prototype_fixed_hot_scan_probe --no-run
```

Использована точная напечатанная Cargo пара:

| Артефакт                                                                  |       Размер | Timestamp           |
|---------------------------------------------------------------------------|-------------:|---------------------|
| `target/profiling/deps/fg_storage_engine_prototypes-f40e40ccf9240234.exe` |  7 767 040 B | 2026-09-04 15:15:23 |
| `target/profiling/deps/fg_storage_engine_prototypes-f40e40ccf9240234.pdb` | 99 979 264 B | 2026-09-04 15:15:23 |

Короткий post-cutover smoke этой же сборки прошёл за 1,30 s и записал
`112 535 ops/s`, `1758,363 MiB/s`, p50 `67 ns`, p99 `14,015 us`, RSS delta
17 928 192 B и hit ratio 1,0.

Hardware Hotspots capture сохранён в
`target/storage-engine-prototype/profilers/vtune-h12-cutover-fixed-hot-20260904`.
Профилировался isolated generic R8 probe на 100 000 000 операций и одном
закреплённом CPU; тест прошёл за 21,10 s. VTune показал 21,374 s elapsed,
15,190 s CPU и 0,017 s spin.

| Hotspot                              | CPU time | Доля CPU |
|--------------------------------------|---------:|---------:|
| `measure_resident_runs`              |  6,991 s |    46,0% |
| `AtomicArenaBufferPool::release_pin` |  1,313 s |     8,6% |
| `AtomicArenaBufferPool::try_pin`     |  1,112 s |     7,3% |
| `parking_lot` shared unlock          |  1,099 s |     7,2% |
| `parking_lot` shared lock            |  1,081 s |     7,1% |
| root mapping lookup                  |  0,360 s |     2,4% |

Exported function и source-line CSV содержат Rust symbol names и строки
`harness.rs`, `hybrid_engine/arena.rs` и `page_cache.rs`. Значит EXE/PDB
соответствуют друг другу и профиль пригоден для интерпретации. Предупреждения
касались только отсутствующих PDB системных DLL Windows; статус для project
symbols — PASS, для системных symbols — WARN.

Профиль объясняет, почему generic guard path остаётся около 75 ns: atomics
pin/release и shared directory lock нужны на каждое независимое обращение.
Cursor-scope путь не «ускоряет mutex», а убирает эти действия из каждой
логической операции, поэтому его стоимость падает до 40,6 ns и production
mixed workloads обгоняют mmap. DashMap или другая hash map не устранили бы
pin/release и потому не являются следующим минимальным изменением.

## 8. Gate summary и решение о cutover

| Метрика                      |            Минимум |                                             Факт | Статус |
|------------------------------|-------------------:|-------------------------------------------------:|--------|
| Generic R8 internal cost     |            <=80 ns |                                        74,898 ns | PASS   |
| Epoch/resolved internal cost |            <=45 ns |                                        40,613 ns | PASS   |
| Full resident throughput     |         >=90% mmap | 94,10..116,06% по concurrency/multi-file medians | PASS   |
| Resident p99                 |    <=mmap + 0,2 us |   +44 ns isolated; multi-file R8 на 1–2 ns лучше | PASS   |
| 2–12 thread scaling          |         >=80% mmap |                            94,10..96,68% medians | PASS   |
| Sequential batched           |          >=90% raw |                                           98,22% | PASS   |
| Fixed-hot hit ratio          |              >=99% |                                             100% | PASS   |
| B+tree root + leaf           |   не хуже mmap >3% |                                     232,93% mmap | PASS   |
| Known sparse                 | >=mmap и >=2x sync |             273,54–300,50% mmap; 4,19–4,66x sync | PASS   |
| Mode 1 regression            |               <=3% |                          отсутствует; min +4,40% | PASS   |
| Resident + transient frames  |        <=110% pool |                                       107,03125% | PASS   |

Cutover выполнен. `SyncPageManager` всегда создаёт `HybridReadEngine`;
`PageRead::Cached` хранит новый atomic guard; обычный batch больше не входит в
Clock-Pro; sequential local cursors используют exact source locator, bounded
read scope и cold-run transport. Runtime rollback не оставлен.

## 9. Проверка инструментами и ограничения checkout

`cargo +nightly fmt --all -- --check` не проходит из-за четырёх уже
изменённых вне этапа файлов: `src/datastore/backend.rs`,
`src/datastore/transaction/conflict.rs`, `src/graph/live.rs` и `src/syn/mod.rs`.
Файлы H12 в rustfmt diff отсутствуют; чужие изменения не форматировались.

Полный Clippy останавливается на 1912 существующих deny-ошибках документации
по всему crate, главным образом в `src/values`. Проверка обоих затронутых
packages с `--cap-lints warn` завершается успешно. Новые production warnings
показывают ровно известную границу: sparse IoRing API пока не вызывается
физическим cursor. Она не маскировалась `allow(dead_code)`.

В repository feature остаётся скрытый measurement API для внешнего
prototype-крейта. Он не является selector и не попадает в default production
surface. Новых runtime knobs, dependencies, VM cache, DashMap, hazard pointers
или custom allocator при cutover не добавлено.

## 10. Следующие зависимые работы

1. Этап 4 должен заменить полную materialization local graph при mutation на
   page/atom deltas.
2. После этапа 4 H14 добавляет явный Mutable delta flush: новая транзакция,
   origin/conflict check, только dirty deltas, явный commit и очистка dirty
   state лишь после успеха. `Drop` никогда не выполняет flush.
3. Следующий минимальный storage-эксперимент нужен только когда graph/property
   cursor реально сможет выдать bounded future-page set. Тогда он должен
   передать это окно существующему `ReadWindow::Sparse`, повторить paired
   production test и проверить transport wait. Новый planner, очередь или
   пользовательская настройка до такого workload не нужны.
4. Если generic singleton hit станет измеренным bottleneck вне cursor,
   следующий эксперимент должен сначала атрибутировать shared-lock и
   pin/release на реальном caller profile. Замена directory допускается только
   после этого; текущий scope path уже устраняет доминирующую работу там, где
   есть reuse.

## 11. Артефакты

- `docs/storage_engine_hybrid_integration_plan.md` — утверждённая архитектура
  и dependency order;
- `docs/reports/storage_engine_hybrid_integration_results.csv` — компактная
  сводка этого отчёта;
- `target/storage-engine-prototype/results/` — H11 paired CSV;
- `target/storage-engine-prototype/page-fault-results.csv` — process faults и
  working set;
- `target/storage-engine-prototype/profilers/vtune-h12-cutover-fixed-hot-20260904-functions.csv`
  и соседний source-lines CSV — post-cutover source-level VTune;
- `target/storage-engine-prototype/profilers/wpr-all-faults-mmap-115ram-20260902-031919-summary.txt`
  — ETW classification mmap faults;
- `crates/fg-storage-engine-prototypes` — воспроизводимые correctness и
  benchmark probes.
