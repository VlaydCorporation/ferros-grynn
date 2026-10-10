# Этап 28 — evidence report

Дата начала: 2026-10-04. Baseline `196d21b0122dd1410752d04a37d7174b116ab228`.
Статус: **OPEN**; нормативный контракт — [план](../gql_live_stage_28_plan.md).

## Актуальный статус — 2026-10-10

Новый лимит оптимизаций исчерпан: **5/5**. Дальнейшие оптимизации прекращены;
фиксированная evidence-матрица и итоговая документация завершены.
Исторические счётчики ниже описывают состояние соответствующих срезов.

| Попытка     | Изменение                                               | Итог                                                                                                                                                     |
|-------------|---------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------|
| C28-15, 1/5 | Вычисление только новой подписки при чистой регистрации | Сохранено; прежний RAM 100K/100 subscribers timeout 900s устранён, тот же probe завершился за 217.75s; correctness gates PASS                            |
| C28-16, 2/5 | Специализация компаратора revision keys                 | Откат; четыре независимые пары не доказали стабильный выигрыш, один conservative upper ratio 1.05540                                                     |
| C28-17, 3/5 | Уменьшение размера AST через boxed LIVE SELECT          | Откат; 10K RETURN mean +6.02%/+8.84%, upper ratios 1.12684/1.14818                                                                                       |
| C28-18, 4/5 | Принудительный inline read-tracking wrapper             | Откат; смешанные результаты, отдельные RETURN/DML upper ratios выше 1.05                                                                                 |
| C28-19, 5/5 | Исключительный доступ к read set через Mutex::get_mut   | Сохранено; все 6 сравнений с C28-15 проходят 5% non-regression; point-read means −5.6%/−0.94%, второй CI касается 1, универсальный speedup не заявляется |

Сохранение отдельной оптимизации не означает прохождения общего L11 gate.
Финальный default runner №6 завершил **72/72 cells**, без timeout:
30 Measured, 28 CapacityGap, 8 VerifiedImportProbe, 6 VerifiedResourceProbe.
Общий paired gate против исходного `196d21b`: **5/9 PASS**, verdict
**FAIL_OR_INCONCLUSIVE**. Не прошли 10K RETURN и point read; 100K RETURN и
1M point read остаются inconclusive по conservative confidence bounds.
Ограниченные budget probes не выдаются за успешные latency measurements.
Artifacts: `c28-final-run6-summary.json`, `c28-final-run6-common-interim.json`.

| Rows | RETURN literal | Point read ×16 | Mutate ×64 |
|------|----------------|----------------|------------|
| 10K  | FAIL           | FAIL           | PASS       |
| 100K | INCONCLUSIVE   | PASS           | PASS       |
| 1M   | PASS           | INCONCLUSIVE   | PASS       |

PASS требует conservative after-upper/before-lower ratio не выше 1.05;
снижение среднего при широком CI само по себе gate не закрывает.

Final source correctness: **1165 passed /10 ignored /0 failed /0 filtered**;
SDK/backend targets: RAM LIVE **10/10**, Sync+NativeAsync LIVE **38/38**, общий
local **16/16**. Production no-default check и scoped nightly format PASS;
Clippy delta к baseline **0**. `just all-check` выполнен: default check и format
PASS, global Clippy остаётся pre-existing FAIL (**6989 errors/537 warnings**).
После formatter восстановлены четыре несвязанных dirty файла; проверены
**646/646** защищённых Rust files, `changed_after_restore=[]`.

Дополнительная неизменяемая large-work матрица 100K/1M, RAM/Sync завершена:
**24/24 MeasuredWithOracle**, по10 samples, без timeout/failure/GAP/faults,
с заранее заданными deadlines и без retries. Она использует явный
100M work budget и не заменяет default-budget или paired regression gate.
Latency NativeAsync и OS/device-cold состояние этой матрицей не проверяются;
backend correctness и engine-reopen evidence учитываются отдельно. Scratch
counter отражает admission, а process peak RSS приводится отдельным измерением.
Незакрытые performance и global tooling gates сохраняют этап 28 **OPEN**.

### Fixed large-work matrix — финальная сборка C28-19

Явный профиль: work100M, scratch256MiB, default queue10000/64MiB; 10 samples,
warmup0.5s, measurement target2s. Крупные commits требуют больше целевого времени;
deadline900s, для1M/100 subscriptions — заранее установленный3600s. Без retries.
Timer охватывает source/control commit с LIVE evaluation и publication;
регистрация, oracle и ACK находятся вне timer. Ниже mean в секундах на commit.

| Rows | Case               | RAM mean, s | Local Sync mean, s |
|------|--------------------|-------------|--------------------|
| 100K | scan_1             | 0.172       | 0.476              |
| 100K | FETCH              | 0.178       | 0.567              |
| 100K | LIVE LET/data      | 0.184       | 0.503              |
| 100K | LIVE LET/parameter | 0.164       | 0.168              |
| 100K | scan_10            | 1.576       | 2.225              |
| 100K | scan_100           | 17.378      | 18.450             |
| 1M   | scan_1             | 2.226       | 5.391              |
| 1M   | FETCH              | 2.413       | 5.600              |
| 1M   | LIVE LET/data      | 2.288       | 5.449              |
| 1M   | LIVE LET/parameter | 1.599       | 1.964              |
| 1M   | scan_10            | 17.504      | 20.886             |
| 1M   | scan_100           | 196.677     | 199.807            |

Проверены24/24 cells: все MeasuredWithOracle, по10 samples, exact before/after,
canonical identity ordering, whole-commit stamp и idempotent ACK; faults/GAP0.
Max admitted scratch438514 bytes и max sampled process peak RSS1357.64MiB.
Эти absolute measurements не являются paired speedup claims.
CI, process deadlines/exit/hash, before/after work/history/page-read counters и
файловые размеры сохранены в `c28-final-large-work-summary.json` и
индивидуальных `c28-final-large-work-{local}-{rows}-{case}/` artifacts.
`wal_bytes`/`feed_bytes` — наблюдаемый размер файлов, не cumulative I/O traffic.

Последний Sync1M/100 subscriptions: mean199.807s, CI190.206–210.506s;
process2702.86s при deadline3600s. RAM counterpart: mean196.677s,
CI188.276–205.797s, process2547.74s. По1200 batches/76800 events и1200 ACK,
без faults/GAP; Sync history page reads433000. Это разные backend workloads,
не before/after optimization pair.

Final source freeze подтверждён после всех замеров: **627/627 source inputs**
и **646/646 protected Rust files** совпадают с C28-19 snapshots, changed=[].
Artifact `c28-final-source-preserved.json`; LIVE EXE SHA-256
`6DD2EA536362E2BE85A6EE6746BA97D7F0C8BCDAE7C8BB50572CE81751E34010`.

Final AST-only `graphify update .` PASS:58472 nodes/157729 edges/1923
communities (`c28-final-graphify.log`), без LLM/API cost. Два прежних JSON inputs
с zero nodes отмечены tool warning; они не считаются новым production defect.
Closure evidence завершено; **stage28 OPEN**, **source optimizations STOP5/5**.

## Возобновление — 2026-10-09

Историческое решение: пользователь снял прежний лимит попыток. Позже введён
новый лимит пяти оптимизаций; на момент C28-15 счётчик был **1/5**.
Этап28 продолжается.
Prior attempt1/2 artifacts и counts сохраняются как история; они не являются
условием остановки. Следующий срез — measured ACK/control и import optimization.

## C28-4 — measured ACK revision COW (2026-10-09)

Installed VTune2026.4 user-mode hotspots capture на exact symbolized profiling
EXE/PDB разрешил Rust functions/source. На100K RAM внутри ACK subtree
`DatabaseRoot::mark_version → Arc::make_mut → BTreeMap::clone` занимает70.98%
sampled CPU (warmup56.18%). Capture включает mutation setup; attribution берётся
только из ACK subtree. Profiling opt3/ltofalse/cgu16/debug2 не смешивается с
latency opt3/fatLTO/cgu1/debug0. Artifacts: `c28-4-profile-pair.json`,
`c28-4-vtune-ack100k.log`, `c28-4-ack-hotspots.csv`, `c28-4-ack-topdown.csv`,
`c28-4-ack-profile-analysis.json` в `target/stage28-baseline/`.

Сначала mechanical extraction `VersionStore` с прежним deep Clone: no-default
check PASS, existing import revision admission characterization1/1 PASS.
Затем private domains primary/LIVE получают независимые inner Arc BTreeMap;
обычный DatabaseRoot clone сохраняет один outer handle. Только LiveDefinition,
LiveBinding и LiveCatalogRange относятся к controls; SchemaClass override,
graph tensor revision, FCW/SSI keys и physical codecs сохранены. Import сравнивает
точное primary содержимое, допускает COW до allocation, restamp-ит provisional
primary values, переносит committed controls без restamp. Retention допускает
два дерева. Selective check PASS; independent source review PASS.
Selective full lib gate **1147 passed /10 ignored /0 failed /0 filtered**,
103.77s; все3 reference/snapshot/COW regressions, import admission/retry/ACK,
FCW/SSI и retention PASS (`c28-4-selective-full-lib.log`). Paired ACK
remeasurement выполняется; speedup пока не заявляется. Exact прежние EXEs/hashes сохранены в `c28-4-before/binaries.json`.

### C28-4 isolated ACK measurements

Current identical optimized build PASS13m08s; previous binaries retained with
hashes. Criterion mean, fresh before/after,10 samples:

| Rows | RAM before  | RAM after | Speedup |
|------|-------------|-----------|---------|
| 10K  | 132.459µs   | 26.878µs  | 4.93x   |
| 100K | 1321.751µs  | 40.666µs  | 32.50x  |
| 1M   | 14205.870µs | 46.840µs  | 303.29x |

Conservative after-upper/before-lower ratios0.276/0.0362/0.00392 подтверждают
снижение. Whole-batch images/stamp и idempotent ACK oracle проходят в timed
fixtures. Samples и hashes: `c28-4-ack-paired/`, `ram-comparison.json`.
Sync mean before/after2.374/2.220ms,23.646/24.224ms,700.202/662.786ms;
conservative upper ratios1.460/3.224/1.0446. Sync speedup не заявляется:
10K/100K intervals inconclusive,1M не доказывает уменьшение по conservative CI.
Это isolated control improvement, **не полный L11 PASS** и не новый full gate run.

Exact symbolized Sync100K own-process capture показывает внутри ACK8.7072% всего capture CPU:
auto-checkpoint7.9401% (**91.19%**), retired-generation cleanup0.0950% (**1.09%**).
Checkpoint переносит ранее накопленные WAL frames в home files; source mutation
не входит в timed ACK, но его accumulated checkpoint work оплачивается ACK.
Гипотеза record-wide Value context priming отклонена: helper обходит только
schema graph properties, parameters и analyzers. Next slice проверит measured
backfill frame read/open/close seam; checkpoint semantics/thresholds не меняются
без отдельного доказательства. Artifacts `c28-4-vtune-sync-ack100k.log`,
`c28-4-sync-ack-topdown.csv`, `c28-4-sync-profile-analysis.json`.
Graphify AST PASS58404 nodes/157524 edges/1923 communities (`c28-4-graphify.log`).

C28-4 final scoped nightly fmt PASS. Clippy нашёл один новый redundant_pub_crate
на private VersionStore; visibility сужена до datastore для type и root field.
Final delta0, baseline6951 errors/544 warnings (`c28-4-final-clippy-delta.json`).
Dedicated LIVE SDK RAM10/10, Sync+NativeAsync36/36 PASS
(`c28-4-live-sdk-backends.log`); прежний live_ filter на общих targets выбрал0
тестов и **не считается evidence** (`c28-4-sdk-backends.log`). Visibility-only
repair после optimized build отмечен отдельно: binary source manifest сохранён;
производительность измеряла semantic COW patch до этого visibility change.

## C28-5 — bounded checkpoint segment reuse

Measured Sync profile обнаружил повторные `File::open`/close в frame backfill.
Сначала извлечён прежний seek/exact-read/page/LSN validator без изменения
поведения:27/27 WAL tests PASS (`c28-5-mechanical-wal.log`). Затем backfill
получил private WAL read scope: один cached last segment, shared prune guard,
File-before-guard Drop и close-before-open switch. `read_frame_from` остаётся
единственным validator. Target/frame order, home sync, checkpoint threshold,
CheckpointEnd/superblock и pruning сохраняются; второй worker/cache registry
или codec не добавляются.

Focused29/29 WAL tests PASS (`c28-5-scope-wal.log`), включая physical same-handle
reuse после rename, reverse seek, cross-segment switch, page/LSN/checksum/short
read и error Drop. Independent source review PASS: scope не захватывает writer/
published locks и завершается до checkpoint publication exclusive prune gate.
Full lib **1149 passed /10 ignored /0 failed /0 filtered**,107.47s
(`c28-5-full-lib.log`). Public backend/lint/performance evidence выполняется;
скорость нового checkpoint scope пока не заявляется.
Public suites PASS: LIVE RAM10/10, LIVE Sync+NativeAsync36/36 и общий local
checkpoint/reopen16/16 (`c28-5-backends.log`). Scoped nightly fmt PASS.
Clippy после let-chain cleanup и удаления мёртвого singleton wrapper: delta0,
6951 errors/544 warnings прежнего baseline (`c28-5-final-clippy-delta.json`).
Два metadata-superblock callers используют временный тот же scoped reader;
validator/guard lifetime сохраняются. Final checkpoint-consumer gate11/11 PASS
(`c28-5-final-checkpoint-tests.log`), включая concurrent commit/backfill и pruning.
Pre-scope optimized hashes сохранены в `c28-5-before/binaries.json`.
Final optimized build PASS13m18s (`c28-5-release-build.log`). Fresh paired Sync
ACK mean:10K2.255→2.235ms,100K24.257→21.850ms (wide CI, inconclusive);
1M664.888→392.555ms (**1.69x**), conservative after-upper/before-lower0.646
подтверждает снижение. Samples/hashes/oracles: `c28-5-ack-paired/`.
Полный L11 по этому isolated improvement не закрывается.
Final narrow cleanup review PASS. Graphify final AST PASS58414 nodes/157565
edges/1896 communities (`c28-5-final-graphify.log`); новые Rust sources
зафиксированы в `c28-5-source-manifest.json` (602 files).


## C28-6 — measured import replacement hypothesis

RAM importOFF100K own-process symbolized capture35s (collector exit4 после
bounded stop; **не completed import oracle**) относит98.44% capture CPU к
import_probe. Внутри import class lookup clone55.61% capture +IndexStoreRoot
drop39.44% capture =**96.55% import subtree**. Fixture populate0.93% отдельно;
attribution не переносит setup в import. CSV uses indented quoted generic Rust
names: стандартный parser требует skipinitialspace=True; CPU columns normalized
Total100 и не являются seconds. Artifacts `c28-6-vtune-importoff100k.log`,
`c28-6-importoff-topdown.csv`, `c28-6-import-profile-analysis.json`.
Source scout подтвердил только2 prepare/apply consumers; empty-plan guard
сохраняет fallible unique validation и staged nonempty index replacement.
Accessor no-default check PASS. First behavioral focused3/5: two index-Arc
sharing assertions failed after public commits. Debugging traced unconditional
COW in mark_deferred_indexes_stale (constructor fixtures Immediate verified),
not an omitted real key replacement. Added exact no-deferred preflight and
no-newly-stale index guard; repeated Stale IDs/version tuples still notify
catch-up. New4 tests +existing2 =**6/6 PASS** (`c28-6-fixed-index-tests.log`):
base/overlay values+statistics, sharing, Count, BASE/EFFECTIVE key movement,
masked inheritance, unique and late statistics errors, repeated invalidation.
Nonempty index updates retain staged preparation before primary mutation.
Full lib**1153 passed /10 ignored /0 failed /0 filtered**,100.03s
(`c28-6-full-lib.log`). LIVE RAM10/10, Sync+NativeAsync36/36, general local16/16
PASS (`c28-6-backends.log`). Final source review PASS after deferred guard fix.
Existing deferred/reopen/SSI/import gates included. Scoped nightly fmt PASS, final Clippy delta0
(6951 errors/544 warnings existing baseline; `c28-6-clippy-delta.json`).
Paired import gates выполняются; speedup не заявляется. Exact baseline binaries сохранены
в `c28-6-before/binaries.json`; source manifest602 files — `c28-6-source-manifest.json`.

C28-6 optimized build PASS13m15s. Complete RAM import probes
(`c28-6-import-probes/summary.json`):100K OFF0.606s success, ON1.885s expected
capture resource rejection;1M OFF10.122s success, ON12.069s expected rejection
(cap268435456). Both success and rejection run primary/clock/whole-result
oracles; resource rejection publishes no partial source/LIVE history. Old1M
900s timeouts устранены для этих RAM probes. Старый100K paired baseline timing
не повторялся в этом isolated probe: численный before/after speedup не заявляется.
Sync будет проверен full runner. AST Graphify PASS58422/157592/1918.
Full paired L11 run3 **Completed**,72 cells/18m34s:30 Measured,28 CapacityGap,
8 VerifiedImportProbe,6 VerifiedResourceProbe,**0 TimedOut**. Common**5/9 PASS**
(all3 mutate64 improve,10K/1M point16 pass;100Kpoint upper1.051 borderline).
RETURN1: mean1.307x10K/1.273x1M;100Kmean0.958x but conservative CI1.134,
all3 criteria not passed. Overall **FAIL_OR_INCONCLUSIVE**. Source defaults/caps
unchanged. Full ledger/hashes maintained; `c28-6-run3-summary.json`,
`c28-6-common-interim.json`. Next measured slice — disabled-LIVE literal path
symbolized profile. Source scout confirmed empty BindingTypes allocations,
unconditional control/commit timestamps and control clone; causal attribution
not yet established. Old profiling EXE/PDB retained in `c28-7-before/profile`.
Stage28 remains OPEN; no performance attempt cap.

## C28-7 — disabled-LIVE empty scope work

Symbolized common build PASS12m47s, exact EXE/PDB pair saved; own-process
RETURN1 capture/report succeeded. Normalized CPU clocks under with_runtime1.143%
and commit0.653%, transient BindingTypes default0.111%, constructor malloc0.274%.
Эти subtrees являются candidates; полного30% attribution по ним не заявляется.
Artifacts `c28-7-profile-pair.json`, `c28-7-return-topdown.csv`,
`c28-7-return-hotspots.csv`, `c28-7-return-profile-analysis.json`.

Control.started сохранён: named TIMEOUT refinement обязан считать от original
origin. Lexical ownership сначала encapsulated без behavior change/check PASS,
затем default entries стали private lazy Option<Arc>. Declarations/overlay
сохраняют full admission, rich-type sharing, shadowing и reactive provenance.
Readonly commit не берёт LIVE clock при пустых mutations+target intents; всякий
possible publisher, включая concurrent final FULL policy, держит прежний
entry-time BLOCK origin across retries. Source review PASS; lexical3/3 PASS
(`c28-7-bindings-tests.log`). Full lib**1154 passed /10 ignored /0 failed /0 filtered**,107.80s
(`c28-7-full-lib.log`): FULL/final-policy, BLOCK/cancellation/deadline/readonly
included. LIVE RAM10, Sync+NativeAsync36, general local16 PASS
(`c28-7-backends.log`). Scoped nightly fmt PASS, Clippy delta0 (6951 errors/544 warnings previous baseline;
`c28-7-clippy-delta.json`). Paired common speedup still pending. Source602 files
recorded in `c28-7-source-manifest.json`.
Exact pre-fix optimized binaries saved in `c28-7-before/binaries.json`.
Graphify AST PASS58426 nodes/157609 edges/1905 communities (`c28-7-graphify.log`).
Current optimized build/isolated common subset выполняются; full L11 not passed.

C28-7 optimized build PASS13m13s. Isolated common subset completed:
**4/9 PASS** (`c28-7-common-paired/comparison.json`). Mutate64 all3 and1Mpoint16
pass;10KRETURN mean1.184x/CI upper1.352,100Kmean0.924x/upper1.107,
1Mmean1.018x/upper1.151. Gate not passed; mean/CI disagreement not hidden.
Fixed-process affinity diagnostic logical CPU0, current/base/base/current:
current/base1.130x and1.145x, conservative uppers1.134/1.154.
Default host power/priority settings untouched; actual masks and hashes stored
in `c28-7-affinity-diagnostic/runs.json`. Core performance class not assumed.
This confirms residual ordinary literal cost under controlled scheduling,
not merely a noisy gate. Next step baseline196d21b readable-symbol call-tree
comparison; current-source candidates do not count as full attribution.

## C28-8 — statement control ownership / readonly root

Baseline196d21b exact common symbol build PASS18m15s; own-process capture and
report PASS. Fixture/profile matched; source-resolved current helper control
clone subtree1.258% capture absent baseline. Readonly source route currently
loads observed root then locked current; baseline performs locked root only.
Normalized profile percentages do not prove latency by themselves. Exact pair/
comparison: `c28-8-base-profile-pair.json`, `c28-8-base-return-topdown.csv`,
`c28-8-profile-comparison.json`.

Mechanical mutable-borrow signatures retained clones/check PASS, then helpers
borrow caller-owned per-statement control. Inferred access applies only to
implicit/autocommit; explicit stays restricted. Named timeout refined control
is used through export/ACK/continuation/commit. Sole post-helper scalar use is
nonce cleanup; original owner lifetime preserved. True readonly (empty mutations
AND target intents) skips LIVE preparation loop and validates one current root
under commit mutex with original interruption checks. Every possible publisher
stays on previous retry/BLOCK path. Source review PASS.
Full lib**1154/10 ignored/0 failed/0 filtered**,103.20s PASS
(`c28-8-full-lib.log`). LIVE RAM10/10, Sync+NativeAsync36/36 и general local16/16 PASS
(`c28-8-backends.log`); scoped nightly fmt PASS, Clippy delta0 (6951 errors/544
warnings pre-existing; `c28-8-clippy-delta.json`). Paired measurements pending;
no speedup claim. Old optimized binaries retained `c28-8-before/binaries.json`.

C28-8 optimized build PASS13m10s. Controlled CPU0 current/base/base/current
RETURN10K diagnostic still fails 5%: mean ratios1.06447 and1.06305,
conservative uppers1.09976/1.07386. Actual affinity1, full exit0/hashes/raw
estimates retained in `c28-8-affinity-diagnostic`. No full performance success
or complete causal attribution claimed.

## C28-9 — пустой ACK и metadata admission

Последнее изменение касается только пустых accounting owners. `prepare_live_acks`
при отсутствии ACK возвращает пустой Vec и None; прежний пустой loop не выполнял
проверок, reserve(0) всегда допускался. Для непустого ACK сохранены authority,
control, checked byte admission до clone и lifetime owner через publication.
Metadata BarrierAccount создаётся только для прежнего metadata-only selector
INFO LIVE/INFO LIVE LET/KILL/ACK. Все его pre-copy проверки и error semantics
сохранены. Общий BarrierMemory, deadline origin и public API не меняются.
Mechanical Option refactor check PASS, scoped nightly fmt PASS; source review
PASS. Полный lib **1154 PASS/10 ignored/0 failed/0 filtered**,85.55s
(`c28-9-full-lib.log`). Отдельные targets: LIVE RAM **10/10**, local LIVE
**36/36** (Sync+NativeAsync), general local **16/16** PASS
(`c28-9-backends.log`). Clippy delta **0** относительно C28-8, прежние
6951 errors/544 warnings (`c28-9-clippy-delta.json`). Release rebuild и paired
measurement ещё выполняются; ускорение не заявлено. Прежние binaries и hashes
сохранены в `c28-9-before`. Source manifest:626 files
(`c28-9-source-manifest.json`); Graphify AST PASS58427 nodes/157612 edges/1894
communities (`c28-9-graphify.log`). Scoped diff whitespace check PASS.
Общий L11 остаётся OPEN; лимит попыток снят.

C28-9 release build PASS11m02s (`c28-9-release-build.log`). Fixed CPU0
RETURN10K current/base/base/current завершён, all exit0/actual affinity1.
Первая пара mean1.02665/upper1.03741 **PASS**; вторая mean1.07815/upper1.10477
**FAIL**. Смешанный результат не считается закрытым gate; raw estimates,
hashes и timings сохранены в `c28-9-affinity-diagnostic`. Новый opt-in
large-work workload проверит registration/commit capacity отдельно от defaults.

## C28-10 — отдельный large-work workload

Все24 default100K/1M scan/FETCH/LIVE LET cells из run3 отклонены именно при
registration: FG-54F13 LIVE evaluation work/max1M. Это не доказывает scratch
или queue failure. Отдельный opt-in `FG28_BUDGET=large-work` использует existing
LiveOptions work100M, прежние scratch256MiB/queue10K/64MiB. Criterion group
`gql_stage28_large_work` исключает смешение с default estimates; metadata печатает
фактические budgets. Unsupported profile/case отклоняется до fixture. Point
bindings сохраняют прежние defaults. Default L11 runner отклоняет inherited
budget override до ledger. Production Rust source unchanged, доказан source
manifest delta only benchmark (`c28-10-source-delta.json`).
Benchmark check PASS (`c28-10-bench-check-final.log`), scoped nightly fmt PASS.
Runner selfcheck PASS включая absent/present override, unlimited sequential
ledger и owned timeout cleanup (`c28-10-runner-selfcheck-final.log`). Первый
selfcheck failed: null environment assignment оставлял empty variable; repaired
native removal/restoration повторно прошёл. Release build/probes выполняются;
registration-only success не считается commit performance evidence.

C28-10 release PASS3m18s. Default/large-work64-row scan/FETCH/LETdata/LETparam
smokes **8/8 PASS**, invalid profile and unsupported case rejected. Same1M probe
now passes work admission but rejects registration on **LIVE dependency scratch
33554432**. Exact work100M/scratch268435456/queue10K/64MiB is printed; failed
registration is not a latency result. Raw logs/hash/exit saved in `c28-10-probes`.
Process peak memory was unavailable for that initial probe, explicitly unknown.

## C28-11 — authority полного CLASS scan

Measured dependency scratch failure motivates coalescing covered BaseAtom keys.
Create/set/remove/delete all stamp BaseClassRange; normal/rebased publication
preserves range_writes, import seals them before clearing and restamps primary
revisions at END. BUILD GRAPH only changes existing occurrences/overlays.
Private LIVE recorder omits covered base keys only after an actual global scan;
point/FETCH outside that range, scoped memberships/overlays, schema/dictionary,
functions and bindings сохраняются. RefCell guard не пересекает external observer.

Initial focused gate **45 PASS/3 FAIL** and review WARN exposed that schema-only
lookup also records a global class range. Repaired private map distinguishes
ordinary observed keys from actual scan authority; successful RootRead scan
notifies through optional internal callback, default constructor remains unchanged
for other consumers. No storage key/revision/codec added. Mechanical map refactor
check PASS. Repaired focused **48/48 PASS** (`c28-11-focused-repaired.log`),
including strict point/FETCH/scoped tests, cancellation and registration SSI
against all4 base mutation categories. 4096 discarded rows now fit1MiB scratch,
with class range retained. Full/backend and fresh review remain in progress.

Финальный C28-11 source учитывает marker array/alignment отдельными32 байтами
на key поверх прежнего192-byte tree+returned-vector reserve. Admission остаётся
до insertion; promotion не выделяет память. Final full lib **1156 PASS/10 ignored/
0 failed/0 filtered**,102.24s (`c28-11-full-lib-final.log`), LIVE RAM **10/10**,
local LIVE **36/36** и general local **16/16 PASS** (`c28-11-backends-final.log`).
Clippy delta **0** (прежние6951 errors/544 warnings;
`c28-11-clippy-delta.json`), scoped nightly fmt/diff whitespace PASS.
Read-only source re-review **PASS**: actual scan origin, normal/rebased/import
range stamping, observer borrow lifetime и point/scoped/external predicates.
Release rebuild выполняется; before binaries/hashes сохранены в `c28-11-before`,
frozen source626 files в `c28-11-source-manifest.json`. Graphify AST PASS58434
nodes/157635 edges/1907 communities (`c28-11-graphify.log`); отдельный re-review
32-byte admission refinement PASS. 1M success и ускорение
до парного probe не заявляются. Stage28/L11 OPEN.

C28-11 release build PASS12m24s (`c28-11-release-build.log`). Exact before/after
1M large-work100M/scratch256MiB RAM scan1: before registration rejects dependency
scratch32MiB; after registration, whole-commit mutation oracle, delivery and ACK
выполнены. Criterion mean **2.7203s**, interval2.6121–2.8731s/10 samples.
Peak admitted evaluation scratch **132964 bytes**; publication12 batches/768
events,0 faults/GAP; result identities/images/stamps проверены до samples.
Process sampled peak after **1226014720 bytes**, включает dataset/import/roots;
это не132964-byte scratch counter. Rejected before peak739315712 bytes не
сравнивается как успешный workload. Logs/sha/process metadata:
`c28-11-large-work-before`, `c28-11-large-work-after`. Это отдельный профиль;
default work1M не изменён. Остальные large-work cases/backends ещё требуют
measurement; общий5% gate остаётся отдельным.

C28-11 fixed CPU0 RETURN10K pairs: mean1.02071/upper1.03478 **PASS**,
mean1.07721/upper1.09039 **FAIL** (`c28-11-affinity-diagnostic`). Общий5% gate
не закрыт. Сохраняются обе пары; удачная не выбирается как итоговый verdict.

## C28-12 — origin только для timed session statements

Source scout и review подтвердили: единственный production restrict_timeout
вызывается для AST Statement.timeout. Nested blocks/PURE/MATCH/import используют
checked_child с invocation clock; сохранённый started там deadline не определяет.
Generic constructors обязаны сохранять fresh origin для последующего restriction.

После mechanical initializer extraction/check PASS main session loop использует
existing elapsed timestamp только при отсутствии обоих timeout: execution option
и parsed statement. Все timed cases принудительно сохраняют fresh capture;
transaction commands/import и generic new/with_runtime не меняются. Representation
остаётся Instant, commit/BLOCK clocks и reporting origin сохранены. Source review
PASS. Options focused **9/9 PASS** (`c28-12-options-tests-repaired.log`), включая
untimed anchor, invocation child, оба timed exclusions и прежний clock-anchor
contract. Первый compile failed только на imports нового test; repaired run PASS.
Scoped nightly fmt PASS. Full/backend/Clippy/release measurement ещё выполняются;
ускорение не заявляется. Stage28 OPEN.

C28-12 final correctness: full lib **1157 PASS/10 ignored/0 failed/0 filtered**,
102.04s (`c28-12-full-lib.log`); LIVE RAM **10/10**, local LIVE **36/36**,
general local **16/16 PASS** (`c28-12-backends.log`). Public LIVE Rust SDK
rustdoc **21/21 PASS** (20 merged+1 standalone; `c28-12-rustdoc.log`).
Clippy delta **0** относительно C28-11, прежние6951 errors/544 warnings
(`c28-12-clippy-delta.json`). Scoped nightly fmt/diff whitespace PASS.
Before binaries/hashes сохранены в `c28-12-before`; source626 files frozen в
`c28-12-source-manifest.json`. Optimized build ещё выполняется; paired timing
не заявлен. Graphify AST PASS58438 nodes/157664 edges/1937 communities
(`c28-12-graphify.log`). Перед just all-check сохранены645 workspace Rust files
(`c28-12-all-check-before/hashes.json`) для восстановления unrelated formatting.
Общий L11 и tooling closure OPEN.

C28-12 release build PASS13m18s (`c28-12-release-build.log`). Fixed CPU0
RETURN10K diagnostic current/base/base/current, all exit0/affinity1:
mean0.99967/upper1.00911 **PASS**; mean1.03198/upper1.05183 **INCONCLUSIVE**
по5% conservative CI rule. Обе means допустимы, последняя upper превышает gate
на0.18pp; результат не скрывается и gate не закрывается. Raw estimates/hash:
`c28-12-affinity-diagnostic`. Полный L11 run4 запущен с тем же baseline, profile,
default budgets и900s per-cell deadline; результат pending.

Полный L11 run4 **Completed**,72 cells/17m15s, **0timeouts**:
30 Measured,28 CapacityGap,8 VerifiedImportProbe,6 VerifiedResourceProbe.
Common **5/9 PASS**, общий **FAIL_OR_INCONCLUSIVE**. Все mutate64 и1M point16
проходят;10K RETURN проходит.100K RETURN mean1.145/upper1.373 не проходит;
point16 10K/100K и1M RETURN остаются inconclusive по CI. Raw ledger/logs/hashes
и comparisons: `c28-12-run4-summary.json`, `c28-12-run4-common-interim.json`,
`l11-attempts/attempt-4`. Controlled CPU0 diagnostic и free-affinity full run
не смешиваются. Следующий measurement контролирует affinity baseline/current;
неудачный run сохраняется. `just all-check` выполняется отдельно после benchmarks.

C28-12 `just all-check` выполнен: default cargo check **PASS21.05s**,
nightly fmt **PASS**, default Clippy **FAIL6989 errors/537 warnings**, прежний
глобальный debt (`c28-12-all-check.log`). Ровно4 unrelated formatter rewrites
в transaction/tests и graph_index/{btree_idx,scalar_idx,streaming} восстановлены
из pre-command snapshot; все **645/645** Rust hashes совпадают
(`c28-12-all-check-preserved.json`). Scoped source остаётся проверенным;
global tooling gate OPEN, commit не создаётся. Benchmark9/CI/workload gaps не
переименовываются в PASS из-за pre-existing tooling failure.

## C28-13 — controlled common process blocks

Exact C28-12/baseline binaries и50samples/2s warmup/10s measurement сохранены;
для всех3 workloads/3 sizes выполнены current/base/base/current blocks на owned
process affinity1. Host power/priority не менялись, core class не предполагается.
Все12 processes exit0; actual masks/hashes/times recorded. **14/18 pairs PASS**
по прежнему mean+conservative-upper≤1.05 rule; все18 means находятся≤1.05.
10K RETURN repeat upper1.068 остаётся inconclusive;100K point16 upper1.059/1.071
и1M point16 repeat upper1.054 также.1M RETURN обе пары PASS (+1.41%/+1.47%);
все mutate64 проходят. Итог strict performance gate **OPEN**.

Same-binary control:10K current RETURN repeat изменился на9.44%, point16 на6.06%;
baseline на−0.19%/+1.90%. Это process variation без source mutation, не доказательство
отсутствия regression.100K shifts меньше. Criterion samples не объединены как
независимые processes. Evidence `c28-13-common-controlled-summary.json`,
`c28-13-process-noise-interim.json`, `c28-13-common-affinity-{10000,100000,1000000}`.
No new source optimization selected. Следующее evidence — оставшиеся large-work
LIVE cases, с прежним scratch и whole-commit oracle.

## Новый лимит оптимизаций — пять попыток

После нового прямого указания пользователя допускаются **пять новых попыток
оптимизации**, затем изменения ради ускорения прекращаются. Текущий C28-15
считается **попыткой 1/5**: одна причинная правка и её correctness/performance
проверка. Повторы сборки и дополнительные workload cells этой же правки не
образуют новые оптимизации. Предыдущие full runner runs 1–4 сохранены отдельно.
Журнал: `target/stage28-baseline/optimization-cap-2026-10-09.json`.

C28-15 убирает треугольное перевычисление уже активных SINCE NOW definitions
при строго registration-only commit. Primary/schema/capture inputs должны
совпадать; bindings, historical/future SINCE и mixed DML сохраняют полный путь.
All-definition admission, capture metadata, fresh arrays, FCW/SSI и WAL/queue
publication остаются общими. Hydrated before-root соответствует replay;
identity clocks и statistics не делают COW при доказанном отсутствии изменений.

Focused registration gate **21/21 PASS**, новое public local reopen gate
**2/2 PASS** (Sync/NativeAsync). Полный lib **1162 PASS/10 ignored/0 failed/0
filtered**, 85.94s; backend targets RAM **10**, local LIVE **38**, общий local
**16**, всё PASS. Scout и reviewer подтвердили selection/fallback, clock и
statistics guards. Промежуточные fixture/compiler failures исправлены и
сохранены в logs; это не performance evidence. Check **PASS7.92s**, Clippy delta **0** относительно C28-12 (6951 errors/544
warnings). Focused nightly format для9 touched files PASS; общий format check
выявляет лишь прежние4 unrelated paths, которые сохраняются. Release build **PASS11m24s**, exact EXE hashes/input626 frozen в
`c28-15-release-provenance.json`. Release-замеры этого среза ещё ожидаются. Final read-only review **PASS**: material missing
correctness evidence для C28-15 не найдено. Runner self-check PASS, включая
negative verdict/owned cleanup/resume/PID mismatch/default budget guard.
Graphify AST update PASS: 58452 nodes/157694 edges/1918 communities.
Fresh snapshot текущих645 workspace Rust files сохранён перед будущим
`just all-check`; прежний snapshot не применяется к новой правке.
C28-15 RAM100K/100subscribers large-work повтор **PASS**: прежний process
таймаутился на registration через900.21s; новый завершил регистрацию, first
oracle,10 samples и whole-commit delivery/ACK. Mean **16.624s**, CI16.174–17.149s
на изменение64 records для100 подписок; это latency processing, не время
registration. Initial candidate attempts **200** (100 registrations +100 first
mutation), after **1300**, faults/GAP **0**,1200 batches/76800 events.
Peak admitted scratch132964 bytes; process peak и полный elapsed записаны в
`c28-15-large-work-0-100000-scan_100/process.json`. Before не имеет successful
latency samples; числовое ускорение не вычисляется. Registration timeout устранён,
общий gate ещё OPEN. Новый счётчик source optimizations **1/5**.
Full runner historical run5 выполняется на этих же binaries. Common9 pairs
готовы: **3/9 PASS** (mutate64 на всех размерах); RETURN/point16 не закрывают
5% conservative CI rule. Raw evidence — `c28-15-run5-common-interim.json`.
Это negative evidence, не объясняется автоматически noise. Старый current-side
profiler относится к C28-6, до исправлений C28-8/9/12; read-only scout не нашёл
в нём актуального доказанного hotspot. Перед следующей source optimization
требуется свежий matching current/baseline symbolized capture. Общий run ещё
завершается; source не меняется во время measurement.

Full run5 завершён за **12m52.59s**:72 cells,30 Measured,28 CapacityGap,
8 VerifiedImportProbe,6 VerifiedResourceProbe,0timeouts. Итог
**FAIL_OR_INCONCLUSIVE**, common3/9 PASS; `c28-15-run5-summary.json` и
`l11-attempts/attempt-5` сохраняют все raw evidence/hashes. Source optimization1
завершена с устранённым registration timeout и незакрытым common gate.
Четыре оставшиеся оптимизации не используются без доказанного hotspot.
C28-15 `just all-check`: default check **PASS22.56s**, nightly formatter прошёл;
default Clippy **FAIL6989 errors/537 warnings**, тот же global baseline debt.
Четыре unrelated formatter rewrites восстановлены из fresh snapshot; hashes
**645/645 unchanged**, `c28-15-all-check-preserved.json`. No-default Clippy delta0
остаётся отдельным доказательством отсутствия новых diagnostics. Tooling gate OPEN.
Fresh exact profiler pair планируется после all-check; old и baseline pairs
сверены по EXE/PDB SHA и сохранены. Attribution profile opt3/ltofalse/cgu16/debug2
не смешивается с latency profile opt3/fatLTO/cgu1/debug0.
Fresh symbolized build **PASS10m36s**, pair/source hashes —
`c28-15-profile-pair.json`. Four20s profiling-mode captures (baseline/current
RETURN/point16,10K) и hotspots/top-down exports PASS; Rust full function/source
names проверены для каждого. User-mode sampling, stack collection и bounded
128MiB data; host settings не менялись. Raw manifests/CSV сохраняются как
`c28-15-vtune-*`; summary `c28-15-fresh-hotspots.json`. Это attribution evidence,
не latency comparison. Point16 измеряет standalone transaction API, не GQL.
Следующая source optimization ещё не выбрана и не начата; used1/5.
Уточнение единиц fresh exports: top-down CPU Time:Total — normalized percent
(root100), CPU Time:Self — seconds; summed memcpy Self совпадает с hotspot
CPU seconds. Self не используется как percentage. Parser-path memcpy текущего
профиля2.228629s против baseline2.398605s не доказывает per-query ухудшение: число
iterations в20s profiling mode не нормировано. AST width и instructions нужны
перед любым новым parser optimization; size-гипотеза не считается измеренным
latency cause.

Статус этапа и performance gate **OPEN**.

## C28-16 — comparator experiment, попытка 2/5

Fresh SDK point16 instructions показывают call к общему ConflictKey comparator
в текущем revision BTree candidate loop; baseline сравнивает ключи внутри поиска.
Normalized revision lookup7.168% против5.305% служит локализацией, не доказательством
5% latency regression. Call/dispatch evidence: `c28-15-instructions-*-reads/cmp.csv`.

Минимальная правка локальна VersionStore: private transparent RevisionKey хранит
тот же ConflictKey. BaseAtom/BaseAtom использует прежний GlobalAtomId Ord; все
прочие пары делегируют неизменённому derive. Это избегает ручного повторения
порядка23 вариантов и не меняет глобальные conflict keys, routing, Hash/Eq,
COW, restamp, capture, codecs или публичный API. Inline hint относится только
к малому comparator; forced-inline/cold attributes не добавлены. Admission
использует actual key width. Новый27-key тест сравнивает пары с исходным Ord,
antisymmetry/transitivity, layout, absent/overwrite и старые snapshots.

Read-only implementation review **PASS**, material hazard/missing test request
не найден. Focused **4/4 PASS**, check **PASS22.79s**, Clippy delta **0** (6951/544
baseline). Full lib **1163 PASS/10 ignored/0 failed/0 filtered**,81.38s;
Backend targets **10/38/16 PASS**, including Sync/NativeAsync; focused
nightly format check PASS. Graphify AST update PASS58460 nodes/157713 edges/
1911 communities. Release build **PASS10m53s**, exact hashes/source626
inputs — `c28-16-release-provenance.json`. Same-profile before/after1M common
block current/before/before/current с50samples/2s warmup/10s measurement и
owned-child affinity1 выполняется; raw `c28-16-before-after-1m`. Measured benefit
и generated fast-path evidence ещё ожидаются. Source optimization counter2/5.
C28-16 first balanced block завершён4 processes/exit0/affinity1. Point16 mean
ratios0.94387/0.99963, conservative upper0.95718/1.03093: только первая пара
доказывает снижение, устойчивый выигрыш не установлен. DML ratios0.91679/
1.02964, вторая upper1.09342. Все estimates сохраняются, успешная пара не
выбирается как verdict. Before final keep/revert заранее фиксируется один
обратный four-process block before/current/current/before с теми же параметрами;
это measurement внутри попытки2, source не меняется. Итог пока INCONCLUSIVE.
Итог всех fixed blocks: point mean ratios0.94387/0.99963/1.03225/0.94490,
upper0.95718/1.03093/1.05540/0.96295. Устойчивый material decrease не доказан;
RETURN/DML также имеют смешанные пары. `c28-16-all-pairs.json` сохраняет12
comparisons без pooling/cherry-pick. Verdict **RevertedNoStableBenefit**:
два source files восстановлены только из точного pre-experiment snapshot,
все626 inputs совпали с proven C28-15 (`c28-16-rollback.json`). Experimental
EXE/PDB/tests/results сохранены; они не объявляются production implementation.
Исходный Ord и VersionStore вернулись в текущий source. Used **2/5**, осталось3;
следующая попытка не начинается без нового подтверждённого cause.
Before source сохранён в `c28-16-before/source`, baseline/current binaries и
fresh profile provenance сохранены. При отсутствии полезного эффекта правка
откатывается только в этом срезе. Новый лимит: **2/5**, после пятой оптимизации
ускорения прекращаются; Stage28/performance/tooling остаются OPEN.

## C28-17 — boxed LIVE syntax, попытка 3/5

Standalone size probe сначала не слинковался с fat-LTO rlib; production source
не менялся. Compile-time LLVM constants без линковки дали exact sizes наMSVC:
StatementKind baseline928/current960, Statement968/1000, LIVE SELECT960.
Matching parser program assembly memcpy наline116 имеет длины968/1000;
`c28-17-ast-sizes.json`, `c28-17-copy-widths.json` иIR/instruction exports сохраняются.
Эта структурная причина оправдывает experiment, но не обещает latency выигрыш.

Один Box создаётся в существующем LIVE parser boundary. StatementKind::LiveSelect
использует ту же boxed форму, что LetValue::LiveSelect; LET не создаёт второйBox.
Borrowed analyzer coercion и EXPLAIN/timeout normalization сохраняются. Новая
development syntax variant теперь принимаетBox при ручном Rust constructing;
LiveQuery SDK, Value/RecordId/qualified identities, typedIR и disk codecs прежние.
Direct LIVE parse/clone получает один cold allocation; ordinary AST width должен
уменьшиться. Arena/new dependencies/new executor не вводятся.

Scope — три syn/stmt files плюс focused parser/layout/wrapper/depth test.
Read-only proposal и actual implementation review **PASS**. Первый focused
run2/3 PASS выявил неверный fixture assumption: LIVE statement TIMEOUT уже
отклоняется parser::statement_timeout. Grammar/production behavior не менялись;
fixture исправлен на valid forms безTIMEOUT и отдельные rejection checks для
всех direct/LET/EXPLAIN prefixes. Failed log сохранён (`c28-17-parser-tests.log`),
Второй fixture некорректно применял object-depth к arrays; заменён nested
object literal по existing object parser contract. Оба failed logs сохранены.
Final focused **3/3 PASS**,0.01s (`c28-17-parser-tests-final.log`), включая layout,
direct/LET/EXPLAIN options/spans и неизменные TIMEOUT/depth rejection. Actual
implementation review PASS, missed AST admission dependency не найден.
Check **PASS22.42s**, Clippy delta **0** vsC28-15 (6951/544 baseline).
Full lib **1163 PASS/10 ignored/0 failed/0 filtered**,90.25s. Backend targets
**10/38/16 PASS** (Sync/NativeAsync), SDK rustdoc **21/21 PASS** (20merged+1
standalone). Focused nightly format PASS; Graphify58456 nodes/157700 edges/1924
communities. Release/performance gates pending. Compile-time after library probe confirms
StatementKind **960→928**, Statement **1000→968**, equal baseline sizes;
LiveSelectStatement stays960 behindBox. `c28-17-ast-sizes-after.json` records all
common types. This is layout evidence, not latency speedup. Before release/profile references
проверены по SHA и указывают на retained C28-15, не rejected C28-16. Fresh645-file
snapshot сохранён для future all-check.
C28-17 release build **PASS10m53s**, exact source626/hashes —
`c28-17-release-provenance.json`. Paired10K current/before/before/current
RETURN/point/DML block с50samples/2s warmup/10s measurement и affinity1 запущен
(`c28-17-before-after-10k`); before — retained C28-15. Current mode/cfg/budgets
остаются прежними, rejected C28-16 не используется как comparator baseline.
Layout уже восстановлен к baseline, latency verdict pending. Counter3/5.
Final fixed10K block: RETURN mean1.06020/1.08836, upper1.12684/1.14818 —
обе пары не проходят5%. Point0.99516/0.96487, DML0.96027/0.99562 не отменяют
непрофильную RETURN regression. Verdict **RevertedReturnRegression**. Три
production files восстановлены только из pre-experiment snapshot; все626
inputs совпадают с proven C28-15 (`c28-17-rollback.json`). Box-представление
и новый test не оставлены в production; новая development AST API signature
также вернулась. Rejected EXEs/IR/layout/instructions/correctness/logs сохраняются
отдельно. Counter **3/5**, осталось2; дальнейшие repetitions этого среза не нужны. Before source — `c28-17-before/source`; engine comparator experiment2
откатан, source before3 соответствуетC28-15. Counter **3/5**, осталось2; после пятой
оптимизации source ускорения прекращаются, closure/evidence работа продолжается.

## C28-18 — single-key forwarding, попытка 4/5

Свежие SDK atom instructions подтверждают helper call с known BaseAtom key;
call есть и вbaseline, дополнительный call не заявляется. Current generic lookup
проверяет schema/live namespace; keytag внутри outlinedhelper остаётсяruntime.
Experiment — один inline(always) у private однострочного record_read forwarding,
чтобы существующий lookup получил constant namespace. Readset mutex/entry,
snapshot.version, missing handling, last_base_read, FCW/SSI не меняются.
No newkeys/maps/cache/API/allocations/dependencies. Код/instruction growth и
переносимость profilingcgu16 → latencyfatCGU1 остаются рисками measurement.

Proposal/actual exact-diff review **PASS**. Check **PASS7.49s**, Clippy delta **0**
(6951/544 baseline); no local lint allowance needed. Full lib **1162 PASS/10
ignored/0 failed/0 filtered**,82.16s; backend **10/38/16 PASS**, scoped fmt PASS.
Release build **PASS10m39s**; exact source626/EXE hashes/bytes —
`c28-18-release-provenance.json`. Common EXE22470144bytes unchanged; LIVE
EXE+512bytes, total file-size evidence, not function-level code-size attribution.
Paired1M current/before/before/current block50samples/2s warmup/10s measurement,
affinity1 запущен (`c28-18-before-after-1m`), before — provenC28-15.
Final fixed1M block completed4 processes/exit0/affinity1. Point mean ratios
0.96418/0.99907 (upper0.97623/1.00824); only firstpair provesdecrease.
RETURN0.95186/1.04604 (second upper1.07742), DML1.01808/0.98511 (first
upper1.08263). No stable material benefit; **RevertedNoStableBenefit**. Only
transaction/mod.rs restored from exact before, all626 inputs matchprovenC28-15
(`c28-18-rollback.json`); rejectedEXEs/gates/pairs retained. Counter **4/5**,
осталась одна source optimization; additionalrepeats этого среза не запускаются.
Exact before source — `c28-18-before/source`, соответствует
provenC28-15 после двух scoped rollbacks. Check/Clippy/full/backend/release/paired
ожидаются. Mirror test дляannotation не добавляется; existing correctness gates
проверяют authority. Source optimizations **4/5**, осталось1. После пятой
оптимизации source ускорения прекращаются; незакрытые gates сохраняются вотчёте.

## C28-19 — exclusive readset, ПОСЛЕДНЯЯ попытка 5/5

Fresh record-read subtree содержит Mutex lock/unlock1.9207%/1.7040%Total
(ещёalternate0.3647/0.2139); Self — seconds. `c28-19-readset-lock-evidence.json`
сохраняет call parents. Transaction.reads — direct privateMutex, graphview
заимствует&Transaction; atom(&mutself) имеет exclusive ownership. Proposal review
PASS; detached Arc/raw tracker не найден. get_mut исключает только synchronization.

Сначала общая запись snapshot revisions выделяется без behavior change;
shared readers сохраняютmutex иentry.or_insert_with(snapshot.version).
Послеcharacterization exclusive path использует туже policy, atom меняет только
private helper call. No skiptracking/namespace/first-version/cache/snapshot/SSI
change, no unsafe/newdependency/cache/executor. Focused tests покрывают first
revision, absent/shared/exclusive interleaving, scopedgraphread, savepoint,
ReadCommitted refresh и concurrentwriter SSI. Full/backend/lint/release/paired
ожидаются. Before source — `c28-19-before/source`, provenC28-15.
Mechanical sharedpolicy characterization **14/14 PASS**. Exclusive/shared
focused tests initially2/2, afterread-only review added independent SSI case
(readA→writerA2→readerB3); final **3/3 PASS**. Same-key case preservesfirst
snapshot through privatewrites andsavepoint, disjointcase provesSSI rather
thanFCW. ReadCommittedrefresh map/cache PASS. Check **PASS7.19s**, Clippy delta
**0** (6951/544). Actual implementation review PASS. Full lib **1165 PASS/10 ignored/0 failed/0 filtered**,83.28s; backend
**10/38/16 PASS**, scoped nightlyfmt PASS. Final release/performance pending;
newtestmodule leaves unrelated dirtytests.rs unchanged.
Final source/verification review **PASS**, independent SSI evidence concern
closed by disjointwrite test; no material missing correctness evidence for this
slice. Scalar ownership/policy/defaults are unchanged. Final release build **PASS11m28s**, exact source627/hashes/bytes —
`c28-19-release-provenance.json`. Common/LIVE EXEs both+16384bytes vsC28-15,
total file-size evidence. Final paired1M current/before/before/current block
50samples/2s warmup/10s measurement/affinity1 запущен (`c28-19-before-after-1m`).
Performance decision is
separate and pending; fifth limit is final, no further optimization suggestions
or patches are accepted after this attempt.
FINAL optimization verdict **KeptMeasuredNonRegression**: pairedpoint ratios
0.94415/0.99063, upper0.96508/1.0000067; secondCI touchesunity, universal/steady
speedup не заявляется. RETURN0.92276/1.00659, DML0.97806/0.96841; all6 controls
проходят5%mean+conservativeCI rule relativeC28-15. Deterministic exclusive access
исключает readset lock/unlock наatom&mutself; sharedreaders сохраняютguard.
Allraw estimates/hashes retained (`c28-19-before-after-1m/comparison.json`).

**ОПТИМИЗАЦИИ ОСТАНОВЛЕНЫ: 5/5.** Attempt1 registrationtimeout fix retained;
attempts2–4 reverted after mixed/nonpassing measurements; attempt5 kept on
ownership/correctness/controllednonregression evidence. Full original196d21b
performance gate ещё OPEN (lastfullrun5 common3/9), allmatrix/full lifecycle/
resource/tooling closure остаётся evidencework. No new latency hypotheses,
profiling foroptimization, workload tuning or source accelerations aftercap.
Final comparisons/defaultmatrix are recorded once without seeking luckyrepeats;
negative/inconclusive evidence keepsStage28 OPEN.
Final original196d21b common pairs готовы: **5/9 PASS**, performancegate после
пятой оптимизации **НЕ ПРОЙДЕН**.10K RETURN mean1.26726/upper1.46060, point1.11340/
1.21916;100K RETURN mean0.93340/upper1.05682,1M point1.00980/1.12069 не проходят
conservative5%rule. Allmutate64,100Kpoint и1MRETURN проходят. Все9 estimates
сохранены в `c28-final-run6-common-interim.json`; full72-cell run ещё завершается.
По прямому лимиту пользователя оптимизации прекращены; это не основание
выбирать удачные пары, ослаблятьgate или объявлятьStage28 завершённым. Frozen627 sourceinputs
and646-file currentall-check snapshot include newtest file.

Счётчик **5/5**. После оценки этой правки оптимизации прекращаются независимо
отperformance gate. Неудачная/inconclusive правка откатывается; remaining
correctness/resource/lifecycle/evidence/docs/tooling work продолжается, новые
latency hypotheses/source changes радиускорения не вводятся. OPEN gates сохраняют
этап28 незавершённым. No sixth optimization authorized.

## C28-14 — large-work registration bottleneck

RAM100K large-work100M/scratch256MiB: scan1 mean210.57ms, FETCH223.10ms,
LETdata157.78ms, LETparam158.64ms, scan10 **1.5202s**; full64-entry identity/image/
stamp/ACK oracle PASS,0fault/GAP. Source mutation and parameter-only updates
separate in counters. Raw Criterion/process observations in `c28-14-large-work-0-*`.
Scan10 preparation counter65 =55 sequential-registration candidate evaluations
plus10firstmutation. SDK's separate initial reads not included in this counter.

RAM100Kscan100 **TimedOut900.212s** before registration completed, no timing sample;
owned child stopped, exit−1/process peak83615744. Batch stopped; remaining cells
not run (`...scan_100/process.json`). This is measured nonlinear registration
work, not hidden as a capacity success. Subsequent scoped fix considers only
RegisterLive-only commits; mixed data/binding/future-SINCE cases remain full.

## Closure snapshot C28-3 — предшествующие gates

Это историческая сводка C28-3; результаты последующих C28-4/C28-5 приведены выше.
L1–L10 реализованы и проверены; этап остаётся **OPEN** до closure gates.

| Проверка текущего source slice             | Evidence                                                                                                                                     |
|--------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------|
| Полный lib, без фильтра                    | **1144 passed / 10 ignored / 0 failed / 0 filtered**, 72.72s; `c28-3-lib.log`                                                                |
| Публичный RAM SDK                          | **10/10 PASS**; `c28-3-ram.log`                                                                                                              |
| Local backend target с NativeAsync feature | **36/36 PASS**: 18 NativeAsync +18 Sync; `c28-3-local.log`                                                                                   |
| Production no-default check                | **PASS**; `c28-3-check.log`                                                                                                                  |
| Public Rust API rustdoc                    | **21/21 PASS**; `c28-3-rustdoc.log`                                                                                                          |
| Graphify AST update                        | Final **PASS**, 58378 nodes /157485 edges /1932 communities; `c28-3-graphify-final.log`                                                      |
| Protected dirty paths                      | **43/43 unchanged**; `c28-3-preserved.json`                                                                                                  |
| Scoped nightly format / Clippy delta       | **PASS / 0 новых diagnostics**; глобальный no-default baseline 6951 errors /544 warnings                                                     |
| Closure review C28-1/C28-2/C28-3           | **PASS** после durable close, snapshot revision docs и scoped streaming admission fixes                                                      |
| Baseline optimized build                   | **PASS**, 9m56s, exact `196d21b` isolated checkout                                                                                           |
| Current optimized builds: common и LIVE    | Post-C28-3 **PASS**, 10m47s; `c28-3-release-build.log`                                                                                       |
| L11 paired performance                     | **Attempt1 Failed; attempt2 Completed, FAIL_OR_INCONCLUSIVE**, ledger `l11-attempts/attempts.json`                                           |
| Whole `just all-check`                     | Final **OPEN**: check/fmt PASS; default columnar/datafusion Clippy 6989 errors /537 warnings, прежний глобальный долг; `c28-3-all-check.log` |

Оба optimized builds используют один command-local `profile.bench.debug=0`,
opt3/fatLTO/codegen1, stable1.98.1/MSVC, Criterion0.8.2 и production allocator.
Это отдельная сопоставимая конфигурация; два LLVM OOM исходного debug2 profile
до benchmarks не объявляются исправленными и не расходуют performance attempts.
Cargo.lock и общий benchmark совпадают по normalized text/parsed TOML;
различия raw SHA относятся к CRLF/LF. Хеши всех трёх EXE и окружение сохранены
в `target/stage28-baseline/l11-debug0-provenance.json`, proof —
`l11-paired-input-semantic-proof.json`. Runner получает explicit binary paths.

Rustdoc/format/Graphify и protected dirty bytes проверены после scoped repair.
Current optimized rebuild PASS; source manifest и новые SHA сохранены в
`l11-attempt2-provenance.json`. Попытка2 использует этот frozen source slice.
Performance failures, capacity rejection и непроверенные сценарии остаются явными;
они не превращаются в успешные latency samples.

## C28-3 — scoped streaming admission и bounded runner

Первая нумерованная попытка L11 сохранена как **Failed**: 36 completed cells
и одна явно остановленная ячейка `live-0-1000000-import_on`. Все девять common
пар измерены; четыре проходят оба ratio criteria, остальные FAIL/INCONCLUSIVE.
Raw comparison — `l11-attempt1-common-comparison.json`. Capture ON на100K
проверил typed256MiB rejection и неизменность primary/clock. Остановленная1M
ячейка не дошла до oracle; атомарность для неё не заявляется. Причина и owned PID/
path/start evidence — `l11-attempt1-early-stop.json`. SHA-проверенные EXE первой
попытки сохранены в `l11-attempts/attempt-1/bin`; счётчик не сбрасывается.

Fresh bounded review обнаружил cumulative transient charging: RootRead
резервировал discarded WHERE rows до конца всего evaluation. Подтверждённый
red reproducer4096→4 rows отклонял8MiB budget с `PURE value scratch`
maximum3662490 (`c28-3-red.log`). Initial fixture ClassId compile mismatch
исправлен до reproducer; unconnected-caller red не считается green evidence.

Сначала перенесён неизменённый identity estimator к внутреннему методу
LiveIdentity и обновлены три existing callers; mechanical check PASS. Затем
существующий BarrierAccount получил scoped lifetime вокруг direct root Class
cursor.next+callback. Escaping value+identity получают отдельную retained
admission до insertion. Nested SELECT, index pages, union/window/JOIN и прочие
materializing callers сохраняют прежнюю conservative policy. Limits/work,
Row::Clone, публичные types, codecs и layout не менялись.

Resource gate **6/6 PASS**: discarded rows, retained values и long identities,
oversized input, nested output, scope nesting/error/cancellation cleanup.
Первая nested fixture была отвергнута по scalar cardinality, а не ресурсу;
заменена корректной однострочной nested projection с большим array payload.
Fresh source re-review **PASS**. Full lib **1144/10 ignored/0 failed/0 filtered**,
72.72s; check/fmt PASS; Clippy delta **0** (прежние6951/544). Public RAM10/10
и separate Sync+NativeAsync36/36 PASS. Независимый work ceiling сохраняется;
исправление byte lifetime не объявляется прохождением performance gate.

Maintained runner теперь ограничивает cell15 минутами, используя Stopwatch
и initial age при resume. Timeout — отрицательное неполное evidence, не
resource oracle: операция могла не дойти до проверки результата/atomicity.
Timeout не может стать PASS, включая resume и common comparison. UTC process
start сравнивается как DateTime, потому что PowerShell JSON parser может
возвращать System.DateTime. Selfcheck actual AST helpers PASS: owned timeout/
cleanup, wrong PID/start rejection без kill, parsed-date resume, common row
binding, negative verdict, failed-cell rejection и original max5 guard.
Fresh runner review **PASS**; real attempts по-прежнему1/5.

## L11 — разделение import scaling и LIVE admission

Read-only source comparison с196d21b подтвердил неизменённую структурную работу:
DatabaseRoot::change_base_property клонирует IndexStoreRoot на каждом point setter;
его owned class-membership BTreeSets копируются даже без explicit indexes
(`src/datastore/root.rs:581`, `src/datastore/index_store/model.rs:218`).
Source ON capture повторяет этот setter при replay journal. Это источник
повторных whole-domain copies, не доказанный CPU hotspot и не количественная
атрибуция wall time; профилирование этой фазы ещё не выполнено.

Гипотеза per-item full savepoint не подтверждена: default Abort import отключает
rollback_mutation_errors и использует journal checkpoint. ON сохраняет один
segment-before root на2048 items; обе политики уже есть в196d21b.
Source capture cap=min(max_write_set_bytes,256MiB) также pre-existing и не требует
LIVE registry. В попытке2 import ON100K отвергнут при268435906>268435456 bytes
через149.27s, с successful atomicity oracle; baseline threshold не измерялся.

После scoped byte repair100K scan/FETCH/LET отклоняются по независимому
`LIVE evaluation work` maximum1000000. Replay/ACK100K и1M измерены успешно.
Defaults не увеличены. Непрошедшие большие scans и timeout cells остаются
capacity/performance gaps и не превращаются в успешный full-matrix verdict.

## Closure rule-to-test matrix — current source

Ниже representative tests, имена проверены по `c28-3-lib.log`; это не замена
полного unfiltered gate1144/10 ignored/0 filtered. Public SDK/runtime coverage
дополнительно подтверждается RAM10/10, Sync+NativeAsync36/36 и rustdoc21/21.

| Rule                                               | Passed test IDs                                                                                                                                                                                                                                     | Evidence              |
|----------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-----------------------|
| Owned images и cursor frames                       | `datastore::transaction::live_result::tests::live_result_final_parameters_net_changes_and_owned_images`<br>`live::cursor::tests::whole_commit_cursor_golden_and_malformed_frames`                                                                   | PASS, `c28-3-lib.log` |
| WHERE/FETCH, negative reads и qualified identities | `gql::live_lowering::query::tests::live_query_qualified_identity_absent_fetch_and_resource_errors`<br>`datastore::transaction::live_backfill::tests::live_backfill_omit_fetch_linked_members_matches_select_and_negative_reads`                     | PASS, `c28-3-lib.log` |
| Frozen LET, LIVE LET replacement/cycles/savepoints | `datastore::transaction::live_binding::tests::live_binding_frozen_inputs_replacements_savepoints_and_old_snapshots`<br>`datastore::transaction::live_binding::tests::live_binding_cycles_and_dependent_removal_are_atomic`                          | PASS, `c28-3-lib.log` |
| Queue pressure, reservations и replay ACK          | `datastore::live_queue::registry::tests::live_queue_session_pressure_applies_gap_disconnect_and_block`<br>`datastore::live_queue::replay::tests::live_replay_resource_error_fuses_attachment_and_keeps_ack`                                         | PASS, `c28-3-lib.log` |
| Whole-commit backfill и aggregate coverage         | `datastore::transaction::live_backfill::tests::live_backfill_transaction_registration_catches_up_whole_commits`<br>`datastore::transaction::live_backfill::tests::live_backfill_aggregate_complete_domain_includes_unchanged_rows_and_empty_domain` | PASS, `c28-3-lib.log` |
| Import retries, controls и final validation        | `datastore::import::live_publication_tests::live_import_preparation_retry_keeps_spool_and_latest_controls`<br>`datastore::transaction::import::tests::live_import_final_validation_failure_keeps_close`                                             | PASS, `c28-3-lib.log` |
| Retention/pins и authoritative recovery bounds     | `datastore::changefeed_retention::tests::live_retention_local_sync_prefix_and_pins`<br>`datastore::transaction::live::tests::live_catalog_recovery_requires_authoritative_commit_bounds`                                                            | PASS, `c28-3-lib.log` |
| Malformed pages/continuations                      | `storage::live_history::tests::live_history_malformed_lengths_reserved_and_admission`<br>`storage::live_history::tests::live_history_shared_continuations_keep_whole_commit_and_budget`                                                             | PASS, `c28-3-lib.log` |
| Streaming lifetime и retained resource limits      | `gql::live_lowering::query::scratch_tests::live_streaming_discarded_rows_charge_peak_scratch`<br>`gql::live_lowering::query::scratch_tests::live_streaming_scope_restore_and_failure_cleanup`                                                       | PASS, `c28-3-lib.log` |

Десять ignored probes относятся к прежним stage11/13/14 query profiling,
stage10/19 index diagnostics, local-file layout/reopen diagnostics и HNSW recall.
Они не считаются исполненным LIVE correctness evidence. Performance matrix имеет
отдельный ledger; resource rejection/timeout не превращаются в successful samples.

## Итог performance attempt2 — 2026-10-09

Полная defined72-cell матрица завершена за85m21s. Counter **2/5**: attempt1
Failed/неполная, attempt2 Completed/**FAIL_OR_INCONCLUSIVE**. Completed обозначает
завершение матрицы, не успешный gate. Attempts3–5 не запущены без новой подтверждённой
гипотезы; лимит пользователя не сброшен и не превышен. Stage28 остаётся **OPEN**.

| Outcome               | Cells | Значение                                                                    |
|-----------------------|------:|-----------------------------------------------------------------------------|
| Measured              |    30 | 6 common processes и24 LIVE latency rows                                    |
| VerifiedImportProbe   |     6 | Successful import ON/OFF с exact whole-commit oracle                        |
| VerifiedResourceProbe |     6 | Expected hard-cap/fault с primary-publication oracle                        |
| CapacityGap           |    26 | Typed registration/evaluation/import rejection; latency отсутствует         |
| TimedOut              |     4 | Import ON/OFF1M RAM/Sync не дошли до oracle за900s; atomicity не заявляется |

|    Rows | Common workload | Mean current/base | Conservative upper ratio | ≤1.05 оба         |
|--------:|-----------------|------------------:|-------------------------:|-------------------|
|   10000 | return_literal  |            1.1599 |                   1.3039 | FAIL/INCONCLUSIVE |
|   10000 | point_read_16   |            1.1262 |                   1.2401 | FAIL/INCONCLUSIVE |
|   10000 | mutate_64       |            0.8543 |                   0.9081 | PASS              |
|  100000 | return_literal  |            1.7176 |                   1.9487 | FAIL/INCONCLUSIVE |
|  100000 | point_read_16   |            1.0091 |                   1.1609 | FAIL/INCONCLUSIVE |
|  100000 | mutate_64       |            1.0635 |                   1.2175 | FAIL/INCONCLUSIVE |
| 1000000 | return_literal  |            1.0710 |                   1.2096 | FAIL/INCONCLUSIVE |
| 1000000 | point_read_16   |            0.9784 |                   1.1162 | FAIL/INCONCLUSIVE |
| 1000000 | mutate_64       |            0.9871 |                   1.0191 | PASS              |

Из9 paired criteria проходят2. Between-run variation меняет ratios относительно
attempt1; контролируемого CPU/allocator profile hot path пока нет. Source-backed
pre-existing whole-class copies не объявляются количественным объяснением wall time.
Work caps и capture limits сохраняются; не заменять rejects/timeout успешными samples.

Evidence: `l11-attempts/attempts.json`, `l11-attempt2-provenance.json`,
`l11-attempt2-summary.json`, `l11-attempt2-comparison.csv`, `l11-attempt2-cells.csv`,
`l11-attempt2-latencies.csv` и per-cell stdout/stderr/criterion distributions.
Source manifest unchanged; attempt1 EXE сохранены отдельно. Peak sampled process
RSS включает fixture/setup; admitted reserves не считаются RSS. Local replay включает
cold-engine reopen/observations, не доказанный cold OS/device cache. NativeAsync
correctness36-target проверен; отдельный NativeAsync performance target не измерялся.

Implementation/correctness текущего slice PASS: full1144, resource6, RAM10,
Sync+NativeAsync36 и SDK rustdoc21. Общий performance и прежний global tooling
gate остаются OPEN. Подтверждённое C28-3 исправление не объявляется прохождением L11.

## Final tooling/preservation closure — 2026-10-09

Повторён `just all-check` на текущем slice: default cargo check и nightly fmt
прошли, Clippy снова6989 errors/537 warnings в прежней columnar/datafusion
конфигурации. Это не PASS whole recipe. No-default slice отдельно имеет delta0
при6951/544 baseline. Четыре formatter rewrites несвязанных files восстановлены
к exact pre-command bytes: storage/graph_index/{streaming,scalar_idx,btree_idx}.rs
и datastore/transaction/tests.rs. Все633 backup hashes подтверждены; artifact
`c28-3-all-check-restored.json`. Source, использованный в attempt2, сохраняется.

Stage28 implementation/correctness срезов L1–L10 и C28-3 проверен. Performance
L11 и global tooling остаются **OPEN**; статус этапа не изменён на завершённый.
Использовано2 из максимум5 performance attempts. Не увеличены defaults и не
запущены повторные длительные matrix runs без нового подтверждённого bottleneck.
Оставшиеся3 попытки сохраняются; это не утверждение, что five-attempt cutoff достигнут.
Для следующей оптимизации требуется controlled CPU/allocator/variance profile
на failed common workload либо отдельный measured resource improvement.

Не выполнены: NativeAsync performance matrix, cold OS/device cache proof,
исправление общего lint debt, paid Graphify semantic re-labeling. Stage29 Events/
LiveGraph не входил в scope. Коммит и PR не создавались; unrelated dirty work сохранён.

## Baseline и сохранность

До изменений сохранены SHA-256 958 tracked files (кроме generated Graphify outputs)
и git status в `target/stage28-baseline/`. Несвязанные README, staged prototype deletions,
disconnected asynchronous/collections и reference trees сохраняются.
`cargo check --lib --no-default-features` baseline: PASS, 210 warnings, 15.18s.
Stage-27 performance/tooling OPEN не считается новым дефектом этапа 28;
новые результаты сравниваются отдельно. Сохранены measured query/DML baseline;
import baseline и paired сравнение текущего состояния ещё требуются.

## Срезы

L0 normative sync: план/specs/grammar/implementation registry согласованы;
paired baseline находится в работе. L1 SDK/descriptor и registry foundation L2
прошли focused correctness gates. Session consumers остаются частью L9;
L3 binding foundation и L4 result/dependency foundation прошли focused correctness gates;
L5 computed history/transport/publication прошёл focused correctness gates;
L6 common publication/import foundation прошёл correctness gates;
L7 queue/ACK SDK и publication получили focused/backend correctness evidence;
scoped tooling PASS без новых Clippy diagnostics, L8–L11 и closure ещё OPEN.
Baseline/check log: `target/stage28-baseline/check.log`.
Closure correctness/resource/performance/tooling остаются OPEN.

## L5a — computed envelope foundation

FGLB v1 сохраняет exact before/after, qualified record/occurrence/group identities,
captured binding revisions, historical dictionaries и owned Path. Initial rows
проверяются на восстановимость при регистрации и pending activation; initial
delivery не создаётся. Exact representation diff сохраняет наблюдаемые различия
числового cost в Path, не изменяя public Eq/Ord/Hash. Кодек не читает current primary.

Fault сохраняет generated declared status schema, typed parameters, locations,
phase/severity/timestamp, extensions и cause chain. Technical source не сохраняется.
Непомещающаяся исходная диагностика заменяется явной typed capture-failure
диагностикой; исходный source commit сохраняется. Related locations, map depth,
UTF-8 key caps и temporary allocations учитываются до allocation.

- `live_history`: PASS, 11 tests, 0 failed, 0.05s;
  `l5-diagnostic-tests-3.log`. Более ранний failing run не является evidence.
- `live_result`: PASS, 6 tests, 0 failed, 0.04s; `l5-result-tests.log`, включая
  real PURE cast fault с 17 KiB input и изменение exact Path cost representation.
- Итоговый focused `live_`: PASS, 62 tests, 0 failed, 3.32s;
  `l5-focused-final.log`, включая current/final registration rejection и source
  image после oversized diagnostic. Это не полный lib gate.
- Generated status restoration: PASS, 1 test; `l5-status-restoration.log`, включая
  точные u64 параметры выше 2^53, missing/wrong types и unknown code.
- `cargo check --lib --no-default-features`: PASS, 224 warnings, 21.04s;
  `l5a-check.log`. Decoder helpers ещё ожидают production physical reader consumer.
- Read-only re-review: PASS после исправления related-vector transient admission,
  initial result preflight и structured fault restoration.
- Nightly rustfmt: changed slice PASS. Clippy: 6962 прежних errors, 562 warnings
  по всем crates; `l5a-clippy.jsonl`/`l5a-clippy-delta.json`. Все 13 добавленных
  diagnostics — dead-code warnings у ещё не подключённых physical reader helpers;
  новых Clippy lint errors нет. Эти warnings не подавлялись; tooling gate OPEN.

Pre-epoch diagnostic timestamp проверяется напрямую: existing JSON display
formatter не поддерживает такой SystemTime и panic-ит; этот прежний display
дефект не считается успешным wire roundtrip. Binary LIVE codec сохраняет его точно.
Physical segment/catalog publication, recovery, reader/resume и очередь пока
не реализованы в L5a; L5 и этап 28 остаются OPEN.

## L5b — transport foundation

Page codec выделен в `history_codec/pages`; HistoryOwner различает source и
private LIVE даже при совпадающих UUID bytes. FCFS tag 0 сохраняет прежние source
golden bytes; LIVE tag 1 отделяет private envelope. Catalog segment codec общий,
source references остаются source-typed. Source walker, retention и local API
проверяют exact owner до filtering/bounds/GAP. Shared LIVE decoder продолжает
page/payload budget; новый бюджет посреди восстановления не создаётся.

- Source regression после refactor: PASS, 85 tests, 0 failed, 7.00s;
  `l5b-source-regression-2.log`, включая import, WAL/recovery и pinned snapshots.
- Computed transport: PASS, 12 tests, 0 failed, 0.04s;
  `l5b-transport-tests-2.log`. Continuations сохраняют целый commit и checked budget.
- History boundary fixtures: PASS, 20 tests, 0 failed, 1.39s;
  `l5b-history-foundation.log`, включая wrong-owner rejection до любого page read.
- Read-only re-review transport foundation: PASS; catalog publication ещё вне
  review. Failing compile attempts в ранних logs не считаются evidence.

После transport refactor полный unfiltered lib gate: 850 passed / 10 ignored /
0 failed / 0 filtered, 65.56s (`l5-transport-lib-full.log`).

## L5b — durable publication (2026-10-05)

Immutable root хранит отдельные LIVE history references и capture epochs.
Final source configuration применяется к целому commit: включение захватывает
предшествующий DML; отключение оставляет явный disabled epoch, без подмены
volatile payload durable историей. Нет initial rows и повторного сохранения
prepared envelope старого commit. Parameter-only aggregate changes тоже сохраняются.

FGDC v12 development header использует offset 132 для history count; logical
metadata header 80 bytes, capture epoch 48 bytes, shared segment reference 128 bytes.
Recovery сверяет stable source identity, final policy/revision/TTL, query version,
clocks, owner, file reachability и snapshot page ceiling. Source SHOW CHANGES
не читает private LIVE envelopes. Source/LIVE pages и catalog используют общий
file catalog, pins, WAL barrier и generation publication.

Lookup work ограничен: admitted Source map и binary query/envelope lookup
заменяют произведение query/source cardinalities. Все lengths, vectors и catalog
bytes проверяются до allocation/write. Ошибка общего publication admission
отклоняет commit атомарно; typed evaluation fault сохраняется вместе с успешным DML.

- Focused publication/codec: 17 passed / 0 failed, 1.55s
  (`l5b-publication-tests-5.log`).
- Full lib: 855 passed / 10 ignored / 0 failed / 0 filtered, 68.68s
  (`l5b-publication-lib-full.log`).
- NativeAsync publication lifecycle: 1 passed / 0 failed, 0.40s;
  отдельный `--features native-async-io` target (`l5b-publication-native-async.log`).
- Sync fixtures проверяют rotation, multipage images, pinned old snapshot,
  checkpoint/reopen, parameter-only catalog publication, unrelated import
  generation replacement и все существующие generation/commit failure points.
- Check PASS; nightly rustfmt changed slice PASS. После visibility cleanup
  Clippy: 6961 baseline errors / 565 warnings; 0 новых lint errors и 16 добавленных
  dead-code warnings у reader helpers до их L8 consumer
  (`l5b-publication-clippy-complete-delta.json`). Global tooling gate OPEN.
- Read-only publication re-review PASS после final-policy crosscheck и
  bounded-lookup fixes. SHA-256 41 unrelated dirty file совпадает с baseline
  (`l5b-unrelated-dirty.json`).

Ранние failed fixtures использовали undeclared bare field в schemaless SELECT;
они исправлены на существующий `properties.value`. Failed/zero-test runs не
считаются evidence. L5 correctness foundation PASS; публичная доставка,
import END LIVE evaluation/restamp, ACK, resume/retention, GQL и общий closure
остаются OPEN. Этот срез не доказывает готовность SDK потока.

## L6 — common publication/import (2026-10-05)

Обычный commit и END используют общий root-bound preparation hook. Accepted-item
spool остаётся доступным для bounded retry; новая попытка заменяет unpublished
history последним committed binding. END проверяет leased primary/schema/capture
inputs, переносит latest CLOSE metadata и назначает свежий stamp/time. Provisional
data/topology/statistics/source/LIVE metadata получают final stamp; committed CLOSE,
user Values и historical Path snapshots не переписываются. CLOSE-only publication
разрешён при writer lease; DML/FULL no-op, schema/capture и maintenance запрещены.
ACK присоединяется к этому узкому control gate в L7.

IMPORT CHANGEFEED=OFF сохраняет явный durable ImportSuppressed GAP; текущий
volatile net result не выдаётся за исторический backfill. Active binding/query fault
и work exhaustion при END приостанавливают LIVE, сохраняя accepted primary/source
data. Final schema validation и общие publication failures ничего не публикуют;
ранее committed CLOSE сохраняется. Пройден отдельный actual END failure-injection
gate для generation build/file-sync/catalog-stage/pre-WAL и append/flush/sync/reopen.

- Focused LIVE: **81 passed**, 0 failed, 6.21s (`l6-live-focused-final-2.log`),
  включая repeated-time RAM/Sync и ordinary/import publication failures.
- Separate NativeAsync import: **14 passed**, 0 failed, 2.04s
  (`l6-native-import-final.log`), включая оба barriers с repeated time,
  actual NativeAsync END, work fault и concurrent CLOSE. После resource fix
  final repeat: **15 passed**, 0 failed, 1.95s (`l6-native-import-resource-final.log`).
- Source import regression: **9 passed**, 0 failed (`l6-source-import-regression.log`).
- После resource fix import focused: **13 passed**, 0 failed, 1.55s
  (`l6-import-resource-final.log`), включая unchanged shared revisions и rejection
  required COW до allocation при недостаточном cap.
- Full unfiltered lib: **869 passed**, 0 failed, 10 ignored, 0 filtered, 75.28s
  (`l6-lib-full-final.log`). Первый failing full run сохранён ниже в ledger.
- Check no-default-features PASS, 227 warnings, 19.98s (`l6-check-final.log`).
  Nightly rustfmt: 12 changed L6 files PASS (`l6-rustfmt-check.log`).
- Clippy FAIL: 6961 baseline errors / 565 warnings; comparison с L3 содержит
  0 новых lint errors и те же 16 dead-code warnings L5b physical reader helpers
  до L8 (`l6-clippy-final-delta.json`). Global tooling gate остаётся OPEN.
- Rust SDK: RAM **2 passed** / 0 failed, local Sync **3 passed** / 0 failed
  (`l6-sdk-integration-final.log`); NativeAsync SDK **3 passed** / 0 failed,
  0.38s (`l6-sdk-native-final.log`).
- Review L6-TIME и L6-RESOURCE: PASS после исправлений.
- SHA-256 **41 unrelated dirty file** совпадает с baseline (`l6-unrelated-dirty.json`).
- Final Graphify AST update PASS: 57 145 nodes / 152 825 edges / 1 854 communities
  (`l6-graphify-resource-final.log`). Первый label signature failure L5b устранён
  повтором module update; tool не переустанавливался.

Finding ledger:

| ID          | Решение и evidence                                                                                                                                                                                                                                                                                     |
|-------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| L6-TIME     | Accepted, fixed. Backend больше не переназначает prepared time при совпадении с предыдущим commit. Red: 0/2 passed; после fix RAM и оба local barriers проходят. Maintenance назначает time отдельно.                                                                                                  |
| L6-RESOURCE | Accepted, fixed. Unchanged revisions/from=final stamp не требуют scratch COW admission. Первый full gate: 867 passed / 1 failed / 10 ignored, actual 6144 > 4096 в существующем quarantine fixture. Narrow quarantine после fix: 1 passed. Required COW/restamp по-прежнему проверяется до allocation. |

Ранние compile/fixture failures не считаются evidence: неверный вариант reader
item, создание source batch вместе с final CHANGEFEED policy, длинное имя local
database и доступ к UUID wrapper исправлены. L6 foundation не предоставляет
consumer delivery/ACK/resume; L7–L11 и общий closure остаются **OPEN**.

## L1 evidence

- `cargo test --lib --no-default-features live -- --test-threads=1`: PASS,
  16 tests, включая 6 новых SDK/descriptor tests; log `l1-tests-final.log`.
- Characterization `storage::changefeed::tests`: PASS, 10 tests; выделенный
  history codec сохраняет bytes. Read-only review подтвердил parity с HEAD
  после visibility/format-only изменений.
- Rustdoc LIVE: PASS, 2 examples без query text; в отдельном merged group 0 tests,
  этот пустой group evidence не считается. Log `l1-rustdoc.log`.
- `cargo check --lib --no-default-features`: PASS, 230 warnings, 23.76s,
  включая owned delivery/info types; log `l1-check-final.log`.
- Полный unfiltered lib gate: PASS, 792 passed, 10 ignored, 0 failed,
  80.38s; log `lib-tests-l1.log`.
- Nightly rustfmt применён только к changed slice. Global format debt сохранён.
- Clippy: повторный run 6964 errors/566 warnings; новых error diagnostics
  в изменённых SDK/codec files нет. Все пять найденных новых ошибок исправлены.
  Descriptor helpers до подключения catalog consumer имеют dead-code warnings.
  Полный tooling gate OPEN из-за прежнего долга; log `l1-clippy-final.jsonl`.

L1-R1 (Medium, accepted): decoder принимал неиспользуемые исторические dictionaries,
создавая несколько bytes одного logical descriptor. Добавлены canonical re-encode
validation в том же CodecBudget и regression fixture; fresh read-only re-review PASS.

Первый test/benchmark build завершился LLVM `no space on device`; такие runs не PASS.
Старые incremental caches освобождены, пользователь также освободил диск;
исходники и baseline artifacts сохранены. Проверки повторены после освобождения места.

## L2 — registry foundation

Дата проверки: 2026-10-05. `Transaction::register_live`, `close_live`,
`reregister_live`, `live_info` используют immutable history binding, обычный
mutation replay, FCW/SSI и savepoints. Registry не добавляет Arc к обычному root
clone. Перерегистрация откатывается целиком даже при отключённом rollback обычных
mutations. ReadCommitted capture, source owner resolution и analyzer используют
один statement snapshot. Recipes активируются после успешной публикации.

FGDC v11 хранит canonical recipes, исходный class owner, policy и watermarks.
Decoder проверяет UUID/version/tags/reserved bytes/lengths, allocation clocks и
aggregate scratch. Writer проверяет тот же recovery admission до pages/WAL.
Reopen сверяет LIVE stamps с trusted root commit clock. Старый snapshot сохраняет
прежнюю incarnation. Root-bound evaluation, LIVE LET, delivery handles/ACK/resume
и GQL session consumers ещё не реализованы; это не evidence доставки событий.

- Focused lib после последнего resource admission hardening: 25 passed,
  0 failed, 2.83s; `l2-tests-final.log`.
- Public RAM target: 1 passed; owned info/recipes, old snapshots и abort;
  `l2-ram.log`.
- Local Sync target: 1 passed; publication/checkpoint/reopen, full generation
  replacement, close/reregister; `l2-local-sync.log`.
- Local NativeAsync target: 1 passed при `native-async-io`, отдельный запуск;
  `l2-local-native-async.log`.
- Rustdoc `register_live`: 1 passed; merged group с 0 выбранных tests не считается
  evidence; `l2-rustdoc.log`.
- `cargo check --lib --no-default-features`: PASS, 211 warnings;
  `l2-check-final.log`.
- Nightly rustfmt: changed slice PASS; временный Windows mapped-file отказ был
  устранён повторным запуском. Несвязанный global formatting debt сохранён.
- Clippy после устранения новых findings: 6963 errors / 542 warnings, прежний
  tooling debt; один прежний missing-doc diagnostic устранён на изменённом VERSION.
  Окончательный run после последнего hardening дал тот же результат;
  `l2-clippy-final.jsonl`. Новых error diagnostics относительно L1 нет.
- Fresh read-only code review L2: PASS; execution evidence получен отдельно.

Resource fixtures проверяют пустой graph-source с impure/missing captures,
wide schemas, повторные rich Type projections, qualified/case-insensitive fields,
повторные большие field/FETCH names, PURE compilation scratch/work и компактные
catalog recipes, чей aggregate recovery scratch превышает cap. Failed registrations
не оставляют definitions. Сначала неверные fixture CheckpointMode/database-name были
исправлены; failed runs не считаются backend evidence.

## L3 — shared executor и reactive binding foundation

Root-bound read adapter переиспользует обычные SELECT/FETCH, class/function analysis,
field resolution и scan/point materialization без вложенной datastore transaction.
Conditional/CASE/AND/OR/coalesce и function arguments сохраняют lazy evaluation и
порядок ошибок в SELECT-special expressions. Existing scalar evaluator и SELECT
используют общий binary short-circuit helper.

Foundation до подключения binding publication: полный unfiltered lib gate —
804 passed / 10 ignored / 0 failed, 70.02s (`l3-root-full-lib.log`); check PASS,
213 warnings (`l3-root-check.log`). Clippy после новых fixes — 6962 errors /
543 warnings (`l3-root-clippy-final.jsonl`); global debt сохраняется. Fresh read-only
review shared adapter/behavior-preserving commit extraction — PASS.

Добавлены Rust define/read/info/replace expression/value/remove LIVE LET, stable DAG,
frozen inputs, checked work/scratch, final-state evaluation и typed pause diagnostics.
PURE versions и nested Record-source class owners захвачены явно. Обычный source DML
при active evaluation fault публикуется, registration fault отклоняет commit.
User evaluation вынесена до commit mutex; root/lease/FCW/SSI повторно проверяются
под lock, один existing WAL barrier остаётся authoritative.

Development catalog изменён на месте до FGDC v12: query и binding sections,
canonical recipes/exact values, stable owners, revisions/functions/fault metadata.
Compatibility decoders не добавлены. Aggregate physical catalog rejection остаётся
общей atomic resource failure, отдельно от configured per-binding evaluation caps.

Окончательные проверки L3:

- Focused LIVE lib: 42 passed / 0 failed, 3.01s (`l3-tests-final.log`). Chains,
  cycles, replacements/savepoints, source DML при pause, PURE recapture, active и Pending
  class recreate, SSI metadata, concurrent Snapshot writers, work exhaustion и codec corruption.
- Полный unfiltered lib: 818 passed / 10 ignored / 0 failed, 70.65s
  (`l3-binding-full-lib.log`). После него добавленный owner-tail fixture прошёл отдельно
  (`l3-binding-owner-codec.log`) и вошёл в окончательный focused run.
- Rust SDK RAM: 1 passed (`l3-binding-ram.log`); Sync: 1 passed
  (`l3-binding-local-final.log`); NativeAsync: 1 passed, отдельный feature target
  (`l3-binding-native-async.log`). Exact values >2^53, nonempty record owner tails,
  checkpoint/reopen, typed fault и full generation replacement проверены без GQL.
- Rustdoc define_live_binding: 1 passed (`l3-binding-rustdoc.log`);
  отдельный merged group с 0 tests evidence не считается.
- Check: PASS, 211 warnings (`l3-binding-check-final.log`). Nightly rustfmt changed slice PASS.
- Clippy: 6962 errors / 550 warnings (`l3-binding-clippy-final.jsonl`), global baseline debt.
  Multiset comparison по file/code/message с учётом mechanical live_catalog module move:
  0 новых error diagnostic instances относительно L2 (`l3-clippy-delta.json`).
- Fresh read-only binding/owner-codec closure review PASS. Graphify AST update PASS
  (`graphify-l3-update.log`); context7.json/config.json не создают code nodes, известный warning.

Readonly review нашёл L3-B1 (recreate nested record source), L3-B2 (live_info binding
SSI reads), early option admission, unique/live PURE references, query binding refs
и runtime metadata admission. Исправления внесены, fresh closure review PASS;
Pending activation regression закрывает найденное при re-review исключение.
Первый local fixture использовал слишком длинное database name, исправлен; failed
run не считается backend evidence. Первый full gate дал 816 passed / 1 failed:
resource fixture использовал count fast path. После замены на обязательный scan
work-cap regression и полный gate прошли. L3 focused gate PASS; L4–L11 и общий
correctness/resource/performance/tooling closure остаются OPEN.

## L4 evidence (2026-10-05)

Root-bound Rust SELECT сохраняет source identity отдельно от owned projection;
полный domain aggregate имеет `Aggregate([])`. WHERE/FETCH до и после commit
учитывают final data и LIVE LET values совместно. Нет initial batch при регистрации;
update→restore не создаёт net event. Active evaluation fault приостанавливает LIVE
с FG-G1F09/GAP, source DML коммитится; registration fault отклоняет регистрацию.
Record/graph occurrences и exact numeric representation сохраняются отдельно.

Graph aggregate использует общий SELECT reducer над class inputs с lazy CASE
для per-class typed arguments/overloads. Вложенный independent SDK SELECT работает
для нуля/одного/нескольких classes. Universal atom columns доступны и в пустой
области; collection eligibility требует VALUE независимо от числа classes.
Textual correlated SELECT не меняет контракт. Typed plans, result rows и distinct
dependencies учитываются до allocation/clone; reads используют общий work budget.

SchemaClass использует durable class revision; class namespace changes сопровождают
create/remove. Recovery reanchor namespace version безопасен для новых process-local
snapshots. Query captures, concrete PURE versions и paused diagnostics сохраняются
в FGDC v12 (query header 136 bytes); executable IR не сохраняется.

- Focused: `cargo test --lib --no-default-features live -- --test-threads=1` —
  **56 passed**, 0 failed (`l4-live-focused-9.log`).
- Full lib: **833 passed**, 0 failed, 10 ignored, 0 filtered, 64.81s
  (`l4-full-lib.log`). Старый datastore::tests фильтр не использован.
- Local Sync query captures: **1 passed** (`l4-query-local-sync.log`).
- NativeAsync: **1 passed** с `--features native-async-io`
  (`l4-query-native-async-final.log`). Первый запуск без feature выбрал 0 tests
  (`l4-query-native-async.log`) и evidence не считается.
- RAM SDK: **2 passed** (`l4-sdk-ram.log`); rustdoc register_live: **1 passed**
  (`l4-query-rustdoc.log`), отдельный merged group с 0 tests исключён.
- Check no-default-features PASS; nightly rustfmt changed slice **69 files PASS**
  (`l4-check-complete.log`, `l4-rustfmt-check-final.log`).
- Global Clippy FAIL из-за baseline: 6962 errors. Multiset file/code/message
  comparison с L3: **0 added diagnostic instances** (`l4-clippy-delta.json`).
  Учитываются mechanical module moves; это не утверждение о чистом global tooling gate.
- Fresh read-only union review PASS после R7/R8 fixes. R1–R6 исправлены и покрыты
  tests: work counts, heterogeneous overloads, alias overlap, nested scalar unions,
  shared dependency admission. R7 — empty atom schema; R8 — VALUE eligibility.
- SHA-256 **41 существующего unrelated dirty file** совпадает с начальным snapshot
  (`l4-unrelated-dirty.json`); initial prototype deletions не восстанавливались.

Первый более широкий LIVE run дал 54 passed / 1 failed: old rich-schema positive
fixture перестал помещаться после admission optimizer copies. Negative admission
сохранён; positive проекция уменьшена до fitting shape, после чего 56 tests прошли.
Ни compile failures, ни zero-test runs не считаются успешным evidence.
Durable computed history, queues, ACK/resume, GQL и общий performance/tooling closure
остаются **OPEN**; подготовленные L4 payloads ещё не являются доступным SDK потоком.

## L7 — whole-commit queues, ACK и owned SDK (2026-10-05)

Datastore владеет одним transient Consumers service. Регистрация заранее admits
attachment, а definition и activation публикуются одним commit. LiveSubscription
использует Weak origin, не удерживает transaction snapshot и не продлевает logical
datastore lifetime; возвращённые payloads/cursors остаются owned. next — polling
с Idle/Delivery/Closed; issued delivery повторяется до successful durable ACK.
ACK проверяет origin/query/version и issuance, сохраняет catalog watermark обычной
FCW/SSI mutation и только затем освобождает capacity. CLOSE/ACK-only commits доступны
при import lease; END переносит latest control metadata и выдаёт свежий final stamp.

Normal commit и import END допускают весь fanout вне commit mutex, повторно сверяют
root и consumer revision, затем устанавливают prepared queues под тем же mutex
после общего WAL barrier. RAII освобождает partial permits/retries без повторного
захвата commit lock. BLOCK проверяет existing cancellation каждые <=25 ms и ждёт
до более раннего statement/CommitOptions deadline либо subscription timeout.
GAP/DISCONNECT/BLOCK применяются также к общему retained ceiling 256 MiB.
Issued и pending controls оплачены при attachment; data не занимает reserved slot.
Overflow не теряет fault diagnostic, unknown GAP bounds или issued payload.

WAL maps — RapidHashMap: map/version/publication capacity prepared до append/fsync,
prune gate удерживает admission в порядке prune → writer → published. Backend
заранее detach-ит COW и создаёт empty history Arcs. Retirement Vec/path/marker
metadata имеет bounded pre-WAL admission, включая retained capacity; LIVE cleanup
происходит перед следующей admission/через existing idle worker, не между barrier
и queue handoff. Второй WAL, worker, executor и dependencies не добавлены.

- First SDK RAM gate: 4 passed / 0 failed (`l7-sdk-ram-third.log`).
- Cancellation/deadline/import RAM: 5 passed / 0 failed, 0.21s
  (`l7-sdk-controls-first.log`).
- Full unfiltered no-default lib: **880 passed**, 10 ignored, 0 failed, 0 filtered,
  67.38s (`l7b-lib-full-second.log`). Это состояние до последних control/retirement
  accounting refinements; оно не подменяет subsequent focused evidence.
- Focused после refinements: **95 passed**, 0 failed, 7.41s
  (`l7c-live-focused.log`), включая WAL ACK failure/recovery и 5×54-MiB aggregate
  queue pressure. Последующий retained Vec-capacity refinement проверен local SDK.
- SDK RAM **5 passed**, Sync **4 passed** (`l7-sdk-ram-sync-final.log`);
  NativeAsync **4 passed**, 0 failed, 4 Sync tests filtered, 0.48s
  (`l7-sdk-native-final.log`). Backend targets запускались отдельно.
- Review findings исправлены: global policy bypass, control-slot/data accounting,
  fault downgrade, unknown GAP start, post-barrier allocations и unbounded deferred
  retirement backlog. Fresh review не нашёл visibility/lock-order blocker;
  сохранённая Vec capacity учтена после последней рекомендации.

Failed compile attempts и первый Sync fixture (слишком длинное имя database directory)
не являются passing evidence. Fixture использует public schema/property API,
не расширяет visibility intern_property. На этом срезе SDK обслуживает SINCE NOW;
historical positions отклоняются явно до реализации L8, без silent NOW fallback.
Check no-default-features PASS, 214 warnings, 19.53s (`l7-check-final.log`).
Nightly rustfmt changed slice и дополнительные private tests PASS
(`l7-rustfmt-check-final.log`, `l7-rustfmt-tests-check.log`). Rustdoc public SDK
пример PASS **1/1** (`l7-rustdoc-final.log`); отдельная группа 0 tests/2 filtered
не считается evidence. Clippy global FAIL: 6961 errors; multiset comparison
с L6 — **0 added diagnostic instances** (`l7-clippy-repaired-delta.json`).
Initial Clippy добавил 5 errors/1 warning; visibility, clone, trait parameter name,
private mutability и nested condition исправлены без blanket lint suppression.
SHA-256 **41 unrelated dirty file** совпадает (`l7-unrelated-dirty.json`).
Graphify AST update PASS: 57 288 nodes/153 267 edges/1 886 communities
(`l7-graphify-final.log`). Повтор после lint repairs получил Errno 22 для `.sig`
(`l7-graphify-repaired.log`); retry PASS без topology changes
(`l7-graphify-retry.log`). Оба исхода сохранены, failure не считается passing evidence.
Общий performance/tooling closure остаётся OPEN; just all-check требуется перед commit,
commit на этом срезе не выполнялся.

## L8a/b — owned replay и общая retention

Добавлен Datastore::resume_live: origin/query/version и persisted ACK проверяются
под existing commit mutex; fixed cutoff и tail attachment фиксируются вместе.
Historical reader держит immutable history references/file pin, не primary root
или transaction. Exact result images читаются без FETCH current records.
Выданная history delivery повторяется до durable ACK; EOF не изменяет watermark.
Head holdback оплачен отдельно от volatile tail в общем session budget, поэтому
pending tail не отбирает память у replay. Typed read/resource error fuses handle,
освобождает attachment/pins и сохраняет ACK для повторного resume.
Shutdown во время unlocked decode возвращает Closed. Historical fault delivery
ограничена теми же 64 KiB, что capture; page/payload admission остаётся aggregate.

PurgeChangefeed включает computed history без второго worker/WAL. Epoch TTL,
whole groups, partial pages и empty-history floors проверяются до publication.
Fully drained partial segment удаляется, включая long→short TTL transition.
Maximum TTL scan оплачивается один раз; old replay pins сохраняют прежние файлы.
Read-only retention re-review: исправлены два findings; дальнейших concrete
COW/reachability blockers не найдено.

- SDK RAM: **7 passed / 0 failed**, 0.23s (`l8a-sdk-ram-third.log`).
- Initial focused после resume/retention: **97 passed**, 0 failed, 6.58s
  (`l8b-live-focused-first.log`).
- После control/head/TTL fixes: **101 passed**, 0 failed, 7.00s
  (`l8b-live-focused-second.log`), включая actual Sync checkpoint/reopen и pins.
- NativeAsync feature target: **106 passed**, 0 failed, 8.54s
  (`l8b-live-native-focused.log`), включая actual NativeAsync resume и prefix purge.
  Это состояние до последующего tight queue-byte refinement.
- Последующий review обнаружил incoming data slot вне byte admission; successor
  теперь измеряется после staging data. Tight-byte cases с/без history head и
  resource-error fusion PASS: **103/103**, 7.49s (`l8ab-live-focused-final.log`).
- Trusted empty history не резервирует head bank; SDK RAM **8/8**, 0.21s
  (`l8ab-sdk-ram-final.log`), rustdoc resume **1/1** (`l8ab-doc-resume.log`).
- Повтор после module/lint repairs: focused **103/103**, 7.22s; NativeAsync
  **108/108**, 10.19s (`l8ab-live-focused-repaired.log`, `l8ab-live-native-repaired.log`).
- Финальный review: исчезновение последней RAM entry или physical reference
  при сохранённом authoritative last_stamp теперь typed corruption error,
  а не silent EOF. Optional missing bytes при доверенном reference остаются GAP.
  Regression gate **105/105**, 7.23s (`l8ab-authoritative-tail-repaired.log`).
  First compile этого fixture (missing Arc import) не является passing evidence.
- Check no-default-features PASS, 211 warnings, 7.69s (`l8ab-check-final.log`).
  Clippy global FAIL: 6961 errors; **0 added diagnostic instances** относительно
  L7 (`l8ab-clippy-final-delta.json`). Required hard tabs в rustdoc имеют узкий
  method-level expect; unrelated baseline lints не подавляются.
- Nightly changed-slice rustfmt PASS (`l8ab-rustfmt-repaired.log`); first check
  указал неверный filename handle и не считается passing evidence.
  SHA-256 **41 unrelated dirty file** совпадает (`l8ab-unrelated-dirty.json`).
  Graphify AST update PASS: **57 248 nodes / 153 560 edges / 1 909 communities**
  (`l8ab-graphify.log`). Это snapshot L8a/b до последующего source-reader extraction.

Два early compile attempts не являются evidence: internal Replay visibility и
private ChangefeedConfig helper в external SDK fixture исправлены с сохранением
public API. Fresh-registration stamp/timestamp SINCE/source reconstruction — L8c,
ещё OPEN; L9–L11/paired performance/tooling closure также OPEN. Current API явно
отклонял этот starting mode на границе L8a/b; последующий L8c расширяет API.

## L8c — source history catch-up, в реализации

Первый структурный срез выделил SourceReader из transaction adapter. Один bounded
RAM/local walker обслуживает NET/FULL, inclusive timestamp, purge/corruption GAP и
metadata-only FULL preflight. Public changes() по-прежнему заимствует Transaction;
ошибка transaction lifetime также fuses reader. Commit evaluator может использовать
общий root-bound reader без открытия обычной transaction.

- No-default source/consumer gate **86/86**, 7.51s (`l8c-source-refactor.log`).
- NativeAsync source/consumer gate **88/88**, 7.78s
  (`l8c-source-refactor-native.log`), включая transaction-error fusion.

Новый catalog FGDC v13 отделяет actual defined_at от exclusive starting_after;
начальный ACK может предшествовать регистрации. Query header — 144 bytes,
верхний database header остаётся 136 bytes. Исторические source epochs сохраняют
точную revision в прежних 32 bytes. Development layout меняется на месте (Q-40).
Source group encoding использует единственную revision из CapturedCommit;
RAM и local decoding сохраняют её одинаково.

CLASS/record row-local reconstruction использует exact saved before/after через
обычный analyzer/SELECT. Root содержит только выбранный source, implicit membership,
стабильную schema и append-only dictionary; foreign records не копируются.
Syntactic gate исключает implicit record dereference, recorded reads проверяются
повторно. Frozen parameters остаются значениями исходного descriptor.

- FGDC/watermark focused foundation: **105/105**, 8.13s
  (`l8c-watermark-live.log`), check PASS (`l8c-watermark-check.log`).
- Row-local direct oracle: **3/3**, 0.02s (`l8c-row-focused-schema.log`).
- Transaction register → catch-up → ACK → tail: **4/4**, 0.03s
  (`l8c-materialization-focused.log`), check PASS
  (`l8c-materialization-check-repaired.log`).
- Initial SDK SINCE wiring check PASS, 211 warnings, 7.58s
  (`l8c-sdk-check.log`). Новые focused SDK/local gates выполняются отдельно.

subscribe_live/reregister_live теперь принимают stamp/timestamp SINCE. Narrow
registration replay и fixed cutoff готовятся/допускаются до WAL barrier; после
publication SDK только переносит owned references. Local staging сохраняет
исходные stamp/time нескольких historical groups даже без события регистрации.
Executable IR по-прежнему не сериализуется.

Review выявил timestamp floor, выбранный по первой непустой batch; потерю coverage
при OFF; пропуск preflight при пустом source; поздний work accounting и повторную
оплату growing COW map. Исправление использует proven older timestamp boundary,
eligibility до iteration, shared remaining work, отдельный source-image bank,
reusable per-group SQL scratch и одну linear COW admission. Unsupported historical
inputs оставляют unavailable interval до actual registration, включая final OFF.
Future timestamp проверяется также у current tail.
External CLOSE сохраняет оплату private registration reader, пока idle handle
не освободит его через poll/Drop. Проверки этих исправлений записываются после
завершения focused run; compile attempts не являются passing evidence.

Последующий coverage/SDK/local gate: **12/12**, 1.52s
(`l8c-coverage-sdk-focused-repaired.log`). Включает 512 small commits в 8-MiB scratch,
future timestamp, registration-commit DML, OFF timestamp prefix, пустой source
с внешним FETCH, Sync SDK replay после clearing RAM history, checkpoint/COW/reopen.
Первый run остановился на visibility внутреннего helper; это не passing evidence.
Regression gate того же состояния: LIVE **118/118**, 9.08s;
CHANGEFEED **87/87**, 9.48s; catalog **18/18**, 0.79s
(`l8c-live-regression.log`, `l8c-changefeed-regression.log`, `l8c-fgdc-regression.log`).

Следующий aggregate slice восстанавливает complete CLASS domain из пустого creation
anchor: INITIAL schema, capture с creation commit, сохранённый prefix без OFF/purge.
NET before проверяется через same_image, целый commit применяется к private logical
image map. COUNT/SUM исполняются тем же analyzer/SELECT; identity — Aggregate.
Current frozen source используется только как consistency witness, не заполняет
утраченные исторические значения. Scratch root-builder общий с row-local consumer;
domain/image/result ownership и source read work имеют aggregate admission.
Focused aggregate foundation: **14/14**, 1.44s
(`l8c-aggregate-focused-repaired.log`), включая COUNT/SUM с неизменившимися
members, empty-domain result и явный GAP без creation anchor. Первый compile
run выявил ошибки вызова PropertyMap::get и inference closure Result; исправлен,
не является passing evidence.
Read-only review подтвердил сохранение private replay charge и отказ выдавать
head, декодированный через external CLOSE. Выявлены два дополнительных остатка:
shared SourceReader EOF обязан достичь authoritative last_stamp (даже при
хвосте 9→10→9), а retained epoch Vec должен резервировать оплаченный count
без бесплатного геометрического роста. Их regression gate выполняется отдельно.

Оба замечания исправлены. EOF/aggregate gate **15/15**, 1.75s
(`l8c-aggregate-eof-focused.log`). Следующий review нашёл cumulative charge
временного dictionary context на каждый historical member. Теперь sequential
insertions делят одну peak reservation, work по-прежнему оплачивается для каждого
clone. Regression с 512 unchanged members / 64 dictionary names / 8-MiB scratch
и atomic rejection при 64 KiB проходит: **16/16**, 2.00s
(`l8c-aggregate-scratch-focused.log`).

После SDK close-race и local aggregate checkpoint/COW/reopen additions:
LIVE **124/124**, 9.81s (`l8c-aggregate-live-regression.log`),
CHANGEFEED **90/90**, 7.29s (`l8c-aggregate-changefeed-regression.log`),
catalog **18/18**, 1.02s (`l8c-aggregate-fgdc-regression.log`).
Check no-default-features PASS, 211 warnings, 7.70s
(`l8c-aggregate-check.log`). NativeAsync, scoped rustfmt и Clippy delta этого
состояния проверяются отдельно. SHA-256 всех **41 unrelated dirty file** совпадает
(`l8c-unrelated-dirty.json`).

NativeAsync того же production состояния: **131/131**, 12.87s
(`l8c-aggregate-native-regression.log`), включая отдельный actual transport test
historical SUM → checkpoint/COW/reopen → persisted ACK.
Дополнительный WAL registration gate: **1/1**, 0.78s
(`l8c-registration-wal-focused.log`). Четыре точки BeforeAppend/AfterAppend/
AfterFlush/AfterSync проверяют atomic descriptor + several historical groups:
BeforeAppend сохраняет прежний catalog; reachably appended commit восстанавливает
новую identity с исходными stamp/time. Source data до регистрации не меняются.
Read-only re-review fixes PASS, remaining concrete blockers aggregate slice не найдено.

Lint/module repairs сохраняют поведение: LIVE **125/125**, 11.43s
(`l8c-live-lint-repaired.log`); nightly rustfmt changed slice PASS
(`l8c-rustfmt-repaired.log`). Clippy global FAIL: **6961 errors**,
**0 added diagnostic instances** относительно L7
(`l8c-clippy-repaired-delta.json`). Первое сравнение было выполнено до закрытия
writer handle JSONL и не является evidence; итоговое сравнение читает закрытый log.
Graphify AST update PASS: **57 346 nodes / 153 875 edges / 1 900 communities**
(`l8c-graphify.log`); это snapshot до последующих module/FETCH edits.

Следующий complete-domain consumer — исторический FETCH и implicit navigation
к members того же CLASS, включая negative references. Saved images проверяются
на foreign/graph references до evaluation; фактические recorded reads остаются
ограниченными доказанной областью. Это расширение использует тот же root-builder,
SELECT/FETCH и aggregate kernels, не отдельный evaluator. Первый focused gate —
**20/20**, 2.94s (`l8c-fetch-domain-focused-repaired.log`). Следующий gate включает
CLASS и Record sources, linked-only writes, negative FETCH, delete/recreate,
link+target changes одного commit и COUNT по implicit navigation: **21/21**,
2.79s (`l8c-fetch-record-focused-repaired.log`). `ImageSelection::CompleteDomain`
сохраняет все linked members; changed-row режим по-прежнему исключает чужие rows
для Record source. Первый COUNT oracle ошибочно читал manager.age на rows без
manager; gate FAIL **20 passed / 1 failed** (`l8c-fetch-record-focused.log`).
Условие ordinary SELECT и SDK уточнено одинаково; evaluator semantics не менялись.
Read-only coverage/resource review этого bounded slice PASS.

Local Sync FETCH checkpoint/reopen и общий LIVE regression: **128/128**, 11.59s
(`l8c-fetch-live-regression.log`). Fresh no-default-features check PASS,
211 warnings, 19.02s (`l8c-fetch-check.log`); scoped nightly format check PASS
(`l8c-fetch-fmt.log`). NativeAsync **136/136**, 11.62s
(`l8c-fetch-native-regression.log`), включая отдельный actual NativeAsync FETCH
checkpoint/reopen target. Clippy global FAIL: **6961 errors**,
**0 added diagnostic instances** (`l8c-fetch-clippy-repaired-delta.json`).
Graphify update PASS: **57 355 nodes / 153 907 edges / 1 884 communities**
(`l8c-fetch-graphify.log`); intermediate snapshot, до окончательных L9 изменений.

## L9a — текстовая frozen-регистрация

Parser → analysis → logical recipe → existing transaction adapter проводит
standalone `LIVE SELECT` и `LET $sub = LIVE SELECT`. Frozen LET/execute captures
читаются из runtime slots при регистрации, включая block-local LET. DIFF/PATCH
имеют собственный syntax output mode и implicit complete-record projection.
Session допускает inactive consumers и handle capacity перед consuming commit;
после publication `take_live_subscription(id)` отдаёт owned pull handle,
который переживает session. CANCEL и TRY восстанавливают registration journal.

Первый compile fixture имел неоднозначный `Into<Value>`; после исправления focused
gate **4 passed / 1 failed**, fixture использовал неверный CATCH syntax.
Исправленный focused gate **7/7**, 0.07s (`l9a-gql-gate.log`): frozen captures,
block LET, explicit COMMIT/CANCEL/TRY, SINCE COUNT с Aggregate identity,
invalid options, alias qualification и DIFF/PATCH.
Первый общий LIVE regression **144/144**, 11.41s (`l9a-live-regression.log`).

Read-only review нашёл alias/property collision, повторное SDK снятие class prefix,
RAW GRAPH metadata/occurrence domain divergence и позднюю allocation admission.
Converter снимает только schema-proved qualifier; bare/nested properties сохраняются.
RAW GRAPH metadata пока explicit unsupported, CLASS IN GRAPH остаётся occurrence
source. Общий borrowed schema helper, target/alias footprint и repeated rich types
оплачиваются до ordinary typed SELECT; literal/FETCH capacity — до decode/allocation.
Intermediate hardening gate **145 passed / 1 failed**: SDK regression использует
class-qualified path. Lowerer уточнён: declared property с именем class имеет
приоритет. Repair regression и parser/SELECT checks выполняются отдельно.

Repair alias/property/admission gate: **146/146**, 12.34s
(`l9a-admission-repaired-live2.log`); parser **26/26**, 0.17s
(`l9a-parser-regression.log`), SELECT **34 passed / 2 ignored**, 1.02s
(`l9a-select-regression.log`). Private compiler/runtime наследуют cancellation и
absolute deadline; частичный scan не публикует регистрацию. Gate **148/148**,
10.11s (`l9a-controlled-live-regression.log`).

Textual preflight использует private bounded compiler. До анализа копируются только
оплаченные lexical types, schema/function metadata и syntax; compiler work учитывается
отдельно. Проверенный PURE plan SINCE/BUFFER разделяется с session через Arc, поскольку
option исполняется после уничтожения private compiler. Query recipe компилируется
через SDK с собственным admission. Oversized first-use function bodies отклоняются
до filling session cache. Final L9a LIVE **150/150**, 10.85s
(`l9a-preflight-live-regression.log`); check PASS, scoped nightly rustfmt PASS.
Clippy global FAIL **6961 errors**, **0 added diagnostic instances**
(`l9a-preflight-clippy-delta.json`). Initial lint failures и неуспешные compilation
fixtures не являются passing evidence. Analysis/schema owners перенесены в `mod.rs`;
при сравнении применены только два проверенных path remaps, baseline сохранён.
Fresh read-only L9a re-review PASS.

Отдельные integration targets проводят текстовую COUNT-регистрацию с frozen LET,
PURE BUFFER, source change → checkpoint/reopen → saved-result replay → durable ACK.
Local Sync **1/1**, 0.20s (`l9a-gql-local-sync-repaired2.log`);
NativeAsync **1/1**, 0.27s (`l9a-gql-local-native.log`). Первые compile fixtures
использовали неверные методы/schema identity/Duration API; после исправления оба
targets используют production public API. L9a focused gate закрыт; общий L9 открыт.

## L9 — INFO/KILL control slice

`INFO FOR LIVE $id` возвращает snapshot metadata и точный Bytes cursor через
обычный analyzer/IR/SDK. `KILL $sub` закрывает LIVE транзакционно, включая Pending
и повторно Closed identities; существующий maintenance job KILL сохраняется.
Routing обновляет ReadCommitted до проверки identity. Registry errors не
маскируются под job lookup; CANCEL и TRY сохраняют прежнюю подписку/consumer.
Direct INFO/KILL допускают graph-map snapshot overlap перед clone и не подключают
physical indexes, включая refresh. Metadata/dependency/output copies имеют
caller admission; общий nested-statement resource gate остаётся открытым.

Initial focused GQL gate **15/15**, 0.11s (`l9-control-gql-focused.log`). Review
нашёл обычный data-writer preflight перед close: он запрещал GQL control во время
import lease. `atomic_live_control` использует узкий control preflight и savepoint
без capture journal. Исправленный import gate **1/1**, 0.02s
(`l9-control-import-repaired.log`): CANCEL/TRY, close, два private segments и Closed
после END. Первый fixture compile с неверным ImportReport method не считается
evidence. Fresh source review PASS. Общий LIVE **155/155**, 10.11s
(`l9-control-live-regression.log`). Широкий GQL gate **221 passed / 1 failed / 4 ignored**,
34.98s (`l9-control-gql-regression.log`): cursor cancellation failure воспроизведён
отдельно. Новый ранний check в общем snapshot callback перехватывал SELECT до
закрытия supplied START AFTER cursor. Check ограничен metadata-only командами;
обычный SELECT достигает своего cleanup. Repair **1/1**, 0.02s
(`l9-control-cursor-cancel-repaired.log`); широкий GQL **222 passed / 4 ignored**,
33.16s (`l9-control-gql-repaired.log`). Check PASS (211 warnings, 7.74s), Clippy
global FAIL **6961 errors**, **0 added diagnostic instances**
(`l9-control-clippy-delta.json`). Initial failure не считается passing evidence.

Ordinary `ALTER LET` initial gate **2/2**, 0.03s; LIVE **157/157**, 17.87s;
GQL **224 passed / 4 ignored**, 46.70s (`l9-alter-let-*-regression.log`).
Sync/NativeAsync text registration → frozen ALTER → INFO → KILL → reopen
**1/1** каждый (0.31s / 0.27s; `l9-controls-local-*.log`). Fresh review нашёл
dynamic Any assignment без runtime type check и потерю annotation option<int>.
Typed slot IR теперь сохраняет expected Type, общий validator проверяет контейнеры,
immutable lexical type metadata переживает session calls/savepoints. Autocommit
type export допускается до commit; explicit transaction locals не выходят в session.
Type-fix gate закрыт: dynamic/annotation **5/5**, 0.04s
(`l9-alter-let-types-closure.log`); borrowed container/admission/cancellation **2/2**,
0.00s (`l9-alter-let-validator-closure.log`); parser **26/26**, 0.19s
(`l9-alter-let-parser-closure.log`). До финальной option-widening поправки широкий
LIVE **160/160**, 9.75s, полный `gql::` **266 passed / 4 ignored**, 38.56s
(`l9-alter-let-types-live.log`, `l9-alter-let-types-gql.log`). Final focused проверяет
option<int> copy/self-assignment и option<number> ← option<int> для integer/NONE,
включая PURE body. Closure signature проверяется borrowed, diagnostic не копирует
rich type. Fresh source re-review PASS. Check PASS (211 warnings, 14.27s); Clippy
global FAIL **6961 errors**, **0 added diagnostic instances**
(`l9-alter-let-types-clippy-repaired-delta.json`). Initial fixture/lint failures
сохранены отдельно и не считаются passing evidence.

## L9 — ACK frontend closure

`ACK LIVE $sub CURSOR expression` подтверждает только delivery retained consumer
текущей session. `GqlSession::next_live` позволяет прочитать её без извлечения handle;
после `take_live_subscription` ACK принадлежит Rust handle. Private issuance witnesses
нормализованы по confirmed watermark, сопровождаются savepoints и повторно проверяются
перед commit. Cursor cache session обновляется после успешной publication.
Conditional expressions и record navigation используют bounded RootRead/SSI.
Typed runtime effects позволяют implicit autocommit mutation и сохраняют explicit
READ ONLY; declared STABLE callback wrapper не скрывает mutating callback.

Focused ACK **7/7**, 0.07s (`l9-ack-cursor-navigation.log`); широкий LIVE **167/167**,
11.37s (`l9-ack-live-regression.log`). После финального разделения timeout/lexical
persistence owners полный `gql::` **272 passed / 4 ignored**, 31.01s
(`l9-ack-gql-closure-repaired.log`), parser **22/22**, 0.02s
(`l9-ack-parser-closure.log`). Check PASS (211 warnings, 22.78s;
`l9-ack-check.log`). Local Sync **1/1**, 0.15s (`l9-ack-local-sync-repaired.log`),
NativeAsync **1/1**, 0.19s (`l9-ack-local-native.log`): GQL registration, CANCEL,
expression ACK, generation build, checkpoint/reopen и confirmed resume.
Первый local fixture превысил 31-byte database-name limit; prefix исправлен.
Промежуточные compile/type/lint failures сохранены отдельно и не считаются evidence.
Final Clippy global FAIL **6961 errors**, **0 added diagnostic instances**
(`l9-ack-clippy-final-delta.json`). Fresh ACK source re-review PASS.
Несвязанные baseline dirty files **41/41** неизменны (`l9-ack-preserved.json`).

## L9b — reactive GQL core, 2026-10-06

Реализованы LIVE LET/ALTER/INFO/REMOVE через существующий pipeline и SDK registry.
Lexical metadata хранит reactive flavor отдельно от Value; slot удерживает UUID
identity, а read разрешает её через exact snapshot и SSI. Closure захватывает
identity, ordinary shadow маскирует flavor. Execute UUID не превращается в value
lookup. Ordinary inputs frozen, parameter-only commit меняет LIVE result membership.
Savepoints/TRY/CANCEL сопровождают registry и lexical metadata совместно.
SINCE/BUFFER используют общий RootRead с ACK operands и фиксируются при регистрации.

Initial core compile/check PASS (`l9b-check-repaired.log`), focused **3/3**
(`l9b-binding-focused-repaired.log`). До review repairs общий GQL **275 passed /
4 ignored**, 27.00s (`l9b-gql-core-regression.log`). После первых repairs SDK/catalog
bindings **14/14**, 0.74s (`l9b-binding-review-repaired.log`), GQL core **5/5**,
2.31s (`l9b-gql-binding-review.log`). Options/index slice **6/6**, 1.80s
(`l9b-gql-binding-options.log`). Final LIVE slice **167/167**, 12.11s
(`l9b-live-closure.log`), включая parser UTF-8 boundaries. Local Sync **1/1**,
0.32s (`l9b-binding-local-sync.log`), NativeAsync **1/1**, 0.24s
(`l9b-binding-local-native.log`): text registration, parameter-only delivery,
ACK, generation replacement, checkpoint/reopen, SDK read и GQL administrative
UUID input. Final Clippy global FAIL **6961 errors**, **0 added diagnostic
instances** (`l9b-clippy-repaired-delta.json`). Первые пять новых errors и
cognitive-complexity warning исправлены; initial logs не являются passing evidence.
Final compile PASS (211 warnings, 8.78s; `l9b-check-final.log`);
полный `gql::` **278 passed / 4 ignored**, 40.63s (`l9b-gql-final.log`).
Промежуточные ошибки compilation fixtures и новые lint diagnostics сохранены
отдельно; они не считаются passing evidence. Полнота L9b не закрыта.

| Finding           | Решение / текущий evidence                                                                                                           |
|-------------------|--------------------------------------------------------------------------------------------------------------------------------------|
| L9b-INFO-OVERLAP  | Accepted; INFO metadata guard жив до output projection, vector admission regression PASS                                             |
| L9b-PAUSED-ERROR  | Accepted; общий borrowed reader сохраняет typed FG-G1F09/cause, old snapshot test PASS                                               |
| L9b-REMOVE-BUDGET | Accepted; controlled GQL staging использует caller caps/checks, tiny-cap atomic rejection PASS                                       |
| L9b-SCALAR-RECOPY | Accepted; scalar copies и collection clones используют shared admission, index/Field перемещают owned cells; focused/final LIVE PASS |

Fresh source re-review принял четыре fixes в указанной clone/staging boundary.
Дополнительное замечание cast compatibility принято: прежнее quoted JSON rendering
сохранено, formatter scratch допускается до allocation. Shared value admission имеет
двух реальных consumers: RootFields и reactive statement reads; новый executor отсутствует.
Account conservatively жив до statement end, возможный отказ длинных scalar loops
отмечен как ресурсное ограничение; произвольное расширение лимитов не выполняется.
Несвязанные README/asynchronous/collections baseline files **43/43** неизменны
(`l9b-preserved.json`, scope этого hash check включает два дополнительных файла).
Graphify update PASS: **57 602 nodes / 154 794 edges / 1897 communities**
(`l9b-graphify.log`), AST-only, без нового semantic labeling. Два non-code JSON
inputs дают zero nodes; это diagnostic инструмента, не correctness evidence.

LIMIT/START OFFSET обычного SELECT теперь получают mandatory snapshot fields
через общий checked usize evaluator. Direct/closure/derived/nested regression
PASS **1/1**, 0.05s (`l9b-pagination-final.log`), cursor cancellation PASS **1/1**,
0.03s (`l9b-pagination-cursor-cancel.log`). Compile-check PASS, 211 warnings,
5.93s (`l9b-pagination-check.log`). Clippy baseline **6961 errors**, delta **0**
(`l9b-pagination-clippy-delta.json`). Три предыдущих отказа fixtures относятся
к недопустимому SELECT в closure RETURN, ambiguous nested field и пока
не представленному unary negative LIVE recipe; passing evidence использует
допустимые alias/capture/arithmetic формы, не меняя эти contracts.

MATCH record-key selectors используют существующий MatchPredicateContext/fields
для seed, fixed/path expansion, HYPER CONTAINS и WITHIN. Focused PASS **1/1**,
0.03s (`l9b-match-repaired.log`), check PASS, 211 warnings, 5.82s
(`l9b-match-check.log`); Clippy delta **0** при baseline **6961 errors**
(`l9b-match-clippy-delta.json`). Initial compile fixture errors сохранены отдельно.
Graph/MAXDEPTH concretization не считается закрытой этим срезом.

Fulltext/KNN prepare использует mandatory snapshot fields. Динамический `any`
не меняет numeric/string/vector semantics: query types и vector element kind
проверяются до source rows; K/EF остаются checked positive integers. Numeric
MINKOWSKI допускает STABLE snapshot input, executable metric сохраняет PURE.
RAM search/admission PASS **2/2**, 0.07s (`l9b-search-admission-udf.log`);
Local Sync physical Fulltext/HNSW PASS **1/1**, 0.34s
(`l9b-search-local-sync.log`), NativeAsync **1/1**, 0.36s
(`l9b-search-local-native.log`). RAM forced index отказ выявил неподходящий
fixture: physical binding проверяется отдельно на local backends; отсутствие
RAM binding не обходится. Initial grammar/forced-index failures не являются
passing evidence.
После UDF forwarding повторный Local Sync PASS **1/1**, 0.31s
(`l9b-search-local-sync-final.log`); NativeAsync уже проверяет этот final source.

Review finding **L9b-SEARCH-QUERY-ADMISSION** принят и исправлен: bounded analyzer,
UDF snapshot fields/argument admission и term insertion используют existing
scratch/work pool. Fresh re-review PASS; empty class scratch/work rejection
и PURE UDF work rejection входят в focused gate. Per-document analyzer policy
не изменена; broad resource gate этим не закрыт. После финального forwarding
общий GQL PASS **282 passed / 4 ignored**, 31.18s (`l9b-inputs-gql.log`),
compile-check PASS, 211 warnings, 6.99s (`l9b-inputs-check.log`). Nightly format
check PASS для десяти changed Rust files (`l9b-inputs-format-check.log`).
Final Clippy global FAIL **6961 errors**, **0 added diagnostic instances**
(`l9b-inputs-clippy-delta.json`); tooling gate остаётся OPEN.
Несвязанные baseline files **43/43** неизменны (`l9b-inputs-preserved.json`).
Graphify update PASS: **57 611 nodes / 154 828 edges / 1901 communities**
(`l9b-inputs-graphify.log`); AST-only, без semantic labeling.

INFO/SHOW CHANGES id/source/SINCE/LIMIT читают binding из QuerySnapshot с SSI.
Scalar-only navigation сохранена; operand account удерживается вместе с metadata
и history reader. Initial replacement walker вызвал два depth regressions
(`l9b-feed-input-regressions.log`, 12/14); он заменён existing image walker
с per-step caller work/cancellation hook. Final changefeed filter PASS **91/91**,
8.05s (`l9b-feed-input-restored.log`), fresh review PASS. Compile-check PASS,
211 warnings, 6.75s; nightly scoped check PASS. Clippy **6961 errors**, delta **0**
(`l9b-feed-input-clippy-delta.json`, JSON messages отфильтрованы от Cargo stderr).
Ошибка fixture Vec<u8> → Value исправлена на existing Bytes conversion.

Полнота input seams (START AFTER/deferred scope/TIMEOUT/admin),
general recipes, graph scopes и общие L8–L11 gates остаются OPEN.

Owned Array/Object и numeric Negate/Positive проходят ordinary analyzer/evaluator;
object keys сохраняют порядок и duplicate last-value semantics. Explicit FGLD
tags 8–11, checked lengths/depth/work и canonical re-encoding проверены отдельно.
Final LIVE filter PASS **175/175**, 12.71s (`l9b-recipes-live-regressions.log`),
GQL **285 passed / 4 ignored**, 33.34s (`l9b-recipes-gql.log`), Rustdoc **2/2**
(`l9b-recipes-rustdoc.log`). Historical compound projection читает captured
17→19 images, хотя current primary уже 99; SDK large array отклоняется work cap.
Compile-check PASS, 211 warnings, 8.14s (`l9b-recipes-check.log`); scoped nightly
format PASS для 11 changed Rust files. Clippy global FAIL **6961 errors**,
**0 added diagnostic instances** (`l9b-recipes-clippy-delta.json`).

Backend SDK scenario выявил **L7-ACK-STATISTICS**: ACK после reopen оставлял
deferred root, а catalog-only publication удаляла immutable physical statistics
binding. Исправлен общий `persist_graph_catalog`; full generation по-прежнему
сбрасывает binding. Final Sync **1/1**, 0.14s
(`l9b-recipes-local-sync-statistics.log`), NativeAsync **1/1**, 0.14s
(`l9b-recipes-local-native.log`): SDK без GQL recipe, owned images/cursor,
reopen→resume→ACK→mutation. Fresh storage re-review PASS.
Codec finding **L9b-CODEC-GOLDEN** закрыт fixed tag and ordered-key goldens;
fresh re-review PASS. Initial compile E0004 и fixture borrow/equality errors,
затем реальный statistics failure сохранены отдельно и не считаются PASS.

Access/Index проходят ordinary expression pipeline: reactive/frozen parameters,
alias/null-safe row fields и external Record-valued bindings. Initial focused
**6/7** (`l9b-access-focused.log`) выявил лишний GAP для Object-constructor Access:
complete-domain reconstruction не имела INITIAL schema anchor. Row-only gate
теперь допускает только гарантированный Object constructor с recursive proof;
Record-valued bases сохраняют conservative coverage. Repaired focused **7/7**,
0.07s (`l9b-access-focused-fixed.log`); с field-cap и foreign-history checks —
**7/7**, 0.07s (`l9b-access-admission.log`). Внешний property update создаёт
18→21 delivery; frozen foreign Record SINCE явно возвращает InsufficientCoverage.
Oversized 1 MiB field name при 128 KiB cap отклоняется typed resource error.
Fresh source review PASS. Low findings **L9b-ACCESS-CODEC-SPEC** и
**L9b-INDEX-GOLDEN-ORDER** закрыты spec tags12/13 и distinct-child fixed golden;
fresh re-review PASS. Final LIVE **178/178**, 14.49s (`l9b-access-live-final.log`),
GQL **287 passed / 4 ignored**, 37.94s (`l9b-access-gql-final.log`), Rustdoc **2/2**
(`l9b-access-rustdoc.log`). SDK Sync **1/1**, 0.17s (`l9b-access-local-sync.log`),
NativeAsync **1/1**, 0.15s (`l9b-access-local-native.log`) включают exact compound
recipes, reopen/resume/ACK и post-ACK mutation. Compile-check PASS, 211 warnings,
5.62s; scoped nightly format PASS для десяти changed Rust files. Clippy global
FAIL **6961 errors**, **0 added diagnostic instances** (`l9b-access-clippy-delta.json`).
Несвязанные baseline files **43/43** неизменны (`l9b-access-preserved.json`).

Graphify update после Array/Object среза FAIL (`l9b-recipes-graphify.log`): AST
extraction завершена, но rebuild отказал на записи `.graphify_labels.json.sig`
с `[Errno 22] Invalid argument`. Этот run не является passing tooling evidence;
Повторный AST-only update для final Access/Index source PASS
(`l9b-access-graphify-retry.log`): **57 628 nodes / 154 902 edges / 1900 communities**.
Ошибка signature не повторилась; installed Graphify не изменялся. Semantic labeling
не запускался; первоначальный failed run не подменяется passing evidence.

Историческая reconstruction внешних FETCH, более общих aggregates, graph scopes и LIVE LET
ещё требует реализации и отдельного evidence. Current active aggregate queries
с отдельной identity уже входят в L4; согласованный historical объём не сокращён.
L8c, L9–L11 и общие correctness/resource/performance/tooling gates остаются OPEN.

START AFTER использует statement snapshot fields и общий control при выборе
обычного session cursor (`live_gql_binding_start_after_selection`, focused 1/1).
Initial GQL gate 287 passed / 1 failed / 4 ignored выявил early cancellation до
получения cursor: supplied spool оставался открытым. L9b-CURSOR-EARLY-CANCEL принят:
borrowed direct Slot/Constant cursor закрывается при eval failure, computed
expression не выполняется повторно. Existing cancellation regression PASS 1/1;
final GQL **288 passed / 4 ignored**, 27.52s (`l9b-cursor-gql-repaired.log`).
Check PASS, 211 warnings, 6.34s; scoped nightly format двух Rust files PASS.
Fresh source re-review PASS; Clippy global FAIL 6961 errors / 547 warnings,
**0 added instances** (`l9b-cursor-clippy-delta.json`); unrelated files 43/43
сохранены. Graphify initial update FAIL на `.graphify_labels.json.sig`
(`l9b-cursor-graphify.log`), retry проверяется отдельно.
AST-only retry PASS (`l9b-cursor-graphify-retry.log`): CLI не обнаружил новых
code-graph topology changes и оставил outputs untouched. Initial failed run сохранён.

Deferred MATCH MAXDEPTH удерживает typed lexical operand; invocation разрешает его
через statement snapshot/SSI и проверяет 0..=30 до planner/row pull. Logical patterns
не клонируются: open range сохраняет provenance, explicit maximum независим.
Runtime CBO, path cursor и direct EXPLAIN/PlanHash используют один bound; outer
embedded estimates до child invocation используют upper bound 30 с warning.
Initial focused FAIL (l9b-depth-focused.log) был fixture mismatch: tv::count
возвращает array, test ожидал scalar. Repaired focused 1/1, 0.04s.
Review finding L9b-MAXDEPTH-EMBEDDED-WARNING принят: общий EXPLAIN helper
рекурсивно проецирует GraphResult source warnings для JSON/text. EXPLAIN BUILD GRAPH
regression и fresh source re-review PASS; final focused 1/1, 0.05s
(l9b-depth-focused-reviewed.log). Final GQL 289 passed / 4 ignored, 32.63s
(l9b-depth-gql-reviewed.log). Scoped nightly format девяти Rust files PASS;
unrelated baseline files 43/43 неизменны (l9b-depth-preserved.json).
Check PASS, 211 warnings, 20.31s (l9b-depth-check.log). Clippy global FAIL
6960 errors / 547 warnings, 0 added diagnostic instances (l9b-depth-clippy-delta.json).
Deferred GRAPH и остальные L9 input seams ещё OPEN. Graphify AST-only update PASS
(l9b-depth-graphify.log): 57 636 nodes / 154 936 edges / 1900 communities;
graph.json, graph.html и GRAPH_REPORT.md пересобраны без semantic API calls.

Owned IF recipe использует ordinary Conditional lowering/evaluator: runtime
вычисляет выбранную ветвь, capture/DAG/admission проверяют все три children.
Tag 14 хранит condition/then/else в фиксированном порядке. Historical fast path
требует proof всех children и actual-read whitelist; foreign/cross-level inputs
не получают ложную coverage из неактивной ветви. Два fresh source reviews PASS.
Frozen integer >2^53, truthiness, inactive divide-by-zero, cycles/effects/resource
rejection и зависимость от record при смене branch проверены. Fixed golden,
truncated/corrupted codec и historical 17→19 при current 99 также PASS.

| IF gate                     | Evidence                                                                                                                |
|-----------------------------|-------------------------------------------------------------------------------------------------------------------------|
| Focused                     | 9/9, 0.13s; `l9b-if-focused-reads.log`                                                                                  |
| LIVE / GQL                  | 190/190, 12.02s; 290 passed / 4 ignored, 31.31s; `l9b-if-live.log`, `l9b-if-gql.log`                                    |
| Rust SDK Sync / NativeAsync | Separate targets 1/1, 0.16s / 0.19s; reopened resume→ACK→branch switch; `l9b-if-sdk-sync.log`, `l9b-if-sdk-native.log`  |
| Rustdoc / check             | 5/5 examples; check PASS, 211 warnings, 7.86s; `l9b-if-rustdoc.log`, `l9b-if-check.log`                                 |
| Format / preservation       | Scoped nightly check 10 files PASS; protected bytes 43/43 unchanged; `l9b-if-format-check.log`, `l9b-if-preserved.json` |
| Clippy                      | Global FAIL 6960 errors / 547 warnings, added diagnostic instances 0; `l9b-if-clippy-delta.json`                        |

CASE/Cast/Set, deferred GRAPH и general history coverage остаются OPEN.
IF Graphify AST-only update PASS (`l9b-if-graphify.log`): 57 639 nodes / 154 944 edges /
1890 communities; graph.json, graph.html и GRAPH_REPORT.md пересобраны без semantic calls.

Deferred MATCH GRAPH удерживает LogicalGraphReference; snapshot/SSI validation
GraphRef-only operand выполняется до demand/CBO/open/row pull. Borrowed logical
patterns сохраняются. Standalone/embedded MATCH, graph assembly и traversal analytics
используют тот же qualified graph. Unbound outer estimates имеют unknown bounds,
warning и absent GraphId; physical encoding v8 включает resolved GraphId, EXPLAIN
отражает binding. Fresh source review PASS.

Finding **L9b-GRAPH-TENSOR-DEFERRED** принят и закрыт: outer ALL SHORTEST ошибочно
показывал ON_DEMAND/ROOTED без разрешённого graph. Scope теперь unbound, format DYNAMIC,
index identity отсутствует. Runtime child replan до исправления сохранял точность;
новая EXPLAIN regression и fresh re-review PASS.

Initial fixture failures сохранены отдельно: `l9b-graph-focused.log` и
`l9b-graph-focused-repaired.log` — неверный read method/trait; `l9b-graph-focused-final.log`
— чужая session lexical identity; `l9b-graph-focused-policy.log` и
`l9b-graph-focused-reviewed.log` — неверные parenthesized/RETURN формы TRAVERSE.
Это failed evidence, не результаты прохождения gate. Исправленный snapshot oracle
меняет reactive graph через DML зависимости, без обхода lexical identity contract.

| MATCH GRAPH gate | Evidence                                                                                                           |
|------------------|--------------------------------------------------------------------------------------------------------------------|
| Focused          | 1/1, 0.04s; `l9b-graph-focused-valid.log`                                                                          |
| GQL / LIVE       | 291 passed / 4 ignored, 30.72s; 191/191, 14.58s; `l9b-graph-gql.log`, `l9b-graph-live.log`                         |
| Check / format   | Check PASS, 211 warnings, 8.84s; scoped nightly 12 files PASS; `l9b-graph-check.log`, `l9b-graph-format-check.log` |
| Preservation     | Protected bytes 43/43 unchanged; `l9b-graph-preserved.json`                                                        |

Initial Clippy added 1 missing private helper documentation; comment repaired.
Final Clippy global FAIL 6960 errors / 546 warnings, added instances 0
(`l9b-graph-clippy-repaired-delta.json`). Graphify initial AST-only update PASS
(`l9b-graph-graphify.log`): 57 644 nodes / 154 976 edges / 1903 communities;
graph.json, graph.html и GRAPH_REPORT.md rebuilt. Последующий documentation-only
helper comment включается в очередной incremental update.
General dynamic LIVE source descriptors, CASE/Cast/Set,
TIMEOUT/admin inputs и general historical coverage остаются OPEN.

Owned Set recipe сохраняет ordered inputs с duplicates в tag 15. Admission,
captures/dependencies и historical proof посещают каждый input; construction,
equality и deduplication выполняет ordinary Set evaluator. Свежий source review PASS,
новых actionable findings нет. Historical replay 17→19 при current 99, exact integer
>2^53, frozen/reactive inputs, empty Set, cycles/evaluation failure и work-cap rejection
проверены focused gate.

Initial failed evidence: `l9b-set-format.log` — два неверных пути в scoped command;
`l9b-set-focused.log` — ambiguity test-only generic expected Set; исправлена явным Value.
`l9b-set-focused-repaired.log` — ALTER LET immutable imported capture внутри BEGIN;
ordinary ALTER перенесён до BEGIN, reactive ALTER+DML остаются одним commit.
Production semantics для этих fixture/tooling failures не менялись.
Final focused `l9b-set-focused-fixture.log`: 11/11, 0.13s.

| Set gate              | Evidence                                                                                                                       |
|-----------------------|--------------------------------------------------------------------------------------------------------------------------------|
| LIVE / GQL            | 193/193, 11.37s / 292 passed, 4 ignored, 33.49s; `l9b-set-live.log`, `l9b-set-gql.log`                                         |
| SDK Sync              | Separate target 1/1, 0.16s; `l9b-set-sdk-sync.log`                                                                             |
| SDK NativeAsync       | Separate target 1/1, 0.18s; `l9b-set-sdk-native.log`                                                                           |
| Rustdoc / check       | 5/5 nonempty examples; check PASS, 211 warnings, 8.80s; `l9b-set-rustdoc.log`, `l9b-set-check.log`                             |
| Format / preservation | Scoped nightly check 10 Rust files PASS; protected bytes 43/43 unchanged; `l9b-set-format-check.log`, `l9b-set-preserved.json` |
| Clippy                | Global FAIL 6960 errors / 546 warnings, added instances 0; `l9b-set-clippy-delta.json`                                         |

Нулевая отдельная rustdoc group evidence не считается. Очередной Graphify update
включает Set и последующий CASE slice; до его окончания прохождение не заявлено.
CASE/Cast, general dynamic sources, general historical coverage и L10–L11 OPEN.

CASE initial focused gate `l9b-case-focused.log`: 13/13, 0.15s. Source review
выявил blocker **L9b-CASE-NULLABLE-TYPE**: shallow branches с repeated NONE/NULL
создавали inferred Option^N через общий unify. Compiler repro
`l9b-case-nullable-repro.log` FAIL, Option^192<Int> вместо Option<Int>.
Первое shared исправление прошло focused 14/14, 0.14s
(`l9b-case-focused-repaired.log`), однако fresh re-review выявил residual:
чередование Int/NONE создаёт Union<Int, Option<Int>>, затем новую Option.
Blocker остаётся OPEN до optional-inner normalization и расширенной regression.
Широкие `l9b-case-*` runs на промежуточном helper не считаются final closure evidence.
Residual воспроизведён отдельным `l9b-case-alternating-repro.log` FAIL, 1/1.
Final shared fix нормализует optional inner types под одной nullable оболочкой;
focused `l9b-case-final-focused.log` 14/14, 0.22s. Regression покрывает 96 triples,
обратный порядок, declared option<int> closure/PURE function и настоящий LIVE LET.
Fresh final source re-review: **L9b-CASE-NULLABLE-TYPE CLOSED**, bounded CASE PASS.
Runtime values и declared schema не изменены; canonical inferred metadata отражает
nullable domain без k-dependent nesting.

| Final CASE gate        | Evidence                                                                                                                                     |
|------------------------|----------------------------------------------------------------------------------------------------------------------------------------------|
| LIVE                   | 196/196, 13.06s; `l9b-case-final-live.log`                                                                                                   |
| GQL                    | 294 passed / 4 ignored, 38.34s; `l9b-case-final-gql.log`                                                                                     |
| SDK Sync / NativeAsync | Separate targets 1/1, 0.14s / 0.17s; `l9b-case-final-sdk-sync.log`, `l9b-case-final-sdk-native.log`                                          |
| Rustdoc / check        | 5/5 nonempty examples; check PASS, 211 warnings, 7.17s; `l9b-case-final-rustdoc.log`, `l9b-case-final-check.log`                             |
| Format / preservation  | Scoped nightly check 11 Rust files PASS; protected bytes 43/43 unchanged; `l9b-case-final-format-check.log`, `l9b-case-final-preserved.json` |
| Clippy                 | Global FAIL 6959 errors / 546 warnings, added instances 0; `l9b-case-final-clippy-delta.json`                                                |

Нулевая отдельная rustdoc group evidence не считается. CASE focused/source gates
PASS; CAST, general dynamic sources, historical coverage и L10–L11 остаются OPEN.

Initial Graphify `l9b-case-graphify.log` FAIL: `[Errno 22] Invalid argument`
для `graphify-out/.graphify_labels.json.sig`; 20/20 uncached AST files обработаны,
но полной успешной publication не заявлено. Signature metadata читается нормально
(обычный Archive file, не reparse/symlink); installed package и curated labels
не менялись. Final retry `l9b-case-final-graphify.log` PASS после final fix/source
review: 57650 nodes / 154996 edges / 1924 communities. AST 7/7 uncached files;
zero-node config.json/context7.json и community label warnings сохранены.

Owned CAST slice использует symbolic target Text и borrowed child lowering,
обычные type-name analyzer/SELECT evaluator; explicit descriptor tag 17 сохраняет
target blob и value. Captures/DAG и historical eligibility обходят value, runtime
type conversions не дублируются. Integer >2^53 и decimal остаются точными;
Option/Vector/collection targets сохраняют прежние ordinary conversion errors.

Scout finding **L9b-CAST-UTF8-ADMISSION** принят: первоначальный decode вызывал
from_utf8 перед descriptor work permit. Shared helper теперь принимает borrowed
bytes, проверяет length/work до UTF-8 validation, bracket scan и owned copy,
ограничивает generic nesting 64 и учитывает lowercase/typed copies с ancestors.
Семантический parser остаётся один. Encode сохраняет resource status; decode
добавляет typed corruption boundary с причиной.

Initial `l9b-cast-focused.log`: 16 passed / 1 failed — encoder превращал typed
depth resource error в corruption wrapper; wrapper оставлен только на decode.
`l9b-cast-final-focused.log`: 16 passed / 1 failed — regression ошибочно искала
nested work cause через Display основного status; исправлена проверка cause().
Это fixture failure: invalid UTF-8 target был отвергнут до scan. Final focused,
fresh review и широкие gates ещё выполняются; CAST closure пока не заявлено.

После UTF8 fix focused `l9b-cast-final-focused-cause.log` PASS 17/17, 0.28s.
Промежуточные `l9b-cast-final-*` broad runs PASS: LIVE 199/199, 10.97s;
GQL 296 passed / 4 ignored, 38.05s; SDK Sync/NativeAsync отдельные targets 1/1,
0.20s / 0.16s; rustdoc 5/5, check PASS 211 warnings, 10.92s. Graphify PASS
57655 nodes / 155011 edges / 1901 communities. Эти runs предшествуют следующему
resource finding и не являются final closure evidence для CAST.

Fresh review: **L9b-CAST-UTF8-ADMISSION CLOSED**; выявлен новый blocker
**L9b-CAST-FAILURE-RENDER (High/resource)**. Failed numeric/unsupported cast
форматировал compound Value через to_string без renderer permit. Общий sibling
`type::to<string>` тоже обходил scalar guard в reactive evaluation. Repro
`l9b-cast-render-repro.log` FAIL 1/1: oversized renderer успешно исполнился.
Existing guard вынесен в общий admit_cast_render; ordinary cast получает
fields/control от scalar, builtin и traversal callers. Permit выполняется перед
success String rendering и failure diagnostic construction; успешные numeric
Value/Any/Bool/Object не тратят renderer budget. Number parser создаёт собственную
diagnostic, поэтому его String input допускается до parse. Final focused/review
и широкие gates shared исправления ещё выполняются.
Initial shared fix: `l9b-cast-render-focused.log` PASS 18/18, 0.21s;
SDK failure без mutation добавлен в `l9b-cast-render-sdk-focused.log` PASS 18/18,
0.22s. Fresh review сохранил finding OPEN из-за двух residual paths: traversal
Algorithm терял RootRead bounded fields; Array/Set→Object не допускал output/key
growth и duplicate-key diagnostic. Final residual fix передаёт bounded_fields,
переносит owned keys через BTreeMap Entry вместо clone и допускает node/diagnostic
bytes через общий scalar pool. Shared helper обоснован двумя реальными consumers,
нового budget нет. Per-field full-node admission консервативен, ceiling отмечен
в коде. Expanded regression проверяет RootRead/non-reactive control, large
duplicate keys в Array/Set, успешный merge и SDK catalog atomicity; final focused
и fresh re-review ещё выполняются.
Fresh residual source review: **L9b-CAST-FAILURE-RENDER CLOSED**, bounded source
PASS, новых findings нет. `l9b-cast-render-residual-focused.log` — compile FAIL
E0425: отсутствовала qualification Text в node-size expression; исправлена
на crate::values::Text. Final focused
`l9b-cast-render-residual-qualified-focused.log` PASS 18/18, 0.33s.

Final broad runs после residual fix:

| Gate                   | Evidence                                                                                                          |
|------------------------|-------------------------------------------------------------------------------------------------------------------|
| LIVE library           | 200/200 PASS, 12.43s; `l9b-cast-render-final-live.log`                                                            |
| GQL library            | 297 passed / 4 ignored, 33.87s; `l9b-cast-render-final-gql.log`                                                   |
| SDK Sync / NativeAsync | Separate targets 1/1, 0.16s / 0.18s; `l9b-cast-render-final-sdk-sync.log`, `l9b-cast-render-final-sdk-native.log` |
| Rustdoc                | 5/5 PASS; `l9b-cast-render-final-rustdoc.log`; отдельная группа с нулём tests evidence не считается               |
| Check                  | PASS, 211 warnings, 7.31s; `l9b-cast-render-final-check.log`                                                      |

Source review и correctness gates CAST PASS. Scoped nightly format 14 files PASS;
43 protected files unchanged (`l9b-cast-render-final-preserved.json`). Graphify
`l9b-cast-render-final-graphify.log` PASS: 57657 nodes / 155025 edges / 1928 communities;
zero-node config.json/context7.json и label warnings сохранены.
Final Clippy обнаружил одну новую visibility diagnostic: redundant_pub_crate
на helper внутри private query module. Helper теперь pub внутри private module,
его re-export остаётся crate-private; публичный SDK не расширяется. Clippy delta
повторно проверен: `l9b-cast-visibility-clippy-delta.json` обнаружил impl_trait_in_params
после visibility fix; named generic Admission сохраняет contract. Final
`l9b-cast-visibility-final-clippy-delta.json`: 6959 errors / 546 warnings,
added instances 0. Общий Clippy FAIL — сохранённый baseline debt. Stage 28 не закрывается.

Owned Range decision unit начат после CAST focused correctness gate. Scout
**L9b-RANGE-BOUND-RENDER (High/resource)** обнаружил diagnostic allocation до permit
при reversed/incompatible values и отсутствие shell admission перед endpoint Boxes.
Принят narrow construction hook с единственным runtime validation predicate и
existing evaluator renderer pool; public Range semantics не меняются.
`l9b-range-render-repro.log` FAIL 1/1: DataException вместо ProgramLimitExceeded,
diagnostic уже был отформатирован. Shared construction hook сохраняет public
Range::new semantics; bounded evaluator допускает endpoint shells и failure
rendering через существующий scalar pool (общий consumer с CAST).
Spill/LIVE history/canonical StorageRange decode отклоняют malformed bounds коротким
typed corruption status до rendering. Дублирования comparison/domain predicate нет.
Tag 18 хранит checked optional flags и children. Final focused/review и Rust SDK
gates ещё выполняются; performance attempts 0/5.
Initial `l9b-range-focused.log`: 20 passed / 1 failed, 0.27s. Renderer/codec/history
tests PASS; GQL fixture повторно объявляла LIVE LET `$sample` в одном lexical scope.
Исправлены только уникальные sample names; повторное объявление остаётся ошибкой.
Final `l9b-range-fixture-focused.log` PASS 21/21, 0.34s. Fresh source review:
**L9b-RANGE-BOUND-RENDER CLOSED**, bounded Range source PASS, новых findings нет.
Scoped nightly format 15 files PASS; 43 protected files unchanged
(`l9b-range-final-preserved.json`). Full lib gate `l9b-range-final-lib.log`:
**986 passed / 10 ignored / 0 failed**, 90.20s, 0 filtered out. Затронутые ordinary
GQL, values/storage и datastore tests входят в этот run.

| Gate                   | Final Range evidence                                                                                                                             |
|------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------|
| SDK Sync / NativeAsync | Separate targets 1/1, 0.20s / 0.16s; `l9b-range-final-sdk-sync.log`, `l9b-range-final-sdk-native.log`; reopen→resume→ACK→mutation с Range result |
| Rustdoc                | 5/5 PASS, `l9b-range-final-rustdoc.log`; нулевая отдельная группа evidence не считается                                                          |
| Check                  | PASS, 211 warnings, 7.92s; `l9b-range-final-check.log`                                                                                           |
| Graphify               | PASS 57662 nodes / 155044 edges / 1922 communities; `l9b-range-final-graphify.log`; baseline zero-node/label warnings сохранены                  |

Final Clippy delta `l9b-range-final-clippy-delta.json`: 6959 errors / 546 warnings,
added instances 0. Общий Clippy FAIL — baseline debt; bounded Range gates PASS.
General recipes/dynamic sources, historical
coverage, L10/L11 и global tooling debt остаются OPEN. Performance attempts 0/5.

## L9b scalar operators / Truthy

L9b scalar operators/Truthy decision unit: добавлены 22 non-Pipeline operators
через ordinary analyzer/evaluator и explicit operator tags 14..35. Truthy —
Not(Not(value)) с admission обоих synthetic nodes; нового expression tag нет.
Finding **L9b-BINARY-REACTIVE-GUARD (High/resource)** воспроизведён:
`l9b-binary-work-repro-fixed.log` FAIL 1/1, reactive ALLIN проигнорировал work cap.
Initial `l9b-binary-work-repro.log` — compile FAIL E0308 только в fixture:
max_loop_iterations ошибочно задан NonZeroU64 вместо u64; исправлено значение
1000, production fix ещё отсутствовал при runtime reproduction.
Shared binary operand/product guard теперь использует existing bounded fields
или reactive scalar pool; cancellation перед/после kernel и внутри quantified
containment loops. Short-circuit и eager validation сохраняются. Новый account
не вводится. Focused tests, fresh source review и SDK gates выполняются;
finding до их завершения OPEN. L11 attempts 0/5.
Initial focused `l9b-operators-focused.log`: 25 passed / 1 failed, 0.48s;
guard/cancellation, spatial/codec/history PASS. Scalar fixture ошибочно ожидала
integer remainder overflow от `-9223372036854775808 % -1`: positive literal
9223372036854775808 читается Number::Decimal до unary minus. Explicit CAST
`<int>'-9223372036854775808'` задаёт именно проверяемый integer contract;
ordinary parser/numeric behavior не меняются.
Final focused `l9b-operators-fixture-focused.log` PASS **26/26**, 0.71s.
Fresh read-only source review PASS в пределах decision unit:
**L9b-BINARY-REACTIVE-GUARD CLOSED**. Minor doc finding
**L9b-NOTIS-SPELLING CLOSED**: Rustdoc указывает ordinary `NOT IS`, без parser
изменения. Все mappings/tags и capture/dependency/history mirrors проверены;
широкие gates выполняются. Scoped nightly format 7 files PASS;
43 protected files unchanged (`l9b-operators-final-preserved.json`).
Broader run до mechanical guard extraction: `l9b-operators-final-lib.log`
PASS **991 passed / 10 ignored**, 81.26s, 0 filtered out. SDK Sync/NativeAsync
separate targets PASS 1/1 каждый, 0.17s/0.17s; `l9b-operators-final-sdk-sync.log`,
`l9b-operators-final-sdk-native.log`. Rustdoc 6/6 PASS,
`l9b-operators-final-rustdoc.log`; отдельная нулевая группа не evidence.
Check PASS, 211 warnings, 8.30s (`l9b-operators-final-check.log`).
Clippy delta `l9b-operators-final-clippy-delta.json`: 6959 errors / 546 warnings,
added 1 — cognitive complexity 32/30 в eval_current. Existing guard выделен в
private admit_binary_operands без изменения policy/order; helper нужен по
observed lint trigger, не как новый account. Post-extraction focused/check/Clippy
и source verification выполняются.
Graphify initial `l9b-operators-final-graphify.log` FAIL: Windows Errno 22 при
записи .graphify_labels.json.sig после AST extraction. Signature/package не
удаляются и не исправляются вне workspace; выполняется повторный normal update.
Post-extraction `l9b-operators-extracted-focused.log` PASS 26/26, 0.56s;
check PASS 211 warnings, 7.85s (`l9b-operators-extracted-check.log`). Clippy delta
`l9b-operators-extracted-clippy-delta.json`: 6959 errors / 545 warnings,
added instances **0**; прежняя cognitive complexity warning evaluator устранена.
Общий Clippy сохраняет FAIL из-за baseline debt. Repeated broad runs после
mechanical helper extraction не требуются: новая focused проверка закрывает
порядок guard/cancellation, предыдущие full lib/SDK/rustdoc результаты указаны выше.
Graphify retry `l9b-operators-extracted-graphify.log` PASS: **57672 nodes /
155081 edges / 1900 communities**. Baseline zero-node config.json/context7.json
и community label warnings сохранены. Package/signature не изменялись вручную.
Operators/Truthy focused/API/codecs gates закрыты; Stage 28 и L11 остаются OPEN,
performance attempts **0/5**.

## L9b call modifiers — admission prerequisite

Scout finding **L9b-CALL-DISTINCT-CLONE (Medium/resource)** принят: DISTINCT
sorter input и previous dedup value получают clone до own copy admission.
`l9b-call-distinct-clone-repro.log` FAIL 1/1, 0.01s: aggregate успешно возвращает
count при work cap 5, использовано только 4 units; два дополнительных deep
copies не учтены. Root fix переиспользует existing copy_evaluation_value/scalar
pool для обоих clone sites; helper становится execution-private, публичный API
не меняется. Новый account/executor не вводится. Focused prerequisite gate
`l9b-call-distinct-clone-focused.log` PASS **27/27**, 0.72s; fresh read-only source
review PASS, finding **CLOSED**. CallWithOptions vertical slice реализуется.
Initial `l9b-call-modifiers-focused.log` — compile FAIL E0282/E0283: closures
с transpose требуют явного FerrosGrynnResult из-за нескольких From impls;
добавлены return type annotations, повторный focused gate выполняется.
Performance attempts 0/5.

`l9b-call-modifiers-inference-focused.log`: 28 passed / 2 failed, 0.71s.
Named-call fixture передавала reactive `any` в параметр `int`; explicit CAST
сохраняет обычную signature semantics. DISTINCT history fixture изменяла class
version через отдельный DEFINE FIELD; начальная ClassSchema теперь содержит поле
при первом commit, сохраняя существующий complete-domain proof.
`l9b-call-modifiers-fixture-focused.log`: 29 passed / 2 failed, 0.54s. Первый
failure — повторный lexical `$check` в fixture; имена сделаны distinct. Второй —
реальный пропуск: historical validator отвергает GraphCatalogRange, который
DISTINCT sorter читает для metadata-only spill context из empty reconstructed
graph catalog. Accepted **L9b-CALL-DISTINCT-HISTORY-CONTEXT** остаётся OPEN до
root fix/test/re-review. Accepted review findings signature admission,
SDK named-tail и case-insensitive count/sum proof проверяются отдельным repro.
`l9b-call-findings-repro.log` FAIL **2 passed / 2 failed**, 0.04s: SDK успешно
принимал positional argument после named, а PURE параметр option nesting 65
при NONE успешно регистрировался. Root fixes выполнены до новых gates:
named-tail проверяется при SDK lowering; shared compiler signature admission
проверяет matching arity candidates до ranking/result clone и direct/transitive
compile до rendering/function clone. Existing shape walker и compiler RAII
budget, без второго account/resolver. Case-insensitive count/sum proof и empty
reconstructed spill metadata read исправлены; чужие graph/input reads сохраняют
coverage rejection. Findings OPEN до focused/re-review. L11 attempts 0/5.
`l9b-call-fixed-focused.log`: 32 passed / 1 failed, 0.55s. Последний failure
в fixture: замена count→COUNT и sum→SUM меняла также object keys. Теперь меняются
только function spellings; exact projected keys сохраняются.
`l9b-call-final-focused.log` PASS **33/33**, 0.82s, включая transitive parameter
depth guard, GQL/Rust named calls, frozen captures, cycles/savepoints, PURE version
pause/recapture, DISTINCT whole-commit history при current primary values 99,
upper-case aggregates, resource repro и old/new codec golden/malformed vectors.
Общий lib gate и backend/API/tooling verification выполняются. Stage OPEN;
performance attempts 0/5.

Pre-final `l9b-call-final-lib.log` FAIL: **996 passed / 1 failed / 10 ignored**,
75.81s, 0 filtered. Существующий resource test требует прежнюю диагностику
`PURE compiler work`; preliminary guard давал `PURE signature work`. Accepted
review finding **L9b-CALL-SIGNATURE-DOUBLE-SCALE** также требует root fix:
raw type-copy bytes ошибочно подавались в source admission с повторным ×1024.
8-parameter/16 MiB prediction reviewer не подтвердилась: `l9b-call-scale-repro.log`
PASS 1/1, 0.02s, оба Call variants. Reviewer отозвал этот конкретный trigger.
32-parameter вариант воспроизвёл rejection: `l9b-call-scale32-repro.log` FAIL
0 passed / 1 failed, 0.01s, PURE compiler scratch 8 377 606 bytes. Исправление
переиспользует тот же compiler RAII/account: source сохраняет ×1024+16 KiB,
signature использует raw shape bytes/work, с per-step compiler cancellation.
Прежние resource labels сохранены. Повторный focused gate включает existing
LIVE suite; общий lib gate будет повторён после него. Этот FAIL не baseline debt.

### Final call-modifier evidence

После raw signature admission `l9b-call-raw-focused.log` PASS **207/207**,
14.67s; оба варианта 32-parameter вызова помещаются в 16 MiB, глубина 65
отклоняется до signature ranking и direct/transitive compilation. Fresh read-only
source review PASS: DISTINCT-CLONE, PARAMETER-TYPE-ADMISSION, NAMED-ORDER,
HISTORY-NAMECASE, DISTINCT-HISTORY-CONTEXT и SIGNATURE-DOUBLE-SCALE CLOSED.
Отозванный 8-parameter trigger не используется как reproduction evidence.

| Gate                   | Final call-modifier evidence                                                                                                                                                   |
|------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Full lib               | `l9b-call-raw-final-lib.log`: **998 passed / 10 ignored / 0 failed**, 73.62s, 0 filtered out                                                                                   |
| SDK Sync / NativeAsync | Separate targets PASS 1/1 каждый, 0.17s / 0.15s; `l9b-call-raw-final-sdk-sync.log`, `l9b-call-raw-final-sdk-native.log`; typed call сохраняется при reopen→resume→ACK→mutation |
| Rustdoc                | `l9b-call-raw-final-rustdoc.log`: 6/6 PASS; отдельная нулевая группа evidence не считается                                                                                     |
| Check                  | `l9b-call-raw-final-check.log`: PASS, 211 warnings, 10.56s                                                                                                                     |
| Scoped nightly format  | 15 файлов PASS; `l9b-call-raw-final-fmt.log`                                                                                                                                   |
| Preserved changes      | 43 protected files unchanged; `l9b-call-raw-final-preserved.json`; HEAD `196d21b`                                                                                              |

Clippy delta `l9b-call-raw-final-clippy-delta.json`: **6959 errors / 545 warnings,
added instances 0**. Общий Clippy FAIL — baseline debt; ошибка pre-final lib
diagnostic label исправлена и не переименована в baseline failure. Call modifiers
имеют focused/API/codecs correctness evidence; Stage 28, general recipes,
historical coverage, L10/L11 и global tooling closure остаются OPEN.
Graphify `l9b-call-raw-final-graphify.log` PASS: **57687 nodes / 155144 edges /
1881 communities**; baseline zero-node/label warnings сохранены. Performance attempts **0/5**.

## L9b static GraphRecord

Добавлен owned `LiveSource::GraphRecord { graph, record }`; static GQL scoped
Record проходит через тот же recipe. Lowering сохраняет exact Record target,
effective scope и canonical Occurrence identity; source ClassId и nested record
names сохраняют original owner. GraphRecord использует GRAPH capture policy и
saved-result resume. Durable source tag 4 — graph dictionary slot + exact
Record value blob; codec/catalog checks расширены. Общие historical eligibility
gates остаются Class/Record, без объявления недоказанной graph coverage.

Read-only scout подтвердил consumers и существующие negative membership/FETCH
kernels. Предположение об origin у GraphRef отозвано после чтения actual type:
он содержит GraphId/name/kind/storage mode, новых origin полей не вводится.
Initial `l9b-graph-record-focused.log` — compile FAIL E0599 только в fixture:
runtime Array не имеет first(); oracle использует существующий get(0).
`l9b-graph-record-fixture-focused.log` PASS **210/210**, 16.45s. Initial
`l9b-graph-record-sdk-sync.log` — compile FAIL E0624: local fixture обращался к
private intern_property. Исправлено на schema commit и public property_dictionary
lookup, без расширения SDK. Review finding GRAPHRECORD-LOCAL-FIXTURE-NAME принят:
31-byte database basename limit требует existing 16hex suffix вместо полного UUID.
Этот name finding подтверждён source validation, не runtime failure (initial
run остановился раньше на compilation). Review/backend gates выполняются;
Stage OPEN, L11 attempts **0/5**.

Final GraphRecord broad gates: `l9b-graph-record-final-lib.log` PASS
**1001 passed / 10 ignored / 0 failed**, 79.65s, 0 filtered out. Public SDK Sync
`l9b-graph-record-public-sdk-sync.log` PASS 1/1, 0.32s; NativeAsync
`l9b-graph-record-final-sdk-native.log` PASS 1/1, 0.40s. Saved qualified images
и tail не зависят от further current overlay changes. Rustdoc
`l9b-graph-record-final-rustdoc.log` PASS **7/7**; отдельная нулевая группа не evidence.
Check PASS, 211 warnings, 6.75s (`l9b-graph-record-final-check.log`); scoped format
12 файлов PASS. 43 protected files unchanged (`l9b-graph-record-final-preserved.json`).
Fresh source review PASS, production/codec findings нет; LOCAL-FIXTURE-NAME CLOSED.

Clippy initial delta `l9b-graph-record-final-clippy-delta.json`: 6959 errors /
546 warnings, added 1 — large_enum_variant у private LiveDescriptor (Query 336
bytes / Binding 104). Это single short-lived codec input/output; он не хранится
в retained очередях/массивах, registration переносит query в existing Arc.
Выбран narrow reasoned allow на enum: новый Box добавил бы отдельную allocation
только ради size heuristic. Owner helper rust-style-clippy активирован после
observed diagnostic; public API/wire bytes и resource policy не меняются.
Post-annotation Clippy `l9b-graph-record-annotated-clippy-delta.json`: **6959 errors /
545 warnings, added instances 0**. Initial annotation format check выявил только
перенос длинного reason; scoped nightly formatter и повторный check PASS
(`l9b-graph-record-annotated-wrap-check.log`). Annotation/format не меняют behavior;
перечисленные broad runtime/SDK результаты сохраняются. Global Clippy FAIL остаётся
baseline debt. Final annotation re-review PASS: codec descriptor не хранится в bulk
collections, отдельная Box allocation не требуется. Graphify AST update PASS
(`l9b-graph-record-final-graphify.log`): **57694 nodes / 155165 edges / 1907 communities**;
baseline zero-node/label warnings сохранены. Performance attempts **0/5**.

## L9b duration I — SLEEP и nested TIMEOUT

Literal parser сохранён, lexical параметры удерживаются как Slot/LiveSlot до
invocation. Общий borrowed converter проверяет kind, calendar units, sign и exact
nanoseconds без копирования invalid payloads. Borrowed AtomFieldRead protocol
переиспользует fixed root/SSI lookup и прежний owned admission; нового resolver
или account нет. Normal/PURE blocks используют checked child, minimum parent
deadline и общий resource budget. Timeout effect учитывается в closure analysis;
single-return collapse не удаляет timeout metadata. Top-level session/direct
TIMEOUT и import frozen metadata — следующий отдельный срез, ещё OPEN.

Regression: `l9b-duration-valid-before.log` FAIL при analysis — SLEEP local `$delay`
не найден в raw Object. Initial `l9b-duration-before.log` FAIL из-за fixture
`LET ... = 0ms` (не expression grammar). Initial after fixture FAIL на неподдержанном
`type::to<duration>(string)`; следующий fixture FAIL на `FROM [1]` (не source grammar).
Исправлено только в тестах: Rust Duration inputs и реальный seeded CLASS.
`l9b-duration-lifecycle-focused.log` compile FAIL E0599: TextSpan не имеет Default;
исправлено на existing `TextSpan::new`. Lifecycle fixture затем FAIL: LIVE LET
statement возвращает NONE, identity извлекается через INFO, а не statement outcome.

Review finding **L9b-DURATION-ANY** принят: `compatible` отклонял Any, включая
same-statement LIVE LET и untyped closure inputs. Используется существующий
`assignment_compatible` и обязательная borrowed runtime validation. Fresh re-review
PASS, finding CLOSED. `l9b-duration-info-focused.log` PASS **5/5**, 0.06s:
lexical ALTER/shadow/Any/closure, LIVE replacement/direct Tx, calendar/negative/overflow,
nested import и fatal timeout при SKIP; Snapshot/ReadCommitted/SSI; PURE mirror и
large invalid container без scratch copy. Existing sleep/deterministic/cancellation
проверяются последующим broad gate. Clippy initial delta added 2 private-variant
doc errors; оба variant получили documentation. Final full lib
`l9b-duration-final-lib.log` PASS **1006 passed / 10 ignored / 0 failed / 0 filtered**,
81.92s. `cargo check --lib --no-default-features` PASS, 211 warnings, 24.41s
(`l9b-duration-final-check.log`). Post-doc Clippy delta PASS: **6959 errors /
545 warnings, added diagnostics 0** (`l9b-duration-final-clippy-delta.json`);
global Clippy exit 101 остаётся baseline debt. Scoped nightly format девяти Rust
файлов PASS; 43 protected files unchanged (`l9b-duration-final-fmt.log`,
`l9b-duration-final-preserved.json`). Separate local backend checks и Graphify
ещё выполняются; закрытие duration I требует их evidence.
Sync `l9b-duration-final-sdk-sync.log` PASS 1/1, 0.31s. Initial NativeAsync
`l9b-duration-final-sdk-native.log` выбрал **0 tests** без required feature и
evidence не считается; повтор с `--features native-async-io` идёт отдельным target.
Feature-enabled NativeAsync `l9b-duration-final-sdk-native-enabled.log` PASS
**1/1**, 0.39s. Runtime backend gates duration I PASS; Graphify ещё выполняется.
Graphify `l9b-duration-final-graphify.log` PASS: **57713 nodes / 155256 edges /
1879 communities**; baseline zero-node/label warnings сохранены. Duration I
focused/broad/backend/source-review gates PASS; global tooling и Stage 28 OPEN.

## L9b duration II — ordinary top-level TIMEOUT

Pre-fix `l9b-duration-top-before.log` FAIL **1/1**, 0.03s: LIVE LET содержит
identity UUID, eager raw Object resolution ошибочно требует duration у identity.
Runtime analyzer теперь возвращает private `(program, timeout)` и lower-ит
operand до declaration/shadowing. Executor разрешает pre-statement slots на
том же QuerySnapshot до mutating writer inference. Caller-owned control
ограничивается construction-anchored checked deadline; autocommit сохраняет
его до export/ACK/admission/WAL publication. Shared accounts/cancellation
не заменяются. Clones/children/inherited checks сохраняют исходный Instant;
nested timeout продолжает использовать собственную invocation-relative bound.

Acceptance: top-level lexical/reactive/zero/direct Tx и predeclaration shadow,
Snapshot/ReadCommitted/SSI, fixed/calendar/negative/overflow; construction
anchor/default minimum и bounded BLOCK wait без partial publication. Focused
Cargo и fresh read-only source review выполняются. Outer import metadata,
EXPLAIN inner timeout и embedded MATCH timeout остаются OPEN отдельными seams.
L11 attempts **0/5**; эти correctness runs попытками performance не являются.
Initial focused `l9b-duration-top-focused.log` PASS **11/11**, 0.13s. Review
**D2-TEST-01** принят: исходный combined SLEEP/TIMEOUT SSI case конфликтовал
из-за обеих bindings, поэтому не доказывал именно timeout dependency. Тест
разделён на независимые fresh-store SLEEP и top TIMEOUT cases: только одна
binding прочитана и изменена на каждый isolation case. Fresh re-review PASS,
finding CLOSED; production findings отсутствуют. Isolated focused gate выполняется.
`l9b-duration-top-isolated-focused.log` PASS **11/11**, 0.11s. Первый broad
`l9b-duration-top-final-lib.log` FAIL **1008 passed / 1 failed / 10 ignored**,
84.96s: `cancellation_closes_the_supplied_cursor_spool` повторно использовал
cursor после cancellation. Narrow `l9b-duration-top-cursor-repro.log` FAIL
**1/1**, 0.02s. Это новая регрессия, не baseline failure: top-level preparation
check обходил SELECT direct-input spool cleanup. Findings **D2-CURSOR-01**
OPEN до focused/re-review. Existing cleanup protocol выделен в общий helper
с двумя реальными consumers; direct Slot/Constant cursor закрывается при
preparation/evaluation error без повторного выполнения computed expression.
Cancellation regression расширена на no clause, literal и LIVE LET TIMEOUT.
Post-fix focused gates выполняются; check/Clippy не запускались после первого
broad failure. Local backend fixture дополнен duration capture/zero rejection/reopen.
Cleanup `l9b-duration-top-cleanup-focused.log` PASS **1/1**, 0.06s (все три
clause cases); `l9b-duration-top-cleanup-duration.log` PASS **11/11**, 0.14s.
Fresh source re-review PASS, **D2-CURSOR-01 CLOSED**. Suspected отсутствующий
`From<jiff::Error>` для test fixture отвергнут: existing status crate реализует
его в `crates/fg-gql-status/src/error.rs:174`, focused target скомпилирован.
Повторный broad/check/Clippy gate выполняется; первая неудача не скрывается.
Scoped nightly format десяти Rust файлов PASS; 43 protected files unchanged
(`l9b-duration-top-fixed-fmt.log`, `l9b-duration-top-fixed-preserved.json`).
Post-fix full lib `l9b-duration-top-fixed-lib.log` PASS **1009 passed /
10 ignored / 0 failed / 0 filtered**, 96.98s. Check PASS, 211 warnings, 12.21s
(`l9b-duration-top-fixed-check.log`). Clippy delta PASS: **6959 errors /
545 warnings, added diagnostics 0** (`l9b-duration-top-fixed-clippy-delta.json`);
global exit 101 — baseline debt. Separate enriched Sync/NativeAsync gates
и Graphify выполняются. Никаких performance attempts не выполнено.
Enriched local Sync `l9b-duration-top-final-sdk-sync.log` PASS **1/1**, 0.37s:
LIVE LET duration прочитан TIMEOUT, zero clause не публикует CREATE, value
сохраняется при checkpoint/reopen. NativeAsync target ещё собирается.
Initial Graphify `l9b-duration-top-final-graphify.log` FAIL: Windows Errno 22
при `.graphify_labels.json.sig`; исходный package/signature не изменены.
Обычный retry идёт отдельным `l9b-duration-top-retry-graphify.log`.
Enriched NativeAsync `l9b-duration-top-final-sdk-native.log` PASS **1/1**, 0.34s.
Duration II correctness/backend/source-review/check/format gates PASS; Clippy
delta added 0. Graphify retry ещё идёт; общий Stage 28 closure OPEN.
Graphify retry `l9b-duration-top-retry-graphify.log` PASS: **57719 nodes /
155280 edges / 1858 communities**; baseline zero-node/label warnings сохранены.
Duration II slice gates закрыты по evidence; Stage 28 и общие L8–L11 gates OPEN.
L11 performance attempts **0/5**, Stage OPEN.

## L9b duration III — import preflight

Regression `duration_import_preflight_uses_live_values` до исправления FAIL
**0 passed / 1 failed** (`l9b-duration-import-before.log`): eager raw resolver
принимал frame identity LIVE LET за ordinary UUID. Вместо него import получает
session BindingTypes и использует один Analyzer/TransactionRead для program и
typed item timeout operands. Conversion заимствует initial Object slots либо
snapshot binding через общий duration kernel; весь owned frame ради scalar
input не создаётся. Sparse typed и frozen vectors допускаются общим RAII account
до allocation. Platform Instant overflow проверяется до writer lease;
zero timeout остаётся item-local execution error с полным rollback.

Existing expected commit_ts/lease сохраняет preflight values до END, nested FOR
locals продолжают invocation conversion. Item checked_child больше не превращает
переполнение Instant в отсутствие deadline. END использует исходный caller
deadline. Неподдерживаемый outer IMPORT TIMEOUT не добавлен; redundant resolver
удалён. LIVE lowering не изменён. Focused/source-review/full/backend/tooling
gates выполняются; Stage OPEN, L11 attempts **0/5**.

Initial focused `l9b-duration-import-focused.log` PASS **3/3**, 0.04s.
Расширенный `l9b-duration-import-final-focused.log` FAIL на compile: UUID fixture
был вставлен в соседний loop test без session (2 × E0425). Source review
подтвердил production contract и указал placement error; fixture перенесён
в reactive-registration test. BLOCK cases разделены: normal clause 40ms без
parent default; import clause 1s при parent default 40ms. Это исключает
ложное доказательство clause deadline через совпадающий parent limit.
Corrected focused `l9b-duration-import-corrected-focused.log` PASS **15/15**,
0.22s: обычный UUID с тем же bytes не становится reactive, private source
изменения не меняют frozen deadline, stale preflight не входит в callback и
не удерживает lease после rejection. Fresh source re-review PASS, actionable
findings отсутствуют. Общие lib/check/backend/Clippy/Graphify gates ещё идут.
Full lib `l9b-duration-import-final-lib.log` PASS **1013 passed / 10 ignored /
0 failed / 0 filtered**, 82.57s. Check `l9b-duration-import-final-check.log` PASS,
211 warnings, 13.28s. 43 protected unrelated files unchanged
(`l9b-duration-import-preserved.json`). Separate backend gates выполняются.
Enriched local Sync `l9b-duration-import-sdk-sync.log` PASS **1/1**, 0.34s;
enabled NativeAsync `l9b-duration-import-sdk-native.log` PASS **1/1**, 0.38s.
Positive/zero import TIMEOUT проверены вместе с GQL LIVE LET и checkpoint/reopen.
Scoped nightly rustfmt одиннадцати Rust files PASS
(`l9b-duration-import-final-fmt.log`). Clippy delta и Graphify выполняются.
Initial Clippy global exit 101; multiset delta **2 added errors**:
`clippy::redundant_pub_crate` у нового preflight helper и его export
(`l9b-duration-import-final-clippy-delta.json`). Narrow visibility исправлена
на GQL boundary без изменения runtime behavior. Owner rust-style-clippy,
mechanical visibility fix; verification — library check и повторный Clippy
delta, повтор behavior gates без новой semantics не требуется.
Visibility check `l9b-duration-import-visibility-check.log` PASS, 211 warnings,
6.34s. Повторный Clippy выполняется.
Final visibility Clippy delta PASS **6957 errors / 545 warnings / added 0**
(`l9b-duration-import-visibility-clippy-delta.json`); global exit 101 — baseline
debt. Final scoped rustfmt PASS (`l9b-duration-import-visibility-fmt.log`).
Duration III focused/broad/backend/source-review gates закрыты; Graphify update
остаётся последним tooling шагом этого среза. Stage OPEN, attempts **0/5**.
Initial Graphify `l9b-duration-import-final-graphify.log` FAIL: AST extraction
18/18 завершён, rebuild получил Windows Errno 22 на
`graphify-out/.graphify_labels.json.sig`. Signature/package не изменены;
обычный retry совмещается с ближайшим verified source update. Tooling gate
до успешного retry OPEN; correctness evidence не отменяется.

## L9b duration IV — EXPLAIN wrapper

Regression `l9b-duration-explain-before.log` FAIL **0 passed / 1 failed**,
0.03s: `EXPLAIN SELECT … TIMEOUT $limit` вернул успешный logical plan при
LIVE LET duration `0ms`. Parser сохранял inner timeout, analyzer переносил
лишь statement kind. Shared normalization теперь переносит единственный clause
во внешний Statement, как для direct MATCH/BUILD GRAPH; новые IR fields или
executor не добавлены. Typed ordinary/block paths удерживают snapshot/SSI,
construction clock и минимальный caller deadline. Plain/FULL не исполняют
inner rows. Early preparation error распознаёт wrapped SELECT и вызывает
тот же borrowed continuation retirement helper, без второго eval.
Focused и source-review gates выполняются. Stage OPEN, L11 attempts **0/5**.
Initial focused `l9b-duration-explain-focused.log` PASS **17/17**, 0.23s;
top-level cursor matrix `l9b-duration-explain-cursor.log` PASS **1/1**, 0.08s
(12 cases). Initial full lib `l9b-duration-explain-final-lib.log` PASS
**1015 passed / 10 ignored / 0 failed / 0 filtered**, 79.49s; check PASS,
211 warnings, 8.10s (`l9b-duration-explain-final-check.log`).
Source review выявил **D4-CURSOR-NESTED (Medium)**: nested duration conversion
либо expired wrapper check возвращает ошибку до inner SELECT retirement.
Широкий PASS не закрывает отсутствующий regression. Независимый narrow test
готовит настоящий continuation и проверяет FG-22F20 после block failure;
root cause — разные preparation error paths, общий dispatcher планируется
для top/block/wrapper control guards. Finding OPEN до reproduction/fix/re-review.
Narrow reproduction `l9b-duration-explain-nested-before.log` FAIL **0/1**,
0.02s: реальный supplied continuation оставался доступным после nested failure.
Общий dispatcher теперь вызывается из top preparation, block conversion/child
preparation и existing statement-entry control/depth guards. Direct SELECT
сохраняет собственный cancellation kernel; дополнительные interruption checks
не добавлены. Computed cursor expressions не перевычисляются. Regression matrix
покрывает direct/wrapped zero, negative reactive conversion, Instant overflow;
отдельный deterministic invocation test отменяет entry после lowering,
использует настоящий CursorRuntime и проверяет retirement без thread timing.
Post-fix focused tests выполняются; D4-CURSOR-NESTED пока OPEN.
Post-fix `l9b-duration-explain-cleanup-focused.log` PASS **19/19**, 0.22s;
`l9b-duration-explain-cleanup-cursor.log` PASS **1/1**, 0.04s (12 top-level
cases). Nested matrix проверяет 12 cases; deterministic cancellation успешно
retire-ит cursor на invocation boundary. Fresh source re-review PASS,
**D4-CURSOR-NESTED CLOSED**. Full/check/backend gates после root-cause fix
повторяются; прежний broad PASS не используется вместо новых проверок.
Post-fix full lib `l9b-duration-explain-fixed-lib.log` PASS **1017 passed /
10 ignored / 0 failed / 0 filtered**, 77.73s. Check PASS, 211 warnings, 7.53s
(`l9b-duration-explain-fixed-check.log`). Enriched Sync
`l9b-duration-explain-fixed-sdk-sync.log` PASS **1/1**, 0.33s; enabled NativeAsync
`l9b-duration-explain-fixed-sdk-native.log` PASS **1/1**, 0.36s. Positive/zero
EXPLAIN и ordinary/import duration проверены вместе с checkpoint/reopen.
Scoped nightly rustfmt PASS (`l9b-duration-explain-fixed-fmt.log`).
Final Clippy global FAIL с прежними **6957 errors / 545 JSON warnings**;
`l9b-duration-explain-fixed-clippy-delta.json` подтверждает **added instances 0**.
43 protected files unchanged (`l9b-duration-explain-fixed-preserved.json`).
Graphify штатный retry `l9b-duration-explain-retry-graphify.log` PASS:
**57734 nodes / 155345 edges / 1928 communities**. Initial Errno 22 сохранён
выше; installed package/signature не менялись, платный labeling не запускался.
Tooling шаг duration III/IV закрыт. Stage OPEN, attempts **0/5**.

## L9b duration V — embedded MATCH

Initial test fixture `l9b-duration-match-before.log` не собрался из-за неверного
вызова transaction graph lookup (E0599); исправлен на existing named_graph,
TRAVERSE GraphRef literal приведён к `@name`. Corrected regression
`l9b-duration-match-before-corrected.log` FAIL **0/1**, 0.02s: TRAVERSE MATCH
успешно выполнил MATCH при нулевом LIVE LET TIMEOUT. Production analyzer терял
inner timeout, хотя direct MATCH parser уже нормализовал его на outer statement.

LogicalMatch теперь удерживает private typed duration; общий visitor создаёт
checked child против переданного snapshot с borrowed lookup до planner/open.
Все три consumers используют этот control до последнего visitor/graph assembly;
по каждому tv::match seed создаётся отдельный deadline. Без clause прежний
control path сохранён. Tests покрывают positive/zero ordinary/reactive/literal,
UUID provenance, invalid/calendar/platform overflow, minimum parent deadline,
ordinary lexical shadow и planning-only EXPLAIN. Snapshot/SSI matrix расширена
TRAVERSE source и tv::match на непустом graph. Sync/NativeAsync shared fixture
проверяет три формы, named build и no-install на zero. Focused/source gates идут;
Stage OPEN, L11 attempts **0/5**.

Scoped nightly rustfmt `l9b-duration-match-fmt.log` FAIL только при записи
`tests/live_local.rs`: Windows OS1224 (open mapped section). Предшествующие пять
Rust files отформатированы; повтор после текущей Cargo сборки pending.
Повтор formatter записи также встретил OS1224 в control.rs/live_local.rs.
Read-only rustfmt check вывел точные deltas, применённые обычным file tool;
`l9b-duration-match-final-fmt.log` PASS для всех шести Rust files, без изменения
installed tooling или несвязанных files.

Initial focused `l9b-duration-match-focused.log` FAIL **18/20**: два fixtures
использовали неподдерживаемый bare/@graph source expression в TRAVERSE FROM.
Existing owned GraphRef LET `$scope = $graph` устранил parse errors без расширения
production parser. Corrected focused PASS **20/20**. По source-review coverage
добавлены MATCH RETURN GRAPH в четвёртой query и deterministic cancellation
из второго/final visitor. Terminal focused `l9b-duration-match-terminal-focused.log`
PASS **21/21**, 0.36s. Fresh source и test follow-up reviews PASS. Callback test
доказывает final-row cancellation semantics; next SeedCursor также проверяет
control, поэтому этот test не изолирует post-loop guard. Graph assembly terminal
check проверен source trace и real positive/zero GQL cases. Full/check/backend
gates идут; L11 attempts **0/5**.
В первом broad pipeline backend filters ошибочно использовали имя shared helper
вместо #[test] wrappers. Такие запуски выбирают ноль tests и evidence не считаются;
после pipeline проверяются точные `live_gql_binding_local_sync` и enabled
`live_gql_binding_local_native_async`, отдельными логами. Full/check unaffected.
Final full lib `l9b-duration-match-final-lib.log` PASS **1019 passed / 10 ignored /
0 failed / 0 filtered**, 90.62s. Check PASS, 211 warnings, 11.13s
(`l9b-duration-match-final-check.log`). Corrected wrappers:
`l9b-duration-match-sdk-sync-corrected.log` PASS **1/1**, 0.89s;
`l9b-duration-match-sdk-native-corrected.log` PASS **1/1**, 0.72s, feature enabled.
43 protected files unchanged (`l9b-duration-match-final-preserved.json`).
Clippy initial global FAIL с прежними 6957 errors / 545 JSON warnings, но
delta **added 1**: новый cognitive_complexity 34/30 в общем MATCH visitor
(`l9b-duration-match-final-clippy-delta.json`). Deadline preparation выделен
рядом с существующими graph/maxdepth resolvers; terminal checks используют
stdlib Option.map_or, без дополнительных checks для отсутствующей clause.
Observed helper refactoring удерживает snapshot/SSI/clock/errors и четыре
caller paths. Focused/re-review и final Clippy ещё выполняются; initial broad
PASS не заменяет post-extraction evidence. Graphify pending, attempts **0/5**.
Post-extraction focused `l9b-duration-match-extracted-focused.log` PASS **21/21**,
0.41s; fresh source re-review PASS. Scoped six-file read-only nightly format
PASS (`l9b-duration-match-extracted-final-fmt.log`). Final Clippy global FAIL
**6957 errors / 545 JSON warnings**, delta **added 0**
(`l9b-duration-match-extracted-clippy-delta.json`); **D5-CLIPPY CLOSED**, без allow.
Full/check/backend после extraction выполняются с exact wrappers.
Post-extraction full lib `l9b-duration-match-extracted-lib.log` PASS **1019 passed /
10 ignored / 0 failed / 0 filtered**, 81.71s. Check
`l9b-duration-match-extracted-check.log` PASS, 211 warnings, 8.40s.
43 protected files unchanged (`l9b-duration-match-extracted-preserved.json`).
Separate backend runs и Graphify ещё идут; source/Clippy finding закрыт.
Post-extraction Sync `l9b-duration-match-extracted-sdk-sync.log` PASS **1/1**,
0.81s; enabled NativeAsync `l9b-duration-match-extracted-sdk-native.log` PASS
**1/1**, 0.72s. Все source/focused/full/check/backend/format/Clippy-delta gates
duration V закрыты, остаётся Graphify update. Stage OPEN, L11 attempts **0/5**.
Initial Graphify `l9b-duration-match-extracted-graphify.log` FAIL: AST 13/13,
communities recomputed, затем Windows Errno 22 на `.graphify_labels.json.sig`.
Installed package/signature не менялись; штатный retry в отдельном log pending.
Штатный retry `l9b-duration-match-retry-graphify.log` также FAIL: AST 6/6,
communities 1895, затем тот же Errno 22 на signature. Tooling gate duration V
остаётся OPEN; новая попытка update последует после следующего проверенного среза.

## L9c — ordinary reactive record navigation

Read-only scout обнаружил отдельную гипотезу: Field base разрешает LiveSlot через
statement RecordFields, но scalar field evaluation после materialization получает
read.bounded_fields(), равное None для обычного TransactionRead. Existing bounded
LIVE recipe route уже проверен; ordinary captured closure/scalar traversal route
требует самостоятельной regression. Initial `l9c-reactive-record-before.log`
FAIL **0/1** относится к fixture: `||` вместо принятого empty closure `| |`.
Исправлена fixture, не parser. Corrected runtime proof pending; production
ordinary expression evaluator в этом срезе пока не изменён. L11 attempts **0/5**.
Corrected `l9c-reactive-record-before-corrected.log` FAIL **0/1**, 0.03s,
уже execution InvalidArgument: `Cannot access field age on record`.
Field materialization теперь передаёт existing RecordFields с transaction-aware
snapshot, сохраняя bounded RootRead delegation и обычный record/qualified resolver.
Исправление находится в общем evaluator, не в traversal-specific adapter;
initial focused и expanded snapshot/resource/source-review gates pending.
Initial focused `l9c-reactive-record-initial-focused.log` PASS **1/1**, 0.02s.
Expanded regression добавляет независимый SELECT oracle, property mutation,
binding replacement, missing field, ordinary shadow и UUID provenance error.
Отдельная матрица двух consumers × трёх isolation levels проверяет base-property
SSI независимо от reactive binding revision. Shared Sync/NativeAsync fixture
проверяет старый snapshot и durable record binding/property после reopen.
Expanded gates выполняются; L11 attempts **0/5**.
Expanded focused PASS **2/2**, 0.08s. Full lib PASS **1021 passed / 10 ignored /
0 failed / 0 filtered**, 83.30s; check PASS, 211 warnings, 8.39s. Initial integration
fixture compile FAIL E0624: root использовал private intern_property; исправлен
на public property_dictionary lookup. Corrected Sync PASS **1/1**, 1.18s;
enabled NativeAsync PASS **1/1**, 1.29s. Source review подтверждает resolver,
snapshot/SSI и qualified delegation, но нашёл suspected property-copy admission.
`l9c-reactive-record-admission-before.log` подтвердил R1: FAIL **0/1**, 0.03s;
standalone closure успешно вернула 20K Array при лимите 4 KiB. Existing owned
point lookup копировал все properties и выбранное поле без reactive admission.
Это общая pre-existing resolver policy, ставшая достижимой из исправленной closure.
R1 resource gate OPEN; repair выполняется в общей record-field boundary до copy,
не post-filter возвращённого owned result. Performance attempts **0/5**.
Initial repair focused `l9c-reactive-record-admission-focused.log` PASS **3/3**,
0.07s. Signature propagation передаёт control через existing private field traits;
adapter использует reactive pool только после reactive copy. Datastore callbacks
допускают payload до clone и не зависят от query control type. Затем общий
borrowed field projection выделен для двух реальных consumers (transaction и
RootRead), сохранив ensure_live, отрицательные reads и effective/base fallback.
RootRead scalar read больше не материализует все unrelated properties;
новая прямая low-budget проверка `.age`/`.payload` и fresh source review идут.
Shared/final focused PASS **3/3**, 0.08s. Final regression дополнительно проверяет
Hidden age, small overlay поверх large base, nonmember base fallback, canonical id
и rejection большого scoped label при 4 KiB. Fresh source review PASS; large id
allocation проверено source admission, отдельного oversized-id fixture нет.
Clippy global FAIL **6957 errors / 545 JSON warnings**, delta **added 0**
(`l9c-reactive-record-admission-clippy-delta.json`). Twelve-file scoped nightly
format PASS; 43 protected files unchanged. Final full lib
`l9c-reactive-record-final-lib.log` PASS **1022 passed / 10 ignored / 0 failed /
0 filtered**, 97.98s. Check PASS, 211 warnings, 11.82s. Enriched low-budget
backend compile встретил fixture E0639: публичные QueryExecutionOptions non-exhaustive.
Исправлено на Default + field assignment, без изменения public API. Corrected
Sync/NativeAsync и final Clippy выполняются; Stage OPEN, attempts **0/5**.
Следующий corrected backend runtime выявил fixture type mismatch:
execute Array parameter типизируется как array<any>, а schema требует array<int>.
Добавлен явный ordinary `<array<int>>` cast до CREATE; schema/analyzer не ослаблялись.
Low-budget fixture теперь также проверяет small closure result при том же 4 KiB,
чтобы исключить ложный PASS от ошибки preflight. Backend gates повторяются.
Graphify `l9c-reactive-record-graphify.log` PASS: AST 19/19, **57744 nodes /
155403 edges / 1904 communities**; прежние duration V signature failures закрыты
успешным штатным update без изменения installed package/signature. После последних
fixture edits graph обновляется ещё раз. Performance attempts **0/5**.
Explicit generic array cast также вернул existing ConversionError до CREATE
(`l9c-reactive-record-sdk-typed-sync.log`, FAIL 0/1, 0.93s). Для test setup
large payload теперь устанавливается public Rust set_base_property, как в RAM
oracle; generic CAST semantics не расширялись. `l9c-reactive-record-sdk-populated-sync.log`
PASS **1/1**, 1.36s: small closure успешно возвращает 20 при 4 KiB, large closure
отклоняется, checkpoint/reopen сохраняет record binding и property. NativeAsync
и final lint gate выполняются.
Final populated NativeAsync `l9c-reactive-record-sdk-populated-native.log` PASS
**1/1**, 2.10s, feature enabled. Final Clippy global FAIL остаётся прежним:
**6957 errors / 545 JSON warnings**, delta **added 0**
(`l9c-reactive-record-final-clippy-delta.json`). R1 correctness/resource/source
и separate backend gates CLOSED; исходные fixture failures не выдаются за evidence.
Stage 28 OPEN, L11 attempts **0/5**. Final Graphify после fixture edits выполняется.
Final fixture refresh `l9c-reactive-record-final-graphify.log` FAIL: AST 7/7,
communities 1916, Windows Errno 22 на signature. Предшествующий successful update
сохраняется как evidence production extraction, но current tooling gate OPEN.
Следующий штатный update выполняется после следующего проверенного среза.

## L9d — точные traversal counts

`l9d-traversal-count-before.log` FAIL **0/1**, 0.01s: `tv::limit($count)` с
LIVE LET `-1` возвращал успешный пустой результат. Общая `prepared_state`
заменяла неверное/слишком большое значение нулём через Option и i64 bottleneck.
Исправление использует существующий exact `Number → u64`; VM constructor
возвращает Result и проверяет все states до первого VM row pull. Source уже
может быть вычислен — preflight перед его materialization не утверждается.
Public API/status/numeric kernel не расширялись, `limit(0)` остаётся early exit.

Initial focused `l9d-traversal-count-initial-focused.log` PASS **1/1**, 0.02s.
Expanded compile log FAIL E0308 в fixture: `as_uuid()` возвращает borrowed UUID;
исправлено dereference без clone/API change. Corrected expanded focused
`l9d-traversal-count-expanded-focused-corrected.log` PASS **3/3**, 0.07s:
ошибочные типы, отрицательные/fractional/overflow counts, NONE/NULL/пустые
sources, execute-parameter NaN/infinities; ordinary/reactive counts против
независимого prefix/suffix oracle с bulk multiplicity, Float/Decimal integral
values, числами выше 2^53/i64::MAX и u64::MAX; snapshot/ReadCommitted/SSI.
Числовые ошибки не превращаются в успешный пустой результат. Backend fixture
добавляет negative replacement, exact u64::MAX и checkpoint/reopen.
Broad/backend/source/tooling gates выполняются. Stage OPEN, L11 attempts **0/5**.

Final L9d gates: `l9d-traversal-count-lib.log` PASS **1025 passed / 10 ignored /
0 failed / 0 filtered**, 96.55s; `l9d-traversal-count-check.log` PASS, 211 warnings,
9.15s. Source reviewer PASS: один caller, exact unsigned conversion, прежний
limit(0) guard и bulk splitting; pre-source preflight не обещается. Scoped
nightly `--check` PASS для трёх Rust files. `l9d-traversal-count-sdk-sync.log`
PASS **1/1**, 1.73s; отдельный feature-enabled
`l9d-traversal-count-sdk-native.log` PASS **1/1**, 1.97s. Оба targets проверяют
replacement/reopen и точный u64::MAX. Clippy global FAIL остаётся baseline debt:
**6957 errors / 545 JSON warnings**, delta **added 0**
(`l9d-traversal-count-clippy-delta.json`). Graphify update выполняется; stage OPEN.
`l9d-traversal-count-preserved.json`: 43 protected files unchanged.
`l9d-traversal-count-graphify.log` PASS: AST 10/10, **57748 nodes / 155416 edges /
1913 communities**; закрывает прежний intermittent signature tooling failure.
L9d slice CLOSED по evidence, Stage 28 OPEN, performance attempts **0/5**.

## L9e — frozen graph scope

Scoped Graphify query и source scout подтвердили отказ Recipe::graph от любого
non-Name input. `l9e-frozen-scope-before.log` FAIL **0/1**, 0.02s:
обычный `LET $scope = GraphRef` получил `QueryFeatureNotSupported` при LIVE SELECT.
Root direct ordinary parameter теперь хранится только в private registration
template; его exact slot разрешается при execution against supplied snapshot.
GraphRef/String преобразуется в существующий static GraphClass/GraphRecord до
SDK validation и publication. Typed IR не сохраняется; durable source tags 3/4,
capture policy, qualified identity и публичный API остаются прежними. ALTER LET
не меняет уже зарегистрированный source; fresh registration использует новое значение.
Tracked LIVE LET, computed и nested scope templates остаются отдельным открытым срезом;
их UUID/current value не замораживаются как ordinary capture.

Initial focused `l9e-frozen-scope-initial-focused.log` PASS **1/1**, 0.02s;
expanded focused PASS **1/1**, 0.04s. Reviewer отметил suspected named-catalog
work/dependency gap при bare RootCatalog. Shared existing command RootRead теперь
живёт в течение expression evaluation и graph-name resolution; QueryCatalog
наблюдает GraphCatalogRange/GraphDefinition и выполняет bounded admission.
Это исправление по inspection, before/after SSI reproduction не заявляется.
Interim repair compile FAIL E0599: graph_if_exists принадлежит QueryCatalog,
не QueryRead. Corrected existing-trait path PASS 1/1; final
`l9e-frozen-scope-final-focused.log` PASS **2/2**, 0.06s, включая catalog SSI.
Source re-review PASS: callback lifetime/work/dependencies, owned source reification,
GraphRef/String, block capture, CANCEL/no registration, stale refs, wrong types,
tracked provenance и сохранённый обычный SELECT resolver.

Local fixture сохраняет прежний SDK GraphRecord handle и добавляет GQL scope:
GraphRef захвачен LET, затем ordinary LET заменён другим GraphRef; static descriptor и
captured GraphRef переживают checkpoint/reopen. Resume читает старый saved result
после более нового overlay, затем ACK/tail/close; значения выше 2^53 остаются exact.
Broad/backend/tooling gates выполняются, Stage OPEN; L11 attempts **0/5**.
Initial `l9e-frozen-scope-sdk-sync.log` FAIL **0/1**, 0.20s: fixture пытался ALTER LET
GraphRef → NONE, что нарушает существующий inferred graph_ref<graph> type contract.
Fixture исправлен на другой GraphRef того же family; production typing не менялся.

Final gates: `l9e-frozen-scope-lib.log` PASS **1027 passed / 10 ignored / 0 failed /
0 filtered**, 69.72s; `l9e-frozen-scope-check.log` PASS, 211 warnings, 8.58s.
Scoped nightly `--check` **7 files PASS** (`l9e-frozen-scope-fmt-check.log`).
Source re-review PASS после использования bounded RootRead QueryCatalog;
положительная SSI проверка не выдаётся за reproduction прежней bare-catalog версии.
`l9e-frozen-scope-sdk-corrected-sync.log` PASS **1/1**, 0.48s;
feature-enabled separate `l9e-frozen-scope-sdk-corrected-native.log` PASS **1/1**,
0.54s. Frozen descriptor/captured GraphRef, old result replay после нового overlay,
ACK/tail/close проверены alongside public SDK. Clippy global FAIL прежний:
**6957 errors / 545 JSON warnings**, delta **added 0**
(`l9e-frozen-scope-clippy-delta.json`). Stage OPEN, L11 attempts **0/5**.

Final protected slice: **43 unchanged** (`l9e-frozen-scope-preserved.json`).
Graphify `l9e-frozen-scope-graphify.log` FAIL: AST 14/14 завершён, rebuild получил
Windows `Errno 22` на `.graphify_labels.json.sig`. Installed Graphify и signature
не изменялись. Correctness/resource/backend gates L9e закрыты; tooling OPEN.

## L9f — отдельная durable capture identity

Перед tracked scope требуется refactor static capture-policy authority:
LiveDefinition и durable catalog удерживают checked ChangefeedSource, publication
и validation не выполняют logical source expressions. Это сохраняет текущие
CLASS/record/graph feeds и допускает сохранение последней успешной identity при
будущем evaluation fault. Public scope expressions относятся к следующему срезу.
Database и serialization owners, refactoring helper; rules num-cast-try-from и
type-result-fallible. L11 attempts **0/5**.
FGDC LIVE header теперь 156 bytes (после прежних 144 идут kind/reserved/id),
MIN_BYTES 212. Запись/чтение проверяют type, class narrowing, nonzero graph и
совпадение статического source. Public query descriptors/API не изменены.
Initial codec focused `l9f-capture-identity-focused.log` PASS **7/7**, 1.06s.
Enriched focused `l9f-capture-identity-live.log` PASS **232/232**, 15.20s;
CLASS/GRAPH golden metadata, wrong owner, overflow/zero/reserved bytes и graph
class/record/all-occurrence roundtrip. Source review PASS: registration, cached
policy consumers, fault/close, recovery conversions и aggregate admission.
`l9f-capture-identity-lib.log` PASS **1028 passed / 10 ignored / 0 failed /
0 filtered**, 88.11s. `l9f-capture-identity-check.log` PASS, 211 existing warnings,
7.47s. `l9f-capture-identity-sdk-sync.log` PASS **1/1**, 0.53s, enriched qualified
GraphRecord SDK/GQL checkpoint/reopen, historical result/ACK/tail.
`l9f-capture-identity-sdk-native.log` PASS **1/1**, 0.56s, отдельный NativeAsync
target с feature enabled. Final nightly --check 9 files PASS;
`l9f-capture-identity-preserved.json`: **43 unchanged**.
Clippy delta (`l9f-capture-identity-clippy-delta.json`) **added 0**; общий lint gate
FAIL на прежних **6957 errors / 545 JSON warnings**. Source re-review PASS.
L9f correctness/resource/source/backend gates закрыты. Graphify update FAIL:
`l9f-capture-identity-graphify.log`, AST 17/17, Windows Errno 22 на signature;
tooling не изменялся, новый update будет после проверенного среза.
Stage 28 OPEN, L11 attempts **0/5**.

## L9g — class/record scope expressions

SDK GraphClassExpr/GraphRecordExpr владеют LiveExpr; syntax и binding dependency
visitors учитывают source expression. Lowering использует ordinary SELECT/RootRead,
root scope разрешается и нормализуется до row pull. QueryEvaluation возвращает
successful capture source; publication сохраняет его даже при пустом result delta,
fault сохраняет прежний owner. FGLD tags 5/6 имеют прежние depth/work/byte caps.
Initial check PASS (`l9g-scope-expr-initial-check.log`), 211 existing warnings,
24.42s. Initial focused compile FAIL: тест вызвал отсутствующий get_atom вместо
atom и оставил неоднозначный Vec element type. Исправлены только fixtures;
production check уже прошёл. Это новые ошибки тестов, не baseline debt.
Focused/review/broad/backend/resource gates пока OPEN. L11 attempts **0/5**.

L9g post-fix initial focused 3/3 PASS (0.05s). Fresh read-only review обнаружил
L9g-R1: неизменный String scope перепривязывался после remove/recreate имени.
Попытка восстановить прежний operand из before-root имела L9g-R2: future-SINCE
skip оставлял owner без обновления, а pending activation не имела before operand.
Выбран полный repair: durable `capture_scope_name` рядом с capture owner;
неизменный String разрешается по сохранённому GraphId, изменённый — по имени.
Success обновляет пару, fault сохраняет её; pending и future boundary используют
тот же anchor. FGDC fixed header 160, minimum 216; bounded UTF-8 payload.
Первый compile этой доработки обнаружил missing imports и test-only Text conversion;
они исправлены. Добавлены active/pending/future regressions, gates ещё OPEN.

Свежий review durable-pair repair PASS: R1/R2 закрыты по source inspection.
Дополнительно проверены decode name cap 128 bytes до allocation, UTF-8/NUL,
static-source rejection и обе converter directions. Execution evidence ещё
в работе; source review не заменяет focused/full/backend gates.
Enriched focused run 5/7 PASS, две fixture ошибки: projected value отсутствовал
в schema future-boundary fixture, PURE function ожидала string вместо обычного
runtime record-field type. Fixtures исправлены (declared int и checked any UDF);
production type/effect rules не менялись. Новые compile/type failures сохранены
в `l9g-scope-expr-*-focused.log`, не скрываются под будущим PASS.
Final focused 7/7 PASS (0.08s, `l9g-scope-expr-focused.log`): name reuse,
pending activation, deterministic future-clock boundary, SDK source-policy/fault,
GQL nested binding chain и external-record PURE scope, FGLD tags/budgets.
Ранний assertion record_sources был неверен для literal Record field input:
этот capture vector относится к record SELECT source owners; data-only switch
проверяется независимыми before/after values. Assertion удалён, contract сохранён.
Broad/lib, Sync/NativeAsync и rustdoc gates выполняются. Stage OPEN, attempts 0/5.
Broad LIVE 239/239 PASS (12.91s, `l9g-scope-expr-live.log`); полный lib
1035 passed / 10 ignored / 0 failed / 0 filtered PASS (86.99s,
`l9g-scope-expr-lib.log`). Backend/SDK/tooling validation продолжается.
Первый новый integration compile выявил test-only private intern_property call;
fixture переведён на опубликованную property dictionary после schema commit.
Никакая дополнительная production API не открывалась; corrected backend runs идут.
Следующий fixture run отклонён existing database-name cap 31 bytes: temporary
leaf использовал full UUID. Исправлен на общий короткий hex suffix; datastore
limits не менялись. Оба исходных integration failures сохранены в logs.
Expression-source saved-result fixtures: Sync PASS 1/1 (0.58s,
`l9g-scope-expr-sdk-sync-final.log`), NativeAsync PASS 1/1 (0.81s,
`l9g-scope-expr-sdk-native.log`). Обе публичные source variants переключают
qualified domain, сохраняют whole-commit before/after через checkpoint/reopen,
сравнивают replay с owned saved batch, ACK→tail, затем проверяют unchanged-name
remove/recreate fault и второе reopen в Paused. Check/rustdoc/lint/tooling идут.

Final check PASS (`l9g-scope-expr-check.log`), 211 existing warnings, 8.26s;
nightly --check 21 files PASS, protected 43 files unchanged. Initial Clippy
обнаружил **18 новых warning instances**: 17 tabs в новом doc example и один
collapsible_if. Узкая style phase (`rust-style-clippy`, rules lint-rustfmt-check,
lint-warn-style) сохранила evaluation order let-chain; новый example компилируется
отдельно как standalone_crate со space indentation. Lint policy/config не менялись,
allow/expect не добавлялись. Final rustdoc **8/8 PASS** (7 merged + 1 standalone);
post-style scope_expression **7/7 PASS**, frozen graph parameter **1/1 PASS**,
scoped nightly --check PASS. `l9g-scope-expr-clippy-final-delta.json`: **added 0**;
общий Clippy FAIL на прежних 6957 errors / 545 JSON warnings. Предыдущие 18 новых
diagnostics исправлены, исходный failure log сохранён. L9g correctness/resource/
source/backend gates CLOSED; general history, tooling и stage closure OPEN.
L11 attempts **0/5**.

L9g Graphify update **PASS** (`l9g-scope-expr-graphify.log`): AST 29/29,
57775 nodes / 155511 edges / 1870 communities. Installed tooling не изменялся;
предыдущие Errno 22 сохранены как отдельные failures. Final protected hash check:
43 unchanged. Общий tooling gate остаётся OPEN из-за baseline lint debt.

## L9h0 — scope пустого relation

До whole-graph expression API отделён internal zero-row source: analyzer больше
не заменяет его пустым graph-catalog scan с потерей source.graph. Empty сохраняет
typed scope, deterministic/effect validation, demand, copy admission и fingerprint;
encoding version 9. Count domain остаётся пустым, ordinary FROM GRAPH metadata
не меняется. Initial check PASS (`l9h-empty-scope-check-initial.log`), 211 existing
warnings, 11.65s. Initial focused **FAIL 0/2**: новая nested fixture использовала
ordinary top-level analyzer для scalar SELECT subquery. Исправлена на тот же
analyze_reactive_expression, что использует LIVE LET; typing contract не менялся.

Read-only review нашёл L9h0-R1: resolve_graph_reference сам не валидирует GraphRef
в catalog, а empty relation не имел последующего graph read. Repair использует
общий private validate_empty_graph: resolve once + existing read.graph против того
же snapshot с identity check, dependency/work/scratch admission. Все streaming,
pull и materialized Empty routes используют helper. Positive GraphRef и removed
GraphRef добавлены к String/missing/type cases для root/nested COUNT/LIMIT0.
Source re-review и focused gate выполняются. GraphExpr API/union normalization
deferred до correctness gate. Stage OPEN; L11 attempts **0/5**.

Fresh source re-review **PASS**, R1 закрыт по inspection всех трёх путей.
Repair focused run **1/2 PASS**: второе новое fixture expectation ожидало []
от scalar nested SELECT без rows, хотя ordinary evaluator возвращает NONE.
Expected value исправлено, production scalar/collection semantics не менялись.
Initial и interim failure logs сохранены. Final focused gate выполняется.

Final focused **2/2 PASS** (0.03s, `l9h-empty-scope-focused-final.log`): graph
String/GraphRef dependencies, missing/removed/type errors, root/nested COUNT0 и
LIMIT0, impure scope rejection и work cap. Scope expression сохраняется даже
при нуле classes; standalone scalar nested SELECT сохраняет обычный NONE.
Full lib и remaining checks выполняются. L11 attempts **0/5**.

Full lib **1037 passed / 10 ignored / 0 failed / 0 filtered PASS** (100.77s,
`l9h-empty-scope-lib.log`). Nightly --check 13 files PASS; protected **43 unchanged**.
Public local scope fixtures, check/Clippy и Graphify ещё выполняются. Никакого
L11 paired workload не запускалось; attempts **0/5**.

Final check **PASS** (211 existing warnings, 9.85s, `l9h-empty-scope-check.log`).
Существующие публичные expression-source regression fixtures: Sync **1/1 PASS**,
0.70s (`l9h-empty-scope-sdk-sync.log`); отдельно NativeAsync **1/1 PASS**, 0.74s
(`l9h-empty-scope-sdk-native.log`). Это qualified capture/reopen/ACK parity; новые
zero-domain cases проверены root-bound unit tests, storage layout не менялся.
Graphify update **PASS**: AST 20/20, 57786 nodes / 155551 edges / 1922 communities
(`l9h-empty-scope-graphify.log`). Final Clippy delta выполняется.

Final Clippy: **0 добавленных diagnostics**, 6957 прежних errors / 544 warnings
(`l9h-empty-scope-clippy-delta.json`); общий lint gate остаётся baseline debt.
Fresh source review PASS, включая pre-admission Static GraphRef: подготовка плана
резервирует copies до optimizer/helper cloning, `read.graph` отдельно допускает
snapshot view. Correctness/resource/source/backend gates L9h0 закрыты по evidence.
Stage 28, general history, L10/L11 и global tooling остаются OPEN; L11 **0/5**.

## L9h1 — whole-graph expression source

Добавлен owned SDK `LiveSource::GraphExpr`; общий analyzer/kernel строит class
union qualified occurrences. Root scope разрешается один раз, nested union
сохраняет shared scope в Empty wrapper и допускает copies до allocation. Descriptor
tag 7 использует существующий bounded expression codec; catalog header/WAL прежние.
Check initial PASS (211 existing warnings, 34.32s). Первоначальный focused build
FAIL: новые fixtures ссылались на отсутствующее BindingEvaluation.reads и имели
неоднозначные Object values. Fixture errors исправлены, production API не расширен.

Read-only source review **FAIL R1**: nested wrapper lower использовал restored
outer captures; собственный параметр терялся, shadowing менял graph при multi-class
domain. Finding accepted; regression до fix выполняется. Correctness/resource/
backend/source/tooling gates L9h1 OPEN, Stage OPEN; L11 attempts **0/5**.

R1-before regression воспроизвёл **FAIL 0/1** (0.02s, 1049 filtered,
`l9h1-graph-expr-r1-before.log`). Wrapper теперь lower scope в собственном capture
frame, с восстановлением frame также при ошибке. Focused suite и fresh independent
source review выполняются. Дополнен существующий Sync/NativeAsync public fixture:
GraphExpr tag 7, multi-class union, exact saved images, ACK/reopen и name reuse.

Focused после fix **3/3 PASS** (0.05s, 1047 filtered,
`l9h1-graph-expr-focused.log`), expanded LIVE **244/244 PASS** (13.68s, 806 filtered,
`l9h1-graph-expr-live.log`). Suite включает root/nested class-specific PURE overloads,
scope codec golden/truncation/depth и parameter-only/name-identity updates.
Nightly --check 15 files PASS; protected **43 unchanged**. Full lib и independent
review выполняются, L11 **0/5**.

Fresh independent source review **PASS**: R1 closed, wrapper frame восстанавливается
при обоих исходах; root/nested scope один, child copies admitted, Empty/PURE/read/
cancellation visitors сохранены. Full lib **1040 passed / 10 ignored / 0 failed /
0 filtered PASS** (88.77s, `l9h1-graph-expr-lib.log`). Public backend/rustdoc/check/
Clippy/Graphify gates выполняются; Stage OPEN, L11 **0/5**.

Public SDK fixture: local Sync **1/1 PASS**, 0.78s (10 filtered,
`l9h1-graph-expr-sdk-sync.log`); отдельно NativeAsync **1/1 PASS**, 0.84s (21 filtered,
`l9h1-graph-expr-sdk-native.log`). Проверены три scope descriptors, multi-class
GraphExpr, same-commit binding/data switch, exact replay/ACK и reopen/name reuse.
Graphify update **PASS**: AST 24/24, 57792 nodes / 155572 edges / 1898 communities
(`l9h1-graph-expr-graphify.log`). Remaining tooling gates выполняются.

Final check **PASS** (211 existing warnings, 6.60s, `l9h1-graph-expr-check.log`).
Rustdoc LIVE **8/8 PASS** (7 merged + 1 standalone, 14.64s,
`l9h1-graph-expr-rustdoc.log`): GraphExpr registration/close без query text вместе
с class/record API. Final Clippy delta выполняется; L11 **0/5**.

Final Clippy **0 добавленных diagnostics** (6957 baseline errors / 544 warnings,
`l9h1-graph-expr-clippy-delta.json`). Correctness/resource/source/backend и scoped
tooling gates L9h1 закрыты. Initial fixture failures и R1-before сохранены отдельно.
Общий lint debt, general history, оставшиеся SELECT clauses, L10/L11 и Stage 28
остаются **OPEN**. Paired performance gate не запускался; attempts **0/5**.

## L9i — GROUP BY/ALL и групповые identities

Выделен bounded per-class CASE из aggregate arguments. Refactor gate **1/1 PASS**
(`l9i0-group-refactor.log`, 0.03s); direct typed grouping **2/2 PASS**
(`l9i0-group-kernel.log`, 0.03s). Raw rows объединяются до ordinary reduction;
проверены AVG/DISTINCT, class-specific overloads/layouts, projected keys внутри
arithmetic, exact integers выше 2^53, отдельные NULL/NONE, empty BY/ALL и work cap.
Read-only kernel review **PASS**. Sorting key move убирает существовавший clone;
RowSet first остаётся в conservative row admission, cap escape не подтверждён.

Rust SDK получил private optional keys, `with_group_by`, `with_group_all`, `group`.
Group expressions входят в GQL recipes, captures/DAG, shared lowering и historical
complete-domain proof; row-only backfill не выдаёт grouped result за row-local.
FGLD query после filter содержит checked group tag и положительный u32 BY count;
layout меняется на месте по Q-40, без дополнительного decoder/WAL.
Initial cargo check **PASS**, 211 existing warnings (10.79s).

Initial focused **0/2 FAIL** (`l9i1-group-focused.log`): **F1** повторное computed
key lowering создавало разные synthetic slots; **F2** golden fixture ошибочно
читала u32 count как u64. Исправление F1 сохраняет identity lexical параметра/
LiveBindingId, а exact literal slots переиспользует по representation. Ordinary
group validation не ослаблена. F2 меняет только fixture. Повторные focused runs
**1/2 FAIL** (`l9i1-group-focused-fixed.log`, `l9i1-group-focused-final.log`)
проходили codec, но fixture пыталась ALTER LET captured input внутри BEGIN.
Следующий fixture run (`l9i1-group-focused-provenance.log`) также **1/2 FAIL**:
повторный LET в том же scope отклоняется. Fixture теперь проверяет ALTER LET
отдельно от BEGIN. Первый build дополнительных regressions выявил неоднозначный
Object::from_iter input; values типизированы до повторного запуска.

Source review обнаружило **F3**: GraphRef metadata внутри Range literal терялось
при slot reuse. Recursive Range bounds теперь сохраняют marker/name/kind/mode.
У разных reactive identities равенство текущих значений не объединяет slots.
Ревью также выявило **F4**: nested multi-class wrapper переиспользовал restored
outer alias для slot собственного параметра, несмотря на собственный captures
frame. Child alias теперь удерживается вместе с frame и восстанавливается при
ошибке. Existing nested capture regression проверяет две/три classes и shadowing.
Fresh focused/source/public backend/full gates выполняются. Stage **OPEN**;
performance gate L11 не запускался, attempts **0/5**.

Focused после F1/F3/F4 fixes **4/4 PASS**, 0.07s
(`l9i1-group-focused-revisions2.log`); expanded LIVE **250/250 PASS**, 16.75s
(`l9i1-group-live.log`). Проверены capture provenance при равных values, nested
scope shadowing, Range descriptor metadata, SDK graph groups, small scratch/work,
nested grouped scalar cardinality/collection, GQL net images/key-only commits и
rollback. Full lib, public backend и final review/tooling выполняются.

Full lib **1046 passed / 10 ignored / 0 failed / 0 filtered PASS**, 110.24s
(`l9i1-group-lib.log`). Nightly --check **14 files PASS**;
protected unrelated bytes **43 unchanged** (`l9i1-group-preserved.json`).

Fresh independent source review **PASS**, F1/F3/F4 closed. Initial public Sync
fixture build **FAIL E0624** (`l9i1-group-sdk-sync.log`): fixture пыталась использовать
private intern_property. Исправлена на existing public property_dictionary после
schema commit; production visibility/API не изменены. Повторный backend run выполняется.

Следующий Sync fixture **FAIL 0/1** (`l9i1-group-sdk-sync-fixed.log`): DELETE без
CASCADE при graph membership; fixture исправлена. После CASCADE **FAIL 0/1**
(`l9i1-group-sdk-sync-final.log`), `graph class statistic is absent`. Diagnostic
run (`l9i1-group-sdk-sync-diagnostic.log`) локализовал ошибку в publication,
не в private mutation. **F5**: primary replay гидратировал next, operation-image
capture replay клонировал current с deferred statistics. Тот же call pattern
виден в HEAD `196d21b`; baseline runtime отдельно не выполнялся.
Теперь before-root гидратируется до общей COW fork primary/capture replay.
Original versions, FCW/SSI, control/import boundaries остаются прежними.
Sync после F5 **1/1 PASS**, 0.45s (`l9i1-group-sdk-sync-hydrated.log`), включая
точные keys выше 2^53, owned images, checkpoint/reopen/ACK и cascade exit replay.
NativeAsync и fresh review/broad gates выполняются. L11 **0/5**.

NativeAsync **1/1 PASS**, 0.48s (`l9i1-group-sdk-native.log`). Fresh F5 review
**PASS**: original current identity/FCW/SSI сохраняются, ACK/close обход hydration
сохранён. Ревью выявило похожий прежний путь первого ON import segment: immutable
origin удерживается до prepare_mutation/hydration. Новый отдельный reopen/cascade
regression выполняется до исправления; open finding **F6**. Shared import/replay
component проверяется в рамках обязательного L6 correctness gate.

F6 before regression **0/1 FAIL**, 0.42s (`l9i1-capture-import-before.log`):
после reopen первый ON segment с DELETE CASCADE global/graph FULL feeds получает
тот же missing class counter. Existing spool helper теперь гидратирует working
origin после metadata admission и до immutable segment clone. Повторный regression
и source review выполняются; rollback/lease/capture flags не изменены.

F6 after regression **1/1 PASS**, 0.84s (`l9i1-capture-import-after.log`), global
и qualified FULL before/after сохранены после END и reopen. Fresh source review
**PASS**: hydration атомарна, savepoint сохраняет origin, OFF/lease/revisions
не меняются. Общая fixture выделена с backend options; отдельный NativeAsync
first-segment regression выполняется. Final full/check/rustdoc/Clippy/Graphify pending.

NativeAsync first-segment regression **1/1 PASS**, 0.56s (1068 filtered,
`l9i1-capture-import-native.log`). Проверены deletion после reopen с deferred
statistics, first ON segment, exact global/graph before/after и повторный reopen.
Sync и NativeAsync проверены отдельными targets; source review F5/F6 PASS.
Полный lib gate после F5/F6 **1047 passed / 10 ignored / 0 failed / 0 filtered
PASS**, 114.30s (`l9i1-group-lib-final.log`). Final check **PASS**, 8.75s,
211 existing warnings (`l9i1-group-check-final.log`). Nightly --check **17 files
PASS** (`l9i1-group-fmt-final.log`). Rustdoc/Clippy/Graphify выполняются;
L11 attempts **0/5**.

Final rustdoc **11/11 PASS** (10 merged + 1 standalone, 18.82s,
`l9i1-group-rustdoc-final.log`). Graphify после F5/F6 **PASS**, 57816 nodes /
155667 edges / 1902 communities (`l9i1-group-graphify-final.log`);
protected **43 unchanged** (`l9i1-group-preserved-final.json`).
Clippy initial delta **5 new warnings** — только tabs в новом rustdoc example,
6957 baseline errors (`l9i1-group-clippy-delta.json`). Простая замена на spaces
конфликтует с nightly format_code_in_doc_comments/hard_tabs: formatter возвращает
tabs. Пример переписывается в короткие отдельные bindings; lint/format policy
не меняются. Clippy/full tooling finding пока OPEN.

Final Clippy delta **0 added diagnostics**, 6957 baseline errors / 544 warnings
(`l9i1-group-clippy-final-delta.json`). Переписанный пример проходит nightly
--check (`l9i1-group-doc-fmt-fixed.log`); changed example rustdoc **1/1 PASS**,
0.01s (`l9i1-group-doc-fixed.log`, остальные examples уже прошли 11/11).
Production GROUP BY/ALL и F5/F6 correctness/source/resource/backend gates PASS.
Общий Clippy gate остаётся baseline debt; Stage 28, general recipes/history,
L10/L11 **OPEN**, attempts **0/5**.

## L9i2 — scalar constructors вокруг graph-wide aggregate

Новая direct SDK regression fixture сначала не компилировалась: неверные
ClassField/PropertyMap/LiveOptions signatures; corrected fixture использует
текущий API (`l9i2-constructors-before.log`). Before regression **0/1 FAIL**,
0.02s (`l9i2-constructors-before-fixed.log`): Array around count/sum отклонялся
shared aggregate union как unsupported outer expression. Gate проверяет 13
constructors against independent single-class domain и explicit runtime Values,
class-specific Int/Float input types, dead division-by-zero branches, optional
CASE/range bounds и work/scratch rejection.

Shared merge сохраняет constructor descriptors и recursively объединяет typed
children; aggregate row arguments по-прежнему lazy per-class CASE. CASE branch
storage и captured Cast target admitted через общий admit_special_copy (два
реальных consumers: merge/materialization). Evaluation IF/CASE остаётся обычным
lazy SELECT kernel. Focused/source gates выполняются; L11 attempts **0/5**.

Initial after regression **1/1 PASS**, 0.03s (`l9i2-constructors-after.log`).
Read-only source review **PASS**: descriptor guards и admission проверены в обоих
реальных consumers, ordinary laziness и class argument plans сохранены. Добавлены
10 typed outer mismatch scenarios на пустом domain, GQL joint data/parameter
net-result oracle и public composite result replay fixture для Sync/NativeAsync;
их expanded focused/backend gates выполняются.

Expanded focused **3/3 PASS**, 0.08s, 1057 filtered
(`l9i2-constructors-focused.log`). Все 10 descriptor mismatches отклоняются именно
typed incompatibility, включая empty inputs; GQL data + LIVE LET commit даёт одну
Aggregate identity и exact before/after, parameter-only commit и CANCEL проверены.
Public SDK Sync/NativeAsync, full lib и scoped tooling ещё выполняются.

Public composite-result Sync **1/1 PASS**, 0.73s, 12 filtered
(`l9i2-constructors-sdk-sync.log`). SDK recipe, exact keys >2^53, all nine
constructor descriptors и owned images переживают checkpoint/reopen, ACK и
cascade exit. NativeAsync проверяется отдельным feature target. Nightly --check
**5 files PASS** (`l9i2-constructors-fmt.log`); full/check/Clippy/Graphify pending.

NativeAsync composite-result **1/1 PASS**, 0.47s, 25 filtered
(`l9i2-constructors-sdk-native.log`). Full lib **1050 passed / 10 ignored /
0 failed / 0 filtered PASS**, 103.47s (`l9i2-constructors-lib.log`), включая
existing GQL/LIVE/changefeed/control-flow/resource suites. Check **PASS**, 6.61s,
211 existing warnings (`l9i2-constructors-check.log`). Protected **43 unchanged**
(`l9i2-constructors-preserved.json`). Graphify **PASS** — 57822 nodes / 155688 edges /
1901 communities (`l9i2-constructors-graphify.log`); zero-node config/context warnings
прежние. Scoped Clippy delta выполняется; closure остаётся OPEN, L11 **0/5**.

Final Clippy delta **0 added diagnostics**, 6957 baseline errors / 544 warnings
(`l9i2-constructors-clippy-delta.json`). L9i2 correctness/source/resource/backend и
scoped tooling gates **PASS**. FGLD variants/schema headers не менялись: compound
aggregate recipes используют существующие recursive children. Stage 28, remaining
clauses/general history, L10/L11 и global lint debt **OPEN**; attempts **0/5**.

## L9j — OMIT в SDK/GQL и durable recipes

Добавлены `LiveQuery::with_omit` и borrowed `omit`, ordered literal paths в
FGLD и общий OMIT/FETCH converter. Projection kernel остаётся обычным SELECT:
OMIT после projection, до FETCH; event identity отделена от payload. Общий
обход теперь проверяет cancellation/work/depth. Новые read dependencies не
создаются; historical complete-domain proof учитывает work paths. Empty FGLD
Query — 61 bytes, catalog minimum — 221 bytes; Q-40 inplace, без compat decoder.

Focused **6/6 PASS**, 0.08s, 1058 filtered (`l9j-omit-focused-fixed.log`):
SDK row/group identities, literal dots/duplicates/missing/non-object paths,
OMIT-before-FETCH с отсутствием удалённого external read, GQL net oracle и
omitted-only idle, historical linked-member replay, descriptor malformed
counts/segments/text/UTF-8/NUL/truncation/scratch/work и cancellation/work guard.
Первый compile остановился на двух ambiguous `.into()` в test Object fixtures
(`l9j-omit-focused.log`); explicit Value исправил fixtures, production не менялся.
Read-only fresh review **PASS**, actionable findings нет. Catalog/full lib,
public Sync/NativeAsync replay и tooling gates выполняются; Stage 28 OPEN,
L11 attempts **0/5**.

Public SDK Sync **1/1 PASS**, 0.29s, 13 filtered
(`l9j-omit-sdk-sync-fixed.log`); NativeAsync **1/1 PASS**, 0.27s, 27 filtered
(`l9j-omit-sdk-native.log`). Recipe, exact integers >2^53 и owned before/after
переживают checkpoint/reopen/ACK и удаление primary row; omitted-only commit
не создаёт delivery. Первый public fixture compile ошибочно использовал
crate-private intern_property (`l9j-omit-sdk-sync.log`); исправлен на публичный
property_dictionary после schema commit. Nightly fmt --check **12 files PASS**
(`l9j-omit-fmt.log`); protected **43 unchanged** (`l9j-omit-preserved.json`).
Full lib/Rustdoc/check/Clippy/Graphify pending; L11 **0/5**.

Full lib **1054 passed / 10 ignored / 0 failed / 0 filtered PASS**, 117.15s
(`l9j-omit-lib.log`), включая FGLD goldens и FGDC malformed embedded OMIT count.
Rustdoc LIVE **12/12 PASS** (11 merged + 1 standalone), 16.35s
(`l9j-omit-rustdoc.log`). Check **PASS**, 9.68s, 211 existing warnings
(`l9j-omit-check.log`). Clippy delta **0 added diagnostics**, 6956 baseline
errors / 544 warnings (`l9j-omit-clippy-delta.json`); global lint gate OPEN.

Первый Graphify update завершился после AST **21/21** с Windows `[Errno 22]`
на `.graphify_labels.json.sig` (`l9j-omit-graphify.log`). Graph/report уже обновлены:
**57835 nodes / 155739 edges / 1934 communities**; частичный результат не назван
успехом команды. Sidecar сохранён в `l9j-graphify-labels-before.sig`; native и
Python rewrite идентичных bytes проходят, SHA-256 unchanged (probe log).
Installed package не изменён. Повторный update выполняется; L11 attempts **0/5**.

Повторный Graphify update **PASS**, AST **3/3** (`l9j-omit-graphify-retry.log`):
topology unchanged, новые core artifacts сохранены, manifest обновлён. Первую
ошибку записи нельзя устойчиво воспроизвести probe; причина не объявлена
исправленным Python/package bug. L9j correctness/resource/backend/source и scoped
tooling gates **PASS**; общие global Clippy, оставшиеся SELECT clauses/history,
L10/L11 и Stage 28 **OPEN**. Performance attempts **0/5**.

## L9k0 — общий bounded result window

Новый `select_window` разрешает LIMIT/START OFFSET через существующий exact
Number→usize evaluator до row pull. Root SELECT и grouped class union применяют
одно окно к `CursorRecord`, сохраняя source/group anchors. Nested values используют
wrapper того же pager; skipped records проверяют work/cancellation. При LIMIT 0
scope всё равно валидируется и наблюдается. START Offset/After expression получает
admission до optimizer/union clone; standalone retained cursor остаётся session
operation. Новых dependencies/executor/format changes нет.

Первый focused compile FAIL из-за test fixtures: Outcome требует `value()`, Value
не имеет From<i128> (`l9k0-window-focused.log`). После исправления 3/4 PASS и одна
parse error из-за положения OMIT (`l9k0-window-focused-fixed.log`); clause исправлен
без production change. Полный focused **6/6 PASS**, 0.06s, 1064 filtered
(`l9k0-window-focused-complete.log`): ordinary SELECT oracle для streaming/order/
group windows, точные ties >2^53, qualified identity после OMIT, GROUP ALL union,
negative/fractional/wrong/overflow bounds до BaseAtom reads, nonempty graph LIMIT 0,
oversized START pre-clone и skip cancellation/work/anchors. Fresh read-only review
**PASS**, findings нет. Его residual coverage (forced disk/invalid Empty) добавлена
в тесты перед full lib gate. `cargo check --lib --no-default-features` **PASS**, 7.81s,
211 existing warnings (`l9k0-window-check.log`). Scoped Clippy **0 added diagnostics**,
6954 existing errors / 544 warnings (`l9k0-window-clippy-delta.json`); global lint
gate остаётся OPEN. Full lib/format/Graphify evidence выполняется.

Это kernel срез; SDK/descriptor windows, row-local global union и grouped ORDER
merge — следующий L9k срез. Stage 28 и L10/L11 **OPEN**, performance attempts **0/5**.

Финальный full lib/datastore gate **1060 passed / 10 ignored / 0 failed /
0 filtered PASS**, 118.16s (`l9k0-window-lib.log`). Все шесть window tests PASS,
включая добавленные TEMPFILES и Empty-source invalid bounds; existing cursor/
subquery/grouping/LIVE/changefeed/local recovery suites не регрессировали.
Selected nightly format **4 files PASS** (`l9k0-window-fmt.log`); protected
**43 unchanged** (`l9k0-window-preserved.json`). Graphify выполняется. L11 **0/5**.

Graphify first update: AST 11/11, core **57853 nodes/155801 edges/1915 communities**,
затем FAIL на Windows Errno22 signature (`l9k0-window-graphify.log`). Сохранён
sidecar, native identical rewrite **PASS** и SHA-256 unchanged
(`l9k0-graphify-signature-probe.json`); installed package не менялся. Standard
retry **PASS**, AST 3/3, topology unchanged (`l9k0-window-graphify-retry.log`).
L9k0 correctness/resource/source/scoped tooling gates PASS; global Clippy и
Stage 28 остаются OPEN. L11 **0/5**.

## L9k1 — global class union и grouped ORDER

Row-local union сохраняет per-class projection/wildcards и comparison keys,
затем использует общий QuerySorter/window. Root и nested consumers сохраняют
anchors и квалифицированные identities; nested maximum применяется после global
START. Unordered окна используют прежний streaming evaluator по классам и
останавливают projection/FETCH после global limit; это сохраняет видимость ошибок
ordinary SELECT. OFFSET по-прежнему вычисляет пропущенные projections, как existing
streaming path. Без новых clauses прежний row-local путь остаётся прежним.
Group ORDER aliases rebased за padded input/class discriminator; direct aggregates
переиспользуют merge_expression с per-class arguments/overloads. ORDER flags и
row windows проверяются до source pull; новые owned recipes/codecs пока не вводятся.

Initial focused **9/9 PASS**, 0.10s (`l9k1-union-focused.log`), затем полный
focused **11/11 PASS**, 0.09s (`l9k1-union-focused-complete.log`): разные class
widths, wildcard/OMIT, exact ties >2^53, global ordered windows в RAM/TEMPFILES,
group alias/count/PURE overload ORDER, mismatch preflight без BaseAtom reads,
unordered early stop перед division-by-zero и nested cap после global START.
Anchor clone получил отдельный checked admission каждого Value после этого run;
full lib gate выполняется. `cargo check --lib --no-default-features` **PASS**,
10.51s (`l9k1-union-check.log`). Fresh read-only review **PASS**, actionable findings
нет, Cargo reviewer не запускал. Scoped Clippy **0 added diagnostics**, 6954 existing
errors / 544 warnings (`l9k1-union-clippy-delta.json`); общий lint gate OPEN.
SDK/window descriptor, general history, L10/L11 продолжаются; attempts **0/5**.

Final full lib/datastore **1065 passed / 10 ignored / 0 failed / 0 filtered PASS**,
91.75s (`l9k1-union-lib.log`), включая все 11 window tests после anchor admission
и existing snapshot/SSI/query/import/recovery suites. Final check **PASS**, 6.88s,
211 existing warnings (`l9k1-union-check-final.log`). Scoped format **7 files PASS**
(`l9k1-union-fmt.log`), protected **43 unchanged** (`l9k1-union-preserved.json`).
Graphify update **PASS** (`l9k1-union-graphify.log`): 57876 nodes, 155890 edges,
1912 communities; AST 14/14, LLM cost 0. Stage 28 OPEN; L11 **0/5**.

## L9k2 — owned ORDER/LIMIT/START recipes

Добавлены публичные `LiveOrder`, `LiveOrderKey`, собственные direction/nulls enums,
builders/accessors и expression bounds в `LiveQuery`. Bare Name сохраняет ordinary
projected alias/input precedence; Expression fields остаются source-qualified.
GQL допускает deterministic ORDER и exact LIMIT/START Offset через тот же typed
pipeline. RAND и START AFTER отклоняются до registration. ORDER выбирает membership,
net events сохраняют canonical identity order, без rank-only notification.

Все operands проходят GQL capture binding, DAG/read dependency traversal и
historical complete-domain proof. Row-only backfill отклоняет window recipes;
неизменные конкуренты восстанавливаются из captured source domain. FGLD сохраняет
checked ORDER keys/flags/reserved bytes и optional bound expressions; Q-40 layout
inplace, empty query body 27/frame 67 bytes, FGDC minimal LIVE entry 227 bytes.
Новых dependencies/executors/worker нет.

Initial `cargo check --lib --no-default-features` **FAIL**, change: explicit lifetime
в helper `Lowering::order` отсутствовала (`l9k2-window-api-check.log`). Аннотация
`&'query LiveOrder` исправлена без clone/lifetime expansion публичного API.
Focused SDK/GQL/codec tests, source review и separate Sync/NativeAsync reopen
gates выполняются; closure не заявлен. Performance attempts L11 **0/5**.

Initial focused run **17 passed / 1 failed / 1064 filtered**, 0.22s
(`l9k2-window-api-focused.log`): alias fixture ошибочно использовала unary minus
с optional Int, который ordinary analyzer отклоняет. Fixture переведена в отдельный
nonoptional Int domain; production numeric semantics не расширялась. Добавлен
invalid reactive bound → fault при успешном source DML → explicit reregistration
после восстановления bound. Fresh source review **PASS**, actionable findings нет;
Clippy/build/full/backend evidence ещё требуются. L11 **0/5**.

Repaired focused **18/18 PASS**, 0.20s (1064 filtered;
`l9k2-window-api-focused-repaired.log`). Alias fixture сохраняет exact ties >2^53;
new fault/recovery scenario также PASS. Initial failure не относится к L11 попыткам.
Full lib/datastore и separate local backend gates запущены; срез ещё не закрыт.

Initial full lib **1070 passed / 2 failed / 10 ignored / 0 filtered**, 107.07s
(`l9k2-window-api-lib.log`). Оба change-caused failures — устаревшие fixture
assertions: LIMIT 1 раньше отклонялся и OMIT offsets ещё не учитывали новый
6-byte empty window. Тесты обновлены на запрет RAND/START AFTER и точные новые
OMIT offsets. Production semantics после focused PASS не менялась; повторные
focused/full gates обязательны. Protected 43 unchanged. L11 **0/5**.

Expanded focused LIVE suite **266/266 PASS**, 18.60s (816 filtered;
`l9k2-window-api-live-focused.log`), включая updated negative syntax/OMIT golden
и existing local publication/recovery. Первый Graphify update **FAIL** после
AST 24/24/core publication: recurring Windows Errno22 на signature sidecar.
Core report: 57931 nodes/156037 edges/1896 communities, LLM cost 0. Native
same-byte probe **PASS**, 52426 bytes/hash unchanged (`l9k2-graphify-signature-probe.json`);
installed package не менялся. Стандартный retry выполняется, L11 **0/5**.

Repaired full lib/datastore **1072 passed / 10 ignored / 0 failed / 0 filtered PASS**,
97.96s (`l9k2-window-api-lib-repaired.log`), без новых production изменений после
focused PASS. Separate public local Sync/NativeAsync и tooling продолжаются;
performance attempts **0/5**.

Initial public Sync build **FAIL** (`l9k2-window-api-sync.log`), change: generic
`Object::from_iter` fixture value `1.into()` не задавала конкретный тип. Исправлено
на owned `Value::from(1_i64)`, production API не менялся. Первый runtime **FAIL**
0/1 (`l9k2-window-api-sync-repaired.log`): fixture применяла COLLATE/NUMERIC к Int,
что ordinary SELECT отклоняет. ORDER key переведён в explicit `<string>age` через
существующий Cast recipe для проверки всех flags и numeric-string order после
reopen. Это correctness/build evidence, performance attempts остаются **0/5**.

Public Sync **1/1 PASS**, 0.23s (`l9k2-window-api-sync-checked.log`): descriptor
equality после checkpoint/reopen, whole-commit membership enter/leave и saved images
без FETCH current primary. NativeAsync запущен отдельно. Graphify retry **PASS**
(`l9k2-window-api-graphify-retry.log`), AST 7/7: 57931 nodes/156037 edges/1904
communities, LLM cost 0. Scoped rustfmt check **16 files PASS**
(`l9k2-window-api-fmt.log`); финальный backend fixture format проверяется повторно.
Remaining check/rustdoc/Clippy evidence pending, Stage 28 OPEN, L11 **0/5**.

Public NativeAsync **1/1 PASS**, 0.28s (29 filtered;
`l9k2-window-api-native.log`), отдельный `--features native-async-io` target.
Backend fixture только compile/semantics repairs; production diff после full PASS
не менялся. Rustdoc и final check/Clippy выполняются. L11 **0/5**.

Selected Rustdoc namespace `live::` **11/11 PASS**, 14.26s
(`l9k2-window-api-rustdoc.log`), включая новый window builder. Финальный `live`
filter дополнительно включает Datastore methods для сравнения с prior evidence.
Final `cargo check --lib --no-default-features` **PASS**, 21.96s, 211 existing
warnings (`l9k2-window-api-check-final.log`); scoped rustfmt **16 files PASS**
(`l9k2-window-api-fmt-final.log`). Clippy delta pending; L11 **0/5**.

Final Rustdoc filter `live` **13/13 PASS**, 7.46s
(`l9k2-window-api-rustdoc-final.log`). Initial Clippy delta **7 added**
(`l9k2-window-api-clippy-delta.json`): W1 doc tabs (3 warnings), W2 exported enums
(2), W3 impl Trait parameter (1), W4 parent filename (1 error). Все accepted.
W1 получает узкий method allow из-за required nightly hard tabs; W2 следует
non-exhaustive SDK policy; W3 сохраняет `Into<Text>` через named generic.
W4 mechanical move `live_descriptor.rs` → `live_descriptor/mod.rs` оставил SHA-256
неизменным (`l9k2-descriptor-layout-proof.json`), namespace/privacy/bytes прежние.
Baseline JSONL сохраняется; known path mapping нормализуется в отдельном copy
для сравнения тех же diagnostics (`l9k2-clippy-layout-normalization.json`).
Focused/check/full/backend и fresh review после repair required. `git diff --check`
также сообщает trailing whitespace в нетронутом fragment другого fixture
`tests/live_local.rs:856`; этот fragment сохранён. Stage 28 и L11 OPEN, **0/5**.

L9k2 repairs W1–W4 прошли fresh source re-review **PASS**, actionable findings
нет. `l9k2-window-api-clippy-repaired-delta.json` показывает **0 added** при
6954 существующих errors/544 warnings. Original baseline JSONL сохранён;
filename-only mapping для mechanical move имеет отдельный proof, diagnostics
не исключались по коду или сообщению. Global Clippy exit 101 не объявляется PASS.

Post-repair full lib/datastore **1072 passed/10 ignored/0 failed/0 filtered PASS**,
106.04s (`l9k2-window-api-post-lint-lib.log`); focused **18/18 PASS**, 0.17s
(`l9k2-window-api-lint-repair-focused.log`). Post-repair check **PASS**, 9.68s,
211 existing warnings (`l9k2-window-api-post-lint-check.log`); scoped nightly
rustfmt **16 files PASS** (`l9k2-window-api-post-lint-fmt.log`). Preservation proof
`l9k2-window-api-post-lint-preserved.json`: **43 unchanged**. Rustdoc/backend и
final Graphify выполняются; Stage 28 OPEN, L11 **0/5**.

L9k2 post-repair public targets: selected Rustdoc **13/13 PASS** (12 обычных
и 1 merged test; `l9k2-window-api-post-lint-rustdoc.log`), Sync **1/1 PASS**,
0.25s (`l9k2-window-api-post-lint-sync.log`), NativeAsync **1/1 PASS**, 0.24s,
отдельный `--features native-async-io` target
(`l9k2-window-api-post-lint-native.log`). Coverage проверяет checked recipe,
checkpoint/reopen, exact saved enter/leave images и ACK. Все correctness,
resource, backend и Clippy-delta gates этого среза PASS; final Graphify запущен.
L11 **0/5**, этап остаётся OPEN.

L9k2 final Graphify **PASS**, AST 10/10; core 57930 nodes/156036 edges/1918
communities (`l9k2-window-api-graphify-final.log`), source layout refresh выполнен.
Срез закрыт по полученным correctness/backend/Clippy-delta/tooling evidence;
joins/SPLIT/ONLY/index hints/general history/L10/L11 и Stage 28 остаются OPEN.

L9l0 shared ONLY kernel начат после L9k2 gate. Первый focused build **FAIL**
(`l9l0-only-focused.log`): новый fixture использовал несуществующий `graph_ref`;
исправлено на checked существующий `named_graph`, production контракт не менялся.
Focused differential проверяет global window, grouping/nested union и прежний
projection error precedence. Это correctness evidence; L11 **0/5**.

L9l0 repaired focused **20/21 PASS, 1 FAIL**, 0.19s
(`l9l0-only-focused-repaired.log`): global window VALUE id получил
`InvalidStorageFile("LIVE union output lost its source identity")`. Source review
локализовал O1: compact source row содержит id в column 0 и пустой stable vector;
unordered union копировал только stable. Обычный single-root visitor читает id
из column 0 и не имеет этой проблемы. Selected repair использует этот же
borrowed id только в union callback, с existing admission до allocation;
общий compact-row layout и непрофильный query path не изменяются. История
первого FAIL сохранена, узкий reproduction запущен. L11 **0/5**.

L9l0 narrowed **FAIL** 0/1 (`l9l0-only-union-narrowed.log`) подтвердил exact
reproduction `START AT 2 LIMIT 1`. После O1 repair focused **21/21 PASS**, 0.34s
(1064 filtered; `l9l0-only-o1-focused.log`). Fallback anchor берётся borrowed
slice из logical id только при пустом stable, count/value bytes допускаются
до `to_vec`; source row layout не менялся. Общий kernel сохраняет ordinary
FG-22F22 после projection/OMIT/FETCH/window, streaming cap2 и полное вычисление
barriers. Full lib, check, Clippy delta и fresh review выполняются. L11 **0/5**.

L9l0 full lib/datastore **1075 passed/10 ignored/0 failed/0 filtered PASS**,
92.74s (`l9l0-only-lib.log`); check **PASS**, 8.02s, 211 existing warnings
(`l9l0-only-check.log`); scoped rustfmt **4 files PASS** (`l9l0-only-fmt.log`).
Fresh O1 source re-review **PASS**, actionable findings нет. Review ограничен
shared kernel/fallback, public/frontend и manually mixed private nested IR
отложены. Clippy delta выполняется; Stage 28 OPEN, L11 **0/5**.

L9l0 Clippy delta **0 added** (6954 existing errors/544 warnings;
`l9l0-only-clippy-delta.json`), full/check/fmt и fresh review PASS. Graphify
**PASS**, AST 9/9, core 57935 nodes/156062 edges/1913 communities
(`l9l0-only-graphify.log`). Kernel срез закрыт, публичный L9l1 начат. Статус
этапа не меняется; L11 **0/5**.

L9l1 public ONLY recipe/default/accessor, GQL/root/nested union lowering и checked
FGLD query flags реализованы. Бит ONLY использует существующий final flags byte;
default bytes/minima 67/227 сохраняются. Changed-row history запрещён для ONLY,
complete-domain proof прежний. Первый focused build **FAIL**
(`l9l1-only-focused.log`): два новых fixtures ошибочно использовали
`LiveExpr::Query`, исправлены на существующий `LiveExpr::Select`. Public API
не переименовывался. Новый focused run и read-only source review выполняются;
history/cardinality/backend/tooling gates ещё не объявляются пройденными. L11 **0/5**.

L9l1 продолжен 2026-10-07. Repaired focused после устранения ошибочных expectations:
**26/26 PASS**, 0.27s (1064 filtered),
`target/stage28-baseline/l9l1-only-fault-repaired-focused.log`.
Исходный focused **24/26 PASS, 2 FAIL** сохранён: active fault имеет нормативную
обёртку FG-G1F09 с точной FG-22F22 cause; tests ожидали cause на верхнем уровне.
Исправлены только SDK/GQL assertions envelope и cause. Production error/publication
поведение не менялось. Historical complete-domain ONLY проверка также PASS;
registration error остаётся direct FG-22F22. Full/backend/rustdoc/tooling и
fresh review выполняются. Stage OPEN, L11 **0/5**.

L9l1 закрыт: полный lib/datastore **1080 passed/10 ignored/0 failed/0 filtered
PASS**, 85.37s (`l9l1-only-lib.log`); check **PASS**, 8.89s
(`l9l1-only-check.log`). Public Sync **1/1 PASS**, 0.24s; NativeAsync **1/1 PASS**,
0.20s (`l9l1-only-sync.log`, `l9l1-only-native.log`). Rustdoc LIVE **14/14 PASS**
(13 ordinary + 1 merged), 10.84s (`l9l1-only-rustdoc.log`). Scoped rustfmt
**10 files PASS** (`l9l1-only-fmt.log`). Clippy delta **0 added**, existing
6954 errors/544 warnings (`l9l1-only-clippy-delta.json`); global command exit101,
общий lint gate OPEN. Fresh read-only bounded source review **PASS**.
Protected **43 unchanged** (`l9l1-only-preserved-final.json`). Graphify **PASS**:
AST 18/18, 57942 nodes/156089 edges/1901 communities (`l9l1-only-graphify.log`),
LLM cost 0. context7.json/config.json дали zero AST nodes и не представлены в
graph; remapped hub labels не являются новым semantic labeling evidence.
Остальные Stage28 gates OPEN, L11 **0/5**.

L9m начат по ответу пользователя: JOIN/SPLIT получают составную qualified identity
с slots источников в syntax order, явным LEFT unmatched и zero-based SPLIT
positions. Source graph каждого участника сохраняется независимо; GROUP identity
не меняется. Scan/output ordinals и physical join order не становятся durable
identity. Обнаружены реальные seams: общий join стирает right graph, stable хранит
physical-order records и не включает LEFT missing; projection добавляет output
ordinal. Поэтому до снятия syntax guards требуются runtime/codec и bounded
provenance gates. На этом месте support JOIN/SPLIT ещё не объявляется готовым.

L9m0 runtime/codec реализованы; fresh bounded source review **PASS**, без findings.
`cargo check --lib --no-default-features` **PASS**, 21.34s
(`l9m0-composite-check.log`). Default focused build и повтор с `-j 1` оба
завершились `rustc-LLVM ERROR: out of memory` до выполнения тестов
(`l9m0-composite-focused.log`, `l9m0-composite-focused-j1.log`). Это tooling
resource failure; peak compiler memory не измерен, change-specific trigger не
доказан. Win32 GlobalMemoryStatusEx после failure: physical 16.87 GB, free 5.87 GB,
commit limit 24.09 GB, available commit 4.86 GB (`l9m0-memory-after-oom.json`).
D: free 74.65 GB. CIM read через sandbox был недоступен; automatic approval
review retry вышел по deadline. Safe Win32 read дал необходимые observations
без изменения процессов/pagefile/settings. Identical focused tests запущены с
command-local `--config profile.test.debug=0 -j1`; optimization/debug assertions
и source не меняются. Такой PASS не объявит default-symbol build исправленным.
L9m0 correctness gate ещё OPEN; JOIN/SPLIT lowering не включён. L11 **0/5**.

L9m0 focused alternative-profile evidence **3/3 PASS**, 0.01s (1090 filtered),
`l9m0-composite-focused-no-symbols.log`. Command-local test debug=0/j1,
unoptimized/debug assertions unchanged; build 4m42s. Default-symbol OOM failures
сохранены и не считаются исправленными. Source review PASS и scoped nightly
rustfmt **6 files PASS** (`l9m0-composite-fmt.log`). Full lib с тем же profile
override выполняется. L11 **0/5**.

L9m0 полный lib/datastore с command-local test debug=0/j1: **1083 passed/10
ignored/0 failed/0 filtered PASS**, 81.76s
(`l9m0-composite-lib-no-symbols.log`). Это полный execution correctness gate
указанного profile; default-symbol build остаётся tooling/resource limitation.
Clippy/rustdoc и Graphify evidence ещё выполняются. L11 **0/5**.

L9m0 initial Clippy delta **1 added**: redundant_pub_crate для shape helper
в private result_identity module (`l9m0-composite-clippy-delta.json`). Helper
сделан pub внутри private module; единственный outward re-export остаётся
pub(crate), public SDK surface не расширена. Повторная delta выполняется.
Rustdoc LIVE с command-local test debug=0/j1 **15/15 PASS** (14 ordinary +1 merged),
3.39s (`l9m0-composite-rustdoc-no-symbols.log`), включая прямой SDK Composite
пример с u64::MAX. Visibility-only repair не меняет bytes или runtime behavior.
L11 **0/5**.

L9m0 repaired Clippy delta **0 added**, existing 6954 errors/544 warnings
(`l9m0-composite-clippy-repaired-delta.json`); global lint command exit101.
Final default `cargo check` **PASS**, 5.58s (`l9m0-composite-check-final.log`),
scoped fmt **6 files PASS**, protected **43 unchanged** (`l9m0-composite-fmt-final.log`,
`l9m0-composite-preserved.json`). Graphify **PASS**, AST14/14, 57962 nodes/
156158 edges/1912 communities (`l9m0-composite-graphify.log`); zero-node
context7.json/config.json limitation unchanged. Runtime/codec focused correctness
gate закрыт указанным no-symbol profile evidence; default test build OOM не
скрывается. JOIN/SPLIT producers/recipes и общие Stage28 gates OPEN. L11 **0/5**.

L9m1a mechanical extraction выполнен: WHERE/spatial refinement и последовательный
SPLIT перенесены в concrete prepare_select_rows, основной SELECT finishing
вызывает его перед grouping/projection. Existing body и controls сохранены;
Source identity/guards/bytes не менялись. Bounded diff source inspection PASS.
Existing focused window/ONLY **26/26 PASS**, 0.20s (1067 filtered), с test debug=0/j1
(`l9m1a-refactor-focused.log`). Полный execution gate выполняется.
L11 **0/5**.

L9m1a полный lib/datastore с test debug=0/j1 **1083 passed/10 ignored/0 failed/
0 filtered PASS**, 81.01s (`l9m1a-refactor-lib-no-symbols.log`). Mechanical
source diff сохраняет original filtering/SPLIT body; final check/fmt/Clippy
выполняются перед расширением consumers. L11 **0/5**.

L9m1b initial focused **1/2 PASS, 1 FAIL** (`l9m1b-split-focused.log`): новая
OMIT fixture поставила SPLIT после OMIT, нарушив существующий SELECT clause order.
ParseError offset29 на SPLIT; kernel/grammar не меняются. Fixture исправлена на
SPLIT ... OMIT; expanded focused window gate выполняется. Root row/aggregate
class-union checks ещё не объявляются завершёнными. L11 **0/5**.

L9m1b repaired focused **28/28 PASS**, 0.27s (1067 filtered),
`l9m1b-split-focused-repaired.log` с test debug=0/j1. Exact compound/duplicate
positions, source scope и OMIT, forced spill, WHERE-before-SPLIT, row/group
class-union и global window/ONLY сравниваются с independent ordinary SELECT.
Full lib и source review выполняются; public LIVE SPLIT recipes ещё не включены.
L11 **0/5**.

L9m1b before-P1 full **1085 passed/10 ignored/0 failed/0 filtered PASS**, 82.16s
(`l9m1b-split-lib-no-symbols.log`). Fresh source review нашёл **P1**: общий
SPLIT loop клонировал rows/growing keys до bounded admission и не проверял
cancellation/work на каждом expansion; collector мог скрыть это через spill.
Finding принят, slice closure заблокирован до исправления.
Shared split_rows теперь проверяет control, work, complete row + checked key
growth и deep values до clone; growth использует try_reserve_exact. Ordinary
unbounded semantics сохранены, добавлена cooperative cancellation. Conservative
RootFields charges могут раньше отвергнуть build, соответствуя existing policy.
Regression directly isolates input column0 от 256 KiB unrelated column1;
проверяет cancellation после input evaluation, exhausted work и 64 KiB scratch
до clone/spill. First fixture compile E0639 TextSpan external non-exhaustive
сохранён (`l9m1b-p1-focused.log`); исправлен на existing TextSpan::new.
Repaired focused **3/3 PASS**, 0.09s (`l9m1b-p1-focused-repaired.log`). Fresh
P1 source re-review conditional PASS; post-fix full/check/fmt/Clippy pending.
L11 **0/5**.

L9m1b post-P1 full **1086 passed/10 ignored/0 failed/0 filtered PASS**, 85.88s,
test debug=0/j1 (`l9m1b-p1-lib-no-symbols.log`). Source re-review закрывает P1
по shared pre-copy guard и direct regression fixture. Final check/fmt/Clippy
выполняются; new public SPLIT clause ещё deferred. L11 **0/5**.

L9m1b kernel correctness/resource gate закрыт: post-P1 full1086/10 ignored/0
failed/0 filtered PASS с test debug=0/j1; final default check **PASS**, 5.85s,
scoped fmt **3 files PASS**, Clippy delta **0 added** (6954/544 existing),
`l9m1b-p1-check.log`, `l9m1b-p1-fmt.log`, `l9m1b-p1-clippy-delta.json`.
Protected **43 unchanged** (`l9m1b-preserved.json`). Graphify **PASS**:
57968 nodes/156192 edges/1904 communities (`l9m1b-split-graphify.log`),
zero-node context7.json/config.json limitation unchanged. Runtime/codec и shared
SPLIT identity kernel готовы к public clause slice; JOIN/recipes/L10/L11 OPEN.
Default-symbol OOM и global lint debt не закрыты. L11 **0/5**.

L9m2 public SPLIT builder/accessor, frontend/lowering, captures/DAG, historical
proof и optional checked FGLD tail реализованы. New tests SDK/GQL/descriptor
ещё проверяются. Initial fixture compile failures E0277 &&str (поле принимает
Into<Text>) и E0282 empty Vec type сохранены (`l9m2-split-focused.log`,
`l9m2-split-focused-repaired.log`). Fixtures исправлены dereference path и
Vec<Value>; public API/types/production semantics не менялись для этих fixes.
Focused gate ещё OPEN; retention/backend/history evidence впереди. L11 **0/5**.

L9m2 дальнейшие fixture compile failures E0283 ambiguous Number/Value conversion
и E0596 mutable named_graph lookup сохранены в repaired2/3/4 logs. После fixes
5/6 PASS, SDK row failed с unknown __fg_live_source_0. Narrowed reproduction
(`l9m2-split-sdk-narrowed.log`) установил fixture error: field("properties.tags")
задаёт один literal segment; составной путь требует LivePath с двумя segments.
Fixture исправлена без изменения production field semantics. Focused **6/6
PASS**, 0.12s (`l9m2-split-path-focused.log`), test debug=0/j1. Добавлены
complete-domain window/no-anchor GAP tests и separate Sync/NativeAsync saved
qualified Composite replay/ACK/tail fixture; их evidence ещё проверяется.
L11 **0/5**.

L9m2 extended focused **8/8 PASS**, 0.13s (1093 filtered),
`l9m2-split-history-focused.log`: complete-domain window с unchanged competitor
и no creation anchor GAP включены. First public Sync fixture compile failure
E0624 private intern_property сохранён (`l9m2-split-local-sync.log`); fixture
переведена на публичный property_dictionary после schema commit. Repaired Sync
**1/1 PASS**, 0.30s (`l9m2-split-local-sync-repaired.log`), с test debug=0/j1.
Qualified duplicate positions и saved before/after проверены после primary
empty, checkpoint/reopen и persisted ACK/tail. NativeAsync и broad gates pending.
Scoped fmt12 files PASS; protected43 unchanged (`l9m2-preserved.json`). L11 **0/5**.

L9m2 NativeAsync **1/1 PASS**, 0.26s (`l9m2-split-local-native.log`), separate
feature target с test debug=0/j1. Fresh baseline-bounded source review PASS,
без actionable findings: capture/DAG, root/nested lowering, complete-domain
proof и checked canonical tail inspected. Общие lib/check/Clippy/rustdoc gates
выполняются; предыдущие kernel gates их не заменяют. L11 **0/5**.

L9m2 window family **34/34 PASS**, 0.31s (`l9m2-split-window-focused.log`),
full lib/datastore **1091 passed/10 ignored/0 failed/0 filtered PASS**, 81.93s,
с test debug=0/j1 (`l9m2-split-lib-no-symbols.log`). Default check PASS10.02s,
scoped fmt12 files PASS, rustdoc LIVE **16/16 PASS**, 4.90s (15 ordinary+1
merged); logs `l9m2-split-check.log`, `l9m2-split-fmt-final.log`,
`l9m2-split-rustdoc-no-symbols.log`. Initial Clippy delta **1 added warning**,
tabs_in_doc_comments в with_split example (6954 errors/545 warnings),
`l9m2-split-clippy-delta.json`. Только snippet indentation исправлен на spaces;
runtime/bytes не менялись. Repaired lint delta и Graphify pending. L11 **0/5**.

L9m2 repaired Clippy delta **0 added** (6954 errors/544 warnings existing,
global exit101), `l9m2-split-clippy-repaired-delta.json`. После doc-only fix
final default check PASS5.70s, rustdoc LIVE16/16 PASS4.69s и scoped snippet fmt
PASS (`l9m2-split-check-final.log`, `l9m2-split-rustdoc-final.log`,
`l9m2-split-doc-fmt.log`). Full1091 и focused34 выполнены на том же runtime;
snippet spaces не меняют production behavior/bytes. Graphify PASS, AST19/19,
57978 nodes/156227 edges/1934 communities (`l9m2-split-graphify.log`); zero-node
context7.json/config.json limitation остаётся. Следующий refresh включит финальную
snippet indentation. SPLIT runtime/recipes/history/backend correctness gates
закрыты; JOIN/index hints/L10/L11 и общие tooling/performance gates OPEN.
L11 **0/5**.

L9m3a mechanical admission extraction завершён без изменения charge order/body:
общие schema_copy/source_copy используются primary path. Existing focused
**34/34 PASS**, 0.24s (`l9m3a-refactor-focused.log`), default check PASS5.67s
(`l9m3a-refactor-check.log`). Затем joined sources получили checked container
admission, schema/alias/target/scope/wildcard и predicate traversal до optimizer
clone. Direct regression отдельно раздувает только joined ownership (256 KiB)
при 64 KiB budget, без изменения input/result schemas.

L9m3a admission test не запустился при двух compiler resource failures:
allocation14 MiB failed (`l9m3a-admission-focused.log`) и allocation28 MiB failed
при command-local incremental=false (`l9m3a-admission-focused-no-incremental.log`).
Default test debug=0/j1 ранее проходил; это не успешный test и не доказательство
code regression. Win32 probe после первой ошибки: available physical2.56 GB,
available commit2.96 GB, C free196 MiB (`l9m3a-memory-after-oom.json`).
Пользователь освободил C и разрешил повтор; original debug=0/j1 focused retry
выполняется (`l9m3a-admission-focused-disk-retry.log`). System settings/processes
не менялись; новые compiler profiles не сохраняются. L11 **0/5**.

L9m3a разрешённый пользователем retry также завершился до test execution:
allocation4 MiB failed (`l9m3a-admission-focused-disk-retry.log`), C free около
230 MiB после попытки. Default `cargo check --lib --no-default-features`
**PASS**, 5.54s (`l9m3a-admission-check.log`); scoped nightly fmt2 files PASS
(`l9m3a-admission-fmt.log`). Clippy delta **0 added**: existing6954 errors/544
warnings, global command exit101 (`l9m3a-admission-clippy-delta.json`).
Пользователь 2026-10-07 попросил закончить check/Clippy и прерваться перед
source review до освобождения места. Запущенный read-only admission reviewer
прерван без verdict; fresh source review и runtime regression/full gate
требуются после resume. JOIN admission slice остаётся OPEN; дальнейшая
реализация JOIN kernel/recipes не начата. M2 SPLIT gates остаются полученным
evidence, Graphify refresh новых M3 edits deferred по explicit pause.
L11 **0/5**, все три compiler failures не являются performance attempts.

L9m3a resume 2026-10-07: source review **PASS**, без новых actionable findings;
schema/source extraction сохраняет прежний accounting, JOIN loop посещает все
owned source components/predicate до optimizer clone. Нового роста callback
types не найдено; причина compiler OOM по source review не устанавливается.
Inherited Graphs vector/Closure/Traversal helper coverage не включена в verdict.
После освобождения C original debug=0/j1 regression **1/1 PASS**, 0.03s
(build1m08s), `l9m3a-admission-resumed-focused.log`; расширенный window gate
**35/35 PASS**, 0.27s (`l9m3a-admission-resumed-window.log`). Полный lib gate
выполняется. Предыдущие compiler failures сохранены, не объявляются исправленным
toolchain defect. Sandbox process creation helper_unknown_error сохраняется;
read-only inspection и Cargo запущены approved outside sandbox, без изменения
system settings. L11 **0/5**.

L9m3a resumed full lib/datastore **1092 passed/10 ignored/0 failed/0 filtered
PASS**, 81.53s (`l9m3a-admission-resumed-lib.log`), original debug=0/j1.
Scoped fmt2 files PASS, protected43 unchanged (`l9m3a-admission-resumed-fmt.log`,
`l9m3a-admission-preserved.json`). Runtime/source gate JOIN copy admission закрыт;
unchanged-source default check5.54s и Clippy added0 evidence сохранены. Graphify
refresh совмещается со следующим mechanical input extraction; stage28 OPEN.
L11 **0/5**.

L9m3b mechanical input extraction gate PASS: window35/35 (0.28s) и ordinary
SELECT36/2 ignored (0.80s), default check5.92s, scoped fmt1 file и Clippy added0
(6954/544 existing); logs `l9m3b-input-window.log`, `l9m3b-input-select.log`,
`l9m3b-input-check.log`, `l9m3b-input-fmt.log`, `l9m3b-input-clippy-delta.json`.
Bounded diff `l9m3b-input.diff` сохраняет source/JOIN/outer body; prepared search
возвращается вместе с rows и живёт до прежнего finish call. Public/durable
contracts не менялись. Graphify PASS:57983 nodes/156252 edges/1908 communities
(`l9m3ab-graphify.log`); zero-node context7.json/config.json limitation сохраняется.
Полный1092 gate относится к pre-extraction admission state; stage28 OPEN.
L11 **0/5**.

L9m3c root JOIN context/type check PASS7.17s (`l9m3c-identity-check-initial.log`).
Initial fixture compile E0599 Array.to_vec исправлен на iter/cloned collect
(`l9m3c-identity-focused.log`). First runtime **2 PASS/1 FAIL**: qualified
overlay query возвращал4 вместо2 (`l9m3c-identity-focused-repaired.log`). Narrow
diagnostic установил тот же неверный результат ordinary SELECT, включая rows
с left_key2/right_key1 (`l9m3c-scope-diagnostic.log`). Это не оправдывает gate:
общий optimizer collect_columns обходил только Column/Binary, теряя Field
sources и оставляя INNER predicate неназначенным. Shared typed visitor заменил
неполный обход; unknown nested captures консервативны, constant/unassigned INNER
predicates назначаются на финальном all-inner JOIN, outer/CROSS barriers
сохраняют syntactic predicate step.

Finding ledger L9m3c: **J1 accept** — повторная primary scope evaluation в
root union/zero window. JOIN identity path пропускает legacy first-scope resolve,
zero window валидирует frozen copied input. **J2 accept/P1** — JOIN product
clone до RootFields work/scratch, включая predicate-free CROSS; общий combine_rows
теперь проверяет control/layout/work/deep copy admission до clone во всех7 callers.
Direct regression с256 KiB payload/64 KiB budget и нулевым pair-work не доходит
до output suffix. **J3 accept** — описанный inherited INNER ON classifier bug;
исправлен в общем planner, не локально в LIVE. Fresh review/expanded gates pending.
Initial J2 fixture E0603 private combine_rows исправлен на pub(super) internal
seam (`l9m3c-pair-on-focused.log`); public API не расширена.
Repaired focused **4/4 PASS**,0.06s (`l9m3c-pair-on-focused-repaired.log`):
qualified overlay rows2 и ordinary equality проходят после classifier fix.
Constant/CASE/outer barriers, reversed physical order и malformed suffix tests
добавлены; их gate выполняется. Stage28 OPEN, L11 **0/5**.

L9m3c expanded focused **6/6 PASS**,0.07s (`l9m3c-expanded-focused.log`);
scope-once read-count regression **7/7 PASS**,0.07s
(`l9m3c-scope-once-focused.log`); window **41/41 PASS**,0.38s
(`l9m3c-window-focused.log`). Before-J4 full lib/datastore **1098 passed/10
ignored/0 failed/0 filtered PASS**,72.75s (`l9m3c-lib-no-symbols.log`).
Fresh re-review: **J4 accept/P1** — global fixed_order при любом outer/CROSS
терял predicate первого source в reordered INNER prefix. Gate остаётся OPEN,
несмотря на preceding full PASS. Shared reorderable_sources теперь задаёт
одинаковую boundary для enumeration/scheduling: prefix использует dependency
readiness и final-prefix fallback, suffix сохраняет syntactic step. Existing
prefix CBO сохранён. Natural planner regression выбирает nonzero first source
для3 INNER sources перед LEFT/CROSS и проверяет exact2rows в root/ordinary.
Post-J4 focused **8/8 PASS**,0.09s (`l9m3c-j4-focused.log`), final full/check/
Clippy и fresh scheduler re-review выполняются. Stage28 OPEN; L11 **0/5**.

L9m3c J4 closure: focused **8/8 PASS**,0.09s (`l9m3c-j4-focused.log`),
post-fix full lib/datastore **1099 passed/10 ignored/0 failed/0 filtered PASS**,
79.90s (`l9m3c-j4-lib-no-symbols.log`) с debug=0/j1. Fresh scheduler review
**PASS**: shared prefix boundary соответствует DP/greedy, suffix не переставляется,
pending prefix predicates назначаются на final prefix step; J4 regression
проверяет actual nonzero-first DP order. Greedy проверен source inspection,
existing full suite сохраняет его regression coverage.
Final default check **PASS**,7.30s (`l9m3c-final-check.log`), scoped nightly
fmt6 files PASS (`l9m3c-final-fmt.log`), protected43 unchanged
(`l9m3c-preserved.json`). Clippy delta **0 added**, current6952 errors/544
warnings (2 baseline errors исчезли, global exit101), `l9m3c-clippy-delta.json`.
J1/J2/J3/J4 source/runtime closure evidence получено; root JOIN kernel готов
к public recipe slice. Public JOIN SDK/GQL/descriptor/lifecycle ещё OPEN;
stage28 не закрыт. Graphify refresh выполняется. L11 **0/5**.

Public JOIN scout установил дополнительный Empty-axis prerequisite: analyzer
создаёт5 universal atom columns, тогда как context принимал Empty только при
нуле columns. Guard расширен на universal Record-id schema; LEFT сохраняет
Unmatched, CROSS empty, scope validation не пропускается. Focused **1/1 PASS**,
0.01s (`l9m3c-empty-axis-focused.log`), default check PASS6.13s
(`l9m3c-empty-axis-check.log`). Full lib/datastore **1100 passed/10 ignored/
0 failed/0 filtered PASS**,75.06s (`l9m3c-empty-axis-lib-no-symbols.log`),
test debug=0/j1. Clippy delta **0 added**, current6952 errors/544 warnings,
global exit101 (`l9m3c-empty-axis-clippy-delta.json`). Empty-axis correctness
gate закрыт; final scoped fmt/preservation и Graphify refresh включены в следующий
mechanical source-codec gate.
Пользователь выбрал per-right durable GraphId/name anchoring до смены String
value (2026-10-07); primary capture policy не переносится в RHS slot. Полиморфный
right LEFT требует одной exact union relation перед JOIN; cartesian class choices
не могут выдать лишний Unmatched. Right exact RecordId restriction source-local,
не global WHERE. Source-aware aliases/captures/DAG/codec/public API — следующий
срез, пока OPEN. L11 **0/5**.

L9m4a common source codec extraction gate **PASS**: existing descriptor tests
**20/20**,0.03s (`l9m4a-codec-focused.log`), default check5.82s
(`l9m4a-check.log`), scoped nightly fmt3 files PASS (`l9m4a-fmt.log`),
protected43 unchanged (`l9m4a-preserved.json`). Mechanical diff сохраняет tags0..7,
dictionary order, record admission, query Work/depth и canonical bytes; helper
не вводит собственный Work charge. Clippy delta0, global6952 errors/544 warnings,
exit101 (`l9m4a-clippy-delta.json`). Новый full gate после mechanical extraction
не требовался: full1100 относится к предыдущему Empty-axis state; codec20
проверяет extraction напрямую. Public JOIN OPEN; L11 **0/5**.

L9m4b common source conversion gate **PASS**: SDK lowering31/31,0.20s
(`l9m4b-lowering-focused.log`), GQL recipe3/3,0.06s
(`l9m4b-recipe-focused.log`), default check5.13s (`l9m4b-check.log`),
scoped fmt2 PASS и Clippy added0/global6952 errors/544 warnings/exit101
(`l9m4b-clippy-delta.json`). Bounded diff: class choices/Empty axis/namespace,
record-owner capture, static scope refresh, borrowed lifetimes и прежние admission
сохранены. Initial direct Python write получил transient Errno22 до записи;
patch применён штатным apply_patch, успешный check относится к нему. Public
JOIN SDK/GQL ещё OPEN; L11 **0/5**.

L9m4c frozen source owner: scalar private capture заменён declaration-owned
vector; single-source registration допускает только ordinal0, остальные и
duplicates отклоняются до evaluation. Analyzer использует exact reserve после
byte/work admission; deterministic visitor проходит весь vector. Direct two-
source conversion fixture проверяет разные lexical slots и отсутствие root
captures у nested scopes. Initial compile E0283 generic Object fixture
(`l9m4c-frozen-focused.log`) исправлен explicit Value::from. Repaired focused
**4/4 PASS**,0.05s (`l9m4c-frozen-focused-repaired.log`); full lib/datastore
**1101 passed/10 ignored/0 failed/0 filtered PASS**,65.73s
(`l9m4c-lib-no-symbols.log`), test debug=0/j1. Final check5.82s PASS
(`l9m4c-final-check.log`), scoped nightly fmt8 PASS
(`l9m4c-final-fmt.log`), protected43 unchanged (`l9m4c-preserved.json`).
Clippy added0/global6952 errors/544 warnings/exit101
(`l9m4c-clippy-delta.json`). Bounded source inspection: every private capture
consumer migrated, existing snapshot/SSI scope resolution сохраняется.
Это registration-only prerequisite; durable RHS computed anchors и public JOIN
ещё OPEN. L11 **0/5**.

L9m5a source alias implementation: owned with_alias/alias, сохранение GQL
qualifier в Field path, independent synthetic lexical capture owner. FGLD bit4
включает nonempty/NUL-free UTF-8 alias после optional SPLIT, bits5..7 reserved;
default frame67/catalog227 unchanged. SDK two-source nested alias shadowing
с отдельными11/22 captures и точными numbers >2^53 **PASS**, codec alias golden/
malformed **PASS2/2**,0.02s (`l9m5a-alias-focused.log`). Existing GQL alias/property
precedence + recipe metadata **PASS1/1**,0.02s
(`l9m5a-gql-alias-focused.log`); descriptor suite **21/21**,0.04s
(`l9m5a-codec-focused.log`). Full lib/datastore **1103 passed/10 ignored/
0 failed/0 filtered PASS**,67.23s (`l9m5a-lib-no-symbols.log`), debug=0/j1.
Первый scoped formatter получил transient Windows mapped-section error1224
для query.rs; retry семи files PASS (`l9m5a-format.log`), checkinitial PASS7.85s.
Backend replay **Sync1/1 PASS**,0.29s (`l9m5a-alias-local-sync.log`),
**NativeAsync1/1 PASS**,0.30s (`l9m5a-alias-local-native.log`): retained source
alias/qualified SPLIT, effective overlays, saved-result replay после empty current
domain, durable ACK и tail. Rustdoc/final lint gates pending. Public JOIN всё ещё OPEN;
source alias support не означает readiness JOIN. L11 **0/5**.

Graphify L9m4abc PASS: AST15/15,58014 nodes/156345 edges/1916 communities
(`l9m4abc-graphify.log`); config.json/context7.json zero-node limitation остаётся.
Alias slice требует нового final refresh после source edits.

L9m5a finding **A1 accept**: old row-only historical proof не учитывал preserved
source alias, поэтому SINCE interval с updates существовавшей до capture записи
получал GAP вместо exact batch. Initial fixture E0433 missing Error import
(`l9m5a-alias-backfill-before.log`) исправлен; failing-before runtime **0/1 FAIL**,
0.02s (`l9m5a-alias-backfill-before-repaired.log`) подтвердил lost coverage.
Сначала allocation-free qualifier rule выделен из lowering; mechanical focused
**1/1 PASS**,0.02s (`l9m5a-qualifier-extraction-focused.log`). Затем row proof
использует тот же helper с retained alias, bounded schema work и recursive
alias context. Foreign record navigation остаётся ineligible; explicit alias
и class-name property precedence проверяются отдельной matrix. Post-fix
backfill **25/25 PASS**,2.42s (`l9m5a-alias-backfill-after.log`), включая local
WAL/barrier/reopen tests. Final full lib/datastore **1104 passed/10 ignored/
0 failed/0 filtered PASS**,68.96s (`l9m5a-a1-lib-no-symbols.log`), debug=0/j1.
Default check **PASS6.63s** (`l9m5a-final-check.log`), scoped nightly fmt10
PASS (`l9m5a-final-fmt.log`), protected43 unchanged (`l9m5a-preserved.json`).
A1 correctness gate закрыт. Initial Clippy delta2: redundant_pub_crate у helper
в crate-private module и tab, внесённый formatter в прежний SPLIT rustdoc.
Helper visibility остаётся ограниченной parent module; example переписан двумя
короткими statements, без lint suppressions. Repaired Clippy **added0**, global
**6952 errors/544 warnings/exit101** (`l9m5a-clippy-repaired-delta.json`).
Rustdoc LIVE **17/17 PASS** (16 merged +1 standalone),4.24s runtime
(`l9m5a-rustdoc.log`). Repaired scoped nightly fmt10 PASS
(`l9m5a-repaired-fmt.log`). Final default check5.29s PASS
(`l9m5a-repaired-check.log`), protected43 unchanged
(`l9m5a-final-preserved.json`). Graphify **PASS**: AST19/19,
**58023 nodes/156377 edges/1906 communities** (`l9m5a-graphify.log`);
zero-node config.json/context7.json limitation сохраняется. L9m5a correctness/
resource/API gate закрыт; global Clippy debt и Stage28 closure остаются OPEN.
Public JOIN recipes/right union relation/durable RHS computed anchors —
следующий срез. Commit не выполнялся, just all-check перед commit ещё обязателен.
Stage28 OPEN,
L11 **0/5**.

L9m6a JOIN runtime/codec gate: checked LiveJoin/LiveJoinKind, ordered with_joins/
joins и borrowed all-source walkers. FGLD bit5 сохраняет declaration order;
kind/reserved/alias/source/ON shape/count/depth/work/scratch проверяются до
acceptance. Default67 bytes сохраняются. Initial fixture E0283 ambiguous
Text vector (`l9m6a-codec-focused.log`) исправлен explicit Text; focused1/1
PASS (`l9m6a-codec-focused-repaired.log`), descriptor suite23/23 PASS,0.05s
(`l9m6a-descriptor-suite.log`), check21.15s/7.30s PASS. Read-only byte/API review
PASS без findings. SDK execution пока явно отклоняет JOIN до relation gate,
не игнорирует stored sources/ON. Historical proofs консервативны; saved-result
replay architecture не меняется. Clippy/rustdoc pending; Stage28 OPEN,L11 **0/5**.

Closure audit (2026-10-08), findings accept: L28-PRESENTATION — DIFF/PATCH пока
не меняют production delivery; L28-PLAN — generic Write EXPLAIN/hash не кодирует
LIVE recipe/policy; L28-OBSERVE — нет LIVE counters; L28-INFO — отсутствуют
declared bounds и dependency/function metadata. L2–L8 повторно не реализуются:
registry/savepoints/DAG/ACK/queues/import/replay/TTL уже имеют code/tests.
Завершение включает эти concrete findings, JOIN scopes/relations, явные
publication race/order tests и финальную backend/performance matrix.

L9m6a repaired Clippy added0, global6952 errors/544 warnings/exit101
(`l9m6a-clippy-repaired-delta.json`); wildcard production import исправлен.
Rustdoc LIVE18/18 PASS (17 merged+1 standalone),4.36s
(`l9m6a-rustdoc.log`). Constructor/codec readiness закрыта, execution gate дальше.
L9m6b mechanical source factory PASS32/32,0.21s
(`l9m6b-factory-focused.log`), check6.51s (`l9m6b-check.log`), scoped fmt
и bounded diff: graph/literal/alias evaluation order сохраняется.
L9m6c BoundRecord metadata содержит только class/parameter Identifiers; normal
Analyzer создаёт ScanTarget::Record из immutable supplied literal, проверяя
class и mutability. Удалены global WHERE restriction и root-only rewrite.
Focused2/2 PASS,0.04s (`l9m6c-point-focused.log`): Int выше2^53/String/UUID/ULID/
Array/Object, scoped overlays, nested/empty count, отсутствие reads unrelated
atoms, missing/wrong/mutable slots. Check7.09s PASS; full/review/lint pending.
Stage28 OPEN,L11 **0/5**.

L9m6c closure: full1107/10 ignored/0 filtered PASS83.19s
(`l9m6c-lib-no-symbols.log`), read-only point-source review PASS без findings,
Clippy added0/global6952 errors/544 warnings/exit101 (`l9m6c-clippy-delta.json`),
scoped fmt13 PASS (`l9m6c-fmt.log`). Source-local target gate закрыт.
L9m7a initial class/record JOIN vertical slice: обычный analyzer сохраняет public
qualifiers, ON prefix и ambiguity; captures удерживают unique synthetic owner.
Rich-type/schema/AST admission распространён на joined inputs. Frozen scopes
reify по declaration ordinal, record copy допускается до clone. Initial fixture
E0433 missing operator import (`l9m7a-join-focused.log`) исправлен; focused1/1
PASS0.03s (`l9m7a-join-focused-repaired.log`), initial full1108/10 ignored/
0 filtered PASS83.38s (`l9m7a-lib-no-symbols-initial.log`), check6.13s PASS.
Computed RHS/multi-class RHS явно guarded до anchors/exact relation; review,
expanded lifecycle/scoped/group/window/nested gates pending. L11 **0/5**.

L9m7a review ledger: **J7-1 accept/P1** — nested GraphExpr union wrapper
использовал public JOIN alias как capture owner, повторный `l` смешивал scope
parameters. **J7-2 accept/P1** — public alias не оплачивал per-column/origin/plan
copies. Initial fixture delimiter error (`l9m7a-review-before.log`) исправлен;
runtime J7-1 reproduced1FAIL/3PASS (`l9m7a-review-before-repaired.log`) с [1,1]
вместо[1,2]. Resource fixture уменьшена64→16 fields, чтобы baseline schema charge
не маскировал alias defect; J7-2 reproducedFAIL (`l9m7a-alias-before.log`).
Wrapper теперь использует unique captured next_scope owner; shared checked
alias_len×(fields+5)×16 admission до copies для primary/RHS и каждого class plan.
Post-fix focused **4/4 PASS**,0.04s (`l9m7a-review-after.log`). Initial full1108
не закрывает findings; expanded/fresh review/full gate впереди. L11 **0/5**.

## L9m8c–L9m8d: один polymorphic JOIN relation

Фабрика SELECT выделена механически (focused 38/38, check PASS), затем temporary
polymorphic RHS guard снят. Bounded Cartesian typing states анализируются обычным
analyzer; canonical raw AtomUnion сканируется один раз перед JOIN. LEFT имеет
отдельный NONE state. Lazy CASE выбирает исходные class-specific выражения;
GROUP/window исполняются один раз. Wildcard использует общий прежний projector.
Внутренние tags исключены из пользовательских names/fields и result identities.
Root scopes используют независимые durable anchors; nested family сохраняет
lexical identity и независимость frozen captures.

Read-only review: исправлены row/aggregate dispatch, прямой grouped ORDER alias,
lexical metadata, отдельные label capacities и pre-admission references/types.
Compound projected alias `ORDER BY n+1` отклоняется обычным SELECT analyzer;
расширение merger удалено, rejection проверен парным oracle. Fresh review PASS.

Evidence: polymorphic focused **2/2** (0.05s); малые независимые oracles проверяют
LEFT match/miss/WHERE, два LEFT, разные widths, wildcard, SUM, nested global union,
ORDER/window, PURE overloads/NONE fallback и resource rejection без publication.
Полный lib gate **1118 PASS/10 ignored/0 filtered** (86.95s). Production check PASS
(10.40s), scoped nightly fmt PASS, защищённые dirty paths **43/43 unchanged**.
Три новых Clippy diagnostics исправлены; repaired delta **0**, global baseline
**6951 errors/544 warnings** остаётся OPEN. Scratch comparator исправлен: stderr
строки больше не повторяют прежнюю JSON diagnostic (ложная дополнительная DTO
diagnostic). Измерения L11 ещё не запускались: **0/5**; Stage28 OPEN.

Backend evidence L9m8d: existing JOIN fixture расширен GraphExpr RHS с разными
class widths и отдельным point RHS. Local Sync **1/1** (0.54s), отдельный
NativeAsync **1/1** (0.62s). Оба проверяют durable descriptors/results, binding-only
смену первого scope, reopen/checkpoint/schema generation, replay+ACK+tail и fault
при recreation неизменённого второго scope. Срез M8d закрыт; Stage28 OPEN.

## L9m9 — native DIFF/PATCH и точный JSON export

Публичные LiveEventOutput/LiveFieldChange/LivePatchOperation сохраняют native
значения и presence независимо от authoritative before/after. Object leaf changes
детерминированы; arrays replace целиком. PATCH хранит add/remove/replace и escaped
JSON Pointer, включая empty root. Явный JSON export проверяет compatibility,
finite numbers и budgets; integers >2^53 и decimals не приводятся к f64.

Publication, historical registration и saved-result replay используют один
bounded renderer до admission/issue. Physical history сохраняет только raw images;
replay не читает current primary/FETCH. Prepared mode повторяется до ACK.
Retained cache reserve исключает unchanged inputs и temporary scratch, входит в
queue и общий candidate budget. Common B-tree reserve механически вынесен в memory,
с internal storage re-export и прежним поведением. Новых dependencies нет.

Review findings по retained accounting, aggregate candidate admission и обоим
backfill producers приняты и исправлены; fresh bounded consumer review PASS.
Source DML при active presentation resource fault коммитится, LIVE становится Paused.
Неподготовимый historical PATCH отклоняет registration до publication.

Evidence в target/stage28-baseline/:

- Native/JSON/representation/budgets: **6/6 PASS**, l9m9-retained-focused.log.
- SDK/GQL fields, VALUE, shorthand/ONLY и saved-result replay: PASS;
  resource fault и historical admission по **1/1**.
- Полный lib: **1128 PASS/0 failed/10 ignored/0 filtered**, 63.84s,
  l9m9-lib-final.log. Предыдущие failures: obsolete ONLY parser guard, затем
  expanded fixture повторяла прежнее значение; оба исправлены и повторены.
- Local Sync PATCH **1/1**, NativeAsync PATCH **1/1** (0.21s), checkpoint/reopen.
- Production check PASS (8.61s); scoped nightly format **21 files PASS**.
- Clippy added diagnostics **0**; global pre-existing **6951 errors/544 warnings**
  остаётся FAIL. Initial new rustdoc-tab/private-module diagnostics исправлены.
- Protected dirty files **43/43 unchanged**, l9m9-preserved.json.

- SDK rustdoc **19/19 PASS** (18 merged +1 standalone), 5.30s,
  l9m9-rustdoc.log; Event::output/JSON example компилируется.

Graphify AST-only update PASS: **58192 nodes/156920 edges/1910 communities**,
l9m9-graphify-update.log. Два existing JSON zero-node warnings не относятся к Rust;
paid relabel не запускался. L10/L11 и общие closure gates
остаются OPEN; performance attempts **0/5**.

## L10a — owned INFO metadata и statement snapshot

LiveInfo открывает snapshot/start/publication clocks, last-success capture source,
frozen primary/record namespace owners и transitive PURE versions. LiveHistoryInfo
удерживает authoritative last recorded group, purge floor/time и ordered capture
epochs с source identity/config revision/fixed retention. Absent history — None;
first available group не выводится из sparse watermarks. Physical refs не открыты.

SDK/GQL используют один bounded field projection. GQL identity operands и metadata
читают supplied QuerySnapshot через existing RootRead; ordinary SDK сохраняет
prepare_read. INFO наблюдает LiveDefinition/direct bindings/LiveCatalogRange.
History epoch/stamp changes публикуют последний predicate вместе с candidate root;
purge использует тот же key. История не превращает каждый source DML в отдельный
definition FCW write.

Finding ledger: **L10-M1 / Medium / accept → fixed**. Fallible Result<Vec<_>>
терял exact size hint и мог удерживать 8192 slots вместо admitted 4097. Conversion
использует try_reserve_exact и checked pushes. Valid alternating 4097 epochs,
intermediate cap и slots-1 rejection проверены; fresh re-review **PASS**.
Initial vector fixture предполагала больший размер typed IDs; corrected 8192
function entries проверяют реальный byte cap. New INFO mutable-borrow Clippy
diagnostics исправлены narrowing до shared transaction borrow.

Evidence в target/stage28-baseline/:

- SDK vectors/epoch capacity, fixed ReadCommitted statement snapshot и SSI:
  **2/2 PASS**, l10a-info-repaired.log.
- Capture enable/drop/re-enable и old snapshots: **1/1 PASS**, l10a-epochs.log.
- RAM/local partial/full purge/pins/reopen: **3/3 PASS**, l10a-retention.log.
- GQL root-bound reactive UUID identity и small metadata cap: **2/2 PASS**.
- Full lib **1130 PASS/0 failures/10 ignored/0 filtered**, 77.13s, l10a-lib.log.
- Production check PASS (19.26s); scoped nightly format **11 files PASS**.
- SDK rustdoc **20/20 PASS** (19 merged +1 standalone), 4.24s; owned metadata
  остаются доступны после Drop datastore.
- Repaired Clippy added diagnostics **0**; global baseline **6951 errors/544 warnings**.

L10 counters/EXPLAIN и L11 closure остаются OPEN; performance attempts **0/5**.

## L10b — process counters и consumer gauges

Datastore::live_statistics возвращает owned payload-free LiveStatistics. Один
constant-size StoreInner owner переиспользует existing saturating counter helper;
queue issuance/gauges принадлежат existing mutex state. Перечисление не создаёт
per-LiveId metrics, snapshots в ожидании, WAL/worker/dispatcher или dependencies.
Attempts известны до outcome; work/reserve — admitted quantities, не RSS и не
неизвестный partial work failed evaluator. Existing budget scopes выделены перед
instrumentation; mechanical query/binding gates **16/16** и **11/11**.

Scalar publication summary собирается до очистки local RAM history; counters
меняются только после successful barrier. ACK сравнивает old/new same-version
watermarks, без registration/repeats. First next/replay/tail issue считается один
раз; fallback first issue также invalidates stale reservation revision. Independent
observed-reader scope переиспользует cache/WAL/page-attempt accounting, включая
errors. Gauges отличают data events от prepaid controls/head/ring/consumer bytes.

Finding **L10-C1 / Medium / accept → fixed**: current-stamp-only accounting
пропускал new SINCE backfill. Durable ranges теперь bounded by previous last_stamp
и purge floor; old handles не пересчитываются на ACK/maintenance. Fresh re-review
PASS; два исторических groups без current delta и repeated replay проверены.
Import OFF отдаёт live net Batch, но сохраняет отдельный durable suppression control.

Evidence в target/stage28-baseline/:

- Public SDK statistics/first issue/ACK/disabled LIVE/backfill/ON-OFF: **3/3 PASS**.
- Fallback stale reservation/repeated token: **1/1 PASS**; first fixture erroneously
  повторяла commit stamp, corrected successor проверяет increasing order.
- Full lib **1134 PASS/0 failures/10 ignored/0 filtered**, 68.41s, l10b-lib.log.
  Включает failed/uncertain WAL/ACK без successful metric advances и reset-on-open.
- Local Sync **1/1 PASS** (0.17s), NativeAsync **1/1 PASS** (0.20s): real page
  requests, admitted replay work, repeated issuance, checkpoint/reopen и native PATCH.
- Production check PASS (19.29s). Windows os1224 formatter failures разрешены exact
  nightly-output patch с успешным read-only check; gate не обходился.

- Scoped nightly format **20 files PASS**; repaired Clippy added diagnostics **0**
  (global baseline6951/544). One bool::then lint исправлен без поведения.
- SDK rustdoc **21/21 PASS** (20 merged +1 standalone), 4.33s.

Graphify L10b PASS: 58255 nodes / 157086 edges / 1881 communities. L10c EXPLAIN/PlanHash
и L11 остаются OPEN; performance attempts **0/5**, измеренный overhead ещё не заявлен.

## L10c — LIVE EXPLAIN и named input PlanHash

Один command selector подключает Schema и LET/Live wrappers к PhysicalPlan::Live.
Регистрация/bindings, INFO, ACK и KILL показывают реальные validation/read/publication
phases, без второго executor. FULL/LOGICAL не вызывают user evaluation; ANALYZE
исполняет обычный statement один раз. INFO adapters используют QuerySnapshot;
данные primary/current results, diagnostic/queue/process counters и ACK/history
watermarks не входят в semantic fingerprint. Sources/scopes/output/SINCE/budgets/
function descriptors имеют самостоятельный PlanHash v10. Captures не печатаются.

Borrowed FGLD Query/Binding view сохраняет owned canonical framing. Existing value
preflight получает optional caller work/cancellation; names/payloads и RecordId
проверяются до copies. Temporary codec/metadata/hash account освобождается после
использования; retained plan/source/output reserve сохраняется до projection.
Новые storage pages/WAL/registry/dependencies/Expr walker не вводятся.

Review findings **L10-H1/H2 / accept → fixed**: deferred BUFFER/SINCE не учитывали
runtime option inputs; затем slot-only order терял lexical name/value correspondence.
Existing Analyzer parameter resolution записывает только реальные inputs в
предварительно admitted mask. Closure children наследуют mask, локальные shadow
ordinals исключены; named LogicalLiveCapture tuples сохраняют correspondence.
Registration-only reactive input включает identity и snapshot value; query body
captures по-прежнему отслеживают identity. Fresh bounded re-review **PASS**.

Evidence: focused EXPLAIN **3/3 PASS**, descriptors **24/24 PASS**. Matrix проверяет
repetition/output/value/buffer hash changes, unrelated inputs, swapped declarations,
equivalent reorder, closure free captures/shadowing, fixed INFO snapshot, LET wrapper,
no registration/evaluation в FULL и single ANALYZE. Existing standalone command-option
SELECT subquery остаётся typed rejection. Первые test runs выявили parser whitelist
и fixture/API/transaction boundary mistakes; исправлены до final evidence.
Initial Clippy delta42 (docs/visibility/style) исправлен без изменения поведения;
final added diagnostics **0**, global baseline **6951 errors/544 warnings**.
Full lib **1138 PASS/0 failures/10 ignored/0 filtered**, 84.79s (l10c-lib.log).
Production check PASS (9.76s); read-only nightly formatting **20 files PASS**.
Local Sync **18/18 PASS** (4.52s); NativeAsync feature target **36/36 PASS**
(18 NativeAsync +18 Sync, 9.77s). Public RAM **8/8 PASS** (0.23s), LIVE rustdoc **21/21 PASS** (3.68s).
Graphify AST-only **PASS**: 58344 nodes /157414 edges /1922 communities;
two existing zero-node JSON warnings сохраняются (context7.json/config.json).
L11 performance **0/5**; Stage28 остаётся **OPEN**.

## L11 — prepared maintained harness (ещё не gate attempt)

Добавлен Criterion target `gql_stage28`, без dependency changes. Каждая process
fixture измеряет самостоятельные subscriptions1/10/100, 64 hot results на реальном
class rescan, FETCH-only fanout, data/parameter LIVE LET chains, saved-image replay,
ACK и import ON/OFF. Setup, previous/after oracle и ACK исключены из commit timer;
ACK-only timing исключает source commit/first issue. Default limits не увеличиваются.
Expected resource rejection/fault — отдельный cell, без successful Criterion samples.

Bounded review R1–R5 accepted/fixed: causes сохраняют typed resource classification;
WAL/history files считаются в действительных dedicated subtrees; перед historical
resume исходный consumer detach; identity/images/version/stamps проверяются точно;
default-limit evaluation fault проверяется вместе с successful primary/binding publish.
Metric strings имеют single-line tabs. Fresh re-review **PASS**.

Public SDK compile check PASS. Cheap64-row smoke **11/11 RAM +11/11 local Sync PASS**,
включая cold-engine reopen, source/history physical byte observations и whole-commit
oracles. Первые compile/local setup ошибки API/private intern/name length исправлены.
Smoke timings не являются performance measurements. Resolved Criterion **0.8.2**
подтверждён в current lock/build и в34 embedded source paths preserved baseline binary.
Runner integrity review G1–G8 accepted/fixed: exclusive lease, strict max5/nonempty
ledger, atomic sibling replacement с previous backup, recorded failed-cell rejection,
in-flight PID/start/path validation и owned-child cleanup, attempt-specific Criterion
home, complete72-cell validation и separate common/capacity verdicts. Selfchecks
проверяют actual original save/classifier/cap code и failure injection: post-launch
ledger failure kills child, final-save failure releases lock. Real ledger ещё отсутствует.

Archived baseline compiler provenance подтверждён embedded rustc hash
48a229ceaefd4985c50990b14116b6d856af0985, совпадает с current rustc1.98.1/LLVM22.1.8.
First unchanged opt3/fatLTO/codegen1/debug2 release build: **LLVM OOM before benchmarks**;
peak observed rustc RSS около4.6GiB, Windows commit headroom около1GiB перед отказом.
Пользователь освободил C до4.85GiB; same-profile retry также **LLVM OOM**
до benchmarks, observed peak RSS около5.1GiB/commit headroom около0.75GiB.
Две build failures не считаются performance attempts. Готовится отдельная comparable
baseline/current конфигурация command-local debug0 с прежними opt3/fatLTO/codegen1;
она не объявляется исправлением default debug2 build. Baseline196d21b использует
managed isolated checkout; artifacts наD, current dirty tree сохраняется.
Build failures не являются performance attempts.

`just all-check` initial run остановился на13 missing LIVE/status methods в default
configuration до formatter. Narrow stale dev-cache diagnosis: definition touch дал
13→2, proc-macro touch не изменил stale DLL, clean только двух packages (macro/status)
и fresh default `cargo check` **PASS** (29.16s), source bytes не менялись. Before-command
backup631 Rust files; formatter changed/restored0. Whole recipe retry: check/fmt прошли, Clippy **6989 errors/537 warnings** в default
columnar/datafusion конфигурации; no-default slice delta0 остаётся самостоятельным
evidence. Formatter изменил5 files; все5 restored exact before bytes (четыре
несвязанных source и benchmark wrapping). Source semantics не менялись.

L11 paired gate ещё не запускался: **0/5**. Сборка release/harness tooling и controlled
runner завершаются перед первой нумерованной попыткой.

## L11 closure review — durable stop и snapshot revision docs

C28-1 /accept→fixed: transient terminal/fused consumer ошибочно делал close no-op,
оставляя durable query Active. Handle state разделён на fused polling и успешно
committed capture stop до исправления early return. Failed control commit не
устанавливает idempotent flag; weak owner/cursor/Drop остаются прежними.
Correct pressure fixture — два individually admissible1-entry commits в queue1:
original guard воспроизводит Active-vs-Closed assertion; corrected guard останавливает
capture, future DML не увеличивает evaluation attempts, repeat close после store drop
идемпотентен. Первый fixture с одним2-entry commit достиг oversized fault и был
исправлен; он не считается доказательством исходного DISCONNECT defect.

C28-2 /accept→fixed docs: INFO direct-binding revisions принадлежат текущему snapshot,
а не последней evaluation query. Closed query + binding replacement regression
подтверждает current revision; LiveBatch по-прежнему сохраняет used revisions.
Public Rust API types/disk layout не меняются. Public RAM **10/10 PASS** (0.24s);
Full lib **1138 PASS/10 ignored/0 filtered** (62.51s); fresh bounded re-review
**PASS**. No-default production check PASS, scoped format PASS, Clippy delta **0**
(global6951/544 baseline). NativeAsync feature backend target **36/36 PASS**
(18 NativeAsync +18 Sync,9.75s). Current optimized debug0 build выполняется
в отдельном `debug0-current` directory; source readonly до binary publication.
Baseline196d21b debug0 build **PASS**, optimized profile,9m56s; binary path и SHA
сохраняются перед current build. Performance attempts **0/5**.

## Лимит performance gate L11

L9m8b repeated full gate **1116 PASS/10 ignored/0 filtered** (66.93s).
Private union purpose заменён explicit enum IndependentOutputs/JoinedTypingVariants;
лишние bool warnings устранены, enum не экспортируется публичным syn root.
Production check PASS (20.33s); final Clippy delta **0** (6951 errors/544 warnings
global baseline; unrelated DTO diagnostic варьирует между incremental runs и
не исправлялся здесь). Final scoped nightly check **22 files PASS**.
Family coordinator ещё не подключён; public polymorphic guard сохраняется.
Следующий mechanical шаг выделяет existing SELECT statement factory до добавления
typing Cartesian alternatives. L11 **0/5**, Stage28 OPEN.

L9m8b full gate выявил resource regression: **1115 PASS/1 FAIL/10 ignored/0 filtered**,
`live_backfill_aggregate_dictionary_scratch_is_peak_not_cumulative` отклонял прежний
8 MiB budget. Narrow reproducer подтвердил одинаковый PURE scratch error.
Cause: full layout charge применялся и к identity mapping, который заново создавал
тот же Vec; conservative scalar charges удерживали лишний buffer admission.
Identity layout теперь сохраняет исходную allocation, после проверки bounds.
Изначальный oracle **1/1 PASS** (0.47s), бюджет не увеличен; resource fixture
дополнен pointer preservation для identity layout. Общий preflight остальных
layouts/graph/sequence сохранён. Full gate повторяется; L11 **0/5**.

L9m8b primitive raw AtomUnion: SDK-only source metadata хранит canonical classes
и matched/NONE typing state. Class-local property positions сохранены при общем
padded width/tag; NUL-prefixed internal columns не доступны name/qualifier lookup.
Scope resolves один раз; negative GraphClassRange reads сохранены. Planner суммирует
class cardinalities checked, не применяет class-specific probe и показывает
AtomUnionScan. PURE/fold/demand/source fingerprint consumers расширены.
Public polymorphic lowering guard пока сохраняется до typing-family collapse.

Review ledger **B1 accept/P1**: padding guard не учитывал full JOIN layout и
graph/sequence copies до allocation. Исправлен общий scoped/layout preflight для
всех source и index nested-loop callers; bounds checked, panic debug assertion
заменён typed error. AtomUnion исключён из неподдерживаемого predicate/limit
pushdown. Fresh findings review **PASS**. Independent raw layout/scope/cardinality
и malformed/resource/name/sequence regression gate **2/2 PASS** (0.02s).
Production check PASS (5.53s); full correctness/fmt/Clippy текущего primitive
выполняются. Это не closure public polymorphic JOIN. L11 **0/5**, Stage28 OPEN.

L9m7b final tooling: SDK rustdoc **18/18** (17 merged +1 standalone), 4.10s.
Scoped nightly format check **14 files PASS**; os1224 resolved via exact
formatter patch, source gate не обходился. Protected dirty paths **43/43**
unchanged (`l9m7b-preserved.json`). Global Clippy остаётся baseline FAIL с delta0.

L9m8a shared row-selector refactor: scalar class ordinal использует прежний CASE;
JOIN tuple хранит distinct NONE tags. Common aggregate merge сохраняет один
reduction и dispatch только row arguments. Independent selector/SUM domain check
**1/1**, existing lowering/constructor/union gate **38/38** (0.28s), production
check PASS (17.64s), scoped format PASS; read-only fresh review **PASS**.
Raw relation/family consumer ещё реализуется; это не closure public polymorphic
JOIN. L11 **0/5**, Stage28 OPEN.

L9m7a post-review closure: J7-1/J7-2 исправлены; fresh review PASS.
Expanded JOIN/alias/SPLIT/reactive-ON/frozen scopes gate **5/5**,
full lib **1111 PASS/10 ignored/0 filtered** (70.20s), production check и
scoped fmt PASS, Clippy delta **0** (global baseline 6952 errors/544 warnings).
Polymorphic RHS relation остаётся отдельным срезом; single-class LEFT не
подменяется глобальным WHERE point restriction.

L9m7b independent root RHS anchors: успешные String scopes удерживают отдельные
GraphId/name по declaration ordinals. Один changed operand не меняет другой;
remove/recreate unchanged name приостанавливает подписку, сохраняя прежние
anchors и committed source DML. Vector публикуется с exact result COW, old root
сохраняет прежние пары. Primary capture policy и nested ordinary resolution
не меняются. FGDC options bit0/tail документированы; default header160/entry227
не меняется. Graph clock, sorted owners, source kind, UTF-8/count/reserved/length
и admission проверяются до copies. **J7-3 accept/P2**: local catalog DTO conversion
не учитывал новые anchor bytes; added retained_bytes preflight. Review не нашёл
identity/publication/codec defects; enclosing-catalog/backend evidence добавлено.

Evidence: focused anchors+malformed tail **2/2**, catalog **22/22**, full lib
**1113 PASS/10 ignored/0 filtered** (72.00s), enclosing nonempty-tail/flags/
diagnostic round trip **1/1**; Sync **1/1** (0.51s), NativeAsync **1/1** (0.54s).
Backend fixture проходит checkpoint/reopen, schema generation replacement,
saved-result equality/ACK/tail и durable fault при recreation RHS name.
Production check PASS (6.18s); repaired Clippy delta **0** (6952/544 baseline).
Первый integration compile выявил только fixture GraphId/GraphRef и payload
PartialEq mistakes; исправлены. Formatter retry нужен из-за Windows os1224
mapped-file transient; gate закрывается только успешным scoped check.
L11 **0/5**, Stage28 OPEN; продолжение — exact polymorphic RHS relation,
DIFF/PATCH consumers, L10 metadata/plans/counters и closure matrix.

Историческое ограничение (2026-10-06), позже снято пользователем: не более пяти попыток. На момент этого среза счётчик **0/5**:
paired L11 gate ещё не запускался; исходный baseline ниже не является попыткой
прохождения L11. Каждый gate run получает номер и evidence/verdict. После пятой
неудачной попытки дальнейшие оптимизации прекращаются, результат фиксируется
здесь, остальная работа продолжается; performance gate остаётся OPEN.

## Measured baseline

Исходный Criterion binary из HEAD `196d21b` сохранён как
`target/stage28-baseline/gql_stage27_common-196d21b.exe`; SHA-256 binary и исходного
rlib в `benchmark-provenance.json`. Smoke run без `--bench` не считается замером.
Measured run: 10 samples, warm-up 0.5s, measurement 2s, baseline
`stage28-196d21b`; log `common-bench-measured.log`.

| Workload, 10K  | Estimate interval                   |
|----------------|-------------------------------------|
| return_literal | 3.2293–4.3509 µs, central 3.9472 µs |
| point_read16   | 3.6078–3.8919 µs, central 3.7182 µs |
| mutate64       | 9.3658–9.5477 ms, central 9.4709 ms |

У return_literal широкий интервал; до paired run текущего binary вывод о regression
не делается. L3–L11 и общие correctness/resource/performance/tooling gates OPEN.
