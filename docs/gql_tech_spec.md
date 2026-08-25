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

> **Важно об архитектуре хранения.** Ранние черновики этого файла описывали LSM-дерево (L0/L1/compaction). Это устранено: `disk_storage_spec.md` §20.2.8 отклоняет LSM в пользу B+tree. Модель дискового режима — **frame-WAL (COW в логе) + бэкфилл на штатные места + MVCC через индекс версий страниц**, журнал единый на базу данных (§12). См. [implementation_plan.md](implementation_plan.md) §1.2.

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
| TxWriteSet        | `FG_TX_WRITE_SET_MAX_BYTES` = 256 МиБ              | Abort до публикации; import дробится  |
| Statement timeout | `statement_timeout_ms` / `TIMEOUT` clause          | Cancel query                          |
| Snapshot age      | `max_snapshot_age`                                 | Force-close stale snapshots           |
| Path expansion    | `FG_PATH_MAX_DEPTH` (30), `MAXDEPTH`               | Обрыв path-finding                    |
| Graph expansion   | `FG_GRAPH_EXPANSION_MAX_NODES/EDGES`               | Обрыв построения графа                |

Fail-closed неприятнее в моменте, но честнее в эксплуатации: система сразу предлагает retry, circuit breaker, fallback или отказ.

### 1.2. Contract-first подход через код

Архитектурные контракты выражаются через код: traits, инварианты типов, паттерны. Ключевые интерфейсы-границы:

- **`SyncPageManager`** ([implementation_plan.md](implementation_plan.md) §3.3) — единый
  crate-private Windows-first manager под frame-WAL: `TxHandle` владеет snapshot-регистрацией,
  `PageRead<'tx>` объединяет staged borrow и immutable `PageGuard`, а passive checkpoint/recovery
  остаются внутренними. Trait с единственной реализацией не вводится.
- **`Index`** (`disk_storage_spec.md` §20.1) — единый интерфейс индексов: `insert`, `delete`, `lookup`, `range`, `rebuild`, `status`.
- **`TransactionLogSink`** — приёмник WAL-записей (append + fsync-барьеры).
- **`RowCursor`** (§7, §9) — эталонный sync pull-контракт:
  `next() -> FerrosGrynnResult<Option<Row>>`. Batch-оператор добавляется поверх него после
  измерений и не меняет семантику плана.
- **`src/storage`** — production Sync Direct I/O для `.fgb`/`.fgidx`; `fg-meta` служит только
  референсом. Опциональный Native Async I/O вводится после sync-gate и реализует тот же внутренний
  page-source контракт.

Интерфейсы фиксируют, что система обязана уметь, где проходят её границы, и какие инварианты держатся.

### 1.3. Изоляция бизнес-логики от data engine

Чем больше пользовательской логики живёт в хранилище, тем сложнее её тестировать, версионировать и наблюдать. Поэтому пользовательские функции (`DEFINE FUNCTION`), события (`DEFINE EVENT`), замыкания и cost-функции исполняются в **песочнице** с явными ограничениями: `MAXDEPTH` для рекурсии, запрет бесконечных циклов (`FOR`/`tv::repeat` требуют `times(n)`/`until`), классификация purity (§4.6).

### 1.4. Sync-first и опциональный Native Async I/O

Sync Direct I/O — обязательная базовая реализация и default. До прохождения correctness-gate и
сравнения Direct+BufferPool с benchmark-only mmap async-код не добавляется. После gate пользователь
может выбрать `Sync` или доступный `NativeAsync` при открытии хранилища; недоступный режим
возвращает типизированную ошибку без fallback. Публичный query API остаётся синхронной pull-моделью.
WAL в обоих режимах использует последовательный buffered I/O и один `flush + sync_data` на commit.

---

## 2. Конвейер обработки запроса

Публичный parser-вход — `gql::parse(text, ParserSettings) -> FerrosGrynnResult<Program>`.
После подключения datastore точкой исполнения становится
`Datastore::execute(text, variables) -> Vec<QueryResult>`. Каждый оператор верхнего уровня
возвращает `QueryResult { kind, elapsed, result: FerrosGrynnResult<Outcome<Value>> }`.

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
QueryResult (Value)
```

Каждая стадия — отдельный компонент с явным контрактом входа/выхода. Между стадиями данные не «протекают» синтаксисом: логический IR не хранит синтаксис (§26 gql_spec, «IR НЕ ДОЛЖЕН хранить синтаксис»).

Разбиение (Split) на **декларативную** ветвь (`SELECT`/`MATCH`) и **императивную** ветвь (Traversal pipeline) происходит на уровне AST → IR: декларативная часть идёт в join/relational engine, императивная — в Traversal VM (§8). Обе ветви сходятся в **едином** логическом IR (риск смешанной семантики фиксируется границей IR, §32.2 gql_spec).

---

## 3. Фронтенд: лексер и парсер

Реализация — `src/syn` (`lexer.rs`, `ast.rs`, `parser.rs`). AST и parser не зависят от storage и
runtime `Value`; публичный facade находится в `src/gql/mod.rs`.

### 3.1. Лексер

- Первый срез лексирует Unicode/quoted identifiers, параметры, числа, строки, комментарии,
  delimiters и жадные семейства операторов. Темпоральные/regex/прочие compound literals
  добавляются вместе с соответствующей parser + analysis + execution веткой.
- **Span** — `gql_status::TextSpan { offset: u32, len: u32 }` в UTF-8 байтах. Запрос длиннее
  `u32::MAX` отклоняется до лексинга статусом `QueryTextTooLarge`.
- Пробелы/комментарии значимы **только** между соседними токенами там, где грамматика это
  требует (`record_id` = `table:id` без gap). Токен хранит `gap_before`; исходный текст в AST
  не удерживается.

### 3.2. Парсер

`Parser<'a>` (`src/syn/parser`) — рекурсивный нисходящий парсер поверх **`reblessive`** (stackless async-рекурсия). Это принципиально: GrynQL допускает глубоко вложенные подзапросы, блоки, объекты и pipeline; stackless-стек (`Stk`) избегает переполнения нативного стека на патологически вложенных запросах.

Механика:
- **`TokenBuffer<4>`** — look-ahead до четырёх токенов через `peek(n)` без отката.
- Первый срез использует fail-fast recursive descent без recovery/speculation; неоднозначности
  разрешаются позицией, look-ahead и gap-флагом.
- Операторы выражений разбираются по точной таблице приоритетов `gql_grammar.md` §12.1 методом
  Pratt; результат — owned `Expr` из `src/syn/ast.rs`.
- AST различает pattern и reference: endpoint паттерна хранит `kind`, optional alias,
  `label_expr` и фильтр, а endpoint-ссылка — `kind` и выражение `record_id | idiom | param`.
  Совпадающий текст `a:b` не объединяется в общий узел с контекстным флагом.

### 3.3. Разрешение синтаксических неоднозначностей

Парсер обязан однозначно разрешать перегруженные символы из `gql_grammar.md` §16:

| Символ            | Роли                                                                                    | Стратегия                                                                                                                                                            |
|-------------------|-----------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `<`               | меньше / cast `<T> e` / generic / аннотация графа / meta-атом / AtomWalk endpoint       | по позиции и режиму: после `ATOM` формы `<(…)>`/`<[…]>` задают vertex/edge endpoint; вне него сохраняют meta-семантику; иначе cast, generic, аннотация или сравнение |
| `\|`              | OR `\|\|` / альтернатива меток `:A\|B` / замыкание `\|$x\|` / range-генератор `\|t:n\|` | `\|\|` — единый токен OR; остальное — по контексту                                                                                                                   |
| `\|>` / `<\|…\|>` | pipe / KNN                                                                              | `\|>` и `<\|`,`\|>` — единые токены                                                                                                                                  |
| `~`               | AtomWalk (`~[`, `]~>`, `]~`, `<~[`)                                                     | жадная токенизация после `MATCH ATOM`/`TRAVERSE ATOM`/`RELATE ATOM`; каждый шаг содержит непустое тело ребра с alias и/или label                                     |
| `@`               | fulltext (`@@`, `@1@`, `@AND@`, `@OR@`)                                                 | единые токены                                                                                                                                                        |
| стрелки           | `->`, `<-`, `<->` в паттернах и `RELATE`                                                | часть `edge_pattern`                                                                                                                                                 |

### 3.4. Пределы разбора

`ParserSettings`: `max_object_parsing_depth` (default 100) и
`max_query_parsing_depth` (default 20). Defaults берутся из одноимённых полей `Config`.

### 3.5. Ошибки парсинга

Парсер сразу возвращает `FerrosGrynnError`: `QueryParseError` с primary location и
`ExecutionPhase::Parsing`; незакрытый delimiter добавляет related location открытия.
Превышение лимитов и известная неподдержанная семья используют генерируемые
`QueryNestingTooDeep`/`QueryFeatureNotSupported`. Локальной иерархии `SyntaxError` нет.

---

## 4. Семантический анализ, биндинг и типизация

Вход — AST; выход — **Typed AST**. Три фазы (соответствуют §60 задела):

### 4.1. Фаза 1 — Binding (scope, alias)

- Построение областей видимости биндингов: `Env = Map<Alias, Value>` (§14 gql_spec).
- Правила биндингов §14.2: объявление `(a:Person)` → `Env[a] = vertex<Person>`; повторный alias — это **ограничение**, не присваивание; `MATCH` создаёт новый scope, `WITH`/`LET` — сужают/расширяют.
- `MATCH ATOM <(a:Person)>` связывает vertex alias `a` с label-ограничением `Person`, а
  `MATCH ATOM <[e:FRIEND]>` — edge alias `e`. В `RELATE ATOM <(person:alice)>` та же
  пунктуация окружает ссылку `record_id`; новый binding там не создаётся.
- Проверка неопределённых алиасов, shadowing (§14.3): внутренний alias перекрывает внешний; при передаче через `WITH a, b` — проекция биндингов.
- Разрешение `record_id` (`table:id`) в публичную ссылку; проверка существования таблиц/графов через каталог (§10, `SchemaCatalog`, `GraphSuperblock`).

### 4.2. Фаза 2 — Type inference

Типизация `Γ ⊢ expr : T` (§24 gql_spec). Первый read-only срез использует направленный вывод и
локальную унификацию `number`, `option` и union. Общий Hindley–Milner solver вводится вместе с
closures/UDF, когда появится его первый потребитель.

Типовые домены (§59.2 gql_spec):
1. Value: `int | float | decimal | string | bool | bytes | datetime | duration | …`
2. Record: `record<T> | record<T1 | T2>`
3. Graph: `vertex<T> | metavertex<T> | edge<T> | hyperedge<T> | metaedge<T> | atom<T> | path | subgraph | graph | tree | …`
4. Collection: `array<T> | set<T> | bag<T>`
5. Function: `(T1,…,Tn) -> U`

Правила вывода — §24.9 gql_spec (T-Var, T-Field, T-FunctionCall, T-SELECT, T-MATCH, T-Traversal, T-Join). Idiom-навигация типизируется через `field(T, f) = τ`.

Graph-специфичные проверки:
- `MATCH`: согласованность меток, совместимость рёбер и endpoint-типы. Без `ATOM` домен
  endpoint'ов равен $V \cup MV$; с `ATOM` — $V \cup MV \cup E \cup ME$. В обоих случаях
  шаг содержит отдельное связывающее ребро.
- Traversal-шаг: `out('FRIEND') : vertex<record<Person>> -> vertex<record<Person>>`.
- `StandardPath` чередует vertex/edge, `AtomWalk` — atom/edge; функции
  `path::nodes/edges/atoms/length/cost` типизируются по фактическому виду пути (§15.6 gql_spec).
- `RETURN GRAPH <constraint> {…}`: тип результата — `subgraph` или заявленный тип графа; **система типов не делает неявных преобразований** (запрос `rooted_tree` при фактическом `forest` → ошибка; проверка **после** построения результата, §16.1 gql_spec).

`option<T>` существует только в Typed AST: `Some(v)` понижается в `v`, `None` — в
`Value::None`. Отдельного runtime-варианта и `TypeTag` нет. Операторы над `none`/`null` всегда
типизируются в `bool`; IR не содержит `TruthValue::Unknown`.

Контракт graph/query результатов фиксируется уже при типизации:

| Оператор                      | Typed result                 |
|-------------------------------|------------------------------|
| `RETURN atom`                 | stable atom/record ref       |
| `RETURN path`                 | materialized `Path`          |
| `RETURN GRAPH {…}`            | `Subgraph<Frozen>`           |
| `RETURN GRAPH <T> {…}`        | точный `T<Frozen>`           |
| именованный `BUILD GRAPH <T>` | `GraphRef<T>` после commit   |
| анонимный `BUILD GRAPH <T>`   | session-owned `T<Frozen>`    |
| `graph::inner` внутри плана   | borrowed graph/subgraph view |

Backend-dispatch не меняет Typed result и остаётся внутренним.

### 4.3. Фаза 3 — Constraint solving

Уточнение типов из контекста: `a.age > 10` ⟹ `type(a.age) = number`. Унификация типовых переменных; проверка `record<A | B>` (union) — §24.4.

### 4.4. Разрешение перегрузки `number`

Единственная разрешённая перегрузка (§18.7 gql_spec) — числовой тип `number`, разрешаемый в
`int`/`float`/`decimal` **на этапе анализа** (не в рантайме). Автоматически разрешены
`int → float` и `int → decimal`. `float → decimal` и все сужающие преобразования выполняются
только явным fallible cast; смешение `float` и `decimal` без явного целевого типа — ошибка.
Контекстный `decimal → float` разрешён с диагностикой о возможной потере точности.

### 4.5. Classification purity

Каждая функция/выражение получает класс purity (`PURE | STABLE | IMPURE`, §18.5 gql_spec), считываемый из `FunctionDefinition.purity` (`disk_storage_spec.md` §18). Класс влияет на оптимизатор (§6): `PURE` можно инлайнить/кешировать/переставлять; `STABLE` фиксирована в рамках запроса (`time::now()` замораживается на снимок); `IMPURE` — барьер порядка.

### 4.6. Проверка computed-полей

`DEFINE FIELD … COMPUTED expr` строит граф зависимостей computed-значений; при обнаружении цикла — ошибка (§5.10 gql_spec). Тип помечается обёрткой `computed<T>`; `COMPUTED`-поля **не хранятся** на диске и вычисляются исполнителем при `SELECT` из `FieldDefinition.expr_bytes` (IR) (`disk_storage_spec.md` §18).

### 4.7. Soundness

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

Минимальный набор (§22.2, §26 gql_spec): `Scan(source, filter?)`, `IndexScan(index, key_range, filter?)`, `Filter(pred)`, `Project(exprs)`, `Join(kind, cond)` (`Inner|Left|Semi|Anti`), `Expand(dir, endpoint_domain, edge_labels?, target_labels?, alias_target, alias_edge?)`, `ExpandEdge(dir, labels?, alias_edge)`, `PathExpand(dir, endpoint_domain, edge_labels?, min, max, mode, objective?, filters, alias_path, alias_end)`, `Aggregate(group_by, aggregates)`, `Sort(keys)`, `Limit(n, offset)`, `Dedup(keys)`, `Unwind(expr, alias)` (для `SPLIT`), `TraversalStep(step)`.

`endpoint_domain = VerticesOnly | Atoms`; отдельного оператора для E–E-инцидентности нет.
Hyper- и binary-рёбра раскрываются тем же `Expand`, но hyper-ребро может выдать несколько
попарных веток.

### 5.2. Правила понижения (grammar → IR)

Понижение связывает конструкции грамматики с операторами IR:

| Конструкция GrynQL                                 | Понижение в IR                                                                   |
|----------------------------------------------------|----------------------------------------------------------------------------------|
| `SELECT f FROM t WHERE p`                          | `Scan(t) → Filter(p) → Project(f)`                                               |
| `FROM a, b` (union)                                | `Union(Scan a, Scan b)`                                                          |
| `FROM a & b` (cross)                               | `Join(Inner, true)`                                                              |
| `SPLIT field`                                      | `Unwind(field)`                                                                  |
| `GROUP [ALL\|BY]`                                  | `Aggregate(group_by, aggs)`                                                      |
| `ORDER BY`                                         | `Sort(keys)`                                                                     |
| `LIMIT n START s`                                  | `Limit(n, s)`                                                                    |
| `(a:L)-[:E]->(b:L2)`                               | `Scan(L as a) → Expand(Out, E, b) → Filter(label(b)=L2)` (§26.3, §54.5 gql_spec) |
| `MATCH ATOM <[a:STEP]>~[r:DEPENDS_ON]~><[b:STEP]>` | `Scan(STEP as a) → Expand(Out, Atoms, DEPENDS_ON as r, STEP as b)`               |
| `(a)-[:E*min..max mode]->(b)`                      | `PathExpand(Out, E, min, max, mode, …)`                                          |
| `OPTIONAL MATCH`                                   | `Join(Left, …)` (§23.5 инвариант 6)                                              |
| `MATCH DISTINCT` / `RETURN DISTINCT`               | `Dedup(keys)`                                                                    |
| `RETURN GRAPH {nodes, edges}`                      | `GraphAssemble(constraint?)` (терминальная проекция в `subgraph`)                |
| `BUILD GRAPH … FROM …`                             | `GraphAssemble` + материализация в каталог                                       |
| `src \|> tv::step(...)`                            | `TraversalStep(step)` поверх источника                                           |
| `TRAVERSE FROM g \|> …`                            | цепочка `TraversalStep` над graph-источником                                     |

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

Эталонный контракт физических операторов — синхронный pull:
`RowCursor::next() -> FerrosGrynnResult<Option<Row>>`. Операторы собираются в конвейер (§9). Row:
```
Row = { values: Vec<Value>, env: Env?, path: Path? }
```

### 7.2. Scan-операторы

- **`TableScan { table_id, projection, predicate }`** — скан таблицы; predicate/projection проброшены внутрь (§16.1, §16.3). На диске — обход через `Label index` (`lbl_node_<id>.fgidx`) или primary index (`pk_<table_id>.fgidx`), затем чтение hot-slot (§10.4).
- **`IndexScan { index_id, key_range, predicate }`** — доступ через `Index::range/lookup` (§13).
- **`GraphScan { atom_type, label_filter }`** — старт с множества атомов графа по метке (`tv::V([labels])`).

### 7.3. Expand-операторы

- **`ExpandOp { input, adjacency, direction, endpoint_domain, label_filter }`** — одношаговое расширение по смежности:
  ```
  for each input atom:
      for each edge in adjacency(atom, direction, label):
          yield (atom, edge, endpoint)
  ```
  В `VerticesOnly` вход и выход проверяются как vertex/meta-vertex. В `Atoms` для edge-атома
  обратный список рёбер берётся из `EdgeEndpointIndex`; сами endpoint'ы читаются из
  `source`/`target` связывающего ребра. Оптимизации: adjacency-кеш; inline + overflow;
  degree-aware batching.
- **`ExpandEdgeOp`** — возвращает рёбра (для `tv::outE/inE/bothE`, §13.4 gql_spec).

### 7.4. PathExpand-оператор

```
PathExpandOp { input, adjacency, min_depth, max_depth, mode, objective?, frontier }
```
- **BFS** (frontier-based): очередь состояний, `emit` при `depth ≥ min`, расширение при `depth < max` и `allowed_by_mode` (§50.1). Bitmap-frontier + ранняя дедупликация (§15.4, §50.2).
- **DFS** — для перечисления путей и constrained-search с pruning.
- **Shortest** — BFS (невзвешенный) / Dijkstra (взвешенный), `weight(NaN) = 1` (§15.1 gql_spec).
- Во время поиска path хранится в `PathTraceArena`: узел trace содержит `previous`,
  stable atom ref, optional via-edge, direction и cumulative cost. Общие prefixes
  разделяются, append выполняется за `O(1)`.

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

На каждом шаге trace расширяется новым `PathTraceNode` (§13.9). Planner включает tracking с
начала обхода, если path именован, возвращается, используется `tv::path()` либо path-функцией.
Если path статически не используется, trace не создаётся. Barrier сам по себе не обязан
материализовывать path; это делают `RETURN`, `collect` и операторы, которым нужна полная
структура для сравнения.

### 8.6. Control flow

`repeat(pipeline)` — fixpoint-итерация с условиями остановки `.times(n)` / `.until(closure)` / `.emit()`. Обязателен конечный критерий (§18.6 gql_spec) — защита от бесконечных циклов. `branch`/`choose`/`coalesce` формируют нелинейный execution graph.

### 8.7. Материализация и барьеры

Материализация потока происходит только на: barrier-шагах (`order`, `group`, `dedup`, `fold`); явном `collect()`; границе (`RETURN`) (§13.7). Планировщик учитывает barrier'ы при оценке стоимости (блокируют pipeline-fusion).

Публичный `Path` материализуется в `Arc<[PathStep]>` со stable refs, origin graph identity и
`SnapshotId`. Структурные функции не требуют I/O. Разрешение свойств выполняется только через
`GraphRead`, совпадающий по origin и snapshot; при отсутствии исторической версии возвращается
`SnapshotUnavailable`, а не latest value.

### 8.8. Параллелизм

VM поддерживает (§13.10): **data parallelism** (партиционирование traverser'ов по потокам) и **step parallelism** (ограниченно — только для stateless-шагов и pure-функций). Cost hotspots (§13.11): fan-out на high-degree узлах, `repeat`, path-tracking. Обязательные оптимизации: adjacency index, degree-aware planning, path pruning.

### 8.9. Терминирование

Без явного терминирующего шага traversal возвращает `array<atom>`. Иначе: `values → array<scalar>`, `path → array<path>`, `project → array<object>`, `count → int`, `fold → array` (§13.6).

### 8.10. Взаимодействие с декларативной ветвью

Источник pipeline — любой оператор, возвращающий набор (`SELECT`, `MATCH`, `TRAVERSE`, массив, переменная). Понижение `src |> tv::step` → `TraversalStep` над IR-узлом источника (§5.3). Внутри одного запроса MATCH исполняется join-движком, traversal — VM; данные передаются через материализованный набор атомов на границе.

---

## 9. Модель исполнения

### 9.1. Эталонный sync pull и последующая векторизация

```
Execution = Pattern Engine (MATCH) + Traversal VM + Relational Engine
Baseline: sync pull (RowCursor::next() → Option<Row>)
```
Первый срез реализует `ScanCursor`, `FilterCursor`, `ProjectCursor`, `LimitCursor`. Он остаётся
эталоном корректности. Vectorized даёт SIMD и cache-locality, но вводится только после измерений:
```rust
trait BatchOperator {
    fn next_batch(&mut self) -> FerrosGrynnResult<Option<Batch>>;
}
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

До подключения `PageManager` executor использует crate-private query-facing границу `QueryRead`:
один `snapshot` на весь запрос, `catalog`, `scan` и `get`. `expand` добавляется с первым `MATCH`.
Adapter обязан вернуть уже декодированные runtime `Value`; `StorageValue`, `TypeTag` и внутренний
`AtomId` выше этой границы не проходят. На входе `get` используется публичный `RecordId`.

### 10.1. Режимы хранения

- **Режим 1 — Full in-memory**: `Graph<Mutable>` или специализированный `T<Mutable>` хранит
  topology в RAM, MVCC — в памяти, WAL + snapshot обеспечивают durability. Frozen-представление
  строится явно через `freeze`.
- **Режим 2 — Disk-backed**: каталог возвращает `GraphRef`; `read(&tx)` создаёт
  `DiskGraphView<'tx>` над `PageManager`, `BufferPool` и `SnapshotId`. View не является копией
  графа и не переживает transaction guard. Фундамент режима 2 — текущая работа P2; deferred
  остаются только advanced page/index-функции после минимального vertical slice.
- **Режим 3 — Hybrid**: `query_to_memory::<T, State>` загружает выбранный disk-backed подграф в
  собственную storage family `T<State>`. После полной загрузки исходный disk snapshot
  освобождается.

Все режимы реализуют общий read-only контракт `GraphRead`. Мутация доступна только типам в
состоянии `Mutable`; query results используют `Frozen`.

### 10.2. Абстракция страничного доступа

Исполнитель никогда не читает файлы напрямую — только через crate-private concrete
`SyncPageManager` поверх `BufferPool`. Полное определение действующей frame-WAL-редакции — в
[implementation_plan.md](implementation_plan.md) §3.3. `read_page(&TxHandle, ...)` возвращает
`PageRead<'tx>` (staged borrow или immutable cache guard), `read_pages` сохраняет порядок и
дубликаты. Snapshot живёт внутри зарегистрированного `TxHandle`, а не передаётся голым token.
LSN и checksum назначает WAL-слой. Checkpoint не бэкфиллит версии новее самого старого живого
`visible_lsn`.

`PAGE_SIZE = 16384`; `PageHeader` (32 байта) хранит `page_lsn` и portable RapidHash V3 checksum
(seed `0`, поле checksum обнулено при вычислении). Ключ `PageKey = (GraphId, DataFileId, PageNo)`;
`graph_id = 0` — системное пространство. При чтении `.fgb` проверяются размер/выравнивание файла,
format version, `page_no`, reserved fields и checksum. Production I/O сейчас только sync:
`O_DIRECT` на Linux и `FILE_FLAG_NO_BUFFERING` на Windows; WAL остаётся buffered. Benchmark-only
mmap и optional Native Async не являются fallback.

### 10.3. Разрешение идентификаторов: `RecordId → DiskAtomRef → AtomId`

Три уровня идентификаторов (`disk_storage_spec.md` §4, §9):
- **`RecordId`** (`table:id`) — публичный, единственный, видимый в GrynQL; хранится как свойство `id`.
- **`DiskAtomRef`** (`u64`: бит40 = kind, биты39..0 = slot) — стабильный дисковый id (без generation).
- **`AtomId`** (`u64`: slot + generation) — внутренний in-memory handle; **никогда не экспонируется** в GrynQL (§3.7 gql_spec).

Разрешение при выполнении:
```
RecordId --(primary B+tree pk_<table_id>.fgidx)--> DiskAtomRef --(read hot slot → gen)--> AtomId
```
`from_disk_ref(r, gen)` восстанавливает `AtomId` из `DiskAtomRef` + generation, прочитанной из hot-slot. Материализация результата (§4.2) выполняет обратный маппинг `AtomId → DiskAtomRef → RecordId` и разыменование `id`-свойства.

Кросс-уровневые endpoint'ы адресуются
`QualifiedAtomRef { sg_slot, local_ref: DiskAtomRef }` (§3.9 disk-spec): когда
`EdgeHotSlot.flags.CROSS_LEVEL = 1`, inline/overflow endpoint'ы разрешаются как qualified refs,
иначе используются plain `DiskAtomRef`.

### 10.4. Как операторы читают данные

**Атомы (hot data).** `NodeHotSlot` и `EdgeHotSlot` имеют по 160 байт
(102 слота/страница) и хранят «горячее»: kind-флаги, `label_id`, `table_id`, счётчики степени,
inline-adjacency и ссылку на свойства (`props_page`/`props_slot`/`props_len`). Адресация — O(1)
без B+tree: `page_no = slot / slots_per_page`,
`offset = 32 + (slot % slots_per_page) × slot_size`.

`table_id` в hot-slot используется планировщиком для быстрой пофильтровой выборки по таблице без обращения к props.

**Adjacency traversal.** Операторы `Expand`/`PathExpand` читают смежность так (`disk_storage_spec.md` §25.2, `out_neighbors`):
```
1. Прочитать NodeHotSlot(node_slot).
2. Взять adj_out[..min(adj_out_count, 4)] до первого NULL_SLOT — inline edge DiskAtomRef'ы.
3. Если adjacency не помещается inline — идти по цепочке overflow:
     каждая AdjacencyOverflowPage: entries[..count] (DiskAtomRef/u64), затем next_page,
     пока != NULL_PAGE.
```
Это зеркалит in-memory модель `fg-meta` (inline `SmallVec` + overflow) при иной стабильной
дисковой ширине ссылок.

**Свойства (cold data).** `PropHeapPage` остаётся row-oriented слоттированной страницей;
`PropertyMap` кодируется как `num_props(uLEB128) + [key_id(u32) + StorageValue]`
(`disk_storage_spec.md` §13.2). Ключи интернированы и отсортированы по `key_id`; duplicate key
при decode является ошибкой. Крупные объекты (> ½ payload) — цепочка `PropLargeChunkPage`.
Late materialization (§7.6): свойства читаются только для строк, прошедших
топологический/label-фильтр.

`PropertyMap` не заменяется `Arrow::MapArray`. Для аналитики отдельный адаптер строит
`PropertyBatch { RecordBatch, presence bitmaps, key_id_to_field }`: `present=0` означает `none`,
`present=1 && valid=0` — `null`, `present=1 && valid=1` — значение. Повторные сканы могут
использовать `PropertyColumnCachePage`; DataFusion подключается только к batch-слою и не
проникает в runtime `Value`.

**Endpoint'ы рёбер.** `EdgeHotSlot` держит `source_inline`/`target_inline`, соответствующие
overflow-цепочки, `weight` (NaN = нет веса) и `transition_gid`. Все V–V, V–E, E–V, E–E,
hyper- и cross-level-связи используют эти два массива. `EdgeEndpointIndex` — производный
обратный индекс для endpoint'ов-edge; отдельной страницы или IR-ветки инцидентности нет.

**Метки и схема.** `string ↔ id` разрешается через `LabelDictionary` (`HashIndexPage`, string→id на этапе разбора/плана/вставки; `DictEntryPage`, id→string при материализации), кешируется LRU (`FG_LABEL_DICT_CACHE_MB`). Определения DDL (Table/Field/Index/Event/Function/Analyzer/LiveSelect) — в `SchemaCatalog`; входная точка на именованный граф — `GraphSuperblock` (даёт `node_count`/`edge_count` для cost-model и root-страницы схемы/статистики).

**Мета-атомы.** `metavertex`/`metaedge` — не отдельные типы хранения, а обычные vertex/edge
с флагом `is_meta` и `subgraph_slot`/`transition_gid`. Edge-to-edge endpoint сам по себе не
делает ребро метаребром. Вложенные подграфы — через `SubgraphDirPage`; доступ из GrynQL —
функциями `graph::inner/nodes/edges` (§15.10 gql_spec).

### 10.5. Доступ к статистике

Планировщик (§6.3) читает `StatisticsStore` (`GraphStatsPage`, `LabelStatsPage`, `PropertyHistogramPage`, `DegreeHistogramPage`) через `PageManager`. Статистика — это то, без чего графовый планировщик не работает (§6.4), поэтому её сбор обязателен для MVP.

### 10.6. Режим 3 — `query_to_memory`

Высокоуровневый API для гибридного режима:
```ignorel
let graph: Dag<Frozen> = db
    .query_to_memory::<Dag<Frozen>>(query, vars, settings)
    .await?;

let working: Dag<Mutable> = db
    .query_to_memory::<Dag<Mutable>>(query, vars, write_back_settings)
    .await?;
```

- Загрузка выполняет запрос на одном `SnapshotId`, в два прохода материализует атомы и
  инцидентность, проверяет constraint `T`, строит нужную storage family и source mapping.
- После успешной загрузки `GraphReadView<'tx>` закрывается. Возвращаемый граф не зависит от
  доступности исторического disk snapshot.
- `T<Frozen>` не содержит dirty-set и flush API.
- `T<Mutable>` содержит source mapping и dirty-set; `flush` является явной fallible операцией
  через новую транзакцию, проверку конфликтов и graph invariants. `Drop` не выполняет скрытый
  commit.
- Публичного неоднозначного `MemoryGraphHandle`/`GraphHandle` нет; backend и storage family
  известны из типа результата.

### 10.7. Graph family и type-state

Общие публичные типы: `Graph<State>`, `Tree<State>`, `RootedTree<State>`, `Forest<State>`,
`Dag<State>`, `Hypergraph<State>`, `Metagraph<State>`, где `State = Mutable | Frozen`.
Один универсальный backing для всех типов не используется:

- general family — универсальная multi-hyper-meta topology;
- forest family — parent/children/roots и tree-specific caches;
- DAG family — incoming/outgoing adjacency и topological rank;
- hyper family — vertices, hyperedges и incidence без meta/cross-level.

`freeze`/`thaw` и owned cross-family conversion могут стоить `O(V+E)`. Расширяющие read-only
views не перестраивают данные; сужающие conversions валидируют и перестраивают representation.
Точная матрица — `gql_spec.md` §3.9 и [graphs.md](graphs.md).

### 10.8. Live graph materialization

`LiveGraph<Frozen>` хранит atomically replaceable `Arc` текущей frozen revision. Changefeed
delta применяется к copy-on-write chunks, валидируется и публикуется одним swap; `read()`
возвращает guard одной revision.

`LiveGraph<Mutable>` дополнительно содержит local dirty-set и incoming queue. При пересечении
remote delta с dirty-set подписка ставится на паузу и выдаёт `LiveEvent::Conflict` с incoming,
local, conflicts и remote revision. Delta не применяется автоматически. После явного
`accept_remote`/`keep_local`/`discard_local`/`rebase`/`flush`/`resume` повторно проверяются
инварианты graph family.

---

## 11. Транзакции, MVCC и снимки в исполнении

Режим 1 реализован в `src/datastore.rs`; `fg-meta/mvcc.rs` используется как набор сценариев и
антипаттернов, но не копируется. В частности, version нельзя назначать в `begin`: поздно
закоммиченная транзакция тогда стала бы видна старому snapshot. Persistent-часть зависит от
frame-WAL и ещё не подключена. Поведение задаёт §17 `gql_spec.md`.

### 11.1. Уровни изоляции

**Serializable (SSI) по умолчанию**; Snapshot Isolation и Read Committed — явный opt-in (§17.1). Причина доменная: под SI структурные инварианты типизированных графов (`DAG`/`TREE`/`BIPARTITE`) не выдерживаются — две конкурентные транзакции независимо проходят проверку, а их объединение нарушает инвариант. Задаётся при `BEGIN ISOLATION LEVEL …`. Вне явной транзакции каждый оператор — автотранзакция.

### 11.2. Снимок на весь запрос

**Инвариант** (§23.5 gql_spec, §55.2): `∀ op: op.snapshot = Γ.Snapshot`. Все чтения (`MATCH`, `SELECT`, traversal) одного запроса видят один снимок; если разные операторы прочитают разные снимки — возможны phantom reads. Snapshot фиксируется как `SnapshotId = (timestamp, tx_id)`.

Timestamp oracle режима 1 — `commit_ts` текущего immutable root. Serializable/SI-транзакция
удерживает этот root от `BEGIN`, а Read Committed атомарно фиксирует отдельный root для каждого
statement и не меняет его между операторами этого statement. В короткой commit-секции новый
`commit_ts = current.commit_ts + 1` назначается только после validation. Затем все изменения базы
публикуются одной заменой root. Поэтому версия с поздним commit физически отсутствует в более
старом root независимо от порядка `BEGIN`.

Текущий исполняемый срез считает один переданный в `Transaction::execute` AST `Program` одной
statement execution unit. Когда появится драйвер нескольких top-level DML statements, он обязан
запрашивать новый RC-root перед каждым statement, не меняя root внутри него.

`SnapshotId` — только identity MVCC-версии. Он не владеет данными и сам по себе не позволяет
читать граф.

### 11.3. GraphView

`GraphReadView<'tx>` — transaction-bound read guard:

- в режиме 1 содержит `Arc` на immutable root in-memory MVCC version;
- в режиме 2 содержит `GraphRef`, `SnapshotId` и доступ к страницам через `PageManager`;
- borrowed atom/subgraph views наследуют `'tx` и не выходят из транзакции.

Самостоятельный возвращаемый `GraphSnapshot` не вводится. `RETURN GRAPH` и
`query_to_memory` сначала полностью материализуют результат, затем освобождают source view.

В рамках одной транзакции граф виден консистентно (§17.3): мутации видны внутри транзакции
немедленно, вне — после `COMMIT`. Traversal **не видит** собственные мутации в рамках одного
шага, если не включён `FG_TRAVERSAL_READ_YOUR_WRITES` (default false).

### 11.4. Обнаружение конфликтов

Режим 1 хранит `properties_version`, `node_set_version`, `edge_set_version` и
`atom_versions[AtomId]` в каждом named graph. Write-write validation по известному dirty-set имеет
`O(|write-set| log |atoms|)`. Точные предикаты не используют текущий `AtomMeta`: query adapter
сохраняет тот же typed matcher, которым исполнялся `WHERE`, а COW-журнал графа перечисляет только
изменившиеся после snapshot атомы.

| Конфликт         | Поведение                                                                       |
|------------------|---------------------------------------------------------------------------------|
| Write-write      | FirstCommitterWins → abort второй                                               |
| Read-write (SI)  | не ловится (возможен write skew)                                                |
| Read-write (SSI) | консервативный abort при устаревшем фактически прочитанном atom                 |
| Phantom (SSI)    | повторный typed-matcher только для изменившихся атомов; abort при смене истины  |

Консервативный вариант намеренно завершает любую исходящую rw-антизависимость, а не только pivot
dangerous structure. Он serializable, но может давать больше abort; усложнение оправдано только
по метрике abort-rate. Read-only транзакции держат safe immutable root и эту проверку не проходят.

### 11.5. Протокол commit

1. проверить snapshot age и atom-delta write-set budget (sync MVP: 256 МиБ);
2. под коротким commit lock проверить FCW и serializable/predicate conflicts;
3. назначить фактический `commit_ts`;
4. слить непересекающиеся atom-delta поверх текущих графов и повторно проверить family invariant;
5. под кратким write-guard зарегистрировать границу очистки predicate history и опубликовать
   единый новый root базы; `BEGIN` регистрирует активный snapshot под read-guard того же root,
   поэтому snapshot не может попасть в окно между очисткой и публикацией.

В чистом in-memory режиме шагов WAL нет и durability после завершения процесса не обещается.
Persistent-адаптер под тем же DB commit lock сливает delta в финальные страницы, последовательно
записывает их фреймы и `TxCommit`, выполняет один buffered `flush + sync_data`, продвигает durable
`visible_lsn` до LSN маркера и только затем публикует root (§17.6, §12). Transaction-private spill
не входит в sync MVP: превышение 256 МиБ завершается resource error.

### 11.6. Graph-специфичная консистентность

Операция graph family сохраняет локальный инвариант немедленно; commit после atom-level merge
строит/проверяет итоговый `Tree`/`RootedTree`/`Forest`/`Dag`/`Hypergraph` повторно. Удаление вершины
каскадно удаляет incident edges в той же транзакции. История predicate changes очищается по
минимальному `start_ts` активных транзакций; просроченная запись исключается из этой границы, а
любая следующая операция её транзакции завершается `FG-25F01`. Удерживаемый Rust handle
освобождает свой старый `Arc<root>` при `Drop`; фоновое принудительное уничтожение handle не
вводится.

---

## 12. WAL, checkpoint, recovery со стороны исполнителя

Согласовано с `disk_storage_spec.md` §22–§24. Модель — **физический журнал из образов страниц (frame-WAL) с индексом версий**, «copy-on-write в логе». Единый журнал на **базу данных**, не на граф. Ни LSM, ни ARIES-фаз Undo/CLR.

### 12.1. Что видит исполнитель

Исполнитель не пишет страницы на месте. Транзакция накапливает atom-delta; под единым DB commit
lock они сливаются в финальные страницы. Коммит дописывает полные фреймы и `TxCommit`, затем
выполняет один buffered `flush + sync_data` на всю транзакцию, сколько бы графов и файлов она ни
затронула. LSN маркера — `commit_lsn`; только после sync он может стать новым `visible_lsn`.

Чтение страницы всегда привязано к `TxHandle`; голый `StorageSnapshot` не выдаётся вызывающему как
самостоятельный долгоживущий token:

```
read_page(&TxHandle, PageKey) =
    staged final page, если он есть,
    самый свежий committed-фрейм с commit_lsn ≤ visible_lsn,
    иначе страница из .fgb
PageKey = (GraphId, DataFileId, PageNo)
```

Отсюда три свойства, на которые опирается исполнение:
1. **Снимок консистентен по всей базе.** Глобальный `commit_ts` задаёт MVCC/SSI-порядок, а
   глобальный `visible_lsn` — durable-префикс WAL для всех графов. Они не сравниваются друг с
   другом; видимость страницы проверяет `commit_lsn ≤ visible_lsn`.
2. **Читатели не блокируют писателя.** Писатель дописывает фреймы, читатель смотрит на префикс журнала.
3. **Отмена транзакции ничего не откатывает.** Отбросить scratch-фреймы и записи индекса; UNDO-лога и CLR не существует.

`PageInit` регистрирует только пустую аллокацию. Как только транзакция записывает в новую страницу
содержательные данные, commit обязан включить полный `PageFrame`; восстановление содержимого из
«логики аллокации» не допускается.

### 12.2. Checkpoint (бэкфилл) и его влияние на планирование

Passive checkpoint переносит **закоммиченные** фреймы обратно в `.fgb` на их штатные места.
Граница не может быть новее самого старого живого reader: иначе home fallback вернёт ему слишком
новую страницу.

```
target = min(durable_visible_lsn,
             oldest_reader_visible_lsn.unwrap_or(durable_visible_lsn))
```

После `fsync` каждого затронутого файла и durable `CheckpointEnd` атомарно заменяются graph
superblocks, последним — `db.fgb`; это единственный commit point. Лишь затем version index и
закрытые сегменты `last_lsn ≤ target` можно prune. Отдельного active-tx watermark нет: до
transaction spill `begin_tx` не пишет WAL.

Практическое следствие для исполнителя: **долгий читатель удерживает журнал**. Многоминутный аналитический обход не даёт обрезать лог, и он растёт. Поэтому:
- долгую аналитику следует выполнять через режим 3 (`query_to_memory`), который материализует подграф и **освобождает снимок**, а не держит его весь расчёт;
- для чтения только отбэкфилленных данных предусмотрен checkpoint-stable режим: такой запрос не удерживает ни одного фрейма и видит состояние на последний checkpoint;
- snapshot timeout/cancellation и `CheckpointMode::Restart` добавляются в этапе 4; срез 3b никого
  не отменяет и просто не продвигается дальше старого reader.

### 12.3. Recovery

Один упорядоченный проход; каталога процедур на каждый вид записи нет — применение фрейма это `memcpy`.

```
1. db.lock + выбор валидного max(generation, page_lsn) из db.fgb/db.fgb.tmp
2. Seed clocks → один streaming-проход WAL `FGW1`, отказ от экспериментального `MVCE` и проверка matching CheckpointEnd
3. Отбросить фреймы без TxCommit  ← весь «откат»
4. Применить post-checkpoint frames В ПОРЯДКЕ ВОЗРАСТАНИЯ frame LSN через sealed registry
5. fsync touched files, проверить home pages, выполнить свежий passive checkpoint
```

**Порядок строго по LSN — обязателен.** Применение по порядку коммитов вместе с монотонным `page_lsn` теряет закоммиченные изменения: если транзакция с меньшим LSN коммитится позже, её правка на общей странице оказывается отфильтрована guard'ом `lsn ≤ page_lsn`. Две транзакции регулярно пересекаются на одной странице (63–102 слота на страницу, общий freelist), поэтому это не краевой случай.

**Память** — `O(число фреймов)`, а не `O(объём payload)`: индекс хранит смещения, а не содержимое.

### 12.4. Torn write

Рваная страница обнаруживается по контрольной сумме и `page_no` и восстанавливается применением фрейма — **фрейм является полным образом**, поэтому отдельного механизма full-page-write не требуется. Единственное условие: обрезка журнала строго после `fsync` файлов данных.

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

Индекс тоже MVCC-aware: `(index entry, ts)`; видимость записи индекса согласована со снимком (§17 gql_spec).

**Разделение ролей `DEFER` и bulk-сборки.** `DEFER` переносит ту же построчную работу в фон — это оптимизация **малой** дельты, а не замена массовому пути: на загрузке в сотни миллионов строк он даёт очередь того же порядка. Поэтому очередь ограничена `FG_INDEX_PENDING_MAX`, а при переполнении происходит эскалация: очередь сбрасывается, индекс помечается `stale`, планируется полная bulk-пересборка. Это crash-safe по построению — устаревший индекс всё равно строится заново из базовых данных, поэтому персистентная очередь (которая незаметно превратилась бы в L0 LSM-дерева) не нужна.

### 13.4. Bulk-путь построения индексов

Построчное сопровождение — правильный steady state для read-heavy нагрузки на NVMe (прецеденты-предостережения: GIN `fastupdate` и InnoDB change buffer решали проблему медленных случайных чтений на HDD, которой здесь нет). Отдельный **bulk-путь** нужен для другого сценария — загрузки и пересборки.

Четыре переиспользуемых компонента (`disk_storage_spec.md` §20.13):

1. **Внешняя сортировка** по непрозрачным ключам. Компаратор — `memcmp`, поскольку кодирование ключей order-preserving; сортируется `encoded_key ‖ DiskAtomRef`, что даёт тотальный порядок и попутно упорядочивает posting-списки.
2. **Упаковщик снизу вверх** — листья по fill factor, posting-страницы, внутренние уровни. `inline_count` известен заранее, поэтому запись пишется в финальной форме и не переписывается.
3. **Side-file + одна WAL-запись + подмена поколения.** Тело сборки не журналируется: файл недостижим из каталога, поэтому крах в процессе безвреден. Сборка индекса на 3 ГБ даёт ~200 байт журнала.
4. **Import epoch** — отсоединённые индексы, отложенные ограничения, сегментные коммиты.

Планировщик обязан задействовать bulk-путь автоматически при `DEFINE INDEX` на непустой таблице, при `REBUILD INDEX` и при `COMPACT INDEX`; `CONCURRENTLY` реализуется поверх него (снимок → сборка в side-file → добор небольшой очереди → атомарная подмена).

**Отложенная проверка ограничений — условие применимости, а не оптимизация.** `VALIDATE ON_MUTATION` для `DAG` делает обход на каждое ребро, то есть `O(E·(V+E))` на загрузку; отложенная проверка — один Tarjan за `O(V+E)`. Режим импорта обязан форсировать `DEFERRED`.

**Производные индексы** (reachability, neighbourhood, path) построчного сопровождения не поддерживают в принципе: одна вставка ребра может инвалидировать `Θ(n)` меток. Они объявляются снимковыми артефактами, актуальными на `build_lsn`; мутация топологии переводит их в `stale`, и планировщик молча ими не пользуется — он откатывается к `ON_DEMAND`.

---

## 14. Управление ресурсами и деградация

Бюджеты — §1.1. Настройки (`gql_spec.md` §29 + `disk_storage_spec.md` §27):

| Настройка                                         | Назначение                           | Default       |
|---------------------------------------------------|--------------------------------------|---------------|
| `FG_PAGE_BUFFER_PAGES`                            | размер buffer pool (страниц)         | 4096 (64 MiB) |
| `FG_WAL_SEGMENT_SIZE`                             | размер WAL-сегмента                  | 64 MiB        |
| `FG_CHECKPOINT_DIRTY_THRESHOLD` / `_INTERVAL_SEC` | триггеры checkpoint                  | 0.25 / 300    |
| `FG_TX_WRITE_SET_MAX_BYTES`                       | atom-delta budget транзакции         | 256 MiB       |
| `FG_MAX_SNAPSHOT_AGE_SEC`                         | максимальный возраст snapshot        | 300           |
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

> **Вынесено в [implementation_plan.md](implementation_plan.md) §1 «Соответствие кодовой базе».**
> Таблица «компонент tech-spec → модуль → статус» и перечень того, что требует реализации,
> объединены там с аналогичной таблицей `gql_spec.md` §31 в единый источник (без дублирования).

---

## 18. Отложено / вне MVP

> **Вынесено в [implementation_plan.md](implementation_plan.md) §6 «Отложено / вне MVP».**
> Сводный список отложенного (JIT, WCOJ, advanced storage-функции, ARIES/UNDO, параллельный
> recovery, optional Native Async I/O до прохождения sync-gate, SPDK, SMT-верификация
> оптимизатора, тензорные кеши) собран там из `gql_tech_spec.md`,
> `gql_spec.md` §32.3 и `disk_storage_spec.md`.

---

*Версия 0.3-draft. Синхронизируется с `gql_spec.md`, `gql_grammar.md`, `disk_storage_spec.md`. При расхождении приоритет у этих трёх спецификаций.*
