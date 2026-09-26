# Отчёт этапа 13: реляционный SELECT для атомов и графов

Дата: 2026-09-26.

## Результат

Этап 13 реализовал SQL-подобный `SELECT` поверх graph-first модели, сохранив
pipeline `parse → structural validation → statement analysis → logical IR →
execution` и внутренний синхронный pull `RowCursor`.

Поддержаны:

- class-, `GRAPH`-, `GRAPHS`- и derived sources с aliases;
- base/effective atom properties, `SCHEMAFULL`/`SCHEMALESS` wildcard и graph
  metadata/properties;
- `INNER`, `LEFT` и `CROSS JOIN` с bag semantics;
- derived, scalar, correlated и collection subqueries;
- `WHERE`, `SPLIT`, projection, `OMIT`, `FETCH` и `ONLY`;
- `GROUP`, distinct и утверждённые aggregates;
- external `ORDER`, offset- и keyset-pagination;
- exact retained-spool continuation и явный `READ COMMITTED` fallback;
- единый statement-wide barrier budget, автоматический spill и `TEMPFILES`.

Публичная граница осталась `Value`/`QueryResult`. Storage DTO, `AtomId`, page
identifiers и внутренние адреса через неё не выдаются. `QueryCursor` является
opaque runtime token, а `Value::Cursor` явно отвергается storage codecs.

## Durable graph properties

`GraphSchema` теперь хранит snapshot-visible `PropertyMap`. `DEFINE GRAPH ...
CONTENT <object>` создаёт свойства, `UPDATE GRAPH ... SET` изменяет их, а
`UNSET` удаляет. Изменения проходят через MVCC, FCW/SSI и атомарную RAM/local
publication, не перестраивая physical graph generation или
`GraphSuperblock`.

Database catalog переведён на `FGDC v8`. `GraphCatalogEntry` кодирует
канонически отсортированные `PropertyKeyId` и canonical `StorageValue`.
Декодер отвергает v7, duplicate/unknown keys, runtime-only values,
неканонические bytes и неверные lengths. Reopen/recovery и Mode 3 сохраняют
catalog-owned graph properties.

## Row, source и execution model

Source binding хранит public identity, metadata и `PropertyMap`, поэтому
разрешение полей не зависит от прежнего фиксированного `Vec<Value>`. Для
schemaless wildcard свойства обнаруживаются в стабильном порядке
`PropertyKeyId`; итоговый public object использует существующий канонический
порядок object keys.

Execution разделён по ответственности:

- source/shape pipeline и expression evaluation;
- join algorithms;
- grouping и aggregate states;
- ordering/pagination и cursor lifecycle;
- transient spill codec, runs и cleanup.

Equality conjunction использует правую hash build-side. Остальные predicates
и `CROSS JOIN` используют block nested-loop. При исчерпании общего budget
join/group/order/distinct переходят на bounded disk runs; новый общий sorter
trait и новая dependency не вводились.

## Spill, память и cursors

Один `BarrierAccount` обслуживает все barriers statement. Значение по умолчанию
— 256 МиБ, пользователь может только уменьшить его. Transient run v1 использует
buffered `std::fs`, canonical value codec, record checksum и exact-length
validation. Runs не являются durable данными, поэтому им требуется `flush`, но
не `sync_data`; crash recovery удаляет orphan query directories.

Persistent `GqlSession` хранит не более 64 cursor spools. Cursor проверяет
session/database identity, normalized query hash, ORDER fingerprint и snapshot
age. Закрытие cursor, исчерпание результата, timeout и Drop session освобождают
spool. Повторное чтение одной continuation возвращает ту же страницу, а exact
continuation не пересчитывает исходный offset.

## Correctness evidence

| Проверка                                                                                         |                             Результат |
|--------------------------------------------------------------------------------------------------|--------------------------------------:|
| `cargo test --locked --lib --no-default-features -- --test-threads=1`                            |       405 passed, 0 failed, 5 ignored |
| `cargo test --locked --lib --no-default-features --features native-async-io -- --test-threads=1` |       407 passed, 0 failed, 6 ignored |
| focused SELECT module                                                                            | 17 passed, 0 failed, 2 ignored probes |
| focused spill module                                                                             |                    4 passed, 0 failed |
| focused numeric canonical-order tests                                                            |                    4 passed, 0 failed |
| status macro crate                                                                               |                    5 passed, 0 failed |
| `cargo check --locked --lib --no-default-features`                                               |                                passed |
| `cargo check --locked --lib --no-default-features --features native-async-io`                    |                                passed |

Матрица покрывает source/alias resolution, ambiguous fields, base/effective
properties, `FGDC v8` corruption, graph-property rollback/reopen, join bag
semantics, subquery cardinality, shape operators, aggregate legality и exact
values, stable ordering, cursor identity/expiry/reuse, spill equivalence,
timeout/cancellation cleanup, SSI predicates и RAM/local equality.

## Performance и resource gate

### Простой SELECT

Paired profiling probe прежнего stage-12 executable и текущего checkout:

| Измерение | Текущий SELECT | Stage-12 baseline | Отношение |
|---|---:|---:|---:|
| 9 paired runs, median | 2.2822 ms | 2.4379 ms | 0.9504 |
| повторная серия 5 paired runs, ratio median | — | — | ~0.903 |

Регрессии более 5% нет. Barrier-free запросы не создают temp files.

### Bounded/spilled workloads

| Workload               |    Budget / режим | Query time |  RSS delta |  Peak temp |
|------------------------|------------------:|-----------:|-----------:|-----------:|
| simple 1K              |       без barrier |   0.688 ms |  ~0.52 MiB |          0 |
| simple 10K             |       без barrier |   5.944 ms |  ~2.07 MiB |          0 |
| simple 100K            |       без barrier |  46.681 ms |  ~0.93 MiB |          0 |
| simple 1M, `LIMIT 100` |       без barrier |  45.677 ms |  ~0.20 MiB |          0 |
| ORDER 100K wide rows   |        1 MiB auto |    1.832 s |  ~0.87 MiB |  45.16 MiB |
| ORDER 100K wide rows   | 1 MiB `TEMPFILES` |    1.733 s |  ~1.01 MiB |  46.72 MiB |
| ORDER 1M, `LIMIT 100`  |             1 MiB |   17.980 s |     ~0 MiB | 514.54 MiB |
| GROUP 100K             |             1 MiB |    2.838 s |  ~1.01 MiB |  50.41 MiB |
| JOIN 100K, `LIMIT 100` |             1 MiB |    7.331 s |  ~1.02 MiB |  88.45 MiB |
| JOIN 100K, `LIMIT 100` |           256 MiB |    1.432 s | ~304.3 MiB |   64.3 MiB |

Малый 1 МиБ budget намеренно является stress-конфигурацией: он подтверждает
bounded RSS и переход на disk partitioning. В default-конфигурации process RSS
включает до 256 МиБ учитываемых source/build rows, allocator slack и bounded
codec/run buffers; эти величины не являются дополнительными независимыми
256-МиБ budgets. Большой публичный result закономерно оплачивается отдельно:
JOIN 100K с `LIMIT 100000` дал около 280.9 МиБ delta из-за материализации
выходного `Value`, тогда как `LIMIT 100` сохраняет малый result.

Automatic spill и `TEMPFILES` дают одинаковые values/order. Runs полностью
удаляются после success, error, cancellation и закрытия cursor.

## Профилирование и устранённые bottlenecks

Профилируемый тест собран с `--profile profiling`. Использована точная пара:

```text
target/profiling/deps/ferros_grynn-2170861548b6ea93.exe
target/profiling/deps/ferros_grynn-2170861548b6ea93.pdb
```

VTune разрешил Rust symbols и source lines. Исходный ORDER 100K тратил около
35.7% sampled CPU на `BigInt`-нормализацию integer order keys (`gcd`, shift и
division), а `sync_data` transient runs занимал около 7.5%. Исправления:

- canonical integer representation больше не вычисляет тривиальный `gcd(x, 1)`;
- сравнение двух integer выполняется напрямую как `i64`;
- transient query run использует `flush`, не durable fsync.

Под VTune время ORDER 100K уменьшилось с 4.884 s до 1.650 s. В финальном
профиле нет доминирующего CPU hotspot: крупнейшие участки — buffered write
(8.9%), runtime→storage conversion (5.8%), `memcmp` (5.1%), buffered read
(4.3%) и decode (3.7%).

JOIN stress profile показывает стоимость ожидаемого внешнего partitioning:
run write 7.4%, file creation 7.2%, unlink 4.1% и encode 3.9%. Патологического
hash-build hotspot не обнаружено. Join reorder, limit pushdown и index-ordered
execution оставлены этапу 14, как требует план.

Артефакты профилирования:

- `target/stage13-vtune-order-100k-final-20260926`;
- `target/stage13-vtune-order-100k-final-functions.csv`;
- `target/stage13-vtune-order-100k-final-source-stack.csv`;
- `target/stage13-vtune-join-100k-final-20260926`.

## Tooling gate

Stage-13 files отформатированы nightly rustfmt. Глобальный
`cargo +nightly fmt --all -- --check` остаётся заблокирован прежним formatting
drift в восьми несвязанных storage/memory files; они не переписывались в этом
этапе. Полный строгий Clippy также остаётся заблокирован существующим
workspace-wide debt `Q-14` (`missing_docs`, `self_named_module_files` и другие
тысячи diagnostics). Focused compile/tests и обе feature-конфигурации успешны;
новых Stage-13 compile errors не осталось. `graphify update .` завершён:
knowledge graph содержит 51 140 nodes и 132 262 edges.

## Отложенная поверхность

- CBO, hints, join reordering, limit pushdown и index-ordered cursor seek — этап 14;
- topology joins и полный `MATCH` — этап 15;
- traversal map/reduce — этап 17;
- `ORDER BY RAND()` — этап 14; collation и numeric-string ordering — этап 21;
- `RIGHT/FULL/SEMI/ANTI JOIN`, `UNION` и `LATERAL` — только после реального
  пользовательского запроса.

Этап не добавлял mmap/query async runtime, общий sorter trait, dynamic function
registry или новые зависимости.
