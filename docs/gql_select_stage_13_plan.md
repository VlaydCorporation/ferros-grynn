# Этап 13. Реляционный SELECT для атомов и графов

## Статус и результат

Документ задаёт dependency-ordered реализацию этапа 13 из [`full_plan.md`](full_plan.md).
**Этап завершён 2026-09-26:** шаги S0–S12 и обязательные RAM/local,
spill/cursor, resource и profiling gates пройдены. Evidence и измерения:
[`gql_select_stage_13_report.md`](reports/gql_select_stage_13_report.md).

GrynnQL получает SQL-подобный `SELECT` поверх graph-first модели:

- class-source перечисляет глобальные атомы данного class;
- `GRAPH`/`GRAPHS` перечисляют именованные графы;
- объявленные `SCHEMAFULL`-свойства атома доступны как знакомые поля (`name`, `p.name`), а
  универсальная форма для любых atom properties — `properties.<name>`;
- пользовательские свойства графа доступны только через `properties`;
- топология остаётся ответственностью `MATCH`/`TRAVERSE`, а прямые ссылки разворачивает `FETCH`;
- `INNER`, `LEFT` и `CROSS JOIN` обслуживают только нетопологические отношения;
- выполнение сохраняет sync pull `RowCursor`, а barrier-операторы используют один общий bounded
  budget и автоматический spill.

Публичный результат остаётся `Value`/`QueryResult`; storage DTO, page identifiers и внутренние
`AtomId` через эту границу не проходят.

## 1. Нормативный профиль SELECT

### 1.1. Источники и aliases

Поддерживаются class/record sources, `GRAPH @name`, `GRAPHS`, явный список `GRAPHS`, а также
`FROM (SELECT ...) AS alias`. Явный список графов сохраняет порядок запроса, `GRAPHS` без списка
использует порядок `GraphId`. Derived source всегда требует alias. Повторное использование одного
source и self-join требуют разных aliases. Неквалифицированное поле допустимо только при одном
однозначном владельце.

`GRAPHS @a, @b` и `GRAPHS [@a, @b] AS g` дают по одной строке на граф. Старые формы `,` как
union sources и `&` как cross join удаляются; cross product выражается только `CROSS JOIN`.
`UNION`, `RIGHT`, `FULL`, `SEMI`, `ANTI JOIN` и `LATERAL` в этап не входят.

### 1.2. Atom и graph rows

Atom-source содержит зарезервированные поля `id`, `class`, `kind`, `label` в graph scope и
`properties`. Для объявленного поля `SCHEMAFULL` формы `name` и `alias.name` являются знакомым
сокращением `properties.name`; явная форма остаётся универсальной. Динамическое свойство
`SCHEMALESS`, которого нет в catalog schema, требует `properties.<name>`, потому что статический
анализатор не может заранее добавить его в row schema. `SELECT *` для `SCHEMAFULL` выдаёт `id` и
объявленные свойства в catalog order; для `SCHEMALESS` — `id` и реально присутствующие свойства в
стабильном порядке `PropertyKeyId`. Graph scope использует effective property view.

Graph-source содержит `id: GraphRef`, `name`, `kind`, `storage_mode`, `revision`, `node_count`,
`edge_count` и единый object `properties`. Пользовательские graph properties не разворачиваются в
корень строки.

### 1.3. Durable graph properties

`DEFINE GRAPH ... CONTENT <object>` создаёт свойства, `UPDATE GRAPH ... SET
properties.<path> = <expr>` обновляет их, а `UNSET properties.<path>` удаляет. `null` хранится;
`none` в `SET` запрещён. Возврат использует существующие `NONE|BEFORE|AFTER|DIFF`; default —
`NONE`. `UPDATE GRAPH` не разрешён в Atomic Import этапа 13.

Graph properties принадлежат schema catalog, участвуют в MVCC/FCW/SSI и local atomic
publication, но не меняют физическое поколение графа и `GraphSuperblock`. `FGDC v8` добавляет к
`GraphCatalogEntry` канонический раздел, отсортированный по `PropertyKeyId`:

```text
property_count: u32
repeat property_count:
    property_key_id: u32
    value_len: u32
    value_bytes: canonical StorageValue
```

Используются общие `PropertyDictionary` и canonical value codec. Duplicate/unknown key, runtime
value, invalid length, non-canonical value и `none` являются corruption. Читается только v8;
v7 не мигрируется. Mode 3 переносит свойства в `GraphMetadata`.

### 1.4. JOIN и подзапросы

`JOIN` означает `INNER JOIN`. `INNER` и `LEFT` требуют `ON`; `CROSS` запрещает `ON`. Predicate
может быть любым boolean expression; falsey, `null` и `none` не дают match. LEFT unmatched right
binding равен `none`. Bag semantics и deterministic baseline: left-major, затем source order
справа.

Conjunction равенств использует hash join с правой build-side; остальные predicates и `CROSS`
используют block nested-loop. Превышение общего budget переводит build/spool в partitioned spill.

Подзапрос допустим как derived source, scalar expression и одно-колоночный operand collection
operators. Scalar cardinality: 0 строк — `none`, 1 — scalar/object, больше 1 — typed
`SubqueryCardinalityViolation`. Correlated scalar subquery видит outer bindings; derived source не
коррелируется. Mutation в подзапросе запрещена. Snapshot, deadline, cancellation и budget общие с
внешним statement.

### 1.5. Shape pipeline

Нормативный порядок:

```text
source/join -> WHERE -> SPLIT -> GROUP/aggregates -> projection -> OMIT -> FETCH
            -> ORDER -> START/keyset -> LIMIT -> ONLY
```

`SPLIT` разворачивает array в исходном, set — в каноническом порядке; `null`, `none` и пустая
collection дают 0 строк. Несколько paths образуют left-to-right Cartesian product. Scalar — typed
error. Split ordinal входит в скрытый stable key.

`OMIT` удаляет paths только из projected value; отсутствующий path — no-op. `FETCH` разворачивает
`RecordId`, `GraphRef`, arrays/sets и последовательный nested path в текущем snapshot. Глубина
ограничена control-depth; циклическое повторение без следующего явно указанного path отсутствует.

`ONLY`: 0 строк — `none`, 1 — scalar/object, больше 1 — typed cardinality error. Обычный SELECT
возвращает `array<object>`, `SELECT VALUE` — `array<Value>`.

### 1.6. GROUP и aggregates

Aggregate без `GROUP` и `GROUP ALL` образуют одну группу. Любой projected non-aggregate обязан
входить в group key. `null` и `none` — разные keys; nested aggregates запрещены. HAVING заменяется
внешним SELECT.

Исполняются `count()`, `count(expr)`, `count(DISTINCT expr)`, `sum`, `avg`, `min`, `max`,
`array::group`, `math::variance_pop`, `math::stddev_pop`, `math::variance_samp` и
`math::stddev_samp`. Кроме `count()`, aggregates пропускают `null`/`none`. Empty: count и sum равны
0, остальные — `none`. `array::group` сохраняет row order, пропускает `none`, сохраняет `null` и
снимает один collection level. Variance использует Welford/Chan и возвращает float; population
делит на N, sample на N-1 и при N < 2 возвращает `none`.

### 1.7. ORDER и pagination

Поддерживаются expression keys, `ASC|DESC`, `NULLS FIRST|LAST`, `LIMIT`, `START AT` и
`START AFTER <cursor> [READ COMMITTED]`. Default — `ASC NULLS LAST`. Projection aliases доступны
в `ORDER BY`; ties дополняются `StableRowKey`, построенным из stable public provenance, group key и
split ordinal.

Keyset требует `ORDER BY`. Exact continuation читает immutable spool исходного snapshot.
Просроченный snapshot возвращает `SnapshotExpired`; явный `READ COMMITTED` повторяет запрос на
новом root после сохранённых sort/stable keys. `ORDER RAND()` распознаётся, но до этапа 14
получает typed unsupported; `COLLATE` и `NUMERIC` принадлежат этапу 21.

## 2. Public API и bounded resources

`QueryResult` получает `continuation: Option<QueryCursor>`, где `QueryCursor` — opaque runtime
token. `Value::Cursor` является runtime-only и отвергается storage codecs.

`QueryExecutionOptions` получает уменьшаемый `barrier_memory_bytes` (default 256 MiB) и
`temporary_directory`. Один statement разделяет один budget между join/group/order/distinct.
Только persistent `GqlSession` создаёт usable continuation и хранит максимум 64 spool. Cursor
закрывается явно, после последней страницы, по timeout либо при Drop session; он проверяет
session/database identity, normalized query hash, ORDER fingerprint и `max_snapshot_age`.

Spill использует `std::fs`, buffered I/O, существующий canonical value codec/checksum и transient
query-run v1. Local runs находятся в `tmp/query/<uuid>`, memory backend — в отдельном системном
temp directory либо пользовательском `temporary_directory`. Fan-in равен 64. Неспособное к
кодированию runtime value возвращает `QueryValueNotSpillable`; RAII и startup cleanup удаляют
orphan runs.

## 3. Dependency-ordered реализация

| Шаг                               | Зависит от | Критерий завершения                                                                   |
|-----------------------------------|------------|---------------------------------------------------------------------------------------|
| ✅ S0. Нормативная синхронизация  | этап 12    | Этот план подключён к `full_plan`; spec/grammar/tech/disk согласованы, baseline снят  |
| ✅ S1. Durable graph properties   | S0         | `FGDC v8`, DEFINE CONTENT, UPDATE SET/UNSET, RAM/local/reopen                         |
| ✅ S2. Row/source model           | S1         | Source binding хранит metadata/PropertyMap, schemaless wildcard и stable provenance   |
| ✅ S3. SELECT syntax/AST          | S0         | GRAPH/GRAPHS/subquery, aliases, JOIN и все clauses разобраны без семантики в parser   |
| ✅ S4. Typed analysis и IR        | S2, S3     | Scope/field resolution, aggregates, subquery cardinality, logical nodes и fingerprint |
| ✅ S5. Budget и spill             | S2, S4     | Shared accounting, run codec/merge, cleanup и TEMPFILES                               |
| ✅ S6. Streaming shape slice      | S4         | Sources, projection, WHERE, SPLIT, OMIT, FETCH и ONLY                                 |
| ✅ S7. JOIN и subqueries          | S5, S6     | INNER/LEFT/CROSS и три контекста subquery                                             |
| ✅ S8. GROUP и aggregates         | S5–S7      | Group keys, distinct и все утверждённые aggregates                                    |
| ✅ S9. ORDER и cursors            | S5–S8      | External sort, stable keys, offset/exact/RC continuation                              |
| ✅ S10. Correctness gate          | S9         | RAM/local/reopen/MVCC/spill/cursor matrix                                             |
| ✅ S11. Resource/performance gate | S10        | RSS/temp/latency и regression gates с profiling evidence при bottleneck               |
| ✅ S12. Закрытие                  | S11        | Отчёт, отметка `full_plan`, deferred owners и `graphify update .`                     |

Execution-код делится по source, shape, join, aggregate, sort/pagination и spill; единый
многотысячестрочный `select.rs`/`pipeline.rs` не создаётся.

## 4. Gates

Correctness покрывает source/alias resolution, base/effective properties, graph properties и
FGDC v8 corruption, join bag semantics, три вида subquery, shape operators, aggregates, ordering,
cursor identity/expiry/reuse, spill equivalence/cleanup, SSI predicates, RAM/local/reopen и Mode 3.

Performance gate требует: простой SELECT не хуже baseline более чем на 5%; barrier-free запросы
не создают temp files; query-owned barrier memory не превышает budget плюс bounded buffers;
1K/10K/100K/1M rows завершаются без O(rows) RSS; retained cursor не пересчитывает offset. Отдельно
измеряются run generation, merge, codec/checksum, temp I/O и cleanup. Профилирование использует
`--profile profiling` и exact EXE/PDB pair.

## 5. Отложено

CBO, hints, join reorder и index-ordered seek принадлежат этапу 14; topology joins — этапу 15;
traversal map/reduce — этапу 17; `RIGHT/FULL/SEMI/ANTI`, `UNION` и `LATERAL` не добавляются без
реального запроса. `ORDER BY RAND()` принадлежит этапу 14; collation и numeric-string ordering —
этапу 21.
Новые dependencies, async runtime, public streaming API, dynamic function registry и общий sorter
trait не вводятся.
