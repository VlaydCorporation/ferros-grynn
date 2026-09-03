# Memory module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/memory`.

## Назначение и границы

`memory` — основной модуль управления и учёта памяти: выбор global allocator,
опциональный allocator tracking, heap accounting, внешние memory reporters и
будущая политика memory budgets/admission. Наблюдения о доступной системной
памяти и CPU предоставляет `src/sys.rs`; `memory` интерпретирует их для
лимитов, но не дублирует сбор системной телеметрии.

Различать:

1. логический размер данных;
2. оценку owned heap конкретного значения;
3. bytes, увиденные allocator tracking;
4. RSS/process/system memory из `sys`.

Эти величины имеют разную точность и не должны подменять друг друга в
`INFO`, `EXPLAIN ANALYZE`, spill или admission решениях.

## Карта подмодулей

- `mod.rs` — target/feature selection единственного global allocator и фасад
  memory reporters;
- `track.rs` — `TrackAlloc`, thread-local batching, global counter и threshold
  при feature `allocation-tracking`;
- `notrack.rs` — прозрачная allocator-обёртка без tracking для обычных builds;
- `registry.rs` — weak registry внешних `MemoryReporter` и агрегирование bytes;
- `measurable.rs` — `Measurable`, `HeapEstimatorCache` и shallow/owned heap
  estimation contract.

`src/sys.rs` владеет `Environment`, cached `Information`, total/cgroup memory,
process CPU/RSS/load и parallelism. Если файловый модуль станет сложнее, его
можно отдельно превратить в `src/sys/**`, но не заранее.

## Инварианты и production readiness

- В crate существует ровно один global allocator для каждой target/feature
  конфигурации; MSVC и non-MSVC выбор проверяется отдельно.
- Tracking не должен рекурсивно аллоцировать без границы и обязан учитывать
  `alloc`, `alloc_zeroed`, `realloc` и `dealloc` согласованно.
- Thread-local batching является оценкой до flush; это ограничение явно
  учитывать в hard budget и shutdown.
- Reporter registry не удерживает владельцев и очищает dead weak references.
- Environment-only prototype настройки и старые внешние имена не становятся
  публичным embedded contract: перед использованием их нужно перенести в
  принятую конфигурацию и покрыть тестами.
- Изменения allocator/unsafe кода требуют отдельного safety и platform аудита.

## Проверка

Проверять tracking on/off, allocator target cfg, alloc/realloc/dealloc balance,
thread flush и reporter drop/cleanup. Для budget policy добавлять тесты на
cgroup/system limits, unavailable telemetry, overflow и расхождение
allocator/RSS. Allocation instrumentation не считается точным доказательством
отсутствия native/external памяти.
