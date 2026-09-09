# Отчёт по этапу 9: DML vertical slices

Дата: 2026-09-09  
Статус: выполняется; завершение заблокировано нормативными решениями `Q-23` и `Q-24`

## 1. Граница текущего среза

Реализован доступный без изменения неоднозначных спецификаций core DML-контур:

- `CREATE`, `UPDATE`, `UPSERT` и `DELETE` для graph-backed таблиц;
- `CONTENT`, `MERGE`, `PATCH`, `REPLACE`, `SET` и `UNSET` в допустимых для statement сочетаниях;
- `WHERE`, pre-image `EXPECT`, `EXPECT EXISTS` и `EXPECT NOT EXISTS`;
- `RETURN NONE`, `BEFORE`, `AFTER`, `DIFF`, object projection и `RETURN VALUE`;
- ULID для неявных ID и bounded `|table:n|` generator;
- transactional table membership, statistics и primary/Standard/Unique/Count maintenance;
- повторная проверка graph-family invariants при commit на каждом isolation level.

`RELATE` и DML таблиц системного `GraphId=0` не реализованы фиктивной семантикой. Они описаны в
разделе 6 и остаются обязательными до завершения этапа.

## 2. Parser, typing и execution

Новая конструкция проходит `src/syn` → semantic analysis → typed logical IR → transaction
adapter. `UPSERT` без exact record ID и без `WHERE` отклоняется синтаксически. Analyzer типизирует
выражения отдельно для каждой таблицы, проверяет schemafull fields и не пропускает изменение
неявного read-only поля `id`.

Перед первой мутацией executor:

1. разрешает все цели и их pre-image;
2. отвергает повторное попадание одного `RecordId` в statement;
3. проверяет `EXPECT` для каждой цели;
4. только после успешного общего preflight применяет изменения по порядку целей.

Тем самым невыполненный `EXPECT` не оставляет частично staged statement. `WHERE` остаётся фильтром:
неподходящая строка молча не изменяется. Автотранзакция `Datastore::execute` не публикует программу,
если statement вернул ошибку.

## 3. Durable point-DML и `Q-05`

Primary и ready scalar indexes обновляются в том же page-WAL commit, что graph pages, membership,
statistics и catalog binding. Для каждого затронутого B+tree сначала выполняется leaf-only update:
hash/full scan не нужен, если сохраняются internal separator, число листьев и posting layout.
Split, изменение первого ключа non-root leaf или posting-chain переводит операцию в существующий
bulk fallback; Unique проверяется до публикации.

Profiling build выполнялся командой:

```powershell
cargo test --locked --profile profiling --lib --no-default-features `
  primary_incremental_vs_bulk_diagnostic -- --ignored --nocapture --test-threads=1
```

Измерение: 50 000 entries, 7 paired runs с ротацией порядка.

| Индекс   | Fill | Batch 1 | Batch 32 | Batch 256 | Вывод                                         |
|----------|-----:|--------:|---------:|----------:|-----------------------------------------------|
| Primary  | 1.00 |   1.003 |    1.003 |     1.052 | Плотная mutable раскладка сразу уходит в bulk |
| Primary  | 0.90 |   0.013 |    0.198 |     0.454 | Выбран fill 0.90                              |
| Standard | 1.00 |   0.009 |    0.134 |     1.290 | Dense batch 256 выгоднее rebuild              |
| Standard | 0.80 |   0.005 |    0.110 |     0.127 | Выбран fill 0.80                              |

Числа — median `incremental_time / bulk_time`; меньше единицы означает выигрыш incremental path.
Immutable external-sort generation по-прежнему использует fill 1.00. Mutable primary и Unique
используют 0.90, Standard — 0.80. Пользовательский knob не добавлен: изменение констант требует
нового paired benchmark и повторной проверки recovery/unique semantics.

## 4. Correctness evidence текущего среза

Проверяются:

- все поддержанные data/return формы, filters и generated IDs;
- общий preflight нескольких целей и отсутствие частичного staging;
- schemafull unknown/type mismatch до mutation;
- local create/update/delete, повторное использование освобождённого unique key и Standard/Unique
  lookup после reopen;
- same-commit primary/scalar bindings и unique-conflict rollback;
- RecordId reuse после удаления;
- family-invariant revalidation для ReadCommitted, Snapshot и Serializable;
- checksummed Label-index generation перед Direct I/O.

В local reopen gate была локализована MSVC-зависимая порча уже заполненной over-aligned 16 KiB
Label-index страницы при перемещении со стека в `Vec`. Builder теперь кодирует страницу сразу в
окончательном `Vec`-слоте. `local_graph_table_and_edge_survive_reopen` прошёл 100 отдельных
последовательных запусков; общий page codec не менялся (`Q-25`).

## 5. Проверки

Доступный срез прошёл следующие gates на текущем checkout:

- `cargo check --lib --no-default-features`;
- `cargo check --lib --no-default-features --features native-async-io`;
- `cargo test --lib --no-default-features -- --test-threads=1`: 346 passed, 9 ignored;
- `cargo test --lib --no-default-features --features native-async-io -- --test-threads=1`:
  349 passed, 9 ignored;
- `cargo test --no-default-features -- --test-threads=1`: 346 library и 5 integration tests
  passed, 9 library tests ignored.
- `cargo +nightly fmt --all -- --check`;
- `graphify update .` через рабочий `uv tool run --from graphifyy` fallback: 49 154 узла,
  128 325 связей.

Это подтверждает реализованный graph-backed срез, но не подменяет отсутствующие контракты
`Q-23` и `Q-24` и потому не является основанием отметить весь этап завершённым.

Полный Clippy остаётся отдельным открытым `Q-14`: текущий crate останавливается на 2407
накопленных нарушениях документации/visibility и связанных lint-policy требований. Их массовое
исправление не относится к DML-срезу и не маскируется как успешный gate этапа 9.

## 6. Незавершённые нормативные блокеры

### `Q-23`: физическое хранение системных таблиц

`gql_spec` относит таблицу без named graph к `GraphId=0`, но `disk_storage_spec` не задаёт её
data/index layout. Текущий `GraphSuperblock` запрещает нулевой graph ID, а module contract запрещает
создавать фиктивный runtime graph. Нужно выбрать database-level files либо специальное system
graph-generation пространство и затем определить WAL, checkpoint, recovery и index ownership.

### `Q-24`: identity создаваемого `RELATE` edge

Синтаксис `[r:FRIEND]` определяет alias и label, а спецификация отдельно утверждает независимость
alias, label, table и record identity. Не определено, из какой relation table и с каким `RecordId`
создаётся edge. Отождествлять label `FRIEND` с именем таблицы нельзя: это изменило бы schema,
membership, unique indexes и durable primary key без нормативного основания.

До решения этих двух вопросов этап 9 не отмечается завершённым.

## 7. Намеренно отложено

- `Q-07`, этап 11: statement refresh для ReadCommitted, staged DDL→DML analysis и утверждённая
  abort/savepoint semantics внутри явного `Transaction::execute`;
- DML `TIMEOUT` и общие execution budgets — этап 12;
- `BEGIN IMPORT`, segmented commits и streaming output writers — этап 10;
- events/changefeed и advanced indexes — их собственные последующие этапы.

Новых зависимостей для текущего среза не добавлено.
