# GQL module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/gql`.

## Архитектурная цепочка

`gql` сохраняет один pipeline:

```text
src/syn parse -> analysis -> logical IR -> execution -> datastore adapter
```

- `analysis.rs` владеет name/type resolution и semantic errors;
- `ir.rs` — typed logical plan, не storage plan;
- `execution/adapter.rs` — единственная граница к transaction snapshot;
- `execution/cursor.rs` — pull-based relational cursors;
- `execution/eval.rs` — вычисление typed expressions;
- `execution/mod.rs` — orchestration и statement results;
- `mod.rs` — публичный facade и re-exports syntax types.

## Инварианты

- Не обходить analysis прямым исполнением AST.
- IR и executor не зависят от page/WAL DTO; storage доступен только через
  transaction/query adapter.
- Публичный `QueryResult` содержит `Outcome<Value>` и стабильные
  `RecordId`/`AtomRef`, никогда внутренние IDs.
- Ошибка получает правильную execution phase и source `TextSpan`; внешние
  ошибки редактируются системой `gql_status`.
- Сохранять детерминированный порядок statements, rows, bindings и projection.
- Новая конструкция добавляется вертикально: grammar/token -> AST -> analysis
  -> IR -> execution -> backend tests. Пустые variants «на будущее» запрещены.
- Core DDL проходит этой же цепочкой и изменяет транзакционный catalog через
  datastore adapter. `INFO FOR DB|TABLE|INDEX` читает transaction-visible
  definitions и показывает состояние индекса; расширенные index kinds и
  неутверждённый `ALTER GRAPH` возвращают явный unsupported status.
- Core DML выполняет общий preflight целей и `EXPECT` по pre-image до первой
  мутации. Graph-backed `CREATE/UPDATE/UPSERT/DELETE` проходят typed IR;
  `RELATE` нельзя выводить из совпадения edge label и table name — его identity
  contract должен быть сначала утверждён в `gql_spec`.

## Спецификации и проверка

Поведение задают `docs/gql_spec.md` и утверждённые части
`docs/gql_tech_spec.md`; при конфликте не угадывать. Минимальная проверка:
parser/semantic/execution tests одной конструкции, RAM/local parity при
storage access, затем `cargo check --lib --no-default-features` и rustfmt.
