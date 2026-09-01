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

Первый Tracy-профиль 17 408 resident calls дал ориентиры 47 нс для metadata
state, 23 нс для lookup, 68 нс для resident update и 53 нс для `PageGuard`.
Однако эти зоны были вложены, а полный instrumented `prepare_access` вырос
примерно до 380 нс. Для участков длительностью в десятки наносекунд стоимость
самих зон уже сравнима с полезной работой, поэтому эти числа нельзя считать
стоимостью соответствующих Rust-операций.

### Повторная декомпозиция без per-operation instrumentation

Добавлен ignored release-probe, который заранее заселяет настоящие 4096
frames, циклически обращается ко всему 64-МиБ working set и измеряет каждый
вариант агрегированно: 1 000 000 операций, семь samples, итог — median. Три
контрольных прогона после полного отката экспериментальных production-изменений
дали следующие диапазоны:

| Участок                                              |        Стоимость |
|------------------------------------------------------|-----------------:|
| Пустой цикл с выбором ключа                          |     0,93–1,41 нс |
| Uncontended `lock` + `unlock` metadata mutex         |   18,69–19,58 нс |
| `entries.get(&key).copied()` под заранее взятым lock |    7,29–13,84 нс |
| `lock` + hash lookup + `unlock`                      |   19,86–23,96 нс |
| `access_resident` без захвата mutex                  |   18,30–20,67 нс |
| Полный `prepare_access`                              |   45,59–51,26 нс |
| Clone/drop resident page `Arc`                       |   15,68–16,46 нс |
| Clone/drop `Shared` `Arc`                            |   17,98–18,48 нс |
| Relaxed read числа capacity waiters                  |     1,18–2,31 нс |
| Полный guard round-trip с payload touch              |   35,63–40,99 нс |
| Полный `get_or_load` resident hit                    | 112,45–117,76 нс |
| Только `DashMap` lookup                              |   26,85–28,16 нс |
| `DashMap` lookup + текущий locked resident update    |   59,35–65,56 нс |

Строки неаддитивны: они по-разному прогревают cache lines, включают возврат и
drop временных значений, а некоторые являются вложенными. Надёжный итог здесь
не сумма строк, а полный hit 112–118 нс, совпадающий по порядку с наблюдаемым
отставанием от mmap.

### Что именно происходит в `prepare_access`

На resident hit цикл выполняет ровно одну итерацию:

1. Берёт `parking_lot::Mutex<ClockPro>`.
2. Делает `entries.get(&key).copied()`.
3. Вызывает `access_resident(frame_index)` и выходит через `break`.

`reserve`, регистрация waiter и `capacity_available.wait(&mut state)` на этой
ветке не исполняются. Проверка после прогона подтверждает
`backpressure_waits == 0`. Следовательно, стоимость condvar wait на resident
hit равна нулю, а 46–51 нс — это lock, lookup, frame lookup/update и page pin,
а не ожидание свободной capacity.

Цикл нужен miss-ветке без свободного/вытесняемого frame. Первый неуспешный
`reserve` освобождает state, регистрирует waiter и повторяет проверку, закрывая
lost-wake окно. Только если capacity всё ещё недоступна, `Condvar::wait`
атомарно отпускает mutex и усыпляет caller. Mutex никогда не удерживается во
время физического I/O.

Из 46–51 нс `prepare_access` около 19 нс занимает uncontended lock/unlock, а
вся комбинация lock+lookup — около 20–24 нс. Остаток включает обращение к
`frames[frame_index]`, проверку `FrameState`, `referenced = true`, увеличение
обычного hit counter и clone resident `Arc`. Это QD1-стоимость короткой
критической секции, а не 47 нс ожидания mutex.

### Что означал `resident update`

`access_resident` делает только match состояния frame, запись reference bit,
увеличение `hits` и `Arc::clone(page)`. Повторный probe измеряет весь вызов
вместе с drop возвращённого `AccessDecision` в 18–21 нс. Отдельный clone/drop
page `Arc` занимает 16 нс, поэтому reference bit, counter и ветвление не
являются самостоятельными десятками наносекунд. Прежние 68 нс были
возмущённым Tracy-измерением вложенной зоны, а не фактической стоимостью
resident update.

Счётчик hit находится под уже взятым mutex и не является atomic. Перенос его в
общий atomic добавил бы cache-line contention, а удаление ухудшило бы
наблюдаемость. Ни одно из этих изменений не оправдано измеренным остатком.

### Что стоит в `PageGuard`

`PageGuard` занимает 16 байт: один `Arc` pinned page и один `Arc<Shared>` в
`ReleaseNotifier`. Метод `guard` не клонирует page: он перемещает уже pinned
`Arc` из `AccessDecision` и клонирует только `Shared`. Но полный жизненный цикл
включает ранее выполненный page pin, clone `Shared`, чтение payload и drop.
Именно этот round-trip стоит 36–41 нс; отдельно clone/drop `Shared` — около
18 нс, page `Arc` — около 16 нс, а relaxed waiter check при drop — 1–2 нс.
Запись двух полей структуры не объясняет прежние 53 нс: это снова был предел
точности instrumented зоны.

На drop сначала освобождается page pin, затем notifier читает число waiters.
При нуле он не берёт mutex и не вызывает notify. Owned `Arc<Shared>` сохраняет
важный контракт: guard можно передать другому thread и хранить независимо от
заимствования конкретного `BufferPool`. Замена на borrowed reference ради
одного atomic clone протащила бы lifetime через `PageRead` и существующие
callers, поэтому это уже изменение API/lifetime contract, а не бесплатная
оптимизация.

### Нужен ли `DashMap`

Нет, как прямая замена `entries` — нет. Его одиночный lookup оказался
медленнее текущего `RapidHashMap` вместе с metadata mutex: 27–28 нс против
20–24 нс. После lookup всё равно требуется согласованно изменить
`FrameState::referenced`, stats и CLOCK-Pro metadata. Вариант
`DashMap + state.lock()` дал 59–66 нс против 46–51 нс текущего
`prepare_access`, то есть добавил второй lock layer.

Настоящий lock-free/sharded resident fast path потребовал бы уже не замены map,
а новой модели: resident directory со стабильными entry-объектами, atomic
reference bits/counters и протоколом согласования eviction, pinning и удаления
устаревшего frame index. Без такого протокола возникает stale-index/ABA race.
Это допустимый будущий прототип, но не локальный безопасный fix.

Контрольный concurrent probe всё же доказал, что единый metadata state станет
пределом при большом числе reader threads:

| Threads | Один сверхгорячий key | Распределённые keys |
|--------:|----------------------:|--------------------:|
|       1 |          13,75 Mops/s |         6,94 Mops/s |
|       2 |           8,18 Mops/s |         7,77 Mops/s |
|       4 |           4,14 Mops/s |         3,57 Mops/s |
|       8 |           2,50 Mops/s |         1,72 Mops/s |

Это aggregate-измерение без per-op timestamp: оно показывает отсутствие
масштабирования, но не меняет вывод по QD1. Узкое место многопоточного пути —
единые CLOCK-Pro invariants и изменяемые cache lines в целом, а не только
контейнер `entries`. Шардирование следует прототипировать, когда реальный
query workload подтвердит такой reader fan-out.

### Почему разница выглядит как миллион ops/s

В итоговом повторном прогоне Direct+BufferPool дал 2 607 620 ops/s
(383,5 нс/op), mmap — 3 722 410 ops/s (268,6 нс/op). Разница 1 114 790 ops/s
полностью следует из дополнительных 114,9 нс, потому что throughput — обратная
величина latency: `1e9 / 268,6` и `1e9 / 383,5`. Миллион не является ещё одним
необнаруженным этапом работы; на базе около 270 нс даже сто дополнительных
наносекунд сильно меняют обратную величину.

`measure_order` берёт `Instant` и записывает latency на каждой операции.
Контрольный no-op через тот же harness дал 132,5 нс/op и p99 0,1 мкс. Эта
общая стоимость входит и в pool, и в mmap, поэтому не объясняет их среднюю
разницу, но делает абсолютные ops/s ниже и квантует короткий p99 шагом около
0,1 мкс. Финальный p99 был 0,8 против 0,5 мкс, ранее — 0,8 против 0,4 мкс.
Следовательно, устойчивый вывод — дополнительный хвост 0,3–0,4 мкс при
пройденном gate `mmap + 0,5 мкс`; приписать каждую десятую микросекунды одному
полю по этому harness нельзя.

Для устранения branch/cold-path overhead был проверен минимальный production
эксперимент: inline resident dispatch и вынос wait/load веток в cold helpers.
Он сдвинул aggregate full-hit лишь на несколько наносекунд внутри межпрогонного
шума и не изменил p99. Изменение полностью отменено. Production hot path и
CLOCK-Pro остались прежними; сохранён только воспроизводимый ignored probe и
учёт стоимости benchmark harness.

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
  14 passed, диагностический resident probe ожидаемо ignored.
- Release resident cost probe — passed; три финальных samples после отката
  production-эксперимента дали полный hit 112,45–117,76 нс.
- Парный resident/mmap probe после отката — passed: 2,608 против 3,722 Mops/s,
  p99 0,8 против 0,5 мкс; no-op harness 132,5 нс/op и p99 0,1 мкс.
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
