# Этап 26 — evidence report

**Дата:** 2026-10-03. **Статус:** в реализации, этап не закрыт.

Контракт: [отдельный план](../gql_tensor_analytics_stage_26_plan.md).

## 1. Исходное состояние

- Commit: `0eb735d` (geometry и derived graph indexes).
- Unrelated dirty files зафиксированы до изменений; не сбрасываются.
- Baseline `cargo check --lib --no-default-features`: PASS; 211 существующих lib warnings.
- T0 scan/DML baseline: PASS (preliminary debug/RAM, 10K rows, 5 samples): scan 651.022 ms,
  geometry candidate lookup 3.658 ms; unrelated SELECT before/after 1.116/0.710 ms,
  DML before/after 35.469/34.415 ms. `target/stage26-baseline.log`.
  Это baseline существующего workload, не performance gate нового tensor/cache.

## 2. Матрица

| Gate                                       | Статус                                                          |
|--------------------------------------------|-----------------------------------------------------------------|
| T0 normative sync                          | PASS; окончательные physical offsets принадлежат T6             |
| T1 qualified incidence                     | focused correctness PASS                                        |
| T2 public tensors/matrices                 | focused correctness PASS                                        |
| T3 scalar/spectral                         | focused correctness PASS; общие performance/resource gates OPEN |
| T4 structural/community                    | focused correctness PASS; общие performance/resource gates OPEN |
| T5 all-shortest                            | focused correctness PASS; общие performance/resource gates OPEN |
| T6–T7 durable cache/lifecycle              | focused correctness PASS; общие resource/performance gates OPEN |
| T8–T9 GQL/planner                          | focused correctness PASS; closure gates OPEN                    |
| T10 full correctness/resources/performance | OPEN                                                            |

## 3. T1–T2 evidence

- `cargo test --test tensor_api --no-default-features`: PASS, 8 tests; все runtime families,
  независимый all-pairs oracle для 64 directed graphs, NODE/ATOM и все directions,
  zero/cancelling weights, qualified shared children, boundary/induced domains, экспорт и лимиты.
- `cargo test --lib --no-default-features datastore::transaction -- --test-threads=1`: PASS, 26 tests.
- `cargo test --lib --no-default-features graph::traversal::index -- --test-threads=1`: PASS, 1 differential test.
- `cargo test --lib --no-default-features datastore:: -- --test-threads=1`: PASS, 86 tests.
  Старый фильтр `datastore::tests` выбирает 0 tests и correctness evidence не является.
- `cargo check --lib --no-default-features`: PASS; 211 прежних warnings.
- Nightly rustfmt применён только к изменённому срезу, без unrelated dirty rewrites.
- `cargo test --doc --no-default-features graph::tensor`: PASS, runtime example и Copy-cell compile-fail;
  `RUSTDOCFLAGS='-C debuginfo=0 -C link-arg=/DEBUG:NONE'` обходит environmental MSVC LNK1318 PDB limit.
- `cargo clippy --lib --no-default-features`: глобальный FAIL, 6967 existing errors вне новых tensor файлов;
  новых error diagnostics в `graph/tensor` и `transaction/tensor.rs` нет. Старые graph/read/transaction files
  сохраняют ранее существовавший private-doc lint debt.
- Read-only review: R26-01–06 исправлены; filtered/empty-row work, lazy property reads,
  LOCAL containment, Copy cells и retained Vec capacities проверяются регрессионными tests.

Представление остаётся sparse; borrowed views держат owned immutable tensor. Первоначальный
runtime depth=64 ceiling снят в T10 итеративным borrowed обходом; depth=96, qualified route,
LOCAL/ROOTED parity и budgets проверены непосредственно. Общие performance gates остаются OPEN.

## 4. T3 evidence

- `cargo test --test analytics_api --no-default-features`: PASS, 12 tests.
  Hop/weighted Brandes и harmonic сравниваются с независимым exhaustive oracle
  на 64 generated graphs; parallel alternatives, Number rounding/mixed costs,
  hyperedge parity, counter overflow, cancellation и nonconvergence проверены.
- `cargo test -p fg-gql-status`: PASS, 14 tests; convergence error использует
  существующий macro-generated status contract (`FG-54F14`).
- `cargo check --lib --no-default-features`: PASS после последнего SDK изменения.
- Rustdoc analytics example: PASS; direct вызовы всех шести algorithms без GQL.
- Nightly rustfmt: changed slice. Clippy: 6967 pre-existing errors вне новых
  analytics файлов; новых error diagnostics в аналитическом срезе нет.
- Read-only review R26-T3-01–06: исправлены prefix rounding/mixed-cost dominance,
  Katz intermediate overflow и PageRank subnormal normalization. Analytical
  vertex-acyclic domain закреплён отдельно от TRAIL Path bindings.

Pure Int distances используют Dijkstra/Brandes. Остальные Number costs пока
исполняются bounded full-state enumeration без prefix pruning; это сохраняет
точные ties ценой exponential work и quadratic Brandes scratch. Лимиты проверяются
до allocation/expansion; scaling/performance этого fallback остаётся OPEN.

## 5. T4 evidence

- `cargo test --test analytics_api --no-default-features`: PASS, 19 tests.
  Sparse motifs и Fagiolo clustering сравниваются с независимым dense oracle
  на 64 directed graphs; loops, parallel edges, weighted normalization и isolates.
- LPA проверяет точные Number votes выше 2^53, IN/OUT/BOTH и сохранение current label
  при равенстве. Louvain проверяет directed modularity, aggregation и limits.
- Два cancellation fixtures проверяют положительный gain, скрытый округлением до 0,
  в integral и half-scaled Float domains. Все candidates, включая singleton split,
  проходят проверку gain до объявления convergence.
- Integral strengths с exact total <=2^53 и gamma=1 используют checked i128 sign;
  остальные uncertain gains — уже подключённый `num::BigRational` с предварительным
  admission scratch. Новых dependencies нет.
- `cargo check --lib --no-default-features`: PASS. Nightly rustfmt применён к срезу.
- Read-only review R26-T4-01–02: PASS после empty-row work и gain certification fixes.

Clippy и общие resource/performance gates продолжают учитываться отдельно.

## 6. T5 evidence

- Direct Rust API: 6 tests PASS; all-shortest сравнивается с exhaustive bounded TRAIL oracle
  на 64 generated graphs. Zero cycles, path modes, exact prefix callbacks, Number ties,
  parallel costs, owned Path materialization и enumeration caps проверены без GQL consumers.
- Общий запуск SDK: 6 all-shortest + 19 analytics + 8 tensor tests PASS.
- Named versioned PURE COST сохраняет SSI для внешних положительных и отрицательных reads.
  Лимиты проверяются на property/array copies, binary collection comparisons, loops и nested
  PURE analyzer. `datastore::text_analyzer::tests`: 7 PASS, включая Mapper/Ngram pre-admission.
- Shared RootFields передаёт read/work observers через nested calls; bulk helper work
  учитывается до выполнения. Ngram admission покрывает minimum Vec capacity и growth overlap.
- Clippy: 6967 прежних errors; новых error diagnostics в изменённом срезе не обнаружено.
- Rustdoc `graph::analytics`: 2 examples PASS, включая callback/all-shortest example.

В T5 bounded PURE COST отклонял regex/time-format helpers до внешнего allocator. В T10
time-format получает потоковый двухпроходный writer с checked output admission; literal
regex использует существующий escape/contains fast path. Complex regex по-прежнему получает
typed resource error: compiler AST/HIR не имеют hard allocation cap. Universal regex admission
и performance остаются OPEN; неполный enumeration result не выдаётся.

## 7. T6 evidence

- `cargo test --lib --no-default-features tensor_`: 3 PASS. Checked definitions, full ATOM
  projection, CSR/DENSE round trips, metadata/revision mismatch, malformed ordinals/padding,
  full-page caps и scratch rejection проверены непосредственно.
- Catalog suite: 10 PASS после добавления durable incidence revision.
- §26.2 disk spec содержит header offsets, section identities и canonical metadata bytes.
- Generalized root reader сохраняет Transaction SSI observers; maintenance/recovery используют
  тот же capture kernel без второй incidence-модели.

- Review R26-T6-01/02: PASS после aggregate restore admission и освобождения
  validation root до изменения incidence revision. Restore учитывает retained capacities,
  validation, повторный capture и сравнение в одном memory/work budget; tests проверяют
  cancellation, совокупный work и spare capacity.
- `cargo check --lib --no-default-features`: PASS. Nightly rustfmt применён к срезу.
- Clippy: 6966 pre-existing errors; новых error diagnostics относительно T4 baseline нет.
  Один прежний missing-doc diagnostic устранён описанием изменённого SSI observer field.

Focused correctness T6 пройден. Publication/lifecycle остаются T7; этап 26 OPEN.

## 8. T7 evidence

- Tensor cache использует существующий worker, external sorter, PageManager и WAL/COW.
  Source/Target planes упаковываются из sorted stream; scratch/session cleanup общий.
  Publication повторно проверяет commit revision под lock. Durable dependency vector
  охватывает ROOTED children и LOCAL boundary/containment bindings.
- `tensor_` lib slice: 6 PASS. AUTO учитывает encoding work, restore сохраняет записанный
  concrete format при изменении лимитов. Spill packing совпадает с incidence oracle;
  malformed sections, forced missing definition work, cancellation и revision race проверены.
- Direct SDK: 6 all-shortest + 19 analytics + 9 tensor tests PASS. Cache fixture проверяет
  private abort, child invalidation, old snapshot, SSI, independent properties, concurrent
  maintenance и remove/info без GQL.
- Local Sync target: tensor fixture PASS для CSR/DENSE/AUTO, old snapshot и checkpoint/reopen.
  Local NativeAsync target: отдельный tensor fixture PASS для WAL reopen, independent commit,
  Restart checkpoint и повторного reopen. Open выполняет passive checkpoint; первоначальное
  ошибочное ожидание продвижения Restart без новых frames исправлено в test setup.
- Physical publication fixture: PASS для девяти graph definitions, включая три tensor formats;
  четыре failpoints, stable file reuse и checksum-corrupt secondary→Stale проверены.
- Shared statistics suite: 4 PASS. Обнаруженный last-label defect исправлен в общем delta helper:
  delete и rename последнего member удаляют пустой label counter до catalog encoding.
- Full datastore namespace: 90 PASS. Запуск exact lib test executable, собранного `tensor_`
  Cargo target, сохраняет no-default-features configuration и отдельный log.
- Rustdoc `GraphIndex::new_tensor`: PASS; define/rebuild/read/info/remove без GQL.
- `cargo check --lib --no-default-features`: PASS, 210 прежних warnings.
- Review R26-T7-01/02/03: PASS после filtered-definition work admission, AUTO encoding
  eligibility и restore concrete-format fixes. External-sort packing и last-label helper проверены.
- Nightly rustfmt применён к изменённому срезу. Clippy: 6966 pre-existing errors,
  новых error signatures относительно T6 нет; новые style warnings учитываются отдельно.

Qualified dictionaries/topology и owned SDK cache copies остаются bounded resident scratch.
Измеренные scaling/peak-memory gates пока OPEN; staged performance evidence не заявляется.
Последующие T8–T9 пройдены; этап 26 остаётся OPEN до полного T10 closure evidence.

## 9. T8 evidence

- Все десять graph-wide GQL steps вызывают публичные Rust kernels; induced stream,
  duplicate seeds, пустой domain, stored/unit weights, closed options и convergence
  metadata проверены. Binary consumers не складывают посторонние stored weights.
- ALL SHORTEST/ALL WEIGHTED SHORTEST проходят grammar→analysis→IR→execution в MATCH
  и TRAVERSE. Eligibility применяется до minimum; per-hop contributions сохраняют
  exact left-to-right Number fold named paths. Проверены LOCAL/NODE и ROOTED/ATOM
  consumers, bounded PURE COST и seed-bulk enumeration limits.
- Tensor cache DDL CSR/DENSE/AUTO переиспользует graph definitions и maintenance.
  Direct Rust SDK: 7 all-shortest + 19 analytics + 9 tensor tests PASS.
- Full lib run: 699 PASS, 10 ignored, 0 failures. Последующий focused `tensor_`:
  13 PASS, включая wide probe-row admission и quadratic uniqueness work.
  Extended nested/AtomWalk fixtures и shared quota/capacity tests PASS.
- Review R26-T8-01–11: закрыты после shared gas admission, bounded COST/predicates,
  phase reservations, options/seed/label pre-admission, remaining quota и probe-row
  fixes. Исчерпанная path quota не запрещает non-enumerating kernels.
- `cargo check --lib --no-default-features` PASS; nightly rustfmt применён к срезу.
	Повторный Clippy T8: 6966 pre-existing errors; новых error signatures и увеличения
	числа прежних diagnostics относительно T7 нет.

Focused correctness T8 пройден; последующий T9 описан ниже. T10 closure остаётся OPEN.

## 10. T9 evidence

- Logical tensor binding содержит index/version/revision, полный dependency vector,
  scope и фактический CSR/DENSE format. Planner и runtime используют общий descriptor,
  revision и origin validator; выбранный binding сравнивается полностью.
- Physical Tensor operators различают ONE/ALL, scope, projection, parameters и cache
  generation. Canonical expression fingerprint переиспользован двумя consumers;
  PlanHash version 6 исключает source spans.
- EXPLAIN ANALYZE сообщает actual cache hits/fallbacks, cells/nonzeros, iterations,
  algorithm work/paths и encoded artifact bytes. Pages не объявляются прочитанными
  только потому, что artifact существует: physical I/O counters остаются фактическими.
- `tensor_` lib slice: 14 PASS. CSR/DENSE bindings, plan hashes и actual counters
  проверены. Этот test выявил и закрыл общий undirected planes defect: обе стороны
  содержат binary union endpoints; original topology/roles сохранены отдельно.
- Direct SDK: 7 all-shortest + 19 analytics + 10 tensor tests PASS. Small-budget
  fallback не резервирует metadata для 128 посторонних ON_DEMAND definitions.
- ROOTED `TRAVERSE FROM MATCH` regression: PASS для analytics и ALL, on-demand/cache,
  default/descending ORDER BY. MATCH root удерживается независимо от порядка seeds;
  projection и ORDER parser не поглощают внешний pipeline. Legacy consumers сохранены.
- Review T9-F1/F2 закрыт; дополнительный ORDER boundary исправлен и проверен.
- `cargo check --lib --no-default-features`: PASS. Nightly rustfmt применён к срезу.
  Clippy остаётся pre-existing global failure: scoped PAGE_SIZE visibility исправлен;
  T10 lib rerun содержит 6956 pre-existing errors. Дополнительный lib-test compile
  сообщает 759 прежних test lints и учитывается отдельно от библиотечного baseline.

Focused correctness T9 пройден. T10 resource/performance/full closure остаётся OPEN.

## 11. T10 resource evidence

- Runtime ROOTED capture и nested endpoint resolution используют итеративные borrowed
  runtime views. Произвольный depth=64 threshold удалён; GraphRead/public identities не изменены.
- Direct SDK depth=96 regression: PASS. Проверены owner dictionaries, distinct domains,
  containment descriptors, cross-level adjacency и явные memory/work rejections.
- Versioned PURE COST helper regression: PASS. Unicode literal regex true/false и `%Y`
  работают; большой regex compiler и wide format под малым work budget получают FG-54F13.
  Lenient Jiff formatting сохраняет исходный typed writer failure, без partial success.
- Read-only T10 helper review: concrete новых flaws не найдено. RegexBuilder size_limit,
  dfa_size_limit и nest_limit не покрывают AST/HIR allocation; normal regex execution
  сохраняет прежнюю семантику, bounded PURE complex patterns пока явно отклоняются.
- Benchmark targets используют существующий Criterion и `profiling` profile. Successful
  samples проверяют полный результат; rejection samples проверяют typed resource status.
  SDK default budgets и поднятые preparation budgets фиксируются раздельно.

Full correctness и backend parity: `cargo test --lib --no-default-features -- --test-threads=1`
— 701 PASS, 10 ignored, 0 failures; datastore namespace входит в этот полный executable.
Отдельные `graph_indexes_local_sync` и `graph_indexes_local_native_async` targets — по
3 PASS. SDK: 7 all-shortest + 20 analytics + 11 tensor tests PASS. Paired performance
evidence дополняется ниже; до получения полного evidence этап 26 не закрывается.

## 12. T10 performance evidence

Windows/MSVC, `profiling` (`opt-level=3`, `codegen-units=16`, без LTO), Criterion 10 samples.
Scalable runtime cycle: `V=E=N`, unit directed connectors, NODE projection. Preparation
задаёт явно `12 GiB / 1B work`; SDK defaults отдельно проверены и остаются `256 MiB / 100M`.
Значения ниже — median Criterion в ms; RSS — peak всего benchmark process, не scratch admission.

| Kernel / N                                   |    10K |    100K |       1M |
|----------------------------------------------|-------:|--------:|---------:|
| Capture, до удаления лишней owner conversion | 34.569 | 426.424 | 6383.891 |
| Projection                                   |  3.160 |  42.358 |  491.731 |
| Degree                                       |  0.876 |   9.564 |  123.634 |
| PageRank                                     |  0.593 |   9.382 |   89.516 |
| Eigenvector                                  |  0.744 |  11.058 |  120.987 |
| Katz                                         |  8.485 | 108.817 | 1252.228 |
| Directed triangles                           |  0.645 |   4.478 |   60.307 |
| Directed clustering                          |  4.005 |  33.925 |  496.046 |
| LPA                                          |  3.537 |  26.116 |  491.986 |
| ALL, depth<=4, единственный 3-hop path       |  1.999 |  21.112 |  337.827 |

- Runtime process peak RSS: 151,977,984 / 417,607,680 / 1,972,277,248 bytes.
  Raised-limit samples возвращают полный проверенный результат. Default capture 100K/1M
  получает resource error; снижение owner admission не объявляет эти defaults достаточными.
- Skewed star 10K выявил canonical-order-dependent quadratic intersection. Общий kernel
  теперь проверяет disjoint ranges и использует binary lookup для skewed degrees.
  До исправления adverse hub order исчерпывал 100M work; после оба порядка возвращают
  zero triangles/clustering: adverse triangle ~0.78 ms, clustering ~4.80 ms (central
  Criterion estimates). Процентное сравнение с failed sample не является acceleration claim.
- Harmonic/Brandes: complete samples 32/128/512; 10K исчерпывает 100M work явно.
  Directed Louvain 128: Int(1) ~0.25 ms, Float(0.5) ~8.06 ms, одинаковая modularity 0.8125;
  exact BigRational gain fallback имеет измеренный потолок, sampling не используется.
- Owner conversion без child больше не создаёт лишние RecordId copies. Capture 10K admission
  уменьшается 60,562,136→50,242,136 bytes; custom-limit latency change статистически
  незначимо. Это уменьшение conservative admission, а не утверждение о таком же снижении RSS.
- Paired Ready CSR cache и on-demand GraphRead SDK проверяют полные axes/connectors equality.
  RAM 10K ~79.82/17.94 ms; RAM 100K ~1108.8/207.55 ms; Local Sync 10K ~67.18/15.47 ms
  (central Criterion estimates). Rebuild ~62.76/934.50/223.19 ms соответственно.
  Cold Local reopen+forced read 10K ~220.26 ms, complete compatible generation.
- 1M RAM import complete (23.97 s); artifact build отклонён до publication по scratch
  4 GiB (4.17 s), FG-54F13. Peak процесса 5,131,718,656 bytes включает resident DB и
  owned expected tensor; такой sample не считается cache success или partial result.
- Local warm SDK reads используют immutable resident artifacts: physical page miss counters
  равны 0; WAL backlog 784 pages. После серии rebuild/reopen retained WAL segment —
  25,845,744 bytes. Backlog и размер файла не подменяются метрикой appended WAL bytes.

Logs/JSON: `target/stage26-perf-{10000,100000,1000000}*`, `stage26-perf-after*`,
`stage26-cache-*`, Criterion `target/criterion`.

## 13. Непрофильный paired gate

Baseline — production files из `git archive 0eb735d`, текущий checkout содержит также
сохранённые unrelated dirty changes. Оба executable собраны одинаковым `profiling` profile;
это сравнение всего checkout с HEAD, а не причинное доказательство стоимости только Stage 26.
Новый work в ordinary RETURN/DML call paths проверен по diff; конкретная причина превышения
gate этими замерами не установлена. Непроверенные изменения shared executor не вносятся.

`FG_STAGE_GATE_ROWS=2000` — штатный размер существующего `gql_stage_common` fixture.
Начатый 10K run остановлен после completed no-index sample (~15.89 s на mutation batch):
полная серия не была завершена и gate не считается пройденным. Штатный paired run completed:

| Workload             | Criterion change estimate |                 95% CI | Gate                        |
|----------------------|--------------------------:|-----------------------:|-----------------------------|
| RETURN 1, unpinned   |                   +17.39% |      +7.42% .. +27.43% | FAIL                        |
| DML no index         |                    +7.74% | +1.51% .. <br/>+14.25% | OPEN, threshold exceeded    |
| DML immediate index  |                    +5.80% |      -3.14% .. +16.13% | OPEN, no significant change |
| Standard index build |                   -13.49% |      -19.84% .. -6.42% | PASS для этого workload     |

RETURN повторён в порядке current/HEAD/HEAD/current с CPU affinity=1. Median pairs:
2.2377/2.2478 µs (-0.45%) и 2.3315/2.2766 µs (+2.41%). Последний run имеет outliers,
его slope estimate отличается от median. Это подтверждает чувствительность короткого
probe к условиям запуска, но не отменяет неуспешный первый gate и не закрывает DML gate.
Logs: `stage26-common-2k-{before,after}*`, `stage26-query-pin-*`; исходные estimates и
confidence intervals сохранены в Criterion. Полная regression matrix остаётся OPEN.

DML повторён отдельно без параллельной компиляции, CPU affinity=1, порядок
current/HEAD/HEAD/current, штатные 2000 rows. Point mean estimates:

| Workload        | Current / HEAD, пара 1, ms | Change | Current / HEAD, пара 2, ms | Change |
|-----------------|---------------------------:|-------:|---------------------------:|-------:|
| No index        |          306.462 / 306.923 | -0.15% |          315.041 / 313.763 | +0.41% |
| Immediate index |          660.544 / 641.106 | +3.03% |          645.808 / 643.015 | +0.43% |

Median pairs также внутри 5%: +0.29%/-0.25% и +2.10%/+0.56% соответственно.
Это положительное evidence для данного fixture при фиксированном CPU, не полная
10K/100K/1M regression matrix. Confidence intervals и outliers сохранены;
предыдущая unpinned серия не удаляется. Common executable не изменялся после
cold fix, который затрагивает только Local open и tensor restore, отсутствующие
в данном RAM workload. Logs: `stage26-dml-pin-*`, summary JSON и named Criterion
baselines каждого run. Общий performance closure по-прежнему OPEN.

## 14. Final tooling

`cargo test --doc --no-default-features` с filters `graph::tensor`, `graph::analytics`
и `GraphIndex::new_tensor`: PASS, 2/2/1 примеров. Tensor example охватывает full/NODE axes,
checked cells, Source/Target/Both, VV/VE/EV/EE и CSR/dense; analytics example — все kernels
и callback all-shortest. `RUSTDOCFLAGS=-C debuginfo=0 -C link-arg=/DEBUG:NONE` сохранён.
Final `cargo check --lib --no-default-features`: PASS, 210 существующих warnings.

Clippy после T10 helpers сначала выявил один новый `renamed_function_params`; параметр
`fmt::Write::write_str` приведён к имени trait contract. Последующий final lib Clippy:
6956 pre-existing errors, без новых signatures/count growth относительно T8 (6966).
Error comparison учитывает code/file/message, а не изменившиеся line numbers.
Full `just all-check` и Graphify update фиксируются после выполнения ниже.

`just all-check` выполнен с точным snapshot всех Rust source files. Первый запуск
выявил cargo artifact collision после сборки archived HEAD в общем target:
`fg-gql-status` ошибочно считался Fresh и давал 10 missing-constructor errors.
После `cargo clean -p fg-gql-status` current default-feature check PASS.
Следующий запуск дошёл до nightly formatter и завершился environmental Windows
OS error 1224 (mapped source file). Все изменённые formatter-ом files восстановлены
из snapshot, совпадение hashes подтверждено; unrelated dirty source сохранён.
Отдельный Clippy gate остаётся FAIL по существующему lint debt, не объявляется PASS.

## 15. Cold recovery: найденная ошибка и исправление

Расширенный 100K Local Sync probe успешно build/rebuild-ил cache, но forced lookup
после cold reopen получал unavailable binding. Малые recovery tests этого не выявили.
Найдены две независимые причины: `open_local` терял заданные maintenance limits и
использовал default 256 MiB; `TensorArtifact::restore` повторно резервировал encoding
work, уже учтённый build, и превышал default 100M. Исправлены передача существующего
scoped options context и удаление только второго reservation. Payload admission,
полное сравнение descriptor/metadata/cells и hard work cap сохранены.

Regression normal→1 KiB→normal: до исправления Sync FAIL (малый budget игнорировался),
после — Sync и NativeAsync PASS для CSR/DENSE/AUTO. Primary atom доступен при fallback;
обычное открытие снова восстанавливает durable Ready без промежуточных writes.
Свежий read-only review новых замечаний не выявил. Повтор scaled cold probe и
общих gates фиксируется ниже; первоначальный 100K run не считается успешным.

Расширенный 10K paired cache series до cold fix: RAM on-demand/Ready 73.386/14.792 ms;
Sync 79.504/15.858 ms, rebuild 198.57 ms, cold 220.72 ms;
NativeAsync 82.253/17.889 ms, rebuild 209.05 ms, cold 226.05 ms.
Single label mutation/rebuild: RAM 8.786/61.318 ms, Sync 124.566/221.365 ms,
NativeAsync 114.722/222.585 ms. Private/new readers отвергают прежнюю generation,
old snapshot сохраняет её; rebuild возвращает changed label.
Для 10K Sync и NativeAsync монотонные WAL segments дают 13,037,959 appended bytes
на mutation+rebuild. На 100K retirement нарушает монотонность, delta остаётся
неизмеренной (`None`), а не подменяется retained file size.

После исправления 100K cold recovery:

| Backend     | On-demand / Ready, ms | Rebuild, ms | Cold reopen, ms | Mutation / rebuild, ms |
|-------------|----------------------:|------------:|----------------:|-----------------------:|
| Local Sync  |       992.50 / 231.16 |      2173.4 |          2447.4 |        1366.5 / 2432.0 |
| NativeAsync |       1104.9 / 230.53 |      2471.5 |          2729.0 |        1559.1 / 2602.7 |

Оба probes завершены exit=0 с полным результатом. Это paired Ready/on-demand
сравнение, а не ускорение самого cold reopen относительно failed pre-fix run.
100K Sync/NativeAsync RSS 1,334,595,584 / 1,295,663,104 bytes.
Restore сохраняет default work cap 100M.
Logs: `target/stage26-cache-recovery-*`; actual options и admitted bytes/work
сохранены в stdout, process peak отдельно — в `*-rss.json`.

1M NativeAsync import завершён за 35.001 s; build за 5.353 s возвращает ожидаемый
FG-54F13 при 4 GiB scratch cap. Process peak 5,162,258,432 bytes включает resident
database и owned oracle. Benchmark exit=0 подтверждает именно checked rejection,
не Ready artifact или successful cold run. 1M cache acceleration gate остаётся OPEN.

После cold fixes full gate повторён: lib 701 PASS / 10 ignored (56.55 s),
SDK 7 ALL + 20 analytics + 11 tensor PASS, отдельные Sync/Native targets по
3 PASS. `cargo check --lib --no-default-features` PASS (210 existing warnings).
Logs: `target/stage26-recovery-{lib,sdk,sync,native,check}.log`.

Final tooling после cold fixes: current default-feature `cargo check` PASS
(211 warnings); `just all-check` вновь останавливается на formatter OS error 1224,
в этот раз `src/gql/ir.rs`. Два formatter changes восстановлены; все source hashes
равны pre-command snapshot (0 mismatches). Targeted nightly formatting сохранено.
Отдельный final Clippy: 6956 errors, новых code/file/message signatures или count
growth нет относительно T8 (6966). Logs: `stage26-all-check.log`,
`stage26-clippy-recovery-final.{jsonl,stderr}`, `stage26-clippy-recovery-delta.json`.

## 16. Незакрытые gates

- Bounded PURE complex regex COST: literal patterns работают, сложные patterns
  явно получают resource error до compiler allocation; полный bounded compiler
  contract пока не доказан. Обычная regex semantics вне bounded COST сохранена.
- Performance: успешная 1M cache серия и полная paired regression matrix не получены.
  Default resource rejections и CPU-pinned 2K probes не заменяют эти gates.
- Tooling: global Clippy lint debt и Windows mapped-file failure formatter-а
  сохраняются. `just all-check` выполнен, PASS не заявляется; dirty work сохранён.

Этап 26 остаётся в реализации. T1–T9 correctness и повторные lib/SDK/backend gates
подтверждены; статус общего завершения не повышается.

Final targeted nightly rustfmt проверил 22 tensor/analytics/helper/SDK/backend/bench
файла через stdout, 0 differences; source files не переписывались. `graphify update .`
завершён exit=0, AST-only без API: 55,261 nodes, 147,424 edges, 1,835 communities;
graph.json, graph.html и GRAPH_REPORT.md обновлены. Existing community labels
частично переименованы по hub; semantic relabeling не запускался. Logs:
`stage26-fmt-final-summary.json`, `stage26-graphify-update.log`.
