# Отчёт по этапу 23: HNSW и DiskANN

**Дата:** 2026-10-01
**Статус:** V0–V10 реализованы; focused correctness, recall и 100K
production-default DiskANN gate пройдены. Полные V11–V13 остаются открыты до
1M/dimension/workload/resource matrix.

## 1. Реализованный срез

- Runtime/storage vectors поддерживают `F16`, `I8` и `U8` вместе с прежними
  kinds; exact distance oracle и KNN сохраняют `O(K)` result memory.
- HNSW и DiskANN имеют отдельные deterministic base codecs/builders и общий
  exact delta с replacements/tombstones, full-byte rerank и exact fallback.
- `HASHED_VECTOR` дедуплицирует одинаковые canonical vectors, проверяет полные
  bytes при hash collision и сохраняет result parity с обычным DiskANN.
- Local DiskANN читает directory/node ranges через snapshot-exact
  `PageManager`/`HybridReadEngine`. Query удерживает только bounded окно из 256
  validated pages и читает cold pages batches до 32; полный base generation в
  память не декодируется.
- Immediate/Deferred maintenance, rebuild/compact, optimizer hints, stale и
  corruption fallback, `EXPLAIN` и local reopen подключены к общим контрактам
  этапов 14 и 20.

## 2. Сверка с SurrealDB

Проверены локальные reference implementations в `crates/surrealdb/`:

- DiskANN использует weighted process-wide cache decoded nodes/vectors и
  batched provider reads;
- HNSW также использует общие vector/document caches;
- DiskANN beam expansion группирует adjacency и vector misses;
- high-dimensional HNSW diagnostics показывают рост recall при увеличении
  query-time `EF`; `KEEP_PRUNED_CONNECTIONS` на их synthetic 768D workload
  существенного выигрыша не дал.

В Ferros-Grynn перенесён только применимый принцип bounded batch reads.
Process-wide ANN cache не добавлен: существующий BufferPool остаётся общей
cache authority, а второй cache дублировал бы memory accounting и eviction.
После bounded query-local page window штатная конфигурация уже проходит
latency gate, поэтому дополнительная cache abstraction не нужна.

## 3. Correctness evidence

```text
cargo test --lib --no-default-features vector -- --test-threads=1
  24 passed; 0 failed; 1 ignored heavy 100K probe

cargo test --lib --no-default-features storage::local_store::tests:: -- --test-threads=1
  21 passed; 0 failed; 2 unrelated heavy diagnostics ignored

cargo test --lib --no-default-features storage::catalog::database::tests:: -- --test-threads=1
  8 passed; 0 failed

cargo test --lib --features native-async-io \
  physical_results_match_scan_after_local_reopen -- --test-threads=1
  2 passed; 0 failed (Fulltext и vector reopen parity)

cargo test --lib --no-default-features -- --test-threads=1
  584 passed; 0 failed; 10 ignored

cargo test --lib --no-default-features --features native-async-io -- --test-threads=1
  586 passed; 0 failed; 11 ignored

cargo check --lib --no-default-features
cargo check --lib --no-default-features --features native-async-io
cargo +nightly fmt --all -- --check
  все команды завершились успешно

cargo test --lib --no-default-features \
  storage::local_store::tests::vector_physical_results_match_scan_after_local_reopen \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  storage::local_store::tests::vector_physical_results_match_scan_after_local_reopen \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; каждый HNSW/DiskANN result и PlanHash совпал в 20 повторах

cargo test --lib --no-default-features \
  storage::local_store::tests::streamed_search_index_failures_discard_unreachable_generation \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  storage::local_store::tests::streamed_search_index_failures_discard_unreachable_generation \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; проверены AfterBuild/AfterFileSync/AfterCatalogStage/BeforeWalCommit

cargo test --lib --no-default-features \
  datastore::vector_distance::tests::builtin_distances_cover_supported_kinds_and_dimensions \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  datastore::vector_distance::tests::builtin_distances_cover_supported_kinds_and_dimensions \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; F64/F32/F16/I64/I32/I16/I8/U8 и dimensions 1/32/128/768

cargo test --lib --no-default-features \
  storage::local_store::tests::deferred_search_indexes_rebuild_before_becoming_query_visible \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  storage::local_store::tests::deferred_search_indexes_rebuild_before_becoming_query_visible \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; Fulltext/HNSW/DiskANN jobs завершились, стали Ready и пережили reopen

cargo test --lib --no-default-features \
  datastore::maintenance::tests::local_reopen_reconciles_published_interrupted_and_orphan_jobs \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  datastore::maintenance::tests::local_reopen_reconciles_published_interrupted_and_orphan_jobs \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; HNSW Publishing match, DiskANN mismatch/rebuild и orphan target сверены после FGJB reopen

cargo test --lib --no-default-features \
  storage::local_store::tests::search_scope_tests::effective_search_indexes_match_scan_after_local_reopen \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  storage::local_store::tests::search_scope_tests::effective_search_indexes_match_scan_after_local_reopen \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; effective overlay HNSW/DiskANN совпадает для scan/index/reopen

cargo test --lib --no-default-features \
  datastore::maintenance::tests::maintenance_retries_when_dml_changes_the_built_snapshot \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  datastore::maintenance::tests::maintenance_retries_when_dml_changes_the_built_snapshot \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; stale Fulltext/DiskANN candidate получает Retry и перестраивается
```

Focused tests покрывают vector kinds/codecs, distance domain checks,
deterministic HNSW bytes/invariants, DiskANN reachability/degree, common delta,
forced HASHED_VECTOR collisions с full-byte comparison, paged search,
streaming writer, base/effective scope, deterministic build/publication
revision-race, corruption fallback и local reopen.

## 4. Recall и 100K latency

Release fixture из `target/gql-stage23/setup.csv`:

```text
rows=100000
dimension=32
buffer_pool_pages=4096
hnsw_build_ms=233950.1015
diskann_build_ms=370401.1503
hashed_diskann_build_ms=367469.1402
hnsw_recall_at_10=0.985
diskann_recall_at_10=1.0
```

Criterion query results:

| Path       |       Mean |     Median |                   Speedup |
|------------|-----------:|-----------:|--------------------------:|
| exact scan | 227,548 ms | 226,525 ms |                     1,00x |
| DiskANN    |  39,667 ms |  40,013 ms | 5,74x mean / 5,66x median |

Hard gates recall@10 `>=0.95` и ANN throughput `>=3x` при `>=100K` пройдены
для штатного BufferPool. Evidence находится в
`target/criterion/gql_stage23_knn_query/`.

## 5. High-dimensional HNSW

Первый 10K×768 прогон с прежним default `EF=64` дал HNSW recall@10 `0,815`
при DiskANN `0,99`. Отдельный probe на одном HNSW generation подтвердил, что
это query-time breadth, а не жёсткий предел builder:

|   EF | Recall@10 |
|-----:|----------:|
|   64 |     0,815 |
|  150 |     0,835 |
|  200 |     0,910 |
|  300 |     0,940 |
|  512 |     0,970 |
|  768 |     0,980 |
| 1024 |     0,985 |

Первоначальная диагностика ошибочно использовала число rows как denominator.
После учёта exact-vector grouping граф содержит `8020` unique vector nodes:
level-0 reachability равна `7939/8020` (`98,99%`), средняя исходящая степень —
`24,00`. Поэтому копирование SurrealDB cache/`KEEP_PRUNED_CONNECTIONS` и
переписывание builder не получили измеренного основания.

Промежуточный dimension-aware default `max(K, 64, DIMENSION)` прошёл 10K×768,
но не прошёл population-scaled 100K×128: при `EF=128` recall@10 был только
`0,765`. На том же 100K generation `EF=512` дал `0,875`, а `EF=1024` —
`0,970`. Граф содержал `80020` unique vectors; reachability `78217/80020`
(`97,75%`) и средняя степень `24,00` подтвердили query-breadth, а не
разрушенную связность, как непосредственный blocker обязательного recall@10.

Финальный default закреплён как `max(K, 1024, DIMENSION)`; explicit `EF`
сохраняет точное пользовательское значение, DiskANN остаётся `max(K, 64)`.
Один generation и exact top-100 oracle дали:

| Dataset  |   K | Default EF |              Recall |
|----------|----:|-----------:|--------------------:|
| 10K×768  |   1 |       1024 |     1,000 (`20/20`) |
| 10K×768  |  10 |       1024 |   0,985 (`197/200`) |
| 10K×768  | 100 |       1024 | 0,983 (`1966/2000`) |
| 100K×128 |   1 |       1024 |     1,000 (`20/20`) |
| 100K×128 |  10 |       1024 |   0,970 (`194/200`) |
| 100K×128 | 100 |       1024 | 0,889 (`1779/2000`) |

Нормативный hard gate относится к recall@10. K=1/100 остаются matrix evidence,
поэтому результат K=100 не скрывается и не используется для бесконечного
увеличения default breadth.

End-to-end benchmark 10K×768 с промежуточным `EF=768` прошёл:

```text
rows=10000
dimension=768
buffer_pool_pages=4096
hnsw_build_ms=246740.592
diskann_build_ms=69173.6684
hashed_diskann_build_ms=71687.4242
hnsw_recall_at_10=0.98
diskann_recall_at_10=0.99
```

| Path                  |      Mean | Speedup против exact |
|-----------------------|----------:|---------------------:|
| exact scan            | 77,433 ms |                1,00x |
| HNSW                  | 21,803 ms |                3,55x |
| DiskANN               | 62,941 ms |                1,23x |
| HASHED_VECTOR DiskANN | 61,433 ms |                1,26x |

Hard throughput gate относится к population `>=100K`; 10K×768 фиксирует
dimension/recall и стоимость нового default, но не заменяет 100K/1M matrix.
Evidence: `target/gql-stage23/hnsw-recall-10k-d768.log`,
`setup-10k-d768-p4096.csv` и `criterion-10k-d768-p4096/`.

Последний tuning-run проверил финальный floor на 100K×32:

```text
rows=100000
dimension=32
buffer_pool_pages=4096
hnsw_build_ms=238540.8896
diskann_build_ms=385229.5857
hashed_diskann_build_ms=388319.6652
hnsw_recall_at_10=1.0
diskann_recall_at_10=1.0
```

| Path                  |      Mean | Speedup против exact |
|-----------------------|----------:|---------------------:|
| exact scan            | 251,79 ms |                1,00x |
| HNSW                  |  8,601 ms |               29,27x |
| DiskANN               | 49,637 ms |                5,07x |
| HASHED_VECTOR DiskANN | 48,358 ms |                5,21x |

Новый HNSW floor сохраняет hard throughput gate `>=3x` с большим запасом.
Evidence: `hnsw-recall-100k-d128.log`, `setup-100k-d32-ef1024.csv`,
`bench-100k-d32-ef1024.stdout.log` и `criterion-100k-d32-ef1024/`.

## 6. Low-cache stress и profiling

Отдельный режим `buffer_pool_pages=256` не является production default:

| Rows |     Exact |   DiskANN | Итог              |
|-----:|----------:|----------:|-------------------|
|  10K | 21,316 ms | 12,568 ms | 1,70x             |
| 100K | 216,92 ms | 302,77 ms | latency gate FAIL |

Этот результат сохранён как larger-than-cache stress evidence, а не скрыт
штатным benchmark. Software-sampling VTune profile
`target/gql-stage23/vtune-diskann-query-current-sw/` показал повторные 16 KiB
page copies/checksum validation; validate-once и bounded page window устранили
hotspot на warm/default route. Hardware sampling в этом прогоне недоступен и
не используется как доказательство.

## 7. Что остаётся до закрытия V11–V13

- populations до 1M, 100K×768/1M high-dimensional и оставшаяся breadth matrix
  на этих масштабах;
- формальные peak RSS/cache-pressure measurements, включая dataset больше
  storage cache;
- HNSW/DiskANN latency matrix на clustered/uniform/duplicate-heavy data;
- sustained public-worker DML stress для initial и catch-up builds;
- расширенная performance/resource matrix.

Этап 23 не объявлен завершённым. Global ANN cache, отдельный runtime и foreign
DiskANN storage format намеренно не добавлены: для текущего подтверждённого
bottleneck достаточно существующего BufferPool и bounded page batching.
