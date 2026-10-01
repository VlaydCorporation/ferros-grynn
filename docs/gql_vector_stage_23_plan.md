# Этап 23. Exact KNN, HNSW и page-backed DiskANN

**Статус: ожидает завершения этапа 22.** Это нормативный dependency-ordered
план этапа 23. Evidence публикуются в
[`reports/gql_vector_stage_23_report.md`](reports/gql_vector_stage_23_report.md).

## 1. Граница и зависимости

Добавляются durable exact vector kinds `F16/I8/U8`, общий distance/KNN oracle,
собственный HNSW codec/builder, page-backed DiskANN adapter, exact common delta
и collision-safe `HASHED_VECTOR`.

Direct dependencies зафиксированы exact versions: `half=2.7.1`,
`diskann=0.59.0`, `diskann-vector=0.59.0`. `diskann` предоставляет generic
algorithm/provider слой, `diskann-vector` — distance/SIMD kernels. Страницы,
WAL, COW, recovery, identities и resource policy принадлежат Ferros-Grynn.
`diskann-disk`, `diskann-inmem` и `diskann-providers` остаются reference only.

`FGFC` не расширяется; base/delta — `DataFileKind::Index`:

```text
hnsw_<ordinal:04>.fgidx
hnsw_delta_<ordinal:04>.fgidx
diskann_<ordinal:04>.fgidx
diskann_delta_<ordinal:04>.fgidx
```

Не вводятся foreign record format, отдельный cache/runtime, общий provider
trait, bitmap/skip-scan либо approximate correctness fallback.

## 2. Vector и distances

`Vector`/`VectorKind`/`Type`/`ValueKind`/`StorageValue` получают `F16`, `I8` и
`U8`. Canonical codec меняется на месте; старые bytes не поддерживаются.
`-0.0` нормализуется к `+0.0`, NaN canonical для обычного durable Value, но ANN
definition/query отклоняет любой non-finite component.

Public distance contracts:

- Euclidean: `sqrt(sum(diff^2))`, ranking может использовать square;
- Manhattan;
- Minkowski с finite `p >= 1`;
- Cosine: `1 - dot/(norms)`, zero vector — domain error;
- Inner product: distance `-dot`;
- Cosine normalized: derived normalized copy и `1-dot`.

F16 widening выполняет `half`; integer L1/L2 используют checked wide
accumulation. Custom distance разрешён durable только для HNSW и обязан быть
exact versioned `PURE COST fn(vector<T>, vector<T>) -> float`, finite и `>=0`.
Session closure доступна только exact scan.

## 3. KNN

```ebnf
knn_op ::= "<|" positive_expr
           [ "," ( knn_distance | "EF" positive_expr ) ]
           "|>"
```

Оператор infix: слева vector field, справа query vector. `K`/`EF` вычисляются
один раз до row pull, `K>0`, `EF>=K`. `<|K|>` использует Euclidean;
explicit distance выбирает compatible ANN либо exact scan; `EF` требует ровно
один Ready compatible ANN index. Natural order: distance, затем public stable
identity. Missing/null пропускается, mismatch type/dimension ошибочен.

Без явного `EF` HNSW использует `max(K, 1024, DIMENSION)`, а DiskANN —
`max(K, 64)`. HNSW floor выбран по 100K×128 и 10K×768 recall/latency sweeps;
explicit `EF` всегда имеет приоритет и не переписывается.

Один source допускает один KNN predicate. Дополнительный row predicate в этом
этапе использует exact prefiltered scan; forced ANN отклоняется. Exact top-K
потребляет `O(K)` memory и является oracle для recall и fallback.

## 4. HNSW

Поддерживаемые types: `F64/F32/I64/I32/I16`; distances: Euclidean, Cosine,
Manhattan, Minkowski и custom UDF. Defaults: `M=12`, `EFC=150`, `M0=2M`, level
multiplier derived from M. Seed детерминирован по IndexId/version/source
revision/stable row identity.

Base хранит full vectors, stable row addresses, levels и adjacency каждого
level. Reader может лениво использовать текущий PageManager/BufferPool; второй
глобальный cache и `FG_HNSW_CACHE_SIZE` не вводятся.

## 5. DiskANN

Поддерживаемые types: `F32/F16/I8/U8`; distances: Euclidean, Cosine,
InnerProduct и CosineNormalized. Defaults: degree 64, `L_BUILD=100`, alpha 1.2.

`FerrosDiskAnnProvider` является одним concrete adapter над captured storage
snapshot. Official generic algorithms/search/pruning и vector kernels не
получают ownership файлов, WAL, IDs либо runtime. Immutable page header/codec
и structural validator принадлежат storage module.

## 6. Common exact delta и HASHED_VECTOR

Insert/replace delta хранит full canonical vector bytes и row address; delete
хранит tombstone. Base tombstones исключаются, все live delta inserts exact-
сканируются, а final candidates rerank по full bytes. Breadth расширяется до K
live rows; если correctness нельзя доказать из-за tombstones/resource boundary,
исполняется exact fallback без partial top-K.

Rebuild threshold:

```text
min(4096, max(1, floor(posting_count / 1000)))
```

`HASHED_VECTOR` разрешён только DiskANN. RapidHash V3 canonical bytes/seed 0
группирует identical vectors в один graph ordinal и stable posting list. Hash
collision всегда разрешается сравнением full bytes; более 4096 distinct
collision candidates даёт resource error. Hashed/unhashed modes строят один
ANN graph и обязаны возвращать те же candidates/results.

## 7. Catalog, planning и recovery

Index definition хранит exact element type/dimension/distance/parameters/flags
и optional `FunctionId + SchemaVersion`; отдельно versioned HNSW, DiskANN и
common-delta headers. Fingerprint включает codec/algorithm versions, parameters,
UDF identity/version, build seed и exact DiskANN crate versions. Mismatch делает
index stale и запускает rebuild.

Physical paths: `ExactKnnScan`, `HnswSearch`, `DiskAnnSearch`, common delta merge
и exact rerank. GQL planner получает только logical metadata и availability,
без DataFileId/pages/provider. Corrupt automatic secondary route использует
exact scan с diagnostic; forced route ошибочен. `EXPLAIN ANALYZE` показывает
hops, evaluations, candidates, delta, reranks и page/cache counters.

## 8. Порядок реализации

| Шаг | Зависит от | Результат |
|---|---|---|
| V0 | 21, F1 | Dependency/contract sync и baseline |
| V1 | V0 | F16/I8/U8 runtime/storage/type/codecs |
| V2 | V1 | Distances, custom UDF, grammar/IR и exact top-K |
| V3 | V1–V2 | HNSW/DiskANN definitions/catalog/dependencies/hints |
| V4 | V3 | Common delta, merge/rerank/rebuild trigger |
| V5 | V2–V4 | Deterministic HNSW builder/page codec/search |
| V6 | V5 | HNSW maintenance/recovery/optimizer/recall gate |
| V7 | V0,V3–V4 | Ferros provider и DiskANN base pages |
| V8 | V7 | Bounded search, rerank/delta/exact fallback |
| V9 | V8 | HASHED_VECTOR dictionary/collision/parity |
| V10 | V8–V9 | DiskANN maintenance/recovery/optimizer/EXPLAIN |
| V11 | V6,V10 | Correctness matrix |
| V12 | V11 | Recall/latency/RSS/build/I/O gate |
| V13 | V12 | Report, full_plan/registry/AGENTS/Graphify closure |

## 9. Gates

Correctness covers all vector kinds and codec laws; scalar distance oracle at
dimensions 1/32/128/768; top-K/ties/scopes/residual fallback; HNSW invariants
and deterministic bytes; DiskANN snapshot/page bounds; UDF version/security;
delta/tombstones/duplicates/rebuild races; stale/incompatible hints;
forced-collision HASHED_VECTOR tests; crash boundaries; RAM/local/reopen/
checkpoint/Sync/NativeAsync parity and 20 deterministic replay runs.

Performance matrix uses populations 10K/100K/1M, dimensions 32/128/768,
K=1/10/100, breadth K/64/200, clustered/uniform/duplicate/effective data and
warm/cold/delta states. Non-vector regression is `<=5%`; default HNSW and
DiskANN recall@10 is `>=0.95`; at `>=100K` ANN is `>=3x` exact scan while
meeting recall; DiskANN query RSS is not `O(N)`; all build/query budgets hold;
HASHED_VECTOR leaves recall/result unchanged; instrumentation is `<=2%`.

Profiling uses `--profile profiling`, exact EXE/PDB and
`D:\Programming\Tools\Intel\oneAPI\vtune\2026.4`; artifacts live under
`target/gql-stage23/`.
