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
- Shared statement timeout normalization переносит clause из direct MATCH,
  BUILD GRAPH и wrapped EXPLAIN в единственное outer Statement поле. Parser
  не разрешает lexical/reactive значения; typed conversion остаётся в GQL.
- `logos` сейчас не используется: перенос требует полного equivalence набора
  для comments, escapes, quoted identifiers, parameters, spans и errors.
- Syntax errors создаются через `gql_status`; semantic validation принадлежит
  `gql::analysis`.
- `stmt/schema.rs` владеет core `DEFINE`/`REMOVE`/`ALTER` для
  `GRAPH|CLASS|FIELD|INDEX|PARAM|FUNCTION` и schema `INFO`; `stmt/maintenance.rs`
  владеет `ANALYZE`/`REBUILD`/`COMPACT`/`KILL` и их bounded options.
- `stmt/dml.rs`, `stmt/graph_dml.rs` и `stmt/match.rs` разделяют direct atom
  DML, graph occurrence DML и pattern grammar. `RecordId` selector, alias,
  optional label и `IN GRAPH` не должны сливаться в один syntax field.
- `TABLE`, `(alias:label)` и `FROM graph` отвергаются; test-only compatibility
  parser не поддерживается.
- Неподдерживаемые DDL/DML clauses завершаются явной диагностикой, а не создают
  speculative AST variants.
- `stmt/live.rs` владеет LIVE SELECT/LIVE LET/ACK syntax; schema module парсит
  ALTER/INFO/REMOVE LIVE LET. Reactive identity, DAG, capture и runtime value
  принадлежат analysis/execution/datastore, а не parser.
- `stmt/event.rs` владеет EVENT owner/options/body syntax; schema dispatch
  подключает DEFINE/ALTER/REMOVE EVENT. Effects, causal depth и frozen versions
  проверяются в GQL/datastore. Новый SELECT graph terminal хранится в owned Box,
  чтобы optional payload не увеличивал каждую обычную statement/future.
- Именованные аргументы допускаются только после positional; duplicate name и positional после
  named отклоняются до analysis. Signature qualifier UDF содержит только declared argument
  types и не использует return type как overload discriminator.
- MATCH global `WITH COST fn::name` стоит после IN GRAPH перед WHERE; AST хранит owned
  function name и byte span. Scope/default/override и проверка функции принадлежат analysis.
- TRAVERSE `WITH COST fn::name` стоит после optional outer IN GRAPH перед первым `|>`;
  embedded MATCH сохраняет собственный COST scope. Signature/version/effect проверяет analysis.

## Проверка

Private SDK SELECT metadata (`union_inputs`, `lexical_id`, empty source,
`PathExpr` projection alias, `SelectTarget::BoundRecord` class/parameter names)
не расширяет текстовую grammar. BoundRecord разрешает immutable nonreactive
literal slot в обычном analyzer; runtime/storage DTO в AST не хранятся.
Она содержит только
syntax recipes, сохраняет исходный VALUE mode и проверяется обычным analyzer.
Executable IR и storage DTO сюда не переносятся.

Для token/grammar изменения добавлять positive/negative boundary tests,
Unicode byte spans, adjacency/whitespace cases и parser-never-panics property
test. После этого запускать затронутые parser tests, `cargo check --lib
--no-default-features` и rustfmt.

EXPLAIN admission включает LIVE SELECT/LIVE LET, LET с LIVE SELECT, INFO FOR LIVE
[LET], ACK и KILL. Ordinary LET/control flow не становятся explainable. Logical
phases, slot provenance, fixed snapshots и side effects принадлежат analysis/execution.
