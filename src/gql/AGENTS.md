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
- MATCH сначала выбирает global exact/class кандидатов, затем пересекает их с
  graph-local label/membership indexes. Старый `(alias:label)` и `FROM graph`
  не являются compatibility grammar.
- Mode 3 materialization возвращает owned effective graph; mutable write-back
  выполняется только явной delta flush operation.

## Спецификации и проверка

Поведение задают `docs/gql_spec.md` и утверждённые части
`docs/gql_tech_spec.md`; при конфликте не угадывать. Минимальная проверка:
parser/semantic/execution tests одной конструкции, RAM/local parity при
storage access, затем `cargo check --lib --no-default-features` и rustfmt.
