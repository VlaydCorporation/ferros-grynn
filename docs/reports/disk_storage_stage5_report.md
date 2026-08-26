# Отчёт этапа 5: Windows sync storage completion

Дата финального прогона: 2026-08-26.

## Результат

Функциональный срез этапа 5 реализован, correctness gate пройден. Полный Windows performance
gate не пройден, поэтому format v1 **не заморожен** и остаётся Windows-кандидатом. Этап 6
Native Async I/O не начат.

Отдельно остаются ранее отложенные Linux/NVMe gate и soak на dataset не меньше
`1,25 × physical RAM`. Они не выполнялись из-за отсутствия тестового окружения.

## Реализованный срез

- Добавлены order-preserving format-v1 keys для scalar, composite, `RecordId` и стабильных
  atom references. `none`, `null`, числа, decimal, строки, temporal и range сохраняют runtime
  order; NaN и значения без полного scalar order отклоняются.
- Durable standard/unique B+tree поддерживает postings, exact и bounded range lookup. Два
  индекса одного kind различаются `DataFileId.instance_id`; catalog/reopen сохраняет поколения.
- Query adapter извлекает single-field exact/range и полный composite exact constraint из
  конъюнкции `AND`. Index выдаёт candidates, после чего обычный evaluator повторно проверяет
  полный predicate; неподдерживаемый constraint корректно деградирует к scan.
- Generic V–V/V–E/E–V/E–E incidence, hyper endpoints, root-wide nested slots,
  cross-level overflow и transition graphs сохраняются через commit/reopen.
- Transaction-bound `LocalGraphView` рекурсивно разрешает nested graphs, а
  `query_to_memory` материализует owned graph только по явному запросу.
- E2E проверки покрывают atomic multi-graph commit, разные атомы одной страницы, same-atom
  FCW, unique rollback, точные SSI predicates, graph-family invariants и все persistable
  `Value` tags.

## Исправления benchmark-а

- Каждый backend/workload/repetition использует собственный fresh file, созданный Direct
  writer-ом и закрытый до чтения. Backend order ротируется; raw Direct и Direct+BufferPool
  имеют одинаковую сумму позиций.
- Пять calibration candidates выполняются в трёх перестановках с одинаковой суммой позиций
  `9` для каждого размера. Это убирает систематическое преимущество раннего или позднего запуска.
- Sparse QD1 сначала заполняет pool отдельным disjoint набором страниц. Измеряемая часть
  содержит `min(4 × pool, dataset / 2)` уникальных misses, поэтому проверяет steady-state
  eviction/reuse, а не одноразовый first-touch frame allocations.
- Resident hit считает один фактически прочитанный marker byte и выводится как ns/op,
  operations/s и percentiles. Mmap RSS печатается, но не участвует в Direct memory gate.
- Raw transport не вычисляет checksum; production path выполняет полную проверку страницы.
  Пороговые значения плана не ослаблялись.

## Окружение и команда

- ОС: Microsoft Windows NT 10.0.22631.0, `x86_64-pc-windows-msvc`.
- Накопитель: KIOXIA BG5 NVMe.
- Rust: `rustc 1.97.1 (8bab26f4f 2026-07-14)`, LLVM 22.1.6.
- Dataset: 65 536 страниц × 16 КиБ = 1 ГиБ.
- BufferPool: 4096 страниц = 64 МиБ.
- Repetition: 3, deterministic seed `0x464753544f524147`.
- Перед нормативным прогоном: 45 секунд без CPU/I/O после `fat LTO`.

```text
cargo test --profile bench --lib --no-default-features storage::benchmarks::disk_storage_stage2_gate -- --ignored --exact --nocapture
```

## Финальный Windows gate

| Проверка                      | Direct+BufferPool |               Comparator | Результат |      Порог | Gate |
|-------------------------------|------------------:|-------------------------:|----------:|-----------:|:----:|
| Single-page miss throughput   |      107,68 MiB/s |  raw Direct 138,92 MiB/s |     77,5% |      ≥ 90% |  нет |
| Single-page miss p99          |         427,4 мкс |     raw Direct 342,1 мкс |    124,9% |     ≤ 125% |  да  |
| Resident-hit p99              |           0,8 мкс |             mmap 0,4 мкс |  +0,4 мкс | ≤ +0,5 мкс |  да  |
| Sequential batch throughput   |     1447,22 MiB/s |        mmap 889,17 MiB/s |    162,8% |      ≥ 90% |  да  |
| Scan-resistance throughput    |     1465,93 MiB/s | raw Direct 1662,64 MiB/s |     88,2% |      ≥ 90% |  нет |
| Scan hot-set hits             |            100,0% |                        — |    100,0% |      ≥ 99% |  да  |
| Durable batch writes          |      264,53 MiB/s |        mmap 203,28 MiB/s |    130,1% |      ≥ 90% |  да  |
| Максимальный Direct RSS delta |   69 046 272 байт |   budget 67 108 864 байт |    102,9% |     ≤ 110% |  да  |

Random/oversized mmap остаётся информационным: в full-random проходе его RSS delta достиг
1 073 827 840 байт против 58 273 792 байт у Direct+BufferPool; системный file cache mmap
дополнительно не ограничен.

## Калибровка batch

|   Extent | raw Direct median |        p99 |
|---------:|------------------:|-----------:|
|   16 КиБ |      314,95 MiB/s |  193,8 мкс |
|   64 КиБ |      585,20 MiB/s |  329,8 мкс |
|  256 КиБ |     1019,40 MiB/s |  528,1 мкс |
| 1024 КиБ |     1750,07 MiB/s |  967,0 мкс |
| 2048 КиБ |     2085,96 MiB/s | 1702,6 мкс |

Windows-константа `SYNC_BATCH_BYTES = 2048 KiB` подтверждена: 1024 КиБ даёт только 83,9%
максимальной median throughput и не попадает в окно 95%. Linux сохраняет provisional 256 КиБ
до отдельной нативной калибровки.

## Flamegraph и стеки

`cargo flamegraph 0.6.14` уже был установлен как глобальный Cargo-инструмент, поэтому manifest
и lockfile ради профилировщика не менялись. ETW потребовал административный запуск. Основной
steady-state профиль содержит 6718 samples и сохранён в ignored `target`:

```text
target/stage5-pool-miss-steady-flamegraph.svg
```

Дополнительные A/B профили checksum path:

```text
target/stage5-pool-miss-flamegraph.svg
target/stage5-pool-miss-flamegraph-optimized.svg
target/stage5-pool-miss-flamegraph-inline.svg
```

Широкий production miss stack проходит через
`measure_order → read_pool_extent → BufferPool::get_or_load → DirectFile::read_page_raw →
seek_read` и обязательную `validate_page_bytes → page_checksum → RapidHash V3`.

В steady-state профиле metadata path мал относительно полного ETW-run:

| Stack                         | Samples | Доля полного профиля |
|-------------------------------|--------:|---------------------:|
| `BufferPool::prepare_access`  |      32 |                0,48% |
| `ClockPro::reserve`           |      24 |                0,36% |
| `Arc<LoadSignal>::new`        |       6 |                0,09% |
| `ClockPro::add_ghost`         |       3 |                0,04% |
| `parking_lot::RawMutex::lock` |       3 |                0,04% |

LTO inline frames для Direct read и checksum частично вложены друг в друга, поэтому их проценты
нельзя складывать. `cargo flamegraph --no-inline` на Windows не поддерживается. Тем не менее
профиль однозначно не показывает требуемых планом 5% CPU внутри ghost/balance и не оправдывает
переписывание Clock-Pro или sharding.

Были проверены immutable streaming checksum и fixed-length inline checksum с временным
zero/restore поля. Inline-вариант не уменьшил hash subtree и не улучшил стабильность throughput;
оставлен более простой safe streaming path. Опциональный `unsafe` feature RapidHash не включался:
ожидаемое локальное ускорение hash не покрывает measured end-to-end разрыв и расширяет unsafe
контракт без достаточного основания.

## Correctness и статические проверки

| Команда                                                    | Результат                                                                                 |
|------------------------------------------------------------|-------------------------------------------------------------------------------------------|
| `cargo test --lib --no-default-features`                   | 299 passed, 0 failed, 2 ignored native benchmarks                                         |
| `cargo check --lib --no-default-features`                  | passed; только существующие warnings                                                      |
| `cargo +nightly fmt -- --check`                            | passed; только warning об устаревшем rustfmt option                                       |
| changed-surface `cargo clippy --lib --no-default-features` | passed с тремя временно разрешёнными глобальными doc lints; оставшиеся warnings вне среза |

Полный Clippy без разрешений по-прежнему блокируется существующим repository-wide долгом
private/error/panic documentation за пределами изменённого storage-среза; он не маскируется как
успешная проверка.

## Решение

Production-архитектура не меняется: Direct I/O и управляемый BufferPool остаются основой,
production mmap не добавляется. Функциональная часть этапа 5 готова, но два throughput-критерия
полного sync-gate не выполнены. Format v1 не фиксируется, optional Async I/O и Unix runtime не
начинаются в этом срезе.

Следующее изменение performance path должно опираться на новый нативный trace с disk bytes,
queue depth, off-CPU wait и CPU frequency/thermal state. Текущий flamegraph не подтверждает
BufferPool policy/allocator как причину, достаточную для ещё одного структурного рефакторинга.
