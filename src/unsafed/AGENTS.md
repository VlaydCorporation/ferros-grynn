# Unsafed module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/unsafed`.

## Назначение и границы

`unsafed` — общий дом **переиспользуемых** unsafe-примитивов, необходимых более
чем одной подсистеме: raw allocation/memory operations, unaligned byte access,
низкоуровневые OS helpers и representation/size utilities.

Это не склад всего unsafe-кода проекта. Unsafe, реализующий инвариант Direct
I/O page, graph storage или конкретной FFI boundary, остаётся рядом со своим
владельцем. В `unsafed` переносится только минимальный повторяемый primitive с
самостоятельным безопасным контрактом.

Модуль сейчас не подключён к `src/lib.rs` и не прошёл production-аудит. Перед
использованием выбранный срез нужно проверить, исправить ошибки и API,
добавить документацию `# Safety`, заменить prototype error boundary на
`gql_status` и включить в обычные checks/tests.

## Карта подмодулей

- `heap_estimator.rs` — shallow/heap size и alignment helpers;
- `memory.rs` — raw allocate/free/set/copy, page size, atomic helpers и
  little-endian unaligned access;
- `os.rs` — physical/swap/virtual memory и file-descriptor observations;
- `error.rs` — текущие prototype errors низкоуровневой памяти; не являются
  production error contract;
- `mod.rs` — фасад; экспортировать только проверенные primitives.

Системная телеметрия общего назначения принадлежит `src/sys.rs`, allocator и
heap accounting — `src/memory`; дубли из `unsafed::os`/`heap_estimator` при
интеграции должны либо получить уникальную ответственность, либо быть сведены
к этим владельцам.

## Safety и проверка

Каждая unsafe-функция документирует допустимые pointers, alignment, length,
initialization, aliasing, ownership и lifetime, включая требования после
ошибки. Не использовать `Layout::unwrap`, raw pointer API без владельца или
`SeqCst` по умолчанию без обоснования.

Минимум перед production: unit/property tests граничных размеров и overlap,
Miri для поддерживаемого среза, platform tests Windows/Unix, allocation-failure
paths и review `rust-unsafe` (для FFI также `rust-unsafe-ffi`).
