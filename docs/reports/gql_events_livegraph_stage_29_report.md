# Этап 29 — журнал реализации Events и LiveGraph

Дата начала: 2026-10-10. Baseline: `055af48`. Статус: **OPEN**.
Нормативный контракт: [план этапа 29](../gql_events_livegraph_stage_29_plan.md).

**Последнее состояние — 2026-10-13.** E0–E5 и G0–G3 реализованы, включая
`next_event()` / `live_graph_select!`, typed conflict branch, явное resolution,
source flush и resnapshot. Этап остаётся **OPEN** до performance/tooling closure.
Retained source — attempt2; использованы4/10 source optimization attempts.
Полные gates текущего source: no-default1334/default1342/NativeAsync1351
library tests и88 public backend tests,0 failures. Ordinary paired series
`verified30-remaining` завершилась FAIL/INCONCLUSIVE:17/36 individual comparisons
прошли upper95CI<=5%;10K `RETURN1` прошёл4/4 пары. Exploratory process-level
mean changes по point/DML/import:10K +2.89/+3.65/−0.39%,100K −1.62/−2.77/+5.06%,
1M +0.86/−2.13/+3.74%. Эти агрегаты не закрывают gate; uncertainty сохранена.
Native SW VTune capture завершён для query/point/import/DML с проверенными
055/source2 images:8 captures exit0,40 CSV reports сохранены. Python launcher
две попытки завершил до benchmark; uninstrumented smoke PASS, точная причина
instrumentation failure не установлена. Прямой EXE запуск с unprofiled monitor
сработал. QoS setter подтвердил состояния scoped descendants, однако PID в
VTune CSV совпал с monitor PID только в4/8 captures. Соответствие остальных PID
не доказано; одинаковый QoS actual sampled processes не заявляется. Profiles
используются только для диагностики, не timing regression gate.

Query: memcpy16.9% baseline/16.3% current sample CPU, основные callers — session
execution и parser. Point: compact_str comparison23.4%/23.8%, стеки ведут в
RecordId comparison и AtomStoreRoot::resolve. Import: comparison10.1%/9.0%,
callers insert_record/insert и private import. DML1M: allocator free-list extend
22.2%/22.4%, BTreeMap drop11.7%/12.2%, Arc clone10.3%/9.5%; allocator stacks
ведут в AtomStore clone как при change_base_property, так и при commit replay;
current также показывает VersionStore::insert. Это общие baseline/current
затраты. Sample proportions и profiler overhead не доказывают throughput gain.
Code-generation aliases также не доказывают запуск event handlers. Source
optimization attempt5 не начата. Запущен final default SDK selector gate;
затем остаются Clippy/protected all-check и predeclared confirmation matrix.

Final default SDK selector PASS:1 test, renamed crate и внешняя Tokio branch,
а также cancellation branch; build4m20s, execution0.01s. Final no-default/lib
Clippy выполнился:6875 общих errors,660 warnings, exit101; changed-line slice
179 owned files содержит0 diagnostics. Это общий Q-14 debt, не новый stage29
lint failure. Protected `just all-check` будет выполнен в отдельной copy с
fresh target directory; primary dirty files проверяются по SHA.

Protected `just all-check` source2-final выполнен6m34s: `cargo check` PASS5m02s,
nightly fmt выполнен в copy, затем Clippy остановил recipe с6913 errors/661
warnings общего debt (exit101). SHA807 primary files после запуска совпали;
основной checkout не изменён. Отдельный primary changed-line Clippy slice0
diagnostics и non-mutating nightly181files PASS сохраняются как stage evidence.
Artifact memory/disk failure не было. Начата зафиксированная fresh confirmation
matrix `g29-confirm8-v2`; пока выполняются same-EXE calibration batches.

**Итог fresh fixed8 matrix.** Все24 ordinary process pairs и8 same-EXE pairs
выполнены; inclusion/hash/phase-directory checks PASS, `g29-confirm8-result.json`
сохранён. Общий gate **FAIL/INCONCLUSIVE**: symmetric calibration не прошла
(mean+1.126%, верхняя граница `mean(C-1.05B)`+10.757ns, нижняя граница
`mean(C-.95B)`+119.278ns). Ordinary paired-t margin прошёл4/12 cells; средние
всех12 cells лежат ниже5%, но uncertainty оставшихся8 cells не исключает
регрессию>5%. Ни выбросы, ни неудачные individual comparisons не удалены.

| Rows | RETURN1 mean | Point16 mean | DML64 mean | Import1000 mean |
| --- | --- | --- | --- | --- |
| 10K | −0.208% (margin PASS) | +2.394% (inconclusive) | +2.623% (inconclusive) | +3.981% (inconclusive) |
| 100K | −2.297% (inconclusive) | −0.585% (margin PASS) | +0.779% (inconclusive) | −1.530% (margin PASS) |
| 1M | +1.021% (margin PASS) | +3.452% (inconclusive) | +1.252% (inconclusive) | +3.004% (inconclusive) |

Все source2 correctness/resource/SDK gates сохраняются. Source attempt5 не
начинается по этим noisy timing данным;4/10 использованы. Этап29 остаётся OPEN,
performance acceptance не ослабляется и не подменяется средними без uncertainty.

Graphify AST-only update завершён exit0:59779 nodes,161484 edges,1934 communities;
graph.json/graph.html/GRAPH_REPORT.md обновлены. Windows parallel extractor
сообщил BrokenProcessPool и успешно перешёл на sequential fallback; guard
`if __name__ == "__main__"` добавлен в local wrapper для следующих запусков.
Два configuration JSON дали0 nodes; LLM relabeling/semantic extraction не
выполнялись. Несвязанные dirty changes сохранены; source2 production files
во время заключительных проверок не изменялись. Artifact resource failure нет.

**Предварительно зафиксированный confirmation protocol.** После текущей серии
будет выполнено ровно8 новых process pairs для каждой из12 workload/cardinality
cells, двумя ABBA4 batches; отдельно8 same-EXE query pairs. Используются те же
проверенные055/source2 EXE,30s window, child HighQoS и logical6. Все пары
включаются; выбросы не удаляются, остановки после удачного prefix нет.
Criterion process mean сохраняет estimator `mean(times/iters)`, process runs
имеют одинаковый вес. Общая оценка — `mean(C)/mean(B)-1`; проверка5% margin:
`D_i=C_i-1.05*B_i`, upper=`mean(D)+t(.975,7)*sd(D)/sqrt(8)`, PASS при upper<=0.
Для закрытия нужна также симметричная same-EXE calibration: дополнительно
нижняя95% граница `mean(C-.95B)` должна быть>=0, чтобы проверить обе стороны
±5% margin и не пропустить directional bias одинакового EXE. Это применение
[парного анализа NIST](https://www.itl.nist.gov/div898/handbook/prc/section3/prc311.htm)
и [CI среднего](https://www.itl.nist.gov/div898/handbook/eda/section3/eda352.htm),
а не новый Criterion estimator. Предполагаются независимые приблизительно
нормальные process-pair differences;8 пар дают ограниченную проверку этих
предпосылок. ABBA контролирует порядок, но не доказывает независимость.
Прежние individual Criterion PASS/FAIL и failed calibration сохраняются;
агрегаты уже наблюдавшейся серии будут только exploratory. Protocol JSON и
assert-based arithmetic checks сохранены в `target/stage29`; новые confirmation
measurements пока не начинались. Production source не меняется.

Ниже сохранён исторический журнал; его старые pending-статусы относятся к дате
записи, а не к текущему состоянию.

**Коррекция baseline evidence — 2026-10-12.** VTune/source-checksum audit
подтвердил stale project cache: сохранённый baseline benchmark был заново
linked, но root `f519c0203cba6775` оставался `fresh=true` из Stage26.
Бинарный dep-info содержит344 `stage26-baseline/src` paths и ни одного Stage29
baseline path; EXE/PDB GUID `D5C0DBA8-395B-4DDF-B0AB-AEBEECC5A2E7`, age3
совпадает. В сохранённом PDB module18 object — эта root rlib, а checksum
`select.rs` — `6B4990CCA423BFBCF2CEE44036B2A7D22C08D927D0FDEA2AF964B6369462025D`:
он совпадает со Stage26 и отличается от archive055
`FE29516D7EE83733B147013A2B3039CD03D26D4AAE8AF3410FB6E43844B1E1C1`.
Это доказательство bytes/source mismatch, не вывод по старым PDB paths.

Все прежние ratios/gates к этому baseline, включая initial series и baseline
VTune comparison, **INVALID для regression относительно055af48**. Raw data
сохранены, исторические PASS/FAIL ниже не закрывают Stage29 gate. Прямые
current-before/current-after сравнения попыток остаются отдельным evidence:
они использовали свежие current root compilations. Сегодняшний standalone
baseline rlib layout probe отдельно помечается mutable-cache evidence.
Archive055 пересобирается в isolated `CARGO_TARGET_DIR`
`target/stage29/fresh-build-baseline-verified`; production source не меняется,
counter4/10. Перед новой paired series обязательны checksum source/PDB audit
и сохранение root rlib вместе с EXE/PDB. VTune указанного пользователем пути
остаётся инструментом последующего анализа.

**Fresh baseline provenance — 2026-10-12.** Isolated build PASS44m35s,
без memory/disk failure. Root compiled (`fresh=false`), features[], тот же
profiling profile. `verify_baseline_artifacts.py` подтвердил checksum archived055
в новом PDB, сохранил root `.rlib/.rmeta` и source audit JSON. Новый EXE/PDB
находятся в `performance-artifacts/baseline-verified/baseline`; все старые
baseline artifacts сохранены с прежней evidence-invalid пометкой.
Новая RETURN1/10K paired series с retained attempt02 начата; gate pending.

**Verified paired query — 2026-10-12.** Три пары дали −37.43/+53.87/+5.67%,
upper95CI −29.56/+71.34/+23.86%; gate FAIL/INCONCLUSIVE. Source provenance
теперь PASS, но runner явно нестабилен: Perfmon `PercentProcessorPerformance`
для выбранного logical6 показал19 при nominal `ProcessorFrequency`2000,
CPU time100%. Абсолютные mean текущего EXE выросли с прежних~3µs до24–35µs
без source изменения. Причина CPU режима ещё не установлена; никаких host
power/pagefile изменений не было. Новый source attempt5 не начинается до
диагностики.

**Текущие correctness и условия измерения — 2026-10-12.** После rollback
попытки4 полный no-default library gate retained source2 прошёл:
`cargo test --offline -j1 --lib --no-default-features -- --test-threads=1` —
1334 passed, 0 failed, 10 ignored, execution141.38s. Default-feature gate
запущен отдельно. Пользователь подтвердил подключение ноутбука к питанию;
работа от батареи не установлена как причина нестабильности.

VTune native software Hotspots успешно собрал current2; attribution memcpy
включает session execution, parser и evaluator. Старый baseline профиль
остаётся INVALID относительно055. Hardware Hotspots collection завершилась,
но wrapper timeout произошёл при symbol finalization; последующий report
завершил обработку сохранённого raw result. Фильтр по полному EXE name оказался
пустым из-за сокращённого process label; исправленный PID16616 summary дал
CPU0.333s,505M instructions, CPI0.452, elapsed8.752s и paused5.684s.
Такое покрытие недостаточно для вывода о причине CPU режима. Collector
включил child gnuplot вопреки no-follow-child и предупредил об этом; raw
системные/child metadata остаются локальными. Nominal platform frequency
не выдаётся за фактическую частоту ядра во время измерения.

Read-only Windows `GetProcessInformation(ProcessPowerThrottling)` self-check
helper прошёл: version1, ControlMask0, StateMask0, priority NORMAL.
Это состояние launcher, а не доказательство состояния каждого benchmark
thread или отсутствия system-managed QoS. Host power/pagefile settings
не менялись. Номер следующей source attempt остаётся5; использованы4/10.

Read-only `powercfg` показал Balanced, AC processor minimum20%, maximum100%;
CPU — Intel i5-12450H. Это параметры схемы, не доказательство ограничения
нагруженного ядра. Для отдельной калибровки подготовлен child-only HighQoS
через [документированный ProcessPowerThrottling API](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setprocessinformation):
ControlMask EXECUTION_SPEED, StateMask без этого бита. Self-check на собственном
короткоживущем child прошёл и подтвердил неизменность priority и parent state.
Следующая paired series применит одинаковый режим к baseline/current;
изменения production source, host scheme и чужих процессов не требуются.
Новая серия ещё не выполнена; причинность и performance PASS не заявляются.
Nightly non-mutating format gate текущих181 owned files прошёл.

**Полный default library gate — 2026-10-12.** Retained source2:
`cargo test --offline -j1 --lib -- --test-threads=1` —1342 passed,
0 failed,10 ignored, execution132.76s; build34m25s. Artifact memory/disk
failure не было. Дополнительные зависимости и production изменения не внесены.
Начата отдельная same-EXE HighQoS calibration,3 pairs; номер source attempt
остаётся5. Первая Perfmon snapshot на logical6 показала195% nominal performance
против ранее19%; первая absolute query estimate около5.57µs против24–35µs.
Это наблюдение поддерживает QoS гипотезу, но не доказывает стабильность всей
серии и не закрывает baseline regression gate. Raw process control/state
masks и неизменный NORMAL priority сохраняются в paired manifest.

HighQoS calibration завершилась **FAIL/INCONCLUSIVE**: same-EXE pairs
−5.10/−6.79/−9.45%, upper95CI+9.31/+3.55/+4.16%. Первая пара не прошла
строгий5% gate; две следующие прошли. Later Perfmon snapshot показала177%
nominal performance. Ускорение относительно system-managed режима заметно,
но шум остаётся и новая source optimization attempt не начинается.
Следующий correctness gate — no-default NativeAsync library и10 публичных
RAM/Windows backend targets; benchmark/profiler не выполняются одновременно.

**Калибровка измерительного harness.** В current и archived-baseline копиях
одинакового `gql_stage27_common` увеличено extended measurement window с10
до30 секунд; прежние тексты и SHA сохранены в `measurement30-before`.
Production source остаётся retained attempt2, использованы4/10 source attempts.

Read-only measurement review нашёл G29-R1: общая Criterion directory
перезаписывала `new/sample.json` и `change/estimates.json` следующих пар.
Исправлено отдельной `CRITERION_HOME` для каждой cardinality/repetition;
paired manifest сохраняет её путь. Старые серии остаются с ограничением:
means/CI и console logs сохранены, но raw second-run samples каждой старой
пары восстановить из общей directory нельзя. Новая ABBA серия ещё не запущена.

Review G29-R2 также принят: runner ранее позволял повторно передать stale
baseline tag и подписать его055. Теперь baseline mode требует preserved root
rlib с SHA проверенного055, проверяет фактические bytes и сохраняет provenance
в manifest. Negative preflight реального invalid profiling16-probe2 tag
отклонил его до запуска benchmark/создания output; positive preflight
baseline-measure30 прошёл. Current-control calibration остаётся отдельным режимом.
Следующие серии используют child HighQoS, `--noplot` и ABBA порядок по четырём
парам. Для current-first пары relative change приводится к current/baseline:
`1/(1+r)-1`, с перестановкой преобразованных границ CI. Algebra self-check
прошёл; standard error обратной оценки не выдаётся за пересчитанный — null,
raw Criterion estimate сохраняется. Gate upper95CI <=5% не ослабляется.

Owner этого decision unit — rust-performance; Cargo provenance и Windows QoS
владеют только своими границами. Новый baseline benchmark будет linked только
с проверенной root library из isolated baseline-verified cache: её SHA проверяется
до и после benchmark-only rebuild. Новые builds дополнительно сохраняют root
rlib/rmeta рядом с EXE/PDB. Longer-window calibration и gates ещё не выполнены.

**Полный NativeAsync/backend gate текущего source2 — 2026-10-12.**
`cargo test --offline -j1 --no-default-features --features native-async-io`
с `--lib` и10 публичными targets прошёл:1351 library tests,11 ignored,
ещё88 integration tests,0 failures; library execution141.53s, build10m46s.
Targets: in_memory_datastore, local_datastore, changefeed_api,
changefeed_lifecycle, changefeed_oracle, changefeed_local_sync,
changefeed_local_native_async, live_in_memory, live_local, events_local.
Это свежая проверка retained source2.

Baseline-measure30 benchmark build PASS34s. Root rlib SHA до и после rebuild
совпадает с проверенной baseline-verified библиотекой; root features[],
архив055 подтверждён предыдущим PDB source audit. Новые EXE/PDB/root images
сохранены отдельно. Current2-measure30 build PASS17m05s; root compiled fresh=false,
features[], EXE/PDB/root images сохранены. Production code не менялся: собраны
новый harness и исправленная
Stage29 checkpoint capacity для10K resource measurements.

ABBA self-control30s дал −3.22/+1.72/+1.15/−0.86%; upper95CI
+1.90/+7.40/+6.13/+3.87%,2/4 strict gate PASS. Raw каждой пары сохранён.
Начата verified055/current30s query series; первые две пары −0.14/−3.08%,
upper95CI+4.86/+0.89%, обе PASS. Полная серия и остальные workloads pending;
Stage29 performance gate пока не закрыт, source attempt5 не начат.

**Окончательные ON/resource measurements source2.** Series current2-measure30
прошла все6 cases с checked outputs; child HighQoS, logical6/mask64.
Fanout1/8/64 mean46.90/239.88/1888.65µs;64 ASYNC jobs завершены с effects и
completion states за5.044664s. Graph1K assembly2.175ms и whole
commit/delta/apply/ACK14.953ms;10K —28.030ms и212.390ms. Initial10K checkpoint
принят с явными10002 queue entries и64MiB byte budget.

При удержании32 immutable guards whole-process WS/private commit увеличились
на29.54/21.21MiB для1K и288.64/286.29MiB для10K; peak WS10K346.10MiB.
Метрика включает datastore/history/allocator и graph materializations, а не
только graph heap. Прежняя partial series остаётся отдельным environment
evidence; эти абсолютные времена не заявляются как paired speedup.
Оставшиеся ordinary point/DML/import workloads10K/100K/1M запущены отдельно;
общий performance gate OPEN.

**Измерения включённых Events/LiveGraph — 2026-10-12.** Сохранённый EXE
попытки2, RAM SDK, profiling, logical CPU6/mask64: sync fanout1/8/64 mean
19.89/113.48/979.01µs;64 ASYNC invocations завершены за5.019s с проверкой
completion states и handler effects. На1000 nodes ordinary graph assembly
mean1.095ms, commit+canonical delta+SDK apply+ACK mean8.813ms. На10000 nodes
assembly mean14.048ms, затем initial LIVE checkpoint отклонён default entry
limit10000: graph metadata тоже входит в targets. Diagnostic выводит byte
limit64MiB и сам по себе не доказывает byte overflow. Это штатное admission
rejection, не нехватка памяти при сборке. Benchmark теперь явно допускает
max(default entries, rows+2), сохраняя64MiB byte limit; повтор и retained RSS
pending. Исходный failed manifest `g29-specific-attempt02.json` сохранён.

**Калибровка и попытка4/10 — 2026-10-12.** Same-EXE self-control attempt2
дал −0.25/−1.50/+2.03%; upper95CI последней пары+6.70%, поэтому строгий5%
gate чувствителен к шуму текущего runner. Это не новая source attempt и не
основание считать gate пройденным. Гипотеза4: ControlRuntime заимствует общий
RuntimeBudget у ExecutionControl вместо нового Arc owner на каждый interpreter
frame. Shared atomic gas, child ownership, loop reset, effects и public SDK
сохраняются. Private lifetime распространён на interpreter/TraversalMachine;
точные12-file backups находятся в `attempt04-before`. Owner rust-performance,
construct profile rust-ownership, evidence rust-verify; profile-first и
black-box benchmark rules действуют. Cargo check no-default lib PASS9.94s;
focused gas/control/traversal **1/19/59 PASS**; fresh read-only review **PASS**,
actionable findings нет; nightly owned formatting **181 PASS**. Optimized build
PASS17m40s, EXE/PDB/checksums сохранены под `attempt04/current`. Прямые пары
с попыткой2 дали +1.33/+9.28/+4.80%; lower95CI второй/третьей пары выше0.
Попытка4 **REJECTED/ROLLED BACK**: точные12 source files восстановлены,
остальные dirty changes сохранены; counter4/10. Baseline compare этого
отклонённого варианта не нужен для решения о его пользе.

**VTune — новое steering пользователя.** Указанный путь
`D:/Programming/Tools/Intel/oneAPI/vtune/2026.4` содержит VTune2026.4.0 build632893.
По встроенной CLI documentation и [Intel Hotspots guidance](https://www.intel.com/content/www/us/en/docs/vtune-profiler/user-guide/2023-0/run-basic-hotspots-analysis-command-line.html)
выбран software sampling10ms со stacks, без characterization counters.
Capture включает launcher и выбранный benchmark child, не всю систему;
приложение pinned logical6/mask64, сам profiler не pinned. Exact baseline
profiling16-probe2 и retained attempt02 имеют verified EXE/PDB hashes;
Criterion15s profiling mode выполняет один RETURN1/10K без statistical analysis
и plots. Лимиты collection25s/128MB; raw results и manifests сохраняются
в `target/stage29/vtune29-query-sw`. Attribution pending; это диагностика,
не source optimization attempt и не доказательство ускорения.

**Текущее возобновление — 2026-10-12.** После нового «Продолжай» выбран
существующий repository-owned `profiling` для обеих версий: opt3, codegen16,
lto=false, effective debug2, strip=none, static CRT. Этот профиль уже одинаков
в baseline/current; manifests и host settings не меняются. Гипотеза resource
strategy: разделить большой LLVM module вместо single-codegen. Logs/artifacts
получают suffix `profiling16`; прошлые LLVM OOM evidence сохраняются.
Source optimization counter **0/10**; baseline measurements ещё не получены.

**Актуальное performance evidence — 2026-10-12.** Обе сборки `profiling`
успешны: baseline23.49s (warm root), current15m01s. EXE/PDB и SHA сохранены
в `target/stage29/performance-artifacts/profiling16`. Optimized smoke — три
executables /10 checked cases PASS. Input Cargo.lock различался только CRLF/LF;
original archive/current hashes и parsed TOML equivalence проверены в
`g29-lock-equivalence.json`, dependency graph не менялся.

Initial paired series (`g29-disabled-initial.json`) содержит шесть полных пар
10K/100K ×3. Общий gate **FAIL**: import1000 на100K +8.74/+9.61/+10.13%,
lower95CI +6.34/+7.70/+8.37% соответственно. Point16 и DML64 прошли все
шесть пар; query имеет широкую CI и требует noise/hotspot диагностики.
На1M baseline DML запросил50samples/766.7s; серия остановлена после подтверждённого
сбоя import gate. Завершённого1M pair нет; все partial logs/samples сохранены.
Остановлены только проверенные parent measurement process и его benchmark child.

Для последующей матрицы heavy1M DML использует10samples (Criterion minimum),
остальные cases50; параметры одинаковы в baseline/current. По сравнению с первым
планом reduced heavy-case statistical coverage указана явно. Добавлен benchmark-only
phase probe через public SDK и существующие ImportReport timings, сначала шесть
phases, затем callback/validation/publication с background100K. Production Rust
не менялся; source optimization attempts **0/10**. RuleSet: `perf-profile-first`,
`perf-black-box-bench`. Evidence: `g29-import-phases.json`, далее
`g29-import-phases2.json`; artifacts имеют отдельные tags `profiling16-probe*`.

**Оптимизация1/10 — в проверке.** Binary/PDB scout обнаружил увеличение
`ensure_capacity`65→422bytes, включая две необходимые optional-presence checks.
Гипотеза — уменьшить code/register pressure редкого retained-payload accounting.
Три базовых saturated domains вынесены в общий private const helper; public
`write_set_bytes` сохраняет полный authoritative total. Capacity guard при
обеих `None` использует базовую сумму, любой `Some` — cold/noinline full path.
Some(empty с retained capacity), limits, CF cap, overflow и typed errors сохранены.
Независимый quota review **PASS**, builds/tests им не выполнялись; measured win
не заявлен. Focused event_import — **12 PASS**, включая ON/OFF, private quota,
SKIP/QUARANTINE, failed END и local recovery. Qualified resource suite — **27 PASS**. Optimized artifact `attempt01` собирается;
paired gate остаётся pending.

**Итог оптимизации2 — RETAINED, общий gate OPEN.** Optimized build PASS
13m09s, EXE/PDB сохранены под `attempt02`. Baseline RETURN1/10K три пары
+8.49/+6.94/+8.72%; 5% gate пока FAIL. Прямой current-before/current-after
показал −3.99/−4.30/−6.83%, все upper95CI ниже0: изменение сохранено.
`attempt02-outcome.json` фиксирует решение; final library/backend/all-workload
checks для окончательного source state ещё pending.

**Итог оптимизации3 — REJECTED/ROLLED BACK.** Build PASS17m15s; baseline
query pairs +26.41/+13.01/+17.22%. Прямое сравнение с retained attempt2
+7.99/+13.08/+2.56%; выигрыша нет. Три файла восстановлены до attempt2 после
проверки точного inverse diff. Rejected source, matching EXE/PDB, logs/samples
сохранены. Счётчик **3/10**; только попытка2 сохранена. Общий5% gate OPEN.

**Оптимизация3/10 — в проверке.** Source-added RELATE endpoint datatype
увеличил common LogicalGraphDml/LogicalStatement1536→1568. В private IR
только source/target endpoints перенесены в Box; AST/SDK/storage без изменений.
Shared endpoint helper допускает pointee перед allocation. Source-span-based
compiler/closure clone budgets не уменьшаются вместе со statement shell.
Три-file scope подтверждён scout, fresh reviewer **PASS**. Focused RELATE
**10 PASS**, включая sync/async qualified context и четыре incidence directions.
Owned nightly format **181 PASS**. Новая цена — две allocations при RELATE;
optimized artifact и paired performance ещё проверяются.

**Оптимизация2/10 — в проверке.** Только новые SELECT `return_graph` поля
AST/IR теперь `Option<Box<...>>`; старые MATCH/TRAVERSE/standalone terminals
сохраняют представление. Parser/SDK/analyzer constructors и borrowed callback
обновлены. Scout подтвердил полный caller scope и отсутствие durable codec
изменений. Общий SELECT-only analyzer helper допускает heap shell перед Box;
copy admission отдельно учитывает четыре shell копии. Добавлен calibrated
low-budget regression test, который отклоняет попытку при бюджете только
на остальные plan/child bytes. Initial check PASS; новый
admission test **1 PASS** после исправления неподдерживаемого FROM[] на пустой CLASS.
Focused graph_result **45 PASS**, SELECT terminals **2+1 PASS**. Fresh reviewer
**PASS**, runtime/build checks им не выполнялись; optimized artifact и paired gain pending.

**Последующая диагностика — 2026-10-12.** Controlled common series остановлена
после трёх10K pairs: RETURN1 +15.59/−1.72/+17.49%; нижние95CI для двух failures
+12.04/+15.82%. Partial100K logs сохранены, полный100K/1M gate не заявляется.
Счётчик1/10; новая source optimization пока не начата.

Installed samply0.13.1/xperf и текущие admin rights подтверждены. Первый запуск
без keep-etl завершился conversion error (invalid handle); диагностический режим
с keep-etl дал оба profiles и local sidecars, без GUI и symbol servers. Requested
9s duration не используется как утверждение о фактической длительности: capture
содержит benchmark main thread и его plot descendants. Анализ отбирает только
main benchmark GQL/frontend stacks; raw traces остаются локальным evidence.

Rust symbols восстановлены из matching PDB publics и bounded procedure ranges
через LLVM, ICF aliases сохранены, inline frames недоступны. Relevant samples:
baseline9395/current9484; exclusive memcpy19.67%→24.73%. PDB содержит исторические
type records, поэтому размеры старых entries не выдаются за текущие: наблюдаемые
SELECT AST728→1096 и IR1248→1904 согласуются с source-added inline graph terminal.
Гипотеза попытки2: Box только новых optional SELECT graph payloads; семейства,
query semantics, MATCH/TRAVERSE и durable codecs сохраняются. Проверяются SDK
construction, borrow/clone consumers и pre-copy admission heap pointee.

**Итог оптимизации1/10 — REJECTED/ROLLED BACK.** Прямые pinned пары
с current до изменения дали +1.19/−3.00/−1.06%; устойчивого полезного эффекта
не подтверждено. Capacity refactor откатан ровно в четырёх проверенных hunks;
все остальные dirty изменения этапа29 и Q-42 сохранены. Rejected source,
EXE/PDB, raw Criterion samples и `attempt01-outcome.json` оставлены для аудита.
Счётчик **1/10** включает эту попытку; source optimization#2 не начата.
Дальнейшее сравнение выполняется для исходного current artifact
`profiling16-probe2` с одинаковой CPU affinity baseline/current.

**Измерения попытки1 — 2026-10-12.** Optimized build PASS за15m18s,
EXE/PDB и hashes сохранены отдельно под `attempt01`. Три unpinned import100K
пары дали −8.91/+16.32/+2.93%; верхняя95CI соответственно −3.39/+23.64/+9.94%.
Gate FAIL_OR_INCONCLUSIVE, улучшение не принято. Phase control до/после имеет
согласованный разброс почти всех фаз; влияние CPU scheduling остаётся гипотезой.

На текущем i5-12450H Windows topology API обнаружил4 SMT performance cores
(EfficiencyClass1) и4 efficiency cores (class0). Measurement runner и оба его
benchmark children временно закреплены за logical processor6, mask64; inheritance
проверен отдельным child self-check. Host/production settings не меняются.
Новая серия `attempt01-import-pcore` сохраняется отдельно, старые samples не
перезаписываются. Это environment control, source attempts остаются **1/10**.
Основание выбора: [Microsoft PROCESSOR_RELATIONSHIP](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-processor_relationship),
[SetProcessAffinityMask](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-setprocessaffinitymask).

**Будущий ASYNC parallelism — Q-42.** По запросу пользователя старое draft
предположение сохранено в реестре как отдельное исследование bounded parallelism.
Нужно доказать bottleneck и проверить независимые/конфликтующие handlers, порядок,
FCW/SSI, fairness, cancellation, recovery и resource budget относительно sequential
oracle. Текущий worker остаётся sequential; Q-42 не блокирует этап 29 и не является
одной из десяти его source optimization attempts.

**Cross-document сверка — 2026-10-12.** Устаревшие формулировки §9.5/§21.4
основной GQL specification приведены к принятому EV-04: sequential worker,
frozen accepted version, дополнительные RETRY и causal MAXDEPTH через sync/async.
DEFINE/ALTER/REMOVE явно принимают CLASS/GRAPH; import modes согласованы с
реализованным независимым EVENTS/CHANGEFEED. Q-33 и full_plan показывают реализованный
correctness-срез и открытый performance/tooling closure; новые решения не вводились.

**Возобновление после LLVM OOM — 2026-10-12.** Пользователь вновь разрешил
продолжить. Для baseline и current выбран один и тот же command-scoped
`--config profile.bench.lto=false`: opt-level3, codegen-units1, debug symbols,
static CRT, allocator, features и toolchain сохраняются. Это отдельный
release-like measurement profile; результат не объявляется измерением fat LTO.
Конфигурация проекта/pagefile и исходный baseline не меняются. Новые artifacts
и logs получают suffix `nolto`, исторический failure evidence сохраняется.
Pair runner проверяет совпадение profile/features/toolchain/Cargo.lock/harness.
При повторном resource failure работа снова останавливается по условию пользователя.

**Лимит оптимизаций пользователя — 2026-10-12:** разрешено не более10 попыток
этапа29. Счётчик **0/10**: сборка paired baseline остановлена, source optimization ещё
не выполнялась. Одна попытка = одна проверяемая гипотеза, изменение и paired
gate; rejected/rolled-back варианты тоже учитываются. После десятой дальнейшие
оптимизации прекращаются; неисполненный gate сохраняет OPEN. Сборки/проверки
неизменного кода отдельно от этого счётчика. Долги и baseline оптимизации28
не включаются молча в этап29.

**Последняя остановка — 2026-10-12, 03:35 MSK.** Даже симметричный no-LTO
profile не позволил собрать baseline library: `rustc-LLVM ERROR: out of memory`
и `Allocation failed`, Cargo101 / rustc0xc0000409. Команда выше дополнена
`--config profile.bench.lto=false`; opt3/codegen1/debug=true/staticCRT сохранены.
Evidence: `g29-performance-build-baseline-nolto.jsonl` и соответствующий `.json`.
Сборка длилась14m00s. Benchmark EXE/PDB и runtime samples не получены.
Перед отказом free physical448,528KiB / virtual794,524KiB; после завершения
процесса C9.83GB/D141.88GB free. Filesystem ENOSPC не наблюдался: это LLVM
allocation OOM. Работа прервана по условию пользователя; повторные сборки,
очистка артефактов, изменения manifests/pagefile не выполнялись.
Контекст сохранён в `target/stage29/resume-closure.md`, счётчик source attempts
**0/10**. Старые остановки ниже — исторический evidence. Этап **OPEN**.

**Остановка по условию пользователя — 2026-10-12.** Optimized baseline artifact
`gql_stage27_common` не собрался: `rustc-LLVM ERROR: out of memory`, затем
`Allocation failed`. Это произошло при fat LTO с одним Cargo job. Cargo exit101;
rustc завершился с `0xc0000409` после явного allocation failure. Filesystem
ENOSPC не установлен; недостаток памяти компилятора подтверждён.

Команда в `target/stage29/baseline-055af48`:
`cargo bench --offline -j1 -p ferros-grynn-bench --bench gql_stage27_common --no-run --message-format=json`,
`CARGO_TARGET_DIR=D:/Programming/Rust/ferros-grynn/target`. Profile:
opt3/fatLTO/codegen-units1/debug=true/staticCRT, root no-default features.
Baseline production sources — `055af48`; common harness идентичен текущему.
Evidence: `g29-performance-build-baseline.jsonl`, `g29-performance-build-baseline.json`.
Время: 17m08s. Финальный benchmark EXE/PDB не получен; measurements не запускались.
Последнее наблюдение до отказа: C3.35GB/D17.18GB free, virtual free1,357,508KiB.
После завершения процесса: C8.11GB/D17.18GB free. Повтор, очистка артефактов,
изменение profiles и pagefile settings не выполнялись.
Точная точка продолжения — `target/stage29/resume-closure.md`; current optimized
build, paired performance, отдельные Stage29 measurements и final Graphify
остаются pending. Этап **OPEN**, commit отсутствует.

| Актуальная проверка | Результат | Evidence в `target/stage29` |
|---|---|---|
| Публичная RAM/local SDK-матрица, NativeAsync, `-j1` | 71 PASS | `g29-public-native-matrix-j1.log` |
| Полный library suite, default features, текущий raw G0 fix | 1341 PASS, 0 FAIL, 10 ignored; 120.39 s | `g29-default-library-selector.log` |
| Полный library suite, no-default NativeAsync, текущий код | 1350 PASS, 0 FAIL, 11 ignored; 122.47 s | `g29-current-native-library.log` |
| Настоящий Tokio selector и external/cancellation branches | 1 PASS | `g29-default-library-selector.log` |
| Свежий независимый G3 closure review | PASS, actionable findings отсутствуют | Review сохранён в текущей задаче |
| Scoped nightly formatting без записи | 181 files PASS | `g29-owned-format.json` |
| Свежий Clippy, no-default NativeAsync library | 0 errors в изменённых строках 179 production files; global run exit101 / 6875 errors | `g29-clippy-current.jsonl`, `g3-current-slice.json` |
| Оба benchmark targets, Clippy `--no-deps` | PASS, 0 diagnostics | `g29-clippy-bench-final.jsonl` |
| Protected `just all-check`, default features | check/fmt PASS; global Clippy exit101, 6913 errors / 662 warnings; 807 primary files без изменений | `g3-just-all-check-protected.log`, `g3-just-all-check-protected-result.json` |
| Benchmark smoke: common query/point/DML/import и graph assembly/delta | 6 cases PASS | `g29-benchmark-smoke.log` |

Benchmark smoke проверяет исполнение fixtures; performance samples из него не
получены. Paired baseline/current measurements пока не закрыты.
Full-plan completion не изменён.
Накопленный global Clippy debt остаётся отдельным `Q-14`; lint levels не ослаблены.
Первая benchmark Clippy-команда остановилась на ошибках dependency root crate,
до проверки benchmarks; финальная `--no-deps` проверяет именно оба bench targets.

**Продолжение 2026-10-11.** После команды пользователя «Продолжай» та же публичная
SDK-матрица собрана с `cargo ... -j1`: `g29-public-native-matrix-j1.log`, все шесть
targets PASS. Артефакты/toolchain не удалялись, pagefile settings не менялись.
Итог: **71 PASS** (Events2, RAM4, LIVE RAM10, LIVE local38, local16, public paths1).
Read-only G3 closure review — **PASS**, actionable findings отсутствуют;
post-registration restore boundary resnapshot сохраняется как явный residual risk.
Дальнейшие сборки выполняются по одной с одним Cargo job. Историческая остановка
ниже сохранена как evidence; текущий этап остаётся OPEN до closure.

**Остановка 2026-10-11 по условию пользователя.** При сборке публичной RAM/local
SDK-матрицы с NativeAsync Windows отказал в mmap Rust `libtest` metadata/rlib:
«Файл подкачки слишком мал для завершения операции» (`os error 1455`, далее
E0786/E0463). Evidence: `target/stage29/g29-public-native-matrix.log`, exit101.
Тесты этой команды не запускались. Свободное место C8.38GB/D42.26GB не доказывает
достаточный virtual-memory commit limit; файловое ENOSPC не установлено.
Повторная сборка, очистка артефактов и изменение pagefile не выполнялись.
Точная точка продолжения: `target/stage29/resume-closure.md`. Performance,
current public SDK matrix, protected all-check и final Graphify остаются pending;
этап **OPEN**, commit отсутствует.

Актуальная точка на 2026-10-11: E0–E5 и G0–G2 имеют focused evidence;
G3 SDK conflict dispatch/resolution, branch-based waiting, explicit transaction
flush и explicit resnapshot реализованы.
Ожидание использует одного consumer, `next_event()` и `live_graph_select!`;
`LiveGraphNext::Conflict` передаёт решение пользователю. Полный этап остаётся
**OPEN** до всех closure gates. Исторические записи
ниже относятся к указанной в них точке реализации.

Текущий full library gate с `--no-default-features --features native-async-io`
ранее прошёл: **1342 PASS, 0 FAIL, 11 ignored**, 132.30 s
(`g3-full-library-native2.log`). Он выполнен после source flush, exact commit
feedback, property key stability и resnapshot; новые conflict corner fixtures
ниже требуют повторной focused проверки.

Последующий `g3-full-library-native-final.log`: **1348 PASS, 1 FAIL, 11 ignored**.
Сбой воспроизведён отдельно (`g3-native-streamed-failure-repro.log`) в старом
oracle `streamed_search_index_failures_discard_unreachable_generation`: число
UUID-каталогов уменьшалось 2→1 вследствие retirement baseline generation.
Oracle теперь проверяет конкретные UUID paths и отсутствие нового каталога
после failed build; scan errors не подавляются, проверки data/reopen сохранены.
Focused `g3-native-streamed-generation-oracle.log` — **1 PASS**; полный повтор
`g3-full-library-native-final2.log` — **1349 PASS, 0 FAIL, 11 ignored**, 267.38 s.
Oracle проверяет идентичности каталогов, а не только count threshold.

`g3-mutable-final-tests2.log` — **17 PASS**, включая post-flush structural
Conflict, accept/discard/rebase более раннего delivery и commit race для
ReadCommitted/Snapshot/Serializable. Каждый isolation сохраняет source FCW
requirements. Clippy после documentation/import/module-layout/error-cause
работы (`g3-clippy-after-docs.jsonl`) возвращал **6877 errors**, из них два
попадали в актуальные изменённые строки; оба исправлены, свежая проверка pending.
Это отдельный результат от предыдущего 7192-errors snapshot.
`g3-clippy-final.jsonl`: **6875 total errors, 0 diagnostics в изменённом
production slice из179 Rust files**. Новый benchmark проверяется отдельно.
Полный Sync gate `g3-full-library-sync-final.log`: **1332 PASS, 0 FAIL,
10 ignored**, 262.84 s. Этот прогон предшествует последней raw G0 correction:
invalid opaque callback теперь отвергается до pending/queue admission; один
validated COW remote tail сохраняет context уже принятых queued updates.
SDK image/delivery path от этой коррекции не меняется.
`g0-raw-admission-final-tests.log` — **21 PASS**, включая rejected overlapping
callback, rejected queued callback, corrected same revision и valid remote /
invalid local queue merge с сохранением pending/base/dirty/queue.
`g29-benchmark-check2.log` — **PASS**, оба maintained Stage29/common targets.

Flush использует принадлежащую caller transaction, проверяет все accepted
effective images/mappings до первой mutation и требует FCW validation при любом
isolation. Result feedback захватывается из exact commit candidate до WAL,
без нового lookup primary root. Polling сохраняет ordering/ACK и применяет
только edits, сделанные после успешного flush. SDK sparse property handles
сохраняются при появлении source dictionary key с тем же именем. Child bindings
применяются до connector topology; equality physical AtomId не подменяет
qualified logical endpoint identity. Потеря result closure не удаляет child
source data. Resnapshot заменяет incarnation через обычную transactional LIVE
registration; старые read guards продолжают читать прежний snapshot.

Focused source flush: `g3-flush-family-tests2.log` — **7 PASS**, в том числе
все восемь graph families, edge-owned child rebinding, SYNC EVENT effects,
cached synthetic property key, work admission и stale/foreign mapping rejection.
`g3-flush-local-tests.log` — **2 PASS**, Sync и NativeAsync: failed WAL barrier
сохраняет local image и cursor; после reopen successful flush/ACK/checkpoint
сохраняет source value и точный graph result.
`g3-flush-overflow-tests2.log` — **1 PASS**: GAP требует explicit resnapshot;
BLOCK rejection происходит до source publication. Первоначальный overflow
fixture имел queue capacity меньше initial checkpoint; исправлен fixture.

Focused G3 evidence: `g3-reviewed-mutable-tests2.log` — 2/2 PASS;
`g3-conflict-search-tests.log` и `g3-snapshot-admission-tests.log` — по 1/1 PASS;
`g3-wait-key-tests2.log` — 4/4 PASS, включая cancellation без ACK,
late local edits, close и lifetime sparse property keys;
`g3-shared-scope-tests.log` и `g3-shared-dag-tests.log` — по 1/1 PASS.
`g3-ack-failure-recovery-tests.log` — 2/2 PASS (Sync и NativeAsync, каждый
проверяет до/после WAL append). Injected I/O failure требует reopen согласно
существующему poisoned-WAL contract; первоначальный fixture ошибочно пытался
публиковать ACK тем же writer (`g3-ack-failure-tests.log`, FAIL fixture).
После recovery подтверждённый delta не доставляется повторно; до append он
остаётся доступным для принятия и ACK. Last local image и unconfirmed token
сохраняются в прежнем SDK handle после shutdown.

Финальная focused проверка conflict waiting: `g3-structural-wait-name-tests2.log`
— 5/5 PASS; добавлен DAG oracle для двух valid disjoint topology edits, вместе
создающих цикл. SDK сохраняет typed Conflict с diagnostic; невалидный keep_local
сохраняет pending state, accept_remote подтверждает весь delta.
`g3-conflict-classification-tests.log` — 1/1 PASS: resource exhaustion остаётся
ошибкой, а graph invariant violation попадает в conflict branch.
`g3-real-tokio-selector-tests.log` — 1/1 PASS для внешнего caller с настоящим
`tokio::select!`, alias crate и external cancellation branch.

Tooling closure остаётся OPEN. Global nightly formatting check обнаруживает
diffs вне текущего среза; автоматическое исправление чужих файлов не выполнялось.
Clippy текущего checkout возвращает 7 192 errors; сюда входят существующие
нарушения и незакрытая documentation/module-layout работа новых Stage29 paths.
Это не green gate и не только baseline debt. Точные diagnostics сохранены в
`target/stage29/g3-clippy.jsonl`, scoped summary — `g3-clippy-summary.txt`.
Graphify AST update успешно завершён через existing atomic sidecar writer после
Windows failure `.graphify_labels.json.sig`; исходная signature сохранена.
Без новых dependencies/runtime, без commit и без очистки build artifacts.

## Область и рабочие ограничения

Реализуются E0–E5, затем G0–G3 и closure. Авторизованный контракт включает
durable outbox, frozen EVENT/UDF source versions, все occurrence changes,
multi-source LIVE SELECT через общий RETURN GRAPH и все graph families.
Открытые performance/tooling gates 27–28 не объявляются закрытыми.
Чужие dirty изменения README, asynchronous, collections, reference trees,
prototype deletions и существующих Graphify outputs сохраняются.

Пользователь потребовал при нехватке места для build artifact сохранить
точную точку продолжения и остановиться. На старте C: свободно 10 271 137 792 B,
D: 55 241 539 584 B. Очистка артефактов для обхода этого ограничения не разрешена.
Windows sandbox command startup возвращает `setup refresh had errors`;
разрешённое выполнение через `require_escalated`, `login:false` работает.

## TaskBrief / ProfileStack

E0: owner `specs`, contract audit; E1: owner `refactoring`, private logical
operation boundaries. E1 сохраняет API, errors, no-op tracking, journal ordinals,
savepoint/import behavior, feature combinations и cheap disabled path.
Rule overlay E1: `pat-exhaustive-enum` для закрытых mutation/replay состояний.
Coverage gaps E0/E1: нет; acceptance — существующие journal/savepoint tests,
import characterization, feature checks и scoped format/Clippy.
После E1 API/serialization/transaction decisions получают отдельные owners;
публичный контракт не обосновывается только компиляцией.

Read-only Events/LiveGraph scouts подтвердили production seams; единственный
writer — основной агент. Scout findings:

- EVENT depth должен быть независим от outer Changefeed operation nesting.
- Semantic no-op определяется before/after image, не количеством mutations.
- Shared historical image projector уже содержит effective values и qualified
  topology; его следует отделить от Changefeed policy перед reuse.
- Outbox authority не может жить только в metadata-COW `FGJB`.
- Import seal очищает mutations, поэтому pending invocations требуют spool.
- SELECT не содержит graph terminal; общий assembler уже существует.
- MaterializedGraph equality сравнивает identity/revision, поэтому graph history
  требует logical image diff и initial checkpoint.
- LiveGraph pending/queue resolution сейчас теряет delta при некоторых errors;
  keep_local не обновляет remote base; qualified writeback mapping пока single-source.

## Проверки

| Срез | Команда | Результат | Evidence |
|---|---|---|---|
| Baseline E1 | `cargo test --locked --lib --no-default-features datastore::transaction::changefeed_journal::tests -- --test-threads=1` | PASS: 4/4 | Baseline EXE `target/debug/deps/ferros_grynn-6ac16c6770f4e12a.exe` |
| E1 extraction | Та же команда | PASS: 4/4 | `target/stage29/e1-journal.log` |
| E1 operation/image owner | `cargo test --locked --lib --no-default-features datastore::transaction -- --test-threads=1` | PASS: 136/136 | `target/stage29/e1-transactions.log` |
| E2 intermediate production check | `cargo check --locked --lib --no-default-features` | PASS | `target/stage29/e2-check.log` |
| E2 initial SDK/source/descriptor | `cargo test --locked --lib --no-default-features event_ -- --test-threads=1` | PASS: 11/11 | `target/stage29/e2-event-tests.log` |
| E2 review repairs | `cargo test --locked --lib --no-default-features event_ -- --test-threads=1` | PASS: 14/14 | `target/stage29/e2-repairs.log` |
| E2 FGDC v14 header/sections | `cargo test --locked --lib --no-default-features storage::catalog::database -- --test-threads=1` | PASS: 19/19 | `target/stage29/e2-catalog-header.log` |
| E2 DDL/parser/SDK | Focused `event_` | PASS: 19/19 | `target/stage29/e2-ddl-tests-fixed.log` |
| E2 local Sync | `cargo test --locked --test events_local --no-default-features` | PASS: 1/1 | `target/stage29/e2-local-sync.log` |
| E2 local NativeAsync | Тот же target с `--features native-async-io` | PASS: 2/2 | `target/stage29/e2-local-native.log` |
| E3 shared runtime budget | Existing control-flow slice | PASS: 19/19 | `target/stage29/e3-control-budget-fixed.log` |
| E3 initial sync repair | Focused `event_sync` | PASS: 8/8 | `target/stage29/e3-sync-tests-fixed.log` |
| E3 transaction suite | `cargo test --locked --lib --no-default-features datastore::transaction -- --test-threads=1` | PASS: 152/152 | `target/stage29/e3-transactions.log` |
| E3 expanded sync | Focused `event_sync` | PASS: 16/16 | `target/stage29/e3-sync-expanded.log` |
| E3 local cold execution | `events_local`, Sync + NativeAsync | PASS: 2/2 | `target/stage29/e3-local-sync-native-fixed.log` |
| E4 source/compiler + common DML | Focused `event_` | PASS: 39/39 до новых review repairs | `target/stage29/e4-dml-reader-event-tests.log` |
| E4 DML frame refactor | Existing `gql::execution::tests::dml` | PASS: 6/6 | `target/stage29/e4-dml-frame-refactor.log` |
| E4 logical value codec extraction | `storage::live_history` | PASS: 14/14 | `target/stage29/e4-logical-values-refactor.log` |
| Tooling E2 | `cargo +nightly fmt -- --check` | FAIL: существующий formatting debt | `target/stage29/e2-global-fmt.log` |
| Tooling E2 | `cargo clippy --locked --lib --no-default-features --message-format=json` | FAIL: 6997 errors, 538 warnings; отдельные новые findings исправляются | `target/stage29/e2-clippy.jsonl` |

Existing compile warnings не исправляются этим срезом. Full stage correctness,
resource/recovery/performance и tooling gates ещё не выполнялись.

## Изменения и продолжение

E0 contract сохранён. E1 переносит `logical_operation` и `atomic_mutation`
в `transaction/operation.rs`, оставляя capture journal у Changefeed.
E1 characterization и read-only review пройдены. E2 SDK definitions, schema
mutations, FCW/SSI namespaces, source-only closure compilation и FGDC v14
Event descriptor round-trip подключены. DEFINE/ALTER/REMOVE EVENT проходят
общие AST → analysis → IR → execution; INFO CLASS/GRAPH возвращает definitions.
Catalog использует 140-byte FGDC v14 header: EVENT count/u32 находится после
LIVE history count; ordered descriptors идут после LIVE history entries.
Минимальный descriptor — 44 bytes.
Local conversion/recovery не исполняют source; Event revisions восстанавливаются.

E2 ProfileStack: public API owner rust-api-design; explicit descriptor owner
rust-serialization; verification rust-verify. Source compilation использует
existing closure analyzer вместо отдельного evaluator или parser. Retained source
и compiler scratch допускаются до copies/tokenization. Rule overlay:
pat-exhaustive-enum — explicit execution/source-kind codec branches.

## Review ledger E2

Helper addressing-findings активирован после confirmed reviewer evidence.

| ID | Triage | Исправление | Evidence |
|---|---|---|---|
| E2-R1 | ACCEPT / FIXED | Owner removal удаляет current Events вместе с class/graph tree; frozen job payloads не затрагиваются | RAM repairs и local reopen PASS |
| E2-R2 | ACCEPT / FIXED | Каждый retained replacement оплачивается независимо от FCW key dedup | Repeated replacement PASS |
| E2-R3 | ACCEPT / FIXED | Bounded compiler/work, pre-wrapper admission, inherited query interruption и predicate retention | Transitive UDF resource PASS |

E3 ProfileStack: transaction owner rust-database; shared gas/depth owner
rust-concurrency; verification rust-verify; read-only review rust-review.
ControlRuntime использует общий RuntimeBudget вместо отдельного gas counter
для каждого callback. Scoped EVENT controls сохраняют cancellation/deadline,
barrier budget и named-function depth. Operation savepoint откатывает original
mutation вместе с handlers; commit replay остаётся без user evaluation.

Nested dispatch первоначально пропускал повторный target из-за Changefeed
coalescing. Detection перенесён на mutation slice; feed ordinals сохраняются.
Initial sync evidence до repair: 5 PASS / 3 FAIL (`e3-sync-tests-run.log`).
После repair: 8/8. Новые review regressions сейчас проверяются.

## Review ledger E3 и точка продолжения

| ID | Triage | Исправление | Evidence |
|---|---|---|---|
| E3-R1 | ACCEPT / FIXED | Source context регистрирует BaseAtom, membership, overlay, incidence, graph definition и qualified owner reads | Serializable-cycle regression PASS; fresh review PASS |
| E3-R2 | ACCEPT / FIXED | Каждый context/property/endpoint/owner/label/value copy допускается до allocation; compiler phases удерживают свои reservations | Property/label resource regressions PASS; fresh review PASS |
| E3-R3 | ACCEPT / FIXED | Bounded per-operation GQL input frame захватывает evaluated payload до normalization/diff; сохраняет equal fields и PATCH removals | Raw-input regression PASS; fresh review PASS |

Qualified endpoint payload сохраняет source/target occurrence arrays и aligned
source_paths/target_paths с exact ordered owners; local path пуст.
E3 focused behavior и fresh review пройдены. После E4 common-kernel repairs
нужны повторные transaction/control/local gates и changed-slice Clippy.

## E4 source foundation / debugging checkpoint

`adapter/event.rs` замыкает оба function lookup на exact accepted source set;
все остальные schema/data readers относятся к текущему root. Transitive
dependencies берутся из того же QuerySnapshot, что compilation. Predicate
и handler compiler/evaluation scopes независимы; false WHEN не компилирует THEN.
Frozen test удаляет/redefines UDF и подтверждает старый body → новое значение 9.

E4-F1: ACCEPT / FIXED — graph-map snapshot copies допускаются compiler ledger
и invocation barrier account до allocation. Regression с 2000 graphs PASS.
Первый frozen fixture с inferred `any` исправлен обычным explicit cast;
следующий runtime failure воспроизведён обычным DML без EVENT/frozen sources
(`e4-dml-udf-repro.log`). Debugging owner: потеря transaction reader в DML operands.
Сначала refactor mutable lexical frame (6/6), затем общий evaluator получил
row context и reader-aware CONTENT/SET (39/39).

Fresh review E4-DML-R1: REJECT — mutable builtins запрещены analyzer в row-producing
DML независимо от наличия UDF sibling; rejection fixture PASS. R2/R3: ACCEPT / FIXED.
Mutable scalar path сохраняет eval_mut;
ambient GraphRef переносится reader scope; WHERE/EXPECT/RETURN и graph selectors
используют общий readonly expression kernel. Predicate source snapshot не меняется
во время cursor scan; return snapshot видит завершённые private writes.
`e4-dml-review-regressions-grammar.log` 3/3 и `e4-dml-scoped-operand.log` 1/1 PASS;
expanded correctness и fresh review ещё требуются.

`EventOutbox`/`EventInvocation`/`EventCause` model подключён под существующим
HistoryRoot COW binding. Pending ceilings, terminal payload release, retry/depth
metadata и completion states подготовлены; WAL и accepted dispatch подключены,
worker находится в реализации.
Owned logical value codec вынесен из live_history в history_codec/logical_values;
bytes и diagnostics сохранены, существующие 14/14 PASS. StorageValue не менялся.

Windows periodically отклоняет truncate/formatter writes с os error 1224
(mapped source section), при наличии свободного места. Nightly rustfmt stdin
output + проверенная same-directory atomic replacement работает; original bytes
сохраняются в target/stage29/*.before-fmt. Это не disk-space failure.
Последний disk check: C: 10 700 566 528 B, D: 47 923 007 488 B.

FGEI v1 payload и FGEO v1 checkpoint/delta codec используют explicit LE framing
и общий logical-values codec. FGDC v14 header расширен до 144 bytes: EVENT count
offset 136, outbox segment count offset 140. Новые HistoryOwner tag 2 и
DataFileKind 0x18 используют общий FCFS/FCFE page transport. Обе LocalStore
publication branches и IMPORT staging включают outbox в один WAL barrier;
eager cold recovery проверяет каждый pending payload без выполнения user code.
Journal compaction threshold 128 MiB, terminal retention 10 000 / 8 MiB.
Orphan/retirement cleanup учитывают новый events namespace.

`e4-wal-codec-events.log`: 50/50 PASS с native-async-io. До feature gate repair
`e4-wal-outbox-tests.log`: 3 PASS, NativeAsync configuration failure; повтор
Sync/NativeAsync PASS. Проверены before-WAL rollback, Running reopen, completion
вместе с effects и fatal pending-page corruption. Review E4 codec R1/R2/R3
ACCEPT / FIXED: terminal aggregate bounds, reachable Queued attempts,
function/type member counts до allocation; regression PASS.

Accepted THEN compiler сохраняет только exact transitive UDF sources; SYNC/ASYNC
cause scope подключён. Existing worker исполняет accepted handlers на fresh transaction;
Running claim предшествует user code, recovery сохраняет номер attempt, effects +
Completed публикуются одним commit. INFO/KILL SDK/GQL используют root projection;
FGJB не принимает event projections. Interval — positive unsigned milliseconds,
default 5000; отсутствующий/некорректный setting сохраняет default. Pending/running
KILL имеет общий invocation FCW key; shutdown прерывает attempt и сохраняет Running
для recovery. Background Datastore handle не продлевает user ownership;
`e4-worker-ownership-refactor.log` 1/1 PASS.

`e4-worker-behavior.log` 5/5 PASS: retries 0/2, rollback effects, MAXDEPTH 0/3/16,
pending/running KILL, TRY rollback, old EVENT/UDF + current data после ALTER/REMOVE.
Первый expanded прогон `e4-worker-events.log` 55 PASS / 1 WHILE fixture grammar FAIL;
исправление WHILE (true) вошло в passed behavior gate.
Review fairness ACCEPT / FIXED: periodic sweep/retention discovery до выбора EVENT.
Wake generation + deadline wait исключают потерю publication wakeup.

Resource review E4-R2 ACCEPT / FIXED: permitted pending diagnostics добавляют
до 39.1 MiB к payload 64 MiB. Publication scratch bounded 768 MiB, whole-journal
recovery 512 MiB, per-payload query decode 256 MiB. `e4-worst-checkpoint-resources.log`
6/6 PASS: >63 MiB payload, 10 000 pending diagnostics по 4096 bytes + 1000 terminal
diagnostics; forced checkpoint → Running → Completed → Cancelled → cold reopen.
Промежуточный 512/256 budget прошёл 8-payload fixture, но failed full diagnostic
recovery, что сохранено в предыдущем checkpoint/evidence.

E4 owner-removal ACCEPT / FIXED: enqueue compilation получает metadata-only
before schema для retired graph names; WHEN и worker data/schema остаются current.
`e4-retired-graph-source.log` 1/1 PASS: REMOVE GRAPH успешен, accepted DELETE job
получает handler error на актуальной схеме.

E4 frozen-source comment allocation ACCEPT / FIXED: metadata.len()*256 заменён
bounded owned catalog Decoder ledger; строки, type Vec/Box и conversion допускаются
перед allocation. Source bytes и primary codecs не менялись. `e4-exact-decoder-events.log`
61/61 PASS, включая comment-heavy payload roundtrip. Real 300-function accepted/reopen
fixture `e4-commented-sources-reopen.log` 1/1 PASS: accepted sources переживают удаление
EVENT/UDF и cold local reopen; completion сохраняет одну попытку.

Private control owner names refactored до event partition behavior;
`e4-control-names-refactor.log` 11/11 PASS. EventInvocation/EventOutboxRange теперь имеют
отдельный bounded inner map в existing control COW; primary и LIVE maps не копируются
при event lifecycle commits. Retired invocation keys удаляются вместе с terminal GC;
старые snapshots сохраняют revisions. `e4-event-revisions.log` 12/12 PASS.
`e4-control-resource-events.log` 63/63 PASS; `e4-pending-admission.log` 1/1 PASS проверяет
отклонение исходного CREATE при 10 000 pending jobs / 64 MiB без data effects.
`e4-uncertain-wal.log` 1/1 PASS содержит восемь source/completion cases до/после
append/flush/sync: recovery сохраняет identity и согласованность marker/data.
Нужны expanded correctness/fresh reviews, GQL job projection fixture и final
tooling/performance gates.

E5 Import: `ImportEventsMode::{Off, On}` подключён к SDK и GQL независимо от
CHANGEFEED. Accepted ASYNC payloads остаются в bounded private spool через segment
seals; savepoints восстанавливают len/bytes без сканирования, capacity остаётся
начисленной. END переносит current control metadata, повторно допускает pending
batch и публикует его вместе с data. Worker startup предшествует WAL; import lease
не допускает attempts до успешного END. SKIP/QUARANTINE/abort откатывают handlers и jobs.
`e5-expanded-import.log` 54/54 PASS после обновления двух старых fixtures: OFF feed
сохраняет suppression GAP, EVENTS=ON больше не считается unsupported.

Fresh E5 review: ACCEPT — outer item boundary подавляла dispatch вложенных
logical changes и теряла caller control до handler invocation. Regression gate
`e5-boundary-before.log`: 4 PASS / 2 FAIL, подтверждены default loop budget и неверный
CREATE context при последующем UPDATE/DELETE. Item savepoint теперь отделён от
statement mutation depth: Changefeed ordinal общий, Events dispatch — после каждой
операции, внутри её caller control; весь item сохраняет rollback boundary. Повторный
gate `e5-boundary-after.log` 56/56 PASS. OFF fast path сохраняет прежнее подавление
internal mutation boundaries и не добавляет root savepoint при ABORT. Fresh review
после исправления не выявило подтверждённых findings.

`e5-import-lifecycle.log` 62/62 PASS: GQL FOR iteration сохраняет отдельные Event
contexts и общий FULL Changefeed ordinal, откат отвергнутого item удаляет результаты
предыдущего успешного statement, failed END ничего не публикует, независимый KILL
сохраняется после END. Local Sync/NativeAsync reopen и четыре WAL interruption
points END сохраняют согласованные primary/sync effects и private spool.
`e5-event-resources.log` 79/79 PASS: global count/byte quota также отвергает IMPORT
source; private spool достигает 10 000 и отклоняет следующую операцию без effects.
write_set_bytes и admission теперь используют один retained-byte counter, включающий
payload/metadata/capacity через segment seals и item rollback.

E5 AtomWalk scout выявил необходимость RELATE reference endpoints, полной проверки
owner chain/terminal и remap каждого nested owner при replay. Сначала общий walk
вынесен из LevelPath/operation images в datastore::incidence; container child binding
имеет один GraphOccurrence accessor. До изменения поведения `e5-incidence-refactor.log`
9/9 и `e5-levelpath-refactor.log` 4/4 PASS. SDK nested-validation/remap regressions
`e5-qualified-before.log` 0 PASS / 2 FAIL подтверждают absent nested owner/terminal
и привязку к rival_leaf после конкурентного allocation. `e5-qualified-after.log`
11/11 PASS после полного remap каждого owner/terminal в его graph arena и
структурных GraphDefinition/Membership/Incidence prerequisites, включая Snapshot.
`e5-qualified-lifecycle.log` 4/4 PASS: depth 65, missing/non-container owners,
rebinding race и dangling endpoint в другом графе отклоняются до publication.

Fresh E5-Q1/Q2 ACCEPT / FIXED: InstallBuiltGraphTree включён в final validation;
для surviving external connectors сравниваются resolved logical owner/terminal
identities до/после изменения. BUILD overwrite не может воскресить прежний handle
для другого RecordId. Valid child replacement также требует явной connector repair.
`e5-qualified-identity.log` 6/6 PASS, включая empty/aliased BUILD overwrite и
detach/reattach repair. E5-Q3 ACCEPT / FIXED: qualified tracking admission включает
pending incidence/posting/schema cost до metadata/data changes. Node/edge cases
на quota boundary сохраняют topology, capability и write bytes;
`e5-qualified-resource.log` 7/7 PASS.

Для newly attached connector тот же контракт требует transient logical anchors
на границе операции, включая private import segments: previous-root comparison
не видит новые edges. `e5-qualified-pending-before.log` 0 PASS / 1 FAIL воспроизводит
alias нового connector при последующем owner rebinding в той же transaction.
Optional transaction journal сохраняет ordered (GraphId, RecordId) routes до
mutation, допускает copies/reserve вместе с pending source/structural metadata,
проверяет последний anchor на final candidate. Journal не сериализуется, ordinary
local-only operations его не аллоцируют; item/TRY rollback восстанавливает checkpoint,
import seals его не удаляют. `e5-qualified-pending-after.log` 8/8 PASS.
Fresh explicit-repair finding ACCEPT / FIXED: validated pending anchor служит
authority для existing connector, даже если два реальных topology changes вернули
прежний wire image. Equality fast-path остаётся no-op; для новой логической
привязки требуется реальная topology change либо detach/reattach. Gate
`e5-qualified-anchor-lifecycle.log` 10/10 PASS, включает private import seals.
`e5-qualified-reopen.log` 13/13 PASS: cold local Sync/NativeAsync guards сохраняют
выбранный endpoint, reject dangling/overwrite alias и сохраняют прежнее durable data;
savepoint удаляет pending anchor authority. Peak vector-growth admission учитывает
полный destination buffer при coexistence со старым.
BUILD installation также регистрирует anchors своих qualified connectors после
целого tree apply внутри прежней atomic boundary. Новый producer gate
`e5-qualified-build-producer.log` 13 PASS / 1 FAIL: fixture получает metagraph
«scope starts at non-meta atom» до ожидаемого commit rejection. Запущен
`e5-qualified-build-repro.log` с phase context; shared materialization path исследуется
без speculative patch. Root cause подтверждён: NodeView/EdgeView наследовали
ref-based is_meta, поэтому owned anonymous child без GraphRef ошибочно считался
обычным atom. Две concrete view overrides используют наличие owned child/transition;
lazy readers сохраняют прежний default. `e5-qualified-build-after.log` 14/14 PASS,
включая BUILD producer anchors и cold local guards. Отдельный generic anonymous
node/edge parity test добавлен, ещё ожидает focused запуск.

RELATE endpoint owner отделён от ATTACH/DETACH DmlTarget через AST и typed IR;
existing key evaluation, class validation, deterministic effects и planner demands
сохранены. Parser пока принимает прежние record endpoints. Gate
`e5-relate-owner-refactor.log` запущен до Reference/AtomWalk behavior.
Результат refactor gate: 5/5 PASS. После него добавлены Reference(Expr)/TypedExpr,
explicit AtomWalk node/edge wrappers и tilde directions; bare endpoint keys не
поглощают connector operators. Transaction-owned resolver принимает Record/Atom,
local occurrence и explicit descriptor `{ occurrence, owners }`, проверяет каждый
graph/id/kind и не переносит qualified references в connector graph автоматически.
Analyzer/IR/deterministic/planner demands используют общие operands; неизвестный
runtime reference не получает фиктивный static class. `e5-relate-references-check.log`
PASS, `e5-relate-references-tests.log` 5/5 PASS (VV/VE/EV/EE, directions, idioms,
bare numeric keys и existing RELATE context/reuse).
`e5-atomwalk-events.log` 17/17 PASS: EVENT использует обычный RELATE для всех четырёх
incidence; explicit owners сохраняют две разные routes к одному child edge.
`e5-atomwalk-async.log` 1/1 PASS для historical context на fresh worker transaction.
`e5-graph-dml-expanded.log` 12/12 PASS: wrong routes/kinds/depth и unattached edge
references отклоняются без target record/auto-attachment; existing family,
write-back и LevelPath contracts сохранены. Fresh Reference/AtomWalk review не
выявило подтверждённых findings. Наблюдение о lexical collection helpers снято:
нормативного запрета локальной LET mutation в endpoint expression не найдено,
запрет datastore writes сохраняется через существующий closure ceiling.
`e5-full-library.log` запущен с no-default-features + native-async-io,
test-threads=1. G0–G3 ещё не начаты; full backend/performance/tooling gates открыты.
Первый full library gate: 1271 PASS / 2 FAIL / 11 ignored. Оба failures — старые
reserved-tag fixtures: DataFileKind 0x18 и HistoryOwner tag 2 теперь EventOutbox.
Golden fixtures проверяют новые valid namespaces и следующий unknown tag 0x19/3;
production decoders не ослаблены. `e5-full-library-after.log` запущен после repair.
Повтор full library: 1273 PASS / 0 FAIL / 11 ignored (111.88 s).
RAM/local standalone SDK targets `e5-backend-integration.log`: 4/4 RAM и 16/16
local PASS, с native-async-io. Disk-space artifact failure не было.
Flat latest-anchor lookup отмечен shortcut; его fanout cost должен быть измерен.
Полные existence/identity scans до WAL требуют
отдельного performance evidence; регрессия без измерения не объявляется.
G0 начат после library/backend gates: behavior-preserving module move прошёл
`g0-module-refactor.log` (cargo check). Sealed LiveFamily сохраняет concrete COW
layouts и пары Frozen/Mutable для всех восьми families; mutable clones сохраняют
dirty metadata и RootedTree root. Existing LiveUpdate::new closure inference
сохранён; explicit families используют LiveUpdate::for_family.
Candidate state готовится до publication; pending/queue/base/dirty не теряются
при callback/rebase failures. Qualified runtime lookup учитывает node inner и
edge transition owners; owned validation проходит полный route и child graphs.
Focused `graph::live` gate: `g0-live-shared-dag.log` 19/19 PASS (0.03 s); начальные compile failures новых fixtures
(orientation names/trait bounds/Into inference) исправлены, production decoder
или family validator не ослаблялся. Fresh bounded read-only review: четыре
подтверждённых findings ACCEPT / FIXED. Discard draining сохраняет queue order;
frozen delta publication проверяет pinned base revision под write lock;
owned validation отклоняет logical containment cycles и допускает shared siblings.
Unique physical traversal + union logical links + checked Kahn исключают
экспоненциальный route expansion; 30-level shared DAG входит в passing gate.
Повторное review не обнаружило remaining blockers. Общий graph gate
`g0-all-graph.log`: 76/76 PASS. Compile-fail typestate example
`g0-typestate-doc.log`: 1/1 PASS; existing graph_core benchmark callers
`g0-benchmark-api.log` cargo check PASS. G1: общая selector-consumption boundary
для RETURN/BUILD/MATCH/TRAVERSE извлечена без изменения row evaluators;
focused pre-behavior gate `g1-selector-refactor.log`: 17/17 PASS. Terminal
SELECT поле проходит AST/analyzer/IR, обычный projected-value visitor и тот же
GraphAssemblyAccumulator; determinism/fingerprint/demand/copy admission/folding
учитывают selector children. EXPLAIN использует общий GraphAssemble wrapper.
`g1-select-terminal-check.log` cargo check PASS; focused ordinary terminal
tests в работе (первый compile failure fixture исправлен: runtime Array использует
iterator API). SDK logical graph recipe и LIVE graph evaluation/history ещё
не подключены; G1 не закрыт.
Первый runtime gate: 18 PASS / 2 FAIL. Scalar correlated fixture требует source
qualifier и исправлена без изменения ordinary SELECT semantics. Shared ROOT
finding подтверждён отдельным exact-selection repro `g1-explicit-root-repro.log`
(0 PASS / 1 FAIL): absent explicit root silently fell back to implicit root.
Общий finish_family теперь отклоняет unselected root; add_root отвергает NONE.
Обе формы проверяются у SELECT и BUILD через один validator. Gate
`g1-select-terminal-root-after.log`: 21/21 PASS. Ordinary projected window/group/JOIN,
all eight result families, endpoint closure, scalar subquery и shared root errors
проверены. Независимое bounded review ordinary path в работе. SDK graph recipe
и LIVE graph image/history/adapter остаются следующими срезами.
Ordinary-path fresh review: пять findings ACCEPT / FIXED в текущем code slice,
verification ещё идёт. Terminal outward schema — одна column `graph`, projected
schema сохранена для selectors; derived/BUILD проходят terminal path. Computed
primary scope разрешается и замораживается один раз с copy admission. VALUE object
сохраняет одну column. Direct graph assembly потребляет обычную retained-session
page, включая continuation/START AFTER; empty ONLY отличается от VALUE NONE row.
Nested EXPLAIN/derived cardinality учитывают GraphAssemble. Первый expanded gate
упал на compile fixture в cardinality branch (нужен Result); исправлен, gate
`g1-reviewed-ordinary-after.log` в работе.
Expanded runtime: 24 PASS / 1 FAIL; fixture replayed an exhausted final cursor,
which ordinary SELECT closes. Третья строка сохраняет live continuation при
replay. Deterministic scope observer использует direct RecordId field access,
без подстановки projection-only `properties` wrapper. Combined
`g1-ordinary-and-recipe-codecs.log`: 29/29 PASS (0.09 s). Source map retention
в partial hierarchy materialization допускает копии до allocation и сохраняет
qualified source identity без transaction/page pins; focused gate в работе.
SDK LiveGraphResult recipe добавлен (family/validators/ROOT/nodes/edges), GQL/SDK
syntax conversion и dependency traversal общие. FGLD v2 сохраняет graph-result
flag и bounded selectors; compatibility decoder v1 не добавлен. Graph evaluation
и owned durable graph payload ещё не подключены; G1 открыт.
G1–G3 не начаты; stage performance/resource/recovery/tooling gates не закрыты.
Последний check: C: 8 086 036 480 B, D: 48 018 808 832 B свободно;
disk-space build failure не было.

E2 framing diagnostic: первая trailing-count реализация дала catalog 12 PASS/7 FAIL:
existing malformed tests обнаружили сдвиг начала секций на 4 bytes. Count перенесён
в явный header; failed evidence сохранён в target/stage29/e2-catalog.log.

Этап 29 не завершён; этот журнал обновляется фактическим evidence.

### G1 — продолжение 2026-10-11

SDK graph evaluation использует тот же GraphAssemblyAccumulator после обычных
SELECT clauses и возвращает отдельный owned graph, без Value::Graph в row map.
Новый ordinary/SDK oracle проверяет восемь families, endpoint closure и
JOIN/SPLIT/GROUP. Первая SDK fixture SPLIT с literal была неверна; schemaless
property использует bound `properties.tags`, как действующий ordinary SDK.
Gate g1-sdk-integrated-tests.log:37 PASS /1 FAIL. Оставшийся failure регистрации
обнаружил qualified Literal/parameter в прежнем property codec; это исправляется
общим owned-value envelope FGLV v1 внутри FGLD v2. Scalar canonical bytes остаются
прежними; graph/closure runtime captures не получают нового StorageValue tag.

Source mappings сохранены также у global-node graph results. Candidate root
reader дополнительно наблюдает BaseAtom, GraphMembership, GraphOverlay,
GraphIncidence всех closure-required origin atoms, включая shared nested children.
GraphKindRange сам по себе не покрывает inherited base-property fanout другого
класса. Regression включает изменение edge properties вне primary person scan
и registration SSI. Fresh review обнаружил LIMIT-error cleanup и ONLY page
cardinality; оба используют общий retained page/cardinality path. Проверяются
удаление forced index между exact pages, original parameter frame, block closures,
READ COMMITTED, отказ admission и закрытие input token после invalid LIMIT.
Cursor-only closures сравнивают полный сохранённый source span; эквивалентное
переформатирование closure требует нового cursor (ограничение отмечено в коде).

G2 scout подтверждает необходимость initial candidate checkpoint handoff при
activation: registration stamp уже ACK floor и не может быть обычной delivery.
Graph data, включая пустой граф, должны иметь data footprint, а не control.
Retention/bootstrap обязаны сохранить checkpoint первого доступного interval;
row source-derived backfill для graph recipe теперь fail-closed/ineligible.
Graph publication/history/automatic SDK adapter пока не подключены. G1/G2/G3
и этап 29 остаются открытыми; correctness/performance/tooling closure не заявлен.

G1 gate после исправления qualified capture и cursor error paths:
`g1-owned-captures-and-cursor-errors3.log` — 41/41 PASS;
`g1-owned-full-descriptors.log` — 27/27 PASS;
`g1-all-live-lowering.log` — 51/51 PASS, 9.00 s.
ONLY review premise была отвергнута actual runtime evidence: initial ONLY не
создаёт cursor; valid cardinality и запрет смены identity проверяются отдельно.
Общий cardinality guard сохранён на retained graph page как invariant.

G2 owned images/capture начаты. Изображения не содержат local AtomId: root/source/
derived scopes, logical AtomRef и ordered qualified routes, named effective
properties, source ClassId/Versionstamp, selected property-key names. Общий
RootFields account допускает копии до allocation; source-backed children
записываются один раз. SDK oracle включает logical images всех восьми families.
`g2-image-capture-check2.log` PASS; `g2-image-capture-tests.log` — 41/41 PASS.
Canonical logical diff использует exact same_value для native representation,
сравнивает properties/topology/containment/source mapping и удалённые targets.
`g2-delta-check.log` PASS; `g2-image-delta-tests.log` — 42/42 PASS.
Graph durable codec/publication/checkpoint/retention/bootstrap/SDK adapter/G3 ещё
не реализованы. Fresh bounded review capture/diff в работе. Этап остаётся открыт.

G2 восстановление: family conversion вынесен из GQL в общий graph owner,
`MaterializedGraph::from_general`; ordinary semantics сохранены (43/43 PASS).
Owned restore строит scopes снизу вверх, nodes перед всеми edge reservations,
затем qualified topology; source maps и sparse property handles не требуют primary.
Root UUID/remote revision задаются явными LiveId/Versionstamp. Семь forest gates
прошли после исправления общей потери containment при conversion; meta owners
теперь отклоняются до копирования. `g2-restore-roundtrip-tests.log`: 43/43 PASS,
0.23 s, восемь families + fixed identity/revision + resource rejection.

FGLG roundtrip/framing/checksum/resource gate и structural owner/closure/cycle/
orphan validation: `g2-image-codec-validated-tests.log` 43/43 PASS, 0.18 s.
Graph LIVE publication/checkpoint/retention/bootstrap/SDK ACK adapter/G3 пока
не подключены. Этап 29 остаётся открыт.

G2 publication/history/checkpoint подключены через существующий LIVE transport:
FGLB v2 kind3 хранит полные FGLG before/after и проверяемый canonical diff.
Начальный image передаётся при activation отдельно от future deliveries.
Checkpoint reference находится в FGDC и публикуется одним WAL commit с primary;
полные images не помещаются в каталог. Общий retained cap — 256 MiB, отдельно
от scratch decoder. Recovery читает сохранённые страницы без SELECT primary.

Fresh review обнаружил и исправил phantom deltas от process-local revisions
после reopen, register+close в одной transaction без initial checkpoint и
расхождение cumulative caps publication/recovery. Source mapping теперь хранит
durable watermark принятого снимка; diff игнорирует только revision, сохраняя
проверку source ClassId, child и qualified routes. Flush обязан сравнивать
current effective image/owner routes в explicit transaction и использовать
обычные requirements/FCW/SSI.

Evidence: g2-graph-history-tests.log — 15 PASS; g2-reviewed-checkpoint-graph-tests.log
— 9 PASS; g2-reviewed-checkpoint-local-tests.log — 2 PASS (Sync/NativeAsync, каждый
с history Off/On, checkpoint/reopen/generation, closed-before-activation,
post-open unrelated DML без delta и graph history replay/ACK). Scoped nightly
formatting выполнен. Automatic LiveGraph SDK, resume/bootstrap, G3, расширенные
recovery/resource/performance/tooling gates остаются открытыми. Этап не завершён.

SDK G2 теперь выполняет automatic graph materialization и whole-delta ACK;
all-family SDK gate — 1 PASS (внутри все восемь families, update/resume/Mutable
transition/old guards). Bootstrap terminal cleanup — 1 PASS. Добавлено
executor-neutral cancellation-safe ожидание: один Waker slot на consumer,
deferred wake после publication mutex и импортного барьера, без runtime/deps.
Readiness gates — 1 PASS, после review — 2 PASS; selector syntax/hygiene — 1 PASS.

По уточнению пользователя normative contract теперь явно требует ветки по
образцу tokio::select!, включая отдельную conflict-ветку и ручное решение.
Точная старая переписка не восстановлена; прежние specs описывали conflict event
и resolution methods, но не branch interface. Скелет live_graph_select! и
next_event добавлен; Mutable conflict dispatch/resolution/flush ещё реализуются.
LiveGraphConflict — owned qualified payload; shared sparse-key capture и
field-level local replay выделены перед изменением mutable behavior. Этап OPEN.
