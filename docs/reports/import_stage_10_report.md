# Отчёт этапа 10: Atomic Import

Дата: 2026-09-24.

## Результат

Этап 10 завершён. Реализован один атомарный import-путь для Rust callback API и
GrynQL `BEGIN IMPORT`: изменения применяются к private COW root, наружу
публикуются одним финальным catalog/WAL commit, а ошибка до публикации сохраняет
прежний snapshot. RAM и local backends дают одинаковый логический результат;
local generation переживает reopen и recovery.

Закрыты ресурсные задачи `Q-17` и `Q-18`:

- property histograms строятся существующим bounded external sort с бюджетом
  256 МиБ;
- готовые Primary/Standard/Unique/Label/membership pages пишутся по одной в
  недостижимые side-files и больше не удерживаются как `Vec<AlignedPage>`;
- posting continuations и Label memberships имеют ограниченное промежуточное
  состояние;
- финальная `AlignedPage` кодируется в стабильном heap slot.

Неподдерживаемые import-options остаются отложенными в `Q-33`. Глобальный долг
форматирования, документации и Clippy остаётся в `Q-14` и не маскируется
локальными проверками этапа.

## Реализованный контракт

### Транзакционная граница

- `BEGIN IMPORT` фиксирует исходный snapshot и получает database-wide writer
  lease. Readers продолжают видеть прежний root, другие writers получают
  `ImportInProgress` (`FG-25F03`).
- Каждый `apply_item` является атомарной границей. При `SKIP` только текущий item
  откатывается; при `ABORT`, превышении reject limit или fatal error отбрасывается
  весь import.
- Проглоченная callback-кодом fatal error сохраняется в состоянии session и не
  позволяет затем опубликовать частичный результат.
- `SEGMENT SIZE` ротирует только transaction-private mutation/read/write state.
  Segment не является commit, не виден читателям и не добавляет WAL publication.
- `Drop`, panic, ранний возврат, validation error и injected I/O failure ничего
  не публикуют.

### Языковой и Rust API

Frontend содержит отдельный `ImportStatement`; parser, analysis, logical IR и
execution используют тот же coordinator, что и публичный callback API:

```rust,ignore
datastore.import(options, |session| {
	session.apply_item(|transaction| {
		// Один логический input item; здесь допустимы связанные DML.
		Ok(())
	})?;
	Ok(())
})?;
```

Поддержаны `CREATE`, `UPDATE`, `UPSERT`, `DELETE`, `ATTACH`, `DETACH` и `RELATE`.
DDL, read/result-producing statements и обычный `RETURN` отвергаются до staging.
`INTO GRAPH` подставляет scope только graph-local DML; явный другой graph,
неизвестный target и cross-graph `DELETE CASCADE` отвергаются preflight.
`Transaction::execute` не разрешает import внутри уже открытой транзакции.

`INDEXES=DEFERRED|MAINTAINED`, `CONSTRAINTS=DEFERRED|IMMEDIATE`,
`ON VIOLATION=ABORT|SKIP`, положительные `REJECT LIMIT` и `SEGMENT SIZE`,
`EVENTS=OFF` и `CHANGEFEED=OFF` исполняются. Опции из `Q-33` распознаются, но
дают typed unsupported до writer lease и создания staging.

`ImportReport` содержит processed/applied/skipped items, число segments,
полные totals по классам нарушений, не более 1000 details, построенные индексы,
phase/index timings и выбранные режимы. Filesystem paths в report не попадают.

### Validation

- Immediate constraints проверяются на границе item.
- Deferred Unique и graph-family constraints проверяются перед publication и
  при ошибке отбрасывают весь import.
- `END IMPORT WITH VALIDATION` дополнительно проверяет весь затронутый AtomStore,
  graphs и side-files, включая данные исходного snapshot.
- `INDEXES=DEFERRED` и `INDEXES=MAINTAINED` дают одинаковый логический root;
  GrynQL и callback API дают одинаковые root и report counters.

## Local COW и recovery

Local backend строит новое недостижимое UUID-поколение и не пишет per-item WAL.
Последовательность публикации:

1. потоково построить data/index/statistics files;
2. синхронизировать каждый файл и generation directory;
3. подготовить catalog image;
4. записать единственную WAL-транзакцию публикации;
5. атомарно заменить видимый root/catalog.

Failure points после build, sync, catalog staging и перед WAL commit проверяют,
что прежний snapshot остаётся authority. Startup cleanup удаляет orphan UUID
generations и незавершённые external-sort sessions. Повреждённый primary index
после публикации остаётся фатальной ошибкой reopen; скрытого scan fallback для
identity index нет.

## Потоковые builders и bounded statistics

Переиспользован один production external sorter. Второй import-specific sorter,
trait с единственной реализацией и private spill format не добавлялись.

- budget run generation: 256 МиБ;
- merge fan-in: 64;
- property values сортируются и сворачиваются без `O(rows x fields)` retention;
- B+tree levels строятся снизу вверх через конкретные writer functions;
- Label и membership output сразу поступают в назначенный side-file;
- standard posting chain удерживает только текущую bounded группу/continuation;
- startup cleanup знает те же session paths, что и обычный index build.

Mutation/read/write sets освобождаются на границе segment и дополнительно
защищены существующим byte limit. Violation totals считаются полностью, но
детали ограничены `FG_IMPORT_MAX_REPORTED_VIOLATIONS = 1000`.

## Correctness evidence

Покрыты следующие наблюдаемые сценарии:

- import как единственный top-level statement и запрет nested/transaction import;
- ранний отказ unsupported options, result-producing DML и неверного graph scope;
- implicit graph target и cross-graph cascade preflight;
- все семь DML, включая одинаковый RAM/local результат до и после reopen;
- атомарность `ABORT`, item-level `SKIP`, обязательный reject limit и полный
  rollback при превышении;
- deferred DAG/Unique failure и full validation прежних данных;
- bounded violation details и сохранение totals;
- старый snapshot для readers и typed busy для concurrent writers;
- очистка orphan generation/sort session;
- failure injection на build/sync/catalog/WAL boundaries;
- одинаковые GrynQL/callback root, statistics и report counters;
- Primary/Standard/Unique/Count/Label/membership output и reopen;
- corruption handling для обязательного primary index;
- отсутствие роста WAL от числа segments.

Основные regression-наборы находятся в `src/datastore/import.rs`,
`src/gql/execution/tests/import.rs`, `src/storage/local_store/tests.rs` и
`tests/local_datastore.rs`.

## Измерения

### Среда и методика

- Windows 10 Home 23H2, build 22631.6199;
- Intel Core i5-12450H, 8 физических / 12 логических CPU;
- 16 874 201 088 bytes RAM;
- режим максимальной производительности, parking отключён, питание от сети;
- `rustc 1.98.1`, target `x86_64-pc-windows-msvc`, LLVM 22.1.8;
- paired порядок backends ротировался;
- profiling binaries собирались только с `--profile profiling`.

Для VTune использована точная пара:

```text
target/profiling/deps/import_stage_10_profile-80c00beb1a7a8ba5.exe
target/profiling/deps/import_stage_10_profile-80c00beb1a7a8ba5.pdb
```

Rust symbols и source lines разрешены (`DatabaseRoot::insert_atom`,
`build_generation_inner`, `external_sort.rs`). Неизвестные адреса системного
`ntoskrnl.exe` не являются ошибкой разрешения project PDB.

### Streaming output

Paired profile на 50 000 Standard records:

| Builder | Pages | Median |
|---|---:|---:|
| прежний buffered oracle | 3145 | 228,288 мс |
| streaming writer | 3149 | 184,999 мс |

Отношение streaming/buffered равно `0,810377`: новый writer быстрее на 18,96%.
Разное число физически корректных страниц связано с разным packing boundary;
полный scan даёт одинаковые записи. Устранено удержание 51 527 680 bytes
(49,14 МиБ) готовых output pages.

### End-to-end import

Результаты показывают отдельные medians режимов и paired отношение
`DEFERRED / MAINTAINED`, которое является основным сравнением.

| Workload | Runs на режим | Deferred median | Maintained median | Paired median | Paired range |
|---|---:|---:|---:|---:|---:|
| 20 000 items, segment 4096 | 15 | 333,618 мс | 329,780 мс | 1,0505 | 0,8917–1,1492 |
| 100 000 items, segment 16384 | 7 | 1076,877 мс | 1068,787 мс | 1,0168 | 0,9013–1,0843 |

Для 100 000 items median RSS delta составил 110 813 184 bytes у Deferred и
110 284 800 bytes у Maintained. Оба режима создали одинаковые 37 094 509 bytes
database files и 32 964 bytes WAL; семь internal segments не изменили объём WAL.
Deferred построил три secondary index families, median index timing —
172,102 мс против 172,989 мс общей index phase Maintained.

На малом 20k workload paired median имеет большой разброс и превышает 3%.
Нормативный gate относится к большим индексированным imports: на 100k Deferred
медленнее на 1,68%, то есть проходит предел 3%. Поэтому default остаётся
`INDEXES=DEFERRED`; parallel builder, отдельные lanes и tuning options не
добавляются без измеренного большого workload из `Q-33`.

### Устранённый bottleneck

До исправления вставка каждой записи клонировала COW maps и давала квадратичный
рост. На 20 000 items medians составляли 24,936 с для Deferred и 44,101 с для
Maintained. После перехода общей `DatabaseRoot` insertion boundary на
`Arc::make_mut` и удаления лишнего item snapshot в ABORT-mode получены сотни
миллисекунд вместо десятков секунд.

Старый VTune 10k profile занимал 4,699 с elapsed / 7,596 с CPU и показывал
`BTreeMap` drop/`Arc` clone как крупные hotspots. Финальный 100k capture занимает
1,199 с elapsed / 1,764 с CPU; эти квадратичные стеки исчезли. Крупнейший
project hotspot — hashing (`rapidhash` write, 0,128 с), затем `memcmp`
(0,077 с). `DatabaseRoot::insert_atom` занимает 0,014 с,
`build_generation_inner` — 0,012 с. Capture однопоточный; low physical-core
utilization ожидаема, а import parallelism намеренно не реализован до владельца
из `Q-33`.

## Verification gate

Пройдены:

- `cargo check --locked --lib --no-default-features`;
- тот же check с `--features native-async-io`;
- полный library test gate: 343 passed, 0 failed, 2 ignored;
- native-async library gate: 345 passed, 0 failed, 3 ignored;
- `tests/local_datastore.rs`: 10/10 без feature и 11/11 с native async;
- targeted import/differential/failure/recovery tests;
- nightly rustfmt для всего изменённого Stage-10 slice;
- focused Clippy для изменённых Stage-10 diagnostics: 0 matches;
- `graphify update .` через установленный `uv` launcher: 50 363 nodes,
  129 827 edges, 1766 communities.

Глобальные `cargo +nightly fmt --all -- --check` и `cargo clippy --lib` пока
блокируются посторонним dirty-tree formatting debt и 3297 накопленными
documentation/layout/API lint errors. Это не объявляется успешным gate и остаётся
открытой задачей `Q-14`.

## Артефакты

В `target/import-stage-10/` сохранены:

- `builder_results_profiling.csv`;
- `import_results_profiling-optimized-20260924.csv`;
- `import_results_profiling-100k-20260924.csv`;
- дооптимизационный `import_results_profiling-20260924-204504.csv`;
- `vtune_import_optimized_hotspots.csv`;
- `vtune_import_optimized_source_lines_top20.csv`;
- полные VTune result directories до и после исправления.

## Закрытие dependency-ordered шагов

| Шаг | Результат |
|---|---|
| I0 | Living docs и реестр синхронизированы. |
| I1 | Grammar, options, atomicity, graph scope, validation и report утверждены. |
| I2 | Private root, lease, segments, callback API и typed statuses реализованы. |
| I3 | RAM parser-to-execution vertical slice и differential tests реализованы. |
| I4 | Local unreachable generation и одна final publication реализованы. |
| I5 | Streaming B+tree/Label/membership output прошёл resource/performance gate. |
| I6 | Bounded property statistics переиспользует production external sort. |
| I7 | Deferred rebuild, Unique/family и full validation реализованы. |
| I8 | RAII/startup cleanup и crash-point tests реализованы. |
| I9 | RAM/local equality, reopen/recovery и все текущие DML проверены. |
| I10 | Correctness/concurrency/corruption matrix пройдена. |
| I11 | RSS/WAL/throughput/VTune gate пройден; Deferred оставлен default. |
| I12 | Отчёт создан; `Q-17`/`Q-18` закрыты, `Q-33` явно отложен. |

Следующий dependency-ordered этап — этап 11: явные `BEGIN/COMMIT/CANCEL` поверх
уже восстановленного обычного `Datastore::execute`. Control-flow внутри import
принадлежит этапу 12 и не реализован наперёд.
