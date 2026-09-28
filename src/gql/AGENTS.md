# GQL module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/gql`.

## Архитектурная цепочка

`gql` сохраняет один pipeline:

```text
src/syn parse -> analysis -> logical IR -> canonical rules -> physical optimizer
              -> execution -> datastore adapter
```

- `analysis.rs` владеет name/type resolution и semantic errors;
- `ir.rs` — typed logical plan, не storage plan;
- `optimizer/` — единственный crate-private rule/CBO optimizer, physical plans,
  fixed-point estimates/costs, deterministic plan encoding и EXPLAIN projection;
- `execution/adapter.rs` — единственная граница к transaction snapshot;
- `execution/cursor.rs` — pull-based relational cursors;
- `execution/eval.rs` — вычисление typed expressions;
- `execution/mod.rs` — orchestration и statement results;
- `mod.rs` — публичный facade и re-exports syntax types.

## Инварианты

- Не обходить analysis прямым исполнением AST.
- IR и executor не зависят от page/WAL DTO; storage доступен только через
  transaction/query adapter.
- Optimizer получает только read-only `PlannerSnapshot`: catalog/index metadata,
  exact counters и востребованные histograms. `DatabaseRoot`, `DataFileId`,
  страницы и physical addresses через эту границу не проходят.
- Executor исполняет выбранные physical operators. Resource fallback допустим
  только внутри заранее выбранного семейства (например, in-memory hash join в
  partitioned hash join), но не как скрытый повторный planner.
- Canonical rules не меняют bag semantics, LEFT/CROSS barriers, effective
  properties, порядок наблюдаемых ошибок и floating reduction order.
- Forced index hint является контрактом: каждый указанный source использует
  совместимый Ready index либо получает planning error до чтения строк.
- `DETERMINISTIC` использует canonical bytes и versioned RapidHash V3; Debug,
  `DefaultHasher` и iteration order hash maps не являются частью PlanHash.
- Публичный `QueryResult` содержит `Outcome<Value>` и стабильные
  `RecordId`/`AtomRef`, никогда внутренние IDs.
- Ошибка получает правильную execution phase и source `TextSpan`; внешние
  ошибки редактируются системой `gql_status`.
- Сохранять детерминированный порядок statements, rows, bindings и projection.
- Новая конструкция добавляется вертикально: grammar/token -> AST -> analysis
  -> IR -> execution -> backend tests. Пустые variants «на будущее» запрещены.
- Core DDL проходит этой же цепочкой и изменяет транзакционный catalog через
  datastore adapter. `CLASS` задаёт namespace и immutable atom kind;
  `INFO FOR DB|CLASS|INDEX` читает transaction-visible definitions и состояние
  индекса.
- Core DML выполняет общий preflight целей и `EXPECT` по pre-image до первой
  мутации. Direct DML адресует global atom по `RecordId = class:id`, а
  graph-scoped DML читает effective properties.
- `ATTACH`, `DETACH`, class-aware `RELATE ... IN GRAPH`, graph-local `LABEL` и
  `RESET` проходят typed IR. Alias, class и label — независимые понятия;
  выводить class из label запрещено.
- `MutateStatement.label` и соответствующее поле IR намеренно имеют форму
  `Option<Option<_>>`: внешний `None` означает отсутствие clause и сохранение
  прежнего label, `Some(None)` — явный `LABEL NONE`, `Some(Some(_))` — замену.
- MATCH сначала выбирает global exact/class кандидатов, затем пересекает их с
  graph-local label/membership indexes. Старый `(alias:label)` и `FROM graph`
  не являются compatibility grammar.
- MATCH связывает `AtomOccurrenceRef`, а не голый `AtomRef`: каждая column
  сама несёт свой graph occurrence context. Один `row.graph` не может быть
  источником истины для cross-level путей, AtomWalk и `WITHIN`.
- StandardPath, AtomWalk, hyper, nesting и shortest проходят один typed
  MATCH pipeline и один physical-plan family. Не добавлять второй executor
  или скрытый выбор алгоритма в cursor: mode, bounds, objective и filters
  фиксируются physical plan.
- `TRAVERSE` проходит тот же typed pipeline и переиспользует MATCH, graph cache,
  `PathTraceArena`, optimizer и query spill. Traversal VM остаётся concrete
  sync pull executor; второй matcher, универсальный traversal trait и отдельная
  async runtime запрещены.
- `RETURN GRAPH` и `BUILD GRAPH` проходят один concrete
  `GraphAssemblyAccumulator`: source читается pull-потоком без промежуточного
  `Vec<Value>`, edge/containment closure добавляет только необходимые atoms, а
  публичный `MaterializedGraph` не удерживает transaction, snapshot или page
  guard. Обычный `graph::inner` возвращает `GraphRef`; копирование всегда явно.
- Именованный `BUILD GRAPH` передаёт уже проверенный owned graph одной bulk
  mutation через datastore adapter. Executor не создаёт base atoms, storage
  generations или отдельную публикацию на каждый atom.
- Language closure принадлежит создавшей её `GqlSession`, учитывается вместе с
  retained typed plan и captures в общем лимите 256 МиБ и не сериализуется.
  Mutation допустима только в явном `tv::side_effect`; schema mutation во время
  traversal запрещена.
- Bulk является только внутренней оптимизацией: equality включает current,
  trace, sacks и env, а observable path либо impure side effect отключает
  небезопасное слияние. Resource exhaustion fail-closed и не выдаёт partial
  traversal result.
- Все рассмотренные topology branches считаются в `max_path_expansions`;
  frontier/history/trace учитываются в statement barrier budget. При исчерпании
  бюджета path execution fail-closed и не выдаёт partial path.
- Mode 3 materialization возвращает owned effective graph; mutable write-back
  выполняется только явной delta flush operation.

## Спецификации и проверка

Поведение задают `docs/gql_spec.md` и утверждённые части
`docs/gql_tech_spec.md`; при конфликте не угадывать. Минимальная проверка:
parser/semantic/execution tests одной конструкции, RAM/local parity при
storage access, затем `cargo check --lib --no-default-features` и rustfmt.
