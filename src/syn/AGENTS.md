# Syntax frontend guidance

Этот файл дополняет корневой `AGENTS.md` для `src/syn`.

## Назначение

`syn` — единственный frontend GrynnQL: lexer, tokens, AST, statements и parser
state. Он не выполняет запросы и не разрешает schema names/types.

## Карта владельцев

- `token.rs` — token kinds и source spans;
- `lexer/mod.rs` — UTF-8 byte scan, comments, literals, operators и
  `gap_before`;
- `lexer/keywords.rs` — case-insensitive keyword lookup;
- `parser.rs` — один token buffer/state, precedence и depth limits;
- `ast` — expressions и общие syntax nodes;
- `stmt` — по одному owner-файлу на поддерживаемый statement.

## Инварианты

- `TextSpan` хранит UTF-8 byte offsets; diagnostics обязаны указывать допустимые
  boundaries исходной строки.
- Lexer greedily распознаёт многосимвольные graph/operators и сохраняет
  информацию о пробеле там, где она влияет на grammar.
- Parser не паникует на произвольном вводе и применяет оба depth limits.
- AST остаётся простым owned tree. Arena/ID/macro codegen вводятся только после
  измеренной проблемы с памятью или compile time.
- `logos` сейчас не используется: перенос требует полного equivalence набора
  для comments, escapes, quoted identifiers, parameters, spans и errors.
- Syntax errors создаются через `gql_status`; semantic validation принадлежит
  `gql::analysis`.
- `stmt/schema.rs` владеет core `DEFINE`/`REMOVE`/`ALTER` и
  `INFO FOR DB|TABLE|INDEX`. Неподдерживаемые DDL clauses должны завершаться
  явной диагностикой, а не создавать speculative AST variants.

## Проверка

Для token/grammar изменения добавлять positive/negative boundary tests,
Unicode byte spans, adjacency/whitespace cases и parser-never-panics property
test. После этого запускать затронутые parser tests, `cargo check --lib
--no-default-features` и rustfmt.
