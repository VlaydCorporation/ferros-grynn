# Интеграционные тесты

`system.rs` — тематический набор тестов публичного API root crate. Структура
`tests/system/` сначала разделяет составляющие системы, затем backend:

```text
system/
  operations/       # create/read/update/delete, rollback, attach/detach/cascade
  transactions/     # Snapshot, ReadCommitted, Serializable, FCW, ReadOnly
  schema/           # SDK catalog, SCHEMAFULL, private definitions
  queries/          # параметры, SELECT, DML, GqlSession, diagnostics
  sdk/              # owned snapshots/results, runtime values, graph algorithms
  columnar/         # PropertyBatch, Arrow import, presence/validity, DataFusion
  indexes/
    scalar/         # Standard, Unique, Count, Effective, Deferred/rebuild
    fulltext/       # mapper, BM25, Unicode highlights, deltas, snapshots, Effective
    hnsw/           # SDK definition, Euclidean oracle, recall, dimension errors
```

В каждой папке `in_memory.rs`, `local.rs` и `local_native_async.rs` подключают общий `cases.rs`. Это
отдельные именованные тесты в Cargo, с одинаковыми assertions для обоих backend.
Local использует Sync Direct I/O на Windows; `local_native_async` — `IoMode::NativeAsync`
с feature `native-async-io`. Эффективный режим проверяется через публичный
`local_storage_status()` после создания и каждого reopen. Проверки опубликованного
состояния повторяются после закрытия/reopen из WAL и после Restart checkpoint
с новым reopen. Каждый тест владеет отдельной временной БД. Handles и snapshots
освобождаются до удаления каталога. Между открытиями используются `RecordId`,
`GraphRef` и заново разрешённые `AtomId`.

Тесты компилируются как downstream crate и используют только доступные exports.
`storage-engine-prototype-api` и private production helpers им не нужны.

## Запуск

```powershell
cargo test --locked --no-default-features --test system -- --test-threads=1
cargo test --locked --test system -- --test-threads=1
cargo test --locked --no-default-features --test system indexes::fulltext
cargo test --locked --no-default-features --test system transactions::local
cargo test --locked --no-default-features --features native-async-io --test system local_native_async -- --test-threads=1
cargo test --locked --no-default-features --features columnar --test system columnar -- --test-threads=1
cargo test --locked --no-default-features --features columnar,datafusion,native-async-io --test system -- --test-threads=1
```

HNSW читает оба исходных JSON-lines `.gz` из `tests/data/` через test-only
`flate2`. Проверяется весь файл: 5000/9000 строк, 20 конечных F32-компонент.
Обычные тесты строят индекс по первым 128 векторам каждого файла и требуют
точного совпадения top-10 с независимым расчётом расстояний и `WITH NOINDEX`.
Большие тесты используют все векторы, четыре детерминированных query-вектора,
`EF 128` и recall@10 ≥ 0.9 для каждого запроса. Они помечены `ignore`, поскольку
повторяют построение и local recovery на полных корпусах:

```powershell
cargo test --locked --no-default-features --test system hnsw_full -- --ignored --test-threads=1
cargo test --locked --no-default-features --features native-async-io --test system local_native_async::hnsw_full -- --ignored --test-threads=1
```

In-memory проверяет семантику HNSW-definition и exact fallback. Local проверяет
forced HNSW lookup, включая durable generation после reopen. Recall-проверки
не являются benchmark или обещанием качества на произвольных данных.
В корпусе на 5000 строк есть одинаковые векторы с разными `RecordId`; oracle
упорядочивает равные дистанции по идентификатору. Отдельные RAM-тесты проверяют
`FG-42F03` при forced HNSW/full-text hint без physical binding.

Full-text читает `lemmatization-en.txt`: первая колонка — каноническая форма,
вторая — словоформа. 128 реальных словоформ входят в inline mapper анализатора;
ожидаемые `RecordId` рассчитываются по исходному словарю. Отдельные сценарии
проверяют update/delete/rollback, старый snapshot, BM25 и границы highlight в
оригинальном Unicode-тексте.

Columnar проверяет существующий публичный адаптер `PropertyBatch`. Исходные
строки получаются из owned SDK snapshots; Arrow batch сохраняет отдельный
presence bitmap наряду с validity. Проверяются field metadata и key mapping,
Arrow import и некорректные входы, dense union без преобразования scalar kinds,
владение данными после закрытия БД, пустые/`none`-строки и ошибки nested values.
При `datafusion` дополнительно проверяются registration, отдельные DataFrame-фильтры
для `none` и `null`, а также числовая агрегация. Для local обоих I/O-режимов
round-trip повторяется после recovery/checkpoint.

## Остальное покрытие

Существующие интеграционные targets сохраняют специализированные сценарии:

| Составляющая                             | Targets                                                                                                                 |
|------------------------------------------|-------------------------------------------------------------------------------------------------------------------------|
| Datastore, import, recovery, BUILD GRAPH | `in_memory_datastore`, `local_datastore`                                                                                |
| Графовые индексы, tensor cache           | `graph_indexes_local_sync`, `graph_indexes_local_native_async`                                                          |
| Geometry                                 | `geometry_local_sync`, `geometry_local_native_async`                                                                    |
| Changefeed                               | `changefeed_api`, `changefeed_lifecycle`, `changefeed_oracle`, `changefeed_local_sync`, `changefeed_local_native_async` |
| LIVE                                     | `live_in_memory`, `live_local`, `live_graph_select`                                                                     |
| Events                                   | `events_local`                                                                                                          |
| Graph SDK / аналитика                    | `tensor_api`, `analytics_api`, `all_shortest_api`, `public_paths`                                                       |
| Bridges                                  | `bridges_surreal`, `bridges_local_large`                                                                                |

Native async targets требуют Windows и `native-async-io`; Surreal targets —
соответствующих bridge features. Performance probes запускаются отдельно от
обычного correctness-набора. Этот набор не заменяет полный production gate
для storage/MVCC и не меняет статусы implementation plans.

Для сборки с меньшим объёмом debug-артефактов можно задать временные параметры
для текущего PowerShell-сеанса перед приведёнными командами:

```powershell
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:CARGO_BUILD_JOBS = '1'
```
