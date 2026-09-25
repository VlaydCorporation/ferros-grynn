# Отчёт этапа 12: выражения и управляющий язык GrynnQL

Дата: 2026-09-25.

## Результат

Этап 12 реализовал утверждённый исполнимый профиль выражений и control flow,
сохранив pipeline `parse → structural validation → statement analysis → logical
IR → execution`. AST напрямую не исполняется.

Поддержаны:

- scalar parentheses, numeric coercions, set/range literals;
- field/index/null-safe idioms, destructuring и array filter;
- collection membership/containment operators и единая truthiness;
- statement/expression `IF`, `CASE`, `FOR`, `WHILE`, `BREAK`, `CONTINUE`;
- expression statements и lexical block/loop/catch scopes;
- статический pure/stable builtin catalog;
- `TRY/CATCH`, `THROW`, runtime-only `ErrorValue`;
- `ASSERT INVARIANTS [FOR GRAPH ...] [REPORT N]`;
- разрешённый control flow внутри Atomic Import с приоритетом языкового
  savepoint над `ON VIOLATION`.

## Expression и builtin core

Typed IR различает collections, ranges, idioms, calls и control blocks.
`FOR` сохраняет порядок array, использует канонический порядок set и принимает
только конечный integer range. Общая truthiness используется операторами и
условными конструкциями.

Built-ins разрешаются статическим enum/signature table и обычным `match`:

- `math::{abs,sqrt,pow,floor,ceil}`;
- `string::{upper,lower,len,contains,split,concat}`;
- `time::{now,format,add,diff}`;
- `array::{len,concat,distinct,first,flatten}`;
- `set::{len,union,intersect,diff,has}`;
- `type::{is<T>,to<T>}`;
- `meta::{id,class,type}`;
- `option::{is_some,is_none}`.

Динамический registry, новая dependency, общая closure-система и query
subexpressions не добавлялись.

## Savepoints и ошибки

`TransactionSavepoint` сохраняет private root, read/missing/range/write и
requirement sets, fanout reservations, write accounting, mutation log и GQL
bindings. `TRY` восстанавливает его до запуска handler. Тот же механизм
переиспользует statement-atomic mutation boundary.

`ErrorValue` является безопасной runtime-проекцией существующего
`DiagnosticRecord`; storage conversion явно его отклоняет. Catchability
определяется классификацией существующего status, а timeout, cancellation,
conflict, I/O, resource и internal invariant не становятся catchable.

`ASSERT INVARIANTS` использует общий validator и возвращает
`checked_graphs`, `violation_total`, полные `violation_totals` и bounded
`violation_details`. Assertion failure можно обработать `CATCH`.

## Atomic Import

Recursive preflight допускает только семь DML этапа 10, `IF`, `FOR`, `TRY` и
`THROW`. Простое top-level DML, top-level `IF`/`TRY` и каждая итерация top-level
`FOR` образуют item boundary. Вложенный control flow остаётся частью item.
Непойманный item-level violation передаётся `ON VIOLATION`; обработанная ошибка
восстанавливается внутренним savepoint, а `THROW`, timeout, cancellation и
resource failure abort-ят весь import.

## Correctness и resource evidence

Полные library gates этапа 11 одновременно покрывают stage-12 parser,
analyzer, IR, evaluator, built-ins, control flow, savepoints, assertions и
import integration:

| Проверка                         |                       Результат |
|----------------------------------|--------------------------------:|
| no-default library               | 374 passed, 0 failed, 3 ignored |
| native-async library             | 376 passed, 0 failed, 4 ignored |
| local transaction/control reopen |                        1 passed |
| status macro tests               |                        5 passed |

Focused tests проверяют numeric cross-product, range boundaries,
deterministic collections, static built-ins, IF/CASE/FOR/WHILE, nested
TRY/rollback, ErrorValue fields, assertion над private root, assertion
catchability, import item accounting и приоритет TRY над violation policy.

Отдельный allocation-tracking probe сравнил 10 и 100 000 loop iterations:
retained memory не выросла более чем на 64 KiB, то есть executor не накапливает
значения всех итераций.

## Отложенная поверхность

- aggregates, variance/stddev, grouping и query subexpressions — этап 13;
- `DETERMINISTIC`/`FAST` — этапы 14/17;
- graph/path/pipeline functions — этапы 16/17;
- UDF, общие closures, mutable collection functions, sleep и stateful matcher
  — этап 21;
- search — этап 22, geo — этап 24.

Эти формы либо дают typed unsupported, либо не входят в grammar; скрытого
fallback и speculative runtime machinery нет.
