# Ferros-Grynn GrynQL Tech Spec — Спецификация реализации механизмов языка запросов

**Версия:** 0.3-draft
**Статус:** проектирование
**Определяется файлами:** [gql_spec.md](gql_spec.md) (семантика), [gql_grammar.md](gql_grammar.md) (синтаксис), [disk_storage_spec.md](disk_storage_spec.md) (хранилище)

---

## 0. Введение и охват

Этот документ описывает **техническую реализацию** языка запросов GrynQL и то, как исполнительная система движка связывается с хранилищем. Он вторичен по отношению к трём базовым спецификациям и не должен им противоречить:

- **[gql_spec.md](gql_spec.md)** — *что* означает запрос (семантика, система типов, транзакционная модель, логический IR §26, правила оптимизатора §27). Является контрактом на поведение.
- **[gql_grammar.md](gql_grammar.md)** — *как* запрос записывается (лексика, EBNF, приоритеты, зарезервированные слова, замечания для парсера). Является контрактом на синтаксис.
- **[disk_storage_spec.md](disk_storage_spec.md)** — *где* и *в каком виде* лежат данные (страницы, `RecordId`/`DiskAtomRef`, WAL, checkpoint/recovery, индексы, статистика). Является контрактом на хранилище.

**Охват этого документа:**
1. Конвейер обработки запроса (§2) — от текста до результата.
2. Фронтенд: лексер и парсер (§3).
3. Семантический анализ, биндинг, типизация (§4).
4. Логический IR и понижение AST → IR (§5).
5. Оптимизатор: rule-based + cost-based (§6).
6. Физические операторы (§7).
7. Traversal VM — исполнение pipeline (§8).
8. Модель исполнения: vectorized + push-based (§9).
9. Взаимодействие исполнителя с хранилищем (§10).
10. Транзакции, MVCC и снимки в исполнении (§11).
11. WAL, checkpoint, recovery со стороны исполнителя (§12).
12. Индексы в исполнении (§13).
13. Управление ресурсами и деградация (§14).
14. Детерминизм и воспроизводимость (§15).
15. Доказательства корректности ключевых правил (§16).
16. Соответствие кодовой базе (§17).
17. Отложено / вне MVP (§18).

**Вне охвата:** физический формат страниц и байтовые раскладки (это `disk_storage_spec.md`); грамматика (это `gql_grammar.md`); семантика операторов языка (это `gql_spec.md`).

> **Важно об архитектуре хранения.** Ранние черновики этого файла описывали LSM-дерево (L0/L1/compaction). Это устранено: и `disk_storage_spec.md` (§20.2.8 — LSM отклонён в пользу B+tree), и `implementation_notes.md` фиксируют модель **WAL + snapshot + fuzzy checkpoint + single-pass physiological REDO + MVCC**. Данный документ следует этой модели. См. §31.2 `gql_spec.md`.

---

## 1. Ключевые контракты ядра

### 1.1. Предсказуемость под нагрузкой

1. Фоновые процессы не «едят» latency-бюджет под нагрузкой. Maintenance (flush, cleanup, compaction, checkpoint) ограничен по влиянию: фоновая работа может быть медленнее, зато p99 не «срывается» в неподходящий момент.
2. Длинные транзакции и горячие конфликты под контролем ядра: лимиты на длительность и размер транзакций по классам workload. Контролируемый abort с диагностикой, а не «всё зависло непонятно почему».
3. Деградация планов — не сюрприз. Есть управляемые рамки: что может поменяться автоматически, а что — только явным действием.
4. Режимы деградации описаны заранее: что происходит на пике, что отбрасывается первым, что приоритизируется.

| Компонент         | Граница                                            | При нарушении                         |
|-------------------|----------------------------------------------------|---------------------------------------|
| BufferPool        | `FG_PAGE_BUFFER_PAGES` — RAM-бюджет на кеш страниц | Eviction (Clock-Pro), WAL-first flush |
| TnxWriteSet       | `tnx_max_write_set_mb` — лимит на транзакцию       | Reject DML                            |
| UndoStore         | `undo_max_size_mb` — лимит UNDO-истории в памяти   | Reject writes                         |
| Statement timeout | `statement_timeout_ms` / `TIMEOUT` clause          | Cancel query                          |
| Snapshot age      | `max_snapshot_age`                                 | Force-close stale snapshots           |
| Path expansion    | `FG_PATH_MAX_DEPTH` (30), `MAXDEPTH`               | Обрыв path-finding                    |
| Graph expansion   | `FG_GRAPH_EXPANSION_MAX_NODES/EDGES`               | Обрыв построения графа                |

Fail-closed неприятнее в моменте, но честнее в эксплуатации: система сразу предлагает retry, circuit breaker, fallback или отказ.

### 1.2. Contract-first подход через код

Архитектурные контракты выражаются через код: traits, инварианты типов, паттерны. Ключевые интерфейсы-границы:

- **`PageManager`** (`disk_storage_spec.md` §29.2) — страничный I/O: `alloc_page`, `free_page`, `read_page`, `write_page_dirty(…, lsn)`, `flush_file`, `flush_all`, `page_count`.
- **`Index`** (`disk_storage_spec.md` §20.1) — единый интерфейс индексов: `insert`, `delete`, `lookup`, `range`, `rebuild`, `status`.
- **`TransactionLogSink`** — приёмник WAL-записей (append + fsync-барьеры).
- **`Operator`** (§7, §9) — узел плана исполнения: `next_batch() -> Option<Batch>`.
- **`StorageIo`** (`SyncIo` / `AsyncIo` из `fg-meta`) — Direct I/O с опциональной async-обёрткой.

Интерфейсы фиксируют, что система обязана уметь, где проходят её границы, и какие инварианты держатся.

### 1.3. Изоляция бизнес-логики от data engine

Чем больше пользовательской логики живёт в хранилище, тем сложнее её тестировать, версионировать и наблюдать. Поэтому пользовательские функции (`DEFINE FUNCTION`), события (`DEFINE EVENT`), замыкания и cost-функции исполняются в **песочнице** с явными ограничениями: `MAXDEPTH` для рекурсии, запрет бесконечных циклов (`FOR`/`tv::repeat` требуют `times(n)`/`until`), классификация purity (§4.6).

### 1.4. Гибридная асинхронность: сначала стабилизировать, потом ускорять

Делать всё синхронным — плохо; делать весь движок асинхронным сразу — ад для отладки (особенно при одновременной постройке storage engine, WAL, recovery, MVCC, buffer manager). Выбираем гибрид: для Storage/WAL/MVCC — отдельные интерфейсы; дефолтная реализация — sync (`SyncIo`), async (`AsyncIo`) — дополнительно. Пользователь выбирает, что использовать.

---

## 2. Конвейер обработки запроса

Точка входа — `Datastore::execute(text: &str, vars: Option<Variables>) -> Vec<QueryResult>` (`src/gql/ds.rs`). Каждый оператор верхнего уровня возвращает свой `QueryResult { time, query_type, result: FerrosGrynnResult<AnyValue> }` (`src/dbs/response.rs`).

Конвейер (соответствует диаграмме §22.1 `gql_spec.md`):

```
Текст запроса
   │
   ▼  §3 Фронтенд
[Lexer] → поток токенов → [Parser] ──► AST (Ast { expressions: Vec<TopLevelExpr> })
   │
   ▼  §4 Анализ
[Binder] scope/alias → [TypeChecker] → Typed AST
   │
   ▼  §5 Понижение (split declarative / traversal)
[Lowering] ──► Logical IR (унифицированный, §26 gql_spec)
   │
   ▼  §6 Оптимизатор
[RuleEngine] rule-based rewrite → [CostEngine] plan selection ──► Logical Plan → Physical Plan
   │
   ▼  §7–§9 Исполнение
[Execution] Pattern Engine + Traversal VM + Relational Engine
   │           └── §10 доступ к хранилищу (PageManager / BufferPool / Index / Stats)
   │           └── §11 снимок MVCC
   ▼
QueryResult (AnyValue)
```

Каждая стадия — отдельный компонент с явным контрактом входа/выхода. Между стадиями данные не «протекают» синтаксисом: логический IR не хранит синтаксис (§26 gql_spec, «IR НЕ ДОЛЖЕН хранить синтаксис»).

Разбиение (Split) на **декларативную** ветвь (`SELECT`/`MATCH`) и **императивную** ветвь (Traversal pipeline) происходит на уровне AST → IR: декларативная часть идёт в join/relational engine, императивная — в Traversal VM (§8). Обе ветви сходятся в **едином** логическом IR (риск смешанной семантики фиксируется границей IR, §32.2 gql_spec).

---

## 3. Фронтенд: лексер и парсер

Реализация — `src/syn` (`lexer`, `token`, `parser`, `error`). Архитектура заимствована из SurrealQL-подхода и адаптирована под GrynQL-грамматику.

### 3.1. Лексер

- Токены соответствуют лексической грамматике `gql_grammar.md` §2: идентификаторы (в т.ч. `` `quoted` ``), параметры `$x`, пространства имён `ns::ident`, числа (`int`/`float`), строки (`'…'`, `"…"`, `"""…"""`), темпоральные литералы (`d'…'`, длительности), `record_id`, коллекции.
- **Span** — `{ offset: u32, len: u32 }` (`src/syn/token`). Ограничение: запрос ≤ `u32::MAX` байт (иначе `FerrosGrynnError::query_too_large()`).
- Составные токены (`d'…'`, regex, длительности) лексируются лениво через `lex_compound` — по стартовому токену вызывается специализированный дочерний лексер.
- Пробелы значимы **только** между соседними токенами там, где грамматика это требует (`record_id` = `table:id` без пробела; `follows_from` в парсере). Для этого лексер сохраняет whitespace-разделение, а `peek_whitespace*` в парсере это учитывает.

### 3.2. Парсер

`Parser<'a>` (`src/syn/parser`) — рекурсивный нисходящий парсер поверх **`reblessive`** (stackless async-рекурсия). Это принципиально: GrynQL допускает глубоко вложенные подзапросы, блоки, объекты и pipeline; stackless-стек (`Stk`) избегает переполнения нативного стека на патологически вложенных запросах.

Механика:
- **`TokenBuffer<4>`** — буфер на 4 токена вперёд: `peek`, `peek1`, `peek2`, `peek_token_at(n)`. Даёт до `LL(4)` look-ahead без отката.
- **`speculate()`** — спекулятивный разбор ветки с откатом (`backup_after`). Применяется только там, где ветку нельзя определить по n-му токену (см. неоднозначности `gql_grammar.md` §16). Отмечен как «мощный, но с недостатками» — по возможности предпочитать `peek`.
- **`table_as_field`** — контекстный флаг для разрешения `ident` как имени таблицы vs поля.
- Расширенные операторы разбираются по таблице приоритетов `gql_grammar.md` §12.1 методом **precedence-climbing (Pratt)**; результат — `Expr` (`src/gql/expression.rs`): `Literal`/`Param`/`Table`/`Range`/`Block`/`Constant`/`PrefixOp`/`PostfixOp`/`BinaryOp`/…

### 3.3. Разрешение синтаксических неоднозначностей

Парсер обязан однозначно разрешать перегруженные символы из `gql_grammar.md` §16:

| Символ            | Роли                                                                                        | Стратегия                                                                                                                                        |
|-------------------|---------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------|
| `<`               | меньше / cast `<T> e` / дженерик `is<T>(…)` / аннотация графа `<dag>` / метавершина `<(…)>` | по позиции: после `RETURN GRAPH`/`CONSTRAINTS` → структурная; в позиции унарного операнда → cast; `<` перед `(`/`[` → мета-атом; иначе сравнение |
| `\|`              | OR `\|\|` / альтернатива меток `:A\|B` / замыкание `\|$x\|` / range-генератор `\|t:n\|`     | `\|\|` — единый токен OR; остальное — по контексту                                                                                               |
| `\|>` / `<\|…\|>` | pipe / KNN                                                                                  | `\|>` и `<\|`,`\|>` — единые токены                                                                                                              |
| `~`               | AtomWalk (`~>`, `~~`, `~[`, `]~>`, `<~[`)                                                   | жадная токенизация только после `MATCH ATOM`/`TRAVERSE ATOM`                                                                                     |
| `@`               | fulltext (`@@`, `@1@`, `@AND@`, `@OR@`)                                                     | единые токены                                                                                                                                    |
| стрелки           | `->`, `<-`, `<->` в паттернах и `RELATE`                                                    | часть `edge_pattern`                                                                                                                             |

### 3.4. Пределы разбора

`ParserSettings` (`src/syn/parser`): `object_recursion_limit` (default 100 — глубина объектов/массивов), `query_recursion_limit` (default 20 — глубина вложенных операторов/блоков). Значения берутся из `Config` (`max_object_parsing_depth`, `max_query_parsing_depth`) через `settings_from_capabilities`. Эти лимиты — часть контракта предсказуемости (§1.1): защита от стек-бомб и от планов недопустимой сложности.

### 3.5. Ошибки парсинга

Ошибки — `SyntaxError { diagnostic: Box<Diagnostic> }` (`src/syn/error`). `Diagnostic` — **цепочка** записей `DiagnosticKind::{Cause, Span{kind, span, label}}` (связаны через `next`), что позволяет накапливать контекст («ожидался разделитель», «этот делимитер должен был закрыться здесь»). При выдаче ошибка рендерится на исходных байтах (`render_on_bytes` → `RenderedError`/`Snippet`/`Location`, с подсветкой участка) и конвертируется в `FerrosGrynnError::query_parse_error(...)`.

Это — часть общей диагностической системы `gql_status`: `Status → DiagnosticRecord → ErrorState → FerrosGrynnError` (см. CLAUDE.md, `src/gql_status`). Синтаксические статусы — подкласс `ClientError` (§4.7 gql_spec).

---

## 4. Семантический анализ, биндинг и типизация

Вход — AST; выход — **Typed AST**. Три фазы (соответствуют §60 задела):

### 4.1. Фаза 1 — Binding (scope, alias)

- Построение областей видимости биндингов: `Env = Map<Alias, Value>` (§14 gql_spec).
- Правила биндингов §14.2: объявление `(a:Person)` → `Env[a] = vertex<Person>`; повторный alias — это **ограничение**, не присваивание; `MATCH` создаёт новый scope, `WITH`/`LET` — сужают/расширяют.
- Проверка неопределённых алиасов, shadowing (§14.3): внутренний alias перекрывает внешний; при передаче через `WITH a, b` — проекция биндингов.
- Разрешение `record_id` (`table:id`) в публичную ссылку; проверка существования таблиц/графов через каталог (§10, `SchemaCatalog`, `GraphSuperblock`).

### 4.2. Фаза 2 — Type inference

Типизация `Γ ⊢ expr : T` (§24 gql_spec). Алгоритм — Hindley–Milner с расширениями для графовых и record-типов.

Типовые домены (§59.2 gql_spec):
1. Value: `int | float | decimal | string | bool | bytes | datetime | duration | …`
2. Record: `record<T> | record<T1 | T2>`
3. Graph: `vertex<T> | metavertex<T> | edge<T> | hyperedge<T> | metaedge<T> | atom<T> | path | subgraph | graph | tree | …`
4. Collection: `array<T> | set<T> | tuple<…> | bag<T>`
5. Function: `(T1,…,Tn) -> U`

Правила вывода — §24.9 gql_spec (T-Var, T-Field, T-FunctionCall, T-SELECT, T-MATCH, T-Traversal, T-Join). Idiom-навигация типизируется через `field(T, f) = τ`.

Graph-специфичные проверки:
- `MATCH`: согласованность меток, совместимость рёбер, endpoint-типы.
- Traversal-шаг: `out('FRIEND') : vertex<record<Person>> -> vertex<record<Person>>`.
- `path<T> = sequence(vertex<T>)`; функции `path::nodes/edges/atoms/length/cost` типизируются по типу пути (§15.6 gql_spec).
- `RETURN GRAPH <constraint> {…}`: тип результата — `subgraph` или заявленный тип графа; **система типов не делает неявных преобразований** (запрос `rooted_tree` при фактическом `forest` → ошибка; проверка **после** построения результата, §16.1 gql_spec).

### 4.3. Фаза 3 — Constraint solving

Уточнение типов из контекста: `a.age > 10` ⟹ `type(a.age) = number`. Унификация типовых переменных; проверка `record<A | B>` (union) — §24.4.

### 4.4. Разрешение перегрузки `number`

Единственная разрешённая перегрузка (§18.7 gql_spec) — числовой тип `number`, разрешаемый в `int`/`float`/`decimal` **на этапе анализа** (не в рантайме). Автоприведение `int → float → decimal` неявное; сужающее (`decimal → int`) — только через явный cast `<int> e`.

### 4.5. Три-значная логика и `null`/`none`

Типизация фиксирует различие `null` (∈ любой nullable-тип, участвует в `UNKNOWN`) и `none` (∉ любого `T`, ведёт себя как `false` в `WHERE`) — §5.1 gql_spec. Линт-предупреждение при смешивании (§32.1 gql_spec).

### 4.6. Classification purity

Каждая функция/выражение получает класс purity (`PURE | STABLE | IMPURE`, §18.5 gql_spec), считываемый из `FunctionDefinition.purity` (`disk_storage_spec.md` §18). Класс влияет на оптимизатор (§6): `PURE` можно инлайнить/кешировать/переставлять; `STABLE` фиксирована в рамках запроса (`time::now()` замораживается на снимок); `IMPURE` — барьер порядка.

### 4.7. Проверка computed-полей

`DEFINE FIELD … COMPUTED expr` строит граф зависимостей computed-значений; при обнаружении цикла — ошибка (§5.10 gql_spec). Тип помечается обёрткой `computed<T>`; `COMPUTED`-поля **не хранятся** на диске и вычисляются исполнителем при `SELECT` из `FieldDefinition.expr_bytes` (IR) (`disk_storage_spec.md` §18).

### 4.8. Soundness

Цель: `WellTyped(Q) ⟹ нет runtime type errors` (за исключением явно разрешённых dynamic-cast). Теоремы Preservation/Progress (§59.4 gql_spec). Unsound-зоны фиксируются явно: floating-point, time-dependent функции, randomness, side-effects (§61.2).

---

## 5. Логический IR и понижение (lowering)

Typed AST понижается в **унифицированный логический IR** (§26 gql_spec). IR: алгебраический, типизированный, переписываемый, платформенно-независимый; не хранит синтаксис и не зависит от API.

Узел IR (§26 gql_spec):
```
LogicalPlan.Node { op: LogicalOp, inputs: Vec<NodeId>, schema: Schema, props: LogicalProps }
Schema        = { columns: Vec<Column{ name, type, nullable }> }
LogicalProps  = { estimated_cardinality, ordering, distinct, keys, path_info }
```

### 5.1. Логические операторы

Минимальный набор (§22.2, §26 gql_spec): `Scan(source, filter?)`, `IndexScan(index, key_range, filter?)`, `Filter(pred)`, `Project(exprs)`, `Join(kind, cond)` (`Inner|Left|Semi|Anti`), `Expand(dir, edge_labels?, node_labels?, alias_node, alias_edge?)`, `ExpandEdge(dir, labels?, alias_edge)`, `PathExpand(dir, edge_labels?, min, max, mode, objective?, filters, alias_path, alias_end)`, `Aggregate(group_by, aggregates)`, `Sort(keys)`, `Limit(n, offset)`, `Dedup(keys)`, `Unwind(expr, alias)` (для `SPLIT`), `TraversalStep(step)`.

### 5.2. Правила понижения (grammar → IR)

Понижение связывает конструкции грамматики с операторами IR:

| Конструкция GrynQL                   | Понижение в IR                                                                   |
|--------------------------------------|----------------------------------------------------------------------------------|
| `SELECT f FROM t WHERE p`            | `Scan(t) → Filter(p) → Project(f)`                                               |
| `FROM a, b` (union)                  | `Union(Scan a, Scan b)`                                                          |
| `FROM a & b` (cross)                 | `Join(Inner, true)`                                                              |
| `SPLIT field`                        | `Unwind(field)`                                                                  |
| `GROUP [ALL\|BY]`                    | `Aggregate(group_by, aggs)`                                                      |
| `ORDER BY`                           | `Sort(keys)`                                                                     |
| `LIMIT n START s`                    | `Limit(n, s)`                                                                    |
| `(a:L)-[:E]->(b:L2)`                 | `Scan(L as a) → Expand(Out, E, b) → Filter(label(b)=L2)` (§26.3, §54.5 gql_spec) |
| `(a)-[:E*min..max mode]->(b)`        | `PathExpand(Out, E, min, max, mode, …)`                                          |
| `OPTIONAL MATCH`                     | `Join(Left, …)` (§23.5 инвариант 6)                                              |
| `MATCH DISTINCT` / `RETURN DISTINCT` | `Dedup(keys)`                                                                    |
| `RETURN GRAPH {nodes, edges}`        | `GraphAssemble(constraint?)` (терминальная проекция в `subgraph`)                |
| `BUILD GRAPH … FROM …`               | `GraphAssemble` + материализация в каталог                                       |
| `src \|> tv::step(...)`              | `TraversalStep(step)` поверх источника                                           |
| `TRAVERSE FROM g \|> …`              | цепочка `TraversalStep` над graph-источником                                     |

### 5.3. Две под-алгебры под одной крышей

- **Реляционная под-алгебра** (`SELECT`, соединения `MATCH`) → операторы Scan/Filter/Project/Join/Aggregate/Sort. Исполняется join/relational engine (§7).
- **Traversal IR** (`|>`, `TRAVERSE`) → цепочка `TraversalStep`, исполняется Traversal VM (§8).

Оба под-IR — узлы одного `LogicalPlan`, что позволяет смешивать стили (`FROM (MATCH …) |> tv::…`, `TRAVERSE FROM SELECT …`). Граница «MATCH → join engine, traversal → VM» фиксируется на уровне IR (§32.2 gql_spec, риск смешанной семантики).

### 5.4. Примеры IR — см. §26.3 gql_spec

`MATCH (a:Person)-[:FRIEND]->(b:Person) WHERE a.age>18 RETURN b.name` →
```
Scan(Person AS a) → Filter(a.age>18) → Expand(Out, FRIEND, b) → Filter(label(b)=Person) → Project(b.name)
```

---

## 6. Оптимизатор

`Optimizer = RuleEngine (logical rewrite) + CostEngine (plan selection)` (§27 gql_spec). Конвейер (§27.1): Normalize → Rule-based rewrite → Stats annotation → Generate alternatives → Cost evaluation → Choose plan → (опц.) adaptive hooks.

### 6.1. Rule-based правила

**Семантически строгие (всегда безопасны, §22.3, §16 доказательства):**
- Predicate pushdown: `Filter(Scan(T), p) → Scan(T, p)` (§16.1).
- Projection pruning: удалять поля, не используемые выше (§16.3).
- Expand + Filter fusion: `Expand → Filter(edge_pred) → Expand(edge_pred)` (§16.4 fusion).
- Constant folding: `1+2 → 3`.
- Dedup elimination — если ключ уже уникален.
- Late materialization — не материализовывать объекты до явного запроса (§7.3).
- Expand optimization: стартовать с узла минимальной степени/кардинальности; использовать adjacency index.

**Условно безопасные:** join reordering (inner join без side-effects, §16.2); OPTIONAL MATCH преобразования; path pruning при известном `max_depth`.

**Запрещены (§22.3, §16.6):** переставлять `IMPURE`; оптимизировать через явный `ORDER BY`; менять семантику пути (`SIMPLE`/`ACYCLIC`/`SHORTEST`); двигать операторы через side-effect барьеры.

### 6.2. Cost-based выбор

`P* = argmin_P Cost(P)` при `∀P: [[P]] = [[Q]]` (§57 gql_spec). Модель стоимости (§22.4–§22.11 gql_spec):
```
cost(op) ≈ rows_in × cost_per_row + overhead(cpu, io, memory)
rows_out(Expand)   ≈ rows_in × avg_degree × selectivity(edge_label)
rows(PathExpand,d) ≈ rows_in × avg_degree^d          -- самая опасная часть
```
Path-pruning-факторы: `effective_degree *= path_mode_factor × predicate_factor × dedup_factor` (Walk 1.0, Trail ~0.7, Simple ~0.3, Shortest ~0.05).

CostEngine решает: join ordering (Selinger + graph-эвристики), стартовую точку MATCH (минимальная кардинальность), выбор алгоритма пути (BFS/DFS/Dijkstra), index vs scan.

### 6.3. Источник статистики

Cost-model **обязан** иметь degree statistics — без них графовый планировщик деградирует (§32.1 gql_spec). Статистика читается из `StatisticsStore` (`disk_storage_spec.md` §19):

| Нужно планировщику                                      | Страница на диске       |
|---------------------------------------------------------|-------------------------|
| `node_count`, `edge_count`, `avg/max_out/in_degree`     | `GraphStatsPage`        |
| per-label cardinality (`atom_count`, `avg_prop_count`)  | `LabelStatsPage`        |
| per-property NDV, `null_fraction`, гистограммы freq+NDV | `PropertyHistogramPage` |
| per-label degree-гистограмма по направлению             | `DegreeHistogramPage`   |

`selectivity(label=Person) = |Person| / |nodes|` (из `LabelStatsPage`); `selectivity(age>18)` — histogram-based (из `PropertyHistogramPage`). Обновление статистики — инкрементально на DML и полностью на `ANALYZE`; авто-триггер при `dirty_row_fraction > FG_STATS_AUTO_ANALYZE_DIRTY` (0.10).

### 6.4. Штрафы и деградация плана

Штрафы (§22.11): `penalty_high_degree_nodes`, `penalty_unbounded_path`, `penalty_no_index`, `penalty_materialization`. Обязательное предупреждение планировщика при `degree > 10 AND depth > 4` (риск path explosion, §32.1). Без `max` в `[:E*]` применяется `MAXDEPTH` clause или `FG_PATH_MAX_DEPTH`.

### 6.5. `WITH`-подсказки

`WITH NOINDEX` — принудительный table scan; `WITH INDEX @i` — планировщик **обязан** использовать указанный индекс (§19.1 gql_spec), даже если optimizer предпочёл бы другой; `WITH COST fn` — пользовательская cost-функция пути (только `MATCH`/`TRAVERSE`).

### 6.6. Plan caching

Ключ кеша: `normalized_query + schema_hash` (§4.7). Кешируются нормализованные логические/физические планы. Инвалидация — при изменении схемы (`schema_hash`) или значимом сдвиге статистики. Для детерминизма — canonical plan selection (§15.5).

### 6.7. Физические альтернативы

Каждый логический оператор раскрывается в несколько физических (§4.2 задела): `Scan → {TableScan, IndexScan}`; `Join → {HashJoin, IndexJoin (NL), LeapfrogTrieJoin}`; `Expand → {ExpandAdjList, ExpandIndex}`; `PathExpand → {BFSExpand, DFSExpand, ShortestPathOp}`. Выбор — по cost model.

---

## 7. Физические операторы

### 7.1. Общая модель

Все физические операторы — `Iterator<Row>` (async: `Stream<Row>`), собираемые в конвейер (§9). Row:
```
Row = { values: Vec<Value>, env: Env?, path: Path? }
```

### 7.2. Scan-операторы

- **`TableScan { table_id, projection, predicate }`** — скан таблицы; predicate/projection проброшены внутрь (§16.1, §16.3). На диске — обход через `Label index` (`lbl_node_<id>.fgidx`) или primary index (`pk_<table_id>.fgidx`), затем чтение hot-slot (§10.4).
- **`IndexScan { index_id, key_range, predicate }`** — доступ через `Index::range/lookup` (§13).
- **`GraphScan { atom_type, label_filter }`** — старт с множества атомов графа по метке (`tv::V([labels])`).

### 7.3. Expand-операторы

- **`ExpandOp { input, adjacency, direction, label_filter }`** — одношаговое расширение по смежности:
  ```
  for each input node:
      for each edge in adjacency(node, direction, label):
          yield (node, edge, neighbor)
  ```
  Оптимизации: adjacency-кеш; сжатое хранение рёбер (inline + overflow, §10.4); degree-aware batching.
- **`ExpandEdgeOp`** — возвращает рёбра (для `tv::outE/inE/bothE`, §13.4 gql_spec).

### 7.4. PathExpand-оператор

```
PathExpandOp { input, adjacency, min_depth, max_depth, mode, objective?, frontier }
```
- **BFS** (frontier-based): очередь состояний, `emit` при `depth ≥ min`, расширение при `depth < max` и `allowed_by_mode` (§50.1). Bitmap-frontier + ранняя дедупликация (§15.4, §50.2).
- **DFS** — для перечисления путей и constrained-search с pruning.
- **Shortest** — BFS (невзвешенный) / Dijkstra (взвешенный), `weight(NaN) = 1` (§15.1 gql_spec).
- Path-representation: `Path = { nodes: Vec<NodeId>, edges: Vec<EdgeId> }`, хранится в arena + offsets, immutable-сегменты (§7.3, §13.9 gql_spec) → O(1) append.

### 7.5. Соединения (Join)

Выбор стратегии — по форме паттерна:

| Задача                           | Стратегия                                | Сложность                         |
|----------------------------------|------------------------------------------|-----------------------------------|
| Обход (out/in/both)              | Adjacency lookup (Expand)                | O(degree)                         |
| MATCH-паттерн-дерево             | **Index Nested Loop** через adjacency    | O(∏ degrees)                      |
| MATCH с циклом (triangle/square) | **Leapfrog TrieJoin** (Ngo et al. 2012)  | O(n^ρ), ρ = fractional edge cover |
| Несвязный MATCH по свойству      | Hash Join                                | O(n + m)                          |
| Атрибуты одного атома            | SparseSet O(1) lookup                    | O(1)                              |
| Несколько атрибутов              | late materialization / vertical grouping | O(min pools)                      |

**Index Nested Loop** — базовая стратегия для дерева-паттерна: стартовый узел минимальной кардинальности, разворот через adjacency без материализации промежуточных таблиц:
```
Scan(Person AS a) → Filter(a.age>18) → Expand(Out,FRIEND,b) → Filter(label(b)=Person)
                  → Expand(Out,LIKES,c) → Project(a.name, c.title)
```
= `O(|Person| × avg_degree_FRIEND × avg_degree_LIKES)`.

**Leapfrog TrieJoin** — для паттернов с циклами: переменные итерируются в фиксированном порядке, на каждом шаге — прыжки по отсортированным adjacency-спискам. Worst-case optimal (§18, отложено для MVP как усложнение — базовый вариант Index NL достаточен).

### 7.6. Late materialization и доступ к свойствам

Атрибуты атомов лежат отдельно от топологии (SoA/columnar, §10.4, `disk_storage_spec.md` §10–§13). Стратегии:
1. **Late materialization**: получить `AtomId` из первого фильтра (label) → применить WHERE-предикаты через lookup → грузить остальные поля только для прошедших фильтр строк (принцип column-store).
2. **Batch prefetch**: после `Expand` собрать батч `AtomId`, затем один batch-lookup свойств → последовательный доступ к `dense_c` вместо random.
3. **Vertical grouping** (для in-memory ECS): часто-совместные компоненты объединяются в один `SparseSet<Composite>` (устраняет второй lookup) — задел для аналитики/ML (`fg-meta` ECS).

---

## 8. Traversal VM — исполнение pipeline

Реализация — `src/gql/traversal` (`TraversalMachine`, `TraversalContext`, `TraversalStep`, `Traverser`). Семантика — §13 gql_spec.

### 8.1. Модель

Traversal — потоковая обработка `Stream<Traverser>`. Каждый шаг: `Stream<Traverser> → Stream<Traverser>`.
```
Traverser = {
    current: atom | value,   -- текущий элемент
    path:    Path?,          -- накопленный путь (по запросу)
    sack:    Value?,         -- локальное состояние (накопление)
    env:     Bindings,       -- биндинги из MATCH/LET
    bulk:    u64             -- счётчик одинаковых traverser'ов (компрессия)
}
```
`TraversalMachine::execute(graph, initial_ids) -> Iterator<Traverser>` строит начальный поток из `initial_ids` и последовательно применяет `steps`. `TraversalContext` держит `side_effects: RefCell<Map<String, AnyValue>>` — глобальное состояние запроса (агрегаты, счётчики).

### 8.2. Ключевая оптимизация — bulk

`bulk` (как в Gremlin) агрегирует одинаковые traverser-состояния без физического дублирования: N идентичных путешественников представляются одним с `bulk = N`. Шаги, не различающие traverser'ы, работают над `bulk` арифметически; ветвление/различающие шаги разворачивают bulk.

### 8.3. Классы шагов (§13.8 gql_spec)

| Класс          | Примеры                                  | Кардинальность             | Материализация |
|----------------|------------------------------------------|----------------------------|----------------|
| Map (1→1)      | `values`, `map`                          | не меняет                  | нет            |
| FlatMap (1→N)  | `out`, `in`, `both`                      | взрыв                      | нет            |
| Filter (1→0/1) | `has`, `where`                           | уменьшает                  | нет            |
| Barrier (N→N)  | `order`, `group`, `dedup`, `fold`        | —                          | **да**         |
| Side-effect    | `side_effect`, `sack`                    | не меняет поток            | нет            |
| Control flow   | `repeat`, `branch`, `choose`, `coalesce` | нелинейный execution graph | зависит        |

Полный список шагов пространства `tv::` — §13.4 gql_spec / `gql_grammar.md` §7.

### 8.4. Sack — состояние traverser'а

Два вида (§13.5 gql_spec): **global sack** (есть у всех traverser'ов по умолчанию, накапливается без инициализации) и **named sack** (создаётся вручную; доступ без инициализации — ошибка). Операции: `init_sack`/`init_named_sack`, `sack(closure)`/`named_sack(name, closure)` (модификация замыканием `(sack, current) -> sack`), `get_sack`/`get_named_sack`.

### 8.5. Path propagation

На каждом шаге: `new_path = old_path + edge + node` (§13.9). Хранение — persistent structure (rope / arena-index) → O(1) append, без копирования сегментов (§7.3). Path трекается **лениво** — только если запрошен (`tv::path()` или именованный путь).

### 8.6. Control flow

`repeat(pipeline)` — fixpoint-итерация с условиями остановки `.times(n)` / `.until(closure)` / `.emit()`. Обязателен конечный критерий (§18.6 gql_spec) — защита от бесконечных циклов. `branch`/`choose`/`coalesce` формируют нелинейный execution graph.

### 8.7. Материализация и барьеры

Материализация потока происходит только на: barrier-шагах (`order`, `group`, `dedup`, `fold`); явном `collect()`; границе (`RETURN`) (§13.7). Планировщик учитывает barrier'ы при оценке стоимости (блокируют pipeline-fusion).

### 8.8. Параллелизм

VM поддерживает (§13.10): **data parallelism** (партиционирование traverser'ов по потокам) и **step parallelism** (ограниченно — только для stateless-шагов и pure-функций). Cost hotspots (§13.11): fan-out на high-degree узлах, `repeat`, path-tracking. Обязательные оптимизации: adjacency index, degree-aware planning, path pruning.

### 8.9. Терминирование

Без явного терминирующего шага traversal возвращает `array<atom>`. Иначе: `values → array<scalar>`, `path → array<path>`, `project → array<object>`, `count → int`, `fold → array` (§13.6).

### 8.10. Взаимодействие с декларативной ветвью

Источник pipeline — любой оператор, возвращающий набор (`SELECT`, `MATCH`, `TRAVERSE`, массив, переменная). Понижение `src |> tv::step` → `TraversalStep` над IR-узлом источника (§5.3). Внутри одного запроса MATCH исполняется join-движком, traversal — VM; данные передаются через материализованный набор атомов на границе.

---

## 9. Модель исполнения

### 9.1. Гибрид vectorized + push-based

```
Execution = Pattern Engine (MATCH) + Traversal VM + Relational Engine
Модель: vectorized (next_batch() → Batch) + push-based (operator pushes downstream)
```
Vectorized даёт SIMD и cache-locality; push-based даёт pipeline-fusion и низкую latency. Оператор:
```rust
trait Operator { fn next_batch(&mut self) -> Option<Batch>; }
```

### 9.2. Данные исполнения

```
Batch  = { columns: Vec<Array>, size: usize }
Array<T> = { data: Vec<T>, validity: Bitmap }
Sel    = { indices: Vec<u32> }   -- selection vector: фильтрация без копирования, lazy materialization
```

### 9.3. Pipeline и pipeline-breakers

Линейный pipeline: `Scan → Filter → Expand → Project → Sink` исполняется как единый push-конвейер. Pipeline-breakers (требуют материализации): `Join (hash build)`, `GROUP BY`, `SORT`, `DISTINCT`. Execution graph: `source pipelines → intermediate → sink pipelines` (DAG).

### 9.4. Graph-специфичная векторизация

`Expand`: `batch(vertex_ids) → batch(neighbor_ids)`. Оптимизация — batch adjacency fetch: сгруппировать вершины → пакетно прочитать adjacency-блоки (§10.4). High-degree узлы → split на суб-батчи (защита от explosion, §44.4).

### 9.5. Adaptive execution

Runtime-решения: batch too large → split; skew → rebalance; выбор interpreter vs (будущий) JIT по размеру данных; re-optimization при обнаружении skew. Runtime может менять batch size, стратегию (vector vs row), алгоритм join.

### 9.6. Отмена и таймауты

Кооперативная отмена: операторы периодически проверяют флаг отмены и `TIMEOUT`/`statement_timeout_ms`. При превышении — `CANCEL` транзакции с откатом (§10.10, §17.5 gql_spec). Профилировочные хуки: rows processed, time per operator, cache misses → адаптивный оптимизатор и уточнение cost-model.

---

## 10. Взаимодействие исполнителя с хранилищем

Этот раздел связывает исполнителя (§7–§9) с дисковым форматом (`disk_storage_spec.md`). Три режима хранения (`disk_storage_spec.md` §2).

### 10.1. Режимы хранения

- **Режим 1 — Full in-memory** (`MetaGraph`, `fg-meta`): весь граф в RAM (SoA), MVCC в памяти, WAL + snapshot для durability; recovery = load snapshot + replay WAL. Максимальная скорость traversal.
- **Режим 2 — Disk-backed** (`DiskMetaGraph`, `disk_storage_spec.md`): страницы графа в наборе `.fgb`-файлов, горячие — в Clock-Pro buffer pool; MVCC на уровне записей; Direct I/O. Для графов, не помещающихся в RAM.
- **Режим 3 — Hybrid** (`MemoryGraphHandle` через `query_to_memory`): подграф из disk-backed загружается в in-memory `MetaGraph`; работа через MVCC in-memory; flush по политике.

### 10.2. Абстракция страничного доступа

Исполнитель никогда не читает файлы напрямую — только через `PageManager` (`disk_storage_spec.md` §29.2) поверх `BufferPool`:
```rust
trait PageManager: Send + Sync {
    fn alloc_page(&self, file: DataFileKind) -> io::Result<u64>;
    fn free_page(&self, file: DataFileKind, page_no: u64) -> io::Result<()>;
    fn read_page(&self, file: DataFileKind, page_no: u64, buf: &mut [u8; PAGE_SIZE]) -> io::Result<()>;
    fn write_page_dirty(&self, file: DataFileKind, page_no: u64, buf: &[u8; PAGE_SIZE], lsn: Lsn) -> io::Result<()>;
    fn flush_file(&self, file: DataFileKind) -> io::Result<()>;
    fn flush_all(&self) -> io::Result<()>;
    fn page_count(&self, file: DataFileKind) -> io::Result<u64>;
}
```
`PAGE_SIZE = 16384`; `PageHeader` (32 байта) хранит `page_lsn` и `checksum` (xxHash3-64). При чтении buffer pool проверяет `page_no` и checksum → при рассинхроне страница помечается как требующая REDO из WAL (torn-write detection, §12). I/O — `SyncIo`/`AsyncIo` с выровненными буферами (O_DIRECT), пакетные чтения через io_uring (§18 задела).

### 10.3. Разрешение идентификаторов: `RecordId → DiskAtomRef → AtomId`

Три уровня идентификаторов (`disk_storage_spec.md` §4, §9):
- **`RecordId`** (`table:id`) — публичный, единственный, видимый в GrynQL; хранится как свойство `id`.
- **`DiskAtomRef`** (`u32`: бит31 = kind, биты30..0 = slot) — стабильный дисковый id (без generation).
- **`AtomId`** (`u64`: slot + generation) — внутренний in-memory handle; **никогда не экспонируется** в GrynQL (§3.7 gql_spec).

Разрешение при выполнении:
```
RecordId --(primary B+tree pk_<table_id>.fgidx)--> DiskAtomRef --(read hot slot → gen)--> AtomId
```
`from_disk_ref(r, gen)` восстанавливает `AtomId` из `DiskAtomRef` + generation, прочитанной из hot-slot. Материализация результата (§4.2) выполняет обратный маппинг `AtomId → DiskAtomRef → RecordId` и разыменование `id`-свойства.

Кросс-уровневые endpoint'ы адресуются `QualifiedAtomRef { sg_slot, local_slot }` (§3.9 disk-spec): когда `EdgeHotSlot.flags.CROSS_LEVEL = 1`, inline-endpoint'ы хранят `QualifiedAtomRef`, иначе plain `DiskAtomRef`.

### 10.4. Как операторы читают данные

**Атомы (hot data).** `NodeHotSlot` (256 байт, 63/страница) и `EdgeHotSlot` (128 байт, 127/страница) хранят «горячее»: kind-флаги, `label_id`, `table_id`, счётчики степени, inline-adjacency, ссылку на свойства (`props_page`/`props_slot`/`props_len`). Адресация — O(1) без B+tree: `page_no = slot / slots_per_page`, `offset = 32 + (slot % slots_per_page) × slot_size`.

`table_id` в hot-slot используется планировщиком для быстрой пофильтровой выборки по таблице без обращения к props.

**Adjacency traversal.** Операторы `Expand`/`PathExpand` читают смежность так (`disk_storage_spec.md` §25.2, `out_neighbors`):
```
1. Прочитать NodeHotSlot(node_slot).
2. Взять adj_out[..min(adj_out_count, 16)] до первого NULL_SLOT — inline edge DiskAtomRef'ы.
3. Если adj_out_count > 16 — идти по цепочке overflow_out:
     каждая AdjacencyOverflowPage: entries[..count] (u32), затем next_page, пока != NULL_PAGE.
```
Это зеркалит in-memory модель `fg-meta` (`SmallVec<[u32;16]>` inline + overflow) — единая семантика в обоих режимах. Направления: `adj_out`/`adj_in` (16 inline), `adj_undir` (8 inline).

**Свойства (cold data).** `PropHeapPage` — слоттированная страница; `PropertyMap` кодируется как `num_props(uLEB128) + [key_id(u32) + Value]` (`disk_storage_spec.md` §13.2). Ключи интернированы (`key_id` из `LabelDictionary`). Крупные объекты (> ½ payload) — цепочка `PropLargeChunkPage`. Late materialization (§7.6): свойства читаются только для строк, прошедших топологический/label-фильтр.

**Рёбра-endpoint'ы и MetaEdge.** `EdgeHotSlot` держит inline `inv_inline`/`out_inline` (по 4), overflow-цепочки, `weight` (NaN = нет веса), `transition_gid` (для MetaEdge с transition-подграфом), `inc_page` (список `EdgeIncidence`, ≠ NULL ⇒ MetaEdge). Participation-инцидентность (`EdgeIncidencePage`, роли Source/Target/Control) читается отдельно от endpoint-инцидентности — два независимых механизма (§3.8 gql_spec).

**Метки и схема.** `string ↔ id` разрешается через `LabelDictionary` (`HashIndexPage`, string→id на этапе разбора/плана/вставки; `DictEntryPage`, id→string при материализации), кешируется LRU (`FG_LABEL_DICT_CACHE_MB`). Определения DDL (Table/Field/Index/Event/Function/Analyzer/LiveSelect) — в `SchemaCatalog`; входная точка на именованный граф — `GraphSuperblock` (даёт `node_count`/`edge_count` для cost-model и root-страницы схемы/статистики).

**Мета-атомы.** `metavertex`/`metaedge` — не отдельные типы хранения, а обычные vertex/edge с флагом `is_meta` / ненулевым `inc_page` / `subgraph_slot`. Вложенные подграфы — через `SubgraphDirPage`; доступ из GrynQL — функциями `graph::inner/nodes/edges` (§15.10 gql_spec).

### 10.5. Доступ к статистике

Планировщик (§6.3) читает `StatisticsStore` (`GraphStatsPage`, `LabelStatsPage`, `PropertyHistogramPage`, `DegreeHistogramPage`) через `PageManager`. Статистика — это то, без чего графовый планировщик не работает (§6.4), поэтому её сбор обязателен для MVP.

### 10.6. Режим 3 — query_to_memory / MemoryGraphHandle

Высокоуровневый API для гибридного режима:
```rust
fn query_to_memory_sample() {
  let handle: MemoryGraphHandle = db.query_to_memory(
    "MATCH (p:Person)-[:FRIEND*1..3]->(q:Person) WHERE p.name = $name",
    Some(vars!{ "name" => "Alice" }),
    MemoryGraphSettings {
      max_nodes: Some(100_000), max_edges: Some(500_000),
      max_memory_bytes: Some(512 * 1024 * 1024),
      isolation: IsolationLevel::SnapshotIsolation,
      flush: FlushPolicy::Auto { dirty_threshold: 1000, interval: Some(Duration::from_secs(30)), on_drop: true },
      overflow: OverflowPolicy::Error,        // | Evict | SpillToDisk
      lifetime: GraphLifetime::Transaction,   // | Session | Named("working_graph")
    },
  ).await?;
}
```
Внутренняя структура:
```
MemoryGraphHandle
├── inner: MetaGraph              -- in-memory граф (graph.rs)
├── mvcc: MvccStore               -- MVCC + SSI (mvcc.rs)
├── source: DiskMetaGraph         -- disk-источник (§10.1 режим 2)
├── dirty_set: FxHashSet<AtomId>  -- изменённые атомы
├── snapshot_ts: TxId             -- снимок на момент загрузки
└── settings: MemoryGraphSettings
```
- **Загрузка** (`load_subgraph`, disk-spec §25): выполнить GrynQL-запрос на disk-backend → набор `DiskAtomRef` → Pass 1 (узлы: пропустить tombstone, резолв label через `LabelDictionary`, чтение props), Pass 2 (рёбра между загруженными узлами) → построить `slot_to_atom: Map<u32, AtomId>`; зафиксировать `snapshot_ts`.
- **Flush**: сериализовать `dirty_set` → WAL-записи → применить через `GraphHandle::commit()` → опционально checkpoint.
- **Параллельный доступ**: через `MvccStore` (несколько читателей lock-free, писатели через SSI-конфликты).

---

## 11. Транзакции, MVCC и снимки в исполнении

Реализация — `fg-meta` (`mvcc.rs`, `mvcc_persist.rs`); поведение — §17 gql_spec, детали — `implementation_notes.md`. Модель — **логический MVCC** (версии как цепочки `VersionChain<T>` в памяти; WAL пишет операции уровня транзакции, не страницы) — **не LSM**.

### 11.1. Уровни изоляции

Snapshot Isolation по умолчанию, SSI опционально, Read Committed поддержан (§17.1). Задаётся при `BEGIN ISOLATION LEVEL …`. Вне явной транзакции каждый оператор — автотранзакция.

### 11.2. Снимок на весь запрос

**Инвариант** (§23.5 gql_spec, §55.2): `∀ op: op.snapshot = Γ.Snapshot`. Все чтения (`MATCH`, `SELECT`, traversal) одного запроса видят один снимок; если разные операторы прочитают разные снимки — возможны phantom reads. Snapshot фиксируется как `SnapshotId = (timestamp, tx_id)`.

Timestamp oracle: глобальный `atomic<u64> next_ts`; при старте tx `start_ts = next_ts.load()`, при commit `commit_ts = next_ts.fetch_add(1)`. Видимость версии: `visible(v, snapshot_ts) = v.begin_ts ≤ snapshot_ts < v.end_ts`.

### 11.3. GraphView

В рамках одной транзакции граф виден консистентно (§17.3): мутации видны внутри транзакции немедленно, вне — после `COMMIT`. Traversal **не видит** собственные мутации в рамках одного шага, если не включён `FG_TRAVERSAL_READ_YOUR_WRITES` (default false).

### 11.4. Обнаружение конфликтов

O(1)-индексы (`implementation_notes.md` §3): `atom_readers[atom] → [TxId]`, `atom_writers[atom] → [TxId]`, `atom_meta[atom] → AtomMeta` (label/property-кеш для phantom-matching).

| Конфликт         | Поведение                                                                       |
|------------------|---------------------------------------------------------------------------------|
| Write-write      | FirstCommitterWins → abort второй                                               |
| Read-write (SI)  | не ловится (возможен write skew)                                                |
| Read-write (SSI) | abort при rw-цикле                                                              |
| Phantom (SSI)    | abort через predicate locking + `AtomMeta` (точный matching, 0 false positives) |

Атомарное `write_with_meta` (§3.3 impl-notes): meta обновляется **до** записи данных → concurrent SSI phantom-check видит актуальные метаданные. При abort — `remove_atom_meta` для `tx.pending_meta`.

### 11.5. Протокол commit

1. validate (SI write-write / SSI rw-цикл + phantom);
2. assign `commit_ts`;
3. install versions (private write-set → visible);
4. WAL append `COMMIT` → `wal.sync()` (fdatasync);
5. publish (изменения видны после `commit_ts`).

Durability: ответ клиенту — только после fsync WAL (§17.6, §12.5).

### 11.6. Graph-специфичная консистентность

Консистентность ребра (src/dst существуют) — валидация **на commit** (прагматичный вариант A: оба endpoint'а видимы), фоновая очистка dangling-рёбер. Удаление вершины: tombstone → lazy-delete рёбер → background GC (§29 задела). MVCC GC: `version.end_ts < min(active_tx.start_ts) → удалить`.

---

## 12. WAL, checkpoint, recovery со стороны исполнителя

Согласовано с `implementation_notes.md` и `disk_storage_spec.md` §22–§24. **Single-pass REDO, без ARIES-фаз Undo/CLR; без LSM.**

### 12.1. WAL

Логический WAL уровня транзакции. Формат записи (`disk_storage_spec.md` §22.4, как в `mvcc_persist.rs`):
```
[ MVCE:4 ][ lsn:8 ][ tx_id:8 ][ kind:1 ][ payload_len:4 ][ crc32:4 ]  = 29 байт заголовка + payload
```
`crc32` покрывает `lsn‖tx_id‖kind‖payload_len‖payload`. LSN — единый монотонный `u64`. Сегменты — `wal/<lsn_hex>.wal`, ротация при `FG_WAL_SEGMENT_SIZE` (64 MiB) и всегда при checkpoint; `MultiSegmentReader` читает прозрачно через несколько файлов.

`WalKind`: базовые `0x01–0x07` (Begin/Write/Delete/Commit/Abort/Checkpoint/**Meta**) — для Режима 1; для Режима 2 добавлены страничные `0x10–0x22` (`NodeAlloc`, `EdgeAlloc`, `NodeHotUpdate`, `AdjAppend`, `PropSet`, `IndexInsert`, `SchemaDef`, `PageAlloc`, `StatsUpdate`, `ChangefeedPut`, …). `WalKind::Meta` хранит `AtomMeta` → точный phantom-matching после рестарта (§3.6 impl-notes).

**WAL-first инвариант** (§22.3 disk-spec): (1) WAL append → (2) грязная страница в пуле, `page_lsn = entry_lsn` → (3) на COMMIT fsync WAL → (4) страницы сбрасываются только при eviction/checkpoint и только если `page_lsn ≤ persisted_wal_lsn`. **Страница никогда не пишется раньше своей WAL-записи.**

> **Контракт NO-STEAL (обязателен для корректности).** Так как recovery в дисковом режиме — чистый REDO без UNDO-фазы (§12.3), buffer pool **не имеет права вытеснять на диск страницу с незакоммиченными данными** (политика NO-STEAL). Иначе оборванная/откатанная транзакция оставит изменения на диске без отката. Eviction ограничивается committed-данными; грязные страницы незавершённых tx остаются в пуле до commit/abort. (Замечание к `disk_storage_spec.md` §22.3/§23.2: политику STEAL нужно явно запретить либо добавить UNDO.)

### 12.2. Checkpoint

Fuzzy (не останавливает систему): flush грязных страниц по файлам → fsync каждого `.fgb` (порядок props→hot→adj→labels→schema→stats→idx) → WAL `CheckpointEnd{snapshot_lsn}` → fsync WAL → атомарное обновление `GraphSuperblock.checkpoint_lsn` (tmp→rename) → ротация WAL → `prune_before(checkpoint_lsn)`. В Режиме 1 — streaming snapshot O(1) RAM + inline compaction (carry-forward только active-tx, `implementation_notes.md` §5.3). Триггеры: `FG_CHECKPOINT_DIRTY_THRESHOLD` (0.25), `FG_CHECKPOINT_INTERVAL_SEC` (300).

### 12.3. Recovery

**Single-pass REDO** (`implementation_notes.md` §5.5, `disk_storage_spec.md` §23.2):
```
1. Открыть GraphSuperblock → checkpoint_lsn (fallback .tmp, иначе FATAL).
2. Читать WAL после checkpoint_lsn (MultiSegmentReader), один проход:
     per_tx_ops: Map<TxId, Vec<Op>>   -- deferred apply
     WRITE/DELETE/PROP/… → накопить в per_tx_ops[tx]  (last-write-wins по atom/page)
     META               → per_tx_meta[tx]
     COMMIT             → применить per_tx_ops[tx] к состоянию (REDO здесь)
     ABORT              → отбросить per_tx_ops[tx]
     BEGIN              → active.insert(tx)
3. После прохода: незакоммиченные tx отброшены (UNDO не нужен при NO-STEAL, §12.1).
4. Flush грязных + fsync всех .fgb → свежий checkpoint.
```
Идемпотентность REDO — через `page_lsn` (в Режиме 2: применять запись, только если `entry_lsn > page.page_lsn`; при torn-write — checksum/`page_no` mismatch → применять безусловно). Recovery = `Checkpoint + WAL + IdempotentReplay`; сложность `O(|WAL с последнего checkpoint|)`.

Крэш-сценарии: до commit → tx отброшена; после commit до flush → REDO из WAL; частичная запись WAL → checksum fail → обрезать хвост.

> **ARIES / physical-WAL** актуальны только для будущего disk-backed режима с page-level physical logging; для MVP single-pass REDO достаточен (§31.2 gql_spec, §18).

---

## 13. Индексы в исполнении

Единый интерфейс `Index` (`disk_storage_spec.md` §20.1): `insert`, `delete`, `lookup(key) -> [DiskAtomRef]`, `range(lo, hi, incl) -> Iterator<DiskAtomRef>`, `rebuild(source)`, `status()`.

### 13.1. Типы индексов

| Тип (GrynQL §9.6)  | On-disk (disk-spec §20)       | Применение в исполнении                        |
|--------------------|-------------------------------|------------------------------------------------|
| primary (implicit) | B+tree `pk_<table_id>.fgidx`  | `RecordId → DiskAtomRef` (§10.3)               |
| label (implicit)   | `lbl_node/edge_<id>.fgidx`    | `Scan` по метке, `tv::V([labels])`             |
| standard / unique  | B+tree (order-preserving key) | `IndexScan`, точечный/range-lookup, uniqueness |
| count              | одностраничный счётчик        | `count() … GROUP ALL` без скана                |
| fulltext (BM25)    | inverted index                | `@@`/`@AND@`/`@OR@`, `search::score/highlight` |
| HNSW (vector)      | in-memory + blob на диске     | KNN `<\|k,metric\|>`, ANN                      |
| geometry (R-tree)  | R-tree                        | `INSIDE`/`INTERSECTS`, `geo::*`                |
| reachability       | tree_cover/GRAIL/FERRARI/BFL  | «достижима ли B из A?»                         |
| neighbourhood      | EXACT/SKETCH/LANDMARK         | `[:E*1..k]`, `k_hop_neighbourhood`             |
| path               | LANDMARK_SSSP/PATTERN_CACHE   | shortest-path, var-length                      |
| edge-endpoint      | B+tree `ee_endpoint.fgidx`    | AtomWalk V-E/E-V/E-E (§10.4)                   |

### 13.2. Использование планировщиком

Планировщик заменяет `TableScan` на `IndexScan` при выигрыше по кардинальности; при нескольких индексах выбирает наименьшую кардинальность (§10.3 gql_spec). `WITH INDEX @i` — принудительно; `WITH NOINDEX` — table scan. Adjacency-обход (`out/in/both`) идёт через inline+overflow (§10.4), а не через отдельный индекс — smežность встроена в hot-slot.

### 13.3. Консистентность и обслуживание

- Primary/secondary/count/fulltext — транзакционно-консистентны (обновляются в той же транзакции: WAL `IndexInsert/Delete` + страница).
- HNSW/fulltext — опционально eventually-consistent (§19.1 gql_spec).
- **`DEFER`** — обновление откладывается в фоновую очередь (`IndexPendingQueue`), eventual consistency; несовместимо с `UNIQUE`.
- **`CONCURRENTLY`** — двухфазная постройка без блокировки таблицы: initial (индексация существующих; новые изменения копятся как `pending`) → update (обработка pending); статус (`building_initial`/`building_update`/`ready`/`error`) виден через `INFO FOR INDEX` (§9.6 gql_spec).
- **`REBUILD INDEX`** — перестройка (актуально для HNSW после многих обновлений).

Индекс тоже MVCC-aware: `(index entry, ts)`; видимость записи индекса согласована со снимком (§32.3 gql_spec).

---

## 14. Управление ресурсами и деградация

Бюджеты — §1.1. Настройки (`gql_spec.md` §29 + `disk_storage_spec.md` §27):

| Настройка                                         | Назначение                           | Default       |
|---------------------------------------------------|--------------------------------------|---------------|
| `FG_PAGE_BUFFER_PAGES`                            | размер buffer pool (страниц)         | 4096 (64 MiB) |
| `FG_WAL_SEGMENT_SIZE`                             | размер WAL-сегмента                  | 64 MiB        |
| `FG_CHECKPOINT_DIRTY_THRESHOLD` / `_INTERVAL_SEC` | триггеры checkpoint                  | 0.25 / 300    |
| `FG_LABEL_DICT_CACHE_MB`                          | LRU-кеш словаря меток                | 32            |
| `FG_PROPS_COMPRESS_THRESHOLD`                     | порог LZ4-сжатия props               | 512           |
| `FG_HNSW_CACHE_SIZE`                              | кеш HNSW                             | 256 MiB       |
| `FG_STATS_AUTO_ANALYZE_DIRTY`                     | авто-`ANALYZE`                       | 0.10          |
| `FG_PATH_MAX_DEPTH`                               | макс. глубина path-finding           | 30            |
| `FG_GRAPH_EXPANSION_MAX_NODES/EDGES`              | лимиты graph-expansion               | 1e6 / 5e6     |
| `FG_TRAVERSAL_READ_YOUR_WRITES`                   | видимость своих мутаций traversal'ом | false         |
| `FG_TEMPFILES_PATH`                               | каталог для `SELECT … TEMPFILES`     | ""            |
| `FG_ASYNC_EVENT_PROCESSING_INTERVAL`              | интервал async-событий               | 5000 ms       |

Path-finding vs graph-expansion (§16.4 gql_spec): `FG_PATH_MAX_DEPTH` применяется **только** к path-finding (`[:E*]` без явного `max`); `RETURN GRAPH`/`BUILD GRAPH` используют graph-expansion mode с лимитами по узлам/рёбрам, не по глубине. Деградация — fail-closed: при исчерпании бюджета операция отвергается с диагностикой, а не «зависает».

---

## 15. Детерминизм и воспроизводимость

### 15.1. Режимы исполнения

`DETERMINISTIC` (строгая воспроизводимость), `BEST_EFFORT` (детерминизм без гарантии порядка; default), `FAST` (максимум производительности, без гарантий). Формула: `Determinism = Snapshot + Seed + StableExecution + CanonicalPlan`.

### 15.2. Источники недетерминизма

Физика (порядок чтения страниц, потоки, нестабильная итерация hash); логика (нет `ORDER BY`, `RAND()`, `time::now()`, `uuid()`, FP-округление, порядок traversal); планировщик (cost-based выбор, статистика, adaptive execution).

### 15.3. Контракты DETERMINISTIC

- Фиксированный снимок + фиксированный seed + стабильный порядок + канонический план.
- Без явного `ORDER BY` — авто-`ORDER BY stable_id(record)`, `stable_id = (table_id, primary_key)`.
- Deterministic traversal: `Expand(v)` возвращает `sorted(adj[v])`; параллельный traversal — stable merge или deterministic scheduler (`task_id = hash(node_id)`).
- `rand()` → `rand(seed, row_id)`; `time::now()` → `snapshot.timestamp`; `uuid()` → `uuid(seed, row_id)`; `seed = f(query_hash, snapshot, user_seed)`.
- Deterministic aggregation: `sum = fold(sorted(values))`; FP — фиксированное дерево редукции (pairwise) или `decimal` для критичного.
- Side-effects — детерминированный порядок либо запрет в DETERMINISTIC.

### 15.4. Протокол воспроизводимости

Минимальный набор для реплея: `Query + SnapshotID + Seed + PlanHash + EngineVersion`. `QueryContext { snapshot: SnapshotId, seed: u64, deterministic: bool, plan_hash: u64 }` — передаётся в каждый оператор.

### 15.5. Канонический план

Cost-based выбор при равенстве стоимостей разрешается tie-breaker'ом `plan_hash` → `Plan = argmin(cost, plan_hash)`; `PlanHash` сохраняется для воспроизводимости.

---

## 16. Доказательства корректности ключевых правил

Все переписывания сохраняют `∀Γ: [[Q₁]](Γ) = [[Q₂]](Γ)` (§23.4 gql_spec).

- **Predicate Pushdown** (§54.1): `Filter(p, Scan(T)) = {x ∈ T | p(x)} = Scan(T, p)`. ∎
- **Join Reordering** (§54.2): `(A ⋈ B) ⋈ C ≡ A ⋈ (B ⋈ C)` при отсутствии side-effects и детерминированных предикатах. ∎
- **Projection Pushdown** (§54.3): `π_f(σ_p(R)) ≡ σ_p(π_{f'}(R))`, `f' ⊇ attrs(p)` — если не удаляются поля, используемые предикатом. ∎
- **Traversal Step Fusion** (§54.4): `out ∘ out = expand(depth=2)` при отсутствии side-effects и path-constraints. ∎
- **MATCH → JOIN** (§54.5): `MATCH (a)-[:E]->(b) = {(a,b) | b ∈ Expand(a)}`. ∎
- **Snapshot consistency** (§55): `∀ op: op.snapshot = S` — иначе phantom reads.
- **Side-effects** (§56): `side_effect ∘ filter ≠ filter ∘ side_effect` — эффектные операторы фиксируют order-barrier и запрещают reordering.
- **Cost-based корректность** (§57): если все правила rewrite корректны и все физические операторы корректны, то `Planner(Q) ≡ Q`.

Инварианты планировщика (§58): результат не меняется; кардинальность сохраняется (кроме `DISTINCT`); порядок сохраняется при `ORDER BY`; snapshot-инвариант; детерминизм при отсутствии randomness; path-инварианты (длина, порядок вершин, constraints).

---

## 17. Соответствие кодовой базе

| Компонент tech-spec                    | Модуль                                                                                                        | Статус                        |
|----------------------------------------|---------------------------------------------------------------------------------------------------------------|-------------------------------|
| Лексер / парсер                        | `src/syn` (`lexer`/`parser`/`token`/`error`, reblessive)                                                      | Каркас реализован             |
| AST / Expr                             | `src/gql/ast.rs`, `src/gql/expression.rs`                                                                     | Каркас                        |
| Datastore / execute                    | `src/gql/ds.rs` (`Datastore::execute → Vec<QueryResult>`)                                                     | Каркас (тело TODO)            |
| Traversal VM                           | `src/gql/traversal` (`TraversalMachine`, `Traverser`, `TraversalContext`)                                     | Каркас                        |
| Диагностика                            | `src/gql_status` (`Status → DiagnosticRecord → ErrorState → FerrosGrynnError`)                                | Реализовано                   |
| Значения                               | `src/values` (`runtime/storable`, `runtime/virt`, `runtime/graph`, `storage`)                                 | Активный модуль               |
| Граф-ядро / traversal-алгоритмы        | `fg-meta` `graph.rs`, `traversal.rs` (BFS/DFS/Dijkstra/Bellman-Ford/SCC/LCA)                                  | Референс-реализация           |
| MVCC (SI/SSI)                          | `fg-meta` `mvcc.rs` (SSI + O(1)-индексы + predicate locking)                                                  | Реализовано                   |
| Персистентный MVCC / WAL               | `fg-meta` `mvcc_persist.rs` (single-pass REDO, segment rotation)                                              | Реализовано                   |
| Snapshot / store                       | `fg-meta` `store.rs`                                                                                          | Реализовано                   |
| Direct/Async I/O, BufferPool           | `fg-meta` `io_sync`/`io_async`/`async_io`; `crates/storage_engine/io`                                         | Реализовано / каркас          |
| PageManager / DiskMetaGraph / страницы | `crates/storage_engine` (`store`, `layout`) — `NodeRecord`/`EdgeRecord`/`PropertyRecord`, `FerrosGrynnLayout` | Каркас (по disk-spec §29–§30) |

Что новое / требует реализации (сводно из `disk_storage_spec.md` §30): полная замена `Value` (types/), `RecordId`/`DiskAtomRef`/`QualifiedAtomRef`, `PageManager` + disk-страницы (storage/), `SchemaCatalog` (schema/), `StatisticsStore` (stats/), индексы (index/), changefeed/live (changefeed/); в query-слое — binder/typechecker, lowering AST→IR, RuleEngine/CostEngine, физические операторы, соединение Traversal VM с реляционным движком.

---

## 18. Отложено / вне MVP

| Область                                                        | Решение                                                                              |
|----------------------------------------------------------------|--------------------------------------------------------------------------------------|
| JIT-компиляция (LLVM: expression/operator-fusion/pipeline JIT) | Не входит в контракт языка; implementation detail; после стабилизации интерпретатора |
| Worst-Case Optimal Join (Leapfrog TrieJoin)                    | Базовый Index NL достаточен для MVP; WCOJ — для паттернов с циклами позже            |
| Disk-backed graph-native pages (полная реализация Режима 2)    | Каркас есть; полная — большая работа, не нужна для in-memory MVP                     |
| Physical-WAL + ARIES (Undo/CLR)                                | Только для disk-backed physical logging; single-pass REDO достаточен                 |
| Параллельный recovery                                          | Отложено; single-pass достаточен                                                     |
| SPDK / NVMe kernel-bypass                                      | Задел (`spdk.rs`), Linux-only, не развивается                                        |
| SMT-верификация оптимизатора                                   | Long-term; property-based тесты как практический инструмент                          |
| Тензорные кеши (dense/CSR на диске)                            | `TensorCachePage` — для ML/аналитики позже                                           |

---

*Версия 0.3-draft. Синхронизируется с `gql_spec.md`, `gql_grammar.md`, `disk_storage_spec.md`. При расхождении приоритет у этих трёх спецификаций.*
