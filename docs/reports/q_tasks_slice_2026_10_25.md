# Q-задачи: сводный срез на 2026-10-25

**Срез проверен и готов к коммиту в обозначенных границах; коммит не создан.** Это основная
точка входа вместо накопленных Q-отчётов и handoff. Нормативные требования
остаются в [`full_plan.md`](../full_plan.md), `gql_spec.md` и
`disk_storage_spec.md`. Q-40 исключена из этой работы.

Работа нужна, чтобы небольшое изменение данных не требовало заново строить
обычные файлы всей базы и чтобы большой payload не обходил лимит транзакции.
Поддерживаемые изменения теперь готовят ограниченный набор страниц, обновляют
связанные индексы и публикуют согласованный результат одной WAL-транзакцией.
Проверялись также отказы: старые читатели сохраняют свой образ, а reopen
восстанавливает целиком старое или новое состояние. Это законченный
функциональный срез; вся Q-01 и performance acceptance Q-05 ещё открыты.

## Коммитный срез

Граница — **source310**, после mixed present→present leaf rebind с несколькими
независимыми detached Base-правками. Новый mixed child-presence 0↔1 + Base
не установлен: его RED/GREEN drafts относятся к следующему срезу.

Сводка охватывает накопленную работу Q-01/Q-03/Q-05, переаудит Q-02/Q-14 и
подготовительные исследования остальных Q. Она не приписывает этому срезу
чужие изменения большого dirty checkout и не объявляет их проверенными
отдельным изолированным коммитом.

Коммит должен включать связанные production modules, их tests и обновлённую
документацию. Семь semantic files последнего batch-шага **не являются**
самодостаточным патчем поверх HEAD: им нужна ранее добавленная point-publication
tree и value/budget helpers. Source manifest310 — проверяемая граница evidence,
а не команда staging. Companion JSON содержит368 dependency-review inputs,
включая Q-03 и publication helpers; это список для review, не автоматический
выбор целых файлов с чужими hunks. В существующем index есть посторонние staged/unstaged
изменения; `git add -A`, reset и clean здесь не применялись.

## Проверки и границы доказательств

HEAD: `a447d51290423cc7f98f92f84bc596d01df36981`, ветка `mvp`.
Stable Rust 1.98.1 используется для build/test/Clippy, nightly — для rustfmt.
Новые зависимости, production defaults и lint policy не менялись.

| Проверка                                         | Итог текущего среза                                                                      |
|--------------------------------------------------|------------------------------------------------------------------------------------------|
| Batch regression до исправления                  | RED3 по собственным markers, controls51 PASS                                             |
| Source310 focused matrix                         | 64 passed, 0 failed, 0 ignored; прежние48 и RED3 сохранены                               |
| Check, strict Native Clippy, `just all-check`    | PASS на том же source310; reused после проверки identities                               |
| Nightly fmt `--check`, scoped `git diff --check` | Свежие PASS после консолидации; первоначальный EOF whitespace отказ исправлен и сохранён |
| Actual source review и layout revalidation       | PASS; исходный Clippy101 сохранён в evidence                                             |
| Полный no-default Native library run             | 2266 passed, 0 failed, 11 ignored; names2277, focused64 и old492 сохранены               |
| Workspace build с обоими Native features         | PASS, root names2300; root runtime2288/0/12 ignored                                      |
| Workspace runtime и strict both-Native Clippy    | PASS: 2700/0/41 ignored, 29 непустых suites; strict Clippy `-D warnings` PASS            |
| Backend integrations                             | System122: RAM42 / Sync40 / actual NativeAsync40; prototype Native4 PASS                 |
| Graphify AST update                              | PASS: 64370 nodes / 172660 edges / 2072 communities, worker1, API false                  |
| Новые performance confirmation series            | NOT_RUN                                                                                  |

Batch matrix: 24 functional cells, 24 generation failures, 32 WAL recovery,
8 Base-only, 8 cumulative-resource, 8 second-source и 8 replay cells.
Проверяются Graph/Metagraph, NODE/EDGE, Sync/actual NativeAsync. Два Base records
могут находиться на одной Hot page: cross-page batch и mixed Unique swap
этим fixture не доказаны. Одна WAL publication может содержать много frames.

Последняя подтверждённая последовательность: preparatory refactor48 →
source305 RED3/control51 → source310 GREEN64/all-check/review/Graphify →
Native2266 → workspace2700 и strict both-Native Clippy. Source/archive/EXE связываются exact hashes; совпадение номера
entries у разных manifests не означает совпадение исходников.

Исторические compiler/lint/OOM/cancellation/INVALID результаты сохраняются.
Clippy BATCH-LAYOUT-CL001 исправлен побайтным переносом `batch_tests.rs` в
`batch_tests/mod.rs`; исключения lint не добавлялись. Перенос комментария,
исправление clock getter и усиление test oracle не скрывают исходные отказы.

## Q-01

### Реализованные группы сценариев

| Область                       | Результат и граница                                                                                                                                                                                           |
|-------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Base и scalar lifecycle       | Existing-key и key-set replacements, create/delete/reuse, полные generation-aware identities, selected Primary/Standard/Unique fallback; ordinary files сохраняют identity там, где layout поддерживает delta |
| Overlay, LABEL и dictionary   | Sparse Effective присутствие/удаление, Null/Hidden/RESET, label lifecycle и точные statistics; append/resize dictionary публикуется с данными                                                                 |
| NODE/EDGE memberships         | Global и graph-local create/attach/detach/reuse/transient, mixed NODE+EDGE/cascade, n-ary endpoints и multiplicity в поддерживаемых family shapes                                                             |
| Families и metadata           | Plain family membership, RootedTree и stable container metadata; family validation сохраняет собственные ограничения                                                                                          |
| Topology и qualified identity | Local/qualified rewire, cross-level routes, first EdgeEndpoint capability, stable endpoint headers и старые traversal snapshots                                                                               |
| Child graphs                  | Pure presence 0↔1, surviving-scope remap и directory chains; present leaf replacement, selected-owner routes, shared ancestors до двух уровней для Graph/Metagraph                                            |
| Mixed publication             | Structural/metadata combinations и present leaf rebind вместе с detached стабильными Base records; records сопоставляются slots через resolve/contains, а не zip                                              |
| Sources, resources и recovery | Borrowed value/DTO/codec admission, IANA/timezone authority, order keys, statistics/index output и initial context; source faults и лимиты отклоняются до durable publication                                 |
| Artifacts и history           | Cold binding recovery, исключение Stale/OnDemand, Ready clone требует rebuild; history-only/no-op сохраняет собственный publication contract                                                                  |

Все эти формулировки относятся к поддерживаемым shapes. Guarded full fallback
может быть корректным и атомарным, но его успех не доказывает sparse publication.
Latest broad matrix покрывает текущую интеграцию; исторические узкие серии
не суммируются в вымышленное число уникальных тестов.

### Что осталось

- DML класса с Fulltext/HNSW/DiskANN/Geometry ещё ограничен scalar-only
  `supports_point_indexes`, включая правки другого поля.
- Detach occurrence с собственным child graph и связанные cascade shapes.
- Mixed child presence + Base; rebind с overlays/memberships, новым dictionary
  key, несколькими incidence targets или дополнительным fanout.
- Nonleaf/изменяемый child, несколько child replacements, depth3+ replacement,
  presence под ancestors и прочие container families leaf writer.
- Остальные catalog/Mapper/derived resource consumers, не охваченные
  component oracles; узкие D7 closures не являются общим resource closure.
- Дополнительные cross-page/mixed-index combinations и свежая production
  performance matrix. Catalog reconstruction и часть структурных scans
  остаются отдельными измерительными вопросами Q-12.

## Q-03

Исправлен recursive accounting retained atom/catalog state: borrowed preflight,
owned journals/capacity, identities/properties/topology, derived adjacency и
актуальный replay fanout. Повторные правки одного target учитывают удерживаемое
состояние; rollback/savepoint/import восстанавливают counters. Label allowance
L-02 дополняет lookup/statistics sidecars; no-op label не превращается в payload
copy. Resource refusal не публикует WAL/root и сохраняет private state.

Итог Q-03 — **исправлено и проверено**. Исторический финальный catalog run:
library1390/0/10 ignored, workspace1745/0/33 ignored, Native rejection/reopen1/0,
`just all-check` и Graphify PASS. Позднейшие Q-01 suites включают label и
storage-resource regressions. Логический budget не является allocator quota
и не закрывает общий D7/COST долг materializing operators.

## Q-05

Primary/Standard/Unique leaf mutators работают в production. При стабильных
separators, page count и posting layout меняются затронутые leaves; иначе
выбранный binding перестраивается streaming fallback. Source validation,
generation-aware removal/reuse, snapshots и failure/recovery обязательны
для обоих путей.

Frozen v4c завершена 2026-10-18: **126 cells / 8064 distinct accepted processes;
92 PASS / 33 inconclusive / 1 FAIL**. Это результаты исходных artifacts,
не performance acceptance нынешнего source310.

| Backend                      | PASS | inconclusive | FAIL |
|------------------------------|-----:|-------------:|-----:|
| Windows Sync, 42 cells       |   27 |           15 |    0 |
| Actual NativeAsync, 42 cells |   29 |           12 |    1 |
| RAM, 42 cells                |   36 |            6 |    0 |

Matrix: семь binding/routes, batches1/32/256, stable/fallback, три backend.
На cell — calibration и confirmation по16 independent process pairs,
поровну AB/BA; process содержит8 warmup и32 measured requests. Estimator:
geometric mean paired process medians, bootstrap20000, seed104729, CI95.
Local stable требует CI upper B/A <1; fallback/RAM — upper ≤1,05.
Same-EXE calibration CI целиком в `[1/1,05; 1,05]`.

**FAIL47**: NativeAsync / Primary / Base / lifecycle / fallback / batch256.
B/A `1.148715149167092`, CI95 `[1.098625944958087; 1.1913884985696774]`;
calibration PASS `[0.9894498881087035; 1.008503420330596]`. Причина **UNKNOWN**.
Candidate действительно сначала проверяет leaf delta, затем при unstable
layout строит selected Primary. Наличие этой работы — гипотеза, а не attribution;
source validation нельзя убрать ради красивого timing.

**Inconclusive** означает, что данных недостаточно для PASS: у28 cells
calibration не прошла, у5 основной CI пересёк gate при прошедшей calibration.
Эти пять ordinals: `9/21/33/58/90`. Inconclusive не является ни PASS,
ни доказательством отсутствия регрессии. Старые samples не дописываются.

Для продолжения: сначала пригодный capture original EXE/PDB и commit subtree.
Предыдущие VTune collections INVALID: неполные rows, no ACK/corrupt trace
и смешанный pause scope. WPR/WPA capability/capture пока NOT_RUN; collection
требует40 iterations, reopen, ACK/exit0, symbols/loss diagnostics и прежний
600s limit. Затем — минимальный candidate после attribution и новая заранее
объявленная confirmation/calibration series. Cargo/Graphify/profilers не
совмещаются с timings; старые baseline identities и margins сохраняются.

Frozen output: `target/q-debt/q05/matrix-v4-full-v4/completion.json` и
`progress.json`. Progress SHA256:
`5bc04f37828853456fada3a798fb1e9275c6829bdbb528dfda869bcaa293315c`;
plan SHA256 `c695cc831c8ecd06e9e3ccb542d473b8ff758f9e390798a1b453364ce7872d5d`.
Original EXE/PDB — `target/q-debt/q05/artifacts/matrix-v3-checked-{control,candidate}`;
raw, source snapshots, runtime и partial exclusions остаются неизменными.

Восемь frozen протоколов сохранены побайтно: full matrix
[v2](q05_full_matrix_protocol_v2.md), [v3](q05_full_matrix_protocol_v3.md),
[v4](q05_full_matrix_protocol_v4.md), continuation
[v2](q05_matrix_continuation_protocol_v2.md), [v3](q05_matrix_continuation_protocol_v3.md),
[v4](q05_matrix_continuation_protocol_v4.md), [v4b](q05_matrix_continuation_protocol_v4b.md),
[v4c](q05_matrix_continuation_protocol_v4c.md). Это hash-checked inputs tooling,
а не дублирующие narrative reports.

## Остальные Q-задачи

| Q                            | Состояние и следующая конкретная работа                                                                                                                                                                                                                                                                                                                                                              |
|------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Q-02                         | Закрыта ранее: graph-first SDK подтверждён; удалённые compatibility aliases не восстанавливать                                                                                                                                                                                                                                                                                                       |
| Q-04/06/07/08/10/11/13/15–20 | Сохраняют закрытие реестра; прежние performance-цифры исторические. Этот срез не объявляет их заново измеренными                                                                                                                                                                                                                                                                                     |
| Q-09                         | Только discovery. Нужны database UUID/published provenance, per-atom accepted baselines, prerequisites/requirements на всех isolation levels, окончательный acceptance bundle и Mutable-only `flush_to` для восьми families. Independent edits разрешены; clean B не обновляет baseline после flush A; failure сохраняет dirty, retries нет, Frozen не получает API                                  |
| Q-12                         | Все пять исследований OPEN: singleton WAL opens, multi-consumer IoRing, catalog growth, linear publication lookup, LiveGraph small-delta/large-graph. Есть source audit catalog/lookup; baseline/attribution NOT_RUN. Clone `prepare_index_reuse` относится к full fallback; retained point reconstruction/encoding измерять отдельно                                                                |
| Q-14                         | Прежний массовый lint debt не воспроизводится; закрыта ранее. Каждый срез сохраняет fmt/check/strict Clippy без ослабления policy                                                                                                                                                                                                                                                                    |
| Q-31a                        | V11–V13: vector correctness/recovery, N10K/100K/1M × D32/128/768 × K1/10/100, breadth/distributions/scopes/states, maintenance DML, cache pressure. Разделить builder peak/HNSW residency/DiskANN query RSS по процессам; recall@10≥0,95, ANN≥3×exact при N≥100K                                                                                                                                     |
| Q-31b                        | D7–D11: admission до allocation и growing temporaries/queues/terminal copies, bounded complex regex, exact PATTERN_CACHE rows/descriptor/multiplicity/paths/qualified identities, invalidation/old snapshots/private writes, revision-checked publication, consumers/EXPLAIN/counters и differential/performance matrix                                                                              |
| Q-32                         | Только discovery: aggregate ALTER IR/candidate, duplicate/conflict rejection, one catalog revision; каждый непосредственно изменённый объект увеличивает SchemaVersion один раз. Нужны bounded CLASS/FIELD validation, GRAPH family/root validation, symmetric indexes, migration/CONCURRENTLY через coordinator. Q32-CLASS-FCW-001 и Q32-EVENT-DUP-001 пока source findings, runtime RED не получен |
| Q-33                         | C10 changefeed matrix → LIVE L11 → Stage29 events/LiveGraph closure с confidence и same-EXE calibration. Не использовать INVALID Stage29 baseline и не подменять исходный acceptance gate удобным новым baseline                                                                                                                                                                                     |
| Q-39                         | После Q-05: near-dense/bounded reserve; сначала page-set≤1,05×dense, затем import→mutation→reads→checkpoint/reopen; mutation gain≥10%, read p99≤1,05×control. Без скрытого FILL_FACTOR clamp; отрицательное решение допустимо                                                                                                                                                                        |
| Q-41                         | T10: воспроизвести1M cache rejection, найти retained memory, добиться Ready без повышения defaults, equality с on-demand oracle, mutation/rebuild/cold reopen и полная kernel matrix. Корректный resource rejection не закрывает acceleration gate                                                                                                                                                   |
| Q-42                         | После Stage29: раздельно independent/conflicting handlers и due/worker/execution/commit lag; concurrency2/4/8 только после attribution и gain≥10%. Сохранить identity, atomic marker/effects, RETRY/MAXDEPTH, budgets, KILL, recovery и fairness; отрицательное решение допустимо                                                                                                                    |

У Q-09 важны три success-точки commit feedback: readonly, no-write и mutation.
Snapshot timestamp и наличие physical handle сами по себе не доказывают
published provenance. Все fallible acceptance operations завершаются до WAL.
Предполагаемый datastore-owned API —
`MutableGraphWriteBackExt::flush_to(&mut self, source: &Datastore) -> FerrosGrynnResult<CommitResult>`;
низкоуровневый `flush(&mut Transaction)` сохраняется. Восемь families:
Graph/Forest/Tree/RootedTree/Dag/Hypergraph/Metagraph/Subgraph. Empty dirty
проходит provenance и обычный empty commit без новой WAL publication;
typed unsupported graph-property write-back сохраняется.

У Q-32 `publish_class_replacement` не регистрирует `SchemaClass`/
`ClassCatalogRange`, а `ReplaceClass` replay не проверяет expected version;
duplicate EVENT actions требуют отдельного preflight regression. Owned
candidate нужен до private mutation: одного `atomic_mutation` при
отключённом rollback недостаточно. Эти source findings ещё требуют runtime RED.

Принятые пользователем условия остаются в силе: лимиты числа optimization
attempts Stage28/29 сняты, статистические acceptance gates не ослаблены.
Независимые ASYNC handlers могут завершаться в разном порядке; внутри causal
chain порядок сохраняется. Для Q-12/Q-39/Q-42 допустимо обоснованное решение
оставить нынешний код; новый механизм требует подтверждённого выигрыша≥10%.

## Точка продолжения

1. Текущая точка — проверенный source310. Коммит не создан; новые source
   changes начинаются следующим отдельным продолжением. Завершённые Native,
   workspace и all-check без drift повторять не требуется.
2. Следующий Q-01 unit: standalone Graph NODE sole child0↔1 + detached Base
   count1/2. Ancestors, EDGE, surviving scopes и selected routes пока fallback.
3. RED publicV4/privateV3 reviewed, **NOT_INSTALLED/NOT_RUN**. Installer V4
   требует полный batch broad-v4 PASS; ожидаются source312/names2286/RED6/control67.
4. GREEN V4 восемь drafts имеют STATIC PASS. Исправлен test-oracle finding
   PRESENCE-REPLAY-ORACLE-001: third Base проверяется сразу в live root после
   commit. Ожидаются source317/names2296/focused83; compile/Clippy/runtime
   ещё не выполнялись. Нельзя наследовать STATIC verdict как runtime PASS.
5. После реализации — focused checks, actual review, all-check и Graphify;
   затем оставшиеся Q-01 shapes и свежая Q-05 acceptance по утверждённым gates.

Артефакты продолжения находятся в `target/q-debt/q05`: batch
`q01-mixed-base-batch-green-v3-{source,identity,closure}.json`,
`q01-mixed-base-batch-broad-v4-*`, presence RED installer V4/runner V1 и GREEN
V4 draft proof/review/installer/runner V1. Exclusive logs старых попыток
не перезаписывать. Перед запуском сверять HEAD, dirty, source и EXE identities.

Для сборок использованы process-only settings: jobs1, incremental0,
dev/test debug0, dependency test CGU256, root test CGU1024,
`CARGO_TARGET_DIR=target/q-debt/q01-cgu256`. Память admission5,25GiB и live
reserve512MiB — локальная защитная схема, не allocator quota и не доказанная
экономия компилятора. Отказы NOT_STARTED/WAIT_EXPIRED не являются compile FAIL.
