# Перспективный анализ кода и зависимостей (этап 1.6)

**Дата:** 2026-08-30  
**Статус:** новый самостоятельный отчёт; `codebase_analysis_stage_1_6.md` сохранён без изменений  
**Срез зависимостей:** `Cargo.lock` после выполненного владельцем репозитория `cargo update`

## 1. Назначение отчёта

Этот документ отвечает не на вопрос «что импортируется и нужно прямо сейчас», а на более важный для мигрирующего проекта вопрос:

> Какие уже имеющиеся модули, прототипы и зависимости могут пригодиться Ferros-Grynn в будущем, в какой подсистеме и при наступлении какого условия их следует рассмотреть раньше, чем писать собственную реализацию?

Поэтому отсутствие текущего потребителя здесь **не является основанием для удаления**. Оно означает только, что решение ещё не дошло до своей точки интеграции. Аналогично, пометка «условный кандидат» означает не «убрать и забыть», а «сохранить в реестре вариантов и сравнить с альтернативами, когда станет известен точный контракт».

Предыдущий отчёт остаётся полезным как аудит текущей связности, качества и стоимости сопровождения. Настоящий отчёт заменяет только его узкую интерпретацию будущей полезности.

## 2. Источники и методика

Прочитаны все спецификации, планы и отчёты в `docs/`, включая:

- `gql_spec.md`, `gql_grammar.md`, `gql_tech_spec.md`;
- `disk_storage_spec.md`, `disk_storage_plan.md`, `graphs.md`;
- `full_plan.md`, `implementation_plan.md`, `values_plan.md`;
- все документы из `docs/reports/`.

Дополнительно рассмотрены:

- производственные модули в `src/`, в том числе пока не подключённые к `lib.rs`;
- сырой пример реализации `crates/fg-meta`, не являющийся источником истины;
- локальные референсные крейты `ferntree`, `storekey`, `surrealmx` и код SurrealDB;
- релевантные реализации в `SurrealDB`;
- все 155 уникальных прямых пакетов, разрешённых для корневого пакета Cargo (156 рёбер зависимостей из-за двух разных kind у `mimalloc-pprof`).

При конфликте документов использовано следующее разделение полномочий:

| Область                                                | Основной источник                                                                                      |
|--------------------------------------------------------|--------------------------------------------------------------------------------------------------------|
| Семантика языка и наблюдаемое поведение                | `gql_spec.md`                                                                                          |
| Синтаксис                                              | `gql_grammar.md`                                                                                       |
| Формат страниц, WAL, recovery, индексы и долговечность | `disk_storage_spec.md`                                                                                 |
| Архитектура планировщика и исполнения                  | `gql_tech_spec.md`, но только там, где она не расходится с более новыми нормативными документами       |
| Порядок поставки                                       | `implementation_plan.md` и `full_plan.md`, с восстановлением реальных зависимостей между возможностями |

Поздние номера этапов в `full_plan.md` местами ссылаются вперёд или образуют циклическое впечатление. Поэтому ниже используется не слепое следование номерам, а семантическая зависимость: например, `LIVE SELECT` зависит от стабильных транзакций и changefeed, а DiskANN — от определённого контракта KNN, хранения векторов и фоновой сборки индекса.

## 3. Как читать оценки

| Оценка                        | Смысл                                                                                                                       |
|-------------------------------|-----------------------------------------------------------------------------------------------------------------------------|
| **Основа**                    | Уже соответствует устойчивому архитектурному направлению; развивать и повторно использовать в первую очередь.               |
| **Сильный кандидат**          | Хорошо покрывает заявленную будущую возможность; перед собственной реализацией обязателен локальный прототип или сравнение. |
| **Вариант выбора**            | Полезен для определённого дизайна, но конкурирует с другими уже имеющимися решениями.                                       |
| **Специализированный резерв** | Может дать пользу в поздней или опциональной области; интегрировать только после появления явного контракта.                |
| **Референс**                  | Ценен алгоритмами, тестами или идеями, но формат/API нельзя переносить буквально.                                           |

Ни одна из этих оценок сама по себе не является рекомендацией удалить код или зависимость. Для будущего выбора важнее три вопроса:

1. Совпадает ли семантика с нормативной спецификацией?
2. Укладывается ли решение в границы `parser → analysis/IR → execution → public Value/RecordId → storage DTO`?
3. Подтверждает ли прототип требуемую корректность, производительность, память и платформенную совместимость?

## 4. Карта будущего продукта

### 4.1. Горизонт A — устойчивое Windows-ядро

Ближайший продуктовый позвоночник:

1. измеряемая delta-oriented мутация графа без полной материализации;
2. `SchemaCatalog` и `StatisticsStore`;
3. базовые индексы и их сопровождение транзакцией;
4. DDL, DML, массовый импорт с внешней сортировкой;
5. транзакции и восстановление;
6. полный `SELECT`, затем cost-based оптимизация;
7. `MATCH`/`StandardPath`, `AtomWalk`, `Traversal`, графовые результаты;
8. выполнение Windows GA-критериев по корректности, памяти и производительности.

Здесь наиболее ценны уже существующие production storage/value/MVCC заделы, структуры ключей и индексов, streaming/external-sort инструменты, профилировщики и property/concurrency testing.

### 4.2. Горизонт B — полная спецификация языка и индексов

После устойчивого ядра появляются:

- анализаторы и полнотекстовый поиск;
- HNSW, затем DiskANN; фильтрация KNN;
- геометрия и R-tree;
- reachability, neighborhood, path и endpoint-индексы;
- фоновые задания сборки/перестройки/компакции;
- tensor/analytics операции;
- параметры, UDF и расширяемый каталог функций.

Именно здесь значительная часть «сейчас неиспользуемых» крейтов получает прямых потребителей: `fst`, text-нормализация, DiskANN-семейство, `geo`, `ndarray`, weighted future scheduling, registries и concurrent collections.

### 4.3. Горизонт C — реактивность, мосты и эксплуатация

Позднее ядро должно поддержать:

- durable changefeed и `LIVE SELECT`;
- события и `LiveGraph`;
- внешние мосты и потоковые адаптеры;
- административные команды, `INFO`, наблюдаемость и управление ресурсами;
- корректное завершение фоновых работ;
- Linux sync/async parity.

Тут становятся важны текущие async-примитивы, каналы с backpressure, cancellation/shutdown, tracing/metrics, typed configuration и платформенные I/O-обёртки.

### 4.4. Горизонт D — опциональная аналитика и ML

`graphs.md`, tensor-операции, векторные индексы и наличие ECS допускают более поздние сценарии:

- PageRank, признаки и векторные компоненты атомов;
- матричные и тензорные представления графа;
- embedding/model inference;
- графовые ML/UDF и аналитические мосты.

Это объясняет потенциальную роль `nalgebra`, `ndarray-*`, `ort`, half/SIMD и ECS. Но это отдельная продуктовая вертикаль, а не основание смешивать ML-форматы с каноническим storage codec.

## 5. Требование → имеющийся задел

| Будущая возможность           | Что уже есть                                                                              | Как использовать перспективно                                                                                                                       |
|-------------------------------|-------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------|
| Delta-oriented graph mutation | `src/graph`, `src/datastore`, `src/storage` и текущие MVCC-тесты                          | Сохранить действующие инварианты публикации; хранить дельту, а не полную приватную копию графа.                                                     |
| Схема и статистика            | `src/datastore/schema`, `src/storage/graph_pages/statistics.rs`, `nexus-stats`, `sysinfo` | Разделить долговечную статистику оптимизатора, потоковую runtime-телеметрию и системные лимиты.                                                     |
| B+tree и вторичные индексы    | page/storage заделы, `ferntree`, `storekey`, `papaya`, `fst`, `roaring`                   | `ferntree` рассматривать для volatile-индекса; on-disk B+tree строить по `disk_storage_spec`; `storekey` использовать как источник законов порядка. |
| Массовый импорт               | `ext-sort`, `tempfile`, `rayon`, `bytes`, canonical order keys                            | Проверить deterministic ordering, ограничение памяти, очистку/возобновление временных файлов и bottom-up pack.                                      |
| Полный `SELECT`               | текущие GQL AST/IR/cursors, Arrow/DataFusion, SurrealDB planner reference                 | Сохранить собственную семантику и storage boundary; Arrow/DataFusion выбирать только как вычислительный backend отдельных операторов.               |
| `MATCH` и `Traversal`         | `PathExpandCursor`, `TraversalIndex`, `src/graph`, `roaring_graphs`                       | Текущая graph-модель задаёт семантику; bitmap-код — кандидат для frontier/visited и индексных set operations.                                       |
| Полнотекстовый поиск          | `fst`, `rust-stemmers`, `deunicode`, `regex`, `fuzzy-matcher`, SurrealDB `FullTextIndex`  | Разделить analyzer pipeline, term dictionary, postings, ranking и транзакционную публикацию сегментов.                                              |
| Векторный поиск               | полное DiskANN-семейство, `half`, `nalgebra`, `ndarray`, `quick_cache`                    | Сначала зафиксировать точность, фильтры, snapshot visibility и формат; затем выбирать слои DiskANN по отдельности.                                  |
| Геометрия                     | `geo`, `geojson`, будущий R-tree                                                          | `geo` — вычисления, `geojson` — внешний DTO; канонический storage layout должен остаться собственным.                                               |
| Changefeed/Live               | async `watch`/`notify`/ordered streams, MVCC, SurrealDB changefeed/live reference         | Построить durable лог событий поверх commit/versionstamp, затем подписки и live recomputation с backpressure.                                       |
| Фоновые задания               | `future-queue`, `asyncband`, Tokio, waitgroup/shutdown primitives                         | Weighted scheduling по памяти/I/O, отмена, progress и recovery для index build/import/compaction.                                                   |
| Мосты                         | `async-trait`/`dynify`, streams/channels, Arrow/Serde/JSON                                | Порт должен оперировать public `Value`/`RecordId`, а не storage DTO; динамический dispatch вводить только при реальной plugin boundary.             |
| Наблюдаемость и бюджеты       | `memory`, `sys`, tracing/metrics/pprof/tracy, `Measurable`                                | Связать admission, spill и EXPLAIN ANALYZE с одними счётчиками, не смешивая диагностику с бизнес-семантикой.                                        |
| Tensor/analytics/ML           | ECS, `nalgebra`, `ndarray-*`, `ort`                                                       | Хранить признаки как sidecar-компоненты; inference и tensor batches делать опциональным вычислительным слоем.                                       |

## 6. Перспективная роль существующих модулей `src`

### 6.0. Уже подключённое ядро

| Модуль                | Ближайшая роль                                                                | Дальнейшее развитие и граница                                                                                                           |
|-----------------------|-------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------|
| `src/datastore`       | MVCC transaction/snapshot root и публикация изменений                         | Delta-oriented graph/table mutation, schema/index coupling и changefeed commit records; не превращать datastore в parser/planner слой.  |
| `src/storage`         | Page manager, graph pages, WAL/recovery, store и статистика                   | B+tree, external-sort import, index segments, checkpoint/compaction и Direct I/O; только этот слой владеет on-disk DTO/format.          |
| `src/values/runtime`  | Публичные Rust SDK values, identifiers, collections и graph foundation        | Богатые GQL types, graph values/results и runtime validation; независим от Arrow/DataFusion и disk DTO.                                 |
| `src/values/storage`  | TypeTag, conversion contexts, canonical bytes и order keys                    | Schema evolution, decode limits, page/index key encoding; любой сторонний codec сначала доказывает совместимость здесь.                 |
| `src/values/columnar` | Будущая граница batch materialization                                         | Arrow/serde_arrow/DataFusion adapters; не менять runtime equality/order и public query result ради columnar backend.                    |
| `src/syn`             | Lexer/token/parser и source diagnostics                                       | Полная EBNF, error recovery, spans, глубокая stack-safe рекурсия и fuzzing; выдаёт syntax tree, но не исполняет запрос.                 |
| `src/gql`             | AST после анализа, expressions, logical/execution cursors, datasets/traversal | DDL/DML/SELECT/MATCH/Traversal, optimizer и resource-aware pipelines; обращается к storage через явные read/write ports.                |
| `src/gql_status`      | Единая project-wide система ошибок и notifications                            | Статусы storage/recovery/index/job/bridge/admin, chaining и machine-readable diagnostics; новые ошибки добавляются через macro catalog. |
| `src/graph`           | Публичный facade/re-export графового runtime                                  | Стабильный SDK путь к AtomId/MetaGraph/algorithms/results; внутренний layout и storage handles не должны вытекать наружу.               |

Эти модули не нуждаются в «поиске оправдания»: они составляют производственный путь. Их перспективная ценность состоит в сохранении границ, чтобы поздние зависимости подключались адаптерами, а не переписывали семантическое ядро.

### 6.1. `src/asynchronous`

Это не просто «неподключённый модуль», а набор потенциальных строительных блоков для горизонта C:

- `watch` и `notify` — подписки на revision/config/schema/job progress;
- ordered/merge stream — детерминированное объединение changefeed, shard scans и параллельных операторов;
- backpressure/want — `LIVE SELECT`, мосты и ограниченные query pipelines;
- waitgroup/shutdown — управляемая остановка index build, import, compaction и bridge workers;
- object pool — повторное использование выровненных I/O-буферов;
- handoff — специализированная передача владения без буфера.

Наиболее перспективна **поэлементная интеграция**, а не включение всего модуля. Перед использованием каждого примитива нужны cancellation и Loom/Shuttle-тесты. `handoff` особенно ограничен `!Send`/`!Sync` и cancel-unsafety, поэтому подходит только для доказанного локального executor-сценария, а не как общий канал движка.

### 6.2. `src/collections/cow_vec`

Уже естественно соответствует MVCC snapshot/delta модели. Кроме текущего применения к graph roots может обслуживать:

- immutable snapshots каталога схемы;
- версии статистики оптимизатора;
- copy-on-write наборы свойств и небольшие plan metadata.

Не следует автоматически использовать для больших batch/vector данных: там важнее Arrow/ndarray или page-backed представление.

### 6.3. `src/collections/siv`

Возможные применения — стабильные handles для планов, cursors, jobs, registry entries и ephemeral query objects. Однако AtomId имеет нормативную битовую раскладку и generation semantics, поэтому `siv` не должен становиться заменой AtomId без доказательства защиты от stale handles. Лучше всего рассматривать его как внутренний arena/registry для объектов, которые не попадают на диск.

### 6.4. `src/collections/borrow_bag`

Typed append-only heterogeneous bag может быть полезен как:

- lifetime-bound контекст компиляции запроса;
- arena для интернированных констант/шаблонов;
- контейнер временных объектов, живущих ровно один planning/execution scope.

Ограничения — рост tuple-типа, сложная передача между слоями и отсутствие естественной динамической адресации. Применять стоит только там, где borrow-checker действительно устраняет аллокации/clone и API остаётся локальным.

### 6.5. `src/collections/radixdb`

Перспективные области:

- prefix-словарь терминов полнотекстового индекса;
- каталог schema/path/label names;
- autocomplete/fuzzy lexicon для tooling;
- immutable trie/segment experiments;
- компактный cache compiled path/pattern keys.

Код не следует оценивать только по отсутствию текущего потребителя: полнотекст, path indexes и analyzer dictionaries запланированы позднее. При этом существующий blob/paged mmap формат не совпадает с нормативными страницами, WAL и recovery Ferros-Grynn. Поэтому ценность — алгоритмы, trie layout и тесты; превращать его в основной storage engine можно только после отдельного доказательства совместимости.

### 6.6. `src/collections/roaring_graphs`

Bitmap-ориентированные графы дают несколько сильных будущих применений:

- frontier/visited sets для BFS и path expansion;
- reachability/neighborhood индексы;
- label/property posting lists;
- быстрые пересечения кандидатов MATCH/KNN;
- компактный oracle для тестов алгоритмов на небольших плотных графах.

Ограничение `u16` и более простая модель графа не позволяют использовать этот код как основное представление метаграфа. Но отдельные bitmap-операции и индексы могут стать производственным ускорителем вокруг нормативной AtomId-модели.

### 6.7. `src/memory`

Это основной модуль работы с памятью и владелец единого ресурсного контура:

- target-dependent global allocator;
- allocator tracking и thread-local aggregation;
- `Measurable`/heap accounting для values, query plans и working sets;
- external memory reporters;
- будущие memory budgets, admission и spill decisions.

Системную память/CPU модуль получает из `src/sys.rs`, но политика лимитов и интерпретация разных видов учёта остаются в `memory`. Важно разделять логический размер данных, оценку owned heap, allocator-visible bytes и фактический RSS: это разные величины.

### 6.8. `src/sys.rs`

Файловый модуль владеет system environment: host/cgroup memory, process RSS/CPU, load average, physical cores и available parallelism. Это источник наблюдений для `memory`, admission, `INFO SYSTEM`, benchmark/CI telemetry и разумных defaults, но не владелец allocator policy.

### 6.9. `src/utils`

Глобальные небольшие safe utilities, не принадлежащие одному предметному модулю. Текущий `ParseBytes` пригоден для человекочитаемых memory/size параметров. При росте utilities следует делить по предметным подмодулям; domain semantics, unsafe и математика остаются соответственно у domain-owner, `unsafed` и `math`.

### 6.10. `src/ecs`

ECS — хороший sidecar-слой для данных, не являющихся каноническим свойством storage-записи:

- PageRank/centrality/communities;
- позиции и визуализация;
- dense/sparse feature vectors;
- transient query marks и algorithm workspaces;
- ML/analytics components.

Он не должен становиться вторым источником истины для схемы, AtomId или долговечных пользовательских свойств без явной persistence semantics.

### 6.11. `src/math`

`math` — общий дом всех переиспользуемых математических операций. Текущий `exact_ops` может защищать:

- вычисление page/offset/slot размеров;
- cardinality/cost formulas;
- memory/spill budgets;
- tensor shapes и batch sizing;
- deterministic integer semantics GQL.

Предметная graph/query/storage математика остаётся у своего владельца, пока не появляется повторно используемый primitive. Перед расширением `exact_ops` следует уточнить error mapping: например, «делится, но не нацело» семантически отличается от division-by-zero.

### 6.12. `src/unsafed`

Потенциальный низкоуровневый дом:

- heap/native memory introspection;
- aligned buffers и Direct I/O;
- platform/FFI helpers;
- оптимизированные representation primitives.

Это глобальный дом переиспользуемого unsafe-кода, но не «всего unsafe проекта». Каждый unsafe-блок по умолчанию принадлежит модулю, инварианты которого он реализует. В `unsafed` выносятся только действительно повторяемые, доказанные primitives. Текущий отключённый код требует отдельного safety/API/error/platform аудита и подготовки к production перед использованием.

### 6.13. Глобальные макросы и detached utilities

При появлении нескольких реальных потребителей допустим отдельный `src/macros` для глобальных переиспользуемых declarative macros; пустой каркас заранее не нужен. Неподключённые хеши и макросы могут пригодиться для plan cache keys, pattern fingerprints, value hashing и generated boilerplate. При интеграции их нужно сверить с каноническим equality/order/hash контрактом `Value` и AtomId. Быстрый хеш нельзя молча превращать в стабильный on-disk fingerprint.

## 7. Локальные крейты и референсные реализации

### 7.1. `crates/fg-meta`

Сырой, непроверенный пример реализации. Он не является спецификацией или поведенческим источником истины и не нуждается в отдельном архитектурном описании. Любая идея или часть кода из него оценивается заново по текущим спецификациям, production `src/` и тестам.

### 7.2. `crates/configuration`

Крейт удалён: полезной логики для переноса в нём не было. Его нельзя указывать как доступную основу конфигурации; будущий config contract проектируется из реальных требований и текущих production-модулей.

### 7.3. `crates/ferntree`

Concurrent in-memory B+tree с optimistic lock coupling, epoch reclamation и range/bidirectional iteration — сильный кандидат для:

- volatile secondary indexes;
- catalog/range lookup;
- memtable или горячего слоя перед page-backed индексом;
- benchmark/reference реализации ordered concurrent map.

Это не готовая замена нормативному on-disk B+tree: нет page/WAL/recovery контракта. Замыкания модификации могут повторно выполняться, что требует чистых/idempotent callbacks. Выбор — после benchmark против `BTreeMap` под lock, `papaya` и реального access pattern.

### 7.4. `crates/storekey`

Order-preserving compact bytes, `BorrowDecode` и derives делают его полезным для:

- composite secondary-index keys;
- prefix/range encoding;
- тестов законов «порядок Value == лексикографический порядок bytes»;
- идей по schema-frozen derives.

Но `values::storage::order_key` и `disk_storage_spec` остаются нормативными. Лучшее применение — источник property tests, encoding techniques и, возможно, внутренний helper после доказательства byte-for-byte совместимости.

### 7.5. `crates/surrealmx`

Lock-free in-memory MVCC KV с SSI/SI, savepoints, range scans, version GC и transaction pool полезен как референс для:

- conflict/predicate test matrices;
- savepoint semantics;
- version garbage collection;
- transaction object pooling;
- сравнительных microbenchmarks.

Он не заменяет storage Ferros-Grynn: его AOL/snapshot формат и durability model не соответствуют нормативным страницам/WAL.

### 7.6. SurrealDB-код в репозитории

Наиболее перспективные части для изучения, а не прямого копирования:

- lexer/token/span и longest-match поведение;
- GQL AST, arena/NodeId, logical planner;
- маленькие sorted `VecMap`/`VecSet` для детерминированных AST/IR-наборов;
- fulltext analyzer/dictionary/postings/BM25 architecture;
- vector indexes и predicate filtering;
- changefeed/live query pipelines;
- bulk import и planner tests;
- SDK DTO ↔ runtime Value derives.

Граница остаётся жёсткой: публичные query results — `RecordId`/`Value`; SurrealDB internal IDs, storage DTO и форматы не должны протекать в API Ferros-Grynn.

### 7.7. `crates/ferros-grynn-macros`

Этот крейт уже является устойчивым владельцем compile-time генерации. Будущие применения:

- расширение каталога GQLSTATUS;
- derives для измерения памяти и структурных accumulator-типов;
- возможно, узкие derives для schema/config/SDK conversions.

Он не должен превращаться в место для бизнес-семантики: generated output обязан оставаться проверяемым, с хорошими spans, compile-fail и expansion tests.

### 7.8. `ferros-grynn-bench`

Benchmark workspace нужен не только для текущих замеров, но и как neutral selection ground для будущих решений: `ferntree` vs lock+map, channel families, Arrow/DataFusion vs RowCursor, bitmap frontiers, external sort и ANN layers. Чтобы результаты влияли на архитектуру, benchmark должен фиксировать dataset shape, seed, warmup, allocator, platform и median/tail metrics.

## 8. Полный каталог прямых Cargo-зависимостей

Версии ниже — разрешённый offline/locked срез после `cargo update`. Каталог намеренно включает пакеты без текущего импортёра. «Когда выбирать» означает точку, в которой этот пакет нужно проверить **до** написания аналога.

<!-- direct-dependencies:start -->

### 8.1. Storage, страницы, импорт и платформа

| Пакет               | Версия | Возможная будущая роль                                                                | Когда выбирать / важное ограничение                                                                                               | Оценка                    |
|---------------------|-------:|---------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------|---------------------------|
| `arrayref`          |  0.3.9 | Без копирования получать ссылки на фиксированные массивы в page/header/codec коде.    | При реализации бинарных заголовков; сначала сравнить с `TryInto<&[u8; N]>` из std.                                                | Вариант выбора            |
| `binrw`             | 0.15.2 | Декларативное чтение/запись фиксированных page, superblock и index headers.           | Когда layout стабилен; не использовать там, где скрытая магия мешает проверить точные offsets и limits.                           | Сильный кандидат          |
| `bitflags`          | 2.13.1 | Page/record/index/atom flags с контролем неизвестных битов.                           | При вводе первых нормативных flags; явно определить forward-compatibility.                                                        | Основа                    |
| `bytes`             | 1.12.1 | Буферы I/O, WAL frames, сетевые/bridge batches, zero-copy slices.                     | При streaming I/O и bridge APIs; владение выровненной Direct I/O памятью потребует отдельного типа.                               | Основа                    |
| `derive-io`         |  0.5.0 | Делегирование sync/async I/O для device/page wrappers.                                | Если количество однотипных адаптеров станет заметным; invariants Direct I/O лучше оставить явными.                                | Вариант выбора            |
| `ext-sort`          |  0.1.5 | Двухпроходная внешняя сортировка для `BEGIN IMPORT`, index build и bottom-up packing. | До собственной реализации импорта проверить memory-limit feature, deterministic order, cancellation, Windows temp I/O и recovery. | Сильный кандидат          |
| `fastcdc`           |  5.0.0 | Content-defined chunking для backup/snapshot transfer и дедупликации экспортов.       | Только после появления backup/replication контракта; не применять к WAL/pages без требования CDC.                                 | Специализированный резерв |
| `memmap2`           | 0.9.11 | Memory-mapped immutable index segments, FST и тестовые/diagnostic readers.            | Сейчас dev-зависимость; не смешивать mmap consistency с Direct I/O page manager.                                                  | Вариант выбора            |
| `nix`               | 0.31.3 | Unix Direct I/O, file flags, alignment, resource and signal integration.              | На этапе Linux parity; все platform branches должны иметь общие capability tests.                                                 | Основа                    |
| `path-clean`        |  1.0.1 | Нормализация import/config/snapshot paths до security/canonicalization checks.        | При файловых DDL/admin операциях; lexical clean не заменяет filesystem canonicalization и sandbox policy.                         | Вариант выбора            |
| `scopeguard`        |  1.2.0 | Гарантированная очистка temp files, locks, pins и частично опубликованных ресурсов.   | При failure/cancellation paths импорта и индексов; использовать RAII-типы, если они выражают контракт яснее.                      | Основа                    |
| `tempfile`          | 3.27.0 | Временные run-файлы external sort, spill, index build и crash tests.                  | При импорте/spill; production lifecycle должен различать удаляемый temp и recovery-visible staging.                               | Сильный кандидат          |
| `windows`           | 0.62.2 | `FILE_FLAG_NO_BUFFERING`, Windows IoRing, system/allocator APIs.                      | Windows GA и async I/O; изолировать unsafe handles и закрытие ресурсов в платформенных адаптерах.                                 | Основа                    |
| `tikv-jemallocator` |  0.7.0 | Global allocator на не-MSVC, allocator stats/profiling.                               | Уже выбран платформенно; измерять fragmentation/RSS под graph и query workloads.                                                  | Основа                    |

### 8.2. Значения, сериализация, идентификаторы и columnar boundary

| Пакет          |  Версия | Возможная будущая роль                                                                   | Когда выбирать / важное ограничение                                                                                               | Оценка                    |
|----------------|--------:|------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------|---------------------------|
| `arrow`        |  58.4.0 | Columnar batches для `SELECT`, аналитики, bulk bridges и vectorized operators.           | После стабилизации `Value`/row semantics; Arrow остаётся materialization layer, не public runtime и не disk DTO.                  | Сильный кандидат          |
| `castaway`     |   0.2.4 | Эффективные generic/runtime conversions без лишней специализации API.                    | В горячих преобразованиях `Value`, только если текущие profiles подтверждают пользу.                                              | Вариант выбора            |
| `compact_str`  |   0.9.1 | Малые labels, symbols, aliases, property names и diagnostic fragments.                   | При измеренном большом числе коротких строк; сохранить единый equality/hash контракт.                                             | Сильный кандидат          |
| `datafusion`   |  54.1.0 | Готовые relational/aggregate/sort/window operators и Arrow execution для части `SELECT`. | Сначала сделать thin vertical slice; собственные GQL semantics, graph operators, errors и resource limits не отдавать DataFusion. | Вариант выбора            |
| `geojson`      |   1.0.0 | Внешний DTO/bridge формат геометрии.                                                     | При GQL geo import/export; валидировать CRS/geometry limits и конвертировать в runtime type.                                      | Сильный кандидат          |
| `half`         |   2.7.1 | F16/BF16 vectors, ANN storage/compute и ML exchange.                                     | После определения точности KNN; спецификация сейчас ожидает full precision, поэтому lossy storage нельзя вводить молча.           | Специализированный резерв |
| `hex`          |   0.4.3 | Диагностика RecordId/page/WAL bytes, import/export и test fixtures.                      | В human-facing diagnostics; не использовать hex как внутреннее хранение binary IDs.                                               | Вариант выбора            |
| `hex-literal`  |   1.1.0 | Читаемые compile-time byte fixtures для codec/page golden tests.                         | При расширении binary conformance tests.                                                                                          | Сильный кандидат          |
| `jiff`         |  0.2.35 | Runtime temporal values, time zones, intervals и GQL temporal operations.                | При полном temporal контракте; централизовать DST/ambiguity/rounding semantics.                                                   | Сильный кандидат          |
| `num`          |   0.4.3 | Комплексные/рациональные/общие числовые утилиты для analytics/tensors.                   | Только для реально публичных числовых типов или алгоритмов; не расширять GQL type system случайно.                                | Специализированный резерв |
| `num-traits`   |  0.2.19 | Общие bounds/conversions в numeric, graph weights и tensors.                             | Когда generic algorithm действительно работает с несколькими типами; избегать абстракции ради одного `f64`.                       | Вариант выбора            |
| `rkyv`         |  0.8.18 | Zero-copy archive для cache, IPC, immutable snapshots и быстрых внутренних артефактов.   | Только с version envelope, validation и decode limits; не подменяет нормативный canonical storage codec.                          | Сильный кандидат          |
| `rust_decimal` |  1.42.1 | Точная GQL decimal arithmetic, ordering и external serialization.                        | При реализации NUMERIC/DECIMAL; закрепить scale, overflow и canonical encoding.                                                   | Основа                    |
| `schemars`     |   1.2.2 | JSON Schema для config/admin/bridge DTO и SDK surface.                                   | Когда появится внешний JSON-контракт; storage schema и GQL schema — отдельные понятия.                                            | Вариант выбора            |
| `serde`        | 1.0.229 | Config, admin/bridge DTO, external formats и test fixtures.                              | На доверительных границах с validation/limits; не считать Serde-формат долговечным disk contract.                                 | Основа                    |
| `serde_arrow`  |  0.15.0 | Перенос typed DTO/rows в Arrow batches.                                                  | Для bridge/analytics materialization; проверить nullability, nested values и zero-copy claims на реальных схемах.                 | Сильный кандидат          |
| `serde_json`   | 1.0.151 | JSON values, config, explain/admin output и bridges.                                     | Ограничить depth/size и сохранить различие JSON-number и GQL numeric types.                                                       | Основа                    |
| `ulid`         |   3.0.0 | Сортируемые по времени внешние job/event/import IDs.                                     | При проектировании admin/changefeed identifiers; не заменять им AtomId/RecordId.                                                  | Вариант выбора            |
| `uuid`         |  1.26.0 | GQL UUID value, external identities, transaction/job IDs.                                | При фиксации UUID version/ordering; UUIDv7 может покрыть часть ULID-сценариев, нужен один согласованный выбор.                    | Основа                    |

### 8.3. Lexer/parser, строки, полнотекст и человекочитаемые значения

| Пакет           | Версия | Возможная будущая роль                                                      | Когда выбирать / важное ограничение                                                                                        | Оценка                    |
|-----------------|-------:|-----------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------|---------------------------|
| `bytecount`     |  0.6.9 | Быстрый подсчёт строк/байтов для spans, diagnostics и import scanning.      | Только в измеренном lexer/import hotspot; корректность Unicode positions должна быть определена отдельно.                  | Вариант выбора            |
| `deunicode`     |  1.6.2 | Опциональная ASCII-folding/transliteration стадия полнотекстового analyzer. | Когда analyzer DDL явно запрашивает её; не делать необратимую нормализацию по умолчанию.                                   | Сильный кандидат          |
| `fst`           |  0.4.7 | Компактный mmap-able term dictionary, prefix/range и automata search.       | На этапе fulltext/static dictionaries; включить fuzzy/Levenshtein feature только при контракте. Не заменяет postings/MVCC. | Сильный кандидат          |
| `fuzzy-matcher` |  0.3.7 | Fuzzy поиск в tooling, schema autocomplete и экспериментальном analyzer.    | Для UI/CLI convenience или явно заданной GQL-функции; ranking не должен быть недокументированным.                          | Специализированный резерв |
| `humantime`     |  2.4.0 | Парсинг duration в `FG_*`, admin options и operational config.              | Для config surface; GQL temporal grammar должна использовать собственную строгую семантику.                                | Сильный кандидат          |
| `logos`         | 0.16.1 | Generated lexer для отдельных DSL/config/token experiments.                 | Сравнить с текущим hand-written GQL lexer на longest match, error spans и compile-time; не держать два production lexer.   | Вариант выбора            |
| `memchr`        |  2.8.3 | SIMD-ускоренные delimiter/newline/quote scans в lexer, CSV/import и codecs. | При профилированном byte-scan hotspot; сохранить корректную state machine вокруг найденных байтов.                         | Сильный кандидат          |
| `phf`           | 0.14.0 | Compile-time таблицы keywords, functions, status descriptors и type names.  | При стабильном наборе ключей; динамические UDF/analyzers требуют отдельного registry.                                      | Сильный кандидат          |
| `reblessive`    |  0.4.3 | Stack-safe recursive parsing/evaluation через explicit stack/futures.       | Для глубокой GQL AST/parser recursion; проверить cancellation и diagnostics.                                               | Основа                    |
| `regex`         | 1.13.1 | GQL regex predicates, analyzer filters и validation.                        | Закрепить flags, Unicode, resource/cost semantics; компилировать и кэшировать patterns.                                    | Основа                    |
| `regex-syntax`  | 0.8.11 | Разбор/анализ regex AST для validation, cost limits и optimizer predicates. | Когда простого `Regex` недостаточно для ранней диагностики или безопасного анализа.                                        | Сильный кандидат          |
| `rust-stemmers` |  1.2.0 | Языковые stemmers в configurable fulltext analyzer.                         | После DDL analyzer contract; versioning analyzer должен инвалидировать/перестраивать индекс.                               | Сильный кандидат          |
| `strsim`        | 0.11.1 | Edit distance для диагностических подсказок, похожих имён и tooling.        | В error reporting/schema suggestions; не запускать O(n*m) по большим словарям без FST/index pruning.                       | Сильный кандидат          |

### 8.4. Коллекции, кэши и наборы кандидатов

| Пакет                 |         Версия | Возможная будущая роль                                                                  | Когда выбирать / важное ограничение                                                                                          | Оценка                    |
|-----------------------|---------------:|-----------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------|---------------------------|
| `append-only-vec`     |          0.1.8 | Монотонные registries, intern tables, immutable-after-publish catalogs.                 | Когда lifetime совпадает с epoch/database lifetime; отсутствие удаления требует явной границы роста. Сравнить с `boxcar`.    | Вариант выбора            |
| `boxcar`              |         0.2.14 | Concurrent append-only vector со стабильными адресами для event/version registries.     | Для многопоточной монотонной публикации; не использовать для бесконечного changefeed без GC/segment rotation.                | Сильный кандидат          |
| `dashmap`             |      7.0.0-rc2 | Sharded concurrent map для active transactions, jobs, subscriptions и caches.           | После определения contention/read-write pattern; это RC-версия, сравнить с `papaya` и lock+map.                              | Вариант выбора            |
| `fnv`                 |          1.0.7 | Дешёвый hash для маленьких доверенных таблиц/legacy compatibility.                      | Только где вход не атакующий и `rapidhash`/std явно хуже; не создавать новый общий hash policy.                              | Специализированный резерв |
| `indexmap`            |         2.14.1 | Детерминированный insertion order в AST, schemas, plans и JSON/explain output.          | Когда порядок является наблюдаемым или нужен reproducible plan; иначе обычная map дешевле концептуально.                     | Сильный кандидат          |
| `inplace-vec-builder` |          0.1.1 | In-place filter/map/rewrite для optimizer expressions и value arrays.                   | Только в профилированном allocation hotspot с хорошими panic-safety тестами.                                                 | Специализированный резерв |
| `itertools`           |         0.15.0 | Читаемые planner/traversal transformations, grouped joins и test/report helpers.        | Для control-plane кода; в hot loop оценить generated code и allocations.                                                     | Вариант выбора            |
| `once_cell`           |         1.21.4 | Lazy/static initialization для compatibility и специальных cell-вариантов.              | В новом коде сначала `std::sync::OnceLock`/`LazyLock`; оставить пакет там, где его API действительно нужен.                  | Вариант выбора            |
| `papaya`              |          0.2.5 | Lock-free concurrent hash map для read-heavy catalogs/caches/conflict indexes.          | Сравнить на реальном key/value/lifetime workload; pin/guard semantics не должны протечь в публичный API.                     | Сильный кандидат          |
| `quick_cache`         |          0.7.0 | Bounded cache планов, regex/analyzers, decoded pages и ANN hot nodes.                   | После определения weight, invalidation и schema/version keys; count-based capacity недостаточна для разного размера entries. | Сильный кандидат          |
| `rapidhash`           |          4.5.1 | Быстрые внутренние hash maps/sets и ephemeral fingerprints.                             | Основа внутреннего hashing; стабильные persisted hashes требуют отдельного versioned алгоритма.                              | Основа                    |
| `roaring`             |         0.11.5 | Posting lists, label/property filters, traversal frontiers, reachability и KNN filters. | При первых bitmap-friendly индексах; измерить sparse/dense crossover и AtomId-to-u32/u64 mapping.                            | Сильный кандидат          |
| `slotmap`             |          1.1.1 | Generational handles для ephemeral arenas/registries и runtime objects.                 | Для объектов вне нормативного RecordId/AtomId layout; stale-handle tests обязательны.                                        | Основа                    |
| `smallvec`            | 2.0.0-alpha.12 | Inline adjacency, малые AST lists, path fragments и small result tuples.                | Уже соответствует access pattern; alpha API/version риск учитывать перед стабильным release.                                 | Основа                    |
| `thin-vec`            |         0.2.19 | Компактные многочисленные пустые/малые vectors в AST/schema/IR.                         | Если heap/header profiling показывает выигрыш против `Vec`/`SmallVec`; выбрать один тип по каждому boundary.                 | Вариант выбора            |

### 8.5. Асинхронность, параллелизм, координация и жизненный цикл

| Пакет                 |        Версия | Возможная будущая роль                                                               | Когда выбирать / важное ограничение                                                                                                 | Оценка                    |
|-----------------------|--------------:|--------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------|---------------------------|
| `affinitypool`        |         0.8.0 | Выделенный FIFO blocking pool для sync disk/index задач с producer affinity.         | Если `spawn_blocking`/Rayon не дают изоляции или locality; пакет не выполняет настоящую CPU pinning автоматически.                  | Вариант выбора            |
| `async-channel`       |         2.5.0 | Runtime-neutral bounded MPMC для bridges, background jobs и tests.                   | Когда компонент не должен зависеть от Tokio; определить backpressure, close и cancellation semantics.                               | Сильный кандидат          |
| `async-fn-stream`     |         0.3.2 | Генерация потоков из async-функций для cursors/traversal/changefeed.                 | Сравнить с `async-stream` и ручным state machine; оставить один стиль на execution boundary.                                        | Вариант выбора            |
| `async-lock`          |         3.4.2 | Runtime-neutral mutex/RwLock/semaphore/barrier для библиотечных async-слоёв.         | Если слой должен работать вне Tokio; не смешивать lock families без documented ordering.                                            | Вариант выбора            |
| `async-signal`        |        0.2.14 | OS signals для server/CLI graceful shutdown и reload.                                | На этапе standalone service/Linux; embedded SDK не должен сам владеть process signals.                                              | Специализированный резерв |
| `async-std`           |        1.13.2 | Dev/runtime-compatibility проверки независимых futures.                              | Пакет сам объявлен deprecated-направлением; не выбирать как production runtime, но можно сохранить для compatibility tests.         | Референс                  |
| `async-stream`        |         0.3.6 | Удобные streaming cursors, graph expansion и live events.                            | Когда stream state machine иначе разрастается; проверить drop/cancellation и отсутствие скрытой буферизации.                        | Сильный кандидат          |
| `async-trait`         |        0.1.92 | Object-safe async ports для Bridge/I/O/plugin boundaries.                            | Только при реальном dynamic dispatch; native async trait/static dispatch предпочтительнее внутри ядра.                              | Вариант выбора            |
| `asyncband`           |         0.6.7 | Runtime-neutral barriers, latches, fair admission, shutdown, singleflight, channels. | Сильный набор для scheduler/live/jobs; до интеграции выбрать, какие собственные/Tokio primitives он заменяет, и не дублировать всё. | Сильный кандидат          |
| `atomic-waker`        |         1.1.2 | Низкоуровневое пробуждение одного consumer в custom future/queue/I/O primitive.      | Только внутри маленького доказанного primitive с Loom/Shuttle тестами.                                                              | Специализированный резерв |
| `crossbeam`           |         0.8.4 | Scoped threads, deques/channels/epoch для sync storage/index/GC и work stealing.     | Для конкретного подмодуля, а не как общий concurrency toolbox; memory reclamation invariants документировать.                       | Основа                    |
| `crossfire`           |        3.1.19 | Быстрые sync↔async и bounded channels разных топологий.                              | Для Windows IoRing bridge или mixed workers после benchmark; close/backpressure должны совпасть с API.                              | Сильный кандидат          |
| `diatomic-waker`      |         0.2.3 | Lock-free multi-notifier/single-consumer eventcount.                                 | Если channel/event-listener пробуждения измеримо узкие; unsafe implementation требует Loom и минимального encapsulation.            | Специализированный резерв |
| `dynify`              |         0.1.2 | Heapless dyn-compatible async traits и in-place initialization для adapters.         | Только если Bridge/I/O порт требует dyn и аллокации доказанно мешают; иначе обычный enum/static dispatch проще.                     | Специализированный резерв |
| `event-listener`      |         5.4.2 | Runtime-neutral notification primitive для state changes, pools и queues.            | При создании маленьких futures-aware primitives; fairness и lost-wakeup tests обязательны.                                          | Сильный кандидат          |
| `future-queue`        |         0.4.0 | Weighted/grouped scheduler будущих задач с ограничением concurrency.                 | Перед собственным scheduler для import/index/bridge; веса можно связать с memory/I/O budgets и tenant/group fairness.               | Сильный кандидат          |
| `futures`             |        0.3.34 | Основной facade combinators, streams, sinks и executors для runtime-neutral кода.    | Использовать на общих async boundaries; избегать одновременного импорта нескольких альтернативных facade.                           | Основа                    |
| `futures-concurrency` |         7.7.1 | Structured joins/races/streams для batch I/O, bridges и параллельных operators.      | Когда cardinality и cancellation известны; unbounded join не должен обходить resource budget.                                       | Вариант выбора            |
| `futures-core`        |        0.3.34 | Минимальные `Future`/`Stream` traits в низкоуровневых API.                           | Для снижения feature/dependency surface primitives; на высоком уровне достаточно `futures`.                                         | Основа                    |
| `futures-executor`    |        0.3.34 | Малый executor для isolated tests/tools или runtime-neutral blocking.                | Не создавать второй production runtime рядом с Tokio; полезен в unit tests и standalone helpers.                                    | Специализированный резерв |
| `futures-lite`        |         2.6.1 | Компактные runtime-neutral combinators/I/O в маленьких adapters.                     | Если конкретный dependency/API уже опирается на него; не вводить параллельный стиль без выгоды.                                     | Вариант выбора            |
| `futures-util`        |        0.3.34 | Полные combinators, stream/sink extensions и pin helpers.                            | В execution/bridge слоях; следить за boxing, buffering и cancellation.                                                              | Основа                    |
| `genawaiter`          |        0.99.1 | Generator-like sync/async traversal и pull cursors.                                  | При сложном yield-oriented AtomWalk; сравнить state size/performance с ручным `RowCursor` и `async-stream`.                         | Вариант выбора            |
| `maybe-async`         |        0.2.11 | Генерация sync/async вариантов общего adapter-кода.                                  | Только если тест подтверждает реальную дубликацию без ухудшения diagnostics/API; storage sync reference должен оставаться явным.    | Вариант выбора            |
| `parking_lot`         |        0.12.5 | Компактные быстрые locks/condvars в sync MVCC, caches, catalogs и page manager.      | Основа sync locking; poison policy, lock ordering и async boundary фиксировать явно.                                                | Основа                    |
| `pin-init`            |         0.2.0 | Безопасная инициализация address-sensitive I/O/ring/self-referential структур.       | Только при доказанной pinning необходимости; сначала предпочесть movable ownership layout.                                          | Специализированный резерв |
| `pin-project`         |        1.1.13 | Безопасная projection полей custom futures/streams.                                  | При ручных execution/changefeed state machines; не писать unsafe projection вручную.                                                | Сильный кандидат          |
| `rayon`               |        1.12.0 | Data parallel index build, import transforms, analytics и graph algorithms.          | Для CPU-bound bounded batches; не запускать из async runtime без изоляции и общего concurrency budget.                              | Основа                    |
| `saa`                 |         5.6.0 | Word-sized allocation-free low-level locks, wait queues и latches.                   | Только при измеренном primitive-level bottleneck; unsafe/loom требования выше, чем у стандартных locks.                             | Специализированный резерв |
| `slot-cell`           |         0.1.2 | `!Sync` move-cell для single-thread executor/state machine ownership.                | Для локального I/O executor или handoff internals; не для общего shared engine state.                                               | Специализированный резерв |
| `tokio`               |        1.53.1 | Основной async runtime, tasks, I/O, timers, sync и Windows/Linux adapters.           | Устойчивое направление для async wrapper/live/bridges; embedded API должен принимать runtime, а не скрыто создавать глобальный.     | Основа                    |
| `tokio_env`           |         0.1.0 | Настройка runtime из окружения для server/tool binaries.                             | Когда появится официальный host binary; library crate не должен читать env при импорте.                                             | Специализированный резерв |
| `tokio-blocked`       |         0.1.0 | Tracing-диагностика долгих poll и блокировки executor threads.                       | На async performance/conformance этапе; `tokio_unstable` уже включён, но слой остаётся диагностическим.                             | Сильный кандидат          |
| `tokio-blocking`      |         0.1.1 | Малый adapter для blocking задач.                                                    | Проверить, не полностью ли заменён `tokio::task::spawn_blocking`; сохранять только при уникальной семантике.                        | Референс                  |
| `tokio-executor`      | 0.2.0-alpha.6 | Историческая pre-1.0 модель executor abstraction.                                    | Не выбирать для нового production кода; полезен только если старый prototype требует чтения/миграции API.                           | Референс                  |
| `tokio-graceful`      |         0.2.2 | Координация graceful shutdown server/background tasks.                               | При service/jobs lifecycle; embedded SDK, вероятно, нуждается в собственном явном shutdown handle.                                  | Вариант выбора            |
| `tokio-stream`        |        0.1.19 | Stream wrappers для Tokio channels, intervals и I/O events.                          | Для live/changefeed/bridge adapters; public abstractions лучше выражать через `Stream` trait.                                       | Основа                    |
| `try-lock`            |         0.2.5 | Минимальный lock primitive для poll-based state machines.                            | Только внутри существующего low-level dependency/primitive; `parking_lot`/atomics обычно яснее.                                     | Специализированный резерв |

### 8.6. Графовая математика, геометрия, статистика, ANN и ML

| Пакет                  |      Версия | Возможная будущая роль                                                                               | Когда выбирать / важное ограничение                                                                                                              | Оценка                    |
|------------------------|------------:|------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------|
| `diskann`              |      0.56.0 | Общие flat/graph ANN algorithms и provider traits.                                                   | После фиксации KNN correctness, visibility и filtering contract; сначала собрать адаптер к собственному snapshot/provider.                       | Сильный кандидат          |
| `diskann-bftree`       |      0.56.0 | Async provider поверх bf-tree и spill-to-disk.                                                       | Рассматривать как пример tiering/spill; его storage backend не совместим автоматически с PageManager/WAL Ferros-Grynn.                           | Референс                  |
| `diskann-disk`         |      0.56.0 | Disk-resident ANN build/search/storage.                                                              | На отдельном этапе DiskANN после HNSW; нужен crash-safe build, versioned publication и Windows/Linux I/O proof.                                  | Сильный кандидат          |
| `diskann-inmem`        |      0.56.0 | In-memory ANN index/provider для HNSW/DiskANN experiments и small datasets.                          | Для раннего KNN vertical slice и oracle/benchmark; транзакционная visibility должна быть адаптирована.                                           | Сильный кандидат          |
| `diskann-label-filter` |      0.56.0 | Filter AST/evaluator, attribute store и Roaring postings для filtered ANN.                           | При KNN predicate pushdown; парсер фильтра не должен конкурировать с GQL parser, использовать уже проанализированный predicate IR.               | Сильный кандидат          |
| `diskann-linalg`       |      0.56.0 | SGEMM, SVD и projections для ANN/embedding preprocessing.                                            | Когда конкретный index build требует этих операций; сравнить с уже подключённым nalgebra/ndarray-linalg.                                         | Вариант выбора            |
| `diskann-quantization` |      0.56.0 | Scalar/binary/product/spherical/min-max quantization.                                                | Только если спецификация разрешит approximate/compressed representation или внутренний candidate stage сохраняет заявленную точность результата. | Специализированный резерв |
| `diskann-record`       |      0.56.0 | Versioned JSON manifest и binary sidecars для index build artifacts.                                 | Можно адаптировать для staging/checkpoints; не использовать как канонический database format. Пакет ориентирован на 64-bit.                      | Референс                  |
| `diskann-tools`        |      0.56.0 | CLI/utilities для подготовки, инспекции и benchmark ANN datasets/indexes.                            | Для dev/admin tooling и conformance fixtures; не тянуть tool API в ядро.                                                                         | Специализированный резерв |
| `diskann-utils`        |      0.56.0 | Shared views, reborrow, futures/I/O/pool/sampling helpers.                                           | Переиспользовать выборочно; проверить little-endian и unsafe/platform assumptions.                                                               | Вариант выбора            |
| `diskann-vector`       |      0.56.0 | SIMD distances, conversion, contains и prefetch для vector search.                                   | До написания собственных distance kernels; может быть полезен даже при отказе от остального DiskANN provider.                                    | Сильный кандидат          |
| `diskann-wide`         |      0.56.0 | Безопасная cross-architecture SIMD dispatch.                                                         | При унификации vector kernels на x86/ARM; подтвердить реальные target и dispatch overhead.                                                       | Сильный кандидат          |
| `geo`                  |      0.33.1 | GQL geometry predicates, distances, intersections и R-tree preprocessing.                            | На этапе geo type/R-tree; закрепить coordinate/CRS/NaN/precision semantics до публикации API.                                                    | Сильный кандидат          |
| `nalgebra`             |      0.35.0 | Graph matrices/tensors, small dense linear algebra, transformations и algorithm reference.           | Для graph analytics и fixed/small dimensions; dynamic columnar workloads могут лучше лечь на ndarray/Arrow.                                      | Основа                    |
| `ndarray`              |      0.17.2 | Dynamic tensor batches, embeddings, analytics и ML feature arrays.                                   | На tensor/analytics вертикали; включённый BLAS влияет на native toolchain и должен пройти Windows/Linux build proof.                             | Сильный кандидат          |
| `ndarray-conv`         |       0.6.1 | N-D convolutions для signal/image/graph feature processing.                                          | Только при конкретной UDF/ML задаче; не часть обязательного GQL ядра.                                                                            | Специализированный резерв |
| `ndarray-einsum`       |       0.9.0 | Выразимые tensor contractions для аналитики и model preprocessing.                                   | После появления tensor expression contract; проверить planning/cost и shape validation.                                                          | Специализированный резерв |
| `ndarray-histogram`    |       0.5.0 | Гистограммы аналитики и потенциально statistics tooling.                                             | Для user analytics или offline optimizer-stat experiments; долговечный optimizer histogram требует versioned bounded schema.                     | Вариант выбора            |
| `ndarray-interp`       |       0.6.0 | Интерполяция рядов/геоданных/feature preprocessing.                                                  | Только для конкретных analytics/UDF semantics.                                                                                                   | Специализированный резерв |
| `ndarray-inverse`      |       0.1.9 | Matrix inversion в малых аналитических/ML задачах.                                                   | После появления алгоритма с доказанной необходимостью; прямое решение системы часто устойчивее инверсии.                                         | Специализированный резерв |
| `ndarray-linalg`       |      0.18.1 | BLAS/LAPACK decompositions/solvers для tensor analytics и index build.                               | При измеренной потребности; native backend, licensing, reproducibility и platform packaging — часть решения.                                     | Вариант выбора            |
| `ndarray-stats`        |       0.7.0 | Batch statistics, quantiles и summary analytics.                                                     | Для tensor/column analytics и offline stats build; не заменяет streaming/persistent catalog stats автоматически.                                 | Сильный кандидат          |
| `ndarray-utils`        |       0.1.4 | Shape/axis/array helpers вокруг analytics pipeline.                                                  | Подключать вместе с реальным ndarray consumer; не строить общий util layer заранее.                                                              | Специализированный резерв |
| `nexus-stats`          |       6.0.1 | Fixed-memory O(1) streaming estimators для latency, throughput, degree и adaptive runtime telemetry. | Для EXPLAIN ANALYZE/admission/operations; HLL/cardinality catalog и cost histograms проектировать отдельно.                                      | Сильный кандидат          |
| `ort`                  | 2.0.0-rc.13 | ONNX inference для embedding, graph ML, model-backed UDF и bridge enrichment.                        | Только после product contract моделей; RC + CUDA/WebGPU/native runtimes требуют feature gating, packaging и resource isolation.                  | Специализированный резерв |
| `rand`                 |      0.10.2 | ANN/index build, graph algorithms, sampling, property tests и deterministic randomized plans.        | Всегда передавать seeded RNG там, где reproducibility наблюдаема; entropy не должна менять query semantics.                                      | Основа                    |

### 8.7. Ошибки, генерация, конфигурация, тестирование и наблюдаемость

| Пакет                   |  Версия | Возможная будущая роль                                                             | Когда выбирать / важное ограничение                                                                                         | Оценка           |
|-------------------------|--------:|------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------|------------------|
| `anyhow`                | 1.0.104 | Удобные ошибки в prototypes, benches, one-shot tools и build/admin utilities.      | Не использовать в production library boundary: там нужен `FerrosGrynnError`/GQLSTATUS с машинно-читаемой классификацией.    | Вариант выбора   |
| `cov-mark`              |   2.2.0 | Явная проверка сложных parser/optimizer/error-recovery ветвей.                     | Когда branch трудно покрыть по имени обычным assert; не заменяет behavioral assertions.                                     | Сильный кандидат |
| `criterion`             |   0.8.2 | Статистические benchmarks storage/index/query/graph primitives и альтернатив.      | В каждой точке выбора конкурирующих структур; фиксировать workload и regression thresholds.                                 | Основа           |
| `derive_more`           |   2.1.1 | Снижение безопасного boilerplate newtype conversions/display/errors.               | Для стабильных простых derives; публичный API и error context не прятать за чрезмерной генерацией.                          | Вариант выбора   |
| `env_logger`            | 0.11.11 | Быстрый logging setup в examples/tests/tools.                                      | Для dev binaries; production наблюдаемость строится на `tracing`.                                                           | Вариант выбора   |
| `ferros-grynn-macros`   |   0.1.0 | `Measurable`, `StructureAccumulator`, generated GQLSTATUS и будущие узкие derives. | Основа уже принятой генерации; добавлять macro только при повторяемом стабильном контракте и compile-fail тестах.           | Основа           |
| `inventory`             |  0.3.24 | Distributed registry settings, UDF, analyzers, bridge factories и status metadata. | Когда plugin/distributed registration действительно нужен; explicit registry проще для deterministic/linker-sensitive ядра. | Вариант выбора   |
| `log`                   |  0.4.34 | Совместимость с зависимостями и простыми tools.                                    | Через bridge в tracing; новый production instrumentation писать структурированными spans/events.                            | Основа           |
| `macro_rules_attribute` |   0.2.3 | Применение declarative macros как attributes для повторяемых declarations.         | В status/config/schema catalogs, если это делает call site яснее и diagnostics остаются хорошими.                           | Вариант выбора   |
| `magic_utils`           |   0.1.1 | Вспомогательная compile-time/macro генерация, используемая локальным кодом.        | Сохранять для текущих macros; перед расширением проверить, не покрывает ли задачу обычный `macro_rules!`/proc macro crate.  | Вариант выбора   |
| `maplit`                |   1.0.2 | Короткие map/set fixtures в tests/spec examples.                                   | Только test/docs ergonomics; production construction должна показывать capacity/error semantics при необходимости.          | Вариант выбора   |
| `mimalloc-pprof`        |   0.9.5 | Allocation profiling на MSVC/mimalloc, включая test/benchmark diagnostics.         | При memory regression и budget work; нормальная и dev-зависимость дают два Cargo edge, но один пакет.                       | Сильный кандидат |
| `num_cpus`              |  1.17.0 | Physical/logical CPU heuristic для defaults и benchmarks.                          | Сначала `available_parallelism`/cgroup-aware telemetry; physical core count нужен только конкретному scheduler policy.      | Вариант выбора   |
| `num-derive`            |   0.5.1 | Generated numeric conversion для stable tags/enums.                                | Только с проверкой unknown values и versioned disk/GQL tags; не полагаться на unchecked casts.                              | Вариант выбора   |
| `obey`                  |   0.1.1 | Детерминированная orchestration/контроль конкурентных тестов.                      | Для MVCC, shutdown, handoff и race regression tests; не включать в production flow.                                         | Сильный кандидат |
| `paste`                 |  1.0.15 | Identifier composition в declarative macro catalogs/tests.                         | Для существующих generated declarations; не использовать ради скрытия нескольких явных имён.                                | Вариант выбора   |
| `pprof`                 |  0.15.0 | CPU/flamegraph profiling в benches и diagnostic builds.                            | При performance gate и regressions; feature/platform setup держать вне обычного SDK API.                                    | Основа           |
| `proptest`              |  1.11.0 | Законы codec/order, parser, MVCC schedules, graph algorithms и index equivalence.  | При каждом новом каноническом формате/структуре; shrinked regressions сохранять как обычные tests.                          | Основа           |
| `semver`                |  1.0.28 | Проверка совместимости plugin/model/index/tool versions и migrations.              | Когда появятся внешние расширения/versioned artifacts; disk format version остаётся собственным строгим типом.              | Сильный кандидат |
| `shuttle`               |   0.9.3 | Systematic concurrency testing async/sync schedules.                               | Для MVCC, async primitives, pools, background publication и shutdown; дополняет, а не заменяет Loom/unit tests.             | Основа           |
| `sysinfo`               |  0.39.6 | CPU/memory/process telemetry, `INFO SYSTEM` и resource defaults.                   | Для observation/admission; cgroup/container semantics проверить отдельно, данные не считать точным allocator accounting.    | Сильный кандидат |
| `thiserror`             |  2.0.20 | Реализация typed internal error enums и `source` chains.                           | Внутри слоёв с конвертацией в GQLSTATUS на boundary; не превращать весь API в строки.                                       | Основа           |
| `thousands`             |   0.2.0 | Читаемое форматирование counts/bytes в reports, benches и admin diagnostics.       | Только presentation layer; machine-readable output должен оставаться числовым.                                              | Вариант выбора   |
| `tokio-metrics`         |   0.5.2 | Runtime/task metrics для async I/O, live и bridge diagnosis.                       | Когда появится Tokio production path; не экспортировать высококардинальные per-task labels без budget.                      | Сильный кандидат |
| `tracing`               |  0.1.44 | Структурированные spans/events через query, transaction, I/O и jobs.               | Основа наблюдаемости; IDs и поля должны быть стабильными, bounded и redacted.                                               | Основа           |
| `tracing-forest`        |   0.3.1 | Иерархическое человекочитаемое представление span tree для dev/tests.              | Для локальной диагностики plan/execution; production sink выбирается отдельно.                                              | Вариант выбора   |
| `tracing-futures`       |   0.2.5 | Instrumentation futures/streams и перенос span context.                            | Для custom futures и bridge adapters; современные `.instrument()` возможности tracing проверить перед обёртками.            | Вариант выбора   |
| `tracing-subscriber`    |  0.3.23 | Filters/layers/format/registry для host-configurable telemetry.                    | Основа host binaries; library должна эмитировать spans, а subscriber устанавливает владелец процесса.                       | Основа           |
| `tracing-tracy`         |  0.12.0 | Экспорт tracing spans в Tracy для timeline/performance analysis.                   | При latency/performance gate; выключать в обычных builds.                                                                   | Сильный кандидат |
| `tracy-client`          |  0.19.0 | Нативные zones, plots, frame/allocator events для глубокого профилирования.        | Для targeted benchmark/diagnostic builds; не рассыпать Tracy-specific API по domain коду.                                   | Сильный кандидат |

<!-- direct-dependencies:end -->

## 9. Точки выбора по этапам

Этот раздел превращает каталог в практический механизм: когда команда входит в этап, она сначала открывает указанный набор существующих вариантов и только затем решает, нужен ли новый код.

| Точка продукта         | Обязательно рассмотреть до реализации                                                                         | Минимальное доказательство                                                                                                             |
|------------------------|---------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------|
| SchemaCatalog/config   | `src/datastore/schema`, `src/utils::ParseBytes`, `inventory`, `phf`, `indexmap`, `schemars`                   | Typed setting/schema declaration, deterministic iteration, validation diagnostics, no hidden global initialization.                    |
| Optimizer statistics   | `src/storage/graph_pages/statistics`, `nexus-stats`, `ndarray-histogram`                                      | Error bounds, update/merge cost, persistence/versioning, representative cardinality tests.                                             |
| Ordered index keys     | `values::storage::order_key`, `storekey`, `binrw`, `arrayref`                                                 | Property tests: runtime order equals byte order; prefix/range laws; malformed input and version migration.                             |
| Volatile ordered index | `ferntree`, `BTreeMap` + `parking_lot`, `papaya` where ordering is not needed                                 | Read/write/range benchmark, memory, retry semantics, reclamation and shutdown.                                                         |
| Durable B+tree         | `disk_storage_spec`, page manager, `ferntree` only as algorithmic reference                                   | Crash matrix around split/merge/root publication; WAL/recovery conformance; bounded memory bulk build.                                 |
| Bulk import            | `ext-sort`, `tempfile`, `rayon`, `bytes`, `scopeguard`                                                        | Dataset larger than RAM; deterministic result; cancellation/crash cleanup; Windows path/I/O; duplicate policy.                         |
| Parser/AST growth      | Current `syn`, SurrealDB lexer/AST arena, `reblessive`, `logos`, `compact_str`, `thin-vec`                    | Full grammar spans/errors, deep nesting without stack overflow, fuzz/property corpus, compile-time comparison.                         |
| Relational execution   | Current RowCursor/IR, Arrow, DataFusion, `serde_arrow`                                                        | Same GQL observable result/errors/order/null semantics; memory budget and cancellation; no storage DTO leakage.                        |
| Graph traversal        | `src/graph`, current graph cursors, `roaring_graphs`, `roaring`, `genawaiter`                                 | Differential tests, multigraph/meta/nested semantics, TRAIL/WALK constraints, bounded frontier memory.                                 |
| Fulltext               | SurrealDB FullTextIndex, `fst`, `rust-stemmers`, `deunicode`, `regex`, `memchr`, `roaring`                    | Analyzer versioning, deterministic term/posting layout, transactional visibility, ranking fixtures, bounded compaction.                |
| HNSW/DiskANN           | Все двенадцать `diskann-*`, `half`, `quick_cache`, `rayon`, vector SIMD                                       | Recall/precision contract, filtered KNN, snapshot visibility, crash-safe publication, memory/disk/build latency, cross-platform proof. |
| Geo/R-tree             | `geo`, `geojson`, numeric/value codecs                                                                        | CRS/precision/NaN contract, exact predicate fixtures, bounding-box false-positive refinement, index recovery.                          |
| Changefeed/LIVE        | MVCC/versionstamp, async ordered streams/watch/notify, SurrealDB changefeed/live, `async-channel`/`crossfire` | Commit-order determinism, resume token, slow subscriber backpressure, disconnect/reconnect, schema change and recovery.                |
| Background jobs        | `future-queue`, `asyncband`, Tokio, waitgroup/shutdown, ULID/UUID                                             | Weighted admission, cancellation, durable progress/restart, single publication, graceful close.                                        |
| Bridge API             | Public `Value`/`RecordId`, Arrow/Serde DTO, `async-trait`/`dynify`, streams/channels                          | No internal ID/DTO leakage, bounded batching, error mapping, cancellation and capability negotiation.                                  |
| Tensor/ML              | ECS, `nalgebra`, `ndarray-*`, `ort`, DiskANN vector layers                                                    | Отдельная feature/build boundary, deterministic preprocessing, model/version/resource limits, no effect on core storage format.        |
| Linux async I/O        | `src/storage` Sync/Page I/O, `src/asynchronous`, Tokio, `nix`, platform modules                               | Один общий I/O contract, aligned/direct tests, cancellation, short I/O, fsync/durability parity с Windows.                             |

## 10. Реестр «не изобретать заново»

Короткий список заделов, которые легко забыть при начале новой реализации:

1. **Graph/MVCC/WAL/I/O:** сначала текущие `src/graph`, `src/datastore`, `src/storage`, спецификации и production-тесты; `fg-meta` допускается только как сырой дополнительный пример после проверки.
2. **SSI/savepoints/version GC:** дополнительно `surrealmx` как сравнительный алгоритм и источник schedule tests.
3. **Concurrent ordered map:** `ferntree` до новой B+tree-in-memory реализации.
4. **Порядок составных ключей:** `storekey` и текущий `order_key` до нового encoder.
5. **Typed configuration:** реальные требования, `datastore::options`, `src/utils::ParseBytes` и уже подключённые registry/schema crates до нового config framework.
6. **GQL lexer/AST/planner:** текущий `syn` и SurrealDB-код до ещё одного parser framework.
7. **Traversal algorithms:** текущий `src/graph` и `roaring_graphs` до новой BFS/path/frontier реализации.
8. **Fulltext:** SurrealDB FullTextIndex + `fst`/stemmer/normalization набор до собственного монолитного индекса.
9. **ANN:** каждый слой DiskANN-семейства оценивать отдельно до написания vector kernel/provider/storage/filter.
10. **External sort/import:** `ext-sort` до собственного run/merge engine.
11. **Streaming statistics:** `nexus-stats` до ручных rolling metrics; долговечную optimizer statistics всё равно проектировать отдельно.
12. **Async coordination:** `asyncband`, `future-queue`, existing `src/asynchronous` и Tokio до новых channel/watch/shutdown primitives.
13. **Columnar execution:** Arrow/DataFusion/serde_arrow до собственного batch format, но только за runtime/public Value boundary.
14. **Graph analytics/ML:** ECS + nalgebra/ndarray/ORT до нового tensor/component слоя.
15. **Profiling/concurrency proof:** pprof/Tracy/Shuttle/Proptest/Obey до ad-hoc диагностик и sleep-based race tests.

## 11. Архитектурные ограничения на будущую интеграцию

Широкий взгляд в будущее не означает подключать всё сразу. Существующие варианты сохраняют ценность, если интеграция соблюдает несколько правил.

### 11.1. Один владелец семантики

- GQL parser не должен сосуществовать с отдельным production-парсером фильтров DiskANN или fulltext DSL, если выражение уже является частью GQL.
- `Value` и `RecordId` остаются публичным контрактом; Arrow, Serde, SurrealDB и DiskANN records — адаптеры.
- On-disk pages/WAL/order keys определяются спецификацией, а не удобным форматом стороннего крейта.

### 11.2. Один выбранный primitive на boundary

В manifest есть несколько семейств channels, maps, streams и locks. Это полезный выбор, но не повод использовать каждое одновременно. Для каждого boundary нужно зафиксировать topology и требования:

- SPSC/MPMC, sync↔async;
- bounded/unbounded и backpressure;
- fairness и ordering;
- cancellation/close;
- runtime-neutrality;
- memory reclamation.

После этого сравниваются только подходящие варианты. Например, `crossfire` может выиграть для sync IoRing worker → async Tokio, а `async-channel` — для runtime-neutral bridge; это разные случаи, а не глобальный рейтинг пакетов.

### 11.3. Формат задела может быть неверен, алгоритм — полезен

Это особенно относится к `radixdb`, `surrealmx`, `storekey`, `ferntree` и DiskANN record/provider слоям. Их не следует удалять из интеллектуальной карты из-за несовпадения формата. Нужно отдельно извлекать:

- инварианты;
- state machine;
- алгоритм;
- тестовые сценарии;
- measured trade-offs;
- только затем — переносимый код.

### 11.4. Опциональные вертикали должны иметь build boundary

Arrow/DataFusion, ndarray/BLAS, ORT/CUDA/WebGPU и часть DiskANN заметно влияют на compile time, native toolchain и размер поставки. Когда дойдёт очередь до них, разумный результат анализа может быть одним из трёх:

1. core feature с обязательным контрактом;
2. opt-in feature/отдельный crate adapter;
3. dev/reference-only зависимость.

Это решение о поставке, а не об обесценивании кода.

## 12. Предлагаемый процесс сохранения и выбора

1. Считать этот отчёт индексом возможностей и ссылаться на него из соответствующих будущих этапов.
2. При старте этапа создавать короткий decision record: требования, 2–4 существующих кандидата, минимальный прототип, измерения, выбранный вариант.
3. Не удалять задел только по результату `rg` «нет импортов». Нужна отдельная причина: противоречие спецификации, неприемлемая безопасность/лицензия, полностью лучшая сохранённая альтернатива или стоимость, подтверждённая измерением.
4. Если зависимость мешает сборке или поставке задолго до потребителя, можно вынести её за feature/dev/reference boundary, сохранив исходный анализ и локальный прототип. Это лучше, чем забыть существующее решение.
5. После выбора переносить только необходимый слой. Например, можно использовать `diskann-vector`, не принимая `diskann-record`; или тесты порядка `storekey`, не меняя canonical codec.
6. Для отвергнутого кандидата фиксировать конкретную причину и условие пересмотра, а не формулировку «сейчас нет потребителя».

## 13. Итоговый прогноз

Большая часть нынешних зависимостей и неподключённого кода имеет правдоподобную область применения в уже заявленной архитектуре. Особенно хорошо будущие требования объясняют наличие следующих крупных групп:

- async/concurrency — changefeed, LIVE, bridges, jobs и Linux async I/O;
- text/FST/normalization — analyzer/fulltext и tooling;
- Arrow/DataFusion — columnar `SELECT`, analytics и bridges;
- Roaring/radix/order-key/B+tree experiments — вторичные, path и posting indexes;
- DiskANN/linear algebra/SIMD — HNSW/DiskANN и filtered KNN;
- geo — geometry/R-tree;
- ndarray/ORT/ECS — поздняя tensor/ML вертикаль;
- tracing/profiling/system statistics — GA performance gates и эксплуатация;
- property/concurrency testing — доказательство MVCC, recovery, codec и async primitives.

При этом не все пакеты обязательно станут production dependencies. Многие — конкурирующие варианты или хорошие референсы. Их ценность состоит в том, что они сужают будущий поиск и не дают заново писать уже имеющийся алгоритм без сравнения.

Главный практический вывод: **до начала каждой будущей подсистемы сначала проверять реестр из разделов 9–10, а решение принимать по контракту спецификации и локальному доказательству, а не по факту сегодняшнего импорта**.
