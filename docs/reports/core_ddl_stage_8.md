# Отчёт по этапу 8: Core DDL vertical slices

Дата: 2026-09-09  
Статус: завершён

## 1. Граница этапа

Реализован только core DDL-контур этапа 8 `docs/full_plan.md`:

- `DEFINE/REMOVE GRAPH`;
- `DEFINE/REMOVE/ALTER TABLE`;
- `DEFINE/REMOVE/ALTER FIELD`;
- `DEFINE/REMOVE/ALTER INDEX` для Standard, Unique и Count;
- `INFO FOR DB`, `INFO FOR TABLE` и `INFO FOR INDEX`;
- транзакционная публикация schema catalog и восстановление local backend.

`ALTER GRAPH`, DML, advanced index kinds, фоновые build jobs и полный admin/INFO surface не
реализовывались. Они остаются в назначенных последующих этапах.

## 2. Нормативные решения

Поведение сверено прежде всего с `gql_spec.md` §§8.6, 9.1–9.2, 9.6, 9.8–9.10 и 17.7,
`disk_storage_spec.md` §§7.3 и 20, а grammar приведена к фактически поддержанному core-профилю.

- Таблица, объявленная без graph, принадлежит системному `GraphId=0`; фиктивный runtime graph для
  неё не создаётся.
- Именованный graph получает стабильный пользовательский `RecordId` вида `graph:<name>`.
- Пустой runtime graph создаётся только для поддержанных семейств Graph, DAG, Hypergraph и
  Metagraph. Ограничения SIMPLE/BIPARTITE/COMPLETE и семейства, которым нужен обязательный root,
  возвращают явную unsupported-feature ошибку.
- `ALTER GRAPH` отвергается явно до утверждения его семантики в `gql_spec.md`.
- Advanced index kinds FULLTEXT, HNSW, DISKANN, GEOMETRY и graph indexes не сводятся к обычному
  scalar index и возвращают явный unsupported status.
- Пользовательский secondary index системной таблицы сохраняет definition, но до появления DML и
  физической сборки имеет состояние `stale` и не получает фиктивный file binding.

## 3. Вертикальный срез

### 3.1. Parser, typing и IR

Добавлен отдельный schema-statement модуль parser. Он поддерживает `IF NOT EXISTS`, `OVERWRITE`,
`IF EXISTS`, table kind, `SCHEMALESS|SCHEMAFULL`, field type, `FIELDS|COLUMNS`, `UNIQUE`, `COUNT`
и `ALTER INDEX ... PREPARE REMOVE`. Неизвестные и отложенные clauses завершаются типизированной
ошибкой, а не игнорируются.

Semantic analyzer переводит syntax в отдельный `LogicalSchemaStatement`; физические storage DTO,
`DataFileId` и page identity в query IR не попадают. Определения графа проверяются против реально
конструируемых runtime families.

### 3.2. Transactional catalog mutations

DDL использует существующий write set `Transaction` и публикуется тем же MVCC commit, что schema и
данные. `Normal`, `IfNotExists` и `Overwrite` имеют раздельную семантику; overwrite сохраняет
стабильные `SchemaId`/`IndexId` и увеличивает version определения.

Удаление field запрещено, пока на него ссылается index. Удаление именованного graph каскадно
удаляет принадлежащие ему таблицы. Повторное определение таблицы в системном graph внутри той же
транзакции учитывается по эффективному staged catalog и не попадает под старое владение.

Автоматический `Datastore::execute` открывает read-write транзакцию только для программ со schema
mutations; read-only программы сохранили прежний путь. Программа с ошибочным DDL не публикует
частичный catalog.

### 3.3. Durable catalog и recovery

Database catalog повышен до `FGDC v5`. Формат сохраняет:

- `TableSchemaMode`;
- `TableIndexState::{Ready, Stale, PreparedForRemoval}`;
- optional secondary `DataFileId/root`;
- системный graph как `GraphId=0` без runtime graph generation.

Decoder принимает только v5 и проверяет допустимость отсутствующего physical binding. При reopen
missing, unopenable, truncated или checksum-corrupt secondary сохраняется в schema catalog как
`stale`, исключается из physical registry и не выбирается query path. Повреждение primary/base
остаётся фатальным.

### 3.4. INFO

`INFO FOR DB` выдаёт определения graphs/tables, `INFO FOR TABLE` — definition, fields и indexes,
`INFO FOR INDEX` — definition, `system`, `state` и `building.status`. Идентификаторы в
восстанавливаемом DDL безопасно quoted; состояние secondary остаётся наблюдаемым после reopen.

Полная причина scan fallback относится к `EXPLAIN` этапа 14. Core INFO уже устраняет silent-state
проблему `Q-19`, поэтому она закрыта.

## 4. Correctness evidence

Проверены:

- parser round-trip core forms и явный отказ advanced clauses/kinds;
- atomic auto-transaction rollback при ошибке одного DDL statement;
- `IF NOT EXISTS`, `OVERWRITE`, `IF EXISTS` и сохранение schema/index identity;
- graph/table/field/index define, alter, prepare-remove и remove;
- удаление graph с cascade и staged переносом таблицы в системный graph;
- системные таблицы без фиктивного runtime graph;
- одинаковое INFO-представление до и после local reopen;
- persisted `stale` и `prepared_for_removal`;
- scan fallback после corruption/truncation secondary и фатальный primary corruption;
- отказ от промежуточного `FGDC v4`.

Повторный read-only review обнаружил и исправил две ошибки границы: cascade первоначально смотрел
на старого владельца таблицы вместо effective staged schema, а parser допускал ненормативную форму
`TYPE SCHEMAFULL`. После исправлений блокирующих findings в срезе этапа 8 нет.

## 5. Проверки

Пройдены:

- targeted `rustfmt +nightly --check` всех Rust-файлов этапа;
- `cargo check --lib --no-default-features`;
- `cargo check --lib --no-default-features --features native-async-io`;
- `cargo test --lib --no-default-features datastore::tests -- --test-threads=1` —
  47 passed, 1 ignored;
- `cargo test --lib --no-default-features -- --test-threads=1` — 336 passed, 8 ignored;
- `cargo test --lib --no-default-features --features native-async-io -- --test-threads=1` —
  338 passed, 9 ignored;
- scoped `git diff --check`;
- `graphify update .` — завершён, новые schema/IR/index-state symbols присутствуют в graph.

Общий `cargo +nightly fmt --all -- --check` остаётся `FAIL (pre-existing)`: единственный diff
находится в не относящемся к этапу пользовательском `src/graph/live.rs`. Полный Clippy остаётся
`FAIL` по зарегистрированному `Q-14` (2274 накопленных errors); локально обнаруженные простые
Stage-8 lints исправлены, глобальный долг не выдан за успешный gate.

## 6. Отложенные границы

- `Q-07`, этап 11: statement snapshots, staged DDL→DML analysis и утверждённая семантика
  abort/savepoint для ошибки statement внутри явного `Transaction::execute`.
- Этап 9: DML и физическое наполнение таблиц системного graph.
- Этап 14: конкретная причина index fallback в `EXPLAIN`.
- Этап 20: полный INFO/admin surface, build jobs, `REBUILD INDEX`, `CONCURRENTLY` и `DEFER`.
- `ALTER GRAPH` — только после отдельного нормативного решения.

Новые зависимости не добавлены; использованы существующие parser, MVCC catalog, typed status и
local recovery contracts.
