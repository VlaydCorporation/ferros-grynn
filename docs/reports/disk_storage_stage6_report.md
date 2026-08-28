# Отчёт этапа 6: Windows Native Async I/O

Дата: 2026-08-28.

## Результат

Optional `IoMode::NativeAsync` реализован на Windows через IoRing. Публичная query-модель осталась
синхронной: `PageIo` ставит независимые Direct-I/O extents в очередь и извлекает все completion
entries до возврата вызывающему. `IoMode::Sync` остаётся default; feature `native-async-io` не
входит в default-набор.

Используются сгенерированные bindings уже подключённого
`windows::Win32::Storage::FileSystem`: `QueryIoRingCapabilities`, `CreateIoRing`,
`BuildIoRingReadFile`/`BuildIoRingWriteFile`, `SubmitIoRing`, `PopIoRingCompletion` и
`CloseIoRing`. Динамическая загрузка символов, IOCP fallback и новые runtime-зависимости не
добавлены. Data-файл получает отдельный handle с
`FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED`; исходный synchronous handle сохраняется для
измеренного QD1 fast path.

## Профиль производительности

Окружение: Windows 10.0.22631, x86_64-pc-windows-msvc, Rust 1.97.1, KIOXIA BG5. Набор данных —
128 МиБ (8192 страницы по 16 КиБ), немеряемый warm-up 1024 страницы и пять повторений с
чередованием порядка Sync/NativeAsync. Оба backend-а используют один `DirectFile`, поэтому QD1
сравнивает один и тот же synchronous handle; сравниваются независимые медианы. Таблица содержит
контрольный release-прогон уже собранного бинарника, без влияния LTO-сборки.

|  QD | Sync, МиБ/с | IoRing, МиБ/с | Отношение | p99 IoRing / Sync |
|----:|------------:|--------------:|----------:|------------------:|
|   1 |       209,9 |         210,3 |    1,002× |            0,963× |
|   8 |       179,1 |         762,9 |    4,258× |            0,223× |
|  32 |       151,9 |        1049,0 |    6,904× |            0,148× |
| 128 |       151,8 |        1110,1 |    7,311× |            0,176× |

Gate пройден: QD8/32/128 значительно превышают требуемые 15% throughput и 10% p99, а QD1 не
регрессирует. Первый запуск сразу после fat-LTO показал нестабильный QD1 tail; поэтому benchmark
получил общий file handle и явный warm-up, после чего два последовательных release-прогона прошли.

## Flamegraph

`cargo flamegraph 0.6.14` запущен на том же QD gate в bench-профиле; артефакт:
`target/stage6-ioring-qd-flamegraph.svg`. Из 21 903 samples:

- `WindowsIoRing::read_extents` — 35,06%;
- `execute` — 34,84%;
- `execute_chunk` — 34,81%;
- `SubmitIoRing` — 33,65%;
- `BuildIoRingReadFile` — 0,39%;
- allocation `NativeRequest` — 0,08%;
- lock/unlock ring mutex — 0,01% / 0,05%.

Профиль снят до QD1 fast path; ETW исказил QD1 gate на 0,5 процентного пункта, но однозначно
показал доминирование ожидания `SubmitIoRing`. Регистрация файлов/буферов, sharding ring-а и
custom allocator не обоснованы текущими стеками.

## Корректность и границы

- Реальный IoRing-тест покрывает sparse multi-extent reads/writes и проверяет все страницы.
- Публичный E2E-тест покрывает local commit, Restart checkpoint, close/reopen и чтение в
  `NativeAsync`.
- Buffered WAL, commit barrier и формат страниц не менялись.
- Режим требует Windows build 22000+; отсутствие feature или runtime-возможности возвращает
  `FG-51I04`, без скрытого fallback.
- Linux/tokio-uring не реализован: нативного окружения для проверки нет.
- Регрессии полного sync-gate этапа 5 (sparse miss и scan resistance), Linux/NVMe и soak
  `≥ 1,25 × physical RAM` остаются release-блокерами; format v1 остаётся кандидатом.

| Проверка                                                             | Результат                                                  |
|----------------------------------------------------------------------|------------------------------------------------------------|
| `cargo check --lib --no-default-features`                            | passed                                                     |
| `cargo check --lib --no-default-features --features native-async-io` | passed                                                     |
| `cargo test --lib --no-default-features`                             | 300 passed, 0 failed, 2 ignored                            |
| `cargo test --lib --no-default-features --features native-async-io`  | 301 passed, 0 failed, 3 ignored                            |
| release `windows_ioring_qd_gate`                                     | passed; QD1/8/32/128 из таблицы выше                       |
| `cargo +nightly fmt --all -- --check`                                | passed; только warning об устаревшем rustfmt option        |
| changed-surface Clippy без/с `native-async-io`                       | passed с тремя временно разрешёнными глобальными doc lints |

Полный Clippy без разрешений остаётся заблокирован 1820 существующими ошибками документации в
старых `datastore`/`syn` и не считается успешной проверкой этого среза. После разрешения трёх
repository-wide doc lints новые storage/IoRing ошибки отсутствуют; оставшиеся warnings находятся
вне изменённой поверхности.

Основные внешние контракты: [CreateIoRing](https://learn.microsoft.com/windows/win32/api/ioringapi/nf-ioringapi-createioring),
[SubmitIoRing](https://learn.microsoft.com/windows/win32/api/ioringapi/nf-ioringapi-submitioring),
[BuildIoRingReadFile](https://learn.microsoft.com/windows/win32/api/ioringapi/nf-ioringapi-buildioringreadfile),
[Synchronous and asynchronous I/O](https://learn.microsoft.com/windows/win32/fileio/synchronous-and-asynchronous-i-o).
