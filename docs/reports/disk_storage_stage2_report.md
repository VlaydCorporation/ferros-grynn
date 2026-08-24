# Отчёт этапа 2: Sync Direct I/O и BufferPool

Дата нормативного прогона: 2026-08-24.

## Реализованный срез

- Data-файл открывается с `FILE_FLAG_NO_BUFFERING` на Windows и `O_DIRECT | O_CLOEXEC` на Linux; buffered fallback отсутствует.
- Каждый frame `BufferPool` один раз получает выровненный `Arc<Box<AlignedPage>>`. При eviction адрес и allocation переиспользуются; на hit страница не копируется.
- Metadata mutex не удерживается во время I/O. Совпадающие misses объединяются, pinned/loading frames создают backpressure, а ошибка batch освобождает все незавершённые reservations.
- Фаза multi-frame admission сериализована отдельным mutex и заканчивается до I/O. Это исключает взаимную блокировку двух batch callers, каждый из которых удерживает частичные reservations при capacity pressure; отдельный regression test проверяет этот сценарий.
- `PageGuard::drop` не берёт mutex при отсутствии capacity waiters. Hash-таблицы используют уже подключённые `RapidHashMap`/`RapidHashSet`.
- Непрерывный extent читается одним позиционным syscall в выровненный batch-buffer. Страницы параллельно проверяются и копируются в постоянные frames; checksum и `(page_no, page_lsn)` проверяются до публикации.
- Read-only `SyncPageManager` соединяет `DirectFile` и `BufferPool`; `read_pages` сохраняет входной порядок и дубликаты. Полный snapshot/frame-WAL manager относится к этапу 3.

Async I/O, production mmap, contiguous unsafe slab, sharding и новая cache policy не добавлялись.

## Исправленный benchmark

Измеряются четыре независимых backend’а: raw `DirectFile` без checksum, production Direct+BufferPool с полной проверкой страницы, обычный buffered positional `File` и benchmark-only mmap. Каждый backend/workload/repetition создаёт свой файл через Direct writer и закрывает writer до измерения; порядок backend’ов ротируется между тремя repetition.

Resident hit учитывает один реально прочитанный marker byte и выводится в ops/s и ns/op. Для throughput учитываются полные логически прочитанные страницы. RSS mmap печатается, но не участвует в memory gate: mapping и системный file cache не имеют лимита 64 МиБ.

Scan-resistance выполняет четыре непрерывных cold batch’а, затем один непрерывный batch из фиксированного прогретого hot set. Поэтому его throughput сравнивается с raw batched Direct, а обычный sequential batch — с fresh-file mmap first-touch, как требует план.

## Окружение и команда

- ОС: Microsoft Windows NT 10.0.22631.0, `x86_64-pc-windows-msvc`.
- Rust: `rustc 1.97.1 (8bab26f4f 2026-07-14)`, LLVM 22.1.6.
- Dataset: 65 536 страниц × 16 КиБ = 1 ГиБ.
- BufferPool: 4096 страниц = 64 МиБ.
- Repetition: 3, deterministic seed `0x464753544f524147`.

```text
cargo test --profile bench --lib --no-default-features storage::benchmarks::disk_storage_stage2_gate -- --ignored --nocapture --test-threads=1
```

## Результат Windows gate

| Проверка                      | Direct+BufferPool |               Comparator | Результат |      Порог | Gate |
|-------------------------------|------------------:|-------------------------:|----------:|-----------:|:----:|
| Single-page miss throughput   |      160,44 MiB/s |  raw Direct 137,79 MiB/s |    116,4% |      ≥ 90% |  да  |
| Single-page miss p99          |         252,0 мкс |     raw Direct 313,7 мкс |     80,3% |     ≤ 125% |  да  |
| Resident-hit p99              |           0,7 мкс |             mmap 0,4 мкс |  +0,3 мкс | ≤ +0,5 мкс |  да  |
| Sequential batch throughput   |     1677,70 MiB/s |        mmap 939,84 MiB/s |    178,5% |      ≥ 90% |  да  |
| Scan-resistance throughput    |     2186,96 MiB/s | raw Direct 1779,64 MiB/s |    122,9% |      ≥ 90% |  да  |
| Scan hot-set hits             |            100,0% |                        — |    100,0% |      ≥ 99% |  да  |
| Durable batch writes          |      275,52 MiB/s |        mmap 210,32 MiB/s |    131,0% |      ≥ 90% |  да  |
| Максимальный Direct RSS delta |   66 850 816 байт |   budget 67 108 864 байт |     99,6% |     ≤ 110% |  да  |

Информационно, production sequential batch составляет 74,0% raw transport (`1677,70/2266,28 MiB/s`). Это не gate между разными семантиками: production path дополнительно вычисляет checksum, проверяет заголовок, копирует данные в постоянные frames и выполняет cache admission. Random/oversized mmap также остаются только в отчёте из-за неограниченной памяти: при полном random-проходе его RSS delta достиг 1 073 831 936 байт против 33 353 728 байт у Direct+BufferPool.

После concurrency-fix было выполнено два полных post-fix запуска. Первый стартовал сразу после 8 мин 21 с `fat LTO` и не прошёл scalar gates на фоне системного падения всех backend’ов: raw sparse снизился до 107,34 MiB/s, mmap resident — до 1,66 Mops/s, buffered path также просел в несколько раз. Эти workload’ы не используют новый batch-admission mutex. После 45 секунд без CPU/I/O и без rebuild повторный запуск дал таблицу выше и прошёл gate. Загрязнённый результат не скрывается: дальнейшие официальные прогоны должны выполняться на изолированном host без предшествующей тяжёлой линковки.

## Калибровка batch

|   Extent | raw Direct median |        p99 |
|---------:|------------------:|-----------:|
|   16 КиБ |      171,58 MiB/s |  368,7 мкс |
|   64 КиБ |      628,29 MiB/s |  271,3 мкс |
|  256 КиБ |     1168,51 MiB/s |  558,7 мкс |
| 1024 КиБ |     2075,69 MiB/s | 1011,2 мкс |
| 2048 КиБ |     2364,95 MiB/s | 1189,8 мкс |

Для Windows выбрано именованное значение `SYNC_BATCH_BYTES = 2048 KiB`: это максимальная median throughput в заданном наборе и допустимый p99. Linux сохраняет provisional 256 КиБ до собственного нативного прогона.

## Профилирование

ETW-трасса подтвердила одинаковый объём физического I/O для raw Direct и Direct+BufferPool при sync QD1; оставшийся разрыв batch path находится в CPU/cache-admission, а не в дополнительном дисковом чтении. Flamegraph показал значимую стоимость `ClockPro::reserve` и ghost/hash операций. Замена std hash-таблиц на уже используемые RapidHash и удаление избыточной ghost-membership проверки сократили основной профильный subtree с 803 до 541 samples, а `reserve` — с 218 до 74 samples. Hot-set gate после изменения остался 100%.

`flamegraph 0.6.14` установлен через `cargo install flamegraph --locked` как глобальный Cargo-инструмент; project manifest и lockfile ради профилировщика не менялись. Артефакты профиля находятся в ignored `../../target`: `stage2-smoke-general.etl`, `stage2-batch-probe-*.svg` и `stage2-buffer-pool-flamegraph*.svg`.

## Решение

Нативный Windows gate этапа 2 пройден. Переход на mmap не требуется; production-архитектурой остаются Direct I/O и управляемый BufferPool.

Общий этап 2 ещё не закрыт: обязательны отдельный Linux/NVMe gate и не-CI soak на dataset не меньше `1,25 × physical RAM`. До них frame-WAL и Native Async I/O не начинаются; текущий Windows-результат не объявляется эквивалентным результату Linux.
