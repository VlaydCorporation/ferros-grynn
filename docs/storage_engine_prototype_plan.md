# План прототипирования нового storage engine и BufferPool V2

## 1. Назначение и границы

Этот документ задаёт экспериментальный этап 3.1 после закрытого Windows sync
gate этапа 3. Его цель — проверить несколько альтернатив resident fast path,
политик admission, Windows Direct-I/O transport и bounded prefetch pipeline до
выбора production-архитектуры.

Нормативные источники:

- `disk_storage_spec.md` задаёт 16-КиБ page contract, Direct I/O, checksum,
  version-aware чтение, WAL и checkpoint;
- `gql_spec.md` и `graphs.md` задают формы point, scan, index и traversal
  workload;
- `gql_tech_spec.md` используется только как ненормативный ориентир будущего
  physical cursor.

Текущие `BufferPool`, `SyncPageManager`, on-disk format и GQL plumbing не
изменяются. Все новые реализации находятся внутри `src/storage` под
`#[cfg(test)]`; mmap остаётся только benchmark-control. `radixdb`, `fg-meta` и
reference crates не используются. Новая зависимость либо код из
`src/asynchronous`/`src/collections` допустимы только после проверки контракта,
тестов и измеримого преимущества над существующими средствами; сам факт их
наличия не является основанием для подключения.

## 2. Аналитическая модель hot path

| Hot path | Форма доступа | Проверяемые механизмы |
|---|---|---|
| Point/index reads | Повторные обращения к малому hot set | Atomic arena, resolved handle, VM address |
| Graph traversal | Skewed random hot pages и sparse cold transitions | Atomic arena, resolved handle, cache-all |
| B+tree range | Hot root/internal и sequential leaf chain | Cache-all roots, cold/bypass leaves |
| One-shot scan/bulk | Последовательный поток без reuse | Bypass, scatter/IoRing, prefetch |
| Properties/overflow | Point reads и короткие runs | Cache-all, shallow prefetch |
| Multi-file/version | Interleaved sources под одним budget | Version-aware identity и глобальная capacity |

`buffer_pool_pages` является общим бюджетом совокупного hot set всех файлов и
одновременно активных версий.

## 3. Матрица прототипов

### 3.1. Resident engines

- `R0` — mmap control;
- `R1` — текущий Clock-Pro `BufferPool`;
- `R2` — `AtomicArenaBufferPool`: contiguous arena, шардированный mapping,
  атомарные lifecycle/generation/pin/usage и miss-only clock sweep;
- `R3` — `R2 + ResolvedFrameHandle`, исключающий hash lookup при повторном
  доступе;
- `R4` — Windows `VmCache`: зарезервированное address space, commit/decommit
  по admission/eviction;
- `R5` — `R4 + VmResolvedHandle` с проверкой source epoch и page generation.

### 3.2. Admission policies

- `A0 CacheAll` — обычный cache admission;
- `A1 RocksLikeBypass` — resident hit разрешён, cold scan miss читается в
  reusable scratch и не занимает frame;
- `A2 InnodbLikeCold` — scan misses находятся в bounded cold-классе не более
  `3/8` pool; тот же scan не повышает страницу, другой cursor или point access
  запрашивает promotion.

Test-only `PrototypeScanCursor` выводит policy из `ReadIntent::{Point,
IndexTraversal, SequentialScan, BulkBuild}`. Production `QueryRead` на этом
этапе не меняется.

### 3.3. Read transports и prefetch

- `T0` — sync Direct extent в contiguous scratch с `memcpy`;
- `T1` — текущий extent-oriented IoRing control;
- `T2` — frame-oriented IoRing, одна SQE на 16-КиБ frame;
- `T3` — один `ReadFileScatter` на extent с прямой раскладкой в destination
  frames.

Все внешние API синхронные. Bounded prefetch pipeline проверяется с глубинами
`0`, `1`, `2`, `4`, `8` extents: worker читает следующий extent, consumer
валидирует и публикует текущий. Ошибка или раннее закрытие отменяет
неопубликованные reservations и завершает owned worker.

## 4. Топологический порядок реализации

1. Повторить текущий raw/validated/pool/mmap/IoRing baseline и записать
   environment contract.
2. Расширить test harness aggregate timing и probes: empty loop, lookup,
   atomic pin, guard, checksum, memcpy, reserve/publish, signal, full hit/miss.
3. Ввести минимальную test-only модель `SourceIdentity`, `SourceEpoch`,
   `ReadIntent` и workload results.
4. Реализовать и проверить `AtomicArenaBufferPool`.
5. Поверх него реализовать `ResolvedFrameHandle` и stale fallback.
6. Реализовать Windows VM reservation/commit/decommit и resolved VM handle;
   WAL-exact pages хранить в bounded VM frame slab, не в file-sized region.
7. Реализовать `CacheAll`, `RocksLikeBypass` и `InnodbLikeCold`.
8. Реализовать test-only `ReadFileScatter` и frame-oriented IoRing поверх
   существующих Direct/IoRing ресурсов.
9. Реализовать bounded prefetch pipeline.
10. До любых performance-выводов пройти correctness gate всех вариантов.
11. Завершить standalone screening всех вариантов.
12. Проверить все совместимые недоминируемые комбинации.
13. Выполнить полный workload/size/concurrency sweep финалистов.
14. Сформировать CSV, итоговый отчёт и отдельный production migration plan.

Ни один вариант не исключается по производительности до завершения пунктов
4–9 и их correctness tests.

Минимальная обязательная комбинационная матрица, проверяемая даже при
проигрыше отдельных механизмов в standalone screening:

- лучший Atomic engine с `A0`, `A1` и `A2`;
- лучший VM engine с теми же policies;
- `ReadFileScatter` и frame-oriented IoRing без prefetch и с лучшей глубиной;
- bypass + scatter + prefetch;
- cold admission + direct-to-frame transport + prefetch;
- mixed B+tree workload с cache-all roots и bypass/cold leaf chain.

Остальной декартов набор не запускается: после обязательной матрицы
комбинируются только технически совместимые недоминируемые варианты. Полный
size sweep выполняется только для кандидатов, которые пережили этот screening;
если ни один вариант не достигает минимальных performance gates, в отчёте
фиксируются недостигнутые проверки и следующий минимальный эксперимент, а
production path не меняется.

## 5. Correctness gate

Для применимых вариантов проверяются:

- byte equality с raw Direct и полная checksum/header validation;
- corruption, short read, EOF и I/O error до publish;
- одновременные `Home`/`Wal` versions и checkpoint generations;
- stale handle, frame generation и source epoch после eviction/reuse;
- ABA, pin во время eviction, coalesced miss и bounded backpressure;
- duplicate keys, batch order и interleaved 1/4/8-file access;
- VM access только между commit/decommit и полное release;
- bypass без роста resident count и bounded cold admission;
- prefetch ordering, cancellation, error propagation и bounded memory.

Atomic lifecycle дополнительно проверяется Shuttle-моделью. Unsafe остаётся
только в минимальных platform/arena wrappers с локальным `SAFETY`-доказательством.

## 6. Измерительный контракт

Component probes отделяют checksum, copy, reserve/publish, signals, guards,
mapping и atomic state. Resident mean измеряется агрегатно без per-operation
`Instant`; percentile перепроверяется batch-normalized timer и fenced TSC
только при подтверждённом invariant TSC и закреплённом CPU.

Screening использует 1-ГиБ dataset, resident hot sets `1/64/1024/4096`,
1/4/8 files, `1/2/4/8/12` threads и pool `0.5/0.75/1/1.25/1.5` от общего hot
set. Финалисты проходят размеры `1`, `10`, `50`, `130`, `300`, `1000`,
`10000`, `65536`, `100000`, `300000` pages и mixed workloads: sequential,
fixed-hot scan, 4x working set, B+tree, graph frontier, sparse properties и
concurrent home/WAL versions.

Resident screening содержит не менее девяти paired runs, финал — пятнадцать;
I/O screening — не менее пяти, финал — семи. Backend order ротируется. В
результатах сохраняются median, min/max, paired ratio, RSS delta, VM reserved
address space и committed bytes.

После standalone screening комбинируются все технически совместимые варианты,
которые не доминируются более простым кандидатом. Разница до 3% отдаёт выбор
более простой реализации; устойчивый проигрыш более 5% без выигрыша по
p99/RSS/CPU исключает кандидат.

## 7. Целевые показатели

Обязательны неизменные bytes/errors/version semantics, отсутствие partial
publish и committed memory не более `110%` pool budget плюс отдельно
зафиксированная metadata.

- generic Atomic internal hit: цель `<= 70 ns`;
- resolved/VM internal hit: цель `<= 35 ns`;
- полный resident workload: цель `>= 95%` mmap, floor `>= 90%`;
- resident p99: цель `mmap + 0.1 us`, floor `mmap + 0.2 us`;
- concurrent resident scaling: не менее 80% scaling mmap без глобальной
  lock-сериализации;
- sequential batched: `>= 90%` raw Direct и `>= 95%` лучшего validated path;
- fixed-hot scan: `>= 90%` raw Direct и `>= 99%` hot hit ratio.

Если gates не достигнуты, production cache остаётся без изменений, а отчёт
фиксирует доказанный остаточный bottleneck и следующий минимальный эксперимент.

## 8. Результаты

Итоговые артефакты:

- `docs/reports/storage_engine_prototype_report.md`;
- `docs/reports/storage_engine_prototype_results.csv`;
- полные локальные logs/profiles в `target/storage-engine-prototype/`.

Отчёт включает standalone и combined результаты, component decomposition,
VM lifecycle cost, `ReadFileScatter` против frame-IoRing, влияние policy и
prefetch, Pareto frontier, отклонённые варианты и dependency-ordered plan
production-интеграции. Tracy, VTune и WPR/WPA запускаются только для
финалистов; недоступность или ограничения записываются как `SKIP`, а не
скрываются.
