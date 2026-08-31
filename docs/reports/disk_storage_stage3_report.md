# Отчёт этапа 3: закрытие Windows sync performance gate

Дата нормативного прогона: 2026-08-31.

## Итог

Windows sync gate пройден на нормативной конфигурации: файл 1 ГиБ, страница
16 КиБ, `BufferPool` 4096 страниц (64 МиБ), три repetition. Sparse miss дал
92,523% raw Direct по парному throughput и 115,592% его p99; scan resistance —
99,018% raw batched Direct при 100% попаданий в проверяемый hot set. Resident
hit уложился в допустимые `+0,5 мкс` относительно mmap, Direct batch и durable
write обогнали mmap, а RSS Direct-пути остался в bounded budget.

Изменять Clock-Pro или production hit path не потребовалось. Главный
воспроизведённый источник прежних 77,5–88,2% оказался методическим: backend'ы
измерялись на разных физических файлах, а gate делил независимые медианы.
На Windows/SSD вариация размещения файла, состояния устройства и host load
оказывалась сопоставима с искомой разницей. Benchmark теперь создаёт один
свежий immutable-файл на repetition, читает его всеми backend'ами с ротацией
порядка и считает median отношений внутри одного repetition.

В production-код кэша внесён только regression-test, подтверждающий, что ключи
двух файлов не конфликтуют и одновременно занимают общий pool. Остальные
изменения относятся к benchmark-only диагностике и корректности измерения.

## Нормативная база и границы

Работа сверялась с `docs/disk_storage_spec.md`, прежде всего с единым
16-КиБ page contract, Direct I/O, BufferPool и runtime-параметром
`FG_PAGE_BUFFER_PAGES`. Спецификация задаёт default 4096 страниц = 64 МиБ, но
не объявляет его универсально оптимальным для любого workload. В коде размер
доступен как `LocalOptions::buffer_pool_pages`, проверяется при открытии local
backend и передаётся в общий `SyncPageManager` базы.

Этап ограничен Windows GA-core. Linux/NVMe parity этим отчётом не заявляется.
NativeAsync проверен только на отсутствие регрессии и сохранение ожидаемого
выигрыша при высокой глубине очереди; синхронный путь остаётся эталонным.

## Исправление методики benchmark

Прежняя методика создавала отдельный файл для каждого сочетания
`backend × workload × repetition`. Это давало два независимых источника
вариации:

- backend'ы сравнивали разные размещения данных и разные состояния SSD;
- `median(pool) / median(raw)` ошибочно принимался за типичный относительный
  результат, хотя он в общем случае не равен `median(pool_i / raw_i)`.

Исправленная методика:

1. Для каждого repetition через Direct writer создаётся один свежий файл.
2. Writer закрывается до измерения; файл после этого immutable.
3. Raw Direct, validated Direct, Direct+BufferPool, buffered I/O и mmap читают
   один и тот же файл.
4. Порядок backend'ов ротируется, чтобы не закреплять эффект прогрева, SSD GC,
   температуры или DVFS за одним вариантом.
5. Gate raw/production использует медиану отношений одной и той же пары
   repetition. Отдельные медианы остаются в отчёте как описательные числа.

На изолированном sparse probe независимые медианы могли одновременно показать
113,31% pool/raw, а корректная парная агрегация — 97,29%. Это не изменение
алгоритма за один запуск, а демонстрация того, почему старый способ отношения
медиан был нестабилен. Unit-test фиксирует парную агрегацию.

## Окружение и команды

- ОС: Windows 10.0.22631, `x86_64-pc-windows-msvc`.
- CPU: Intel Core i5-12450H, 8 физических / 12 логических CPU.
- RAM: 16 874 201 088 байт, примерно 15,71 ГиБ.
- SSD: KIOXIA KBG50ZNV512G, NVMe, 512 110 190 592 байт.
- Rust: `rustc 1.97.1 (8bab26f4f 2026-07-14)`, Cargo 1.97.1.
- Dataset: 65 536 страниц × 16 КиБ = 1 ГиБ.
- BufferPool: 4096 страниц × 16 КиБ = 64 МиБ.
- Repetition: 3, deterministic seed `0x464753544f524147`.

Нормативный sync gate:

```text
cargo test --profile bench --lib --no-default-features storage::benchmarks::disk_storage_stage2_gate -- --ignored --nocapture --test-threads=1
```

Название теста сохраняет `stage2`, поскольку это существующий нативный
контракт производительности, усиленный в этапе 3. Результат:
`stage2_gate=passed`.

Проверка NativeAsync:

```text
cargo test --release --lib --no-default-features --features native-async-io storage::page_io::tests::windows_ioring_qd_gate -- --ignored --exact --nocapture
```

Диагностические пробы:

```text
cargo test --release --lib --no-default-features storage::benchmarks::disk_storage_stage3_sparse_probe -- --ignored --exact --nocapture
cargo test --release --lib --no-default-features storage::benchmarks::disk_storage_stage3_resident_probe -- --ignored --exact --nocapture
cargo test --release --lib --no-default-features storage::benchmarks::disk_storage_stage3_cache_index_probe -- --ignored --exact --nocapture
```

## Результат Windows gate

Описательные throughput ниже — медианы backend'ов. Для raw Direct gates в
последнем столбце приведены именно парные отношения, а не отношение двух
медиан.

| Workload                     |    raw Direct | validated Direct | Direct+BufferPool |          mmap | Парный pool/raw |
|------------------------------|--------------:|-----------------:|------------------:|--------------:|----------------:|
| Sparse random QD1            |  163,25 MiB/s |     153,09 MiB/s |      151,04 MiB/s |  115,12 MiB/s |         92,523% |
| Random full coverage         |  145,04 MiB/s |     129,09 MiB/s |      141,02 MiB/s |  220,41 MiB/s |        100,299% |
| Sequential scalar QD1        |  382,56 MiB/s |     370,46 MiB/s |      347,63 MiB/s |  970,07 MiB/s |         90,870% |
| Sequential batched           | 2384,37 MiB/s |    1846,44 MiB/s |     1522,34 MiB/s |  957,99 MiB/s |        69,345%¹ |
| Working set 4× pool          |  143,23 MiB/s |     152,35 MiB/s |      158,44 MiB/s |  277,30 MiB/s |        112,281% |
| Scan с фиксированным hot set | 1916,92 MiB/s |    1540,50 MiB/s |     1477,87 MiB/s | 1168,31 MiB/s |         99,018% |

¹ Batched gate намеренно сравнивает production Direct+BufferPool с mmap, а
не с raw transport: production дополнительно валидирует, копирует страницы в
постоянные frames и выполняет admission. Его результат относительно mmap —
158,91%, порог — не менее 90%.

| Gate                                 |                       Результат |              Порог | Статус  |
|--------------------------------------|--------------------------------:|-------------------:|:-------:|
| Sparse throughput / raw Direct       |                         92,523% |              ≥ 90% | пройден |
| Sparse p99 / raw Direct              |                        115,592% |             ≤ 125% | пройден |
| Resident-hit p99                     |     0,8 мкс против mmap 0,4 мкс | ≤ `mmap + 0,5 мкс` | пройден |
| Sequential batch / mmap              |                         158,91% |              ≥ 90% | пройден |
| Scan resistance / raw batched Direct |                         99,018% |              ≥ 90% | пройден |
| Scan hot-set hit ratio               |                            100% |              ≥ 99% | пройден |
| Durable Direct write / mmap          | 515,61 / 289,29 MiB/s = 178,23% |              ≥ 90% | пройден |
| Максимальный Direct RSS delta        |         примерно 67–72 млн байт |   ≤ 110% от 64 МиБ | пройден |
| Batch calibration                    |    выбрано и настроено 2048 КиБ |    должны совпасть | пройден |

Buffered durable write в том же прогоне дал 368,74 MiB/s. Это подтверждает,
что production Direct сохраняет выигрыш не только относительно mmap, но и
относительно обычного buffered write на данном host.

## Что сравнивают три Direct-варианта

`raw Direct` — нижняя граница transport cost: aligned `NO_BUFFERING` read без
проверки page header/checksum и без кэша. Это диагностический comparator, а не
безопасный production read.

`validated Direct` — тот же transport и тот же файл, но с production-проверкой
checksum и page header; BufferPool отсутствует. Путь добавлен только в
benchmark, чтобы отделить стоимость формата страницы от cache metadata.

`Direct+BufferPool` — production-семантика: validated read на miss, bounded
frames, immutable-version key, coalescing, pinning, backpressure и Clock-Pro.

На sparse QD1 переход raw → validated стоил по описательным медианам около
6,2%, а validated → pool — ещё около 1,3%. Следовательно, прежнее большое
отставание нельзя было целиком приписать ни checksum, ни mutex/hash/Clock-Pro.
На scalar path дополнительная стоимость pool относительно validated составила
около 6,2%. На batch path цена validation, копирования и admission заметнее,
но production всё равно быстрее mmap в 1,59 раза.

Некоторые независимые медианы, например working set, показывают validated
быстрее raw, хотя validated выполняет больше работы. Это ещё одно проявление
host/SSD variance, а не «ускорение от checksum». Причинные выводы raw/pool и
сам gate поэтому опираются на пары одного repetition и профиль, а не на
отношение произвольно выбранных строк таблицы.

## Сравнение с mmap

Утверждение «Direct уже догнал mmap» без указания workload было бы неверным.
Результат неоднороден:

- Direct+BufferPool быстрее mmap на sparse first-touch, sequential batch,
  scan-resistance и durable write;
- на fully resident scalar/random доступе mmap часто быстрее, потому что после
  page fault получает почти прямую адресацию через page tables и не выполняет
  `PageCacheKey` lookup, pinning и явное обновление cache policy;
- на полном random/working-set и scalar sequential mmap на этом host также
  быстрее, но использует другую модель памяти — file-backed working set
  управляется ОС и не ограничен 64 МиБ process-level budget.

Поэтому цель этапа достигнута в корректной формулировке: bounded Direct-путь
прошёл нормативные gates, выигрывает важные storage workload'ы и минимизирует
отставание там, где сравнение семантически допустимо. mmap остаётся полезным
benchmark comparator, но не заменой production architecture.

## RSS, системный кэш и PageCache

RSS (`resident set size`) — объём виртуальных страниц процесса, которые в
момент измерения находятся в физической памяти. Размер отображения или файла
сам по себе ещё не равен RSS: mmap поднимает RSS по мере page faults и касаний.
После полного касания 1-ГиБ файла mmap может удерживать примерно 1 ГиБ
file-backed страниц; затем ОС имеет право вытеснить их под давлением памяти.

У Direct-пути специальный PageCache есть: это `BufferPool`. На Windows
`FILE_FLAG_NO_BUFFERING` обходит обычный file-data cache для этих чтений, а
pool заранее выделяет ровно `buffer_pool_pages` выровненных 16-КиБ frames.
При default это 64 МиБ payload. Наблюдаемые примерно 67–72 млн байт RSS delta
состоят из этих frames плюс `FrameState`, 48-байтные ключи, hash buckets,
`Arc`, ghost history и allocator rounding.

Resident benchmark касается ровно 4096 страниц и в обоих вариантах прогревает
64 МиБ: поэтому в нормативном прогоне RSS pool и mmap был почти одинаковым —
68 325 376 и 68 198 400 байт. Разница становится большой на workload'ах,
которые проходят весь 1-ГиБ dataset: pool остаётся bounded, mmap working set
может вырасти примерно до размера всего файла. Это не означает, что mmap
«плох»: ОС может использовать свободную память эффективнее. Но это не тот же
memory contract, что жёсткий application-level budget.

## Разбор resident hit

В нормативном прогоне:

- Direct+BufferPool: 2 673 215 ops/s, 374,1 нс/op, p99 0,8 мкс;
- mmap: 3 722 029 ops/s, 268,7 нс/op, p99 0,4 мкс.

Среднее отставание здесь около 105 нс/op. `+0,4 мкс` относится к p99, а не к
средней операции; смешивать эти две величины нельзя. В более ранних запусках
разница throughput была меньше, что подтверждает чувствительность короткого
resident probe к CPU frequency и состоянию host.

Tracy-профиль 17 408 resident calls дал ориентиры для вложенных участков:

- захват и удержание metadata state — около 47 нс;
- lookup реального 48-байтного `PageCacheKey` — около 23 нс;
- resident update (`referenced`, счётчик hit, `Arc::clone`) — около 68 нс;
- построение `PageGuard` — около 53 нс.

Значения вложены и частично перекрываются, поэтому их нельзя просто сложить.
Полный instrumented `prepare_access` вырос примерно до 380 нс: это показывает,
что Tracy заметно возмущает такой короткий путь, и его instrumented ops/s не
использовался как итоговый benchmark.

Mutex не удерживается во время I/O. На hit он защищает lookup, reference bit,
обычный счётчик и clone resident `Arc`; затем освобождается до создания/жизни
guard. На QD1 профиль не обнаружил «слишком долгого» удержания. Это не доказывает
отсутствие contention при множестве reader threads: прежде чем шардировать
pool, нужен отдельный concurrent resident benchmark с измерением wait time.

Счётчик hit находится под тем же уже взятым mutex и не является atomic. Его
отдельная доминирующая стоимость не обнаружена. Удалять observability ради
предположения не следует.

## Hash lookup, плотная память и VecMap

Фраза «hash map обязательно даёт CPU cache miss на каждом чтении» слишком
сильна. Hash lookup действительно добавляет hashing и менее локальный доступ к
metadata, но фактический cache miss зависит от размера таблицы и состояния CPU
cache. Для проверки добавлен ignored release-probe с настоящим
`PageCacheKey` размером 48 байт.

Сравнивались:

- текущий `RapidHashMap<PageCacheKey, frame>` под тем же `parking_lot::Mutex`;
- отсортированный `Vec` с алгоритмом reference `VecMap`: linear search до
  64 entries, binary search выше; этот алгоритм взят из
  `crates/surrealdb/surrealdb/collections/src/vec_map.rs` только как гипотеза;
- плотный одноисточниковый `Vec`, индексируемый `PageNo`, как оптимистичная
  верхняя граница, а не готовая замена.

Последний release-прогон:

| Entries / files | RapidHashMap | ordered Vec | dense one-source |
|-----------------|-------------:|------------:|-----------------:|
| 64 / 1          |     21,48 нс |    53,98 нс |         20,27 нс |
| 64 / 8          |     21,56 нс |    37,19 нс |                — |
| 1024 / 1        |     23,74 нс |    90,17 нс |         20,53 нс |
| 1024 / 8        |     21,18 нс |    85,74 нс |                — |
| 4096 / 1        |     21,44 нс |   128,04 нс |         22,58 нс |
| 4096 / 8        |     23,40 нс |   117,80 нс |                — |

При replacement churn на 4096 entries RapidHashMap дал 50,63 нс/op, ordered
Vec — 5472,22 нс/op. При lookup + случайном касании 64-МиБ page payload +
`Arc` pin: 67,95 нс, 212,35 нс и 58,34 нс соответственно. В независимом
предыдущем прогоне dense выиграл у hash около 22 нс; в последнем — около
9,6 нс. Такой разброс и небольшой абсолютный потолок не объясняют весь
resident gap.

Вывод по `VecMap`: он подходит небольшим read-mostly коллекциям, но не
4096-entry replacement cache. Binary search выполняет несколько зависимых
сравнений 48-байтного ключа, а вставка/удаление двигает contiguous tail. В
измерении lookup он примерно в 5–6 раз медленнее hash, churn — более чем в
100 раз. Копировать или подключать reference-реализацию не следует.

Плотный индекс потенциально интереснее, но текущий одноисточниковый probe не
выражает production contract:

- `PageNo` разрежены и относятся к разным `DataFileId` и `GraphId`;
- одновременно могут жить несколько immutable-версий одного `PageKey`
  (`Wal(PageLsn)` и checkpoint `Home { generation, ceiling }`);
- двухуровневый dense directory сам добавит indirection, memory overhead и
  сложность reclamation.

Production dense/sharded index оправдан только после end-to-end прототипа,
который выигрывает достаточно, чтобы окупить эту сложность. Этап 3 такого
доказательства не дал.

## Почему default равен 4096 и что происходит с несколькими файлами

4096 — заданный спецификацией и `LocalOptions::default` bounded default:
`4096 × 16 КиБ = 64 МиБ`. Это не размер «текущего файла» и не hard-coded
предположение, что база читает один файл. Один `SyncPageManager` local-базы
владеет одним `BufferPool`; все зарегистрированные data/index/catalog files
используют его совместно.

`PageCacheKey` содержит `GraphId`, `DataFileId`, `PageNo` и immutable version.
Одинаковые page number/version из двух файлов не alias'ятся. Regression-test с
capacity 2 загружает такие страницы из двух `DataFileId`, затем получает два
resident hit и разные payload/frame addresses.

Следовательно, ловушка сформулирована так:

- неверно: pool помнит только последний прочитанный файл;
- верно: страницы всех файлов конкурируют за одну глобальную capacity и одну
  Clock-Pro policy; per-file quota/fairness сейчас нет.

Если совокупный одновременно активный hot set превышает capacity, файлы могут
вытеснять страницы друг друга. Clock-Pro защищает повторно используемые
страницы от scan pollution, но не гарантирует долю pool отдельному файлу.

## Калибровка `buffer_pool_pages` для многфайлового hot set

Размер нужно выбирать по одновременно активным версиям страниц всех файлов,
а не по числу файлов или размеру крупнейшего файла. Начальная оценка:

```text
base_pages = ceil(
    (sum(hot_bytes_per_active_file)
     + snapshot_or_wal_version_overlap_bytes
     + batch_and_scan_margin_bytes)
    / 16384
)
candidate_pages = ceil(base_pages * safety_factor)
```

`safety_factor` 1,15–1,30 — только старт sweep, не новый норматив. Итоговый
кандидат обязан помещаться в общий memory budget процесса с учётом metadata
pool, других кэшей, query memory, WAL/checkpoint и allocator overhead.

Практическая процедура:

1. Снять representative trace именно одновременного interleaved доступа к
   graph/index/catalog files и активным snapshot/WAL-версиям.
2. Посчитать p95/p99 числа уникальных горячих page versions в общем окне reuse.
3. Проверить как минимум `0,5×`, `0,75×`, `1,0×`, `1,25×` и `1,5×` от этой
   оценки, не превышая memory budget.
4. Для каждого кандидата измерить throughput, p95/p99, общий и пофайловый hit
   ratio, evictions/misses после warm-up, scan resistance и RSS.
5. Выбрать наименьший кандидат, который выполняет latency/hit/RSS gates на
   смешанном workload, а не тот, который просто использует всю свободную RAM.
6. Повторить sweep при максимальном числе одновременно живых snapshots,
   потому что version overlap может быть важнее числа физических файлов.

Текущая production telemetry агрегирована на весь pool. Per-file counters,
автоподбор, partitioning или admission quotas не добавлялись: они нужны только
если representative trace докажет, что один класс файлов загрязняет остальные
или ручная настройка недостаточна. Ближайший разумный следующий benchmark —
interleaved multi-file hot set с заведомо известными долями и sweep capacity.

Разведочный resident sweep уже показал, что один короткий прогон недостаточен
для автоматического правила:

| Pool pages | Direct+BufferPool |         mmap |
|-----------:|------------------:|-------------:|
|         64 |      5,502 Mops/s | 6,836 Mops/s |
|        256 |      6,382 Mops/s | 5,801 Mops/s |
|       1024 |      3,549 Mops/s | 4,738 Mops/s |
|       2048 |      4,372 Mops/s | 7,141 Mops/s |
|       4096 |      2,351 Mops/s | 4,262 Mops/s |
|       8192 |      2,189 Mops/s | 3,753 Mops/s |

Немонотонность указывает на DVFS/host noise. Общая тенденция больших наборов
совместима с ростом давления на CPU cache/TLB и metadata, но эти single-run
числа нельзя использовать для изменения default. Для решения нужен
изолированный многократный resident/multifile benchmark.

## NativeAsync

Windows I/O Ring gate прошёл:

| Queue depth |        sync |  NativeAsync | Throughput ratio | p99 ratio |
|------------:|------------:|-------------:|-----------------:|----------:|
|           1 | 148,3 MiB/s |  156,3 MiB/s |           1,054× |    0,997× |
|           8 | 156,6 MiB/s |  582,5 MiB/s |           3,720× |    0,340× |
|          32 | 152,7 MiB/s |  911,6 MiB/s |           5,968× |    0,131× |
|         128 | 135,7 MiB/s | 1288,2 MiB/s |           9,494× |    0,120× |

Регрессии QD1 нет, а рост queue depth сохраняет существенный throughput/p99
выигрыш. Изменения benchmark sync-пути не затронули NativeAsync page contract.

## Профилирование и ограничения инструментов

### Flamegraph

Sparse miss flamegraph: `target/stage3-sparse-before-flamegraph.svg`.
Cache metadata на физическом miss была мала: `prepare_access` около 0,19%,
reserve около 0,15–0,18%, создание load signal около 0,07–0,09% samples.
Профиль не подтвердил Clock-Pro как источник 12–22% sparse gap.

### Tracy

Sparse trace `target/stage3-sparse.tracy`: 147 456 zones. На 24 576 misses
средние участки составили примерно 90 114 нс load+validate, 999 нс
`prepare_access`, 761 нс reserve, 196 нс publish и 79 нс finish. Не-I/O часть
`get_or_load` — около 1,94 мкс, или 2,1%.

Resident trace `target/stage3-resident.tracy`: 120 832 zones и 17 408 calls;
его вывод разобран выше. Tracy сообщил `SymInitialize error 87` для системного
sampling, но пользовательские zones записались корректно; ошибка не блокировала
анализ.

### WPR/WPA

ETW traces: `target/stage3-sparse-before.etl` и
`target/stage3-sparse-after.etl`. CPU/host summary показал сопоставимый service
time raw и pool; заметная часть разницы находилась вне cache metadata. Штатный
`WPAExporter` на широком профиле зависал и создавал чрезмерный export, поэтому
для bounded таблиц использован `xperf`. ETW-трассы получены; это ограничение
экспортера, а не блокер этапа.

### Intel VTune

Результаты сохранены в `target/stage3-vtune-before` и
`target/stage3-vtune-after`. VTune предупреждал об отсутствующих Windows PDB
для системных модулей, но пользовательский код профилировался. Эксперимент с
объединением validation/publication под более крупным lock не улучшил результат
и полностью отменён; 14 buffer-pool tests подтверждают сохранённый контракт.

### Неиспользованные инструменты

`mimalloc-pprof`/allocation tracking не потребовались: pool заранее выделяет
фиксированные frames, allocation count стабилен, RSS gate проходит, а профили
не показали allocation growth как причину. Shuttle/sanitizers не запускались,
поскольку измеренный вопрос был про QD1 performance, а не воспроизведённую
concurrency/safety ошибку. Добавлять instrumentation или зависимости без
наблюдаемого триггера не требовалось.

## Проверка изменённого среза

- `rustfmt +nightly --edition 2024` для двух изменённых Rust-файлов — пройден.
- `cargo +nightly fmt --all --check` — пройден.
- `cargo test --lib --no-default-features storage::buffer_pool::tests` —
  14 passed.
- Unit-test парной агрегации — passed.
- Release cache-index probe — passed.
- `cargo check --tests --no-default-features` — passed.
- `cargo check --lib --no-default-features --features native-async-io` — passed.
- Changed-surface Clippy для `src/storage/benchmarks.rs` и
  `src/storage/buffer_pool/mod.rs` — замечаний нет.

Полный workspace Clippy остаётся красным на существующих нарушениях вне
изменённого среза, включая `allow` без `reason`, `unwrap`/`expect`, missing docs
и panic-policy в `values`, `syn` и других модулях. Они не исправлялись в рамках
performance-этапа.

## Решение и остаточные риски

Этап 3 для Windows sync GA-core закрыт. Production остаётся на Direct I/O +
bounded BufferPool; mmap остаётся comparator. Clock-Pro, cache sharding,
dense directory, per-file quotas и auto-sizing не реализованы, потому что
текущие измерения не доказывают их окупаемость.

Остаточные задачи, не блокирующие переход к этапу 4:

- отдельный Linux/NVMe parity gate;
- изолированный concurrent resident benchmark перед любым sharding;
- interleaved multi-file capacity sweep перед изменением default 4096;
- end-to-end dense-index prototype только если resident gap станет продуктовым
  bottleneck после появления representative workload.
