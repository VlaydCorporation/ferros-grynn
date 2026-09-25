# Этапы 11–12: транзакционные операторы, выражения и управляющий язык GrynnQL

## Статус

- [x] T0–T7: транзакционные операторы и persistent `GqlSession`;
- [x] E0–E7: выражения, управляющий язык и интеграция с Atomic Import.

Завершено 2026-09-25. Evidence:
[`gql_transaction_stage_11_report.md`](reports/gql_transaction_stage_11_report.md)
и [`gql_control_stage_12_report.md`](reports/gql_control_stage_12_report.md).
Функциональные, RAM/local/reopen/resource gates пройдены. Одно-statement
performance probe показал `+5,09%` при лимите `+5%`; по явному решению
пользователя дальнейшая микрооптимизация отложена в `Q-34`, а превышение не
помечается как успешно пройденный performance gate.

Текущий прогресс отмечается в [`full_plan.md`](full_plan.md). Этот документ
фиксирует контракт и dependency-ordered порядок реализации; отчёты этапов не
переписывают его задним числом.

## 1. Результат и границы

После этапа 11 поддерживаются:

- ограниченная одним вызовом `Datastore::execute` программа
  `BEGIN … COMMIT/CANCEL`;
- постоянная `GqlSession`, в которой транзакция продолжается между несколькими
  вызовами `execute`;
- отдельная автотранзакция для каждого top-level statement вне `BEGIN`;
- `READ COMMITTED`, `SNAPSHOT`, `SERIALIZABLE`, `READ ONLY`, timeout и
  cooperative cancellation.

После этапа 12 работают скалярные выражения, set/range и collection idioms,
статически известные built-ins, `IF/CASE`, `FOR/WHILE`, `BREAK/CONTINUE`,
`TRY/CATCH`, `THROW` и `ASSERT INVARIANTS`. Разрешённый control-flow подключён
к Atomic Import, причём языковой `TRY/CATCH` предшествует `ON VIOLATION`.

Сохраняется единственный pipeline:

```text
parse → structural validation → statement analysis → logical IR → execution
```

AST напрямую не исполняется. Новые traits, фабрики, runtime function registry,
общая closure-система и зависимости не добавляются.

## 2. Транзакционный контракт

### 2.1. Statement boundaries

- Программа полностью разбирается и структурно проверяется до исполнения.
- Семантический анализ выполняется непосредственно перед каждым statement по
  текущему published/private schema root; DDL виден следующему statement.
- Ошибка автотранзакции откатывает только текущий statement. Уже опубликованные
  statements сохраняются.
- Непойманная ошибка в явной транзакции немедленно откатывает её целиком.
- `BEGIN` внутри активной транзакции и `COMMIT/CANCEL` без неё — типизированные
  ошибки.
- `Datastore::execute` требует закрытой транзакции в конце программы и
  откатывает незавершённый `BEGIN`.
- `GqlSession` может удерживать транзакцию между вызовами; `Drop` только
  откатывает и никогда не публикует.
- `Transaction::execute` запрещает transaction statements и `BEGIN IMPORT` до
  мутации. Непойманная GQL-ошибка отменяет транзакцию; прямые Rust-методы
  сохраняют statement-atomic поведение.

### 2.2. Isolation и commit

- Default — `SERIALIZABLE READ WRITE`.
- `READ COMMITTED` фиксирует root один раз в начале каждого top-level statement.
- `SNAPSHOT` и `SERIALIZABLE` удерживают один root до конца транзакции.
- Мутация в `READ ONLY` откатывает явную транзакцию.
- Обычный `COMMIT` выполняет FCW/SSI и hard constraints.
- `COMMIT WITH VALIDATION` дополнительно проверяет весь private root тем же
  validator, что Atomic Import и `ASSERT INVARIANTS`.
- Только `BEST_EFFORT` исполним в этапах 11–12. Явные `DETERMINISTIC` и `FAST`
  распознаются, но отклоняются до открытия транзакции; владельцы — этапы 14 и 17.

### 2.3. Scopes

Имена разрешаются в порядке `block → transaction → session/program → execute
parameters`. `LET` вне транзакции принадлежит временной программе либо
persistent session; внутри транзакции живёт до её завершения. Block/loop/catch
bindings уничтожаются при выходе. Shadowing внешнего scope разрешён, повторное
объявление в одном scope — ошибка, кроме discard-binding `$_`. Параметры одного
`execute` неизменяемы и не могут перекрывать существующий session/transaction
binding.

### 2.4. Результаты и ошибки

Parse/structural ошибки возвращаются внешним `FerrosGrynnResult`, и программа не
запускается. Analysis/runtime/timeout/commit/publication error разобранного
statement хранится в его `QueryResult`, после чего выполнение прекращается.
Добавляются `QueryStatementKind::{Begin,Commit,Cancel,Expression,If,For,While,
Try,Throw,Assert}`. Вложенные statements не создают публичные результаты;
значение блока — последний результат либо `none`.

## 3. Публичная execution boundary

Добавляются конкретные типы:

```rust
pub struct GqlSession { /* private */ }
pub struct QueryExecutionOptions { /* settings, timeout, budgets, cancellation */ }
#[derive(Clone)]
pub struct QueryCancellation { /* Arc<AtomicBool> */ }
```

- `Datastore::session() -> GqlSession`;
- `Datastore`, `GqlSession` и `Transaction` получают `execute_with_options`;
- существующие `execute`/`execute_with_settings` остаются wrappers;
- `GqlSession::has_active_transaction()` сообщает состояние.

Defaults: timeout отсутствует, control depth `20`, суммарный loop budget одного
top-level statement `1_000_000`, cancellation отсутствует. Эффективный deadline
— минимум clause и API option. Проверки выполняются на границах operators,
cursor pull и loop backedge, а в collection built-ins — каждые 1024 элемента.
Timeout/cancellation не перехватываются и откатывают явную транзакцию. Системный
sync I/O прерывается после возврата вызова.

## 4. Исполнимый профиль выражений

### 4.1. Входит в этап 12

- numeric coercions §5.6, parentheses, set/range literals;
- field/index/null-safe idioms, object destructuring, `array[WHERE predicate]`;
- `?=`, `*=`, `IN`, `NOT IN`, `ALLIN`, `ANYIN`, `NONEIN`, `CONTAINS*`;
- statement/expression `IF/ELSE`, `CASE`;
- `FOR`, `WHILE`, `BREAK`, `CONTINUE`, expression statements;
- `TRY/CATCH`, `THROW`, `ASSERT INVARIANTS`;
- статически известные scalar built-ins.

Единая truthiness: falsey — `false`, числовой ноль, пустые
string/bytes/collection/object, `null` и `none`. `FOR` принимает array в
исходном порядке, set в каноническом порядке и конечный integer range (`..` /
`..=`). Остальные варианты дают typed error.

### 4.2. Built-ins

Статический enum/signature table и `match` реализуют:

- `math::{abs,sqrt,pow,floor,ceil}`;
- `string::{upper,lower,len,contains,split,concat}`;
- `time::{now,format,add,diff}`; `now()` фиксируется на начало statement;
- `array::{len,concat,distinct,first,flatten}`;
- `set::{len,union,intersect,diff,has}`;
- `type::{is<T>,to<T>}`;
- `meta::{id,class,type}`;
- `option::{is_some,is_none}`.

`distinct` сохраняет первое вхождение, `flatten` снимает один уровень,
`first([])` возвращает `none`, set-result канонически упорядочен. Domain,
overflow и incompatible numeric errors используют текущую typed value-error
систему. Неявный `decimal → float` в float-контексте добавляет notification о
потере точности.

### 4.3. Отложенная поверхность

- aggregates, `stddev/variance`, `array::group`, query subexpressions — этап 13;
- `DETERMINISTIC`/`FAST` — этапы 14/17;
- graph/path/pipeline functions — этапы 16/17;
- UDF, общие closures, `SLEEP`, stateful regex/matcher, mutable collection
  functions и остальной catalog — этап 21;
- search — этап 22; geo — этап 24; computed fields — назначенный schema/query
  этап.

`CATCH |e| { … }` — специальный handler с одним binding, не общая closure.

## 5. Ошибки, savepoint и validation

Один внутренний transaction savepoint переиспользуется `atomic_mutation` и
`TRY`: private root, read/missing/range/write/requirement sets, fanout
reservations, accounting, mutation log и lexical scopes восстанавливаются до
запуска handler.

Перехватываются пользовательски исправимые value/type/domain errors, DML
item-level violations, missing/duplicate identity/membership, row-local
constraints, graph-family mutation errors, `THROW` и assertion failure. Не
перехватываются parse/analysis/planning, budgets, timeout/cancellation,
serialization conflict, I/O/checksum, resource exhaustion и internal invariant.

Runtime-only `ErrorValue` — безопасная проекция `DiagnosticRecord` (code,
message, condition, classification, category, domain и cause chain). Storage
codecs его отклоняют. `THROW` string создаёт generated `UserThrown`; повторный
выброс использует `THROW $e.message`.

`ASSERT INVARIANTS [FOR GRAPH ...] [REPORT N]` проверяет private root общим
validator. `REPORT` ограничен `0..=FG_IMPORT_MAX_REPORTED_VIOLATIONS`. Успех
возвращает structured zero-total object, нарушение — typed error с полными
totals и bounded details; ошибка catchable.

## 6. Atomic Import

До writer lease выполняется рекурсивный preflight. В import body разрешены семь
DML этапа 10, `IF/ELSE`, `FOR`, `TRY/CATCH`, `THROW`; `WHILE`, `LET`, `ASSERT`,
result-producing `RETURN` и transaction statements запрещены.

Item boundary: простой top-level DML — item; каждая итерация top-level `FOR` —
item; top-level `IF`/`TRY` — item; nested flow остаётся частью item. TRY создаёт
nested savepoint. Только непойманное item-level violation поступает в
`ON VIOLATION`; `THROW`, timeout, cancellation и resource error abort-ят import.
Успешный no-op считается item для `SEGMENT SIZE`.

## 7. Dependency-ordered реализация

### Этап 11

| Шаг | Зависит от | Результат                                                                 |
|-----|------------|---------------------------------------------------------------------------|
| T0  | этап 10    | Этот документ, синхронизация spec/grammar/tech spec, statuses и baseline. |
| T1  | T0         | Transaction AST/parser, duration `TIMEOUT`, structural preflight.         |
| T2  | T1         | Analysis текущего statement по актуальному catalog и concrete scopes.     |
| T3  | T2         | RAII `StatementContext`, стабильный RC root внутри statement.             |
| T4  | T3         | Общий concrete driver: autocommit и bounded explicit transaction.         |
| T5  | T4         | Persistent `GqlSession`, scopes, rollback on error/Drop.                  |
| T6  | T4–T5      | Options, cancellation/deadline, READ ONLY, shared full validator.         |
| T7  | T6         | RAM/local/reopen/concurrency gate, `Q-07`, измерение `Q-06`, отчёт.       |

### Этап 12

| Шаг | Зависит от  | Результат                                                              |
|-----|-------------|------------------------------------------------------------------------|
| E0  | T7          | Exact signatures/truthiness/order/range/catchability matrix и goldens. |
| E1  | E0          | AST/IR/evaluator: expressions, coercions, idioms и collections.        |
| E2  | E1          | Статический pure/stable builtin catalog.                               |
| E3  | E1          | Blocks, scopes, IF/FOR/WHILE/BREAK/CONTINUE и budgets.                 |
| E4  | E3          | `ErrorValue`, общий savepoint, THROW и TRY/CATCH.                      |
| E5  | E3, T6      | ASSERT через общий validator.                                          |
| E6  | E4, этап 10 | Recursive import preflight, item boundaries, nested savepoints.        |
| E7  | E2, E5, E6  | RAM/local/import gate, отчёт и обновление deferred owners.             |

E2 может выполняться независимо от E3 после E1.

## 8. Gates

### Этап 11

Проверяются unmatched/nested transactions; несколько blocks и autocommit
statements; multi-call session; Drop/CANCEL/error rollback; partial autocommit и
whole explicit rollback; DDL→DML visibility; RC statement refresh/stability;
Snapshot/Serializable и FCW/SSI; READ ONLY; validation commit;
timeout/cancellation; ранний отказ `Transaction::execute`; lifetime/shadowing
всех scopes.

### Этап 12

Обязательны parser/analyzer/IR/evaluator goldens, numeric cross-product,
null/none/truthiness, deterministic ranges/sets, idioms/destructuring/filter,
builtin type/domain/overflow, loop boundaries/budgets/cancellation,
BREAK/CONTINUE placement, nested TRY/savepoint bookkeeping, catchability,
THROW/ErrorValue fields, ASSERT над private state, import ABORT/SKIP/TRY и
одинаковый RAM/local/reopen результат.

### Финальный gate

- focused parser/GQL/datastore/import/status tests;
- полные library tests без default features и с `native-async-io`;
- `cargo check` в обеих конфигурациях;
- `cargo +nightly fmt --all -- --check` и focused Clippy;
- one-statement `Datastore::execute` не медленнее baseline более чем на 5%;
- loop memory не растёт из-за накопления результатов;
- `graphify update .`;
- отчёты `gql_transaction_stage_11_report.md` и
  `gql_control_stage_12_report.md`.
