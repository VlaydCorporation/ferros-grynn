# GrynQL — Грамматика языка запросов Ferros-Grynn

**Версия:** 0.2-draft
**Статус:** проектирование
**Основано на:** [gql_spec.md](gql_spec.md) v0.2-draft

---

## О названии

Язык запросов движка Ferros-Grynn называется **GrynQL** (*Grynn Query Language*, произносится «грин-кью-эл»).

Обоснование:
- Имя наследуется от движка (**Ferros-Grynn**) и мгновенно ассоциируется с ним.
- Суффикс `QL` фиксирует принадлежность к семейству языков запросов, но при этом имя **не совпадает** с зарезервированным
  ISO-термином `GQL` (стандарт ISO/IEC 39075), чтобы не создавать путаницы со стандартным Graph Query Language.
- Короткое, произносимое, свободно образует производные: `grynql` (CLI/расширение файлов `.grynql`), `GrynQLError`, `grynql::parse(...)`.

> В остальном тексте спецификаций сохраняется исторический термин «GQL»; он и «GrynQL» — синонимы.

---

## Содержание

1. [Нотация](#1-нотация)
2. [Лексическая грамматика](#2-лексическая-грамматика)
3. [Структура программы](#3-структура-программы)
4. [Операторы (statements)](#4-операторы-statements)
5. [DML — мутации](#5-dml--мутации)
6. [MATCH и паттерны графа](#6-match-и-паттерны-графа)
7. [Traversal Pipeline](#7-traversal-pipeline)
8. [Граф как результат](#8-граф-как-результат)
9. [DDL — DEFINE / REMOVE](#9-ddl--define--remove)
10. [Административные и live-операторы](#10-административные-и-live-операторы)
11. [Общие clauses](#11-select-и-общие-clauses)
12. [Выражения](#12-выражения)
13. [Паттерны и цели](#13-паттерны-и-цели)
14. [Система типов](#14-система-типов)
15. [Зарезервированные слова](#15-зарезервированные-слова)
16. [Замечания для парсера](#16-замечания-для-парсера)

---

## 1. Нотация

Грамматика записана в расширенной форме Бэкуса — Наура (EBNF).

| Мета-символ        | Значение                                             |
|--------------------|------------------------------------------------------|
| `::=`              | Определение правила                                  |
| `\|`               | Альтернатива                                         |
| `[ x ]`            | Необязательный элемент (0 или 1)                     |
| `{ x }`            | Повторение (0 или более)                             |
| `( x )`            | Группировка                                          |
| `"..."`            | Терминал-литерал (пунктуация, оператор)              |
| `UPPERCASE`        | Терминал-ключевое слово (регистронезависим, см. §16) |
| `lower_snake_case` | Нетерминал                                           |
| `(* ... *)`        | Комментарий грамматики                               |
| `x⁺`               | То же, что `x { x }` — одно или более                |

Соглашения:
- **Ключевые слова регистронезависимы**: `SELECT`, `select`, `Select` эквивалентны.
- Списки, разделённые запятыми, оформлены как отдельные правила `*_list` и допускают завершающую запятую там, где это указано.
- Пробелы и комментарии игнорируются между токенами (кроме случаев внутри path-constraint списков, где значим порядок токенов, а не пробелы).

---

## 2. Лексическая грамматика

### 2.1. Пробелы и комментарии

```ebnf
ws            ::= " " | "\t" | "\r" | "\n"
comment       ::= line_comment | block_comment
line_comment  ::= ( "--" | "//" ) { any_char_except_newline }
block_comment ::= "/*" { any_char } "*/"          (* без вложенности *)
```

`--` — канонический однострочный комментарий; `//` поддерживается как алиас.

### 2.2. Идентификаторы

```ebnf
ident        ::= ident_start { ident_cont }
ident_start  ::= unicode_letter | "_"
ident_cont   ::= unicode_letter | unicode_digit | "_"
quoted_ident ::= "`" { any_char_except_backtick } "`"
name         ::= ident | quoted_ident
```

Обратные кавычки позволяют использовать произвольные Unicode-символы и зарезервированные слова как имена
(таблиц, полей, меток, идентификаторов записей).

### 2.3. Параметры

```ebnf
param      ::= "$" ident
```

`$name`, `$limit`, `$_` (сброс результата). `$before`, `$after`, `$event`, `$value`, `$input` — контекстные параметры событий.

### 2.4. Пространства имён функций

```ebnf
builtin_ns  ::= "math" | "string" | "time" | "array" | "set" | "geo"
              | "search" | "graph" | "type" | "meta" | "regex"
              | "option" | "computed" | "tv" | ident
builtin_fn  ::= builtin_ns "::" ident
user_fn      ::= "fn" "::" ident
fn_name      ::= builtin_fn | user_fn
```

`tv::` — пространство traversal-шагов (§7). `fn::` — пользовательские функции.

### 2.5. Числовые литералы

```ebnf
integer      ::= digit { digit | "_" }
int_lit      ::= [ "-" ] integer
float_lit    ::= [ "-" ] integer "." integer [ exponent ]
              | [ "-" ] integer exponent
exponent     ::= ( "e" | "E" ) [ "+" | "-" ] integer
number_lit   ::= int_lit | float_lit
```

Тип `decimal` получается явным приведением (`<decimal> 3.14`). Целое вне диапазона
`[-9223372036854775808; 9223372036854775807]`, стоящее в позиции `id`-части записи, трактуется как строка (§3.7 spec).

### 2.6. Строковые литералы

```ebnf
string_lit   ::= sq_string | dq_string | triple_string
sq_string    ::= "'" { escaped_char | any_char_except_quote } "'"
dq_string    ::= "\"" { escaped_char | any_char_except_dquote } "\""
triple_string::= "\"\"\"" { any_char } "\"\"\""      (* многострочная *)
escaped_char ::= "\\" ( "n" | "t" | "r" | "\\" | "\"" | "'" | "u" hex4 | ... )   (* JSON-стиль *)
```

### 2.7. Темпоральные и специальные литералы

```ebnf
datetime_lit ::= "d" string_lit                (* d'2025-02-14T01:52:50.375Z' *)
duration_lit ::= duration_term { duration_term }
duration_term::= integer duration_unit
duration_unit::= "ns" | "us" | "µs" | "ms" | "s" | "m" | "h" | "d" | "w" | "y"
bool_lit     ::= "true" | "false"
null_lit     ::= "null"
none_lit     ::= "none"
```

### 2.8. Литералы-коллекции

```ebnf
array_lit    ::= "[" [ expr_list [ "," ] ] "]"
tuple_lit    ::= "tuple" "[" [ expr_list [ "," ] ] "]"
set_lit      ::= "set" "[" [ expr_list [ "," ] ] "]"
object_lit   ::= "{" [ object_field { "," object_field } [ "," ] ] "}"
object_field ::= object_key ":" expr
object_key   ::= ident | string_lit | quoted_ident
expr_list    ::= expr { "," expr }
```

### 2.9. Литерал записи (record) и диапазоны

```ebnf
record_id    ::= table_name ":" id_part
table_name   ::= name
id_part      ::= ident
              | quoted_ident
              | integer
              | string_lit
              | array_lit
              | object_lit
              | id_gen_fn
id_gen_fn    ::= "rand" "::" "id" "(" ")" | "ulid" "(" ")" | "uuid" "(" ")"

(* Диапазоны записей — только в позиции цели (§13.1) *)
record_range ::= table_name ":" [ range_bound ] range_op [ range_bound ]
range_op     ::= ".." | "..="
range_bound  ::= id_part
```

Примеры диапазонов: `person:1..1000`, `temperature:['London', NONE]..=['London', ..]`,
`temperature:..['London', '...']`, `t:['London','...']..`.

### 2.10. Литералы (сводное правило)

```ebnf
literal ::= number_lit | string_lit | bool_lit | null_lit | none_lit
          | datetime_lit | duration_lit
          | array_lit | tuple_lit | set_lit | object_lit
```

---

## 3. Структура программы

```ebnf
program      ::= [ statement ] { ";" [ statement ] }
block        ::= "{" [ statement ] { ";" [ statement ] } "}"
```

- Операторы разделяются `;`; завершающий `;` необязателен (§4.5 spec).
- Блок `{ ... }` — последовательность операторов; значение блока — его последнее выражение, если контекст это допускает (§4.4 spec).

---

## 4. Операторы (statements)

```ebnf
statement ::=
      let_stmt
    | return_stmt
    | if_stmt
    | for_stmt
    | while_stmt
    | break_stmt
    | continue_stmt
    | begin_stmt
    | commit_stmt
    | cancel_stmt
    | throw_stmt
    | sleep_stmt
    | try_stmt
    | explain_stmt
    | assert_invariants_stmt
    | import_stmt
    | alter_index_stmt
    | compact_stmt
    | define_stmt
    | remove_stmt
    | rebuild_stmt
    | info_stmt
    | show_changes_stmt
    | kill_stmt
    | create_stmt
    | update_stmt
    | upsert_stmt
    | delete_stmt
    | relate_stmt
    | build_graph_stmt
    | select_stmt
    | live_select_stmt
    | match_stmt
    | traverse_stmt
    | return_graph_stmt
    | expr_stmt

expr_stmt ::= expr        (* самостоятельное выражение / pipeline: `$xs |> tv::…`, вызов функции и т.п. *)
```

### 4.1. LET

```ebnf
let_stmt ::= "LET" param [ ":" type ] "=" expr
```

### 4.2. RETURN

```ebnf
return_stmt ::= "RETURN" expr [ "AS" alias ]
```

`RETURN GRAPH` — отдельная конструкция, см. §8. Внутри тела функции `RETURN` завершает выполнение.

### 4.3. Условные операторы

```ebnf
if_stmt   ::= "IF" expr block
              { "ELSE" "IF" expr block }
              [ "ELSE" block ]
```

Выражение-форма `IF ... THEN ... ELSE ... END` и `CASE` описаны в §12.9.

### 4.4. Циклы

```ebnf
for_stmt      ::= "FOR" for_binder "IN" expr block
for_binder    ::= param | ident
while_stmt    ::= "WHILE" "(" expr ")" block
break_stmt    ::= "BREAK"
continue_stmt ::= "CONTINUE"
```

`BREAK` / `CONTINUE` допустимы только внутри `FOR` / `WHILE`.

### 4.5. Транзакции

```ebnf
begin_stmt   ::= "BEGIN" [ "ISOLATION" "LEVEL" isolation_level ] [ "READ" "ONLY" ]
                 [ "EXECUTION" "MODE" execution_mode ]
execution_mode ::= "DETERMINISTIC" | "BEST_EFFORT" | "FAST"
isolation_level ::= "READ" "COMMITTED" | "SNAPSHOT" | "SERIALIZABLE"
commit_stmt  ::= "COMMIT" [ "WITH" "VALIDATION" ]
cancel_stmt  ::= "CANCEL"
```

### 4.6. Прочие управляющие операторы

```ebnf
throw_stmt   ::= "THROW" expr
sleep_stmt   ::= "SLEEP" ( duration_lit | expr )
try_stmt     ::= "TRY" block [ "CATCH" closure ]

explain_stmt ::= "EXPLAIN" [ "ANALYZE" | "FULL" ]
                 [ "FORMAT" ( "TEXT" | "JSON" ) ]
                 statement

assert_invariants_stmt ::= "ASSERT" "INVARIANTS" [ "FOR" "GRAPH" graph_ref ] [ "REPORT" integer ]

(* Массовая загрузка: эпоха импорта поверх обычного DML *)
import_stmt  ::= "BEGIN" "IMPORT" [ "INTO" "GRAPH" name ] [ with_options ]
                 { statement ";" }
                 "END" "IMPORT" [ "WITH" "VALIDATION" ]
graph_ref    ::= name | param
```

`CATCH`-обработчик — замыкание `|e| { ... }`, где `e : error` (§7.9 spec). `EXPLAIN` доступен и как
префиксный оператор (здесь), и как суффиксная clause `... EXPLAIN [FULL]` (§11.11).

---

## 5. DML — мутации

### 5.1. CREATE

```ebnf
create_stmt ::= "CREATE" create_target_list
                [ content_or_set ]
                [ create_return ]
                [ expect_clause ]
                [ timeout_clause ]

create_target_list ::= create_target { "," create_target }
create_target ::= record_id | table_name | range_gen | "(" record_id ")"
range_gen     ::= "|" table_name ":" integer "|"          (* |person:5| — генератор диапазона *)

content_or_set ::= "CONTENT" expr
                 | "SET" assignment_list
assignment_list ::= assignment { "," assignment }
assignment      ::= idiom "=" expr

create_return ::= "RETURN" "NONE"
                | "RETURN" "VALUE" statement_param
                | "RETURN" statement_param_list
statement_param      ::= idiom | "*"
statement_param_list ::= statement_param { "," statement_param }
```

### 5.2. UPDATE / UPSERT

```ebnf
update_stmt ::= "UPDATE" update_target_list
                [ update_data ]
                [ where_clause ]
                [ mutate_return ]
                [ expect_clause ]
                [ timeout_clause ]

upsert_stmt ::= "UPSERT" update_target_list
                [ update_data ]
                [ where_clause ]                (* WHERE или явный ID обязателен для UPSERT *)
                [ mutate_return ]
                [ expect_clause ]
                [ timeout_clause ]

update_target_list ::= target { "," target }

update_data ::= "CONTENT" expr
              | "MERGE"   expr
              | "PATCH"   expr
              | "REPLACE" expr
              | "SET"   assignment_list
              | "UNSET" idiom_list

idiom_list  ::= idiom { "," idiom }

mutate_return ::= "RETURN" "NONE"
                | "RETURN" "BEFORE"
                | "RETURN" "AFTER"
                | "RETURN" "DIFF"
                | "RETURN" "VALUE" statement_param
                | "RETURN" statement_param_list
```

### 5.3. DELETE

```ebnf
delete_stmt ::= "DELETE" target_list
                [ where_clause ]
                [ mutate_return ]
                [ expect_clause ]
                [ timeout_clause ]
```

### 5.4. RELATE

```ebnf
relate_stmt ::= "RELATE" ( relate_binary | relate_hyper )
                [ "WEIGHTED" expr ]
                [ content_or_set ]
                [ mutate_return ]
                [ expect_clause ]
                [ timeout_clause ]

relate_binary ::= relate_endpoint edge_dir_left edge_atom edge_dir_right relate_endpoint
relate_endpoint ::= "(" ( record_id | idiom | param ) ")" | record_id | idiom | param
edge_atom     ::= "[" [ alias ] ":" label_expr [ object_lit ] "]"
                | "(" record_id ")"
edge_dir_left  ::= "<-" | "-"
edge_dir_right ::= "->" | "-"
(* Комбинации: -[:E]->  (a→b) | <-[:E]-  (b→a) | -[:E]-  (undirected) | <-[:E]->  (bidirectional) *)

relate_hyper ::= "HYPER" edge_atom "TARGETS" "[" relate_target_list "]"
                 [ hyper_direction ]
relate_target_list ::= relate_endpoint { "," relate_endpoint }
hyper_direction ::= "AS" "DIRECTED"   [ hyper_headtail ]
                  | "AS" "UNDIRECTED"
                  | "AS" "BOTH"       [ hyper_headtail ]
hyper_headtail ::= "(" "head" ":" expr "," "tail" ":" array_lit ")"
```

`WEIGHTED` задаёт вес (`duration` — для темпоральных графов, иначе `number`).

---

## 6. MATCH и паттерны графа

```ebnf
match_stmt ::= "MATCH" [ match_cardinality ] [ match_mode ] [ atom_kw ] [ match_scope ]
               [ constraints_clause ]
               match_pattern_list
               [ from_clause ]
               [ where_clause ]
               [ contains_clause ]
               [ with_clause ]
               [ order_clause ]
               [ timeout_clause ]
               [ maxdepth_clause ]
               [ constraints_clause ]
               [ keep_clause ]
               [ match_return ]

match_cardinality ::= "OPTIONAL" | "DISTINCT"
match_mode        ::= "REPEATABLE" "ELEMENTS" | "DIFFERENT" "EDGES" | "DIFFERENT" "ATOMS"
atom_kw           ::= "ATOM"                        (* включает AtomWalk-режим *)
match_scope       ::= "HYPER" | "META" | "METAVERTEX" | "METAEDGE"

match_pattern_list ::= match_pattern { "," match_pattern }
match_pattern      ::= [ alias "=" ] ( graph_pattern | atomwalk_pattern | hyper_pattern )

maxdepth_clause    ::= "MAXDEPTH" integer
constraints_clause ::= "CONSTRAINTS" "<" path_constraint { path_constraint } ">"

match_return ::= "RETURN" [ "DISTINCT" ] ( return_item_list | graph_return_body )
return_item_list ::= return_item { "," return_item }
return_item      ::= expr [ "AS" alias ]
```

### 6.1. Паттерны вершин и рёбер

```ebnf
graph_pattern ::= pattern_term { edge_pattern pattern_term }

(* Вложенность в мета-атомы: читается слева направо, от листа к корню *)
pattern_term  ::= node_pattern { within_op node_pattern }
within_op     ::= "WITHIN" [ "*" "[" [ integer ] ".." [ integer ] "]" ]

node_pattern  ::= "(" [ alias ] [ ":" label_expr ] [ node_filter ] ")"
                | meta_vertex_pattern
meta_vertex_pattern ::= "<" node_pattern ">"          (* <(n:Label)> *)
node_filter   ::= object_lit | ( "WHERE" expr )

edge_pattern  ::= edge_dir_left edge_body edge_dir_right
                | "->" | "<-" | "-"                    (* ребро без тела: -[]-> и т.п. *)

edge_body     ::= "[" [ alias ] [ ":" label_expr ] [ var_length ]
                  { edge_flag } { path_constraint } [ node_filter ] [ cost_clause ] "]"
                | meta_edge_body
meta_edge_body ::= "<" "[" [ alias ] [ ":" label_expr ] "]" ">"   (* <[e:Type]> *)

label_expr    ::= label { "|" label }                 (* :TYPE|OTHER *)
label         ::= name

var_length    ::= "*" [ length_spec ]
length_spec   ::= integer ".." integer                (* *1..3 *)
                | integer ".."                         (* *3.. *)
                | ".." integer                         (* *..5 *)
                | integer                              (* *3 — ровно 3 *)

edge_flag     ::= "CROSS_LEVEL"
cost_clause   ::= "COST" fn_name
```

### 6.2. Модификаторы пути (path constraints)

```ebnf
path_constraint ::= path_mode | path_objective | path_filter
path_mode       ::= "WALK" | "TRAIL" | "SIMPLE" | "ACYCLIC"
path_objective  ::= "SHORTEST" | "WEIGHTED" "SHORTEST"
path_filter     ::= "WEIGHTED" | "META" | "METAVERTICES" | "METAEDGES"
                  | "HYPEREDGES" | "CROSS-LEVEL"
```

Модификаторы задаются двумя способами (§12.7 spec): внутри `[ ... SIMPLE SHORTEST ]` для конкретного пути
либо в `CONSTRAINTS <SIMPLE SHORTEST>` для всего `MATCH`. Порядок модификаторов не важен.

### 6.3. Гипер-паттерны

```ebnf
hyper_pattern ::= "HYPER" edge_atom                   (* MATCH HYPER [e:TYPE] ... *)

contains_clause ::= "CONTAINS" contains_arg { "," contains_arg }
contains_arg    ::= [ "START" | "END" ] contains_atoms
contains_atoms  ::= "[" node_pattern { "," node_pattern } "]"
                  | node_pattern
                  | record_id
```

### 6.4. AtomWalk-паттерны

```ebnf
atomwalk_pattern ::= atom_node { atomwalk_step atom_node }

atom_node      ::= node_pattern
                 | edge_as_node
                 | "<" "(" [ alias ] ":" type_name ")" ">"    (* <(x:edge)> — атом заданного подтипа *)
edge_as_node   ::= "[" [ alias ] [ ":" label_expr ] "]"

atomwalk_step  ::= endpoint_incidence | participation_incidence
endpoint_incidence ::=
      "~[" edge_inner "]~>"      (* Source → Target *)
    | "~[" edge_inner "]~"       (* ненаправленная endpoint-инцидентность *)
    | "<~[" edge_inner "]~"      (* Target ← Source *)
    | edge_pattern               (* классические ->, <-, - тоже допустимы *)
participation_incidence ::=
      "~>"                       (* participation Source→Target (между рёбрами) *)
    | "~~"                       (* participation любой роли *)
edge_inner     ::= [ alias ] [ ":" label_expr ]
```

---

## 7. Traversal Pipeline

```ebnf
traverse_stmt ::= "TRAVERSE" [ "ATOM" ] traverse_source pipeline_tail [ traverse_return ]

traverse_source ::=
      "FROM" graph_expr pipeline_step        (* граф-источник + первый порождающий шаг *)
    | "FROM" ( record_id | param | idiom )   (* начать с конкретного атома *)
    | "FROM" match_stmt                       (* MATCH как источник *)
    | "FROM" select_stmt                      (* SELECT как источник *)
    | "(" node_pattern ")"                    (* краткая форма: TRAVERSE (p:Person {...}) *)

graph_expr ::= name                           (* именованный граф *)
             | param
             | fn_call                         (* graph::inner(mv) *)
             | "(" statement ")"               (* подзапрос, ... RETURN GRAPH *)

pipeline_tail  ::= { "|>" pipeline_step }
pipeline_step  ::= step_call { "." step_call }    (* цепочки вида repeat(...).until(...) *)
step_call      ::= step_name "(" [ arg_list ] ")"
step_name      ::= [ "tv" "::" ] ident            (* префикс tv:: необязателен при отсутствии конфликтов *)

traverse_return ::= "RETURN" return_item_list
```

Pipeline-оператор `|>` лево-ассоциативен (§12.10). Источником pipeline может быть любой оператор,
возвращающий набор (`SELECT`, `MATCH`, `TRAVERSE`, массив, переменная).

Шаги (аргумент `arg_list` может содержать замыкания и именованные аргументы):
`tv::V`, `tv::E`, `tv::out`, `tv::in`, `tv::both`, `tv::outE`, `tv::inE`, `tv::bothE`, `tv::inV`, `tv::outV`,
`tv::hyper`, `tv::meta_inner`, `tv::has`, `tv::hasattr`, `tv::hasLabel`, `tv::where`, `tv::dedup`, `tv::dedup_by`,
`tv::simplePath`, `tv::cyclicPath`, `tv::map`, `tv::flatMap`, `tv::values`, `tv::select`, `tv::match`, `tv::as`,
`tv::project`, `tv::init_sack`, `tv::init_named_sack`, `tv::sack`, `tv::named_sack`, `tv::get_sack`,
`tv::get_named_sack`, `tv::side_effect`, `tv::path`, `tv::emit`, `tv::limit`, `tv::skip`, `tv::order`,
`tv::group`, `tv::repeat`, `tv::branch`, `tv::choose`, `tv::coalesce`, `tv::count`, `tv::sum`, `tv::min`,
`tv::max`, `tv::avg`, `tv::fold`, `tv::unfold`, `tv::shortestPath`, `tv::weightedShortestPath`, `tv::algo`, … (§13.4 spec).

Модификаторы `repeat`: `.times(n)`, `.until(closure)`, `.emit()`.

---

## 8. Граф как результат

```ebnf
return_graph_stmt ::= "RETURN" "GRAPH" [ graph_constraint_ann ] graph_return_body
graph_return_body ::= "{" "nodes" ":" array_lit "," "edges" ":" array_lit "}"
graph_constraint_ann ::= "<" graph_constraint ">"

build_graph_stmt ::= "BUILD" "GRAPH"
                     [ "OVERWRITE" | "IF" "NOT" "EXISTS" ]
                     [ graph_constraint_ann ]
                     [ name ]
                     "FROM" build_source
                     [ "NODES" expr ]
                     [ "EDGES" expr ]
                     [ "LIMIT" "NODES" integer ]
                     [ "LIMIT" "EDGES" integer ]
build_source     ::= match_stmt | select_stmt | traverse_stmt | "(" statement ")" | graph_expr
```

`graph_constraint` — тот же набор типов графов, что и в `DEFINE GRAPH` (§9.8, §14.4). Проверка типа выполняется
**после** построения результата; несоответствие → `ConstraintViolation`.

---

## 9. DDL — DEFINE / REMOVE

```ebnf
define_stmt ::= define_table | define_field | define_param | define_function
              | define_event | define_index | define_analyzer | define_graph

overwrite_or_ifne ::= "OVERWRITE" | "IF" "NOT" "EXISTS"
if_exists         ::= "IF" "EXISTS"
```

### 9.1. DEFINE TABLE

```ebnf
define_table ::= "DEFINE" "TABLE" [ overwrite_or_ifne ] name
                 [ "TYPE" ( "ANY" | "NORMAL" | "RELATION" | "SCHEMALESS" | "SCHEMAFULL" ) ]
                 [ "DROP" ]
                 [ "AS" select_stmt ]
                 [ "CHANGEFEED" duration_lit ]
                 [ "COMMENT" string_lit ]
```

### 9.2. DEFINE FIELD

```ebnf
define_field ::= "DEFINE" "FIELD" [ overwrite_or_ifne ] name "ON" [ "TABLE" ] name
                 [ "TYPE" type ]
                 [ ( "VALUE" | "COMPUTED" ) expr ]
                 [ "DEFAULT" expr ]
                 [ "ASSERT" expr ]
                 [ "READONLY" ]
                 [ "COMMENT" string_lit ]
```

### 9.3. DEFINE PARAM

```ebnf
define_param ::= "DEFINE" "PARAM" [ overwrite_or_ifne ] param "VALUE" expr
                 [ "COMMENT" string_lit ]
```

### 9.4. DEFINE FUNCTION

```ebnf
define_function ::= "DEFINE" [ fn_purity ] [ "COST" ] "FUNCTION" [ overwrite_or_ifne ]
                    user_fn "(" [ fn_param_list ] ")" [ "->" type ]
                    block
                    [ "COMMENT" string_lit ]
fn_purity     ::= "PURE" | "STABLE" | "IMPURE"
fn_param_list ::= fn_param { "," fn_param }
fn_param      ::= ( ident | param ) ":" type
```

Тело функции — блок; допускается финальное `RETURN` или `RETURNS`-значение (§9.4 spec).

### 9.5. DEFINE EVENT

```ebnf
define_event ::= "DEFINE" "EVENT" [ overwrite_or_ifne ] name
                 "ON" ( [ "TABLE" ] name | "GRAPH" name )
                 [ "ASYNC" [ "RETRY" integer ] [ "MAXDEPTH" integer ] ]
                 "WHEN" expr
                 "THEN" ( block | expr )
```

`MAXDEPTH` ∈ [0, 16] (default 3). Контекст: `$event`, `$before`, `$after`, `$value`, `$input`.

### 9.6. DEFINE INDEX

```ebnf
define_index ::= define_table_index | define_graph_index

(* --- Табличные индексы --- *)
define_table_index ::= "DEFINE" "INDEX" [ overwrite_or_ifne ] name
                       "ON" [ "TABLE" ] name
                       ( "FIELDS" | "COLUMNS" ) idiom_list
                       [ table_index_kind ]
                       [ "COMMENT" string_lit ]
                       [ "CONCURRENTLY" ]
                       [ with_options ]
                       [ "DEFER" ]

table_index_kind ::=
      "UNIQUE"
    | "COUNT"
    | "FULLTEXT" "ANALYZER" name [ "BM25" [ "(" number_lit "," number_lit ")" ] ] [ "HIGHLIGHTS" ]
    | "HNSW" "DIMENSION" integer [ "TYPE" vector_type ] [ "DIST" distance ]
             [ "EFC" integer ] [ "M" integer ]
    | "GEOMETRY" [ "TYPE" name ]
vector_type ::= "F64" | "F32" | "I64" | "I32" | "I16"
distance    ::= "COSINE" | "EUCLIDEAN" | "MANHATTAN" | "MINKOWSKI" | fn_name

(* --- Графовые индексы --- *)
define_graph_index ::= "DEFINE" "INDEX" [ overwrite_or_ifne ] name
                       "ON" [ "GRAPH" ] name
                       graph_index_kind
                       [ "EDGES" label ]
                       [ "DIRECTION" ( "OUT" | "IN" | "BOTH" ) ]
                       [ "PATTERNS" expr ]
                       [ "COMMENT" string_lit ]
                       [ "CONCURRENTLY" ]
                       [ with_options ]
                       [ "DEFER" ]

graph_index_kind ::=
      "PATH"          [ "ALGORITHM" path_algo ]
    | "REACHABILITY"  [ "ALGORITHM" reach_algo ]
    | "NEIGHBOURHOOD" "(" integer ")" [ "ALGORITHM" neigh_algo ]

path_algo  ::= "LANDMARK_SSSP" "(" integer ")" | "PATTERN_CACHE" | "ON_DEMAND"
reach_algo ::= "TREE_COVER" | "GRAIL" [ "(" integer ")" ] | "FERRARI"
             | "BFL" [ "(" integer ")" ] | "ON_DEMAND"
neigh_algo ::= "EXACT" | "SKETCH" | "LANDMARK" "(" integer ")" | "ON_DEMAND"
```

### 9.7. DEFINE ANALYZER

```ebnf
define_analyzer ::= "DEFINE" "ANALYZER" [ overwrite_or_ifne ] name
                    [ "FUNCTION" fn_name ]
                    [ "TOKENIZERS" tokenizer_list ]
                    [ "FILTERS" filter_list ]
                    [ "COMMENT" string_lit ]

tokenizer_list ::= tokenizer { "," tokenizer }
tokenizer      ::= "blank" | "camel" | "class" | "punct"
filter_list    ::= filter { "," filter }
filter         ::= "ascii" | "lowercase" | "uppercase" | "nfc" | "nfkc"
                 | "edgengram" "(" integer "," integer ")"
                 | "ngram" "(" integer "," integer ")"
                 | "snowball" "(" ident ")"
                 | "mapper" "(" string_lit ")"
```

### 9.8. DEFINE GRAPH

```ebnf
define_graph ::= "DEFINE" "GRAPH" [ overwrite_or_ifne ] name
                 [ "TYPE" graph_constraint ]
                 [ "ROOT" root_ref | "ROOTS" array_lit ]
                 [ "VALIDATE" ( "ON_MUTATION" | "DEFERRED" ) ]
                 [ "COMMENT" string_lit ]
root_ref ::= record_id | "(" record_id ")"

graph_constraint ::= graph_type { graph_type }        (* совместимые ограничения комбинируются *)
graph_type       ::= "SIMPLE" | "TREE" | "ROOTED_TREE" | "FOREST" | "DAG"
                   | "BIPARTITE" | "COMPLETE" | "HYPER" | "META"
```

### 9.9. REMOVE

```ebnf
remove_stmt ::= "REMOVE" remove_target
remove_target ::=
      "DATABASE" [ if_exists ] name
    | "TABLE"    [ if_exists ] name
    | "GRAPH"    [ if_exists ] name
    | "EVENT"    [ if_exists ] name "ON" [ "TABLE" ] name
    | "FIELD"    [ if_exists ] name "ON" [ "TABLE" ] name
    | "INDEX"    [ if_exists ] name "ON" [ "TABLE" ] name
    | "ANALYZER" [ if_exists ] name
    | "FUNCTION" [ if_exists ] user_fn
    | "PARAM"    [ if_exists ] param
```

---

## 10. Административные и live-операторы

```ebnf
info_stmt ::= "INFO" "FOR" (
                  "SYSTEM"
                | "DB"
                | "TABLE" name
                | "INDEX" name "ON" [ "TABLE" ] name
              )

show_changes_stmt ::= "SHOW" "CHANGES" "FOR" "TABLE" name
                      "SINCE" ( datetime_lit | integer | expr )
                      [ "LIMIT" integer ]

kill_stmt ::= "KILL" ( param | expr )

rebuild_stmt ::= "REBUILD" "INDEX" [ if_exists ] name "ON" [ "TABLE" ] name
                 [ "CONCURRENTLY" ] [ with_options ]

alter_index_stmt ::= "ALTER" "INDEX" name "SET" ( "ENABLED" | "DISABLED" | "READ_ONLY" )

compact_stmt ::= "COMPACT" ( "INDEX" name | "GRAPH" name ) [ "CONCURRENTLY" ]

(* Общий блок опций сборки: FILL_FACTOR, SORT_MEM, PARALLELISM, ... *)
with_options ::= "WITH" "(" option_assign { "," option_assign } ")"
option_assign ::= ident "=" expr

live_select_stmt ::= "LIVE" "SELECT" live_projection
                     "FROM" target_list
                     [ where_clause ]
                     [ fetch_clause ]
                     [ "SINCE" ( param | integer | datetime_lit ) ]
                     [ "ON" "OVERFLOW" overflow_policy ]
                     [ "BUFFER" integer ]
overflow_policy  ::= "GAP" | "DISCONNECT" | "BLOCK"
live_projection ::= [ "VALUE" ] select_field_list [ "AS" alias ]
                  | "DIFF"
                  | "PATCH"
```

`LIVE SELECT` обычно связывается с идентификатором: `LET $id = LIVE SELECT ...`, где `$id` используется в `KILL`.

---

## 11. SELECT и общие clauses

### 11.1. SELECT

```ebnf
select_stmt ::= "SELECT" [ "ONLY" ] select_projection
                "FROM" target_list
                [ with_clause ]
                [ where_clause ]
                [ split_clause ]
                [ group_clause | order_clause ]
                [ limit_clause ]
                [ start_clause ]
                [ fetch_clause ]
                [ timeout_clause ]
                [ "TEMPFILES" ]

select_projection ::= "VALUE" select_field [ omit_clause ]
                    | select_field_list [ omit_clause ]
select_field_list ::= select_field { "," select_field }
select_field      ::= "*"
                    | expr [ "AS" ( idiom | alias ) ]   (* алиас может быть вложенным: AS name.last *)
```

`omit_clause` также может стоять перед `FROM` (§10.9 spec). `SELECT ONLY` требует ровно одной строки.

### 11.2. Clauses

```ebnf
from_clause  ::= "FROM" target_list
target_list  ::= target from_join { from_join }
from_join    ::= { "," target }                        (* union *)
              | { "&" target }                          (* cross join *)

where_clause ::= "WHERE" expr

with_clause  ::= "WITH" ( "NOINDEX"
                        | "INDEX" name { "," name }
                        | "COST" fn_name )

split_clause ::= "SPLIT" [ "ON" ] idiom { "," idiom }

group_clause ::= "GROUP" ( "ALL" | [ "BY" ] idiom_list )

order_clause ::= "ORDER" [ "BY" ] ( order_rand | order_term_list )
order_rand   ::= "RAND" "(" ")" [ nulls_pos ]
order_term_list ::= order_term { "," order_term }
order_term   ::= idiom [ "COLLATE" ] [ "NUMERIC" ] [ "ASC" | "DESC" ] [ nulls_pos ]
nulls_pos    ::= "NULLS" ( "FIRST" | "LAST" )

limit_clause ::= "LIMIT" [ "BY" ] ( integer | param )
start_clause ::= "START" [ "AT" ] ( integer | param )
              | "START" "AFTER" ( param | expr )          (* keyset-курсор *)

fetch_clause ::= "FETCH" idiom_list
omit_clause  ::= "OMIT" idiom_list

timeout_clause ::= "TIMEOUT" ( duration_lit | param )
keep_clause    ::= ( "KEEP" "LEVELPATH" ) [ "FOR" alias_list ]
alias_list     ::= alias { "," alias }

expect_clause  ::= "EXPECT" ( expr | "EXISTS" | "NOT" "EXISTS" )

(* EXPLAIN — только префиксный оператор (§4.6); суффиксной формы нет *)

target ::= record_range | record_id | table_name | param
         | "(" statement ")"          (* подзапрос / (MATCH ...) / (SELECT ...) *)
         | idiom
```

---

## 12. Выражения

### 12.1. Приоритет операторов

От высшего к низшему:

| Уровень | Операторы                                                                | Ассоциативность |
|---------|--------------------------------------------------------------------------|-----------------|
| 1       | postfix: `.` `?.` `[...]` `.{...}` вызов `(...)`                         | лево            |
| 2       | unary: `!` `NOT` `!!` унарный `-` `+`, cast `<T> e`                      | право           |
| 3       | `**`                                                                     | право           |
| 4       | `*` `/` `%`                                                              | лево            |
| 5       | `+` `-`                                                                  | лево            |
| 6       | `<` `>` `<=` `>=`, KNN `<\|…\|>`, geo-ops, `CONTAINS…`, fulltext `@@`    | лево            |
| 7       | `=` `IS` `!=` `NOT IS` `?=` `*=`, `IN`/`NOT IN`/`ALLIN`/`ANYIN`/`NONEIN` | лево            |
| 8       | `&&` `AND`                                                               | лево            |
| 9       | `\|\|` `OR`                                                              | лево            |
| 10      | `??` `?:`                                                                | лево            |
| 11      | тернарный `?:`                                                           | право           |
| 12      | pipeline `\|>`                                                           | лево            |

### 12.2. Грамматика выражений (по уровням)

```ebnf
expr           ::= pipeline_expr
pipeline_expr  ::= ternary_expr { "|>" pipeline_step }
ternary_expr   ::= coalesce_expr [ "?" expr ":" expr ]
coalesce_expr  ::= or_expr { ( "??" | "?:" ) or_expr }
or_expr        ::= and_expr { ( "OR" | "||" ) and_expr }
and_expr       ::= equality_expr { ( "AND" | "&&" ) equality_expr }
equality_expr  ::= comparison_expr { equality_op comparison_expr }
comparison_expr::= additive_expr { comparison_op additive_expr }
additive_expr  ::= multiplicative_expr { ( "+" | "-" ) multiplicative_expr }
multiplicative_expr ::= power_expr { ( "*" | "/" | "%" ) power_expr }
power_expr     ::= unary_expr [ "**" power_expr ]
unary_expr     ::= ( "!" | "NOT" | "!!" | "-" | "+" ) unary_expr
                 | cast_expr
                 | postfix_expr
cast_expr      ::= "<" type ">" unary_expr
postfix_expr   ::= primary_expr { postfix_op }

equality_op    ::= "=" | "IS" | "!=" | "NOT" "IS" | "?=" | "*="
                 | "IN" | "NOT" "IN" | "ALLIN" | "ANYIN" | "NONEIN"
comparison_op  ::= "<" | ">" | "<=" | ">="
                 | knn_op | fulltext_op | geo_op | contains_op
knn_op         ::= "<|" expr [ "," ( ident | "EF" expr ) ] "|>"
fulltext_op    ::= "@@" | "@" integer "@" | "@AND@" | "@OR@"
geo_op         ::= "INSIDE" | "NOTINSIDE" | "ALLINSIDE" | "ANYINSIDE" | "NONEINSIDE"
                 | "OUTSIDE" | "INTERSECTS"
contains_op    ::= "CONTAINS" | "CONTAINSNOT" | "CONTAINSANY" | "CONTAINSALL"
```

### 12.3. Postfix-операции и idioms

```ebnf
postfix_op ::= "." name
             | "?." name                       (* null-пропагирующий доступ *)
             | "." "{" name { "," name } "}"    (* деструктуризация: address.{city, country} *)
             | "[" expr "]"                     (* индекс / ключ *)
             | "[" "WHERE" expr "]"             (* фильтрация массива объектов *)
             | "(" [ arg_list ] ")"             (* вызов *)

idiom      ::= idiom_base { postfix_op }
idiom_base ::= name | param
```

### 12.4. Первичные выражения

```ebnf
primary_expr ::=
      literal
    | param
    | closure
    | fn_call
    | record_range
    | record_id
    | idiom_base
    | if_expr
    | case_expr
    | quantifier_expr
    | query_expr
    | "(" statement ")"

query_expr ::= select_stmt | match_stmt | traverse_stmt
```

### 12.5. Вызовы функций

```ebnf
fn_call   ::= ( fn_name | ident ) [ "<" type ">" ] "(" [ arg_list ] ")"
arg_list  ::= arg { "," arg }
arg       ::= [ ident "=" ] expr                (* именованные аргументы: damping=0.85 *)
```

Дженерик-форма `type::is<edge>(x)`, `type::to<int>(x)` — через `[ "<" type ">" ]`.

### 12.6. Замыкания

```ebnf
closure        ::= "|" [ closure_param_list ] "|" [ "->" type ] closure_body
closure_param_list ::= closure_param { "," closure_param }
closure_param  ::= param [ ":" type ]
closure_body   ::= block | expr
```

Примеры: `|| { ... }`, `|$x| $x.age > 18`, `|$x, $y| { RETURN $x + $y }`, `|x: float, y: float| -> float { math::abs(x - y) }`.

### 12.7. Кванторы по путям

```ebnf
quantifier_expr ::= ( "ALL" | "ANY" | "NONE" ) "(" ident "IN" expr "WHERE" expr ")"
```

Например: `ALL(v IN nodes(path) WHERE v.age > 18)`.

### 12.8. Приведение типов

```ebnf
(* Явное приведение (см. §12.2, cast_expr): *)
<int> 3.14
<regex> "colou{3}r"
<decimal> x
```

### 12.9. Условные выражения

```ebnf
if_expr   ::= "IF" expr "THEN" expr "ELSE" expr "END"
case_expr ::= "CASE" [ expr ] { "WHEN" expr "THEN" expr } [ "ELSE" expr ] "END"
```

Тернарное сокращение `cond ? a : b` — уровень 11 (§12.2).

### 12.10. Alias

```ebnf
alias ::= ident | quoted_ident
```

---

## 13. Паттерны и цели

Сводка ключевых нетерминалов, используемых несколькими операторами.

```ebnf
(* Цель мутаций и источник выборок *)
target        ::= record_range | record_id | table_name | param
                | "(" statement ")" | idiom

(* Направления рёбер в паттернах *)
edge_dir_left  ::= "<-" | "-"
edge_dir_right ::= "->" | "-"

(* Метки *)
label_expr ::= label { "|" label }
label      ::= name
```

Полный синтаксис узлов/рёбер/гипер-/мета-/AtomWalk-паттернов — в §6.

---

## 14. Система типов

```ebnf
type ::= union_type
union_type ::= atomic_type { "|" atomic_type }          (* record<A | B> и т.п. *)
atomic_type ::= primitive_type
              | composite_type
              | domain_type
              | utility_type
              | type_name
              | "(" type ")"

type_name ::= ident

primitive_type ::= "bool" | "int" | "float" | "decimal" | "number"
                 | "string" | "bytes" | "datetime" | "duration"
                 | "null" | "none" | "any"

composite_type ::=
      "array"  "<" type [ "," integer ] ">"
    | "set"    "<" type ">"
    | "tuple"  "<" type_list ">"
    | "object"
    | "option" "<" type ">"
    | "range"  "<" type ">"
    | "record" "<" table_ref { "|" table_ref } ">"
    | "closure" "<" "(" [ type_list ] ")" "->" type ">"
    | "regex" | "geometry" | "error"
type_list ::= type { "," type }
table_ref ::= name

utility_type ::= "computed" "<" type ">" | "formatter" | "matcher" | "cursor"

domain_type ::=
      ( "vertex" | "metavertex" | "edge" | "hyperedge" | "metaedge" | "atom" )
        [ "<" label ">" ]
    | "path" | "subgraph" | "graph"
    | "tree" | "forest" | "dag" | "hypergraph" | "metagraph"
```

### 14.4. Ограничения графа

`graph_constraint` (используется в `DEFINE GRAPH TYPE`, `RETURN GRAPH <...>`, `BUILD GRAPH <...>`) —
см. §9.8.

---

## 15. Зарезервированные слова

Регистронезависимы. Не могут использоваться как идентификаторы без обрамления обратными кавычками.

**Операторы чтения/записи:** `SELECT`, `ONLY`, `VALUE`, `FROM`, `WHERE`, `SPLIT`, `GROUP`, `ORDER`, `LIMIT`,
`START`, `FETCH`, `OMIT`, `TIMEOUT`, `TEMPFILES`, `CREATE`, `UPDATE`, `UPSERT`, `DELETE`, `RELATE`, `CONTENT`,
`SET`, `UNSET`, `MERGE`, `PATCH`, `REPLACE`, `RETURN`, `NONE`, `BEFORE`, `AFTER`, `DIFF`, `WEIGHTED`, `HYPER`,
`TARGETS`, `AS`, `DIRECTED`, `UNDIRECTED`, `BOTH`.

**Паттерн-матчинг / traversal:** `MATCH`, `OPTIONAL`, `DISTINCT`, `ATOM`, `META`, `METAVERTEX`,
`METAEDGE`, `CONTAINS`, `START`, `END`, `CONSTRAINTS`, `MAXDEPTH`, `WALK`, `TRAIL`, `SIMPLE`, `ACYCLIC`,
`SHORTEST`, `METAVERTICES`, `METAEDGES`, `HYPEREDGES`, `CROSS_LEVEL`, `CROSS-LEVEL`, `COST`, `TRAVERSE`,
`KEEP`, `LEVELPATH`.

**Граф-результат:** `GRAPH`, `BUILD`, `NODES`, `EDGES`.

**Управление / транзакции:** `LET`, `IF`, `THEN`, `ELSE`, `END`, `CASE`, `WHEN`, `FOR`, `IN`, `WHILE`, `BREAK`,
`CONTINUE`, `BEGIN`, `COMMIT`, `CANCEL`, `WITH`, `VALIDATION`, `ISOLATION`, `LEVEL`, `SERIALIZABLE`, `SNAPSHOT`,
`READ`, `COMMITTED`, `THROW`, `SLEEP`, `TRY`, `CATCH`, `EXPLAIN`, `ANALYZE`, `FULL`, `FORMAT`, `ASSERT`,
`INVARIANTS`.

**DDL:** `DEFINE`, `REMOVE`, `TABLE`, `FIELD`, `PARAM`, `FUNCTION`, `EVENT`, `INDEX`, `ANALYZER`, `DATABASE`,
`OVERWRITE`, `EXISTS`, `NOT`, `TYPE`, `NORMAL`, `RELATION`, `SCHEMALESS`, `SCHEMAFULL`, `ANY`, `DROP`,
`CHANGEFEED`, `COMMENT`, `COMPUTED`, `DEFAULT`, `READONLY`, `PURE`, `STABLE`, `IMPURE`, `ASYNC`, `RETRY`,
`FIELDS`, `COLUMNS`, `UNIQUE`, `COUNT`, `FULLTEXT`, `SEARCH`, `BM25`, `HIGHLIGHTS`, `HNSW`, `DIMENSION`,
`DIST`, `EFC`, `M`, `CONCURRENTLY`, `DEFER`, `PATH`, `REACHABILITY`, `NEIGHBOURHOOD`, `ALGORITHM`, `DIRECTION`,
`OUT`, `PATTERNS`, `TOKENIZERS`, `FILTERS`, `ROOT`, `ROOTS`, `VALIDATE`, `ON_MUTATION`, `DEFERRED`.

**Индексы/графы (типы):** `ROOTED_TREE`, `FOREST`, `DAG`, `BIPARTITE`, `COMPLETE`, `TREE`.

**Административные / live:** `INFO`, `SYSTEM`, `DB`, `SHOW`, `CHANGES`, `SINCE`, `KILL`, `REBUILD`, `LIVE`, `ON`,
`FOR`.

**Clause-модификаторы:** `BY`, `ALL`, `ASC`, `DESC`, `COLLATE`, `NUMERIC`, `NULLS`, `FIRST`, `LAST`, `AT`,
`NOINDEX`.

**Морфизм и путь:** `REPEATABLE`, `ELEMENTS`, `DIFFERENT`, `EDGES`, `ATOMS`, `WITHIN`.

**Условная запись и импорт:** `EXPECT`, `IMPORT`, `VALIDATION`, `REPORT`.

**Пагинация и подписки:** `AFTER`, `SINCE`, `OVERFLOW`, `BUFFER`, `GAP`, `DISCONNECT`, `BLOCK`.

**Режимы исполнения:** `EXECUTION`, `MODE`, `DETERMINISTIC`, `BEST_EFFORT`, `FAST`, `ONLY`.

**Обслуживание индексов:** `ALTER`, `COMPACT`, `ENABLED`, `DISABLED`, `READ_ONLY`, `GEOMETRY`.

**Литералы:** `true`, `false`, `null`, `none`.

**Операторы-слова:** `AND`, `OR`, `IS`, `IN`, `ALLIN`, `ANYIN`, `NONEIN`, `CONTAINSNOT`, `CONTAINSANY`,
`CONTAINSALL`, `INSIDE`, `NOTINSIDE`, `ALLINSIDE`, `ANYINSIDE`, `NONEINSIDE`, `OUTSIDE`, `INTERSECTS`.

**Кванторы:** `ALL`, `ANY`, `NONE`.

---

## 16. Замечания для парсера

Символы `<`, `|`, `~`, `@` и стрелки перегружены; их однозначная интерпретация требует контекстно-зависимого
разбора (обычно — Pratt-парсер с look-ahead).

1. **`<` — четыре роли:**
   - меньше: `a < b`;
   - каст: `<int> expr` (за `<` следует `type`, затем `>` и операнд);
   - дженерик вызова: `type::is<edge>(x)` (после `ident<`);
   - аннотация графа: `RETURN GRAPH <dag> { … }`, `MATCH … CONSTRAINTS <SIMPLE SHORTEST>`;
   - открытие метавершины: `<(n)>`, метаребра `<[e]>`, атом-узла AtomWalk `<(x:edge)>`.

   Стратегия: решать по позиции (после `RETURN GRAPH` / `CONSTRAINTS` / `<` перед `(`|`[` → структурная роль;
   в позиции унарного операнда → каст; иначе — сравнение).

2. **`|` — три роли:** логическое ИЛИ `a || b` (двойное), альтернатива меток `:A|B`, ограничители замыкания
   `|$x| …` и range-генератора `|table:n|`. `||` всегда токенизируется как один токен OR.

3. **`|>` против `|` `>`:** pipe-оператор `|>` — единый токен, имеет наинизший приоритет.

4. **`<|  … |>`** — KNN-оператор; открывающий `<|` и закрывающий `|>` — единые токены. Не путать с pipe `|>`.

5. **`~`-семейство (AtomWalk):** `~>`, `~~`, `~[`, `]~>`, `]~`, `<~[` токенизируются жадно, только в
   AtomWalk-контексте (после `MATCH ATOM` / `TRAVERSE ATOM`).

6. **Стрелки:** `->`, `<-`, `<->` разбираются как части `edge_pattern`; в `RELATE` они же задают направление.

7. **`@`-операторы:** `@@`, `@AND@`, `@OR@`, `@1@` (индексируемый) — единые токены полнотекстового поиска.

8. **`record_id` vs `label`:** `table:id` (двоеточие без пробела) — литерал записи; `:Label` (после `(`/`[`) —
   метка в паттерне. Различаются по контексту (внутри паттерна vs выражение).

9. **`record_range` vs числовой диапазон / `..`:** `..`/`..=` внутри `id`-части активны только в позиции цели
   (`FROM`, `CREATE`, `UPDATE`, …).

10. **`SELECT`/`MATCH`/`TRAVERSE` как выражения** (`query_expr`) допустимы без скобок в позиции первичного
    выражения (источник pipeline, аргумент `LET`); в неоднозначных местах используйте скобки `( … )`.

11. **Ключевые слова vs идентификаторы:** ключевые слова регистронезависимы; чтобы использовать
    зарезервированное слово как имя — обрамляйте обратными кавычками: `` `graph` ``, `` `order` ``.

12. **Path-constraint списки** внутри `[ … ]` и `CONSTRAINTS < … >` — последовательность модификаторов через
    пробел; порядок незначим, но каждый модификатор — отдельный токен-ключевое слово.
