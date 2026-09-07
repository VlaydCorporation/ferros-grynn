# Отчёт по этапу 6: StatisticsStore

Дата: 2026-09-07  
Статус: завершён

## 1. Граница этапа

Реализован только этап 6 `docs/full_plan.md`:

- точные graph/table/label counts;
- точные целочисленные degree totals и производные средние;
- equi-depth degree/property histograms, exact NDV и null fractions при полном scan;
- транзакционное обновление простых счётчиков при point-DML;
- явная stale-семантика bulk-распределений;
- сохранение и восстановление одного `statistics.fgb` в local backend.

Индексы, cost optimizer, публичный `ANALYZE`, sampling/HLL и import-time external sort не
реализовывались: они принадлежат последующим этапам.

## 2. Нормативный контракт

`docs/disk_storage_spec.md` §19 синхронизирован с format-v1 candidate:

- `StatisticsStore` влияет только на выбор плана; residual predicate остаётся обязательным;
- устаревшая или отсутствующая histogram не влияет на корректность результата;
- durable-образ — один checksummed `statistics.fgb`, а не три независимых файла;
- точные integer totals являются authority, записанные средние — проверяемой избыточностью;
- label statistic хранит точный `property_count(u64)`, а не lossy `avg_prop_count(f32)`;
- variable-width property bounds имеют явные `u16` lengths;
- текущий bulk-путь считает NDV точно; прежнее неподтверждённое обещание «64-byte HLL ≈ 2%»
  удалено;
- table counts не дублируются на disk: их authority остаётся generation-aware membership
  `SchemaCatalog`.

## 3. Реализация

### 3.1. Логическая модель

`src/stats` владеет одним MVCC `StatisticsStore`, содержащим `GraphStatistics` для каждой
точной `RecordId` графа. В статистике хранятся:

- `revision`, `analyzed_revision`, `dirty_rows`;
- node/edge/table/label cardinality;
- точные out/in/undirected degree totals;
- maximum out/in degree;
- global и per-node-label degree histograms;
- per-table/per-field property histograms.

RAM и local full-image используют один `analyze_graph`. Он выполняет полный scan одного
snapshot, учитывает нулевые степени, сортирует order-preserving property keys и формирует не
более 100 сбалансированных equi-depth buckets. Неподдерживаемый schema type пропускается. Если
конкретное значение нельзя безопасно закодировать индексным codec, отключается только histogram
этого поля; пользовательский commit не отклоняется.

### 3.2. Транзакционная семантика

Point-DML обновляет node/edge/label counts, точный label property count и degree totals один раз
в составе commit. Table counts одновременно выводятся из публикуемого `SchemaCatalog` membership.
Membership-only commit также увеличивает `dirty_rows`, поскольку меняет множество строк property
histogram. Degree/property pages сохраняются без изменений, а `revision`, `analyzed_revision` и
`dirty_rows` делают их stale-состояние наблюдаемым. Maximum degree после уменьшения может быть
консервативной верхней границей до следующего full scan.

Full graph image и schema-affected materialized rebuild заменяют counts и distributions одним
точным анализом. Публикация local statistics входит в ту же frame-WAL/catalog транзакцию, что и
данные графа.

### 3.3. Durable layout

`statistics.fgb` имеет закрытую последовательность страниц:

1. `StatisticsRoot` page 0;
2. contiguous degree range;
3. contiguous property range;
4. contiguous label tail.

Root хранит start/count каждого диапазона и точную длину логического образа. Decoder отвергает
разрывы, перестановки, лишние страницы, неверные links, duplicate keys, non-zero reserved bytes,
неизвестные directions/type tags и несогласованные totals. Point delta публикует root и label
tail целиком; commit-validator запрещает изменение ranges degree/property и неполный label-range.
Degree/property pages не переписываются и не становятся частично обновлёнными.

Legacy one-page root читается совместимо. Перед первой delta-публикацией label counts и
property totals гидратируются из live hot slots; после этого файл записывается в layout v1.

## 4. Обнаруженные и закрытые проблемы

### Q-15. Расхождение спецификации и формата

Старый §19 одновременно обещал несуществующие sampled/auto `ANALYZE`, неточный HLL budget,
три stats-файла и неоднозначный bucket codec. Контракт заменён фактически проверяемым layout v1
и явными границами отложенной функциональности.

### Q-16. Delta decode context и over-aligned root

При переиспользовании `RecordId` один target `EncodeContext` уже не содержал удалённое старое
сопоставление, нужное для декодирования прежних properties. Delta builder теперь использует
отдельный source context для чтения старой версии и target context для записи новой.

На Windows MSVC также воспроизводилось повреждение первого байта magic (`FGST` → `\0GST`) при
перемещении уже заполненного, выровненного на 16 KiB `AlignedPage` в `Vec`. Повреждение возникало
до Direct I/O и resident arena. Root теперь сначала создаётся в окончательном слоте `Vec` и
заполняется там in-place. После исправления один и тот же datastore test executable прошёл десять
последовательных полных запусков без рецидива.

## 5. Проверки

Пройдены:

- `cargo test --lib --no-default-features stats::tests::bulk_analysis_and_point_delta_preserve_exact_counters -- --exact --test-threads=1`;
- `cargo test --lib --no-default-features statistics_file_round_trips_auxiliary_pages -- --test-threads=1`;
- local full-image/reopen и point-delta/reopen regressions;
- `local_point_mutation_reuses_removed_record_id`;
- `cargo test --lib --no-default-features datastore::tests -- --test-threads=1`;
- `cargo test --lib --no-default-features --features native-async-io datastore::tests -- --test-threads=1`;
- `cargo check --lib --no-default-features` и тот же check с `native-async-io`;
- `cargo +nightly fmt --all -- --check`, targeted `git diff --check` для файлов этапа и
  `just build`;
- тот же 40-test datastore executable десять раз подряд (400 успешных тестов).

Полный repository Clippy не используется как ложное доказательство этого среза: его блокирует
2191 накопленным documentation/panic/error нарушением из `Q-14`. Повторный filtered запуск не
показал diagnostics в новых `src/stats/mod.rs` и `src/storage/graph_pages/statistics.rs`.

## 6. Известный остаточный потолок

`Q-17` оставлен открытым и не маскирует завершение correctness-контракта:

- point-DML кодирует полный логический stats image и пишет root плюс все label pages —
  `O(number of labels)`;
- exact property analysis удерживает encoded values до сортировки — `O(rows × fields)` памяти.

Первый потолок оптимизируется только после измеренного вклада в commit profile. Второй должен
переиспользовать external-sort/bulk infrastructure этапа 10. Ни touched-label directory, ни HLL,
ни новый dependency заранее не добавлялись.
