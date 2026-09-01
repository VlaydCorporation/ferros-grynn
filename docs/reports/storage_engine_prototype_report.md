# Отчёт этапа 3.1: прототипы storage engine и BufferPool V2

Дата прогона: 2026-09-01.

## Итоговый статус

Этап выполнен как изолированный `#[cfg(test)]`-эксперимент внутри
`src/storage`. Production `BufferPool`, `SyncPageManager`, формат страниц,
WAL/home semantics и GQL plumbing не заменялись. mmap использовался только как
контроль.

Архитектурный выбор намеренно отделён от реализации прототипов: кандидат может
считаться пригодным к production-миграции только после correctness gate,
standalone screening и совместимых комбинаций. Если performance gate не
достигнут, текущий production path сохраняется, а остаточный bottleneck
фиксируется как основание для следующего минимального эксперимента.

## Нормативная база и модель hot path

Решения сверялись с `disk_storage_spec.md`, `gql_spec.md` и `graphs.md`.
`gql_tech_spec.md` использовался только как ненормативный ориентир будущего
physical cursor. Из спецификаций следуют разные формы чтения, которым не должна
навязываться одна универсальная admission policy.

| Hot path                 | Форма доступа                                         | Проверенные кандидаты                                  |
|--------------------------|-------------------------------------------------------|--------------------------------------------------------|
| Point/index reads        | Повторные обращения к небольшому hot set              | Atomic arena, resolved handle, VM address              |
| Graph traversal          | Skewed random hot adjacency и редкие cold transitions | Cache-all, Atomic/VM resident paths                    |
| B+tree range             | Горячие root/internal и последовательная leaf chain   | Cache-all roots, bypass/cold leaves                    |
| One-shot scan/bulk build | Последовательное чтение без reuse                     | Rocks-like bypass, scatter/IoRing, prefetch            |
| Properties/overflow      | Point reads и короткие runs                           | Cache-all, shallow prefetch                            |
| Multi-file/version       | Interleaved источники под общим лимитом               | Exact version-aware identity, общий replacement budget |

`buffer_pool_pages` во всех прототипах означает общий бюджет совокупного hot
set всех файлов и активных версий. Это не ёмкость «на последний файл» и не
отдельный budget на каждый source.

## Проверенные архитектурные аналоги

Прототипы воспроизводят не названия, а конкретные механизмы, подходящие под
контракты Ferros-Grynn:

- PostgreSQL упаковывает flags, refcount и usage count buffer descriptor в одно
  атомарное состояние и применяет clock sweep только при выборе victim. Это
  основание для `R2`, но не разрешение копировать его процессную архитектуру:
  [buffer descriptor](https://github.com/postgres/postgres/blob/master/src/include/storage/buf_internals.h),
  [buffer-manager README](https://github.com/postgres/postgres/blob/master/src/backend/storage/buffer/README);
- RocksDB делит block cache на shards, а актуальный HyperClock описывает
  lock-free lookup/release и bounded counting-CLOCK. Для bulk iterator
  `ReadOptions::fill_cache=false` не меняет eviction order существующих
  элементов. Отсюда взяты отдельно `R2` и `A1`, а не единая копия RocksDB:
  [cache API](https://github.com/facebook/rocksdb/blob/main/include/rocksdb/cache.h),
  [ReadOptions](https://github.com/facebook/rocksdb/blob/main/include/rocksdb/options.h);
- InnoDB помещает новые scan pages в old-сегмент, исторически `3/8` pool, и не
  повышает их немедленным повторным обращением. Это проверяемая гипотеза `A2`,
  а не заранее выбранная production policy:
  [scan-resistant buffer pool](https://dev.mysql.com/doc/refman/9.6/en/innodb-performance-midpoint_insertion.html);
- `ReadFileScatter` требует `OVERLAPPED | NO_BUFFERING`, system-page-aligned
  segments и допускает `ERROR_IO_PENDING`; `VirtualAlloc` отделяет reserve от
  commit, а `VirtualFree(MEM_DECOMMIT)` возвращает physical storage, сохраняя
  диапазон адресов. Эти платформенные контракты определяют `T3` и `R4`:
  [ReadFileScatter](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfilescatter),
  [VirtualAlloc](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualalloc),
  [VirtualFree](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualfree).

Ни один из этих источников не доказывает выигрыш на нашей 16-КиБ page layout:
поэтому решения ниже принимаются только по paired измерениям нашего hot path.

## Реализованные прототипы

### Resident engines

- `R0 mmap` — benchmark-control без production-семантики cache ownership;
- `R1 Clock-Pro` — неизменённый текущий production pool;
- `R2 AtomicArenaBufferPool` — стабильная contiguous arena, отдельная
  cache-line-aligned metadata, 64 шардированных `RapidHashMap` под
  `parking_lot::RwLock`, атомарные lifecycle/generation/pin/usage и miss-only
  replacement lock;
- `R3 ResolvedFrameHandle` — повторный hit без directory/hash lookup;
- `R4 VmCache` — фиксированный home-address range, commit при admission и
  decommit при eviction с общим home budget;
- `R5 VmResolvedHandle` — непрозрачный resolved VM handle с cache identity,
  source epoch и frame generation. WAL-exact версии используют отдельный
  bounded Atomic slab, а не file-sized VM range на каждый LSN.

### Admission policies

- `A0 CacheAll`;
- `A1 RocksLikeBypass`: resident hit сохраняется, cold miss читается в
  cursor-local reusable scratch и не загрязняет resident set;
- `A2 InnodbLikeCold`: не более `3/8` pool выделено cold-классу; повтор того же
  cursor epoch не продвигает страницу, point access или другой cursor может её
  повысить.

`PrototypeScanCursor` остаётся storage-only моделью. Текущий GQL scan сначала
материализует строки и ещё не управляет физическим page cursor, поэтому
production `QueryRead` не менялся.

### Windows transports и prefetch

- `T0` sync Direct в contiguous scratch;
- `T1` существующий extent-oriented IoRing;
- `T2` frame-oriented IoRing с одной SQE на 16-КиБ frame и явным requested SQE
  batch `1/8/32/128`;
- `T3` один `ReadFileScatter` request на extent с system-page-aligned disjoint
  destinations;
- bounded `std::thread + sync_channel` prefetch с глубинами `0/1/2/4/8`.

`ReadFileScatter` обрабатывает immediate completion и `ERROR_IO_PENDING`,
проверяет exact byte count и сохраняет lifetime segment array, `OVERLAPPED`,
event и frames до completion. Его фактический request QD равен единице. Число
SQE в frame-IoRing отчёте называется requested submission batch, а не
измеренной глубиной очереди SSD.

Production-зависимости для прототипов не добавлялись. Детерминированные модели
атомарного lifecycle используют test-only `shuttle`, уже присутствующий в
текущем checkout. У зависимости `windows` включены только необходимые API
features `Win32_System_Memory` и `Win32_System_Threading`.

## Correctness и safety

Прототипы используют exact `PageCacheKey`: `GraphId`, `DataFileId`,
checkpoint generation/home ceiling и WAL LSN не могут alias друг друга.
Publication допускается только после проверки текущих bytes самой frame;
переданный отдельно старый `PageHeader` не считается доказательством
валидации.

В ходе review до измерений были исправлены ошибки, которые performance-тест не
имеет права скрывать:

- lost wake между последним unpin и регистрацией capacity waiter;
- rollback `Loading` frame и signal при loader panic;
- cross-pool/cross-cache resolved handles;
- конструируемый VM handle с подменяемым сырым адресом;
- batch reservation deadlock при двух конкурирующих частично зарезервированных
  extent;
- публикация bytes после изменения уже возвращённого header;
- duplicate hot/cold residency при пересечении cold load и point promotion;
- потеря panic worker как ложного clean EOF в prefetch;
- cold metadata/publication race и физически не удаляемая promoted cold page.

Correctness tests покрывают exact home/WAL versions, разные sources/epochs,
stale handle после reuse, pin/eviction, coalesced miss, backpressure,
all-or-none batch publication, corruption rollback, panic rollback, bounded
cold admission, bypass без pollution, prefetch order/error/cancellation и
Windows VM/scatter lifetime. Unsafe ограничен arena/VM/platform wrappers;
safe caller не может сконструировать raw VM address handle.

## Окружение и повтор production baseline

- Windows 10 Home 23H2, build `10.0.22631.6199`, x86_64;
- Intel Core i5-12450H, 8 физических / 12 логических CPU;
- RAM: 16 874 201 088 байт;
- SSD: KIOXIA KBG50ZNV512G NVMe, 512 110 190 592 байт;
- Rust `1.97.1 (8bab26f4f 2026-07-14)`, Cargo `1.97.1`;
- allocator: `mimalloc-pprof`;
- Windows power scheme: Balanced;
- нормативный dataset screening: 65 536 страниц = 1 ГиБ;
- pool: 4096 страниц = 64 МиБ.

Повтор production `disk_storage_stage2_gate` на текущем host завершился
ошибкой. Resident pool дал 2,916 Mops/s против mmap 3,583 Mops/s
(343,0 против 279,1 нс/op; p99 0,8 против 0,4 мкс). Sequential batched:
raw 2479,44, validated 1836,67, pool 1515,78, mmap 982,91 MiB/s. Fixed-hot
scan: raw 1948,58, validated 1595,82, pool 1450,71, mmap 1139,65 MiB/s при
100% hits, но paired pool/raw составил только 72,854%. Тест также отклонил
Direct RSS gate: resident-путь показал delta 135 364 608 байт.

Это не переписывает и не удаляет прежний отчёт этапа 3. Новый факт означает,
что прежний закрытый gate на данном запуске не воспроизвёлся; поэтому результаты
3.1 рассматриваются как paired screening evidence и не используются для
автоматической production-миграции.

## Измерительный контракт

Mean измеряется блоком операций без `Instant` на каждом hit. p50/p95/p99 для
resident path — нормализованные batch samples; fenced TSC запускается только
при invariant TSC и CPU affinity, а raw overhead показывается отдельно и не
вычитается под видом непосредственно измеренного результата.

CSV различает:

- физический transport throughput;
- логические DB-page bytes resident workload (это не bandwidth SSD/RAM);
- end-to-end extent latency, включающую submit/wait/validation и для cache
  combinations — reserve/publish;
- process working-set delta;
- отдельно software-accounted VM reserved и committed bytes.

Первые smoke-строки до исправления QD/RSS/engine isolation сохранены как
`pre-fix` artifacts, но не участвуют в итоговых таблицах.

## Итоговый набор данных

Финальная schema-v2 campaign содержит 1089 строк со статусом `ok`. Канонический
raw-файл — `target/storage-engine-prototype/results.csv`, SHA-256
`0D90394C2BC76229076931D7B6249774AFAF9528AE403019FCDB5A2091507C47`.
В него вошли 928 неизменённых строк основной campaign и повторённые 161 строка
`combination/one_shot_sequential`: в ту же ротацию добавлены raw и validated
Direct controls, по семь повторов для каждого из 23 кандидатов. Отдельный
аудитируемый файл повторного прогона имеет SHA-256
`3BD0808E12A79B7C1DC5F849AE9DF17AA15C0846E23008CE40805376499A1025`.
Однопроцессный RSS-срез сохранён отдельно как
`resident-isolated-schema2-results.csv`. Компактный
`storage_engine_prototype_results.csv` содержит 156 агрегированных групп с
числом samples, median, min/max и paired ratio там, где внутри одной фазы есть
сопоставимый control; SHA-256 —
`F37317B294AA42F974B1841F197430A432C5589D7366652EDC9C04091C4F986C`.

`pre-fix`, `pre-final-fixes`, `pre-rss-order-review`, `pre-schema2` и
`combination-smoke` сохранены только для аудита развития harness. Они не
участвуют ни в одной таблице ниже. `full-schema2-results.csv` является копией
итогового объединённого raw-файла, а `combination-controls-schema2-*` хранит
изолированный повтор combination-фазы и его log.

Важные ограничения измерений:

- `GetProcessTimes` на этом host квантован примерно по 15,625 мс; 267 из 1089
  коротких строк имеют `cpu_time_ns=0`. Поэтому CPU используется только как
  грубый диагностический сигнал, а не как строгая ось Pareto;
- `p99=0` у concurrency, prefetch и scoped-pipeline строк означает «не
  измерялся», а не нулевую задержку;
- у frame-IoRing `T2` одна операция равна фактически отправленному batch:
  p99 для QD1, QD8 и QD32 нельзя напрямую сравнивать с двухмегабайтным extent
  QD128/T0/T1/T3;
- схема питания Balanced и 330 фоновых процессов создают DVFS/scheduler drift.
  Его влияние уменьшается ротацией кандидатов и paired repetitions, но не
  объявляется устранённым;
- отношение медиан может отличаться от медианы парных отношений. Gate ниже
  использует paired ratio, когда control находился в той же campaign-фазе.

## Resident fast path

### Standalone screening

Медианная пропускная способность, Mops/s; последняя колонка — median paired
ratio к mmap при hot set 4096 страниц.

| Candidate            | 1 page | 64 pages | 1024 pages | 4096 pages | p99 @4096 | paired mmap @4096 |
|----------------------|-------:|---------:|-----------:|-----------:|----------:|------------------:|
| R0 mmap              | 196,25 |   228,06 |      76,06 |      57,46 |     31 нс |              100% |
| R1 Clock-Pro         |  11,33 |    12,57 |      13,82 |       8,23 |    358 нс |             14,0% |
| R2 Atomic            |  12,54 |    15,81 |      10,70 |       7,01 |    317 нс |             12,1% |
| R2 Atomic + counters |  11,04 |    15,03 |      10,79 |       7,45 |    314 нс |             13,0% |
| R3 Atomic resolved   |  25,32 |    33,04 |      20,80 |      13,82 |    148 нс |             24,6% |
| R4 VM address        |  20,53 |    22,54 |      25,86 |      12,75 |    186 нс |             21,5% |
| R5 VM resolved       |  17,49 |    19,06 |      17,63 |       8,59 |    256 нс |             14,7% |

При 4096 страницах R3 занимает 72,35 нс/op против 142,73 нс у R2 и
17,40 нс у mmap. Устранение directory lookup экономит 70,38 нс относительно
R2, но остаточный разрыв R3 с mmap равен 54,95 нс по отношению медиан
(54,7 нс в paired оценке). p99 R3 хуже mmap на 117 нс: минимальный предел
`mmap + 0,2 мкс` пройден, целевой `mmap + 0,1 мкс` — нет. Главный resident
gate `>=90% mmap` не пройден с большим запасом.

Это не противоречит повтору production baseline, где Clock-Pro занимал
343,0 нс против 279,1 нс mmap. Production gate включает другой API/harness и
получил разрыв 64 нс и 0,668 Mops/s; schema-v2 screening намеренно изолирует
внутренний test-only hit. Абсолютные Mops/s между двумя harness складывать
нельзя.

Fenced TSC подтвердил RDTSCP, invariant TSC и affinity mask 1. Значения ниже
сырые и не вычитают overhead:

| Hot pages | Empty probe | R3 resolved probe |
|----------:|------------:|------------------:|
|         1 |   86 cycles |        164 cycles |
|        64 |   67 cycles |        134 cycles |
|      1024 |   67 cycles |        206 cycles |
|      4096 |   63 cycles |        522 cycles |

Рост до 522 cycles при 4096 случайно выбираемых страниц показывает стоимость
metadata/data cache и TLB locality, которую одно арифметическое вычисление
адреса не устраняет. Скорректированные cycles намеренно не публикуются как
непосредственно измеренные.

### Декомпозиция resident и miss

| Probe                                        |       Median cost |
|----------------------------------------------|------------------:|
| `RapidHashMap::get` без shard lock           |       17,95 нс/op |
| shard selection + `RwLock::read` + lookup    |       38,35 нс/op |
| atomic pin/state/usage + borrowed guard/drop |       50,16 нс/op |
| full memory-loader miss                      |    1,132 мкс/page |
| full validated Direct miss                   |   44,214 мкс/page |
| reserve 128 unique frames + signals          |  2,739 мкс/extent |
| abort reservations + complete signals        |  2,508 мкс/extent |
| publish 128 frames + complete signals        | 12,566 мкс/extent |
| coalesced signal wait/complete cycle         |   3,970 мкс/cycle |

Компоненты нельзя механически суммировать: full hit дополнительно строит key,
проверяет generation/state до и после pin, получает адрес frame и обращается к
случайной metadata/data cache line. Тем не менее R2/R3 является прямым A/B:
удаление directory resolve почти вдвое ускорило 4096-page path, то есть lookup
и его locality — доказанный, а не предполагаемый bottleneck.

Предыдущие названия `prepare_access` и `capacity_available.wait` не описывают
resident hit: этот цикл работает на miss/backpressure. На устойчивом hit
ожидания condition variable нет. Прежние «68 нс resident update» и «53 нс
PageGuard» были агрегатными разностями участков, а не стоимостью одного
присваивания структуры. Новый borrowed guard вообще не клонирует `Arc`, но
pin, generation/state recheck, usage update и atomic unpin/drop всё равно
занимают около 50 нс. Сам литерал `PageGuard` дешёв; дорог его протокол
владения.

Global/sharded counters не объясняют основной разрыв. В paired сравнении
`R2 counted / R2` получилось 0,91/0,95/0,97/1,05 для hot set
1/64/1024/4096: на малых наборах counters стоят 3–9%, а на 4096 эффект тонет
в разбросе и locality. Обязательный global hit counter в fast path всё равно
не нужен.

`DashMap<PageCacheKey, _, rapidhash::RandomState>` технически возможен, но не
решает этот single-thread bottleneck. R2 уже шардирует `RapidHashMap` под
`parking_lot::RwLock`; DashMap добавит собственные shard selection, lock и
hash/key comparison. Он имеет смысл только после профиля directory
contention, тогда как R3 доказывает, что полезно устранить повторный lookup
целиком. Следующий минимальный resident-эксперимент — source-bound dense
locator/двухуровневая page table и cursor/batch handle, который удерживает pin
на срок обработки нескольких значений, а не повторяет pin/drop для каждого
доступа.

### Concurrency

Медианы 1 и 12 threads и отношение масштабирования по медианам:

| Candidate          |     1 thread |    12 threads | scale 1→12 | scale относительно mmap |
|--------------------|-------------:|--------------:|-----------:|------------------------:|
| mmap               | 36,99 Mops/s | 167,20 Mops/s |      4,52× |                    100% |
| R1 Clock-Pro       |         8,11 |          1,80 |      0,22× |                    4,9% |
| R2 Atomic          |         7,25 |         25,84 |      3,57× |                   79,0% |
| R3 Atomic resolved |        13,73 |         56,50 |      4,11× |                   90,9% |
| R4 VM              |        12,43 |         44,59 |      3,59× |                   79,4% |
| R5 VM resolved     |         8,21 |         33,91 |      4,13× |                   91,4% |

Clock-Pro действительно сериализуется под нагрузкой. Atomic/VM paths убрали
эту форму глобального lock bottleneck. Однако строгий gate проверял все
`2/4/8/12` thread points paired-by-repetition: R3 сохранил
93,3%/75,8%/101,5%/84,7% масштабирования mmap, R5 —
81,4%/73,6%/96,8%/88,6%. Оба провалились на 4 threads, поэтому concurrency
gate целиком не пройден. Кроме того, даже R3 на 12 threads имеет только 33,8%
абсолютной mmap throughput.

### RSS, commit и смысл bounded pool

Одно-процессный срез (`n=1`) исключает взаимное влияние последовательно
созданных engines:

| Candidate            |   RSS delta | VM reserved | VM committed |
|----------------------|------------:|------------:|-------------:|
| mmap                 | 1023,99 МиБ |           — |            — |
| R1 Clock-Pro         |  129,00 МиБ |           — |            — |
| R2 Atomic            |   65,59 МиБ |           — |            — |
| R3 Atomic resolved   |   65,89 МиБ |           — |            — |
| R2 Atomic + counters |   65,58 МиБ |           — |            — |
| R4 VM                |   68,44 МиБ |    1024 МиБ |       64 МиБ |
| R5 VM resolved       |   69,05 МиБ |    1024 МиБ |       64 МиБ |

RSS — это находящиеся в физической памяти страницы working set процесса, а не
размер файла, не вся зарезервированная VA и не полный Windows file cache.
Prototype mmap во время fixture validation коснулся всего 1-ГиБ mapping,
поэтому его изолированный RSS близок к 1 ГиБ. В старом production baseline
mmap коснулся только 4096 hot pages и показал около 64 МиБ; различие задаёт
scope/touch pattern, а не «бесплатная» память mmap.

Direct I/O обходит обычный buffered file-cache path, поэтому его данные должны
оставаться в явных frames. Специальный page cache в проекте есть: это
`BufferPool`/Clock-Pro, а в прототипе — Atomic/VM arenas. 4096 × 16 КиБ =
64 МиБ; дополнительные 1,6–5,1 МиБ Atomic/VM — metadata, handles, page tables
и allocator granularity. VM reserve 1 ГиБ расходует адресное пространство, но
не RSS; commit остаётся ровно 64 МиБ. R2/R3/R4/R5 укладываются в 110% frame
budget плюс отдельно показанную metadata. R1 в текущем scope держит около
двух frame budgets и требует отдельного production accounting review.

## Sequential Direct path

### Checksum, copy и transports

| Component/transport          |                                    Median |             Paired control |
|------------------------------|------------------------------------------:|---------------------------:|
| checksum одной 16-КиБ page   |                                 907,68 нс |        17,21 ГиБ/с logical |
| checksum 128-page extent     |                               118,046 мкс |        16,94 ГиБ/с logical |
| validation готового extent   |                               121,384 мкс |        16,48 ГиБ/с logical |
| contiguous memcpy 2 МиБ      |                                94,043 мкс |        21,27 ГиБ/с logical |
| scattered-frame memcpy 2 МиБ |                               130,691 мкс |        15,30 ГиБ/с logical |
| T0 raw Direct                |                             2047,26 МиБ/с |                   100% raw |
| T0 validated Direct          |                             1681,15 МиБ/с |                  80,8% raw |
| T1 extent IoRing             |                             1671,66 МиБ/с |            99,4% validated |
| T3 ReadFileScatter           |                             1625,62 МиБ/с | 96,1% validated; 77,9% raw |
| T2 frame IoRing QD1/8/32/128 | 378,58 / 572,76 / 1099,14 / 1255,52 МиБ/с |           максимум 61% raw |

Raw Direct быстрее validated не из-за одного большого `memcpy`: checksum и
header validation выполняются последовательно между I/O batches и не
перекрываются с чтением. Pool сверху добавляет reserve, mapping publication,
signals, scattered destinations и eviction metadata. Cache не может ускорить
one-shot scan, потому что reuse отсутствует; admission только копирует и
вытесняет полезный hot set. Поэтому правильное исправление — вывести intent из
physical cursor и не кешировать cold one-shot pages.

`ReadFileScatter` устраняет contiguous-to-frame copy, но его segment setup и
Windows completion path съели выигрыш: standalone T3 на 3,3% медленнее T0
validated по отношению медиан. Frame-IoRing выиграл от batching, но даже QD128
сильно уступил extent request. Увеличение количества SQE не заменяет один
эффективный extent read.

### Admission policies

Standalone one-shot scan:

| Policy             |             Median [min–max] |    p99/page | RSS delta | Paired к A0 |
|--------------------|-----------------------------:|------------:|----------:|------------:|
| A0 CacheAll        | 358,98 [301,88–367,42] МиБ/с |  49,315 мкс | 64,69 МиБ |        100% |
| A1 RocksLikeBypass |       363,27 [330,63–368,47] |  50,557 мкс |  0,26 МиБ |        102% |
| A2 InnodbLikeCold  |       187,49 [167,83–199,34] | 101,669 мкс | 24,73 МиБ |         52% |

A1 доминирует A0 на one-shot workload: throughput эквивалентен в пределах
3%, а resident pollution отсутствует. A2 сохраняет уникальную семантику для
потенциально повторяемого range scan, но на one-shot workload доминируется.
Повтор тем же cursor epoch не повышает cold page; point access/другой epoch
может продвинуть её.

### Prefetch

| Transport           |      P0 |      P1 |      P2 |      P4 |            P8 |
|---------------------|--------:|--------:|--------:|--------:|--------------:|
| T0 validated Direct | 1816,21 | 2223,86 | 2219,09 | 2252,30 | 2241,05 МиБ/с |
| T3 reusable scatter | 1760,20 | 2217,76 | 2169,90 | 2177,25 | 2183,42 МиБ/с |

P1 даёт paired +23% для T0 и +26% для T3: один worker перекрывает I/O
следующего extent с checksum/consumption текущего. P2/P4/P8 находятся в
пределах 3% от P1, но расходуют больше памяти; у T3 P8 был отдельный провал до
1367 МиБ/с. Поэтому недоминируемый простой выбор — P1. Latency percentiles для
этой aggregate pipeline фазы не измерялись.

### Обязательные комбинации

Engine × policy, синхронный per-page transport:

| Engine |     A0 |     A1 |           A2 |   RSS A1 |
|--------|-------:|-------:|-------------:|---------:|
| Atomic | 370,85 | 379,82 | 204,46 МиБ/с | 0,26 МиБ |
| VM     | 329,05 | 377,86 | 227,72 МиБ/с | 4,02 МиБ |

Direct-to-frame extent без prefetch:

| Combination                     |                Median [min–max] | Вывод                                       |
|---------------------------------|--------------------------------:|---------------------------------------------|
| scratch + copy + Atomic publish | 1319,82 [1271,64–1427,56] МиБ/с | control                                     |
| Atomic + ReadFileScatter        |       1426,88 [1396,31–1576,37] | paired +9,5%; 68,4% raw                     |
| Atomic + frame-IoRing           |                         1193,52 | submission/completion дороже copy           |
| VM + ReadFileScatter            |                          902,38 | commit/first-touch и VM metadata доминируют |

Полная обязательная pipeline-матрица, MiB/s:

| Combination                   |      P0 |      P1 |      P2 |      P4 |      P8 |
|-------------------------------|--------:|--------:|--------:|--------:|--------:|
| A1 bypass + scatter           | 1635,41 | 2022,51 | 2015,41 | 2011,31 | 2045,56 |
| A2 Atomic cold + scatter      | 1487,90 | 1806,19 | 1798,31 | 1810,46 | 1803,45 |
| A2 Atomic cold + frame-IoRing | 1229,80 | 1471,31 | 1477,11 | 1417,35 | 1455,93 |

Для A1+scatter paired ratio к raw Direct по repetition равен
78,1%/97,3%/96,1%/96,0%/98,4%. P1 выбран вместо формально более высокой
медианы P8: разница 1,1%, а RSS составляет 8,00 МиБ вместо 35,92 МиБ.
P2 занимает 12,00 МиБ и не даёт устойчивого преимущества. В той же ротации P1
даёт 119,5% paired validated control, то есть sequential throughput gate
проходит по заранее выбранной median paired метрике. При этом paired raw range
P1 равен 88,8–98,4%: отдельные повторы не гарантируют 90%-й floor. P8 требует
35,92 МиБ, но даёт median 98,4% и range 90,9–99,1% raw, поэтому остаётся
недоминируемым вариантом, если production gate потребует прохождения каждого
повтора. Это не разрешение менять production path: p99 pipeline не измерен, а
production fixed-hot gate отдельно не закрыт.

Atomic scatter напрямую в frames действительно устранил измеримую часть copy
и publish overhead (+9,5% к scratch-copy path), но остался на 68,4% raw.
Следовательно direct-to-frame сам по себе недостаточен; основной выигрыш дал
pipeline overlap вместе с bypass.

## Workloads из спецификаций

| Workload                       | Variant |    Throughput |       p99 | Hit ratio |       RSS |
|--------------------------------|---------|--------------:|----------:|----------:|----------:|
| B+tree hot roots + leaf scan   | A0      | 1464,32 МиБ/с | 47,99 мкс |      0,75 | 64,94 МиБ |
| B+tree hot roots + leaf scan   | A1      |       1486,22 | 46,17 мкс |      0,75 |  0,45 МиБ |
| B+tree hot roots + leaf scan   | A2      |        807,62 | 85,71 мкс |      0,75 |  24,8 МиБ |
| Graph frontier hot+sparse cold | A0      | 179 910 ops/s |  7,53 мкс |  0,999826 |  64,8 МиБ |
| Properties/overflow short runs | A0      | 184 731 ops/s |  7,45 мкс |       1,0 |  64,6 МиБ |

A1 в B+tree workload даёт paired 101,5% A0 и сохраняет root hits, не допуская
leaf-chain pollution. Это подтверждает аналитический split: cache-all для
верхних уровней, bypass для одноразовой leaf chain. A2 проиграл paired до
54,9%; его promotion heuristic нужно проверять только на доказанном повторном
range workload.

### Многфайловый общий hot set

| Pool / aggregate hot set |                      1 file |       4 files |       8 files | Median hit ratio |       p99 range |
|--------------------------|----------------------------:|--------------:|--------------:|-----------------:|----------------:|
| 0,50×                    |                      362,98 |        362,18 |  354,40 МиБ/с |         0,499298 | 47,77–53,41 мкс |
| 0,75×                    |                      723,39 |        716,79 |        717,93 |         0,750420 | 25,86–26,11 мкс |
| 1,00×                    | 103266–136905 logical МиБ/с |             — |             — |              1,0 |     0,2–0,3 мкс |
| 1,25× и 1,50×            |               resident hits | resident hits | resident hits |              1,0 |     0,2–0,3 мкс |

Количество файлов не меняет hit ratio при одинаковом совокупном hot set. Pool
не «помнит последний файл»: key содержит source/version, а replacement budget
общий. Ловушка возникает только если сумма одновременно горячих страниц всех
файлов превышает capacity — тогда ожидаемый hit ratio в равномерном тесте
приближается к `pool_pages / aggregate_hot_pages`.

Практическая калибровка: оценить по physical cursor горячие root/index,
adjacency/property и active WAL/home pages каждого одновременно используемого
source, сложить их, затем добавить headroom на skew и in-flight reservations.
Для 100% resident workload минимум равен aggregate hot set; если бюджет
ограничен, leaf/bulk scan следует отправить через A1, а не раздувать pool.

## Windows VM стоимость

| Операция на 16-КиБ VM page |    Median |
|----------------------------|----------:|
| reserve                    | 0,500 мкс |
| commit                     | 0,848 мкс |
| first touch                | 6,639 мкс |
| decommit                   | 2,330 мкс |
| recommit                   | 0,841 мкс |

VM home address даёт O(1) вычисление адреса, но commit/first-touch/decommit
нельзя помещать в resident hit. R4 поэтому выполняет их только на
admission/eviction. На 1024 pages R4 был быстрее R3 (38,67 против 48,07
нс/op), но на 4096 pages уже медленнее (78,44 против 72,35): TLB/cache locality
перекрывает преимущество адресной арифметики. R5 оказался хуже R4, потому что
epoch/generation/cache-identity validation handle не исчезает при вычислимом
адресе. Отдельный hardware TLB counter получить не удалось; hot-set sweep и
TSC являются только косвенным evidence.

## Screening, dominance и gates

| Gate                                            | Результат                                                                            | Статус                          |
|-------------------------------------------------|--------------------------------------------------------------------------------------|---------------------------------|
| Exact bytes/checksum/home-WAL/error semantics   | Correctness suite прошёл                                                             | PASS                            |
| No partial extent publication                   | corruption/EOF/short-read rollback tests прошли                                      | PASS                            |
| Bypass не увеличивает resident count            | A1 RSS 0,26 МиБ, resident pollution отсутствует                                      | PASS                            |
| VM/Atomic memory `<=110%` pool + metadata       | R2–R5: 65,58–69,05 МиБ при pool 64 МиБ                                               | PASS                            |
| R1 memory budget                                | 129,00 МиБ                                                                           | FAIL                            |
| Generic Atomic hit `<=70 нс`                    | R2 142,73 нс; lookup+guard probes уже 88,51 нс                                       | FAIL                            |
| Resolved/VM hit `<=35 нс`                       | лучший R3 72,35 нс                                                                   | FAIL                            |
| Resident `>=90% mmap`                           | лучший R3 24,6% paired                                                               | FAIL                            |
| Resident p99                                    | R3 `mmap +116–117 нс`                                                                | target FAIL, floor PASS         |
| Concurrency `>=80% mmap scaling` на всех точках | R3 75,8% на 4 threads                                                                | FAIL                            |
| Sequential `>=90% raw`, `>=95% validated`       | P1 median: 97,3% raw, 119,5% validated; P1 raw range 88,8–98,4%; P8 range 90,9–99,1% | median P1 PASS; per-run P8 PASS |
| Fixed-hot `>=90% raw`, hits `>=99%`             | отдельный finalist gate не запускался                                                | SKIP                            |
| Direct-to-frame устраняет измеримую работу      | Atomic scatter +9,5% к scratch/copy/publish                                          | PASS, но 68,4% raw              |

Pareto frontier получился разделённым:

- `R3` — лучший общий resident-исследовательский кандидат: тот же 64-МиБ
  budget, наибольшая 4096-page и concurrent throughput, но слишком далеко от
  mmap для production;
- `R4` даёт уникальную VM-address гипотезу и локально выигрывает на 1024
  pages, но при 4096 pages уступает R3 и не проходит gate;
- `R5` доминируется R4 по throughput, p99 и RSS; дополнительный VM handle не
  окупил validation;
- `R1` остаётся production control, но его contention и RSS хуже
  test-only arenas. Это ещё не доказывает эквивалентность replacement/error
  surface R3, поэтому автоматическая замена запрещена;
- `A1` доминирует A0/A2 на one-shot и B+tree leaf scan;
- standalone T0 validated проще и быстрее T1/T3 в пределах screening, но T3
  становится полезен как destination transport для A1 pipeline;
- frame-IoRing T2 доминируется extent/scatter transport на всех QD;
- P1 выбирается по median-правилу «разница до 3% — более простой и менее
  прожорливый вариант»; P8 остаётся на Pareto frontier по устойчивости каждого
  повтора ценой дополнительных 27,92 МиБ.

Ни один resident candidate не пережил минимальный 90%-й floor, поэтому
условный finalist sweep по размерам
`1/10/50/130/300/1000/10000/65536/100000/300000` страниц не запускался.
Отдельные finalist workloads fixed-hot/raw, 4× pool и concurrent home/WAL
performance также помечены `SKIP`; их version/correctness случаи проверены
тестами. Запуск дорогого sweep для заведомо отклонённых кандидатов не добавил
бы основания для production-решения.

## Архитектурный вывод

Единого «нового BufferPool», одинаково хорошего для point reads и scan, не
получилось. Измерения подтверждают разделение path по intent:

```text
Point / index root / graph hot page
  -> resident cache -> source-bound resolved/batch handle

One-shot leaf/bulk scan
  -> A1 bypass -> extent Direct/ReadFileScatter -> P1/P8 overlap
  -> checksum -> ordered delivery, без admission

Potentially repeated range
  -> сначала A1 или A0 по доказанному reuse;
     A2 только после отдельного workload evidence
```

Для sequential path рабочая архитектурная гипотеза найдена:
`ReadIntent + A1 + bounded prefetch`; P1 — минимальный median-кандидат, P8 —
кандидат для строгого per-run floor. Windows T3 полезен в этой комбинации, но
не сам по себе. Для resident path переписывание production pool на текущий
R2/R4 не оправдано. Остаточный доказанный bottleneck — обязательный per-access
ownership protocol и metadata locality после устранения hash lookup.

Следующий минимальный resident-эксперимент:

1. Ввести test-only source-bound dense locator: page index адресует компактный
   locator array/двухуровневую page table; exact source epoch и frame
   generation остаются в locator.
2. Разрешить physical cursor получить `ResolvedRun`/batch pin для нескольких
   операций и выполнить один pin/generation check на batch, а не на каждое
   поле/ребро.
3. Измерить отдельно resolve once, batch length 1/4/16/64, stale fallback и
   eviction pressure. Gate остаётся тем же `>=90% mmap`.
4. Только если directory contention останется доказанным после dense locator,
   сравнить текущие shards с DashMap на `rapidhash::RandomState`. До этого
   замена hash container не является минимальным экспериментом.

## Dependency-ordered план production-интеграции

1. **Стабилизировать benchmark contract.** Сохранить raw/validated controls в
	той же финальной pipeline rotation, увеличить CPU measurement blocks,
	собирать ненулевой pipeline p99 и повторно подтвердить production baseline.
2. **Провести internal physical intent.** Добавить crate-private `ReadIntent`
   к storage cursor без изменения публичных `QueryRead`/`TransactionRead`;
   спецификационные point/index/traversal/leaf/bulk формы должны иметь явное
   отображение.
3. **Интегрировать A1 первым.** Реализовать bypass поверх текущего sync Direct
   T0, сохранив resident lookup перед I/O, exact validation, ordering и
   error semantics. Проверить отсутствие pollution на production pool.
4. **Добавить bounded prefetch.** Начать с P1 и одного scoped worker; P8
	выбирать только при явном требовании per-run floor или повторённой
	нестабильности P1. Windows T3 включать внутренним transport только если он
	снова выигрывает paired throughput, p99 и CPU у T0 на том же control.
5. **Прототипировать dense/batch resident ownership.** Не менять production
   Clock-Pro до прохождения correctness и resident gates новым locator/handle.
6. **Выбрать resident adapter.** Если dense/batch вариант проходит gates,
   перенести минимальный Atomic arena slice за существующую page-manager
   границу; если нет — оставить Clock-Pro и зафиксировать следующий измеренный
   bottleneck.
7. **Проверять A2 только при reuse evidence.** Добавлять cold admission после
   workload, где один и тот же range действительно перечитывается разными
   cursor epochs и A1 проигрывает.
8. **Закрыть recovery/rollout gate.** Corruption, cancellation, crash/reopen,
   concurrent home/WAL, multi-file budget, RSS/p99 и rollback проходят на
   production path до переключения default. Старый path удаляется только
   после сравнения формата и поведения ошибок.

Новые runtime dependencies для этого порядка не требуются. Пользователь
разрешил новые и закомментированные зависимости, а также подготовленный код из
`src/asynchronous`/`src/collections`, но std `thread + sync_channel`, текущие
`rapidhash`, `parking_lot`, Windows bindings и test-only `shuttle` полностью
покрыли эксперимент. Подключать дополнительные crates без измеримого пробела
не было причины.

## Проверка реализации

- `cargo check --locked --lib --no-default-features` — PASS;
- `cargo check --locked --lib --features native-async-io` — PASS;
- полный lib test без default features — 381 PASS, 12 ignored;
- полный lib test с `native-async-io` — 395 PASS, 15 ignored;
- оба детерминированных Shuttle-сценария, 14 policy tests и два Windows EOF
  rollback test — PASS;
- контракт ротации 23 combination candidates — PASS;
- release `storage_engine_prototype_combination_probe`: 1 PASS, 161/161 raw
  rows `ok`, 117,54 с без времени сборки;
- `cargo +nightly fmt --all -- --check` — PASS;
- `cargo clippy -- -D warnings` ограничен существующим workspace debt: сначала
  три `dead_code` в proc-macro crate, после их диагностического разрешения —
  348 других существующих errors и 513 warnings; фильтр по `src/storage/prototypes`
  дал ноль ошибок изменённой поверхности;
- `graphify update .` через записанный в `.graphify_python` interpreter — PASS:
  47 670 nodes, 122 375 edges, 1669 communities. Штатный launcher сначала
  вернул `Failed to canonicalize script path`; повтор через управляемый Python
  успешно обновил `graph.json`, `graph.html` и `GRAPH_REPORT.md`.

## Профилирование

Первичная попытка профилирования R4 была диагностической, но не давала
source-level evidence:

- **Tracy — SKIP.** В test-only harness нет Tracy client initialization и
  zones; запуск GUI capture без инструментации не атрибутировал бы hot path.
- **VTune hardware hotspots — первичная попытка недействительна.** Запуск из
  обычного sandbox-процесса не имел доступа к sampling driver. Сообщение VTune
  допускало как отсутствие драйвера, так и отсутствие administrator rights;
  последующий аппаратный capture с повышенными правами прошёл успешно, поэтому
  приписывать сбой драйверу было ошибкой.
- **VTune user-mode hotspots — PARTIAL.** Collection завершилась за 15,510 с,
  CPU time 11,436 с. Обычный release test executable не имел пригодной пары
  debug symbols; первые адреса дали 61,9%, 12,6% и 9,7% CPU, но приписывать их
  функциям нельзя.
- **WPR/WPA — SKIP.** `wpr -start CPU -filemode` завершился до benchmark с
  `0xc5585011: Failed to enable the policy to profile system performance`; ETL
  не создан.

Первичная VTune command line, test binary и arguments сохранены в
`target/storage-engine-prototype/profilers/vtune-r4-vm-sw/config/runss.options`,
tool logs — в соседнем `log/`, software CSV — в `vtune-r4-vm-sw.csv`.
Сводка первичных попыток и ошибок находится в `profiler-status.txt`; эти данные
не используются для source-level вывода.

### Контрольный VTune Hotspots с профилем `profiling`

1 сентября 2026 года R3 повторно профилировался с профилем из
`.cargo/config.toml`, который наследует release-оптимизации, но задаёт
`debug = 2` и `strip = "none"`. Точная команда сборки:

```powershell
cargo test `
  --locked `
  --profile profiling `
  --lib `
  --features native-async-io `
  storage_engine_prototype_resident_probe `
  --no-run
```

Сборка завершилась за 78 мин 31 с и напечатала точный executable
`target/profiling/deps/ferros_grynn-26bd229c59dc9337.exe`. Рядом создан PDB с
тем же stem:

| Артефакт                            |        Размер | Время создания |
|-------------------------------------|--------------:|---------------:|
| `ferros_grynn-26bd229c59dc9337.exe` |  66 238 976 B |       14:24:06 |
| `ferros_grynn-26bd229c59dc9337.pdb` | 519 933 952 B |       14:24:06 |

Smoke-run этого EXE с единственным кандидатом `R3_atomic_resolved` прошёл.
Затем VTune 2026.4 выполнил аппаратный Hotspots capture с event-based sampling
driver: `50 000 000 × 3` resident операций, 15,134 с wall и 13,766 с CPU.
Запуск из обычного sandbox снова получил отказ в доступе к hardware sampling;
та же команда с administrator rights завершилась успешно. Это подтверждает,
что драйвер установлен и работает, а исходная проблема состояла в профиле
сборки и контексте запуска.

VTune разрешил полные Rust symbol и source file для test executable:

| Hotspot                                             | CPU time | Доля CPU |
|-----------------------------------------------------|---------:|---------:|
| `harness::measure_operations<...resident_probe...>` |  8,211 с |    59,6% |
| `AtomicArenaBufferPool::release_pin`                |  1,643 с |    11,9% |
| `AtomicArenaBufferPool::try_pin`                    |  1,453 с |    10,6% |
| `AtomicArenaBufferPool::get_resolved`               |  0,468 с |     3,4% |
| `drop_glue<AtomicPageGuard>`                        |  0,186 с |     1,4% |

Предупреждения о недостающих symbols относились к системным DLL/SYS и не
затронули `ferros_grynn-26bd229c59dc9337.exe`. Collection-процесс вернул код
`1` после этих предупреждений, несмотря на завершённую базу и отчёт; отдельный
экспорт Hotspots CSV завершился с кодом `0`. Поэтому результат классифицирован
как **PASS** для Ferros-Grynn symbols и **WARN** для необязательных symbols
системных модулей. Полная база VTune находится в
`target/storage-engine-prototype/profilers/vtune-r3-resolved-profiling-20260901-142639-admin/`,
экспорт функций — в соседнем файле
`vtune-r3-resolved-profiling-20260901-142639-admin-hotspots.csv`.

## Индекс артефактов

- `docs/storage_engine_prototype_plan.md` — утверждённый порядок;
- `docs/reports/storage_engine_prototype_results.csv` — 156 compact groups;
- `target/storage-engine-prototype/results.csv` — 1089 raw schema-v2 rows;
- `target/storage-engine-prototype/run.log` — log основной campaign;
- `target/storage-engine-prototype/combination-controls-schema2-results.csv` и
  `combination-controls-schema2-run.log` — повтор combination-фазы с controls;
- `target/storage-engine-prototype/resident-isolated-schema2-results.csv` —
  изолированный RSS;
- `target/storage-engine-prototype/production-baseline-summary.txt` — повтор
  неизменённого production gate;
- `target/storage-engine-prototype/profilers/` — VTune/WPR/Tracy evidence;
- остальные `pre-*`/`smoke-*` — исключённая диагностическая история.
