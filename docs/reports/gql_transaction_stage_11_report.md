# Отчёт этапа 11: транзакционные операторы GrynnQL

Дата: 2026-09-25.

## Результат

Этап 11 завершил функциональный транзакционный контур GrynnQL:

- `Datastore::execute` выполняет каждый statement вне `BEGIN` в отдельной
  автотранзакции;
- `BEGIN … COMMIT/CANCEL` работает как ограниченная одним вызовом программа;
- `GqlSession` сохраняет транзакцию и bindings между вызовами, а `Drop` только
  откатывает незавершённое состояние;
- `READ COMMITTED` обновляет root один раз на границе statement, тогда как
  `SNAPSHOT` и `SERIALIZABLE` удерживают исходный snapshot;
- `READ ONLY`, `COMMIT WITH VALIDATION`, timeout и cooperative cancellation
  имеют типизированные ошибки и утверждённое rollback-поведение;
- semantic analysis выполняется перед текущим statement, поэтому DDL виден
  следующему DML без исполнения AST напрямую.

Публичная граница состоит из `GqlSession`, `QueryExecutionOptions` и
`QueryCancellation`. Внутренний concrete driver находится в
`src/datastore/session.rs`; отдельные traits, фабрики и runtime registry не
добавлялись.

## Реализация

- Transaction AST и parser вынесены в `src/syn/stmt/transaction.rs`; duration
  `TIMEOUT` разбирается общим parser.
- `src/gql/analysis.rs` анализирует один заимствованный statement по текущему
  catalog. Временный `Program` и clone AST в hot path удалены.
- `Transaction::run_gql_statement` образует стабильную statement boundary:
  Read Committed refresh выполняется ровно один раз, вложенные query reads его
  не повторяют.
- `src/datastore/session.rs` объединяет autocommit и explicit transaction,
  проверяет структуру всей программы до первой мутации и хранит lexical
  bindings в правильном session/transaction scope.
- Full-root validation переиспользует общий validator AtomStore, ссылок,
  graph-family invariants и derived bindings.
- Новые статусы транзакционного состояния, timeout, cancellation и control
  limits генерируются существующим `define_status_codes!`.

## Correctness evidence

Проверены unmatched/nested transaction statements, несколько transaction
blocks в программе, partial autocommit, whole-transaction rollback,
persistent session через несколько вызовов, Drop/CANCEL, DDL→DML visibility,
READ COMMITTED refresh, Snapshot/Serializable stability, READ ONLY,
`COMMIT WITH VALIDATION`, timeout/cancellation и ранний отказ
`Transaction::execute`.

Фактические gates:

| Проверка                                                                                |                       Результат |
|-----------------------------------------------------------------------------------------|--------------------------------:|
| `cargo test --lib --no-default-features -- --test-threads=1`                            | 374 passed, 0 failed, 3 ignored |
| `cargo test --lib --no-default-features --features native-async-io -- --test-threads=1` | 376 passed, 0 failed, 4 ignored |
| local reopen `gql_transaction_and_control_state_survive_reopen`                         |                        1 passed |
| `cargo test -p ferros-grynn-macros --lib`                                               |                        5 passed |
| `cargo check --lib --no-default-features`                                               |                          passed |
| тот же check с `native-async-io`                                                        |                          passed |

Local reopen подтверждает, что explicit transaction, nested savepoint,
validation commit и последующее control expression дают тот же результат после
повторного открытия базы.

## Q-06: Serializable concurrency

GrynnQL point reads участвуют в текущей SSI/FCW validation. Диагностический
probe disjoint Serializable writers дал:

| Writers | Operations | `FG-40001` aborts |
|--------:|-----------:|------------------:|
|       1 |         16 |                 0 |
|       2 |         32 |                 1 |
|       4 |         64 |                37 |
|       8 |        128 |               101 |

Это не ошибка statement driver: независимые логические записи конкурируют за
текущий физический slot allocator и последующее root merge. `Q-06` остаётся
открытым до этапа 19. Следующий минимальный эксперимент — разнести reservation
диапазоны/merge ownership для disjoint writers и повторить тот же paired probe;
dangerous-structure history до такого доказательства не добавляется.

## Performance gate

Paired probe собран с `--profile profiling` и сравнил текущий одно-statement
`Datastore::execute("RETURN 1")` с прежним прямым transaction path в том же
checkout. После удаления clone AST и лишнего результата получены медианы:

- control: `1620,20 ns/op`;
- current: `1702,60 ns/op`;
- ratio: `1,0509` (`+5,09%`).

Цель `≤ +5%` формально превышена на 0,09 процентного пункта. По явному решению
пользователя код оставлен без дальнейшей микрооптимизации; долг зарегистрирован
как `Q-34` и не скрывается как успешно пройденный performance gate. Functional
и correctness gates этапа завершены.

## Tooling

Только относящиеся к этапам 11–12 файлы приведены к nightly rustfmt, чтобы не
переписывать чужой dirty checkout. Workspace-wide `fmt --check` и Clippy всё
ещё видят существующий форматный шум и тысячи старых lint violations в
несвязанных `storage`, `graph`, `memory` и `values`; эти ошибки не выдаются за
регрессии текущего этапа.
