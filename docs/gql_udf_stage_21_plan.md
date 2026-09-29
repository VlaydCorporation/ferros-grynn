# Этап 21. Durable PARAM, именованные UDF и оставшийся язык

**Статус: U0–U12 завершены, 2026-09-29.** Фактические correctness- и
performance-проверки приведены в
[`reports/gql_udf_stage_21_report.md`](reports/gql_udf_stage_21_report.md).

## 1. Durable schema

- `PARAM` и overloads `fn::name` принадлежат единому `SchemaCatalog` и общему
  `next_schema_id`; отдельного function catalog нет.
- `FunctionId` является typed `SchemaId`. `FGDC v10` хранит source UTF-8 body,
  resolved types, effect, `COST`, optional `MAXDEPTH` и comment; typed IR/bytecode
  на диск не записываются.
- Parameter resolution: block → transaction → session/program → execute
  parameter → durable database parameter. Durable values обязаны иметь
  canonical storage encoding.
- Built-in namespaces не перекрываются; пользовательские functions существуют
  только в `fn::`.

## 2. Analysis и execution

- Overload выбирается по `(max_argument_rank, sum_argument_ranks)` после
  сопоставления positional/named arguments с объявленными parameters. Ранги:
  exact, lossless numeric, `number`, precision-losing numeric. Равенство лучших
  рангов является ambiguity; `FunctionId` не используется как tie-break.
- Effect вычисляется транзитивно: `PURE`, `STABLE`, `IMPURE`. Schema/admin,
  transaction/import и nested `EXPLAIN ANALYZE` в UDF запрещены; `COST` требует
  PURE non-negative finite number.
- IMPURE call использует transaction savepoint. Recursion требует explicit
  `MAXDEPTH 1..=64`; mutual cycle требует bounds и return type у каждого
  участника.
- Concrete typed cache ключуется `(FunctionId, SchemaVersion, schema_epoch)` и
  лениво восстанавливается после reopen. PlanHash включает identity/version.
- `tv::algo(fn::name, ...)` передаёт current первым implicit argument и
  разворачивает collection result без отдельного algorithm registry.

## 3. Runtime language

Реализованы cooperative `SLEEP`; runtime-only shared-state `Matcher` с
Unicode-scalar offsets; mutable lexical helpers array/set; object construction;
UUIDv7, ULID и `rand::id`; ICU4X `COLLATE`/`NUMERIC`. Matcher, closure, cursor и
ErrorValue не сериализуются и не spill-ятся. ICU configuration входит в ORDER
fingerprint, а равные keys разрешаются canonical UTF-8/stable-row key.

## 4. Порядок реализации

| Шаг | Зависит от | Результат |
|---|---|---|
| U0 | J14 | Нормативная матрица signatures/effects/recursion |
| U1 | U0, J1 | `FunctionId`, schema model и FGDC activation |
| U2 | U1 | PARAM/FUNCTION DDL, INFO и signature qualifiers |
| U3 | U2 | Durable parameters, precedence, MVCC/reopen |
| U4 | U2–U3 | Overloads, body typing, effects и cycles |
| U5 | U4 | Cache, scopes, savepoints, recursion, security |
| U6 | U5 | COST, `tv::algo`, PlanHash/EXPLAIN |
| U7 | U3 | Sleep, matcher, collections, object и IDs |
| U8 | U0 | ICU4X collation, spill и cursor fingerprints |
| U9–U12 | U6–U8 | Integration, correctness/performance и closure |

## 5. Gate

Проверяются parameter precedence/reopen; overload cross-product, named
arguments и ambiguity; transitive effects; recursion; savepoint restoration;
cache invalidation; `tv::algo`; Unicode/zero-length matcher; lexical mutation;
randomness/determinism; ICU accents/case/digits/nulls/cursors; spill equality;
RAM/local и обе I/O configurations. Новые plugin traits, bytecode, generic UDF,
varargs и definer rights не вводятся.

