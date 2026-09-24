# План доведения Ferros-Grynn до Windows GA-core и полной draft-conformance

## Результат

Работа делится на три рубежа:

1. **Windows GA-core**: стабильное локальное хранилище, schema/DDL/DML, импорт, транзакции, SELECT, MATCH, Traversal, графовые результаты, статистика, базовые индексы и оптимизатор.
2. **Полное соответствие draft-спецификациям**: оставшийся язык, Fulltext/HNSW/DiskANN/R-tree, графовые индексы, Changefeed, Live, Events и Bridges.
3. **Поддержка Linux**: Linux хранилище и специфичные оптимизации.

Нумерация этапов ниже топологически отсортирована: каждый этап расположен после всех указанных зависимостей. Работа ведётся вертикальными срезами `grammar → AST → analysis → IR → execution → storage → tests`, без создания каркасов «на будущее».

## Обязательный контекст реализации

Перед началом каждого этапа агент должен сверить выбранный срез со следующими источниками:

- корневым [`AGENTS.md`](../AGENTS.md) и ближайшим `AGENTS.md` затрагиваемого модуля — это актуальная карта владельцев кода, границ и локальных правил;
- [`codebase_analysis_stage_1_6.md`](reports/codebase_analysis_stage_1_6.md) — снимок текущей связности, зрелости и рисков кода и зависимостей;
- [`future_capability_analysis_stage_1_6.md`](reports/future_capability_analysis_stage_1_6.md) — каталог будущих областей применения существующего кода, подключённых крейтов и локальных reference-crates; раздел 6 задаёт карту глобальных модулей, а разделы 7–10 — варианты для последующих решений.

Отчёты пункта 1.6 являются навигационным и аналитическим контекстом, а не заменой спецификаций. При расхождении нормативный контракт задают утверждённые спецификации и актуальные пункты этого плана.

## Этап 1 — архитектурное разбиение production-кода

**Зависимости:** нет.

**Цель:** разделить крупные файлы в `src/` по владельцам логики, не меняя API, поведение, формат хранения, ошибки, порядок результатов или производительность. `fg-meta`, benchmark crate и proc-macro crate в этот этап не входят.

### 1.1. Зафиксировать сохраняемые контракты

Перед перемещением кода добавить или уточнить characterization-тесты для:

- публичных путей `datastore`, `gql`, `graph`, `values`, `gql_status`;
- RAM/local, Sync/NativeAsync и feature-комбинаций;
- кодов статусов, сериализации, WAL/page/catalog golden vectors;
- порядка строк, атомов, свойств и результатов запросов;
- reopen/recovery и основных SSI/FCW сценариев;
- контрольных storage/query benchmark smoke-профилей.

Если в модуле много общих типов, то они выносятся либо в `<module>/mod.rs`, либо в `<module>/types.rs` (в зависимости от количества).

Если в модуле много общих функций, которые нельзя отнести в конкретный подмодуль, 
то они выносятся либо в `<module>/mod.rs`, либо в `<module>/utils.rs` (в зависимости от количества).

Если в модуле много констант, то они выносятся в `<module>/consts.rs` и разделяются по общим признакам.

Если в модуле много тестов, то они выносятся в `<module>/tests.rs` и подключаются через `#[cfg(test)]`.

Переиспользуемую повторяющуюся логику (функции), разбросанную по разным модулям, необходимо вынести в отдельные утилитарные модули
(например, `unsafed`, `math` и прочие), при необходимости разрешено создавать новые модули. 

### 1.2. Разделить `datastore.rs` -> `datastore/**`

Текущий крупный модуль разложить на:

- `consts.rs`: константные настройки;
- `options.rs`: options for: I/O, checkpoint, datastore и transaction;
- `schema/mod.rs`: основная схема;
- `schema/table.rs`: исторические TABLE definitions; A5 этапа 8.1 заменяет их
  `schema/class.rs`;
- `schema/field.rs`: определения полей;
- `schema/index.rs`: определения индексов;
- `schema/graph.rs`: определения графов;
- `root.rs`: `DatabaseRoot`, версии, snapshots и каталожное состояние;
- `backend.rs`: выбор RAM/local и адаптация хранилища;
- `transaction/conflict.rs`: read/write sets, FCW, SSI и commit validation;
- `transaction/mutation.rs`: atom/graph mutation views после A6–A13;
- `transaction/query.rs`: `QuerySnapshot`, predicates и query adapter;
- `transaction/graph_view.rs`: transaction-bound `GraphRead`.
- `transaction/mod.rs`: `Transaction`.
- `mod.rs`: сам Datastore.

### 1.3. Разделить storage

Разбить только подтверждённые смешанные ответственности:

- `consts.rs`: константные настройки;
- `direct_io/mod.rs`: основной кроссплатформенный фасад Direct I/O;
- `direct_io/direct_windows.rs`: Windows-специфичный слой Direct I/O;
- `direct_io/direct_unix.rs`: Unix-специфичный слой Direct I/O;
- `graph_index/mod.rs`: публичный фасад и реэкспорт;
- `graph_index/label_dict.rs`: label dictionary;
- `graph_index/primary_idx.rs`: primary index;
- `graph_index/scalar_idx.rs`: scalar index;
- `graph_index/btree_idx.rs`: B+tree page codec;
- `graph_pages/hot.rs`: hot slots;
- `graph_pages/adjacency.rs`: adjacency;
- `graph_pages/props.rs`: properties/overflow;
- `graph_pages/freelist.rs`: freelist;
- `graph_pages/subgraphs.rs`: subgraphs;
- `graph_pages/statistics.rs`: statistics;
- `graph_pages/mod.rs`: фасад и прочие реализации;
- `page_io/mod.rs`: основной кроссплатформенный PageIO.
- `page_io/page_io_windows.rs`: Windows-специфичный слой PageIO.
- `page_io/page_io_unix.rs`: Unix-специфичный слой PageIO.
- `page_manager/sync_manager.rs`: SyncPageManager.
- `page_manager/mod.rs`: общие структуры и реэкспорт.
- `store/mod.rs`: фасад, open/load;
- `store/view.rs`: read views;
- `store/graph.rs`: graph image build/decode;
- `store/commit.rs`: commit/publication;
- `store/gc.rs`: startup GC;
- `wal/mod.rs`: фасад и реэкспорт;
- `wal/codec.rs`: record codec;
- `wal/state.rs`: published/version state;
- `wal/writer.rs`: writer/segments;
- `wal/checkpoint.rs`: checkpoint;
- `wal/recovery.rs`: recovery;
- `catalog/mod.rs`: file catalog;
- `catalog/definitions.rs`: database definitions;
- `catalog/codec.rs`: codec и validation;
- `buffer_pool/mod.rs`: BufferPull, cache facade;
- `buffer_pool/batch.rs`: batch loading/reservations;
- `buffer_pool/clock_pro.rs`: Clock-Pro policy.

registry, superblock и другие уже атомарные владельцы не перемещать без отдельной причины.
types при необходимости расширяется общими типами.

### 1.4. Разделить query frontend и execution

- `syn/stmt/**.rs`: отдельный `.rs` файл под каждый statement (MATCH, SELECT, CREATE, RELATE и так далее).
- `syn/token.rs`: реализация Token.
- `syn/lexer/keywords.rs`: все доступные ключевые слова GrynnQL.
- `syn/lexer/mod.rs`: Lexer (возможно logos).
- `syn/parser.rs`: token buffer/parser state.
- `syn/ast/**`: реализация AST.
- `gql/execution/adapter.rs`: transaction adapter.
- `gql/execution/cursor.rs`: cursor primitives, relational cursors.
- `gql/execution/eval.rs`: expression evaluator.
- `gql/execution/mod.rs`: parsing и executor.
- Сохранить один parser state и существующую цепочку `parse → analyze → IR → execute`.
- `analysis` и `ir` пока не дробить: текущий размер и ответственность этого не требуют.

### 1.5. Разделить values, graph algorithms и statuses

- `values/runtime/**`: kind, основной value contract, formatting и conversions.
- `values/storage/**`: tags/refs, DTO, contexts и public facade.
- Canonical codec: encode, decode и archive envelope.
- `graph` переносится на верхний уровень в `src/`.
- `graph/**`: все текущие модули графов, idx, path и topology остаются.
- `graph/algo/**`: все алгоритмы на графах.
- `graph/traversal/**`: index, connectivity, shortest paths, spanning structures, cuts и LCA.
- `graph/traversal/vm/mod.rs`: TraversalVM.
- `graph/traversal/vm/traverser.rs`: Traverser, sack, bulk, side effects.
- `graph/traversal/vm/pipeline.rs`: traversal pipeline.

Большой декларативный список `define_status_codes!` остаётся единым владельцем в `definitions`: его нельзя механически дробить по количеству строк.

### 1.6. Анализ кодовой базы

Результаты анализа хранятся в [`reports`](/D:/Programming/Rust/ferros-grynn/docs/reports) и должны отражать:

1. Создать `AGENTS.md` для каждого модуля верхнего уровня `src/`, описать его ответственность и подмодули и прилинковать к главному [`AGENTS.md`](/D:/Programming/Rust/ferros-grynn/AGENTS.md); файловый `src/sys.rs` описать в корневом документе.
2. Проанализировать КАЖДЫЙ установленные крейты в [`Cargo.toml`](/D:/Programming/Rust/ferros-grynn/Cargo.toml) и прочий код в `src/`, не относящийся к пунктам выше;
   отметить, что из установленных крейтов и кода может пригодиться в будущем и в какой области.
3. Рассмотреть возможность использовать крейта вроде [`logos`](https://crates.io/crates/logos) в качестве лексера 
   (пример из SurrealDB: [`SurrealDB Token`](/D:/Programming/Rust/ferros-grynn/crates/surrealdb/surrealdb/token)).
4. Рассмотреть возможность использования `VecMap` и `VecSet` из [`SurrealDB Collections`](/D:/Programming/Rust/ferros-grynn/crates/surrealdb/surrealdb/collections)
   или их модифицированных версий, использующих другие реализации динамических массивов (SmallVec, ThinVec и так далее).
5. Рассмотреть использование [`SurrealDB TopLevelExpression`](/D:/Programming/Rust/ferros-grynn/crates/surrealdb/surrealdb/core/src/sql/ast.rs) и 
   макросного подхода к [`SurrealDB AST`](/D:/Programming/Rust/ferros-grynn/crates/surrealdb/surrealdb/ast).
6. Рассмотреть использование макросного подхода к системе типов как [`SurrealValue`](/D:/Programming/Rust/ferros-grynn/crates/surrealdb/surrealdb/types/derive).
7. Что полезного можно позаимствовать из [`storekey`](/D:/Programming/Rust/ferros-grynn/crates/storekey)?
8. Что полезного можно позаимствовать из [`surrealmx`](/D:/Programming/Rust/ferros-grynn/crates/surrealmx)?
9. Что полезного можно позаимствовать из [`ferntree`](/D:/Programming/Rust/ferros-grynn/crates/ferntree)?

Результат анализа закреплён двумя взаимодополняющими документами:

- [`codebase_analysis_stage_1_6.md`](reports/codebase_analysis_stage_1_6.md) фиксирует фактическое состояние, связность и непосредственные риски;
- [`future_capability_analysis_stage_1_6.md`](reports/future_capability_analysis_stage_1_6.md) даёт прогноз областей применения и набор вариантов, которые необходимо повторно оценивать в контексте конкретного этапа.

### 1.7. Правила выполнения и критерии готовности

- Сначала перемещение и реэкспорты, затем проверка, и только отдельным изменением — сужение visibility или удаление дублей.
- Не добавлять неоправданные crates, зависимости, traits с одной реализацией и пустые расширительные точки.
- Использование любого крейта — уже подключённого, локального reference-crate или нового — должно быть обосновано требованиями и сравнением с ручной реализацией. Ручная реализация требует такого же обоснования; итоговый выбор согласуется с выводом `rust-workflow` для текущего среза.
- Перед использованием кода из непроверенных модулей (`asynchronous`, частей `collections`, `unsafed` и других отмеченных в отчётах областей) провести предметный аудит и довести выбранный код до production-состояния: подтвердить API и ошибки, safety/concurrency/platform-инварианты, необходимые тесты и включение в обычные проверки проекта.
- Использовать принятую в проекте схему каталогов с `mod.rs`.
- Не должно остаться рукописных production-файлов свыше 2000 строк; файл свыше 1000 строк допустим только при одной явно сформулированной ответственности.
- После каждого подэтапа должны проходить соответствующие `cargo check` и targeted tests.
- Финальный gate: обе I/O feature-конфигурации, library tests, rustfmt check, changed-surface Clippy, benchmark smoke и обновлённый Graphify.
- Новую фактическую структуру записать в раздел модулей [implementation_plan.md](/D:/Programming/Rust/ferros-grynn/docs/implementation_plan.md), не перенося туда нормативные byte layouts.
- Готовый отчёт пункта 1.6 отвечает на все вопросы и находится в [`reports`](/D:/Programming/Rust/ferros-grynn/docs/reports).

**Статус (отмечен 2026-09-05): этап 1 завершён.** Production-модули разделены
по владельцам, структура и правила закреплены в `AGENTS.md` и
`implementation_plan.md`, анализ 1.6 опубликован в двух связанных отчётах.
Последующий рост и дублирование кода отслеживаются отдельно записью `Q-14`, а не
переоткрывают исторический refactor-срез.

## Топологический порядок дальнейших этапов

### Рубеж A — Windows GA-core

2. **Повторная синхронизация спецификаций** — зависит от **1**.  
   Установить приоритет: `disk_storage_spec` задаёт байтовый формат и durability, `gql_spec` — семантику языка, `gql_grammar` — точный синтаксис, `gql_tech_spec` — реализацию. Устранить расхождения DiskANN, ALTER, NativeAsync, format-v1 gate и статусов реализованности. В `implementation_plan` добавить conformance-матрицу и получить утверждение GrynQL-срезов до их реализации.

   **Статус (отмечен 2026-09-05): этап 2 завершён.** Приоритет нормативных
   источников и conformance-матрица закреплены в `implementation_plan.md`;
   утверждённый GrynQL P0–P2 отделён от будущих draft-срезов. Новые расхождения
   после этого среза регистрируются в этапе 4.1.

3. **Закрытие Windows sync performance gate** — зависит от **1, 2**.  
   Воспроизвести регрессии sparse miss 77,5% и scan resistance 88,2%, снять ETW/flamegraph и исправить измеренный общий источник. 
   Gate: не менее 90% raw Direct throughput, требуемые p99/hot-set/RSS показатели и отсутствие регрессии NativeAsync.
   Необходимо произвести калибровки, профилирование и оптимизацию Direct I/O, BufferPool и других связанных механизмов.
   Задача обогнать или хотя бы приблизиться к mmap, хорошим результатом будет 95/110, при этом hot-set и RSS тоже должны быть на высоком уровне, а отставание от raw Direct - минимальным.
   Переписывать Clock-Pro разрешается только при доказанной причине.
   Замеры имеет смысл производить на файлах разных размеров. Поскольку одна страница - это 16384 байта, то есть 16 килобайт, лучше разделять тестовые файлы по количеству страниц. Например: 1, 10, 50, 130, 300, 1000, 10000, 100000, 300000 страниц.
   Для профилирования по необходимости использовать все доступные инструменты (+ навык профилирования из `rust-engineering` и профиль сборки `profiling`):
   - `allocation-tracking` feature для трекинга аллокации (см. `src/memory`),
   - `cargo flamegraph`,
   - `mimalloc-pprof` (с `pprof` feature или runtime переменной, смотри документацию),
   - `Tracy Profiler` (исполняемые файлы Tracy доступны через `PATH` или по пути `D:/Programming/Tools/tracy-0.14.1`),
   - `Intel VTune Profiler` (через скрипт `D:/Programming/Tools/Intel/oneAPI/setvars.bat`; тут важно, что `.bat` работает через `cmd`, поэтому, если будешь открывать его через `PowerShell`, используй скрипт: `C:\Program Files\PowerShell\7\pwsh.exe -NoExit -Command "& { cmd /c '""D:\Programming\Tools\Intel\oneAPI\setvars.bat"" intel64 vs2022 > nul && set' | ForEach-Object { $n, $v = $_ -split '=', 2; Set-Item -Path Env:$n -Value $v } }"`),
   - `shuttle`,
   - `criterion`,
   - `tracing`,
   - `tokio-metrics`, `tokio-blocked`,
   - `sanitizers`,
   - `Windows Performance Recorder` + `Windows Performance Analyzer`.

   **Статус (2026-08-31): Windows sync gate закрыт.** Исправлена методика
   сравнения на одном immutable-файле с парными repetition; нормативный gate,
   bounded RSS и NativeAsync gate пройдены. Clock-Pro не переписывался, так как
   профили не подтвердили его причиной прежней регрессии. Результаты, сравнение
   трёх Direct-путей и mmap, разбор RSS/resident hit и процедура калибровки
   многфайлового `buffer_pool_pages` зафиксированы в
   [`disk_storage_stage3_report.md`](reports/disk_storage_stage3_report.md).

   **Уточнение (2026-09-01):** статус 2026-08-31 является историческим
   результатом прежней методики. Повтор baseline на текущем checkout не
   воспроизвёл performance/RSS gate; до production-интеграции он считается
   непрошедшим и должен быть повторно подтверждён.

   **Финальный статус (2026-09-04): этап 3 завершён через этапы 3.1–3.2.**
   Повторная экспериментальная кампания выбрала гибридную архитектуру, а
   production cutover повторно прошёл correctness и обязательные performance
   floors. Актуальное evidence находится в отчёте этапа 3.2; историческое
   уточнение выше сохранено как описание причины повторного исследования.

3.1. **Прототипирование нового storage engine и BufferPool V2** — зависит от
	**3**. Этап остаётся test-only и сам по себе не закрывает текущий
	неподтверждённый production performance/RSS gate. Варианты Atomic
   Arena, resolved handles, Windows VM cache, scan admission policies,
   `ReadFileScatter`, frame-oriented IoRing и bounded prefetch реализуются и
   сравниваются до выбора production-архитектуры. Подробный порядок,
   correctness gate и измерительная матрица зафиксированы в
   [`storage_engine_prototype_plan.md`](storage_engine_prototype_plan.md).

   **Статус (2026-09-01): этап 3.1 завершён как test-only prototype
   campaign.** Результаты находятся в
   [`storage_engine_prototype_report.md`](reports/storage_engine_prototype_report.md),
   компактные данные — в
   [`storage_engine_prototype_results.csv`](reports/storage_engine_prototype_results.csv),
   сырые логи и profiler artifacts — в `target/storage-engine-prototype/`.
   Ни один resident candidate не достиг минимального `90% mmap` floor,
   поэтому production path не изменён, а полный size sweep финалистов условно
   пропущен.

   **Продолжение (2026-09-01):** экспериментальная кампания возобновлена для
   dense locator, batch/run ownership и больших mmap-наборов с page faults.
   Прототипы перенесены из production source tree в отдельный workspace-крейт
   `crates/fg-storage-engine-prototypes`; основной крейт предоставляет ему
   только временный feature-gated `storage-engine-prototype-api`. Все следующие
   tests, benchmarks и profiler runs выполняются из нового крейта, production
   path остаётся неизменным.

   **Промежуточный результат (2026-09-02):** source-bound dense locator с
   одним pin на 64 логических чтения страницы достиг median `93,3%` mmap и
   p99 `10 ns` против `9 ns`, но per-access путь остался на `18,7%`; это
   подтверждает необходимость cursor/run ownership, а не замены hash table
   самой по себе. На fully validated scan файла `18,31 GiB` (`1,165x` RAM)
   sync Direct сохранил `1936 MiB/s`, тогда как mmap снизился до `606 MiB/s`
   при `4 809 466` process page faults. Production path не менялся;
   дальнейший порядок — epoch-scoped borrowed read для point/traversal,
   перенос waiter bookkeeping с resident release path и oversubscribed
   сравнение Direct/scatter/prefetch — зафиксирован в итоговом разделе отчёта.

   **Итог продолжения (2026-09-02):** dependency-ordered эксперимент завершён.
   Cursor-scoped `R8` read epoch достиг `97,4–105,1%` mmap в изолированном
   resident-тесте; в общем бюджете для `1/4/8` файлов — `93,9/94,5/100,5%`
   paired median при разнице p99 не более `12 ns`. Для B+tree leaf scan
   комбинация `R8 + Rocks-like bypass + ReadFileScatter + P1` достигла
   `185,4%` mmap, а для заранее известного sparse frontier комбинация
   `R8 + frame-IoRing QD32` — `457,6–506,3%` mmap. ETW подтвердил, что mmap
   большого файла создаёт четыре 4-КиБ fault на одну страницу БД; `99,26784%`
   из них были transition faults. Выбранная Pareto-архитектура и
   dependency-ordered production migration plan записаны в итоговом разделе
   отчёта. Production path по-прежнему не изменён.

3.2. **Внедрение гибридного storage engine** — зависит от **3.1**.
   Перенести в production подтверждённую Pareto-комбинацию: R8 atomic arena
   для resident cursor windows, Rocks-like bypass + `ReadFileScatter` + P1
   для последовательных ranges, frame-IoRing QD32 для заранее известного
   sparse frontier и sync validated Direct для непредсказуемых miss.
   Сохранить page/WAL/checkpoint contract и режимы Full In Memory, Full On Disk
   и Query To Memory. После correctness/performance gates выполнить единый
   cutover без постоянного runtime-переключателя и удалить старый Clock-Pro.
   Подробная архитектура, порядок, gates и влияние на режимы зафиксированы в
   [`storage_engine_hybrid_integration_plan.md`](storage_engine_hybrid_integration_plan.md).

   **Статус (2026-09-04): H0-H13 выполнены, production cutover завершён.**
   `SyncPageManager` использует единый `HybridReadEngine`; старый Clock-Pro
   исключён из production-сборки и сохранён только как test/prototype control.
   Query path стал streaming, Mode 3 получает owned двухпроходную
   materialization. Correctness и обязательные performance floors пройдены;
   точные результаты и ограничения находятся в
   [`storage_engine_hybrid_integration_report.md`](reports/storage_engine_hybrid_integration_report.md),
   компактные данные — в
   [`storage_engine_hybrid_integration_results.csv`](reports/storage_engine_hybrid_integration_results.csv).
   Known-sparse IoRing transport готов и проверен, но будет выбран production
   router только после того, как физический graph/property cursor предоставит
   доказуемое bounded lookahead-окно. Низкоуровневый H14 write-back для
   graph-local topology/overlay реализован этапом 8.1; самостоятельная операция,
   которая открывает транзакцию, проверяет изменения после origin snapshot и
   явно выполняет commit, остаётся задачей `Q-09`.

4. **Delta-oriented mutation foundation** — зависит от **3**.  
   Убрать полную материализацию local-графа из обычного `edit_graph`; формировать atom/page delta непосредственно из point mutations. Учесть свойства, крупную adjacency, составные `RecordId` и nested graph payload в write-set limit. После профиля сохранить лимит 256 МиБ; private spill реализовывать только при подтверждённом неделимом workload, который нельзя обработать сегментированным импортом.

   **Статус (2026-09-05): этап 4 завершён.** `mutate_graph` выполняет bounded
   atom/page delta без скрытой материализации; полная загрузка вынесена в
   `edit_graph_materialized`, а совместимый `edit_graph` помечен устаревшим.
   Реализованы все поддерживаемые node/edge point operations, рекурсивный
   write-set accounting и узкая COW-публикация dynamic `GraphSuperblock`.
   Scalar-index maintenance и segmented import остаются в назначенных этапах 7–10.

4.1. **Аудит реализованных допущений, сквозных проблем и дублирования** —
   зависит от **2, 3.2, 4**.
   До расширения schema/DML зафиксировать проверяемый реестр проблем уже
   реализованных поверхностей. Это не перечень ещё не реализованных возможностей
   этапов 5–33: сюда входят только временные допуски, расхождения спецификаций и
   кода, скрытые fallback, измеренные потолки и пользовательские ловушки.

   Для каждой записи указать: наблюдаемое поведение и источник доказательства,
   затронутый нормативный контракт, влияние на корректность/производительность/UX,
   минимальный воспроизводящий test или benchmark, выбранное решение, файлы
   спецификации и кода, этап-владелец, severity, trigger пересмотра и статус.
   Если желаемое поведение ещё не утверждено, сначала изменить соответствующую
   спецификацию; текущее устройство кода само по себе не становится контрактом.
   Допуск без владельца, срока или измеримого trigger считается открытой проблемой.

   **Статус (2026-09-24):** graph-first cutover A5–A16 и Atomic Import I0–I12
   завершены; `Q-08`, `Q-17`–`Q-18` и `Q-23`–`Q-29` закрыты текущими
   RAM/local/recovery/resource/performance gates. `Q-01`–`Q-03`,
   `Q-15`–`Q-16` и `Q-21` остаются закрытыми прежними этапами. `Q-04`–`Q-07`,
   `Q-09`–`Q-14`, `Q-20`, `Q-22` и `Q-30`–`Q-33` остаются открытыми либо
   явно отложенными до назначенных зависимых этапов.

   **Известный реестр на текущем checkout:**

   | ID      | Класс                                   | Текущий допуск, риск или закрытое решение                                                                                                                                                                                                                                                                                                                                                                                                                                      | Обязательное решение / evidence                                                                                                                                                                                                                                                                                                                                                                               | Статус                                                                          |
   |---------|-----------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------|
   | `Q-01`  | Correctness + performance               | Point commit больше не читает и не переписывает полное поколение графа: он публикует изменённые data pages, полное текущее primary-index image и одну новую страницу `GraphSuperblock`, сохраняя `DataFileId` остальных файлов. Commit и checkpoint сериализованы одним catalog lock; checkpoint-owned `generation`, `backfilled_lsn` и `checkpoint_record_lsn` берутся из текущей authority `PageManager`, поэтому stale transaction descriptor не может откатить checkpoint. | Контракт закреплён в `disk_storage_spec` §17. Regression: `local_node_append_stays_delta_backed_until_commit`, `local_disjoint_point_mutations_merge_and_rebase_revision`, checkpoint/reopen tests. Полная перестройка primary index остаётся отдельной performance-задачей `Q-05`, но прежней `O(size of graph generation)` публикации point commit больше нет.                                              | ✅ закрыто — этап 4, 2026-09-05                                                 |
   | `Q-02`  | Functionality + UX                      | `mutate_graph` является delta-only contract для поддерживаемых create/update/delete node/edge, labels, properties, incidence и каскадного удаления nested/cross-level зависимостей. Полная family materialization доступна только через явный `edit_graph_materialized`; `edit_graph` — deprecated compatibility alias. Scalar-index graph получает явную ошибку до мутации вместо скрытого fallback. DAG/Forest revalidate объединённую topology под commit lock.             | Проверено point/reopen, large property/adjacency overflow, RecordId reuse, многостраничным subgraph directory, metagraph cascade и disjoint DAG commit regressions. Ограничения family-specific Tree/RootedTree операций и scalar-index maintenance остаются явными и принадлежат назначенным будущим этапам.                                                                                                 | ✅ закрыто — этап 4, 2026-09-05                                                 |
   | `Q-03`  | Resource correctness + UX               | Лимит 256 МиБ нормативно остаётся консервативной логической precommit-оценкой, а не allocator quota. Единый рекурсивный accounting теперь включает mutation state, составные `RecordId`, properties, текущую и предыдущую topology, полностью выведенную adjacency затронутых узлов и nested graph payload; `FG-54F07` возвращается до commit lock, WAL, I/O и публикации.                                                                                                     | Контракт и момент отказа закреплены в `gql_spec`. Regression: `write_set_limit_counts_recursive_graph_payload_before_publication` и `local_write_set_limit_counts_derived_adjacency_before_publication`. Bounded segmented import остаётся самостоятельным этапом 10; private spill всё ещё запрещён без доказанного неделимого workload.                                                                     | ✅ закрыто — этап 4, 2026-09-05                                                 |
   | `Q-04`  | Durability + resources                  | Недостижимые COW-поколения удаляет startup-GC. Частые COW commit между reopen могут накапливать disk bytes и file registrations, тогда как активные snapshots требуют безопасно удерживать старую версию.                                                                                                                                                                                                                                                                      | До этапа 19 измерить длительный churn и реализовать snapshot-aware runtime retirement/GC либо нормативно доказать жёсткую границу; crash-orphans и ошибки удаления должны иметь отдельные тесты и наблюдаемый статус.                                                                                                                                                                                         | ⬜ открыто                                                                      |
   | `Q-05`  | Performance                             | Point-DML сначала обновляет только затронутые B+tree leaves, если сохраняются separator, page count и posting layout; иначе использует проверенный bulk fallback. Immutable external-sort generation остаётся плотным, mutable primary/Unique собираются с fill 0.90, Standard — 0.80.                                                                                                                                                                                         | Paired profiling на 50 000 entries и batch 1/32/256 показал для primary fill 0.90 отношения incremental/bulk 0.013/0.198/0.454, для Standard fill 0.80 — 0.005/0.110/0.127. Dense primary/Standard batch 256 подтвердили необходимость bulk fallback. Unique validation, one-commit WAL и reopen regression пройдены.                                                                                         | ✅ закрыто — этап 9, 2026-09-09                                                 |
   | `Q-06`  | Correctness-preserving UX + concurrency | Serializable validation намеренно abort-ит любую исходящую rw-антизависимость; это корректно, но может давать лишние `FG-40001`. Blind-write `GraphWriteView` требует отдельного предварительного tracked read, что легко пропустить пользователю.                                                                                                                                                                                                                             | До этапов 11/19 измерить abort rate, явно документировать read-before-write либо ввести tracked guard; dangerous-structure history добавлять только при доказанном выигрыше.                                                                                                                                                                                                                                  | ⬜ открыто                                                                      |
   | `Q-07`  | Transaction semantics                   | `ReadCommitted` пока фиксирует один root на весь переданный `Program`, а refresh между top-level statements и GrynQL transaction statements отсутствуют. Анализатор также типизирует весь `Program` по начальному schema snapshot: DDL текущей программы ещё не видна последующему DML/SELECT. При явном `Transaction::execute` ошибка отдельного statement возвращается в `QueryResult`, но контракт abort/savepoint для уже staged изменений не утверждён.                   | Этап 11 обязан синхронизировать observable statement boundary, staged-schema visibility и поведение statement error/abort в `gql_spec`, Rust API и differential tests до объявления RC/TX surface завершённым. До этого автотранзакция атомарно не публикует программу с ошибкой, а смешанный DDL→DML внутри одного `Program` не объявляется поддержанным.                                                    | ⬜ открыто                                                                      |
   | `Q-08`  | Functionality + data model              | Этап 5 реализовал generation-aware `AtomTableMemberships`, но они дублировали class namespace из `RecordId = class:id` и делали class принадлежностью, а не identity. Историческая реализация удалена полностью.                                                                                                                                                                                                                                                               | A5–A16 ввели `ClassId`/`ClassSchema`, class-aware `RecordId`, global AtomStore и graph occurrence membership. `TableSchema`, `AtomTableMemberships`, system graph и compatibility selector отсутствуют; RAM/local/recovery и DDL/DML gates пройдены.                                                                                                                                                          | ✅ закрыто — этап 8.1, 2026-09-22                                               |
   | `Q-09`  | Functionality + UX                      | Mode 3 `Mutable` уже хранит origin/dirty state и явно передаёт только dirty graph-local topology/overlay в предоставленную вызывающим кодом транзакцию. Ошибка writer сохраняет dirty state, `Drop` не пишет. Пока отсутствует самостоятельная source-level операция, которая открывает новую транзакцию, обнаруживает изменение source после materialization origin и сама выполняет commit.                                                                                  | До публикации высокоуровневого mutable write-back surface добавить origin-aware conflict check и explicit commit поверх существующего delta writer. Конфликт или ошибка commit обязаны оставлять dirty state доступным для повторного решения; неявное изменение Base и flush из `Drop` запрещены.                                                                                                            | ⬜ открыто — low-level delta writer готов, source transaction/commit UX отложен |
   | `Q-10`  | Performance                             | Frame-IoRing QD32 измеримо выигрывает на sparse frontier, но production graph/property cursors пока не предоставляют доказуемое bounded lookahead-окно, поэтому router выбирает sync Direct.                                                                                                                                                                                                                                                                                   | На этапах 16–17 добавить окно только из реального physical cursor и повторить paired gate; эвристическое угадывание и пользовательский transport knob не добавлять.                                                                                                                                                                                                                                           | ⬜ открыто                                                                      |
   | `Q-11`  | Performance + operability               | Остались измеренные границы: generic R8 hit около 74,9 нс против stretch 70 нс, один global read-epoch counter, единичный fixed-hot outlier до 65,87% raw и ручной общий `buffer_pool_pages=4096` без auto-sizing/per-file fairness.                                                                                                                                                                                                                                           | До этапа 19 выполнить long-run/multi-file/concurrency sweep, вывести effective resident/transient/metadata budget и cache pressure. Directory/epoch/sharding/auto-sizing менять только по профилю representative workload.                                                                                                                                                                                    | ⬜ открыто                                                                      |
   | `Q-12`  | Scalability                             | Известны пока не доминирующие потолки: `File::open` на WAL-frame miss, один mutex IoRing, `O(graphs + classes)` clone каталога, линейный changed-graph lookup и `O(V+E)` copy при `LiveGraph` update/conversion.                                                                                                                                                                                                                                                               | Каждый владелец-потребитель обязан измерить свой потолок на соответствующем этапе; оптимизация выполняется только при заметной доле профиля, а не по одному наличию комментария в коде.                                                                                                                                                                                                                       | ⬜ открыто                                                                      |
   | `Q-13`  | UX + configuration                      | Источники конфигурации расходятся: спецификация называет `FG_PAGE_BUFFER_PAGES`, production API использует `LocalOptions::buffer_pool_pages`, allocation tracking читает process-wide `SURREAL_MEMORY_THRESHOLD`; эффективные budget/fallback пользователю не видны.                                                                                                                                                                                                           | До этапов 19–20 определить одну embeddable configuration authority, единицы и precedence, убрать чужой prefix, добавить validation и read-only introspection эффективных значений/pressure без глобальной env-зависимости.                                                                                                                                                                                    | ⬜ открыто                                                                      |
   | `Q-14`  | Maintainability + release               | Полный Clippy на текущем checkout блокируется 3297 накопленными documentation/layout/API lint errors (повторный прогон 2026-09-24); global rustfmt также видит посторонний dirty-tree drift. Изменённый срез этапа 10 проходит отдельные nightly rustfmt и focused Clippy, но это не подменяет workspace gate. Внутренние `Option<Option<T>>` accessors не объясняют две семантические оси; скрытый prototype measurement API остаётся в repository.                           | До этапа 19 документировать либо упростить двойные `Option`, погасить или узко обосновать lint debt, синхронизировать workspace formatting и удалить временные hooks, когда внешний prototype-крейт перестанет быть обязательным oracle.                                                                                                                                                                      | ⬜ открыто                                                                      |
   | `Q-15`  | Format + correctness + UX               | Первоначальная stage-6 реализация объединила физически разные graph/label/property statistics в один файл и тем самым связала их startup/read lifecycle. Одновременно прежний §19 содержал lossy `avg_prop_count(f32)`, неразбираемые variable-width bucket bounds, несуществующие sampled/auto `ANALYZE` и неверную оценку «64-byte HLL ≈ 2%».                                                                                                                                | §19 и format-v1 синхронизированы на трёх checksummed файлах: eager root в `graph_stats.fgb` и независимо lazy degree/label/property parts. Сохранены exact integer totals/`property_count`, явные длины bucket bounds, exact full-scan NDV, revision/staleness markers и optional-histogram isolation. Codec и reopen regressions проверяют directory, физическое разделение и deferred auxiliary corruption. | ✅ закрыто — этап 6, 2026-09-07                                                 |
   | `Q-16`  | Durable correctness                     | В stage-6 срезе обнаружены два пути повреждения/отказа: один target `EncodeContext` не мог декодировать старое свойство после переиспользования `RecordId`, а перемещение уже заполненного over-aligned 16 KiB statistics root в `Vec` на Windows MSVC наблюдаемо превращало magic `FGST` в `\0GST`.                                                                                                                                                                           | Для delta разделены source decode context и target encode context. Statistics root инициализируется непосредственно в окончательном слоте `Vec`, без перемещения заполненной страницы. Regression: `local_point_mutation_reuses_removed_record_id`; полный однопоточный `datastore::tests` прошёл 10 последовательных запусков по 40 тестов после исправления.                                                | ✅ закрыто — этап 6, 2026-09-07                                                 |
   | `Q-17`  | Performance + resources                 | Atomic Import больше не удерживает и не сортирует все encoded property values в памяти: histogram collection использует тот же bounded external-sort run/merge с фиксированным бюджетом 256 МиБ. Import-specific statistics output пишется в недостижимое поколение до единственной публикации.                                                                                                                                                                                | Correctness/reopen gates и 100k RSS profile пройдены; семь segments не увеличивают WAL. Измерения и границы приведены в `reports/import_stage_10_report.md`. Возможную оптимизацию обычного point-DML `label_stats.fgb` рассматривать на этапе 19 только при заметной доле commit profile, без возврата unbounded bulk path.                                                                                  | ✅ закрыто — этап 10, 2026-09-24                                                |
   | `Q-18`  | Resource scalability                    | Primary/Standard/Unique/Label и membership builders теперь передают готовые 16 KiB pages прямо в недостижимый side-file; posting continuations и Label groups имеют bounded state. `Vec<AlignedPage>` output и `O(index pages + labeled atoms)` retention устранены, финальные over-aligned pages кодируются в стабильных heap slots.                                                                                                                                          | Paired 50k profile: streaming writer удерживает на 51 527 680 bytes меньше готового output и имеет отношение времени 0,810377 к прежнему buffered oracle. Scan equality, local reopen/recovery и corruption gates пройдены; evidence — `reports/import_stage_10_report.md`.                                                                                                                                   | ✅ закрыто — этап 10, 2026-09-24                                                |
   | `Q-19`  | UX + operability                        | Secondary index теперь имеет persisted `ready/stale/prepared_for_removal` state. Missing, unopenable или checksum-corrupt secondary при reopen сохраняет definition, становится `stale`, исключается из physical registry и наблюдаем через `INFO`; query использует scan. Primary/base corruption остаётся фатальной.                                                                                                                                                         | Контракт закреплён в `gql_spec` и `disk_storage_spec`, `FGDC v5`; regressions проверяют unavailable/corrupt secondary, INFO, scan fallback и fatal primary corruption. Детальная причина fallback в `EXPLAIN` остаётся работой этапа 14, но silent-state проблема реестра закрыта.                                                                                                                            | ✅ закрыто — этап 8, 2026-09-09                                                 |
   | `Q-20`  | Format + performance                    | Core B+tree хранит полные order-preserving keys. Immutable bulk pages заполняются до capacity; измеренная mutable writer policy использует 0.90 для primary/Unique и 0.80 для Standard без нового format discriminator. Draft prefix/block compression по-прежнему не имеет layout discriminator или измеренного выигрыша.                                                                                                                                                     | До этапа 19 на representative string/composite indexes измерить size/CPU/page count. Затем либо ввести один однозначный compressed layout с golden vectors до format-v1 freeze, либо удалить недоказанный compression candidate. Изменять утверждённый mutable fill только по новому paired benchmark с recovery/unique gate.                                                                                 | ⬜ открыто                                                                      |
   | `Q-21`  | Format + correctness + UX               | Stage-7 код разошёлся с нормативным layout: `GraphSuperblock` назывался `graph.fgb`, шесть freelist были объединены, а Standard/Unique получили общий filename `scalar_*`; schema и physical identities также оставались одним нетипизированным `u32`.                                                                                                                                                                                                                         | В correction C0–C6 закреплены `superblock.fgb`, шесть `freelist_*.fgb` с `FGFC v4.companion_of`, `std_/uniq_` по безопасному ordinal и публичный `IndexId`; catalog/index/datastore/recovery gate и correction appendix пройдены.                                                                                                                                                                             | ✅ закрыто — этап 7, 2026-09-08                                                 |
   | `Q-22`  | Performance + UX                        | Форматно корректное разделение freelists усилило уже существующий потолок eager `FileRegistry`: reopen открывает все generation side-files и проверяет шесть freelist roots на граф. Диагностика дала median 11,397 ms для 1 графа и 626,703 ms для 100 графов; это абсолютные warm-значения, не paired-регрессия относительно неподдерживаемого FGFC v3.                                                                                                                      | До этапа 19 измерить долю open/validation отдельно и реализовать lazy handle opening, если она остаётся значимой на production-profile и representative multi-graph базе. Сохранять exact `DataFileId`, typed errors и fatal primary/base semantics; не объединять freelists обратно.                                                                                                                         | ⬜ открыто                                                                      |
   | `Q-23`  | Format + correctness + UX               | `GraphId=0` и system-table/system-graph path смешивали database-level данные с graph identity; прямой row source был пуст. Этот контур удалён.                                                                                                                                                                                                                                                                                                                                 | `DataSpaceId::Database`, global AtomStore и `AtomStoreSuperblock` прошли byte/recovery и RAM/local differential gate. `GraphId` всегда ненулевой; direct class read не требует graph.                                                                                                                                                                                                                         | ✅ закрыто — этап 8.1, 2026-09-22                                               |
   | `Q-24`  | Language + identity + correctness       | Исторический `RELATE [alias:label]` не задавал class и `RecordId` edge и не мог однозначно выбрать identity, schema и indexes.                                                                                                                                                                                                                                                                                                                                                 | Реализованы class/id/`AS`/`LABEL` и обязательный `IN GRAPH`: edge class фиксирует kind, ID — global identity, alias и graph-local label независимы. Family, multi-graph identity, rollback и reopen tests пройдены; class из label не выводится.                                                                                                                                                              | ✅ закрыто — этап 8.1, 2026-09-22                                               |
   | `Q-25`  | Data model + storage correctness        | Прежний `StoredGraph` владел полными копиями атомов и свойств. Теперь один `RecordId` имеет один global payload, а graphs владеют только occurrences.                                                                                                                                                                                                                                                                                                                          | Global AtomStore, reverse memberships и sparse overlays прошли shared-base/isolated-overlay, multi-graph edge, graph-removal, local reopen и checkpoint recovery tests. Unattached atom читается напрямую.                                                                                                                                                                                                    | ✅ закрыто — этап 8.1, 2026-09-22                                               |
   | `Q-26`  | Correctness + performance + UX          | Base mutation затрагивает graph views и effective indexes, поэтому unbounded fanout, частичный `CASCADE` и пропуск Unique recheck были недопустимы.                                                                                                                                                                                                                                                                                                                            | Реализованы reverse-membership fanout, полный preflight до private mutation, write-set limit, `SCOPE BASE` / `SCOPE EFFECTIVE IN GRAPH`, повторная проверка Effective Unique и `RESTRICT` по умолчанию. Atomicity tests и fanout benchmark `1/8/32` memberships подтверждают стоимость `O(memberships изменяемого atom)`.                                                                                     | ✅ закрыто — этап 8.1, 2026-09-22                                               |
   | `Q-27`  | Durable correctness                     | На Windows MSVC перемещение уже заполненной over-aligned 16 KiB страницы иногда повреждало первый byte magic/checksum до Direct I/O. Проблема проявлялась для Label index, statistics roots и combined AtomStore/Graph superblocks.                                                                                                                                                                                                                                            | Builders кодируют populated pages в окончательном `Vec`-slot/arena либо держат долгоживущие metadata pages в стабильном `Box`. Последний superblock call site локализован до первого catalog I/O; exact regression прошла 100/100 отдельных процессов, затем полную native-async матрицу. Page codec не изменён.                                                                                              | ✅ закрыто — этап 8.1, 2026-09-22                                               |
   | `Q-28`  | Format + correctness                    | Target offsets обоих superblocks, property keys, graph labels/timezones и overlay authority ранее не были согласованы.                                                                                                                                                                                                                                                                                                                                                         | S1 layouts, `FGDC v7`, разделённые dictionaries и overlay codec реализованы в A8–A10; golden bytes, reopen, sparse overlay и A15 recovery gates пройдены.                                                                                                                                                                                                                                                     | ✅ закрыто — этап 8.1, 2026-09-22                                               |
   | `Q-29`  | Durability                              | Per-commit metadata COW нарушал one-WAL-barrier, superblock `page_lsn` смешивался с `CheckpointEnd`, recovery продвигал `.tmp`, а partial checkpoint не сохранял previous candidate.                                                                                                                                                                                                                                                                                           | Logical superblocks публикуются как PageFrame одной WAL transaction; checkpoint COW использует `.tmp/main/.prev` и заменяет `db.fgb` последним. AtomStore+graph checkpoint/recovery, truncation/corruption и native-async gates пройдены.                                                                                                                                                                     | ✅ закрыто — этап 8.1, 2026-09-22                                               |
   | `Q-30`  | Functionality + resources + UX          | `ANALYZE` не имел синтаксиса/детерминизма, 64-byte HLL ошибочно обещал около 2%, sampling и auto policy не имели bounds.                                                                                                                                                                                                                                                                                                                                                       | S3–S4 закрепили FULL/APPROXIMATE, HLL p=12, reservoir 1024..=1,000,000, default threshold 32 pages, job/revision semantics и auto threshold. Реализация и correctness/resource gates принадлежат этапам 20/optimizer.                                                                                                                                                                                         | 🟡 contract approved; implementation deferred                                   |
   | `Q-31a` | Vector-index correctness + durability   | DiskANN/HNSW не имели согласованного side-file/delta recovery, а `HASHED_VECTOR` был undefined.                                                                                                                                                                                                                                                                                                                                                                                | S3 утвердил separate base formats, common exact delta, fingerprints, rebuild threshold и collision-checked HASHED_VECTOR; этап 23 реализует recall/latency/recovery gates.                                                                                                                                                                                                                                    | 🟡 contract approved; implementation deferred to 23                             |
   | `Q-31b` | Graph-index correctness + performance   | Reachability/path draft смешивал BFL с landmarks, неверно применял GRAIL, не описывал FERRARI/CHAIN_COVER/PATTERN_CACHE bindings и использовал неadmissible landmark upper bound.                                                                                                                                                                                                                                                                                              | S3–S4 утвердили SCC condensation, Yes/No/Maybe fallback, exact section codecs, five algorithms, binding-row PATTERN_CACHE и directed ALT для compatible additive costs. Этап 25 реализует differential correctness и budgets.                                                                                                                                                                                 | 🟡 contract approved; implementation deferred to 25                             |
   | `Q-32`  | Schema correctness + UX                 | ALTER GRAPH, migration/validation boundary, revision race и scope-symmetric index actions не были определены.                                                                                                                                                                                                                                                                                                                                                                  | S4 утвердил atomic one-revision ALTER, bounded prevalidation, constrained migration, ALTER GRAPH и ON CLASS/ON GRAPH symmetry. A11/core admin и отдельный migration gate реализуют контракт.                                                                                                                                                                                                                  | 🟡 contract approved; implementation deferred                                   |
   | `Q-33`  | Import functionality + UX               | `QUARANTINE`, `EVENTS=ON`, `CHANGEFEED=ON`, `FILL_FACTOR`, `SORT_MEM` и `PARALLELISM` распознаются синтаксисом import, но намеренно не исполняются в GA-profile этапа 10.                                                                                                                                                                                                                                                                                                      | Анализатор возвращает typed unsupported до создания staging и writer lease. `QUARANTINE`/durable rejected-set и measured `SORT_MEM`/`PARALLELISM` принадлежат этапу 20; `FILL_FACTOR` требует отдельного performance gate; `CHANGEFEED=ON` — этапу 27; `EVENTS=ON` — этапу 29.                                                                                                                                | 🟡 contract approved; implementation deferred                                   |

   Уточнение к `Q-16`/`Q-27` от 2026-09-23: последующее ASan/Miri/assembly-исследование
   подтвердило порчу до Direct I/O и эффективность инициализации окончательного frame in-place,
   но не доказало общий запрет на stack-размещение `AlignedPage` и не получило автономный
   минимальный reproducer. Проверенные stack/heap адреса удовлетворяли alignment, а замена
   `mimalloc` на `System` не устраняла прежнее падение. Поэтому закрыты наблюдавшиеся production
   call sites, а точная причина классифицируется как Windows MSVC ASan/codegen-sensitive до
   отдельного compiler reproducer. Полный разбор: `reports/aligned_page_corruption_investigation.md`.

   **Review дублирования кода:**

   - сравнить `write_new_graph_generation` и `write_delta_graph_generation`:
     creation, catalog entries, registration, sync, abort cleanup и publication
     должны иметь одного владельца без изменения crash contract;
   - проверить повторение между full-image builder и delta encoders, особенно
     properties, labels, statistics и primary/scalar indexes;
   - свести в один источник mutation invariants, `RecordId` uniqueness и
     write-set accounting для `GraphWriteView` и `GraphMutationView`;
   - проверить, что recursive heap/value accounting, memory/local/index query
     cursors и transport validation не имеют расходящихся копий одной семантики;
   - после стабилизации production Hybrid path удалить устаревшие копии из
     prototype/legacy control либо явно оставить минимальный oracle. Line-based
     сходство generated status/codec tables и Windows/Linux wrappers само по себе
     не является основанием для объединения.

   Объединять код разрешается только после characterization tests и проверки, что
   различия не выражают реальные platform, format или ownership contracts.
   Результат этапа — русскоязычный отчёт в `docs/reports`, исправленные
   спецификации и минимальные code changes для подтверждённых блокеров. Остальные
   записи получают конкретный зависимый этап и trigger. Каждый последующий этап
   добавляет вновь найденные допуски в реестр; перед этапом 19 не остаётся открытых
   Windows GA correctness, unbounded-resource или user-visible silent-fallback
   проблем.

5. **Полный SchemaCatalog** — зависит от **2, 4.1**.
   Довести durable definitions таблиц, полей, графов, параметров и индексов, их версии и transactional publication. Primary/label/adjacency системные индексы должны иметь каталожное представление, но не удаляться пользователем.

   **Статус (2026-09-06): ✅ завершён.** Единый MVCC `SchemaCatalog` владеет
   стабильными `SchemaId` для graph/table/field/parameter, типизированными
   `IndexId` из того же clock, версиями definitions,
   engine-owned primary/label/adjacency definitions и явным generation-aware
   membership. Local backend сохраняет тот же контракт в checksummed `FGDC` v5
   и hot slots; публикация схемы, данных и индексов происходит одним commit с
   FCW-проверкой. `u32::MAX` закреплён как `NULL_TABLE` и не выдаётся как
   `SchemaId`. Реализация StatisticsStore, incremental index maintenance и
   пользовательских GQL DDL-команд остаётся соответственно этапам 6–8.

   **Коррекция 8.1:** закрытый статус относится к историческому TABLE-контуру.
   `Q-08` переоткрыт. A5/A11 заменяют TABLE на CLASS, убирают data membership из
   schema catalog и переносят durable catalog на FGDC v7; property keys принадлежат отдельному
   append-only dictionary file.

6. **StatisticsStore** — зависит от **5**.  
   Целевой контракт после 8.1: точные class/base, graph/label/effective counts,
   degree statistics, distinct/null fractions и property histograms. Обновлять простые
   счётчики транзакционно; крупные гистограммы собирать при bulk scan. Устаревшая
   статистика не нарушает корректность и только ухудшает план.

   **Статус (2026-09-07): ✅ завершён.** MVCC `StatisticsStore` хранит точные
   historical table/label/graph counts и degree totals; RAM и local full-image используют
   один exact bulk-анализатор для equi-depth degree/property histograms,
   distinct/null fractions. Point-DML транзакционно обновляет простые счётчики,
   сохраняет bulk pages и явно отмечает их stale. Durable layout разделён на
   `graph_stats.fgb`, `label_stats.fgb` и `prop_stats.fgb`: при reopen eager
   читается только root, остальные части загружаются независимо по требованию.
   Формат и ограничения закреплены в `disk_storage_spec` §19. Синтаксис и semantics `ANALYZE`,
   sampled/HLL и bounded-memory policy утверждены S3–S4; implementation остаётся этапу 20.
   A10 переносит
   counters/histograms в явные base/effective scopes без изменения ленивого разделения файлов.
   `docs/reports/statistics_store_stage_6.md` сохраняет первоначальный stage-6
   снимок; актуальный split-layout подтверждается текущим codec/reopen gate.

7. **Базовый индексный контур** — зависит от **5, 6**.  
   Завершить standard/unique B+tree, Label и Count indexes, order-preserving key codec, generation side-files, crash-safe publication и `stale` fallback. Bulk build выполняется внешней сортировкой и bottom-up packing; WAL хранит только атомарную публикацию поколения.

   **Статус (2026-09-08): ✅ завершён после layout correction C0–C6.**
   `Q-21` закрыта: format-v1 использует `superblock.fgb`, шесть явных freelist
   bindings, раздельные `std_/uniq_` filenames и типизированный `IndexId`.
   Standard/unique B+tree используют
   существующий order-preserving scalar-key contract, а graph-wide primary —
   отдельный canonical `RecordId` contract. Оба пути используют Direct-I/O
   external runs по 256 МиБ с fan-in 64 и bottom-up packing. Label side-files
   строятся как плотные immutable arrays, Count — как exact single-page index с
   транзакционным page-WAL update. Полное UUID-поколение синхронизируется до
   единственного catalog/WAL commit; missing, unopenable и checksum-corrupt
   secondary indexes сохраняют schema и деградируют к scan, тогда как primary
   corruption остаётся фатальной. Correctness evidence и границы `Q-05`,
   `Q-18`–`Q-20` зафиксированы в
   [`index_store_stage_7.md`](reports/index_store_stage_7.md).

   - [x] `C0` — нормативный contract и запись `Q-21`;
   - [x] `C1` — публичный `IndexId` и typed catalog/commit identity;
   - [x] `C2` — codec и semantic validator `FGFC v4.companion_of`;
   - [x] `C3` — `superblock.fgb` и шесть отдельных freelist-файлов;
   - [x] `C4` — канонические `std_<ordinal:04>`/`uniq_<ordinal:04>`;
   - [x] `C5` — catalog/index/datastore/recovery и feature gates;
   - [x] `C6` — AGENTS, correction appendix, plan и Graphify.

8. **Core DDL vertical slices — частично superseded этапом 8.1** — зависит от **5, 7**.
   Реализованный GRAPH DDL сохраняется. Исторический TABLE DDL и system-graph ownership
   не являются целевым контрактом: A11 заменяет их `CLASS`/`FIELD ON CLASS`/
   scope-aware `INDEX`; compatibility alias `TABLE` после A16 не сохраняется.

   **Исторический статус (2026-09-09): ✅ gate пройден для прежней модели.** Parser → typed IR → execution
   реализует `DEFINE/REMOVE GRAPH`, `DEFINE/REMOVE/ALTER TABLE`,
   `DEFINE/REMOVE FIELD`, базовые Standard/Unique/Count
   `DEFINE/REMOVE/ALTER INDEX` и ограниченные `INFO FOR DB/TABLE/INDEX`.
   DDL публикуется через существующий MVCC commit; таблицы без graph живут в
   системном `GraphId=0`, а удаление именованного graph каскадно удаляет только
   реально принадлежащие ему таблицы. `FGDC v5` сохраняет schema mode,
   `ready/stale/prepared_for_removal` и optional physical binding. Тем самым
   `Q-19` закрыта: недоступный secondary наблюдаем и деградирует к scan,
   primary corruption остаётся фатальной. Подробности и evidence — в
   [`core_ddl_stage_8.md`](reports/core_ddl_stage_8.md).

   - [x] core GRAPH/TABLE/FIELD/INDEX grammar и явный отказ advanced kinds;
   - [x] typed logical schema IR и transactional catalog mutations;
   - [x] system-graph tables, graph-removal cascade и schema identity modes;
   - [x] `INFO FOR DB/TABLE/INDEX` с observable index state;
   - [x] `FGDC v5` round-trip, local reopen и secondary fallback gate;
   - [x] RAM/local, no-default/native-async correctness matrices;
   - [x] спецификации, AGENTS, отчёт и Graphify для исторического TABLE-среза.

   `ALTER GRAPH`, advanced index kinds, DML, staged DDL→DML analysis и
   statement abort/savepoint semantics не реализованы наперёд; последние две
   границы остаются в `Q-07` и принадлежат этапу 11.

8.1. **Возврат к graph-first модели атомов** — зависит от **8**.
   Отдельный dependency-ordered план и полный контракт:
   [`rework_plan.md`](rework_plan.md). SQL-like TABLE удаляется; `CLASS` задаёт namespace
   `RecordId = class:id`, immutable `NODE|EDGE` kind, schema и scope-aware indexes. Данные
   атома живут в global AtomStore, а граф хранит occurrence, topology, label и
   sparse overlay. `GraphId` всегда ненулевой; database pages адресует
   `DataSpaceId::Database`.

   `AtomHotPage` хранит только global base slots. `NodeHotPage`/`EdgeHotPage` и
   `nodes_hot.fgb`/`edges_hot.fgb` сохраняются как graph-local occurrence storage; legacy
   является прежний payload с `table_id`, graph-owned properties и query-scratch flags, а не
   сами page families.

   **Статус (2026-09-22): ✅ A0–A16 завершены, production cutover выполнен.**
   TABLE/system-graph paths и selector удалены; RAM/local/native-async,
   recovery/concurrency и performance gates пройдены. A8 зафиксирован в
   [`reports/atom_store_stage_a8.md`](reports/atom_store_stage_a8.md), итоговый
   срез — в
   [`reports/graph_first_atom_model_report.md`](reports/graph_first_atom_model_report.md).

9. **DML vertical slices** — зависит от **4, 8, 8.1**.
   Реализовать `CREATE`, `UPDATE`, `UPSERT`, `DELETE`, `RELATE`, `ATTACH`, `DETACH`,
   `EXPECT` и DML-return clauses для global atoms и graph occurrences. `UPSERT`
   синтаксически требует ID или `WHERE`; graph mutation требует explicit `IN GRAPH`, а
   commit повторно проверяет graph-family invariants при любом isolation level.

   **Статус (2026-09-22): ✅ завершён после graph-first cutover A11–A16.**
   Parser → typed IR → execution адресует global atoms и graph occurrences;
   local point-DML сохраняет primary, Standard, Unique и Count одним WAL commit.
   Commit повторно проверяет family invariants при ReadCommitted, Snapshot и
   Serializable. Исторический TABLE-срез описан в
   [`dml_stage_9.md`](reports/dml_stage_9.md), актуальный результат — в
   [`graph_first_atom_model_report.md`](reports/graph_first_atom_model_report.md).

   - [x] class-aware `CREATE/UPDATE/UPSERT/DELETE` grammar, typing, IR и execution;
   - [x] обязательный ID/`WHERE` для `UPSERT` и pre-image `EXPECT` до первой мутации;
   - [x] `RETURN NONE/BEFORE/AFTER/DIFF`, object fields и `RETURN VALUE`;
   - [x] transactional AtomStore/occurrence indexes, statistics и
     primary/Standard/Unique/Count maintenance;
   - [x] `Q-05` paired profiling и mutable fill policy;
   - [x] family-invariant revalidation при всех isolation levels;
   - [x] доступный sync/native-async и workspace integration gate;
   - [x] direct class DML на global AtomStore и graph-scoped effective DML;
   - [x] `ATTACH`/`DETACH`/class-aware `RELATE ... IN GRAPH`;
   - [x] RAM/local/recovery/index/fanout gate A15 и очистка старой модели A16;
   - [x] полный stage gate и перевод этапа в завершённое состояние.

   Statement refresh, staged DDL→DML analysis и savepoint/abort semantics не
   выполняются наперёд: они остаются в `Q-07` и этапе 11.

10. **BEGIN IMPORT** — зависит от **7, 9**.  
    ✅ **Завершён 2026-09-24.** Реализован атомарный COW import по
    dependency-ordered плану
    [`import_stage_10_plan.md`](import_stage_10_plan.md): bounded private segments,
    Rust callback и GrynQL vertical slices, deferred rebuild/validation, streaming
    side-file writers, bounded statistics и одну финальную catalog/WAL publication.
    `Q-17`/`Q-18` закрыты correctness/resource/performance gates; unsupported
    options остаются `Q-33`. Evidence и итоговые метрики:
    [`import_stage_10_report.md`](reports/import_stage_10_report.md).

    - [x] I0–I1: living docs, grammar и нормативный import-contract;
    - [x] I2–I3: private root, writer lease, callback и GrynQL RAM slice;
    - [x] I4: local unreachable generation и одна catalog/WAL publication;
    - [x] I5–I6: streaming side-files и bounded statistics;
    - [x] I7–I9: rebuild/validation, cleanup/recovery и local end-to-end;
    - [x] I10–I12: correctness, resource/performance gate и отчёт.

11. **Транзакционные операторы GrynnQL** — зависит от **9, 10**.
    Добавить явные `BEGIN/COMMIT/CANCEL`, isolation/read-only options, timeout и
    multi-statement write execution поверх восстановленного на этапе 10 обычного
    `Datastore::execute`. Выполнение внутри уже открытого `Transaction` запрещает
    вложенные transaction statements.

12. **Выражения и управляющий язык** — зависит от **10, 11**.
    Завершить coercions, idioms, collections, subexpressions, built-ins, `IF/ELSE`,
    loops, `TRY-CATCH`, `THROW` и `ASSERT`; подключить эти statements к import body
    с приоритетом языкового `TRY/CATCH` над `ON VIOLATION`. Установить
    depth/iteration/time budgets и проверять отмену на границах операторов.

13. **Полный реляционный SELECT** — зависит от **6, 7, 12**.  
    Реализовать joins/subqueries, aggregation, `GROUP`, `ORDER BY`, `START`, `LIMIT`, `FETCH`, `SPLIT`, `OMIT` и keyset cursors. Сохранить sync pull `RowCursor`; barrier-операторы используют ограниченную память и явный temporary spill.

14. **Rule/CBO optimizer и EXPLAIN** — зависит от **6, 7, 13**.  
    Ввести deterministic rule passes, scan/index/join alternatives, statistics-based costs и degradation penalties. При отсутствии или устаревании индекса всегда доступен корректный scan. `EXPLAIN` показывает logical/physical plan, estimates, selected indexes, fallbacks и resource warnings.

15. **Полный StandardPath MATCH** — зависит от **13, 14**.  
    Добавить OPTIONAL, aliases/joins, match modes, cardinality, inline labels/properties, directions, bounded variable paths, TRAIL/ACYCLIC/SIMPLE, path filters/objectives и shortest/weighted paths. Path explosion ограничивается budgets и предупреждениями планировщика.

16. **AtomWalk, hyper/meta и nested MATCH** — зависит от **15**.  
    Реализовать `ATOM`, HYPER, META, WITHIN, edge-as-endpoint, cross-level incidence, qualified refs и переходы во вложенные графы. `LevelPath` не смешивать с traversal path. На этом этапе допустим корректный scan без специализированного индекса.

17. **Traversal VM** — зависит от **14, 16**.  
    Реализовать отдельный от MATCH IR и VM: traverser, map/flat-map/filter, barriers, sack, path propagation, control flow и вызов существующих graph algorithms. Сначала однопоточный pull baseline; parallel execution добавлять только после профиля.

18. **Path и graph results** — зависит от **16, 17**.  
    Подключить `RETURN GRAPH`, `BUILD GRAPH`, path results и graph-return matrix. Публичные результаты содержат `RecordId`, `AtomRef`, `Path`, `GraphRef` или `MaterializedGraph`, никогда `DiskAtomRef`, page addresses или `StorageValue`. Результат не удерживает завершённый storage snapshot.

19. **Windows GA-core gate** — зависит от **3–18** и закрытия блокеров реестра **4.1**.
    Прогнать crash/recovery matrix, codec/parser fuzzing, RAM/local и Sync/NativeAsync differential tests, long-running SSI/checkpoint tests и storage/query benchmarks. После успеха заморозить core format v1, публичный API и поддерживаемый GrynQL-profile; advanced side-files остаются отдельно версионируемыми.

### Рубеж B — полная draft-conformance

20. **Фоновые задания и admin surface** — зависит от **19**.
    Реализовать durable job state для `CONCURRENTLY`, ограниченный `DEFER`, `REBUILD INDEX`, `INFO`, `KILL` и обслуживающих операций. После crash незавершённая сборка становится `stale`, а не молча `ready`.

21. **Параметры, UDF и оставшийся язык** — зависит от **12, 20**.
    Реализовать `DEFINE PARAM/FUNCTION`, closures, overload resolution, purity/security rules, recursion `MAXDEPTH`, object constructors и оставшиеся административные выражения.

22. **Analyzers и Fulltext** — зависит от **7, 20, 21**.
    Реализовать tokenizer/filter pipeline, inverted index, BM25, highlights, transactional/deferred updates и bulk build. Проверить Unicode normalization, analyzer versioning и одинаковость incremental/bulk результатов.

23. **Vector indexes** — зависит от **7, 20, 21**.
    Последовательно реализовать HNSW, затем DiskANN, типы векторов, distances и KNN/ANN
    operators по утверждённому §20.6: разные immutable base formats, общий exact delta,
    fingerprint/rebuild и collision-checked `HASHED_VECTOR`. Для каждого алгоритма зафиксировать
    recall/latency/memory/build/recovery gates; custom distance использует versioned pure UDF.

24. **Geometry R-tree** — зависит от **7, 20**.
    Реализовать geometry validation, STR bulk packing, spatial predicates, planner selectivity и exact post-filter после MBR lookup.

25. **Производные графовые индексы** — зависит от **16, 17, 20**.
    Реализовать EdgeEndpoint, Path, Reachability и Neighbourhood indexes. Проверить все пять
    reachability algorithms, `Yes/No/Maybe` exact fallback, binding-row PATTERN_CACHE и directed
    ALT для hop/weighted/compatible custom cost. Производные индексы являются snapshot artifacts
    с `build_lsn`; dependency mutation переводит их в `stale`.

26. **EndpointTensor и аналитические представления** — зависит от **16, 17**.  
    Добавить нормативные tensor/matrix views поверх полной incidence-модели без изменения runtime/storage границ. Кеширование допускается только как revision-bound derived data.

27. **Durable Changefeed** — зависит от **9, 19, 20**.
    Добавить глобальный versionstamp, before/after entries, retention, rotation, resume cursor и WAL-coordinated publication. Доставка — at-least-once; потеря истории выражается явным GAP.

28. **LIVE SELECT** — зависит от **13, 27**.
    Реализовать registry подписок, сериализованный predicate IR, bounded per-session queue, resume и output modes. Публичный Rust API остаётся pull-based: subscription id, `next`, `resume` и `close`; silent event loss запрещён.

29. **Events и LiveGraph integration** — зависит от **12, 20, 27, 28**.
    Реализовать `DEFINE EVENT`, sync/async execution, retry, `MAXDEPTH`, idempotency guidance и генерацию AtomWalk-рёбер. Mutable LiveGraph применяет remote delta только после явного conflict resolution и повторной проверки family invariants.

30. **Bridges** — зависит от **18, 21, 29**.
    Реализовать нормативные `Bridge`, `.use_bridge(...)` и `BridgeData`, затем `SurrealBridge`, включая RecordOrTable, ExternalQuery, InternalQuery, RELATE и graph construction. Bridge boundary преобразует данные только в публичные `Value`/`RecordId`/graph-типы.

31. **Финальный conformance gate** — зависит от **20–30**.
    Каждая строка conformance-матрицы должна быть реализована, протестирована либо явно помечена как ненормативная/out-of-scope. Прогнать обе платформы, все feature-комбинации, fuzz/crash/soak/performance suites, examples и cross-document audit. Выпустить итоговый русскоязычный отчёт.

### Рубеж C — Linux

32. **Linux sync parity** — зависит от **19**.  
    Реализовать Unix ownership/locking, atomic superblock replacement и Direct I/O. Проверить byte-identical format-v1 vectors, recovery, NVMe performance и soak на dataset не меньше `1,25 × RAM`. Если Linux требует несовместимого layout, format v1 не изменять: выпускать format v2 с явной миграцией.

33. **Linux NativeAsync** — зависит от **32**.
    Добавить проверенный tokio-uring backend под `native-async-io`, сохранив Sync default и синхронный query API. Недоступный явно выбранный режим возвращает typed error без fallback.

## Публичные границы

Основной поток остаётся таким:

```text
GrynQL text
  → syntax
  → binding/type analysis
  → logical IR
  → rule/CBO optimizer
  → RowCursor или Traversal VM
  → Value / RecordId / Path / MaterializedGraph
```

- `Datastore::execute`, `Transaction`, `GraphRead` и текущие публичные пути сохраняются.
- `QueryStatementKind` расширяется вместе с реально реализованными statements.
- NativeAsync остаётся внутренним способом page I/O, а не отдельным async query API.
- Schema, statistics, page/index DTO и `DiskAtomRef` остаются crate-private.
- Несовместимое изменение core format требует новой версии и миграции; существующий v1 не переопределяется.

## Общая проверочная матрица

- **Структура:** public-path compile tests и проверки feature/config parity.
- **Frontend:** positive/negative grammar cases, source spans, parser depth и fuzz без panic.
- **Analysis/IR:** scope, type inference, lowering golden tests и одинаковые diagnostics.
- **Query:** одинаковые результаты RAM/local Sync/NativeAsync; deterministic order только там, где он обещан.
- **Writes:** commit/abort/reopen, FCW, SSI phantoms, multi-graph atomicity и family invariants.
- **Storage:** malformed pages/WAL/catalog, torn tail, checkpoint watermarks, crash points и fuzz codecs.
- **Indexes:** incremental/bulk parity, unique violations, concurrent generation publication и stale fallback.
- **Live:** at-least-once, duplicate delivery, resume, retention expiry, GAP и backpressure.
- **Bridges:** fake bridge contract tests и feature-gated SurrealDB integration tests.
- **Каждый этап:** rustfmt check, focused check/test/Clippy; найденные допуски и
  silent fallback добавляются в реестр 4.1 с evidence, владельцем и trigger;
  полный workspace gate выполняется на рубежах 19 и 33, а несвязанный
  существующий lint debt отчётливо отделяется.

## Зафиксированные допущения

- Первый архитектурный этап касается только `src/` и связанных тестов; `fg-meta` остаётся сырым read-only примером реализации, а не источником требований или готовым production-кодом.
- Windows GA не ждёт Linux. После Windows freeze несовместимость Linux решается format v2 и миграцией.
- Production spill не создаётся без измеренного неделимого workload; стандартный bulk-путь — segmented import.
- Новые и закомментированные dependencies разрешены для экспериментальных и
  production-срезов, но их подключение, как и ручная реализация, требует
  подтверждённого fit, вывода `rust-workflow`, минимального feature set и
  измеримого преимущества. Наличие зависимости в комментариях не считается
  архитектурным решением. Код из `src/asynchronous` и `src/collections`
  допускается после аудита его контракта, ошибок, concurrency/safety свойств и
  тестов; в этапе 3.1 он не понадобился.
- macOS, distributed execution, SPDK, ARIES/UNDO, JIT, SMT optimizer verification и parallel recovery остаются вне плана.
- Документы и отчёты пишутся по-русски, комментарии Rust-кода — по-английски.
