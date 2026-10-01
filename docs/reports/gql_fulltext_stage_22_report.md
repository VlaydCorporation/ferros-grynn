# Отчёт по этапу 22: durable analyzers и Fulltext

**Дата:** 2026-10-01
**Статус:** F0–F9 реализованы; focused correctness и 100K performance gate
пройдены. Полные F10–F11 остаются открыты до concurrent-DML и
resource/performance matrix.

## 1. Реализованный срез

- Durable analyzers принадлежат общему schema catalog и поддерживают built-in
  tokenizers, ordered filters, inline `mapper`, span-aware PURE UDF и
  dependency invalidation.
- Fulltext definition хранит analyzer/version, BM25, highlight mode и отдельные
  base/delta bindings. Side-file строится streaming writer-ом через общий
  external sort; полный `Vec<AlignedPage>` не создаётся.
- Реализованы exact scan и physical index paths для `@@`, numbered match slots,
  `@AND@`/`@OR@`, BM25 score и highlight. Stale/corrupt automatic path
  деградирует в exact scan; forced incompatible path возвращает typed error.
- Immediate updates публикуют exact replacement/tombstone delta, Deferred и
  dependency changes используют Stage-20 maintenance/rebuild protocol.
- Optimizer и `EXPLAIN` получают analyzer/index versions, fallback reason и
  search counters без передачи storage DTO в `gql`.

## 2. Correctness evidence

Текущий focused gate:

```text
cargo test --lib --no-default-features analyzer -- --test-threads=1
  9 passed; 0 failed

cargo test --lib --no-default-features fulltext --quiet -- --test-threads=1
  9 passed; 0 failed

cargo test --lib --no-default-features gql::execution::tests::select:: -- --test-threads=1
  32 passed; 0 failed; 2 ignored Stage-13 probes

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
  storage::local_store::tests::streamed_search_index_failures_discard_unreachable_generation \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  storage::local_store::tests::streamed_search_index_failures_discard_unreachable_generation \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; проверены AfterBuild/AfterFileSync/AfterCatalogStage/BeforeWalCommit

cargo test --lib --no-default-features \
  storage::local_store::tests::deferred_search_indexes_rebuild_before_becoming_query_visible \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  storage::local_store::tests::deferred_search_indexes_rebuild_before_becoming_query_visible \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; Fulltext/HNSW/DiskANN jobs завершились, стали Ready и пережили reopen

cargo test --lib --no-default-features \
  datastore::text_analyzer::tests::covers_class_ascii_uppercase_ngrams_and_mapper_drop \
  -- --exact --test-threads=1
  1 passed; 0 failed; class/ASCII/uppercase/ngram/mapper replacement и drop

cargo test --lib --no-default-features \
  storage::local_store::tests::fulltext_physical_results_match_scan_after_local_reopen \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  storage::local_store::tests::fulltext_physical_results_match_scan_after_local_reopen \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; NFKC + overlapping ngram highlight совпадает для scan/index/reopen

cargo test --lib --no-default-features \
  storage::local_store::tests::udf_backed_physical_indexes_survive_local_reopen \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  storage::local_store::tests::udf_backed_physical_indexes_survive_local_reopen \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; UDF spans дают одинаковый highlight для scan/index/reopen

cargo test --lib --no-default-features \
  datastore::maintenance::tests::local_reopen_reconciles_published_interrupted_and_orphan_jobs \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  datastore::maintenance::tests::local_reopen_reconciles_published_interrupted_and_orphan_jobs \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; FGJB reopen завершает published, перезапускает interrupted и отменяет orphan

cargo test --lib --no-default-features \
  storage::local_store::tests::search_scope_tests::effective_search_indexes_match_scan_after_local_reopen \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  storage::local_store::tests::search_scope_tests::effective_search_indexes_match_scan_after_local_reopen \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; effective overlay Fulltext совпадает для scan/index/reopen

cargo test --lib --no-default-features \
  datastore::maintenance::tests::maintenance_retries_when_dml_changes_the_built_snapshot \
  -- --exact --test-threads=1
cargo test --lib --no-default-features --features native-async-io \
  datastore::maintenance::tests::maintenance_retries_when_dml_changes_the_built_snapshot \
  -- --exact --test-threads=1
  по 1 passed; 0 failed; stale Fulltext/DiskANN candidate получает Retry и перестраивается
```

Проверки покрывают catalog round-trip, analyzer pipeline и UDF spans,
scan/index/delta parity, stale/corrupt fallback, Deferred rebuild до
query-visible состояния, built-in Unicode/tokenizer/filter matrix, сохранение
исходного scalar span в n-gram/UDF highlight, malformed token shape/range и
PURE/signature rejection, base/effective scope, durable job reconciliation,
детерминированный DML interleave между build/publication, local reopen и обе
I/O routes в reopen slice.

## 3. Performance evidence

Release Criterion executable:

```text
target/release/deps/gql_stage22_23-9fa1f224caa273a9.exe
```

Dataset из `target/gql-stage22/setup.csv`:

```text
rows=100000
import_ms=644.1704
fulltext_build_ms=577.1296
```

| Path           |      Mean |    Median | Отношение по mean |
|----------------|----------:|----------:|------------------:|
| exact scan     | 30,431 ms | 30,867 ms |             1,00x |
| Fulltext index |  1,138 ms |  1,137 ms |            26,74x |

Hard gate `>=3x` при population `>=100K` пройден. Небольшой query не создаёт
query-run files; builder использует общий bounded external-sort lifecycle.
Criterion evidence находится в
`target/criterion/gql_stage22_fulltext_query/`.

## 4. Что остаётся до закрытия F10–F11

- sustained public-worker DML stress во время initial/catch-up;
- формальная проверка peak RSS и 256 MiB budget на больших postings/offsets;
- отдельная regression/instrumentation matrix для non-search SELECT/DML;
- расширенная performance matrix и формальный resource gate.

Этап 22 не объявлен завершённым: текущие результаты закрывают реализацию F0–F9
и минимальный проверяемый correctness/performance checkpoint, но не заменяют
полный gate из stage-plan.
