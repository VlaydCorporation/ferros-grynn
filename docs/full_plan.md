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
- `schema/table.rs`: определения таблиц;
- `schema/field.rs`: определения полей;
- `schema/index.rs`: определения индексов;
- `schema/graph.rs`: определения графов;
- `root.rs`: `DatabaseRoot`, версии, snapshots и каталожное состояние;
- `backend.rs`: выбор RAM/local и адаптация хранилища;
- `transaction/conflict.rs`: read/write sets, FCW, SSI и commit validation;
- `transaction/mutation.rs`: graph/table mutation views;
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

## Топологический порядок дальнейших этапов

### Рубеж A — Windows GA-core

2. **Повторная синхронизация спецификаций** — зависит от **1**.  
   Установить приоритет: `disk_storage_spec` задаёт байтовый формат и durability, `gql_spec` — семантику языка, `gql_grammar` — точный синтаксис, `gql_tech_spec` — реализацию. Устранить расхождения DiskANN, ALTER, NativeAsync, format-v1 gate и статусов реализованности. В `implementation_plan` добавить conformance-матрицу и получить утверждение GrynQL-срезов до их реализации.

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

4. **Delta-oriented mutation foundation** — зависит от **3**.  
   Убрать полную материализацию local-графа из обычного `edit_graph`; формировать atom/page delta непосредственно из point mutations. Учесть свойства, крупную adjacency, составные `RecordId` и nested graph payload в write-set limit. После профиля сохранить лимит 256 МиБ; private spill реализовывать только при подтверждённом неделимом workload, который нельзя обработать сегментированным импортом.

5. **Полный SchemaCatalog** — зависит от **2, 4**.  
   Довести durable definitions таблиц, полей, графов, параметров и индексов, их версии и transactional publication. Primary/label/adjacency системные индексы должны иметь каталожное представление, но не удаляться пользователем.

6. **StatisticsStore** — зависит от **5**.  
   Добавить точные table/label counts, degree statistics, distinct/null fractions и property histograms. Обновлять простые счётчики транзакционно; крупные гистограммы собирать при bulk scan. Устаревшая статистика не нарушает корректность и только ухудшает план.

7. **Базовый индексный контур** — зависит от **5, 6**.  
   Завершить standard/unique B+tree, Label и Count indexes, order-preserving key codec, generation side-files, crash-safe publication и `stale` fallback. Bulk build выполняется внешней сортировкой и bottom-up packing; WAL хранит только атомарную публикацию поколения.

8. **Core DDL vertical slices** — зависит от **5, 7**.  
   Последовательно реализовать `DEFINE/REMOVE` для GRAPH и `DEFINE/REMOVE/ALTER` для TABLE, FIELD и базовых INDEX. `ALTER GRAPH` не реализовывать до появления и отдельного утверждения его семантики в `gql_spec`. Каждый slice проходит parser, typing, IR, transactional catalog update, recovery и `INFO`-представление. Неподдерживаемые advanced index kinds пока возвращают явный статус.

9. **DML vertical slices** — зависит от **4, 8**.  
   Реализовать `CREATE`, `UPDATE`, `UPSERT`, `DELETE`, `RELATE`, `EXPECT` и DML-return clauses для таблиц и графов. `UPSERT` синтаксически требует ID или `WHERE`; commit повторно проверяет graph-family invariants при любом isolation level.

10. **BEGIN IMPORT** — зависит от **7, 9**.  
    Реализовать segmented commits, deferred constraints, rejection report, external sort, bottom-up index build и cleanup незавершённых side-files. Обычный импорт не требует unlimited transaction spill.

11. **Транзакционные операторы GrynnQL** — зависит от **9**.  
    Добавить `BEGIN/COMMIT/CANCEL`, isolation/read-only options, timeout и multi-statement write execution. `Datastore::execute` использует одну автотранзакцию, если программа не содержит явного transaction block; выполнение внутри уже открытого `Transaction` запрещает вложенные transaction statements.

12. **Выражения и управляющий язык** — зависит от **11**.  
    Завершить coercions, idioms, collections, subexpressions, built-ins, `IF/ELSE`, loops, `TRY-CATCH`, `THROW` и `ASSERT`. Установить depth/iteration/time budgets и проверять отмену на границах операторов.

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

19. **Windows GA-core gate** — зависит от **3–18**.  
    Прогнать crash/recovery matrix, codec/parser fuzzing, RAM/local и Sync/NativeAsync differential tests, long-running SSI/checkpoint tests и storage/query benchmarks. После успеха заморозить core format v1, публичный API и поддерживаемый GrynQL-profile; advanced side-files остаются отдельно версионируемыми.

### Рубеж B — полная draft-conformance

20. **Фоновые задания и admin surface** — зависит от **19**.
    Реализовать durable job state для `CONCURRENTLY`, ограниченный `DEFER`, `REBUILD INDEX`, `INFO`, `KILL` и обслуживающих операций. После crash незавершённая сборка становится `stale`, а не молча `ready`.

21. **Параметры, UDF и оставшийся язык** — зависит от **12, 20**.
    Реализовать `DEFINE PARAM/FUNCTION`, closures, overload resolution, purity/security rules, recursion `MAXDEPTH`, object constructors и оставшиеся административные выражения.

22. **Analyzers и Fulltext** — зависит от **7, 20, 21**.
    Реализовать tokenizer/filter pipeline, inverted index, BM25, highlights, transactional/deferred updates и bulk build. Проверить Unicode normalization, analyzer versioning и одинаковость incremental/bulk результатов.

23. **Vector indexes** — зависит от **7, 20, 21**.
    Последовательно реализовать HNSW, затем DiskANN, типы векторов, метрики и KNN/ANN operators. До начала DiskANN отдельно утвердить `HASHED_VECTOR`, catalog payload, recovery и отдельно версионируемый side-file contract из `disk_storage_spec` §20.6.4; DiskANN не входит в core format v1. Для каждого алгоритма зафиксировать recall/latency/memory/build gates; custom distance использует утверждённый pure UDF contract.

24. **Geometry R-tree** — зависит от **7, 20**.
    Реализовать geometry validation, STR bulk packing, spatial predicates, planner selectivity и exact post-filter после MBR lookup.

25. **Производные графовые индексы** — зависит от **16, 17, 20**.
    Реализовать EdgeEndpoint, Path, Reachability и Neighbourhood indexes. Производные индексы являются snapshot artifacts с `build_lsn`; topology mutation переводит их в `stale`, после чего planner использует `ON_DEMAND`.

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
- **Каждый этап:** rustfmt check, focused check/test/Clippy; полный workspace gate выполняется на рубежах 19 и 33, а несвязанный существующий lint debt отчётливо отделяется.

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
