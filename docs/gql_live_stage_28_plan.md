# Этап 28 — LIVE SELECT, LIVE LET и Rust API

Статус: **в реализации, closure OPEN**. Контракт принят пользователем 2026-10-04.
Baseline: `196d21b0122dd1410752d04a37d7174b116ab228` (этап 27).
Зависимости: этапы 13 и 27; открытые performance/tooling gates предыдущих этапов
не блокируют focused correctness срезы. Events и LiveGraph относятся к этапу 29.

## 1. Семантика

LIVE SELECT работает над CLASS/record sources и graph-local qualified occurrences.
Containment не расширяет source. Обычные SELECT/FETCH, overlays, hyperedges,
edge-endpoints и cross-level references сохраняют текущую семантику.
WHERE, projection и внешний FETCH регистрируют все прочитанные зависимости,
включая отсутствующие записи, ranges, membership, schema и function versions.
Недоказанная точность reverse dependencies требует bounded conservative rescan.

Результат — net enter/update/leave за целый commit по before/after результата.
Данные и параметры одного commit учитываются совместно. Регистрация по умолчанию
начинает доставку после своего commit, без initial rows. SINCE включает catch-up.
Используется существующий commit_ts; дедупликация: LiveId + versionstamp + ordinal.
Доставка at-least-once, без разрезания commit. Silent truncation запрещён.

Обычный LET и execute parameters фиксируются по значению при регистрации.
ALTER LET меняет lexical binding, но не captures существующих подписок;
execute parameters immutable. Для обновления frozen captures нужна перерегистрация.
LIVE LET имеет отдельную versioned identity; зависимости связываются по identity,
а не имени. Его expression перевычисляется при изменении данных и LIVE LET;
обычные LET внутри expression фиксируются. Replace value устанавливает literal
expression и удаляет прежние зависимости. Зависимости образуют DAG; cycle
отклоняется до publication. Вычисление выполняется в топологическом порядке.
Create/replace/remove транзакционны, участвуют в savepoints, активируются при commit.
REMOVE LIVE LET с dependents получает dependency error.

Ошибки регистрации отклоняют регистрацию. Ошибка активного evaluator/FETCH/LIVE LET
или превышение его бюджета приостанавливает затронутые подписки с fault/GAP,
а исходный DML коммитится. Общие WAL/resource failures сохраняют атомарность.
Impure/nondeterministic evaluation и невосстановимые runtime-only captures запрещены.
PURE functions связаны с конкретными versions; несовместимая замена приостанавливает LIVE.

## 2. SDK и delivery

Самостоятельный публичный `live` содержит owned runtime types:
LiveQuery/LiveExpr/LiveProjection/LiveSource, LiveOptions/LiveSince/LiveOverflowPolicy,
LiveId/LiveBindingId/LiveCursor, LiveBatch/LiveEvent/LiveGap/LiveDelivery,
LiveInfo/LiveBindingInfo. Builders работают без GQL-текста и понижаются в обычный pipeline.
Datastore предоставляет subscribe_live/resume_live/reregister_live и binding operations.
Перерегистрация атомарно заменяет подписку новой identity.
LiveSubscription предоставляет id/next/ack/cursor/close; idle отличается от terminal.
Одна неподтверждённая delivery на handle; next повторяет её до ACK.
cursor — последний подтверждённый watermark; delivery несёт отдельный ACK token.
ACK проверяет database/query/version и выданную позицию; повтор принятого ACK идемпотентен.
Durable ACK сохраняется до освобождения queue. Drop отсоединяет consumer,
close прекращает подписку. Ожидание не удерживает transaction snapshot.
Value/RecordId/public graph identities не меняются; GQL id — UUID, cursor — Bytes.

Resume existing identity использует authoritative persisted ACK: устаревший
client cursor не откатывает watermark, позиция выше сохранённого ACK отклоняется.
Под commit mutex фиксируется cutoff и подключается tail; saved payloads до cutoff
декодируются лениво вне mutex. Scan/EOF не подтверждают delivery. Отдельный
prepaid historical head использует максимум queue bytes/entries и входит в общий
session budget; он не расходует quota pending live tail. После EOF/Drop/error
исторические pins и head holdback освобождаются. Typed materialization error
останавливает этот handle; новый resume повторяет чтение от durable ACK.

Режимы full/projected/VALUE/DIFF/PATCH сохраняют точные внутренние before/after.
PATCH — последовательные add/remove/replace с JSON Pointer (RFC 6901/6902).
JSON export допускает лишь совместимые значения без Number → f64.

## 3. Executor, история и publication

Один analyzer/typed IR/SELECT/FETCH evaluator через root-bound read adapter.
Обычная datastore transaction внутри commit evaluator запрещена.
Durable registry хранит versioned canonical logical descriptors, не executable IR.
С CHANGEFEED сохраняются вычисленные result images, binding revisions и whole-commit
envelope в private live payload существующих .fcf с отдельными catalog references.
SHOW CHANGES остаётся source history. Resume не FETCH-ит текущие primary records.
Без CHANGEFEED — transient queue; restart/queue loss обозначаются GAP.
SINCE не превращается молча в Now. Историческая новая регистрация требует доказанной
coverage всех inputs; недостаточная coverage — GAP перед доступным хвостом,
неизвестные authoritative bounds — typed error.

Prepare candidate root/bindings/live frames против captured root/registry revisions.
Полный набор BLOCK reservations получить вне commit lock; partial permits освободить
до ожидания. Под lock повторно проверить revisions; race освобождает permits и
запускает bounded retry. Primary/history/bindings/registry/bounds публикуются одним
WAL barrier. Под тем же lock коротко добавить admitted immutable handles в queues,
затем уведомить readers. После durability нет evaluation/allocation/capacity waits.
RAII освобождает permits при cancellation/timeout. Batch, не помещающийся в пустую
queue, вызывает pause/fault/GAP вместо бесконечного BLOCK.

Import lease допускает только узкие ACK/close control commits других writers.
END проверяет primary/schema/source capture revisions, сохраняет актуальный LIVE catalog,
назначает свежий финальный stamp и restamp-ит provisional versions/feed groups/metadata
до encoding. Это отдельный correctness gate. CHANGEFEED=OFF сохраняет suppression/GAP;
volatile LIVE может вычислить END net, но не создаёт durable backfill подавленного интервала.
Rename сохраняет identity; recreate не перепривязывает LIVE. Capture policy создаёт
явный durability epoch. Existing maintenance worker выполняет retention/retirement,
не delivery dispatch. Queues не durable; descriptors/captures/ACK переживают reopen.

## 4. Срезы и evidence

| Срез | Результат                                                         | Gate                                     |
|------|-------------------------------------------------------------------|------------------------------------------|
| L0   | Normative sync, baseline, rule matrix                             | cross-document contract                  |
| L1   | SDK identities/builders/options/descriptor и cursor codecs        | SDK/rustdoc, malformed frames            |
| L2   | Registry mutations/savepoints/session/activation/close/reregister | commit/abort, registration race          |
| L3   | Frozen captures, LIVE LET DAG/replacements/faults                 | chains/cycles/shadowing/TRY              |
| L4   | Root-bound SELECT/FETCH/dependencies/result net                   | independent SELECT oracle                |
| L5   | Durable computed result envelopes                                 | exact replay, continuation corruption    |
| L6   | Common publication/import/rebase/restamp                          | ACK/close during import, all generations |
| L7   | Queues/reservations/ACK/policies/shutdown                         | BLOCK races/cancel/order/resource        |
| L8   | Replay-to-tail/coverage/retention/pins                            | crash/reopen/epoch/GAP                   |
| L9   | GQL grammar → AST → analysis → IR → execution                     | parser/semantic/backend parity           |
| L10  | Explain/PlanHash and counters                                     | deterministic encoding/observations      |
| L11  | Full/backend/resource/performance/tooling gates                   | evidence report, no open gates           |

Следующий срез начинается после focused correctness gate предыдущего.
L1 coding RuleSet: api-newtype-safety (query/binding identities различны),
api-parse-dont-validate (validated cursor/options), api-builder-must-use (owned builders),
api-default-impl (утверждённые defaults), type-enum-states (idle/delivery/terminal),
type-result-fallible (invalid inputs — existing status errors).
Owner profiles: specs для L0, rust-api-design для SDK, rust-serialization для byte contract.

L3 ProfileStack: rust-database владеет transactional bindings/final replay images;
rust-refactoring — общей RootCatalog/root-bound SELECT поверх существующего executor;
rust-concurrency — optimistic preparation вне commit lock с bounded root retry;
rust-errors — fault boundary и code-generated FG-G1F09. Coding overlay:
type-result-fallible и api-newtype-safety. Конкретные owner constraints:
immutable Arc roots, admission до allocation и отсутствие user code под commit lock.
Новые зависимости, второй executor/worker/WAL не нужны: immutable roots, existing
SELECT/FETCH, shared historical codec и один publication barrier покрывают срез.

Binding scratch/work failures относятся к evaluation fault. Aggregate physical
catalog/WAL/resource failures относятся к общему atomic publication: такой commit
может быть отклонён целиком. Это разделение не ослабляет per-binding budget checks.

L4 ProfileStack: rust-database владеет before/after dependencies и net result diff;
refactoring — row-aware sink общего SELECT/FETCH без изменения обычных callers;
rust-api-design — owned aggregate identity. Пользователь выбрал поддержку верхних
агрегатов: Aggregate хранит canonical group keys; пустые keys обозначают один
graph/class-wide aggregate. Coding overlay: type-result-fallible,
api-newtype-safety. Conservative bounded rescan сохраняет зависимости WHERE=false,
missing FETCH и schema namespace до появления доказанного reverse lookup.

SDK scalar SELECT над graph source сохраняет ordinary cardinality/VALUE rules
при любом числе classes, включая ноль. Независимый вложенный query объединяет
induced qualified source до aggregate reduction. Пустой domain сохраняет стандартные
atom fields для typing; неизвестные declared fields не получают synthetic values.
Копии всего nested typed tree входят в scratch admission до optimizer clone.

L5 ProfileStack: rust-serialization владеет FGLB whole-commit result bytes,
historical dictionaries и checked continuation framing; rust-database —
отдельными LIVE catalog references и единой publication/recovery границей.
RuleQuery: private storage codecs, LE counts/ordinals, owned Value/Path и
qualified identities, Rust 2024/MSVC, общий WAL cap 256 MiB. RuleSet:
type-result-fallible (malformed bytes/resource failures без partial results),
api-newtype-safety (LIVE owner не подменяется ChangefeedId).
L5 выполняется двумя focused gates: exact computed envelopes, затем shared
segment transport/catalog integration. Новая сериализационная dependency
не нужна: existing CanonicalCodec сохраняет scalar representations;
explicit LIVE wrappers сохраняют runtime graph references без storage DTO наружу.

L5 diagnostic sub-slice: rust-errors владеет восстановлением declared status
с typed parameters и cause chain; rust-serialization — bounded bytes.
Перечень statuses остаётся единственным источником code/parameter schemas:
restoration и pre-allocation measurement генерируются define_status_codes,
hand-written status variants и lossy summary не вводятся. Technical sources
не сериализуются; diagnostic locations/phase/extensions сохраняются логически.
При превышении control capture budget публикуется отдельная typed диагностика
невозможности сохранить исходную ошибку; это не отменяет source DML. Initial
result restorability проверяется при регистрации и перед pending activation,
без выдачи initial rows. rust-macros владеет generated reverse construction и
checked parameter measurement из тех же declarative status schemas.

L7a TaskBrief: отдельный whole-commit queue state перед подключением publication.
Owner `rust-concurrency`: issuance, net queue successor, GAP/DISCONNECT/BLOCK;
coding `rust-ownership`: owned immutable payload handles и отдельный issued slot.
RuleQuery: private Queue/Entry/Footprint, VecDeque/Arc, Rust 2024 / 1.98.1,
RAM/Windows Sync/NativeAsync, existing parking_lot; правила `own-arc-shared` и
`own-mutex-interior`. Public API, ACK replay и store reservations — следующие
decision units; они не входят в L7a foundation evidence. Coverage gaps: none
для очереди; focused gate проверяет forged ACK, повторную delivery, issued GAP
с pending GAP, whole-commit overflow и отсутствие изменений при failed admission.
При подключении RAII permit Drop не берёт commit mutex; registry operations
используют порядок commit → queue. Отдельный issued slot допускает issued control
и один pending control одновременно. Universal no-allocation-after-WAL gate
остаётся OPEN до аудита существующих WAL version maps и generation retirement.

L7b ProfileStack: `rust-concurrency` владеет atomic fanout reservations,
commit/queue revision handshake и BLOCK; `rust-database` — durable ACK replay
и import control commits; `rust-api-design` — owned pull handle с Weak origin.
Coding profiles: `rust-ownership`, правила `own-arc-shared`, `own-mutex-interior`,
`api-must-use`, `api-newtype-safety`, `type-result-fallible`. Reuse fit: existing
parking_lot Mutex/Condvar выражают один capacity predicate без нового dispatcher;
VecDeque удерживает canonical commit order; Arc делит immutable payloads.
Existing QueryCancellation используется также CommitOptions, поэтому statement,
BLOCK и import END имеют один cancellation/deadline boundary.

Во время replay/ACK capacity не освобождается до общей durable publication.
Pending registration заранее attached, но активируется вместе с definition.
RAII permits освобождают partial preparation и retries без commit mutex.
Две 64-KiB control images (issued/pending) и pending control Entry оплачены
при attachment; data entries не могут использовать reserved control budget.
Общий 256-MiB retained ceiling применяет GAP/DISCONNECT/BLOCK, а не отклоняет
source DML независимо от policy. Session pressure сохраняет fault diagnostic
и unknown authoritative GAP bounds.

WAL visibility maps являются RapidHashMap, а не BTreeMap. Capacity maps,
existing/new version vectors и publication buffer допускаются до append/barrier;
prune gate фиксирует reserved vectors в порядке prune → writer → published.
Backend COW/empty history Arcs и generation retirement queue подготовлены до WAL.
При LIVE cleanup выполняется перед следующей admission и existing idle maintenance,
с bounded aggregate paths/markers/retained Vec capacity; между successful WAL
и queue handoff нет sweep, user evaluation или capacity wait.
Это focused publication contract; общий performance/tooling closure остаётся OPEN.

L8 TaskBrief: lazy saved-result replay до fixed cutoff, затем admitted live tail.
Reader не выполняет FETCH current primary и освобождает pins при EOF/Drop.
При resume persisted ACK является authority; replay-head capacity должна быть
отделена от tail, иначе BLOCK может препятствовать чтению старого head.
Новая регистрация SINCE и историческая source/dependency coverage — отдельный
decision unit от existing-incarnation replay. LIVE TTL/floors используют тот же
maintenance worker, но отдельные namespace references.

L8a/b ProfileStack: rust-concurrency владеет fixed-cutoff attachment, head/tail
admission, issuance/ACK/close/shutdown races; rust-database — saved-result replay,
capture epochs и bounded prefix purge; rust-serialization — consumer control
admission поверх неизменного FGLB/FCFS codec. Rust API использует существующие
owned runtime identities; ownership rules own-arc-shared/own-mutex-interior и
api-newtype-safety/type-result-fallible остаются активны. Новых dependencies нет.
Fault/GAP decode имеет отдельный 64-KiB limit с aggregate page admission;
oversized persisted diagnostic не получает бесплатный control slot.

TTL saved group определяется captured policy соответствующей durability epoch.
Для full-segment shortcut maximum TTL вычисляется один раз на query с charged,
cancellable epoch scan; partial traversal проверяет каждую complete group.
Полностью прочитанный partial segment удаляется из references, partial ordinals
сбрасываются, stamp/time floor сохраняется. Empty history не стирает purge floor.
Closed descriptors остаются addressable; physical generations освобождаются
через существующий pinned retirement механизм. Fresh registration stamp/timestamp
SINCE и доказанная source/dependency reconstruction остаются отдельным L8c gate.

L8c ProfileStack: refactoring владеет переносом existing source walker и сохранением
public borrowed API; rust-database — coverage proofs и historical result construction;
rust-serialization — отдельной нижней позицией подтверждения и backfilled groups
в общем catalog/WAL. RuleQuery: own-arc-shared для immutable root/history pins,
type-result-fallible для checked images/coverage; нет trait/factory второго executor.
SourceReader исполняется только в bounded preparation; ожидание delivery по-прежнему
не удерживает primary root. Первый result slice использует CLASS/record row-local
images через обычный SELECT; FETCH, aggregates, graph scopes и LIVE LET расширяются
после его gate, без изменения общего согласованного объёма этапа.

L8c aggregate reconstruction использует complete source domain, а не сумму delta
rows: неизменившиеся members обязаны участвовать в ordinary aggregate evaluation.
Пустой creation anchor требует INITIAL schema, capture с creation commit и полный
доказанный prefix. Перед применением whole NET сравниваются exact before images;
cutoff primary может проверять полноту, но не заполнять отсутствующую history.
Отсутствующий anchor/coverage обозначается GAP; named aggregate identity сохраняется
в computed history. Retained source images, временные roots/results и reader work
оплачиваются вместе до allocation/read, временная SQL scratch переиспользуется.
Initial SDK registration reader готовится до publication и сохраняет оплату своих
private encoded buffers до EOF/Drop, в том числе при external CLOSE.
SourceReader EOF сверяется с authoritative source last_stamp независимо от числа
выданных NET rows: возврат cutoff images к прежним значениям не доказывает полноту
пропущенного хвоста. Purged floor и полностью прочитанные empty NET groups учитываются.
Retained epoch capacity допускается до allocation, вместе с остальной history metadata.
Последовательные временные dictionary contexts shared root-builder получают одну peak
reservation на evaluation; retained row bytes и work каждого clone учитываются отдельно.
Complete-domain consumer также обслуживает FETCH/record navigation внутри того же CLASS.
Перед historical evaluation bounded proof проверяет references в literal/frozen values
и captured images, включая отсутствующие targets. Foreign/graph references требуют
дополнительных input domains, их нельзя заполнять current primary. Actual recorded
reads повторно проверяются против доказанных identities/ranges после ordinary execution.

L9a ProfileStack: specs владеет LIVE syntax/explicit unsupported boundaries;
rust-architecture — AST → logical recipe → existing typed SELECT / transaction adapter;
rust-concurrency — pending attachment до WAL и session exposure после commit;
rust-api-design — session pull access без изменения Value/CommitResult.
RuleQuery: type-result-fallible для fallible commands, own-arc-shared для retained
immutable delivery. Command templates остаются crate-private; session handle
не раскрывает storage/queue types. Frozen captures читаются из runtime slots в момент регистрации,
а не из analysis-time parameter Object. Session handles допускаются до consuming
commit и освобождаются при rollback. Reactive lexical identity и ACK validation
расширяются отдельным следующим срезом; неподдержанный syntax не отбрасывается.

L9 control ProfileStack: rust-concurrency владеет узким import control preflight
и rollback; rust-ownership — catalog-only snapshot lifetime и metadata overlap;
rust-api-design — fallible INFO/cursor/closed identity semantics. Ранний
cancellation check применяется к metadata-only командам; обычный SELECT должен
достичь собственного cursor cleanup, если caller уже отменён.
Lexical ALTER LET использует собственный typed slot IR, а не schema mutation.
Specs владеет scope/mutability/type contract; rust-ownership — заменой значения
и TRY rollback; existing expression executor — единым вычислением replacement.
Execute parameters и внешние immutable captures сохраняют исходные правила scope.
Type-fix ProfileStack: specs владеет retained annotation и dynamic assignment
contract; rust-ownership — immutable lexical metadata/session export/savepoints;
rust-errors — проверкой recursive members до slot write в normal/PURE evaluators.
Types остаются private metadata, публичный Value не меняется. Shared rich-type
walker допускает clone, retained metadata ограничена 256 MiB; session export
подготовлен до consuming commit. Verification/re-review принадлежат rust-verify
и rust-review. Изменения reactive frontend до закрытия этого gate отложены.

ACK frontend использует checked session attachment authority: UUID/cursor сами
по себе не доказывают выдачу delivery. Session poll заимствует retained consumer;
после take_live_subscription ACK выполняется извлечённым Rust handle. GQL ACK
staging не открывает вложенную transaction. Private key+nonce/token witnesses
участвуют в savepoints, проверяются против retained session handles до COMMIT и
registry перед WAL. Queue release остаётся existing publication; fixed cursor
cache обновляется только после успешного commit. No-op ACK не создаёт capture
intent; mixed primary mutations сохраняют writer lease. ACK ProfileStack:
rust-concurrency — issuance/ownership/commit races; rust-ownership — capability
lifetime/savepoints; rust-api-design — session polling/typed control API;
rust-serialization — existing fixed cursor codec; rust-verify/rust-review — evidence.

ACK review-fix ProfileStack: rust-concurrency владеет normalized issuance evidence
(confirmed watermark либо единственный issued token на attachment) и repeated
validation; rust-architecture — существующим QueryRead seam после typed analysis
для write access только implicit session transactions. Explicit READ ONLY и SDK
transactions не повышаются. Specs владеет CURSOR expr/read-only contract;
rust-ownership — bounded RootRead evaluation и SSI observer без новой transaction.
RootRead остаётся общим executor для scalar cursor и record navigation;
SELECT subquery вне SELECT сохраняет существующую semantic error.
Append-only normalized witnesses сохраняют savepoint truncation.
Actual runtime closure effect повторно проверяется перед inferred promotion;
вложенные language callbacks наследуют ceiling declared PURE/STABLE/IMPURE body.
Это не позволяет STABLE wrapper скрыть mutating callback за type any.

L9b ProfileStack: specs владеет lexical reactive flavor и identity captures;
rust-ownership — immutable BindingTypes metadata, frame/shadowing/savepoint
lifetime и borrowed snapshot value admission; rust-database — controlled SDK
binding operations и SSI reads существующего каталога. RuleQuery для
slot/recipe boundary выбирает `own-borrow-over-clone` (lookup до clone) и
`own-clone-explicit` (owned recipe/result после admission). Одна текущая фаза
требует трёх owner units: syntax contract, lexical ownership и existing registry
publication. Physical codecs/queue delivery не меняются.
Reactive slot хранит UUID identity; value type Any, read effect StableRead.
Closure захватывает identity по значению; invocation читает binding из своего
snapshot. Ordinary shadowing маскирует reactive flavor; execute parameters не
интерпретируются как reactive identities. Проверяются scalar/SELECT/FETCH,
query input/options, closures, replacement, cycles, TRY/CANCEL и reopen.

L9b core реализует LIVE LET/ALTER/INFO/REMOVE, frozen ordinary captures,
identity-bound closures и statement snapshot/SSI reads. SINCE/BUFFER
фиксируются при registration. Полнота query inputs пока OPEN: LIMIT/START,
search operands, MATCH keys/deferred GRAPH/MAXDEPTH, TIMEOUT/admin и
SHOW CHANGES требуют отдельного среза; general recipes/scoped sources также OPEN.
Helper addressing-findings сопровождает замечания, rust-review проверяет closure;
metadata guard остаётся жив вместе с INFO vectors, resource limits REMOVE
применяются к GQL staging. Commit replay сохраняет existing transaction budget.
Reactive scalar copies оплачены в shared statement pool; выбранная owned
array/object cell перемещается без третьей копии. Ordinary cast formatting
сохранён; transient renderer scratch допускается консервативно до allocation.

| Правило L9b                                            | Проверка                                                          |
|--------------------------------------------------------|-------------------------------------------------------------------|
| frozen ordinary capture / identity closure / shadow    | live_gql_binding_chain_replacement_and_closure_identity           |
| cycles / TRY / explicit CANCEL / dependency removal    | live_gql_binding_cycles_savepoints_and_removal                    |
| retained INFO vectors/output overlap                   | live_binding_info_retains_vector_admission                        |
| typed paused diagnostic / old snapshot                 | live_gql_binding_paused_diagnostic_and_old_snapshot               |
| caller cap REMOVE / atomic rejection                   | live_gql_binding_remove_catalog_budget                            |
| value/builtin/cast/index admission / immutable binding | live_gql_binding_value_admission_and_readonly                     |
| registration-time SINCE/BUFFER                         | live_gql_binding_registration_options                             |
| grammar boundaries / UTF-8                             | live_let_parser_lifecycle_and_boundaries                          |
| durable identity / parameter-only delivery / reopen    | live_gql_binding_local_sync / live_gql_binding_local_native_async |

L9b pagination TaskBrief: текущий следующий срез — LIMIT и START OFFSET
обычного SELECT, включая nested/derived SELECT и invocation captured closure.
Owner rust-ownership владеет fixed-snapshot resolver lifetime и admission before
copy; существующие checked Number conversions сохраняются. RuleQuery использует
own-borrow-over-clone и type-result-fallible для borrowed RecordFields и fallible
usize operand. Существующий eval получает resolver через mandatory argument;
новые executor/registry/cache отсутствуют. Готовность проверяется прямым SELECT,
переиспользованной closure после ALTER, invalid integer/paused binding и прежними
cursor cancellation tests. START AFTER cursor value, search и MATCH seams
в этом срезе отложены; их presence в общем плане не расширяет текущий diff.
Coverage gaps: none для LIMIT/offset signature и названных consumers.

Pagination focused gate PASS: прямой/closure/derived/nested SELECT после ALTER,
checked negative/non-number inputs и прежний cursor cancellation. Compile-check
PASS; Clippy delta 0 при неизменном global baseline 6961 errors.
Следующий TaskBrief — record-key selectors MATCH: seed, fixed/path expansion,
HYPER CONTAINS и WITHIN. Owner rust-ownership сохраняет borrowing существующего
MatchPredicateContext/MatchGraphFields в одном snapshot; own-borrow-over-clone,
type-result-fallible применяются к обязательному resolver и checked RecordId.
Новые planner/API/codec contracts отсутствуют. GRAPH/MAXDEPTH остаются отдельным
IR-срезом; coverage gaps none для общего selector resolver и его callers.

MATCH selector focused gate PASS для seed/fixed/path/HYPER/WITHIN после ALTER;
check PASS, Clippy delta 0. Следующий TaskBrief — statement-fixed Fulltext/KNN
inputs. Owner specs определяет deferred runtime validation для Any query operands:
Fulltext проверяет string/array<string>, KNN — exact vector element kind;
K/EF сохраняют positive integer validation. Numeric MINKOWSKI parameter допускает
STABLE reads из invocation snapshot; executable distance callback остаётся PURE.
Owner rust-ownership сохраняет borrowed RecordFields в prepare_search_inputs и
его реальных consumers, с общим admission до clone. RuleQuery: own-borrow-over-clone,
type-result-fallible. Новых IR/public/storage типов нет; broader LIVE recipes,
dynamic GRAPH и TIMEOUT не входят в diff. Coverage gaps none для этих operands.

Observed trigger L9b-SEARCH-QUERY-ADMISSION: query token/filter/term allocations
не попадали в input value accounting. Helper addressing-findings применён к
accepted finding; существующие bounded analyzers получают snapshot fields,
shared scratch/work callback и admission term insertion. PURE UDF argument copy
допускается до allocation. Per-document search path сохраняет прежнюю policy;
она не входит в этот finding. Fresh re-review PASS; builtin scratch/work и UDF
work rejection проверяются на empty class, до source rows. Helper снят после
проверки; broad resource/performance gates остаются OPEN.

Следующий L9b TaskBrief — INFO CHANGEFEED и SHOW CHANGES operands. Owner
rust-ownership владеет borrowed reactive lookup внутри scalar-only Operands и
переносом owned BarrierAccount за пределы lookup scope: admission остаётся
активным при последующем history reader/metadata projection. Owner rust-database
владеет точным QuerySnapshot и SSI LiveBinding read через существующий adapter.
RuleQuery: own-borrow-over-clone, own-lifetime-elision, type-result-fallible.
INFO id, source, SINCE и LIMIT читают текущие identity values; direct primary
record navigation в history operands по-прежнему отклоняется. EXPLAIN сохраняет
deferred scalar policy и не выполняет UDF. Новые API/formats/worker отсутствуют.
Acceptance: class/id parity, parameter replacement, old snapshot, tiny cap и
существующие changefeed regressions. Coverage gaps none для названных consumers.

History operand gate PASS: 91 changefeed tests, включая новый LIVE LET test,
сохраняют depth64 и typed Path/Geometry/Error preflight. Используется existing
image walker с optional per-step caller work/cancellation hook; output walker
сохраняет прежнюю policy. Общий runtime walker не заменяет history depth contract.

Следующий L9b TaskBrief — owned Array/Object и numeric unary recipes. Owner
rust-api-design владеет самостоятельными LiveExpr variants без syntax/IR types;
rust-serialization — explicit FGLD tags и bounded canonical codecs;
rust-ownership — borrowed lowering, captures и admission до collection allocation.
RuleQuery: own-borrow-over-clone, type-result-fallible, api-newtype-safety.
Q-40 сохраняет development format на месте; compatibility decoders отсутствуют.
Готовность: SDK без query text, GQL parameter chains и source projection,
canonical roundtrip/golden tags, malformed count/depth/resource rejection и
local Sync/NativeAsync reopen. Составные literals не заменяют executable IR;
general SELECT clauses и dynamic sources остаются отдельными срезами.

Observed trigger L7-ACK-STATISTICS: reopened root сохраняет deferred statistics;
ACK пропускает hydration по control contract, но catalog-only publication удаляла
его physical binding. Следующая mutation ошибочна. Helper debugging локализует
общий persist_graph_catalog; owner rust-database сохраняет statistics binding,
так как catalog-only path не переписывает statistics files. Rust-concurrency
сохраняет отсутствие новых allocations/evaluation после WAL barrier. RuleQuery:
own-borrow-over-clone, type-result-fallible. Reopen→resume→ACK→mutation становится
обязательным backend regression; обход через предварительную hydration в клиенте
не допускается. Whole generation path по-прежнему сбрасывает прежний binding.

Array/Object/unary focused gate PASS: 175 LIVE tests, включая исторические
compound projections, canonical golden tags и checked SDK work rejection;
полный GQL gate — 285 passed / 4 ignored. Rustdoc examples — 2/2,
local SDK reopen/resume/ACK/mutation — Sync 1/1, NativeAsync 1/1.
Compile-check и scoped nightly format PASS; Clippy delta 0 при global baseline
6961 errors. Fresh reviews закрывают codec golden finding и L7-ACK-STATISTICS.
Следующий срез начинается после этих проверок; общие L8–L11 gates остаются OPEN.

Следующий L9b TaskBrief — owned field access и indexed access recipes.
Owner rust-api-design владеет самостоятельными Access/Index без syntax/IR types;
rust-serialization — explicit FGLD tags 12/13 и checked null-safe flag;
rust-ownership — borrowed children/captures, snapshot reads и admission до copies.
RuleQuery: own-borrow-over-clone, type-result-fallible, api-newtype-safety.
Qualified row aliases разрешаются прежним schema.qualified_index; property/record
navigation, null-safe access и indexing исполняет общий evaluator. Historical
row-only gate остаётся conservative для Access; complete-domain gate принимает
только доказанную coverage children/captures и recorded reads. Проверки: GQL
ordinary/frozen/reactive equivalence, qualified/null-safe row paths, wrong types,
cycles и external dependency mutation; SDK builders/reopen без query text;
explicit tags/flags, malformed lengths/depth и budget rejection. General SELECT
clauses, cast/conditional recipes и cross-source backfill не входят в этот срез.

Observed historical trigger: unconditional Access=false требовал complete-domain
anchor даже для `{value: age}.value`, хотя этот constructor гарантирует Object
и не выполняет record lookup. Row-only proof допускает Access непосредственно
над Object constructor после recursive проверки всех members; остальные bases
сохраняют conservative rejection. Это не ослабляет isolated/read whitelist.

Access/Index focused gate PASS: LIVE 178/178; GQL 287 passed / 4 ignored;
SDK local Sync/NativeAsync 1/1 каждый; Rustdoc 2/2. Compile-check, scoped nightly
format и fresh source re-review PASS; Clippy delta 0 при 6961 baseline errors.
43 unrelated baseline files сохранены. Graphify retry PASS; initial signature
failure остаётся в evidence ledger, tooling closure требует общего gate.

Следующий L9b TaskBrief — START AFTER cursor operand обычного SELECT. Owner
rust-ownership сохраняет mandatory statement snapshot fields и общий control
в existing evaluator; RuleQuery own-borrow-over-clone/type-result-fallible.
Reactive conditional выбирает из session-owned ordinary cursors; cursor remains
runtime-only и не допускается durable LIVE LET capture. Тест проверяет выбор
разных replay positions после parameter-only mutation, ordinary continuation
replay/cancellation и fail-closed durable capture. Новые recipes/codecs/registry
или cursor ownership protocol не вводятся. Остальные input seams отложены.

Observed trigger L9b-CURSOR-EARLY-CANCEL: controlled eval может прерваться до
получения cursor value и обходить прежний discard. Before evaluation сохраняется
borrowed direct Slot/Constant input; любой eval failure освобождает этот spool.
Computed selection не выполняется повторно при ошибке; после успешного выбора
сохраняется existing cancellation/limit cleanup. Helper addressing-findings
сопровождает regression cancellation_closes_the_supplied_cursor_spool.

START AFTER focused gate PASS: GQL 288 passed / 4 ignored; check и scoped
nightly format PASS, Clippy delta 0 при baseline 6961 errors. Initial cancellation
regression и Graphify signature failure сохранены в evidence, stage closure OPEN.

Следующий L9b TaskBrief — deferred MATCH MAXDEPTH. Owner rust-ownership сохраняет
borrowed logical patterns без clone; specs владеет различием open range и explicit
maximum. Analyzer удерживает typed lexical operand; invocation разрешает его через
тот же snapshot/control до planning и row pull. Runtime CBO и cursor используют
одно validated значение; direct EXPLAIN показывает этот bound. Outer embedded
estimates до invocation могут использовать checked upper bound 30. RuleQuery:
own-borrow-over-clone/type-result-fallible. Focused gate проверяет ordinary/live
LET, open/explicit/zero ranges, invalid values, old snapshots и embedded MATCH.
Deferred GRAPH и general TIMEOUT/admin inputs остаются отдельными срезами.

Observed trigger L9b-MAXDEPTH-EMBEDDED-WARNING: GraphResult EXPLAIN показывал только
root warnings и терял child warning о conservative upper-bound estimate. Общий
plan_warnings recursively проецирует source warnings для JSON/text без clone
logical patterns или изменения child plan. Regression — EXPLAIN BUILD GRAPH с
deferred MATCH source; finding принят, fresh re-review обязателен.

MAXDEPTH correctness gate PASS: final focused 1/1, GQL 289 passed / 4 ignored.
L9b-MAXDEPTH-EMBEDDED-WARNING закрыт fresh source re-review; initial fixture
failure (scalar expectation для tv::count array) отделён от passing evidence.
Open/explicit ranges, zero, invalid values, old snapshots, embedded MATCH и direct
EXPLAIN/PlanHash проверены. Scoped nightly format девяти Rust files PASS;
check PASS, Clippy delta 0; Graphify результат фиксируется в evidence отдельно.

Следующий L9b TaskBrief — owned IF recipe. Owner rust-api-design сохраняет собственные
runtime-типы; rust-serialization владеет checked explicit tag 14 и ordered children;
rust-ownership — borrowed lowering/capture/dependency walkers. RuleQuery:
api-newtype-safety/own-borrow-over-clone/type-result-fallible. IF lowering использует
ordinary Conditional analyzer/evaluator (truthiness и short-circuit), без нового
executor. Все три children проходят bounded admission и capture validation, в том
числе inactive branch; исторический fast path требует recursive proof всех children.
Focused gate: exact numbers, frozen/reactive captures, branch-dependent reads,
inactive branch failures/effects/cycles, golden codec, history и SDK reopen.
CASE/Cast/Set и dynamic scope остаются отдельными срезами.

IF correctness gate PASS: focused 9/9, LIVE 190/190, GQL 290 passed / 4 ignored;
SDK reopen→resume→ACK→branch switch PASS на отдельных Sync и NativeAsync targets.
Rustdoc 5/5, check и scoped nightly format PASS; Clippy delta 0, global debt OPEN.
Fresh source reviews отдельно проверили API/codec/captures и historical proofs;
branch-dependent record reads, inactive cycles/effects и fixed golden проверены.

Следующий L9b TaskBrief — deferred MATCH GRAPH. DesignBrief discovery: текущий
analyzer читает raw variables; runtime consumers включают standalone/embedded MATCH,
BUILD/RETURN GRAPH, traversal source и EXPLAIN. Owner rust-ownership удерживает
LogicalGraphReference и borrowed patterns; specs владеет GraphRef-only параметром
и snapshot identity validation; rust-observability — resolved graph в PlanHash/EXPLAIN.
RuleQuery: own-borrow-over-clone/type-result-fallible/api-newtype-safety.
Same-phase reason третьего owner: изменение IR graph reference и physical fingerprint
должно одновременно сохранить binding/hash/EXPLAIN identity; разделение оставит
одинаковые hashes для разных graphs с одинаковыми counts. Поля GraphId/binding отражаются
только в явном EXPLAIN, не в unbounded metric labels или user payload logs.
Runtime разрешает reference через statement fields/SSI до demand/planning/graph open/row pull.
Outer embedded planner до invocation использует unknown cardinality и warning без
фиктивного GraphId или чтения всех графов. Acceptance: ordinary/live LET, same-batch
LET, old snapshots, invalid/stale references, embedded traversal/graph assembly,
EXPLAIN binding/hash и existing MATCH graph-family suites. General dynamic LIVE
source descriptors, TIMEOUT/admin inputs и CASE/Cast/Set остаются отдельными срезами.

MATCH GRAPH focused correctness gate PASS: resolved ordinary/live LET, same-batch
LET и parameter-only replacement, old snapshot при dependency DML, tv::match,
BUILD/RETURN GRAPH, traversal analytics и stale name recreation. GraphId различает
physical hashes одинаковых topology counts. L9b-GRAPH-TENSOR-DEFERRED принят:
unbound ALL SHORTEST не получает ложный ROOTED/ON_DEMAND, metadata DYNAMIC;
regression и fresh source re-review PASS. Final GQL 291 passed / 4 ignored,
LIVE 191/191, check/scoped format PASS; Clippy и Graphify evidence фиксируются в report.

Следующий L9b TaskBrief — owned Set recipe. Owner rust-api-design сохраняет отдельный
runtime constructor, rust-serialization — explicit tag 15 и checked ordered element
list; rust-ownership — borrowed lowering/walkers. RuleQuery:
api-newtype-safety/own-borrow-over-clone/type-result-fallible. Ordinary Set analyzer
и evaluator владеют deduplication/equality, без нового контейнера или executor.
Списки элементов используют существующие Array admission/work/codec paths до allocation.
Captures/dependencies/history proof обходят все elements. Acceptance: SDK без GQL,
frozen/reactive equality и duplicates, ordinary SELECT differential, fixed golden,
malformed count/resource rejection и historical replay. CASE/Cast и general dynamic
sources остаются отдельными срезами, performance L11 не запускается в этом срезе.

Owned Set focused gate: 11/11, включая historical 17→19 при current 99,
ordered/duplicate recipe golden и malformed count/child rejection. Fresh read-only
source review PASS; общие LIVE 193/193 и GQL 292 passed / 4 ignored PASS.
SDK Sync/NativeAsync separate targets 1/1, rustdoc 5/5 и check/scoped format PASS;
Clippy added instances 0 при общем baseline FAIL. Evidence — в report.

Следующий L9b TaskBrief — owned CASE recipe. Owner rust-api-design сохраняет
optional operand, ordered непустые condition/value branches и optional ELSE;
rust-serialization — explicit tag 16, option flags 0/1, checked count и все children;
rust-ownership — bounded borrowed lowering и полный capture/dependency/history proof.
RuleQuery: api-newtype-safety/own-borrow-over-clone/type-result-fallible.
Переиспользуется ordinary Case analyzer/evaluator: simple operand вычисляется один
раз, first matching branch определяет результат, absent ELSE даёт NONE. Все branches
проходят registration validation независимо от runtime selection. Acceptance:
searched/simple CASE, frozen/reactive captures, inactive failure/effect/cycle,
SDK reopen/resume/ACK, fixed golden, zero/oversized count и unknown flag rejection,
historical before/after без current primary reads. CAST/type-name admission,
general dynamic sources и L11 остаются отдельными decision units.
CASE resource finding L9b-CASE-NULLABLE-TYPE принят: shallow nullable branches
увеличивали inferred Type depth через общий unify. Focused compiler regression
воспроизводит Option^192<Int> вместо Option<Int>. ProfileStack owner rust-errors
сохраняет bounded compile contract, addressing-findings — shared symmetric fix
и fresh re-review. Nullable widening оставляет уже Option type; CASE/Array/Set и
остальные реальные unify callers проходят общий GQL gate. Runtime values и
explicit declared nested types не нормализуются этим исправлением.
Первый consecutive-nullable fix прошёл focused 14/14, но fresh re-review выявил
alternating Int/NONE residual через Union<Option<T>, T>. Finding остаётся OPEN;
следующая shared правка унифицирует optional inner types с одной nullable оболочкой
и проверяется CASE/Array/Set alternating regression до широких final gates.
Final shared fix унифицирует optional inner types и повторно использует одну
nullable оболочку; alternating repro FAIL сохранён. Final focused 14/14, 0.22s;
96 Int/NONE/NULL triples, обратный порядок, declared option<int> closure/PURE fn
arg/result и настоящие LIVE LET проходят. Fresh source re-review закрывает
L9b-CASE-NULLABLE-TYPE; остальной bounded CASE slice source PASS. Final LIVE 196/196,
GQL 294 passed / 4 ignored, SDK Sync/NativeAsync separate targets 1/1 и rustdoc 5/5
PASS; check/scoped format PASS, protected 43/43 unchanged. Global Clippy baseline
FAIL: 6959 errors / 546 warnings, added instances 0. Graphify retry PASS:
57650 nodes / 154996 edges / 1924 communities. Evidence — в report.

Следующий L9b TaskBrief — owned CAST recipe. Owner rust-api-design: symbolic
target Text и owned child, аналогично существующему named Call; ordinary type-name
analyzer/evaluator остаётся единственной semantic authority. rust-serialization:
explicit tag 17, UTF-8 target blob и child; rust-ownership: borrowed lowering и
все capture/dependency/history consumers. Реальный trigger общего helper —
recursive type-name lowercase/copies в analyzer до cap: один bounded admission
в live/query используется syntax conversion, SDK lowering и codec. Проверять
bytes/work до scan/copy, generic nesting <=64 до recursive parsing, typed copies
с учётом ancestors. RuleQuery: api-newtype-safety/own-borrow-over-clone/type-result-fallible.
Acceptance: exact integer/decimal casts; vector/nullable/collection targets сохраняют
существующие ordinary conversion errors, frozen/reactive captures,
unknown/malformed type, error atomicity, deep/oversized SDK inputs, fixed golden,
malformed lengths/children и historical replay/reopen без current record reads.
Scout подтвердил fit Text; catalog-private Type codec шире и имеет иной budget.
Finding L9b-CAST-UTF8-ADMISSION принят: raw UTF-8 validation в decode должна
следовать length/work permit. Общий helper получает borrowed bytes и возвращает
borrowed str; scalar runtime semantics остаётся ordinary evaluator.
Fresh review закрыл UTF8 finding, но выявил L9b-CAST-FAILURE-RENDER (High/resource):
failed numeric/unsupported casts форматируют полный Value до renderer permit.
Owner rust-errors/addressing-findings: воспроизвести общий scalar/builtin путь,
затем расширить существующий shared renderer admission до error construction.
Все cast callers проверяются; success numeric/Any/Bool/Object не получают
ненужное renderer admission. Бюджет и errors сохраняют общую status-систему.
Первый renderer fix прошёл focused 18/18, но fresh review обнаружил residual:
Algorithm передавал fields=None при bounded RootRead; merge_objects копировал
keys и создавал duplicate-key diagnostic без permit. Исправление передаёт
bounded_fields, переносит owned keys через существующий BTreeMap/Entry и допускает
output nodes/diagnostic bytes через тот же scalar pool. Объект identity не
форматируется. Admission полного B-tree node на поле консервативен и отмечен
комментарием ponytail. Проверки включают RootRead с non-reactive control,
Array/Set large duplicate keys, успешный merge и SDK failure без catalog mutation.
Нового type parser, Type enum, dependencies или executor не вводить. Fresh review
закрыл оба CAST findings; final focused 18/18, LIVE 200/200, GQL 297 passed/4 ignored,
SDK Sync/NativeAsync 1/1, rustdoc 5/5 и check PASS. Final tooling evidence — в report.
Dynamic sources и L11 остаются отдельными decision units; performance attempts 0/5.

Следующий L9b TaskBrief — owned Range recipe: optional start/end children и
inclusive_end через обычный analyzer/evaluator, без enumeration диапазона.
Owner rust-api-design: публичный owned descriptor; rust-serialization: explicit
tag 18 и checked flags (start/end/inclusive), children в порядке start/end;
rust-ownership: borrowed lowering, capture/DAG и обе historical eligibility proofs.
RuleQuery: api-newtype-safety/own-borrow-over-clone/type-result-fallible.
Acceptance: open/closed ranges, exact bounds >2^53, frozen/reactive captures,
membership boundaries, ordinary invalid/reversed-bound errors, cycle/rollback,
bounded depth/work/scratch, golden/corrupt flags/children, historical replay и
SDK reopen→resume→ACK→mutation. Потенциальные allocation/error rendering paths
проверяются в существующих kernels до добавления helpers. Новые range semantics,
Value variants, parser, executor и dependencies не вводить.

Scout finding L9b-RANGE-BOUND-RENDER (High/resource) принят: invalid bounds
форматируются Range::new до permit, а boxes результата не имеют shell admission.
Owner rust-errors/addressing-findings + rust-ownership: сначала shared crate-private
construction hook на единственном runtime validation predicate, затем bounded
evaluator policy через существующий renderer pool. Public Range::new сохраняет
прежние semantics/errors. Decoder callers проверяются на короткую typed corruption
вместо форматирования malformed values. Helper reuse обоснован реальными CAST
и Range consumers; новый budget или второе сравнение bounds не вводить.
Fresh source review закрыл L9b-RANGE-BOUND-RENDER: source PASS, новых findings нет.
Final focused 21/21, 0.34s; ordinary range oracle, both endpoint captures,
golden/flags/truncation, historical 17→19/current99, reactive+RootRead renderer
и SDK rejection без catalog mutation PASS. Full lib/SDK/tooling runs — в report.

Следующий L9b TaskBrief — 22 non-Pipeline binary operators и Truthy.
Owner rust-api-design: расширить существующий LiveBinaryOperator, ordinary
analyzer/evaluator — semantic authority; rust-serialization: checked explicit
operator tags 14..35 в существующем Binary tag 5. Truthy нормализуется через
existing Not(Not(value)) и SDK composition method, без нового durable tag;
оба synthetic nodes учитываются в syntax admission/depth. rust-ownership:
сохранить short-circuit, captures и existing dependency/history traversal.
RuleQuery: api-newtype-safety/own-borrow-over-clone/type-result-fallible.
Pipeline — traversal IR с tv:: steps, не scalar Binary; отдельный recipe shape
остаётся OPEN. Typed/named calls и dynamic sources не входят в этот decision unit.
Scout finding L9b-BINARY-REACTIVE-GUARD (High/resource) принят: shared binary
operand/product guard имеет bounded fields branch, но пропускает reactive control
fallback. Owner rust-errors/addressing-findings сначала воспроизводит bypass и
переиспользует существующий scalar pool/work admission; второго account/executor нет.
Acceptance: exact arithmetic/overflow, sentinels/empty quantifiers, Elvis/coalesce,
spatial touching/boundaries/invalid later member, inactive failures/effects/cycles,
reactive work cap/cancellation, golden unknown op tags, history и Rust SDK reopen.
Focused gate operators/Truthy PASS 26/26; source review PASS, finding
L9b-BINARY-REACTIVE-GUARD CLOSED. NotIs Rustdoc соответствует ordinary NOT IS.
Public SDK сохраняет existing Binary shape; operator tags не зависят от Rust
discriminants. Полнота остальных expression/source recipes остаётся OPEN.
Observed helper trigger rust-style-clippy: binary guard внутри общего evaluator
поднял cognitive complexity до 32/30. Private admit_binary_operands отделяет
существующую scalar admission policy, сохраняя порядок checks/copies/work;
нового account или kernel нет. После extraction повторяются focused/Clippy gates.

Следующий L9b TaskBrief — typed/named/DISTINCT call modifiers. Owner rust-api-design:
existing Call сохраняется; CallWithOptions содержит name, optional symbolic type,
distinct и ordered Vec<(Option<Text>, LiveExpr)>, без новых public types и parallel
vectors. Original order сохраняется до ordinary signature mapping/ranking; syntax
named arguments — `name = expression`. Owner rust-serialization: expression tag 19,
checked flags и optional names, existing tag 6 неизменён. rust-ownership: tuple
value walkers/captures/DAG и ordinary type/effect/version resolution. RuleQuery:
api-newtype-safety/own-borrow-over-clone/type-result-fallible.
Accepted scout finding **L9b-CALL-DISTINCT-CLONE (Medium/resource)**: aggregate
sorter input и previous dedup value клонируются до соответствующего admission.
Owner rust-errors/addressing-findings сначала проверяет reproduction и допускает
copies через existing scalar pool, без новых accounts. Historical complete-domain
proof расширяется только для proven count/sum; другие calls сохраняют coverage
rejection. Pipeline/dynamic sources и ordinary nested textual generic lookahead
остаются отдельными decision units. Acceptance: typed predicate, named PURE args,
DISTINCT duplicates/NONE/NULL, exact numbers, byte/work budgets, captures, version
invalidation, original/new golden bytes, historical count/sum и Rust SDK reopen.
Review findings (L9b call modifiers): **CALL-PARAMETER-TYPE-ADMISSION** принят
(High/resource): bounded signature metadata перед compiler-owned parameter copies;
**CALL-NAMED-ORDER** принят (Medium/parity): SDK сохраняет grammar named-tail rule;
**CALL-HISTORY-NAMECASE** принят (Medium/correctness): borrowed case-insensitive
count/sum proof. Focused DISTINCT history выявил **CALL-DISTINCT-HISTORY-CONTEXT**:
spill context читает graph catalog реконструированного root, который заведомо пуст;
metadata read допускается, graph/foreign input coverage не расширяется. Closure
каждого finding требует regression test и fresh source review. L11 attempts 0/5.
Signature fix owner rust-errors: общий ordinary named_function допускает matching
arity signatures до ranking/result clone; compile_named_function использует тот
же borrowed shape guard до recursive formatting/function clone, включая transitive
calls и direct compiler consumers. Existing compiler budget/RAII reservation и
admit_type_shape переиспользуются; bounded flag сохраняет обычный unbounded путь.
Review finding **CALL-SIGNATURE-DOUBLE-SCALE** принят: shape bytes уже описывают
metadata copies и не должны повторно получать source multiplier 1024. Общая
compiler RAII reservation отделяет source expansion от raw signature bytes/work;
account и resource labels остаются прежними. RuleQuery
type-result-fallible/own-borrow-over-clone.

Следующий L9b TaskBrief — static graph-scoped single Record source. Owner
rust-api-design: `LiveSource::GraphRecord { graph: GraphRef, record: RecordId }`
сохраняет текущие публичные identities, qualified result и frozen ClassId owner.
Owner rust-serialization: отдельный source tag 4, existing graph dictionary slot
и exact Record value blob; checked admission и malformed vectors. Coding
rust-ownership покрывает borrowed matching и owned clones после admission.
RuleQuery: api-newtype-safety, own-borrow-over-clone, type-result-fallible.
Переиспользуются ordinary SELECT/FETCH/negative dependency kernel, graph capture
policy и saved-result resume. Acceptance: direct SDK/GQL differential для
attach/detach/overlay/FETCH и разных occurrences одного base atom; class/graph
name reuse; codec golden/truncation; separate Sync/NativeAsync reopen. Dynamic
scope/source templates и general historical graph reconstruction — последующие
decision units. Coverage gaps отсутствуют; никаких новых origin полей у GraphRef.
Performance gate не запускается, attempts 0/5.
GraphRecord focused 210/210 и broad lib 1001 passed / 10 ignored PASS; separate
Sync/NativeAsync 1/1 каждый, rustdoc 7/7, check PASS. Fresh source review PASS,
LOCAL-FIXTURE-NAME CLOSED. Observed Clippy trigger large_enum_variant относится
к private single codec input/output, не retained queue/vector. Helper
rust-style-clippy выбран после actual diagnostic: narrow reasoned allow сохраняет
inline short-lived descriptor, без добавления Box allocation и изменения bytes.
Post-annotation Clippy delta PASS: 6959 baseline errors / 545 warnings, added 0;
scoped nightly format PASS. Final annotation re-review PASS. Graphify AST update
PASS: 57694 nodes / 155165 edges / 1907 communities. Общие gates Stage OPEN.

Call-modifier final gates PASS: LIVE 207/207; full lib 998 passed / 10 ignored /
0 failed, 0 filtered; SDK Sync/NativeAsync 1/1 каждый; rustdoc 6/6; check и scoped
nightly format. Fresh read-only review закрыл все принятые call findings, включая
raw signature admission. Global Clippy FAIL сохраняет baseline 6959 errors /
545 warnings, added instances 0. 43 protected files unchanged. L9 completeness,
general history, L10/L11 и Stage 28 остаются OPEN.

Пользовательский лимит L11 (2026-10-06): не более пяти попыток пройти performance
gate. Попытка — один paired gate run для зафиксированного состояния реализации;
отдельные workloads и statistical samples внутри него не являются новыми попытками.
Перед каждым run фиксируются номер, состояние кода и workloads; после — evidence
и verdict. Сейчас L11 не запускался: 0/5. После пятой неудачной попытки результат
фиксируется в русском отчёте, дальнейшие оптимизации прекращаются, остальные
implementation/correctness/resource/tooling работы продолжаются. Failed performance
gate остаётся OPEN и не заменяется декларацией завершения этапа.

L9b duration I TaskBrief: SLEEP и TIMEOUT внутри logical blocks сохраняют typed
lexical operand до invocation вместо чтения raw Object при analysis. Literal
duration сохраняет прежний fixed parser; parameter value использует общий checked
converter и обычный Slot/LiveSlot evaluator против того же statement snapshot.
ProfileStack: owner rust-database — snapshot/SSI/effect contract; coding
rust-ownership — borrowed operand/frame и отсутствие второго account/resolver.
Helper rust-design-protocol подтвердил все consumer seams; rust-testing выбран
для regression/lifecycle scenarios. RuleQuery own-borrow-over-clone и
type-result-fallible. Acceptance: nested LET/ALTER/shadowing, live value replacement,
paused binding diagnostics, fixed/calendar/negative/overflow, timeout/cancellation,
PURE/STABLE effect и single-return closure с TIMEOUT, import nested operands.
Normal и PURE executors используют один conversion helper. Coverage gaps нет.
Top-level TIMEOUT/session/import metadata — следующий отдельный срез: child deadline
должен сохраняться до autocommit publication, его нельзя потерять после row execution.
Public AST/Value, codecs и histories не меняются; L11 attempts 0/5.
Duration I focused 5/5, full lib 1006 passed / 10 ignored, separate Sync/NativeAsync
1/1 каждый, check/format/source review и Clippy delta added 0 PASS. Graphify
57713 nodes / 155256 edges / 1879 communities. Initial feature-disabled NativeAsync
выбрал 0 tests, evidence не считается; правильный enabled target прошёл.

L9b duration II TaskBrief: ordinary top-level TIMEOUT должен разрешать typed
Slot/LiveSlot на statement snapshot до подготовки mutating writer. Analyzer
lowering precedes declaration/shadowing; caller-owned control сохраняет resolved
deadline до export/admission/autocommit publication. Construction Instant не
обновляется после analysis или clone. ProfileStack owner rust-database —
snapshot/SSI и атомарный timeout до publication; coding rust-ownership — короткие
borrows и mutable handoff уже существующего control. Scout подтвердил три
adapter callers и отсутствие необходимости нового IR program container.
Helper rust-testing: success/invalid/zero/shadowing, isolation и bounded BLOCK
publication regression. RuleQuery own-borrow-over-clone/type-result-fallible;
coverage gaps нет. Outer import preflight/item metadata, EXPLAIN inner timeout
и embedded MATCH timeout остаются следующими самостоятельными input seams.
Clock test подтверждает construction anchor и minimum default deadline;
literal Instant overflow возвращает typed error. L11 attempts 0/5.
Broad duration II gate выявил SELECT continuation cleanup regression; timeout
preparation errors и existing cursor evaluation errors должны использовать
один borrowed direct-input retirement protocol. Computed cursor expression
не выполняется повторно ради cleanup. Cleanup 1/1 и duration 11/11 PASS;
fresh source re-review закрыл D2-CURSOR-01. Full/backend/tooling gates ещё идут.
Post-fix full lib 1009 passed / 10 ignored; enriched Sync/NativeAsync 1/1 каждый,
check/format и Clippy delta added 0 PASS. Graphify retry после Errno 22 ещё идёт.
Graphify retry PASS: 57719 nodes / 155280 edges / 1858 communities. Duration II
focused/broad/backend/source-review gates закрыты; общий Stage 28 closure OPEN.

L9b duration III TaskBrief: import item TIMEOUT lower-ится на одном preflight
Analyzer/TransactionRead с inherited session BindingTypes, затем фиксируется
как checked std::Duration против того же snapshot. Initial-slot lookup заимствует
Object по logical initial dictionary и не клонирует весь frame ради scalar input.
Metadata vectors допускаются до allocation; typed metadata содержит только
операторы с clause. Existing lease/expected commit_ts гарантирует freeze до END;
nested FOR locals продолжают invocation-time conversion. Parent execution-option
deadline управляет END publication; item TIMEOUT остаётся локальным.
Owner rust-database — preflight snapshot/lease/frozen inputs; coding
rust-ownership — borrowed slot lookup и shared RAII metadata admission.
Helpers rust-design-protocol/scout и rust-testing; rules own-borrow-over-clone,
type-result-fallible. Coverage gaps нет: lexical/reactive/ordinary UUID, invalid
и zero/fixed overflow до lease, loop shadowing, frozen source-dependent binding,
caller deadline/cancellation и existing lease/publication tests.
Outer import TIMEOUT недоступен в принятой grammar; redundant resolver удаляется
без расширения syntax. Reactive import graph selector и FOR iterable — отдельные
seams. LIVE lowering и его bounded compiler entry не меняются. L11 attempts 0/5.
Duration III focused 15/15, full lib 1013 passed / 10 ignored, check и separate
enriched Sync/NativeAsync 1/1 каждый PASS. Source review PASS; fixture placement
error исправлен до final focused. Clippy выявил два новых visibility findings;
narrow GQL exports закрыли их, final delta added 0, 6957 baseline errors.
Final scoped format PASS, 43 protected files unchanged. Graphify initial update
FAIL Errno 22 сохранён; штатный duration IV retry PASS (57734 nodes / 155345
edges / 1928 communities) закрыл tooling шаг duration III/IV.

L9b duration IV TaskBrief: EXPLAIN inner TIMEOUT переносится в existing outer
Statement timeout, аналогично direct MATCH/BUILD GRAPH. Все режимы EXPLAIN
используют тот же typed snapshot/SSI/construction clock и caller control;
plain/FULL не выполняют inner rows. Early preparation failure освобождает
supplied direct SELECT continuation и под EXPLAIN wrapper, через existing
borrowed retirement helper, без повторного eval. Public AST types/IR не
расширяются. Owner rust-database — snapshot/deadline/publication; coding
rust-ownership — единственный owned AST transfer и borrowed cleanup. Helpers
rust-design-protocol/scout и rust-testing; rules own-borrow-over-clone,
type-result-fallible. Graphify scoped query и caller trace подтверждают одну
production boundary; PURE отвергает EXPLAIN, import payload не допускает его.
Coverage gaps нет: parser positive/duplicate/nested, frozen/reactive/UUID,
zero/calendar/negative/overflow, no-op planning и ANALYZE execution, snapshot/SSI,
nested lexical shadow, cancellation cursor retirement. Embedded MATCH сохраняет
отдельный invocation timeout и реализуется следующим срезом. L11 attempts 0/5.
Duration IV initial focused 17/17 и full lib 1015/10 ignored PASS не закрыли
найденный source-review **D4-CURSOR-NESTED**. Real continuation regression
FAIL 0/1 доказал ранний return без retirement. Shared dispatcher теперь
охватывает top/block preparation и statement-entry control/depth failures,
без дополнительных interruption checks или computed cursor evaluation.
Post-fix focused 19/19, top-level cursor matrix 1/1 PASS; fresh source re-review
закрыл finding. Post-fix full lib 1017/10 ignored, check, enriched separate
Sync/NativeAsync 1/1 каждый, scoped format PASS; Clippy delta added 0,
6957 baseline errors. 43 protected files unchanged. Graphify retry PASS,
Stage OPEN.

L6 ProfileStack: rust-concurrency владеет writer-lease исключением только для
LIVE control commits и bounded optimistic END preparation; rust-database —
свежим final stamp, переносом latest LIVE metadata и restamp provisional versions
и source envelopes; refactoring — общим publication preparation hook обычного
commit и import. RuleSet: type-result-fallible, api-newtype-safety.
User evaluation остаётся вне commit mutex, primary/schema/capture revisions
проверяются повторно под lock. User payload values и historical Path snapshots
не restamp-ятся. ACK consumer подключается в L7 через тот же узкий control gate.
Commit time фиксируется до source/LIVE encoding; совпадение с предыдущим временем
не является основанием обновить его в backend barrier. Maintenance назначает
своё время независимо. L6-TIME закрывается repeated-time differential на RAM,
local Sync и NativeAsync для обоих barriers; malformed history не считается
допустимым исходом совпавших timestamps.

L9b duration V TaskBrief: embedded MATCH в TRAVERSE source, tv::match и BUILD
GRAPH source сохраняет private typed LogicalDuration до общей invocation
boundary visit_match_with_environment. Ordinary direct MATCH по-прежнему
нормализуется parser-ом на outer statement. Lowering выполняется до pattern
aliases; borrowed converter читает переданный invocation Snapshot, а не новый
read.snapshot(). Checked child применяет minimum caller deadline отдельно
для каждого tv::match seed; проверка до planner/open и после последнего
visitor/graph assembly исключает успешный частичный результат при timeout.
Без TIMEOUT сохраняется прежний control path. Public Value/AST, physical
plans/codecs и executable history не расширяются. Owner rust-database —
snapshot/SSI и deadline; coding rust-ownership — borrowed slot/fields и child
lifetime. Helpers design-protocol/caller scout, rust-testing, rust-review и
rust-verify только для обнаруженных seams и evidence. RuleSet:
own-borrow-over-clone, type-result-fallible. Gate: три реальные consumers,
positive/zero/invalid durations, UUID provenance, lexical shadow, snapshot/SSI,
no installed graph после ошибки, planning-only EXPLAIN и existing MATCH suites.
L11 performance attempts 0/5.

Duration V corrected focused 21/21 и source/test re-review PASS. RETURN GRAPH
terminal и final-row cancellation покрыты реальными consumers. Initial E0599
и unsupported TRAVERSE source fixture failures сохранены в report; production
parser не расширялся. Scoped format шести файлов PASS после применения exact
read-only formatter deltas при Windows mapped-file locks. Broad/backend/tooling
gates pending, Stage OPEN.
Duration V full lib 1019 passed / 10 ignored, check, exact separate
Sync/NativeAsync wrappers 1/1 каждый PASS. Нулевые backend selections первого
pipeline не считаются evidence; Clippy delta и Graphify pending. 43 protected
files unchanged.
Observed D5-CLIPPY: cognitive_complexity 34/30 — один added warning. Deadline
preparation переносится без изменения поведения в private resolver рядом с
graph/maxdepth; Option.map_or сохраняет conditional terminal checks. Для этого
structural subunit owner refactoring с сохранённым contract database; coding
ownership, helper style-clippy по observed warning. Никаких allow/dependencies;
post-extraction focused/source-review/Clippy required. Performance attempts 0/5.
Post-extraction focused 21/21, source re-review и six-file nightly format PASS;
Clippy delta added 0 закрыл D5-CLIPPY. Broad/backend gates повторяются для final
state, Graphify pending. Stage OPEN.
Post-extraction full lib 1019/10 ignored, check и exact enriched Sync/NativeAsync
wrappers 1/1 каждый PASS; 43 protected files unchanged. Все correctness/source/
format/Clippy-delta gates duration V закрыты, Graphify update pending.
Initial и штатный retry Graphify update FAIL с Windows Errno 22 на signature;
tooling gate duration V OPEN. Installed tooling/signature не менялись.

L9c TaskBrief — ordinary reactive record navigation. Scout trace нашёл потерю
statement resolver после materialization Field base в shared transaction-aware
expression evaluator: LiveSlot получает value через RecordFields, затем Field
снова входит в scalar evaluator с bounded_fields(), которое у TransactionRead
равно None. Гипотеза до runnable regression: RETURN из captured closure и
scalar tv::map над LIVE LET RecordId должны использовать обычный record oracle,
сохраняя snapshot/SSI, negative property reads и qualified occurrences. Owner
database; coding ownership для borrowed fields и captured identity; helpers
design-protocol/scout, testing/review/verify только для этого observed seam.
RuleSet own-borrow-over-clone, type-result-fallible. Public recipes/codecs и
traversal source preparation не меняются без evidence. Numeric limit/skip
fallback — отдельная гипотеза и последующий unit с errors owner; preflight до
source materialization не обещается. L11 attempts 0/5.
Corrected regression доказала runtime ошибку Field resolver; existing
RecordFields теперь передаётся после materialization base. Initial focused
1/1 PASS. Expanded oracle/isolation/reopen и fresh read-only review required;
бюджет performance gate не расходуется на эти correctness проверки.
L9c-R1 TaskBrief: low-budget test доказал обход admission при reactive RecordId
navigation: RETURN closure копирует large stored property через owned point row.
Owner database; coding ownership для borrowed immutable atom/occurrence и callbacks.
Shared record-field boundary должна выбирать borrowed effective/base value и
допускать selected property/id/label до copy; unrelated properties не копируются.
Existing QueryRead/AtomFieldRead получают caller control, bounded RootRead сохраняет
свою admission policy. Datastore использует existing callback pattern вместо
зависимости от ExecutionControl. RULES own-borrow-over-clone, type-result-fallible;
snapshot/negative/SSI/graph fallback errors сохраняются. R1 focused/resource и
повтор broad/backend/review gates обязательны. L11 attempts 0/5.
Selected scalar projection использует один существующий storage-row boundary
helper с двумя реальными consumers; RootRead borrowed point_parts разделяет
dependency resolution и owned row construction. Bounded graph scope и negative
reads сохраняются до lookup/admission. Новых public wrappers/DTO/codecs нет.
R1 final focused 3/3, fresh source review, format и Clippy delta added 0 PASS.
Full lib 1022/10 ignored и check PASS. Low-budget backend fixtures и Graphify
выполняются отдельно; closure stage OPEN, performance attempts 0/5.
Final populated Sync/NativeAsync 1/1 каждый и final Clippy delta added 0 PASS.
R1 CLOSED по focused/full/resource/source/backend evidence. Graphify update
успешно закрыл прежние signature failures; final fixture refresh pending.
Final fixture refresh снова FAIL Errno 22; current tooling gate OPEN. Source,
correctness/resource/backend evidence сохраняется; следующий update не меняет package.

L9d TaskBrief — exact traversal count inputs. Graphify query + existing scout
нашли silent zero fallback в prepared_state для Limit/Skip: wrong/negative value
и number выше i64::MAX теряются через Option. Owner errors; coding ownership для
fallible VM construction/collect и освобождения partial state. Rules
err-result-over-panic, err-question-mark. Scope: typed count rejection через
existing Number exact u64 conversion, без нового numeric kernel/status/dependency.
Недоступный/неверный count должен ошибаться до VM row pull, включая пустой поток;
source уже может быть materialized, preflight перед source evaluation не обещается.
Zero сохраняет существующий early termination, большие exact counts не превращаются
в ноль. Borrowed LiveSlot/snapshot resolver переиспользуется. Первый gate —
runnable negative-count regression; затем oracle/domain/snapshot/backend matrix.
L11 attempts 0/5.
Before regression FAIL: negative LIVE LET превращался в limit(0), успешный пустой
результат. Existing exact Number → u64 и fallible VM constructor устранили общий
fallback. Expanded focused 3/3 PASS: invalid/empty/domain/non-finite, независимый
slice oracle до u64::MAX и snapshot/RC/SSI. Initial expanded fixture E0308 &Uuid
исправлен dereference; это compile failure, не correctness evidence. Общий lib,
check, separate backend, source review и tooling выполняются.
L9d full lib 1025/10 ignored, check, scoped nightly format, fresh source review
и separate enriched Sync/NativeAsync 1/1 каждый PASS. Clippy delta added 0,
6957 baseline errors/545 JSON warnings сохраняются. Graphify update pending;
stage OPEN, performance attempts 0/5.
Graphify L9d update PASS: 57748 nodes/155416 edges/1913 communities;
43 protected files unchanged. L9d slice CLOSED; Stage OPEN.

L9e frozen scope TaskBrief: confirmed Recipe::graph rejects even ordinary LET/
execute GraphRef at LIVE registration. First bounded slice resolves a direct
ordinary parameter at registration against the supplied snapshot and reifies the
existing static GraphClass/GraphRecord recipe before SDK validation/publication.
Owner database: registration snapshot, frozen identity and unchanged feed policy.
Coding ownership: borrowed typed slot and owned GraphRef; refactoring owns the
ordinary graph-value resolver shared with registration. Existing command RootRead
construction is shared by expression and scope resolution; its bounded QueryCatalog
keeps named graph work/admission and GraphCatalogRange/GraphDefinition observation.
Rules own-borrow-over-clone, type-result-fallible. Design-protocol/scout confirmed
source capture, codecs/catalog and replay use the same static descriptor after
reification; no durable layout change required for this slice. LIVE LET scope must
not be frozen: tracked/computed/nested source templates remain a subsequent slice,
with qualified leave/enter and fault-safe per-commit policy resolution. Gate:
genuine registration regression, frozen lexical/execute values, CLASS/Record scope,
wrong type/provenance, block capture, rollback and saved-result reopen/ACK.
L11 performance attempts 0/5.
L9e regression FAIL 0/1: dynamic LIVE graph scope QueryFeatureNotSupported.
Initial 1/1, expanded 1/1 and final focused 2/2 PASS after named-scope RootRead
re-use: CLASS/Record, GraphRef/String, lexical capture/re-registration/block,
qualified overlays, cancellation rollback, stale/type/provenance errors and
graph catalog SSI. Source re-review PASS; catalog seam repaired by inspection,
not claimed as a before/after SSI reproduction. Compiler E0599 in interim repair
was fixed by using RootRead's existing QueryCatalog implementation. Backend fixture
keeps the SDK subscription and adds frozen GQL saved-result reopen/ACK. Broad,
separate backend and tooling gates running; tracked/nested/computed scopes OPEN.
Final L9e full lib 1027/10 ignored, check, scoped nightly --check, source re-review
и enriched separate Sync/NativeAsync 1/1 каждый PASS; Clippy delta added 0,
6957 baseline errors/545 JSON warnings. Initial local fixture GraphRef → NONE
нарушал inferred type и заменён другим GraphRef без production typing change.
43 protected files unchanged. Graphify L9e FAIL: AST 14/14, Windows Errno 22
при записи `.graphify_labels.json.sig`; installed tooling не изменялся.
Correctness/resource/backend gates L9e закрыты, tooling gate остаётся OPEN.
L11 attempts 0/5.

L9f capture identity TaskBrief: сначала отделить durable source-policy metadata
от logical query descriptor. Два реальных consumers — publication и catalog
validation — читают сохранённый ChangefeedSource без выполнения user expressions.
Owners: rust-database для immutable root identity и rust-serialization для checked
catalog layout; refactoring helper для сохранения текущего статического поведения.
Rules num-cast-try-from/type-result-fallible: checked tag/identity/reserved bytes,
никаких default identities. Public API/динамические scopes пока deferred.
Acceptance: CLASS/record/qualified graph policy parity, roundtrip/golden malformed
catalog tests, Sync/NativeAsync reopen, focused/full/check/format/Clippy delta.
Coverage gaps нет; Q-40 layout меняется на месте. L11 attempts 0/5.
L9f focused LIVE 232/232 PASS (15.20s), full lib 1028/10 ignored PASS (88.11s),
check PASS (211 existing warnings, 7.47s), fresh read-only source review PASS.
Source-policy behavior сохранено; Sync GraphRecord reopen/ACK PASS 1/1 (0.53s).
NativeAsync GraphRecord PASS 1/1 (0.56s), nightly --check 9 files PASS,
Clippy delta added 0; общий lint FAIL на прежних 6957 errors/545 JSON warnings.
43 protected files unchanged. L9f correctness/resource/source/backend gates CLOSED;
Graphify update выполняется, Stage OPEN, L11 attempts 0/5.

L9g source-expression TaskBrief: GraphClassExpr/GraphRecordExpr дополняют SDK source
owned LiveExpr. Direct root ordinary parameter сохраняет frozen registration
contract L9e; tracked/nested/computed class/record graph scopes понижаются в общий
SELECT/RootRead. Graph-wide expression source пока отдельный срез.
Owners: rust-api-design (SDK variants), rust-database (successful resolved owner,
before/after roots и fault boundary), rust-serialization (FGLD source tags/codecs).
Три units нужны вместе: частичный enum/codec/policy diff не компилируется или
теряет durable capture semantics. Ownership coding покрывает boxed recipes и
borrowed captures; RuleQuery: own-borrow-over-clone, type-result-fallible,
num-cast-try-from. Существующие kernels/codecs подходят: нет dependencies,
нового executor, registry или worker. Scope разрешается до row pull через тот же
RootRead; успешное вычисление публикует capture owner, fault оставляет прежний
owner для контрольной истории. Catalog проверяет metadata, не исполняет recipe.
Dynamic String root scope сохраняет последний успешный name operand вместе с
GraphId; неизменное имя удерживает identity при rename/remove/recreate, изменённое
разрешается заново. Durable pair нужна для pending activation/future SINCE/reopen:
before-root reconstruction не является достаточным контрактом. Fixed header 160,
minimum 216; name cap 128 UTF-8 bytes, admission до clone. Nested scopes остаются
ordinary snapshot read dependencies. Source review repair PASS; execution gates OPEN.
Acceptance: direct SDK/rustdoc; root/nested GQL; parameter-only A→B и combined
DML/parameter, qualified same-record leave/enter; empty domains/invalid scope;
GraphRef/String, frozen captures, binding DAG/removal/revisions; graph feed policy
switch/off и fault с успешным DML; codec malformed/depth/budget; old snapshots и
Sync/NativeAsync saved result resume. General historical graph reconstruction
остаётся coverage GAP. Focused gates обязательны; L11 attempts 0/5.

L9g final evidence: focused 7/7, LIVE 239/239, full lib 1035 passed / 10 ignored,
Sync/NativeAsync SDK 1/1 каждый, check и rustdoc 8/8 PASS. Узкий style repair
устранил 18 новых Clippy warnings; final delta added 0, global baseline debt
6957 errors / 545 JSON warnings. Rules lint-rustfmt-check/lint-warn-style;
публичная семантика и конфигурация lint не менялись. Protected 43 files unchanged.
Correctness/resource/source/backend gates CLOSED, stage/tooling closure OPEN.
Graph-wide SDK expression source — следующий отдельный срез; L11 attempts 0/5.

L9h0 empty-source TaskBrief: перед расширением whole-graph recipes сохранить
scope у internal SelectSourceKind::Empty. Сейчас analyzer заменяет его пустым
graph-catalog scan и теряет expression, PURE validation и reads. Owner
rust-database: typed empty relation сохраняет optional graph reference; общие
SELECT pull/materialized paths валидируют scope до пустого результата. Coding
ownership покрывает borrowed source/slots и checked typed-tree admission;
RuleQuery type-result-fallible, own-borrow-over-clone, lint-rustfmt-check.
Одно представление обходит analyzer/effect/demand/fingerprint/executor;
никаких public Value, storage DTO, WAL, dependencies или нового executor.
Acceptance: zero classes с валидным/неверным/missing/impure scope, COUNT=0,
nested SELECT, deterministic validation, bounded work/cancellation и прежний
LIVE/full/backend gate. GraphExpr API/codecs и union single resolution идут
после correctness gate этого refactor. Coverage gaps нет; L11 attempts 0/5.

L9h0 initial focused FAIL 0/2: nested fixture использовала обычный top-level
expression context вместо production analyze_reactive_expression. Исправлена
только fixture. Source review R1: Empty должен не только разрешить operand,
но и выполнить существующую graph validation/read, иначе stale GraphRef выдаёт
нулевой результат без GraphDefinition dependency. Shared validate_empty_graph
переиспользует read.graph с checked identity/snapshot/work/scratch; ClassCatalogRange
для SDK union остаётся отдельным существующим read. PLAN_ENCODING_VERSION=9,
новых storage bytes нет. Expanded focused и source re-review выполняются.

L9h0 final evidence: focused 2/2, full lib 1037 passed / 10 ignored; separate
SDK expression-scope regressions Sync/NativeAsync 1/1 каждый. Check PASS,
nightly --check 13 files PASS, Clippy added diagnostics 0 (6957 baseline errors /
544 warnings). Fresh source review PASS, включая Static GraphRef pre-admission.
Graphify update PASS, protected 43 unchanged. Срез закрыт; Stage OPEN, L11 0/5.

L9h1 TaskBrief: добавить owned `LiveSource::GraphExpr(Box<LiveExpr>)` для полного
qualified occurrence domain. SDK lowering перечисляет class incarnations в одном
root; scope разрешается один раз до root/nested union, включая zero classes.
Ordinary GQL graph metadata source и general historical coverage deferred.
ProfileStack: `whole-graph-api` owner rust-api-design; `scope-union` owner
rust-database; `descriptor-tag` owner rust-serialization. Три units меняются
атомарно: новый public enum требует всех lowering/catalog/codec matches, а nested
union должен сохранить тот же контракт. Coding rust-ownership покрывает borrowed
inputs и pre-admitted copies; rules own-borrow-over-clone, type-result-fallible,
num-cast-try-from. Helpers rust-testing только для конкретной acceptance matrix.
Acceptance: zero/one/multi classes, root/nested once-only scope, aggregate parity,
per-class PURE overloads, ClassCatalogRange, parameter-only switch, budgets,
typed invalid/impure errors, canonical tag 7/truncation/depth, rustdoc без query
text и отдельные Sync/NativeAsync reopen. Gates: focused/full/check/fmt/Clippy
delta/source review/Graphify. Coverage gaps нет; L11 attempts 0/5.

L9h1 R1 accepted: nested union повторно lowered scope после восстановления outer
capture frame; nested-only параметр ошибочен, shadowing выбирает outer graph.
Closure: regression до исправления, собственный frame при wrapper lowering,
one/multi classes × scalar aggregate/collection × отсутствующий/shadowed outer
parameter, fresh source review. Initial focused build FAIL (test fixture:
BindingEvaluation не хранит reads; Object::from_iter нуждается в concrete Value).
Fixture repaired без изменения production API. L11 attempts 0/5.

R1-before regression FAIL 0/1 воспроизведён при двух classes. Wrapper expression
lowered в собственном query.parameters() frame с pop до propagation Result.
Fresh independent source review и focused/broad/backend gates выполняются.

L9h1 final evidence: focused 3/3, expanded LIVE 244/244, full lib 1040 passed /
10 ignored / 0 failed / 0 filtered; fresh independent source review PASS и R1
closed. Separate Sync/NativeAsync SDK fixture 1/1 каждый, rustdoc 8/8, check PASS,
nightly --check 15 files PASS; Clippy added diagnostics 0 (6957 existing errors /
544 warnings). Graphify update PASS; protected 43 unchanged. Срез закрыт по
evidence; remaining SELECT clauses/general history/L10/L11/Stage OPEN, L11 0/5.

L9i discovery TaskBrief: существующий grouped SELECT поддержан ordinary pipeline,
но LIVE owned recipe отклоняет GROUP BY/ALL. Завершить выбранный пользователем
aggregate-result identity contract для groups через общий executor и Rust builder.
Сначала trace/refactor class-union grouping kernel; затем owned clauses/codecs/GQL,
без metadata sources/ORDER/LIMIT/joins в текущем срезе. Discovery owner
rust-database; constructs typed grouped expressions, immutable per-class inputs,
admitted copies (rust-ownership). Acceptance: grouped LIVE против ordinary before/
after SELECT, stable group identities, merge одинаковых keys разных classes,
per-class overloads, zero-domain, exact Number keys, budget/fault и saved replay.
API/codec decisions deferred до kernel evidence; L11 attempts 0/5.

L9i0 ProfileStack после scout: `union-group-kernel` owner rust-database,
coding rust-ownership для borrowed variant slices и admitted CASE copies.
Сначала behavior-preserving extraction `row_expression`, затем reuse обычного
GROUP sorting/reduction над merged raw rows. Typed projected key subtree
сопоставляется обычным `group_expr_equivalent`; различающиеся outer overloads
над агрегатами по-прежнему несовместимы. RuleQuery: per-class TypedExpr ownership,
bounded heap copies, shared snapshot/work/cancellation; rule own-borrow-over-clone.
Coverage: refactor — существующий overload test; grouping — direct typed plans,
same keys, DISTINCT/AVG, wide numeric/NULL/NONE, BY/ALL empty domain и rejection
по budget. Public API/codecs/history — следующий срез. Coverage gaps нет.

L9i0 evidence: CASE extraction 1/1 PASS; direct union grouping 2/2 PASS (0.03s),
exact Number/NULL/NONE, shared AVG/DISTINCT, row overloads, projected key внутри
arithmetic и empty BY/ALL. Source review выполняется.
L9i1 ProfileStack: `group-builder` owner rust-api-design — private
Option<Vec<LiveExpr>>, checked with_group_by(nonempty), with_group_all и borrowed
group(); `group-descriptor` owner rust-serialization — FGLD query group tag после
filter (0=None, 1=ALL, 2=BY + checked positive count/children), Q-40 inplace;
`group-lowering` owner rust-database — shared SELECT, captures/DAG and historical
complete-domain proof. Same-phase reason: field, codec и every recipe consumer
нужны вместе для recoverable registration. Coding rust-ownership: borrowed groups,
bounded retained copies. Acceptance: direct SDK/GQL before/after grouping, reactive
key-only changes, nested scalar/collection, corruption/depth/count, owned resume
Sync/NativeAsync и historical proof. RuleQuery: owned optional clause, borrowed
accessor, checked counts/bytes and fallible registration; own-borrow-over-clone,
type-result-fallible. Deferred: metadata sources, ORDER/LIMIT/joins, L10/L11.
Coverage gaps нет; L11 attempts 0/5.

L9i1 initial focused 0/2 FAIL: F1 computed projected key получает другой synthetic
slot при повторном lowering; ordinary equivalence отклоняет GROUP BY. F2 fixture
считала codec count u64 вместо authoritative u32. F1 accepted: reuse exact owned
literal slots в одном immutable candidate invocation; representation/qualified
graph descriptors сохраняются, scan comparison work admitted до обхода. Ordinary
group_expr_equivalent не ослабляется. F2 исправляет только checked-layout fixture.
Regression — frozen/reactive/computed keys + fresh bounded source review.

L9i1 accepted review findings: F3 Range literal slot reuse обязан сохранять
GraphRef descriptor metadata в обоих bounds; добавлен recursive exact comparison
и public evaluator regression. F4 новый provenance slot использовал restored outer
alias при nested multi-class GraphExpr wrapper: собственный capture мог попасть
в slot родителя. Wrapper временно удерживает child alias/captures и восстанавливает
оба на success/error. Existing nested capture regression запускается вновь.
F1 regression дополнительно проверяет rejection разных LiveBindingId/lexical names
при равных current values и acceptance одного key identity. Fixture LET mutation
вынесена из BEGIN; build ambiguity Object values исправлена отдельно. L11 0/5.

L9i1 F5 diagnostic TaskBrief: public grouped resume fixture после reopen/ACK
позволяет подготовить DELETE CASCADE, но commit падает `graph class statistic is
absent`. Причина: основной replay использует hydrated next, changefeed operation
replay повторно клонирует исходный current с deferred statistics. Owner
`capture-replay-origin` — rust-database; coding rust-ownership для immutable
hydrated before root и COW next. Helper debugging локализовал failure в publication,
addressing-findings ведёт F5. Refactor hydration до общей before/next fork, затем
capture получает тот же before root; original revisions/FCW/SSI не меняются.
Regression: existing public Sync/NativeAsync fixture (reopen/ACK/cascade, exact
saved grouped images) + changefeed/full library suites. Budgets/recovery и отсутствие
обычной nested transaction сохраняются; coverage gaps нет. L11 0/5.

L9i1 F6 ProfileStack: `import-capture-origin` owner rust-database, coding
rust-ownership для retained segment_before. Ревью подтвердило cloning до первого
prepare_mutation; On import после reopen может воспроизвести F5. Сначала отдельный
first-segment DELETE CASCADE regression с global и graph FULL feeds; затем
hydration исходного working root перед retained clone в существующем spool helper.
Metadata admission предшествует clone, ON/OFF/savepoint/lease правила прежние.
Helper debugging проверяет воспроизведение; fresh source review после fix.
Deferred другие import changes; coverage gaps нет, performance attempts 0/5.

## 5. Ресурсы и закрытие

L9i2 TaskBrief/ProfileStack: следующий bounded срез после GROUP BY focused/full
correctness — owned scalar constructors вокруг graph-wide aggregates. Change slice:
shared aggregate_union::merge_expression и admit_special_copy; existing LiveExpr
Array/Set/Object/Conditional/Case/Access/Index/Cast/Range, без нового SDK/storage
format. Owner `aggregate-constructors` — rust-database: один SELECT reduction domain,
snapshot-bound evaluation и сохранение per-class argument overloads. Coding
rust-ownership — borrowed typed children, admitted retained nodes/names/types до
clone/allocation. RuleQuery: own-borrow-over-clone, type-result-fallible.
Acceptance: SDK graph/class differential и independent expected Values для всех
constructors, lazy branches, optional CASE/range bounds; typed incompatibility
для различающихся outer descriptors; work/scratch rejection до partial output;
GQL net changes и saved-result resume через existing recipe/history consumers.
Helper rust-testing активируется для этой concrete acceptance matrix; read-only
source review medium-risk shared kernel. Coverage gaps нет; ORDER/LIMIT/joins,
general historical coverage и L10/L11 отложены. Performance attempts 0/5.

L9j TaskBrief/ProfileStack: owned OMIT paths для SDK/GQL, persisted recipes и
обычный SELECT projection kernel. Три связанные decision units проходят один
срез (same_phase_reason: recoverable descriptor должен одновременно строиться,
lower/decode и исполняться): `omit-api` owner rust-api-design;
`omit-descriptor` owner rust-serialization; `omit-integration` owner rust-database.
Coding rust-ownership: borrowed path segments, checked admission до retained
copies. RuleQuery: own-borrow-over-clone, type-result-fallible. До изменения
поведения выделяется общий converter для OMIT/FETCH; отдельный executor не нужен.
OMIT выполняется после projection, перед FETCH; identity результата остаётся
отдельной, включая удаление id из payload. Literal segment boundaries, порядок
и повторные paths сохраняются; missing/non-object paths используют обычный no-op.
Shared OMIT traversal получает существующие cancellation/work/depth checks.
Acceptance: SDK/GQL differential, nested/literal-dot/missing paths, OMIT-before-FETCH,
omitted-only changes без delivery, отдельные row/group identities; descriptor
goldens/roundtrip/count/text/truncation/scratch rejection; catalog и Sync/NativeAsync
reopen/resume; исторический proof с path work admission. Helper rust-testing
активирован этой матрицей; read-only scout проверил descriptor consumers,
fresh source review после реализации. Coverage gaps нет; ORDER/LIMIT/START/ONLY,
joins/SPLIT, general history, L10/L11 deferred. Performance attempts 0/5.

L9j evidence: focused 6/6 PASS, full lib 1054 passed/10 ignored/0 filtered,
SDK Sync/NativeAsync 1/1 каждый, LIVE rustdoc 12/12, check и nightly --check
12 files PASS. Clippy added diagnostics 0 при existing global debt; source review
PASS. Graphify retry PASS после transient signature write failure; core graph
57835 nodes/155739 edges. Protected 43 unchanged. Stage OPEN, L11 0/5.

L9k discovery TaskBrief: ORDER/LIMIT/START OFFSET для owned LIVE recipes через
обычный SELECT sorting/pagination. Entry layer Domain/Design; triggered helper
rust-design-protocol установил два existing consumers: root identity visitor и
nested independent union values. Graph-wide window должен быть один для всего
class domain, с per-class typed expressions и qualified identities. Repo facts:
root visitor сейчас не применяет bounded page к barrier output; class union
объединяет только aggregates, row-local branches исполняются отдельно.
Pre-edit decision units: `window-kernel` owner rust-database (общий bounded
result window и class-specific union), `window-api` owner rust-api-design,
`window-descriptor` owner rust-serialization. Общий kernel корректируется первым;
API/descriptor после его focused gate. Coding rust-ownership для borrowed
variants/admitted retained rows; rust-testing после concrete differential cases.
Alternatives: shared union/sorter/pager сохраняет query pipeline; per-class window
не соответствует global SELECT domain и не выбирается. START AFTER требует
runtime-only query cursor и остаётся unsupported LIVE capture, RAND не считается
PURE. Нерешённые discovery facts: wildcard per-class projection, grouped ORDER
expressions, ordering-only net semantics и negative LIMIT preflight до rows.
Root sole writer; read-only kernel scout допустим по high-risk workflow.
Deferred joins/SPLIT/ONLY/index hints и L10/L11. Attempts 0/5.

L9k0 DesignBrief/ProfileStack после scout: confidence High, gaps нет для первого
kernel среза. Owner `window-kernel` rust-database; coding rust-ownership для
anchor-preserving moved records и borrowed plan/values. RuleQuery
own-borrow-over-clone, type-result-fallible. Сначала record-preserving pager и
общий offset/LIMIT preflight; root и grouped union используют их до row pull.
START expression admission дополняет existing select_copy до plan clone.
Negative/fractional/wrong/overflow bounds сохраняют eval_usize; LIMIT 0 валидирует
graph scope/reads. Independent union/global wildcard и grouped ORDER merge
остаются следующей частью kernel, затем SDK/codec vertical slice. Net contract:
ORDER задаёт membership окна; перестановка неизменённых retained identities
не создаёт event, LiveBatch сохраняет canonical identity order без rank metadata.
Acceptance L9k0: direct typed root visitor против ordinary SELECT для streaming,
ORDER barrier и GROUP ALL; offsets/zero/invalid bounds before row pull; pager
skip cancellation/work и anchors, START admission resource rejection. Concrete
tests активируют rust-testing; fresh read-only review после kernel. API/descriptor,
joins/SPLIT/ONLY/index hints, general history и L10/L11 deferred. Attempts 0/5.

L9k0 implemented: shared select_window использует existing exact eval_usize и
RecordFields до row pull. Root visitor и grouped union применяют window к
CursorRecord, сохраняя anchors; value consumers используют wrapper того же pager.
Skip/pull проверяет work/cancellation, LIMIT 0 валидирует graph scope/reads без
row materialization. START Offset/After expression получает admission до clone.
Focused 6/6 PASS, check PASS; fresh source review PASS. Два residual test surfaces
review — forced disk paging и invalid Empty bounds — добавлены в full lib gate.
Clippy delta 0 added (6954 existing errors/544 warnings); global lint gate OPEN.
Full lib/tooling evidence выполняется. Public recipes/codecs, global row-local
union и grouped ORDER merge остаются L9k continuation. L11 attempts 0/5.

L9k0 full lib/datastore **1060 passed/10 ignored/0 failed/0 filtered PASS**, 118.16s,
включая новые TEMPFILES и invalid Empty bounds. Selected nightly fmt 4 files PASS,
protected 43 unchanged. Graphify pending; Stage 28 OPEN, L11 0/5.

L9k1 pre-edit ProfileStack: тот же `window-kernel` owner rust-database, coding
rust-ownership (borrowed class plans/order variants, moved CursorRecords), rules
own-borrow-over-clone/type-result-fallible. Existing ordinary analyzer допускает
projected ORDER alias только как top-level Name; другие expressions читают input
schema. Поэтому grouped ORDER rebase касается top-level Column alias, остальные
expressions объединяются existing merge_expression. Row-local class union
сохраняет per-class projection/keys, общий QuerySorter и один window. Без новых
clauses прежний streaming путь сохраняется. Новый private union module имеет
два реальных consumers — root identities и nested values. SDK/codec не меняются.
Acceptance: mixed schemas/wildcards, exact global ordering/ties/window, omitted
identity, grouped alias/direct aggregate/class-specific overload и nested cap
после global START; budget/failure без publication. Fresh review после gate.
L11 0/5; первый L9k0 Graphify update снова получил Windows Errno22 после core
publication, signature bytes/hash не изменились, стандартный retry выполняется.

L9k0 Graphify standard retry PASS: AST 3/3, topology unchanged; current core
57853 nodes/155801 edges/1915 communities. L9k0 focused/full correctness,
resource/source/scoped tooling PASS. Stage 28/global Clippy OPEN, L11 0/5.

L9k1 kernel implemented: row-local global sorter/window сохраняет original class
projection/keys/anchors; unordered non-TEMPFILES работает через existing streaming
evaluator и останавливается после global limit. Nested maximum после global START.
Grouped aliases rebased после padded row/tag; aggregate ORDER использует общий
merge_expression с typed arguments. Focused 11/11 PASS, check PASS, fresh source
review PASS, Clippy 0 added (6954 existing errors/544 warnings). Full lib/scoped
tooling выполняются; SDK/codec — следующий срез. L11 0/5.

L9k1 full lib/datastore **1065 passed/10 ignored/0 failed/0 filtered PASS**, 91.75s;
final check PASS 6.88s, Clippy 0 added. Scoped format 7 files PASS, protected 43
unchanged, Graphify update PASS: 57876 nodes/155890 edges/1912 communities,
AST 14/14, LLM cost 0. Public API/codec ещё не расширены. L11 0/5.

L9k2 TaskBrief: добавить owned Rust/GQL ORDER/LIMIT/START Offset recipes поверх
прошедшего L9k1 kernel, canonical FGLD layout и все capture/DAG/history walkers.
ORDER определяет membership окна; net events сохраняют qualified identity order,
изменение rank без изменения identity/payload не создаёт событие. Ordinary ORDER
name разрешается analyzer как projected alias либо input name; expression key
сохраняет source qualification. START AFTER и RAND не входят в durable PURE recipe.
Window bounds проходят ordinary exact Number validation при registration/evaluation.
ProfileStack: `window-api` owner rust-api-design (private owned fields, builders,
borrowed accessors); `window-descriptor` owner rust-serialization (checked counts,
tags/flags, bounded encode/decode); same phase требуется для restorable recipes.
RuleQuery: api-builder-pattern, api-parse-dont-validate, type-result-fallible.
Existing manual FGLD/history codec выбран из-за canonical layout и общего admission;
новых dependencies/derive codecs нет. Acceptance: direct SDK и GQL window oracle,
frozen/reactive parameters, class union/group alias, historical complete domain,
malformed codec/golden bytes/reopen, rustdoc, focused/full/backend/check/fmt/Clippy
и fresh read-only source review. Coverage gaps нет. Other SELECT clauses/general
history/L10/L11 deferred до correctness gate этого среза; L11 attempts 0/5.

L9k2 focused **18/18 PASS**, 0.20s (1064 filtered): SDK/ordinary SELECT equality,
alias versus source key, grouping/global graph window, exact bounds и complete-domain
history, frozen/reactive ORDER/bounds, invalid bound fault при успешном DML и ручная
перерегистрация после восстановления значения. Initial fixture optional unary
ошибка исправлена без расширения production numeric semantics. Fresh source review
PASS, actionable findings нет. FGLD key headers имеют explicit tag/flags/reserved;
empty body 27/frame 67 bytes, FGDC minimum 227. Full/backend/tooling выполняются.
L11 performance attempts 0/5.

L9k2 full lib/datastore **1072 passed/10 ignored/0 failed/0 filtered PASS**, 97.96s,
после обновления старых tests: obsolete LIMIT rejection и OMIT byte offsets.
Expanded focused LIVE **266/266 PASS**, 18.60s; protected 43 unchanged. Public
Sync/NativeAsync/rustdoc/check/Clippy и Graphify evidence выполняются. Первый
Graphify core опубликован, signature Errno22 сохранён в log; same-byte native
probe не изменил bytes/hash, standard retry запущен. Stage OPEN, L11 0/5.

L9k2 lint repair ProfileStack: `window-api-lints` owner rust-api-design;
`descriptor-layout` owner rust-module-layout. Serialization deferred: bytes и
runtime semantics уже gated, изменяется только private source layout. Findings
W1 doc tabs (3), W2 exhaustive new enums (2), W3 impl Trait parameter (1),
W4 parent module filename (1) accepted; addressing-findings helper ведёт ledger.
Direction/nulls enums следуют project non-exhaustive SDK policy; wire flags остаются
закрытыми и checked. Новый window child требует `live_descriptor/mod.rs` по текущему
Clippy convention. Namespace/privacy/API и bytes не меняются; путь Clippy baseline
нормализуется для mechanical move с сохранением original JSONL. Acceptance:
focused codec/window/full/backend/check/rustdoc/Clippy delta и fresh source review.
Coverage gaps нет. Existing unrelated test-string whitespace preserved. L11 0/5.

L9k2 W1–W4 fresh source review PASS: actionable findings нет. Post-repair
focused **18/18 PASS**, полный lib/datastore **1072 passed/10 ignored/0 failed/
0 filtered PASS**, 106.04s; `cargo check --lib --no-default-features` PASS,
9.68s. Scoped nightly rustfmt check **16 files PASS**. Clippy repaired delta
**0 added** (6954 existing errors/544 warnings), с documented filename-only
normalization original baseline. Protected README/asynchronous/collections:
**43 unchanged**. Rustdoc/backend и final Graphify evidence выполняются;
Stage 28 OPEN, L11 **0/5**.

L9l discovery TaskBrief: включить обычный `SELECT ONLY` в owned LIVE recipes.
Сначала L9l0 shared cardinality kernel, затем L9l1 SDK/GQL/descriptor. Owner
`only-kernel` rust-database; coding rust-ownership для borrowed plans и moved
payloads. RuleQuery type-result-fallible, own-borrow-over-clone. Existing SELECT
проверяет число строк после общего ORDER/START/LIMIT и projection/OMIT/FETCH:
zero — NONE для scalar caller, но пустой identity-map для LIVE; one сохраняет
row/group identity; more than one — FG-22F22. Streaming уже ограничивает pulls
двумя, barriers сохраняют полное вычисление и прежний error precedence.
Graph class union проверяется один раз после global window; per-class ONLY cap
не должен обрезать global START. Nested union требует того же final check.
Changed-row history для ONLY недоказан и запрещается; complete-domain proof
сохраняется. Read-only scout завершён; discovery gaps нет. Acceptance L9l0:
root/ordinary differential, zero/one/two, barriers/groups/union/nested union,
global offset >=2, exact invalid bounds, projection error precedence, focused
full/check/fmt/Clippy и fresh review. API/codec owner units deferred до kernel
correctness gate; joins/SPLIT/index hints/general history/L10/L11 deferred.
Root sole writer, high-risk read-only scout/reviewer по rust-workflow. L11 0/5.

L9l0 observed O1: focused **20/21 PASS, 1 FAIL** — unordered union VALUE id
теряет anchor. Compact atom rows имеют logical id в column 0, но пустой stable
vector; существующий union callback копировал только stable. Общий storage-row
layout не меняется: union использует borrowed logical id как fallback anchor,
с admission до copy. Owner `only-kernel` rust-database сохраняется, coding
rust-ownership покрывает borrowed slice. Это существовавший seam, впервые
проверенный новым differential; silent synthetic ordinal не вводится. Narrow
reproduction и focused/source re-review обязательны. L11 **0/5**.

L9l0 O1 repair focused **21/21 PASS**, 0.34s (1064 filtered): common ONLY
cardinality, compact VALUE id qualified anchor, global offset crossing class
boundary, grouped/nested union и mismatch/bad-window preflight. Separate narrowed
FAIL START AT 2 LIMIT 1 сохранён; fallback использует `slice::from_ref` borrowed
id и existing admitted vector copy. Общие compact rows не расширяются. Full,
check, Clippy delta и fresh source review выполняются. API/codec deferred;
Stage OPEN, L11 **0/5**.

L9l0 full lib/datastore **1075 passed/10 ignored/0 failed/0 filtered PASS**,
92.74s; check **PASS**, 8.02s, scoped rustfmt **4 files PASS**. Fresh O1 source
re-review **PASS**, actionable findings нет. Public/frontend recipes и manually
mixed private nested IR вне scope этого review. Clippy delta выполняется;
дальнейший SDK срез начинается после focused correctness evidence этого kernel.
L11 **0/5**.

L9l1 pre-edit TaskBrief/ProfileStack: public `with_only(bool)`/`only()` в
`LiveQuery`, default false; syntax conversion и lowering сохраняют flag для
root и nested class union. Decision units `only-recipe` owner rust-api-design
(SDK, lowering, eligibility semantics), `only-flags` owner rust-serialization
(canonical FGLD byte). RuleQuery api-builder-pattern, type-result-fallible.
Output byte становится query flags: bits 0–1 Full=0/Diff=1/Patch=2 (3 invalid),
bit 2 ONLY, bits 3–7 reserved zero. Default bytes/minima 67/227 не меняются;
никаких derive codecs, decoder aliases/dependencies. Checked node/byte budgets
прежние. ONLY требует complete-domain historical proof и не допускает changed-row
replay. Registration >1 отклоняется; active 1→2 приостанавливается с fault при
успешном DML. Пустой result не создаёт synthetic NONE identity. API/codec должны
быть restorable в одном срезе. Acceptance: direct SDK/GQL differential, graph
class union и nested ONLY shape, lifecycle/history, true/false/invalid flags
goldens, separate Sync/NativeAsync reopen/ACK, rustdoc/focused/full/check/fmt/
Clippy-delta и fresh source review. Coverage gaps нет; remaining SELECT clauses,
general historical coverage/L10/L11 deferred. Root sole writer; L11 **0/5**.

L9l1 resume diagnostic TaskBrief: focused 24/26 PASS, два новых tests ошибочно
ожидают FG-22F22 как top-level active fault. Existing prepare_fault сохраняет
FG-G1F09 envelope и FG-22F22 cause, bounded codec удаляет только technical source.
Decision unit `only-fault-tests`, owner rust-testing; debugging helper подтвердил
причину по prepare_fault/status definition и существующему diagnostic test.
Изменяются только два assertions SDK/GQL: проверять envelope и точную cause.
Registration/historical errors остаются direct FG-22F22. Acceptance: focused
26 tests, затем прежняя L9l1 API/codec матрица. Coverage gaps нет; production
publication/error semantics не меняются. L11 **0/5**.

L9l1 закрыт focused correctness evidence: 26/26 PASS, полный lib/datastore
1080 passed/10 ignored/0 failed/0 filtered PASS (85.37s), check PASS (8.89s).
Rustdoc LIVE 14/14 PASS; отдельные Sync/NativeAsync window reopen/ACK 1/1 PASS
каждый. Scoped nightly rustfmt 10 files PASS; Clippy added 0 при existing
6954 errors/544 warnings. Fresh read-only source review PASS. Protected 43
unchanged. Graphify AST 18/18, 57942 nodes/156089 edges/1901 communities PASS;
zero-node context7.json/config.json остаются явно записанным ограничением graph.
Stage OPEN, L11 **0/5**.

L9m normative decision от 2026-10-07: пользователь выбрал составную identity
JOIN/SPLIT. Participants идут в порядке FROM/JOIN declaration, независимо от
CBO execution order; LEFT unmatched сохраняет явный отсутствующий slot.
Qualified source identity удерживается до projection/OMIT/SPLIT. Positions —
checked u64, zero-based, в порядке SPLIT clauses: arrays сохраняют order и
duplicates, sets используют существующий canonical Value order. GROUP остаётся
Aggregate identity. Обычные single-source rows сохраняют Record/Occurrence.
Array reorder может менять payload на фиксированной позиции; ordinal scan,
output ordinal и aliases не входят в identity. Slices: L9m0 runtime identity/
checked history codec; L9m1 общий bounded relational provenance kernel;
L9m2 public SPLIT recipes; L9m3 JOIN recipes/combined schema. Каждый slice
начинается после correctness gate предыдущего. Derived JOIN sources требуют
отдельного proven provenance contract; молчаливое guessed identity запрещено.
Existing ordinary stable/sequence keys не меняются. L11 **0/5**.

L9m0 pre-edit TaskBrief/ProfileStack: decision units `composite-runtime`
owner rust-api-design и `composite-history` owner rust-serialization. Сначала
перенести result identity из delivery в cohesive private module с прежним
public re-export; затем добавить flat nonrecursive Composite и LiveIdentityPart
(Record/Occurrence/Unmatched/Split). First part — primary source, всего >=2;
все source slots перед positions. One-part Composite noncanonical и запрещён.
Ord/Hash используют GraphId/AtomRef без presentation name, checked u64 positions
не преобразуются в f64. FGLB identity tag3: checked u32 part count, part tags
0 Record blob/1 Occurrence/2 Unmatched/3 Split u64 LE. Existing tags0..2 и
FGLD/default minima неизменны. Admission/work/count checks до allocation/write;
shape rules общие runtime/codec, source images для resume не читаются.
RuleSet api-common-traits, api-non-exhaustive, type-result-fallible; Serde rules
не применяются к принятому manual format. Existing
manual canonical codecs предпочтительны установленным derive envelopes, поскольку
это authoritative explicit byte contract. Acceptance: exact equality/order/hash,
duplicates/missing/large ordinals, golden tags/bytes, malformed shape/tag/count,
truncation/budget/whole-batch roundtrip, focused/full/check/fmt/Clippy/rustdoc
и source review. ROOT sole writer; no new dependencies. Coverage gaps нет.
Provenance/JOIN/SPLIT lowering deferred до gate. L11 **0/5**.

L9m0 correctness evidence закрыт: focused 3/3 и полный lib/datastore 1083 passed/
10 ignored/0 failed/0 filtered PASS с command-local test debug=0/j1; default
debug-symbol test build остаётся OOM tooling limitation. Rustdoc LIVE 15/15 PASS
в том же profile. Final default metadata check PASS, 5.58s; scoped fmt 6 files
PASS. Fresh source review PASS; Clippy repaired added0 при existing6954/544.
Protected43 unchanged. Graphify AST14/14, 57962 nodes/156158 edges/1912 communities
PASS; zero-node context7.json/config.json limitation сохранена. Producer/lowering
не включён. Stage OPEN, L11 **0/5**.

L9m1 уточнён по actual consumers: сначала L9m1a mechanical extraction общего
WHERE/SPLIT row preparation без behavior changes (owner `split-kernel`
rust-database, move/borrow mechanics остаются в existing generic QueryRead
contract). Затем L9m1b single-source/graph-union SPLIT identity, где существующий
stable key уже сохраняет original RecordId, positions и separate output ordinal;
graph scope известен из каждого plan. JOIN qualification остаётся отдельным
следующим kernel: его scopes/LEFT/CBO нельзя вывести из первого stable key.
Такой порядок избегает Row provenance allocation для SPLIT, где extra field
не требуется. Shared WHERE/SPLIT helper получит реальный второй consumer в
aggregate class union после mechanical gate. RuleSet type-result-fallible;
acceptance L9m1a existing SELECT/window + full lib с exact profile evidence,
check/fmt/Clippy. Сначала refactor, потом расширение semantics. Coverage gaps нет.
L11 **0/5**.

L9m1a mechanical gate PASS: focused26/26, full1083/10 ignored/0 filtered с
test debug=0/j1, default check4.89s, fmt1 file, Clippy added0. L9m1b pre-edit
TaskBrief: owner `split-kernel` rust-database; existing ownership/Number helpers
сохраняют checked exact positions. Один shared root identity converter читает
original RecordId и ровно N SPLIT positions из anchor, игнорируя final output
ordinal. Single source graph scope известен plan; JOIN guard сохраняется.
Row union допускает SPLIT через existing eager barrier fallback и global window;
aggregate union использует prepare_select_rows до padding/grouping и очищает
повторное filter/split после merge. Старые stable/sequence keys, ordinary ordering
и transient codecs не меняются. RuleSet type-result-fallible. Acceptance:
independent SELECT oracle, одинаковые projected duplicates, compound positions,
scope/OMIT, WHERE-before-SPLIT, row/group class union, global window/ONLY, forced
spill, invalid input/resource rejection, focused/full/check/fmt/Clippy и review.
Public recipes/codec clause children deferred до kernel correctness gate.
Coverage gaps нет, ROOT sole writer. L11 **0/5**.

L9m1b focused/full/resource/re-review gates PASS после P1 shared expansion
guard; default metadata check5.85s, fmt3 files и Clippy added0. L9m2 pre-edit
TaskBrief/ProfileStack: `split-recipes` owner rust-api-design, `split-tail`
owner rust-serialization; RuleSet api-builder-pattern/api-common-traits/
type-result-fallible. Private Vec<LiveExpr>, with_split replacement и borrowed
split accessor; ordinary column/field binding/type/effect validation сохранены.
Все captures/DAG/lowering/history consumers обходят children. Changed-row replay
для SPLIT запрещён; complete-domain proof и explicit GAP переиспользуются.
FGLD final flags bit3 — nonempty SPLIT tail: positive checked u32 count, обычные
expressions depth+1. Masks0x0f/output3invalid; reserved bits4..7zero. Empty tail
не кодируется, default body27/frame67/catalog227 неизменны. No extra DTO/deps.
Acceptance: direct SDK/GQL, duplicates/compound/set/empty/NULL, fields vs invalid
operands, graph/group/window/ONLY/nested, saved-result reopen/ACK, historical
complete-domain/no-anchor GAP, golden/invalid counts/flags/depth/budgets,
focused/full/check/fmt/Clippy/rustdoc и fresh review. Scout завершён; coverage
gaps нет. ROOT sole writer, L11 **0/5**.

L9m2 SPLIT correctness gate закрыт: focused34/full1091/10 ignored/0 filtered
с test debug=0/j1; Sync/NativeAsync1/1 каждый, rustdoc16/16, default check/fmt и
Clippy added0. JOIN work уточнён по read-only scout: eligible root atom scopes
не могут зависеть от joined row columns (analyzer передаёт outer=None), physical
layout сохраняет logical source column offsets независимо от CBO. Сначала
L9m3a извлекает schema/source copy admission без semantic changes (owner
`join-copy`=rust-engineering:refactoring), затем общий helper используется для
joined schemas/aliases/scopes/predicates (resource owner=rust-api-design,
existing RootRead policy). Runtime Value/Row/spill/ordering bytes не меняются.
Acceptance: focused characterization, explicit joined-copy budget rejection,
check/fmt/Clippy delta и bounded source review. L11 **0/5**.

L9m3a JOIN-copy admission закрыт: regression1/1, window35/35, full1092/10 ignored/
0 filtered PASS с debug=0/j1; fresh source review/check/fmt и Clippy added0.
L9m3b pre-edit TaskBrief/ProfileStack: `joined-input` owner=refactoring, contract —
выделить concrete prepare_select_input из execute_select_output с прежними
source scans, JOIN operators, outer-row append и prepared search ownership.
Обычный consumer вызывает новый helper перед finish_select_rows; будущий реальный
второй consumer — class aggregate union, где joins сейчас запрещены. Mechanical
gate выполняется до расширения этого consumer. Query values/ordering/errors,
public types и durable bytes сохраняются; new Row provenance/spill layout не нужны.
Acceptance: existing window и ordinary JOIN focused, default check, scoped fmt,
Clippy delta, bounded diff inspection; затем отдельный root identity behavior gate.
Source scope scout подтверждает outer=None для root atom sources и fixed logical
offsets; ROW-free graph scopes можно freeze один раз на copied plan. L11 **0/5**.

L9m3b extraction gate PASS window35/ordinary SELECT36/check/fmt/Clippy added0.
L9m3c pre-edit TaskBrief/ProfileStack: `join-identity` owner=rust-api-design,
coding serialization только для private temporary anchor envelope; approved
public Composite parts не меняются. Root-only context фиксирует row-free atom
scopes один раз на copied plan, logical source offsets и SPLIT count. Source
tuples/positions захватываются до projection, suffix добавляется после прежнего
unique output ordinal. Row/spill layout и ordinary/nested value execution
сохраняются. Graph-class row union применяет global tie ordinal перед suffix;
aggregate union переиспользует общий prepared input до WHERE/SPLIT/padding/group.
LIMIT0 проверяет все root JOIN scopes до row pull. Derived/graph-metadata source
provenance не угадывается; они остаются вне owned LIVE source contract.
Acceptance: qualified/base/LEFT/empty/CBO/spill/SPLIT/OMIT/ORDER/union/group,
all-source LIMIT0 и malformed/budget/cancellation guards; focused/full/check/fmt/
Clippy, fresh source review. Public JOIN recipes/codec tail deferred до этого gate.
L11 **0/5**.

L9m3c root JOIN kernel correctness/resource gate PASS8/full1099/10 ignored/
0 filtered, default check/fmt и Clippy added0. J1 scope-once, J2 pair-before-clone,
J3 full ON dependency walk и J4 prefix scheduling закрыты fresh source/runtime
evidence. No Row/private row spill/public Value changes; public JOIN recipes и
FGLD capture/lifecycle integration остаются следующим срезом. L11 **0/5**.

Public JOIN scope policy выбран пользователем 2026-10-07: computed right-hand
String scope сохраняет last successful GraphId/name до смены значения, как
primary source. Rename/remove/recreate не перепривязывает identity. Каждый JOIN
source получает собственный durable anchor, не общий primary capture owner/name.
Primary capture policy остаётся отдельной; nested scope behavior не меняется
случайно через primary slot. Before API acceptance нужны slot-owned frozen
captures/record targets, source-aware alias lowering и right union relation для
LEFT over multi-class graph: cartesian class branches не дают exact LEFT.
Zero-class graph axis сохраняет universal atom schema и explicit Empty source.
L11 **0/5**.

L9m4a pre-edit TaskBrief/ProfileStack: `source-codec` owner=refactoring,
coding=rust-serialization. Выделить encode_source/decode_source из общего FGLD
query codec перед добавлением реального JOIN consumer. Tags 0..7, dictionaries,
record copy admission, expression depth и Work charges сохраняются побайтно;
default frame67/catalog227 не меняются. Никаких новых DTO или dependencies.
Gate: existing descriptor golden/malformed/limit tests, default check,
scoped nightly fmt, Clippy delta и mechanical diff inspection. Root sole writer.
Public JOIN acceptance начинается только после отдельного codec/API/lifecycle
gate; stage28 OPEN, L11 **0/5**.

L9m4a source codec gate PASS20/check/fmt/Clippy added0; bytes/depth/work preserved.
L9m4b pre-edit TaskBrief/ProfileStack: `source-conversion` owner=refactoring,
coding profiles для borrowed recipes/typed errors. Выделить concrete syntax-source
conversion и SDK source choices из single-source query builders. Прежние budget
charges, static GraphRef refresh, captured record class incarnation, graph-wide
class namespace и Empty axis сохраняются. Только реальные существующие single-
source consumers, затем JOIN использует эти же helpers; public surface не меняется.
Gate: direct recipe/lowering tests, descriptor characterization, default check,
scoped fmt/Clippy delta и bounded mechanical diff. L11 **0/5**.

L9m4b source conversion gate PASS SDK31/GQL3/check/fmt/Clippy added0.
L9m4c pre-edit TaskBrief/ProfileStack: `frozen-source-owner` owner=rust-api-design,
coding=rust-ownership; internal logical IR, не public Value/storage DTO. Заменить
shared frozen_graph Option на ordered source-owned captures с declaration slot.
Primary slot0 сохраняет прежнюю registration semantics; unknown/duplicate slots
отклоняются до scope evaluation. Analyzer допускает capture storage до push;
deterministic visitor проверяет каждое выражение. Это prerequisite для выбранного
RHS scope contract, не durable computed-anchor implementation. Gate: direct
multi-slot conversion, existing frozen scope/SSI tests, full lib, check/fmt/
Clippy delta. Public JOIN всё ещё отклоняется до собственного acceptance gate.
L11 **0/5**.

L9m4c source-owned frozen registration gate PASS4/full1101/10 ignored/0 filtered,
check/fmt/Clippy added0. Public JOIN integration, right union relation и durable
computed RHS anchors остаются OPEN; internal vector не является доказательством
поддержки JOIN SDK/GQL. L11 **0/5**.

L9m5a pre-edit TaskBrief/ProfileStack: `source-alias` owner=rust-api-design,
coding=rust-serialization для FGLD. Сохранить explicit primary alias в owned
LiveQuery (with_alias/alias); qualified Field path остаётся qualified при GQL
conversion. SDK lowering различает public alias и существующий уникальный
synthetic lexical owner, чтобы nested одинаковые aliases не смешивали captures.
FGLD bit4 означает checked nonempty alias tail после SPLIT; bits5..7 reserved.
Без alias default frame67/catalog227 неизменны. Source identity от alias не
зависит. Реальные consumers — SDK и GQL single-source recipes; JOIN reuse после
отдельного acceptance. Gate: alias/field/capture shadowing differential, codec
golden/malformed/budgets, rustdoc, full/check/fmt/Clippy. L11 **0/5**.

L9m5a A1 finding accept: сохранённый alias prefix не распознаётся старым row-only
historical proof, поэтому exact changed-row interval ошибочно требует creation
anchor. Сначала выделить существующее lowering rule в borrowed
source_field_segments, затем использовать его в proof с alias context и bounded
schema work. Primary class-name property precedence сохраняется; foreign record
navigation остаётся ineligible. Acceptance: failing-before/passing-after SINCE
без creation anchor, eligibility matrix, весь backfill и full lib gate.
L11 **0/5**.

L9m5a source alias gate PASS: SDK/GQL/codec, nested capture identity, A1 changed-row
SINCE without creation anchor, backfill25/full1104/10 ignored/0 filtered,
Sync/NativeAsync replay1/1 каждый, rustdoc17, default check/scoped fmt и
Clippy added0. Shared qualifier rule сохраняет declared class-name property
precedence и foreign navigation proof. Public JOIN, right union relation и
durable RHS computed anchors остаются отдельным следующим срезом. L11 **0/5**.

L9m6a pre-edit TaskBrief/ProfileStack (2026-10-08): requested_now — завершить
Stage28; текущий срез — owned JOIN recipes и общий bounded descriptor/dependency
traversal. `join-api` owner=rust-api-design, `join-bytes` owner=rust-serialization;
оба нужны для атомарного runtime/codec contract. Coding: private checked fields,
owned aliases/predicates, explicit canonical tags/counts. RuleQuery:
api-parse-dont-validate, api-common-traits, api-builder-must-use. Coverage gaps=[];
acceptance — direct constructors/borrowed accessors, all sources/ON dependencies,
codec golden/default67/malformed/depth/byte limits, check/fmt/Clippy/rustdoc.
MAIN sole writer; scout/reviewer read-only по high-risk workflow.
FGLD bit5 — nonempty ordered JOIN tail после alias: checked positive u32 count,
kind 0 INNER/1 LEFT/2 CROSS +3 reserved zero bytes, source alias UTF-8 blob,
существующий source codec, optional ON flag/expression; INNER/LEFT требуют ON,
CROSS запрещает ON. No new dependencies/serialization framework.
Регистрация не считается поддержанной до следующего exact relation/lowering
gate. Polymorphic RHS LEFT требует raw atom union перед JOIN; existing Derived
не заменяет её и не сохраняет provenance. L11 **0/5**.

L9m6b pre-edit TaskBrief/ProfileStack: `atom-source-factory` owner=refactoring.
Выделить existing concrete SelectSource construction из SDK selects_inner:
class/Empty, GraphRef/expression и synthetic alias; без изменения charges,
evaluation order, snapshot/depth или public/durable shape. Real consumers —
primary и следующий JOIN source. Acceptance — direct lowering differential,
check/fmt и mechanical diff; затем отдельный BoundRecord slice переводит
point restriction в scan target для root/nested/JOIN вместо global WHERE.
Coverage gaps=[], source-only helper без framework. L11 **0/5**.

L9m6c pre-edit TaskBrief/ProfileStack: `bound-point-source` owner=rust-api-design;
internal syntax metadata и existing ScanTarget::Record. SelectTarget::BoundRecord
хранит только class/parameter Identifiers, не runtime/storage DTO или executable
IR. Analyzer разрешает immutable nonreactive literal Value::Record из supplied
lowering parameters и проверяет class match. Общий source factory использует
point target для всех SDK keys в root/nested/будущих JOIN; global WHERE restriction
и root-only post-analysis rewrite удаляются. Text grammar не меняется.
Acceptance: all six key families, missing/scoped/nested point targets, invalid
binding/class/mutable metadata, source reads без whole-class scans; full/check/
fmt/Clippy. Source factory mechanical gate PASS32/check; coverage gaps=[].
L11 **0/5**.

L9m7a pre-edit TaskBrief/ProfileStack: `join-lowering` owner=rust-api-design,
coding=rust-ownership для lexical contexts. Common source factory и ordinary
SELECT analyzer/JOIN operators — единственный execution путь. Multi-source
fields сохраняют public qualifiers и bare names; analyzer проверяет prefix ON,
duplicate/ambiguous names. Synthetic parameter owner остаётся unique per query,
не user alias. Class/record RHS, static graph scopes и graph-wide primary union
исполняются теми же kernels; polymorphic RHS и computed RHS temporary guard
до exact union/anchor slices. Guards не игнорируют запрос и удаляются только
после соответствующего correctness gate. Acceptance — SDK/GQL inner/left/cross/
self, exact absent RHS, aliases/nested/SPLIT/group/window/qualified identities,
budgets и ordinary SELECT differential. Coverage gaps=[], L11 **0/5**.

L9m7b pre-edit TaskBrief/ProfileStack: `rhs-scope-owner` owner=rust-api-design
для root JOIN declaration metadata; `rhs-anchor-bytes` owner=rust-serialization
для durable catalog tail. GraphId/name last-success pairs принадлежат positive
source ordinals, primary capture policy остаётся отдельной. Unchanged String
operand удерживает GraphId при rename/recreate; failed evaluation сохраняет
старые anchors и fault, value change допускает новый GraphId. Nested graph inputs
сохраняют прежнюю ordinary read dependency semantics, как primary nested scope.
Catalog options byte bit0 означает nonempty RHS-anchor tail после scope-name:
positive count u32, ordinal u32/reserved4/GraphId u64/name length u32 +UTF-8
1..128. Ordinals sorted distinct и относятся к expression-scoped JOIN sources;
GraphId проверяется against allocation clock. Header160/default227 unchanged.
Existing MVCC/WAL publication/cost/admission/recovery paths сопровождают vector
атомарно с successful result. No second registry/worker/WAL. Gate: SDK/GQL
multi-scope/rename/remove/recreate/value changes, old snapshots/fault, codec
malformed/budgets, Sync/NativeAsync reopen/ACK/replay и full/check/fmt/Clippy.
Coverage gaps=[], L11 **0/5**.

Defaults: queue 10 000 entries и 64 MiB/subscription; overflow GAP;
L9m8a pre-edit TaskBrief/ProfileStack: `typed-row-selector` owner=refactoring.
Общий existing lazy CASE/aggregate merge остаётся в SELECT owner; добавить
конкретный borrowed selector для class ordinal либо ordered JOIN tag tuple.
Старые ordinal callers сохраняют прежние constructors, allocations/admission,
aggregate shape и semantics. Не переносить helper в новый project-wide модуль.
Реальные consumers — existing primary class union и следующий JOIN variant
family. Acceptance — mechanical diff, constructor/aggregate/union focused tests,
production check/fmt/Clippy. L11 **0/5**, coverage gaps=[].

L9m8b contract для polymorphic RHS: один bounded raw atom relation перед JOIN,
ProfileStack: `raw-atom-relation` owner=rust-api-design для internal source shape;
`typing-family` owner=rust-ownership для admitted borrowed plans/canonical layout;
`relational-consumers` owner=refactoring для общего scan/projection kernels перед
семантическим расширением. Syntax/IR без runtime/storage DTO; class schemas,
read cursors, RowCollector, JOIN scheduler и CASE остаются authoritative.
один scope по declaration, canonical class inputs и fixed padded width/tag.
Каждый matched class анализируется обычным analyzer отдельно; LEFT Unmatched
имеет отдельный Type::None typing state, не выбирает class0 overload. Missing/
ambiguous NONE overload — обычная registration error. Lazy selector dispatch
сохраняет class-specific arguments; aggregate nodes остаются над row CASE.
ON использует только declaration prefix, без future tags. Extra columns не
доступны name/qualifier/wildcard/identity. Checked variant fanout и все copies
допускаются до allocation; CBO не привязывает class index probe ко всей union.
Primary global GROUP/window reduction остаётся существующим SELECT kernel.

L9m8c: фабрика select_statement выделена механически; focused 38/38 и check PASS.
L9m8d завершён: polymorphic guard снят, full lib 1118/10 ignored/0 filtered,
Sync/NativeAsync по 1/1, check/fmt PASS, Clippy delta 0, fresh review PASS.

L9m9 TaskBrief/ProfileStack: `native-presentation` owner=rust-api-design;
`json-export` owner=rust-serialization; `delivery-mode` owner=rust-ownership.
Runtime presentation принадлежит live, publication/issuance — datastore.
Exact before/after остаются authoritative и durable; DIFF хранит owned path
segments и optional images, PATCH — ordered add/remove/replace с JSON Pointer.
Objects обходятся детерминированно; arrays заменяются целиком, без edit-distance
kernel. Native values сохраняются без JSON coercion. Явный JSON export принимает
только JSON-compatible values и finite numbers, с точным integer/decimal output.
Work/bytes/depth допускаются до allocation; лимиты доступны SDK. Delivery mode
берётся из immutable query version. Reader/replay используют сохранённые images,
без current primary FETCH.
Retained presentation reserve отделён от inspection/temporary scratch; queue
и общий candidate budget учитывают только owned output дополнительно к raw images.
Historical registration готовит тот же mode до whole-commit queue admission,
сохраняя на диск только authoritative raw images. ONLY shorthand использует
общий SELECT cardinality contract.

L10a TaskBrief/ProfileStack: `owned-metadata` owner=rust-api-design;
`statement-snapshot` owner=rust-ownership; `history-revisions` owner=rust-concurrency.
LiveInfo открывает authoritative starting_after/published snapshot bounds,
историю last change/purge floor/time и capture epochs, frozen PURE versions
и record namespace owners. Absent history остаётся явно None. First available
group не выводится из purge floor либо epochs. Typed runtime metadata не открывает
physical segment/page/LSN. Материализация допускает vectors/names до copies.
GQL operand и INFO читают supplied QuerySnapshot; SDK сохраняет prepare_read.
History metadata mutations отмечают existing LiveCatalogRange в том же candidate
root, metadata readers регистрируют этот predicate. ACK/close не получают FCW
конфликты на каждый source DML. Acceptance: enable/drop/re-enable, old snapshots,
partial/full purge, SSI read/write, bounded metadata и root-bound GQL UUID operand.
coverage.gaps=[]; L11 attempts **0/5**.

L10a correctness/tooling: full lib 1130/10 ignored/0 filtered, focused SDK/SSI
2/2, epochs1/1, retention3/3, GQL2/2; check/fmt PASS, Clippy delta0,
rustdoc20/20. L10-M1 epoch capacity fixed with exact reservation; re-review PASS.

L10b TaskBrief/ProfileStack: `process-metrics` owner=rust-observability;
`statistics-sdk` owner=rust-api-design; `first-issue-and-publication` owner=rust-concurrency.
Один constant-size StoreInner counter owner, без per-LiveId labels/payloads.
Переиспользовать existing saturating helper и backend barriers. Попытки подготовки
учитываются до outcome; publication/ACK — только после успешного barrier, source
registration не считается ACK. Repeated next/ACK не удваивают counters.
Query/binding successful evaluator work отделяется от unknown failed partial work;
scratch метрика описывает admitted reserve, а не RSS. Replay использует independent
existing observed-reader scope; page requests включают cache/WAL и failed requests.
Queue gauges берутся под existing mutex и различают queued data entries от полной
consumer retention/controls. Метрики не являются durable bounds и не входят в hash.
Acceptance: disabled LIVE, retries/faults/failed publication, idempotent next/ACK,
replay RAM/local, import ON/OFF, reset-on-open и bounded gauges. L11 **0/5**.

L10b correctness: full lib1134/10 ignored/0 filtered; statistics3/3, queue fallback
1/1, Sync/NativeAsync1/1 each. L10-C1 backfill totals fixed via fresh ranges;
re-review PASS. First fallback issue also marks revision before later reservations.

L10c TaskBrief/ProfileStack: `physical-live-contract` owner=rust-api-design;
`canonical-borrowed-descriptor` owner=rust-serialization; `shared-encoder-extraction`
owner=refactoring; `fixed-root-planning` owner=rust-ownership. Общий query executor
сохраняется. Один borrowed command selector охватывает Schema и LET/Live wrappers.
PhysicalPlan::Live отражает registration/binding validation/publication, read-only
INFO, ACK и capture-stop. PlanHash version10 включает logical descriptor/captures,
source/scope/output/SINCE/budgets/functions и fixed snapshot; queue/process counters,
ACK watermark и current binding/history results исключаются. FULL не исполняет UDF.
Existing FGLD encoder получает borrowed Query/Binding view; bytes/validation прежние,
typed IR не сериализуется, второй Expr walker не создаётся. Deferred operand spelling
принадлежит analyzer source, допускается до копии и переживает saved function lowering.
Acceptance: hash repeat/change matrix, no-side-effects FULL/LOGICAL, one ANALYZE,
old snapshot metadata, resource/cancellation и borrowed/owned descriptor equality.
coverage.gaps=[]; L11 **0/5**.
Shared B-tree reserve извлекается механически из values/storage в memory facade;
прежний internal re-export сохраняет callers, behavior не меняется. Renderer
использует ту же оценку complete root/split nodes вместо отдельной формулы.
Gate: enter/update/leave, escaping ~/empty keys,
nested objects/arrays, native non-JSON values, >2^53, budgets и durable replay.
L9m8d TaskBrief: lowering допускает Cartesian class typing states; analyzer
сохраняет обычные overloads и объединяет result types; SELECT preparation
сворачивает family в один JOIN до pull. Wildcard выбирает существующий field-list
projector. Scope freezing остаётся по declaration. Acceptance: LEFT, aggregates,
nested unions и resource rejection. Stage OPEN; performance attempts 0/5.

Concrete family coordination: private syntax/IR union-purpose отделяет
IndependentOutputs от JoinedTypingVariants; members не исполняются как отдельные
JOIN outputs. SDK source AtomUnion содержит canonical class choices и выбранный
typing state (class ordinal либо NONE); typed raw source держит ordinary scan
targets, один graph scope и fixed tag column. Width допускается по всем class
schemas до copies. Padding/tag names начинаются NUL и исключены из всех schema
name/qualifier lookups и wildcard. Matched rows сохраняют class-local property
positions, дополняются NONE и canonical class tag перед RowCollector/JOIN.
Сложность typing Cartesian product имеет checked work/scratch gate до build.

Family collapse выполняется общим SELECT preparation до optimizer/pull для root
и nested consumers. Canonical tuple tags dispatch через тот же lazy CASE helper;
NONE slot не преобразуется в class0. ON j читает только source-prefix tags.
Row clauses используют whole-expression dispatch; grouped output использует
existing aggregate merge. Wildcard variants выбирают обычный projection list
строки с сохранением paths и projected alias positions, без нового evaluator.
Result types widened обычным unify, чтобы nested VALUE не объявлял только первый
class type. Raw relation не получает global WHERE как per-class predicate и не
использует ложный class-specific probe; class cardinalities summed checked.
Existing qualifier errors, exact RecordId targets и root RHS anchors сохраняются.
Gate: LEFT A-match/B-miss, WHERE rejects matched A, all-unmatched, consecutive
polymorphic LEFTs, class widths/PURE overloads/NONE ambiguity, sum/count/distinct,
ORDER alias/ONLY/offset/spill/CBO, empty axes/LIMIT0-invalid scope и nested unions.

retained session 256 MiB; evaluation scratch 256 MiB/work 1M units
с сохранением специализированных query limits. BLOCK timeout 30s либо более ранний
transaction deadline. Commit не режется; GAP/fault используют reserved control slot.
History делит общий WAL cap 256 MiB с primary/catalog. Hard caps до allocation/write.

Rule-to-test matrix включает SDK owned lifetime/wrong origin/version/ACK/stamps >2^53;
result oracle WHERE/FETCH/negative lookups/overlays/qualified identities;
LET/LIVE LET chains/literal/query replacements/parameter-only commits/cycles/rollback;
registration/catch-up/ACK/close/BLOCK/import/root races/cancellation/shutdown;
WAL failure/crash/reopen/checkpoint/generation replacement/pins/corruption;
oversized commits/dependency fanout/fault при успешном DML и отсутствие partial publication.
Paired 10K/100K/1M: disabled LIVE, 1/10/100 subscribers, FETCH fanout, cascades,
replay/ACK/import, warm/cold; latency/page reads/WAL bytes/queue/work/peak memory.
Непрофильные regressions ≤5%; pre-existing stage-27 results учитываются отдельно.

После срезов focused tests, cargo check --lib --no-default-features, nightly rustfmt,
Clippy. Closure: полный lib/datastore gate, separate RAM/Sync/NativeAsync targets;
ноль выбранных тестов не является evidence. Перед commit just all-check с сохранением
несвязанных dirty bytes. Новых dependencies/WAL/worker/query executor нет.
Русский report, specs/AGENTS/registry/full_plan и graphify update . обязательны.
Любой открытый correctness/resource/performance/tooling gate сохраняет stage OPEN.

L10c registration-inputs refinement: owner=rust-ownership. Existing Analyzer
parameter resolution optionally records a pre-admitted mask only during LIVE
option preflight; closure children share it, new local ordinals are excluded.
Named LogicalLiveCapture tuples preserve lexical correspondence without a second
Expr walker. Registration options hash reactive identity and snapshot value;
query captures retain reactive identity only. Transient metadata/encoder/hash
accounts release before execution; retained plan/output account survives rendering.
Focused EXPLAIN3/3, descriptor24/24 PASS; fresh review PASS after both hash fixes.
Full lib1138/10 ignored/0 filtered PASS; check/fmt PASS, Clippy delta0 (6951/544 existing baseline). Local Sync18/18 PASS; Native/RAM/docs/Graphify pending. L11 **0/5**.

L11 TaskBrief/ProfileStack: `paired-regression` owner=rust-performance;
`benchmark-fixtures-and-oracles` owner=rust-testing; `harness-evidence` helper=rust-verify.
No optimization before measurement/profile. Resolved Criterion0.8.2 (manifest minimum0.8.1), production
mimalloc-pprof, stable1.98.1/MSVC, default-feature-free bench profile (opt3/fatLTO/
codegen1/debug2) retain paired baseline settings. Dataset10K/100K/1M. Same controlled
machine/process sampling on baseline/current; benchmark startup/setup excluded from
Criterion latency, process peak RSS includes fixture/setup and is labelled separately.

Unchanged gql_stage27_common is disabled-LIVE baseline/current for RETURN1, read16,
mutate64. New gql_stage28 is distinct subscriptions1/10/100, hot64 class scan,
FETCH boss fanout, LIVE LET data/parameter cascades, history replay/ACK and import
ON/OFF. Whole-commit oracle precedes timing and rejects GAP/fault as success. ACK
outside mutation timer prevents changing queue backlog. Cold engine caches after
reopen are distinct from unproven cold OS/device caches. Replay uses separate
attachments (repeated unacked next is cache access, not repeated decoding).

Default work/scratch/resource rejections at100K/1M remain explicit labelled cells;
no silent budget increase or incomplete result is a successful sample. Peak admitted
scratch is not RSS, page requests include resident/WAL hits, logical history bytes
are distinct from physical WAL/feed bytes. Retained session/queue gauges are sampled
outside timed work. One numbered gate attempt contains the complete defined paired
matrix and verdict; build/check/test/harness smoke alone do not increment attempts.
Existing baseline debt stays separate. Attempt counter remains **0/5**.

L11 resumed build slice: owner=rust-cargo-build, helper=rust-verify.
Default debug2 optimized builds exhausted Windows commit memory twice before gate.
Both196d21b baseline and current are rebuilt with identical command-local bench
`debug=0`; opt3/fatLTO/codegen1, compiler/allocator/target/features stay unchanged.
Runner requires explicit BaselineBinary to prevent mixing archived debug2 with new
debug0. Build preparation is not a gate attempt. Library/src Rust bytes remain stable.

Build-state isolation: existing `debug0-paired` directory belongs exclusively to
baseline196d21b; current uses separate `debug0-current`. Neither directory is a
cross-worktree compiler cache. Serialized Cargo commands preserve profile/config
identity and archived executables. Verify final hashes/provenance before runner.

L11 closure findings: C28-1 accept (durable close after transient terminal/fused
consumer); owner=rust-concurrency. Separate handle fused polling from confirmed
capture stop before behavioral fix. Failed close preserves state; only successful
control commit establishes idempotence. Regression: drain DISCONNECT/ACK/Closed,
close, inspect Closed definition and zero further evaluation; repeated close after
store drop. C28-2 accept (metadata docs): owner=rust-documentation. LiveInfo.bindings
means current snapshot direct dependency revisions; LiveBatch.bindings owns the
actual evaluated revisions. No catalog/layout/API types change. coverage.gaps=[];
performance attempts remain0/5.

L11 C28-3 TaskBrief/ProfileStack: confirmed cumulative transient row charging.
Owner=rust-ownership; retained-result identity accounting owner=rust-api-design;
regression fixtures owner=rust-testing; runner bounded failure handling
owner=rust-concurrency; evidence helper=rust-verify. First numbered attempt is
Failed with incomplete matrix, not reset: nine common pairs fail/inconclusive
against5%, 100K scratch rejections, 1M import terminated after15min before oracle.
Root is sole writer; bounded reviewer is read-only.

Refactor existing identity estimator into LiveIdentity internal responsibility and
separate retained admission before changing lifetimes. Then reuse BarrierAccount
via drop-scoped transient guard only for direct root streaming Class row callbacks.
Nested SELECT/index/union/window/JOIN/materialized collection retains existing
conservative policy. Persistent result admission includes both identity and value;
work/byte limits are not raised. No shared token across infallible deep Row clones.
Focused gate: discarded rows under tight bytes, retained outputs still bounded,
oversized row rejected before copy, nested output retained, early exit/error/
cancellation guard cleanup. Apply behavior only after mechanical gate.

Runner records explicit timeout as unsuccessful/incomplete evidence, never a
successful resource oracle. Checked owned process termination, complete ledger,
max5 attempts and mandatory hashes remain. System.DateTime JSON parsing must not
invalidate actual process-start identity comparison; compare normalized UTC times.

Current closure snapshot2026-10-09: C28-3 resource6/6, full lib1144/10 ignored/
0 failed/0 filtered, public RAM10, Sync+NativeAsync36, SDK rustdoc21 PASS.
No-default check/scoped nightly fmt PASS, Clippy added0 (6951/544 existing debt).
Final just all-check: default check/fmt PASS, global Clippy6989/537 FAIL;
formatter rewrites4 unrelated files restored,633 source hashes verified.
Paired attempt2 completes72 cells/85m21s:30 measured,26 capacity gaps,
4 timeouts,6 import probes,6 resource probes; common2/9 criteria PASS.
Verdict FAIL_OR_INCONCLUSIVE; counter2/5, attempts3–5 not used without a new
confirmed hypothesis. Default limits remain; NativeAsync performance/cold OS proof
unmeasured. Stage28 OPEN, no commit/PR. Exact logs/CSV/provenance are linked by
current report; do not read historical0/5/pending entries as current authority.

Continuation2026-10-09: пользователь снял лимит performance attempts. Номера
прошлых runs остаются provenance, max5 guard и stop-after5 policy отменены.
Current TaskBrief: довести stage28, профилировать ACK/control publication и import,
устранить подтверждённые costs, затем повторить correctness и paired gates.
Events/LiveGraph stage29 deferred. No new dependencies/WAL/worker/executor.

Phase `runner-limit`: owner=rust-concurrency; preserve exclusive lock, sequential
ledger, immutable binary digests, negative timeout, owned cleanup and failure
semantics. Acceptance=actual AST helper selfcheck admits13 sequential runs and
rejects empty/out-of-order history; legacy attempt artifacts unchanged.
Phase `control/import-profile`: owner=rust-performance; symbol-build owner in a
separate build phase=rust-cargo-build. Root sole writer; bounded scout is read-only.
Acceptance=installed VTune own-process sampled call-tree with exact profiling
EXE/PDB and local report; no speedup from source appearance. Structural refactor
and Rust coding profiles chosen after measured hotspot. coverage.gaps=[].

C28-4 revision ownership TaskBrief/ProfileStack: owner=rust-ownership, helper
refactoring(move-before-edit), FCW/SSI preservation constraints covered by current
MVCC contracts, evidence helper rust-verify. Profiling measured mark_version COW
clone~71% of sampled ACK subtree at100K; raw VTune/PDB report retained.

Change slice: private VersionStore, DatabaseRoot lookup/mark/rebase, retention count,
existing import revision tests. Public Value/RecordId/LiveCursor and physical
codecs unchanged. Standard BTreeMap+Arc reuse fits exact order/snapshot semantics;
no unused crate, new dependency or generic persistent-map framework.
First compile mechanical encapsulation with deep Clone unchanged. Then selective
COW domains for LIVE control keys; ordinary root keeps one outer shared handle.
Acceptance: complete conflict-key reference oracle, old snapshots unaffected,
control write preserves primary map identity, noncontrol write preserves live map,
import latest controls/restamp/resource rejection before COW, existing FCW/SSI/
LIVE ACK/recovery/backend tests. No reuse of transaction.working over current root.
Root sole writer; bounded high-risk scout/reviewer read-only. coverage.gaps=[].

C28-5 checkpoint frame-read TaskBrief/ProfileStack: measured Sync100K ACK
checkpoint CPU91.19%; frame load repeatedly opens/closes segment files. First
phase owner=refactoring: move existing seek/exact-read/page/LSN validation into
borrowed-file helper without behavior changes. Focused WAL gate before scope.
Second phase owner=rust-concurrency; supporting rust-ownership for scoped File
and prune guard lifetimes, rust-performance for paired evidence. Reuse one
bounded last-segment handle per backfill call; no global cache or dependency.
Preserve target/frame order, full validator/error contexts, home sync,
CheckpointEnd/superblock, cancellation and pruning. Scope drops before publish
checkpoint exclusive gate; cached File drops before shared guard. No threshold
changes, silent checkpoint skipping, second WAL/worker. Root sole writer;
read-only bounded scout/reviewer. Tests: reuse/segment switch/backward seek,
malformed page/LSN/short read, error Drop releases guard, existing pruning/reopen.
coverage.gaps=[].

## Финальное состояние L11 — 2026-10-10

Новый лимит source optimizations исчерпан, **STOP5/5**. Сохранены C28-15 и
C28-19; C28-16/17/18 откатаны. Final correctness: full lib1165/10 ignored,
backend10/38/16 PASS, no-default check/scoped nightly format PASS, Clippy delta0.
Final default runner6: 72 cells без timeout, common5/9 PASS против196d21b,
FAIL_OR_INCONCLUSIVE. Mandatory just all-check: check/fmt PASS, global Clippy
6989/537 pre-existing FAIL; protected646 Rust files восстановлены точно.
Фиксированная large-work100K/1M RAM/Sync матрица завершена:24/24
MeasuredWithOracle, по10 samples, без timeout/fault/GAP и retries.
Final source627/protected646 hashes неизменны. Новых оптимизаций не вводится;
performance/global tooling gates сохраняют этап OPEN.

C28-6 empty index replacement TaskBrief/ProfileStack: profiling phase owner
rust-performance, captured RAM importOFF100K35s (exit4 on bounded stop, not a
completed import oracle). Exact symbolized import subtree clone+drop96.55%;
setter plan is empty in no-explicit-index fixture. Implementation owner
rust-ownership; existing datastore atomic/fallible-preparation contract preserved.
First add PreparedReplace::is_empty accessor and no-default check (move before
behavior). Then guard only index clone/detach/application in change_base_property
and replace_occurrence; nonempty plans retain previous staging until statistics
preparation succeeds. Never early-mutate indexes or skip actual property/overlay,
statistics, validation or artifact invalidation. Count/BASE/EFFECTIVE/Unique and
old snapshots must remain exact. No layout/dependency change; primary and
capture budgets unchanged. Focused index/overlay/failed-unique tests plus existing
normalized/reopen/import suites, measured import probes afterward. Root sole
writer; bounded read-only scout/reviewer. coverage.gaps=[].

C28-6 debugging addendum: first focused3/5 PASS; two published Arc assertions
failed despite empty setter. Reproduction traced prepare_write/commit_prepare
mark_deferred_indexes_stale unconditional schema/index COW. Immediate-only
fixtures verified from constructor defaults. Add exact absence-of-deferred
preflight and skip index COW only when newly_stale empty; repeated Stale affected
IDs/version tuples continue catch-up reporting. Add dedicated reinvalidation
sharing test. This is same empty-work ownership slice, no deferred policy change.

C28-7 disabled-LIVE TaskBrief/ProfileStack: measured full run3 common literal
mean1.307x10K/1.273x1M, 100KCI inconclusive; mutate all3 PASS and point mostly
PASS. Discovery owner rust-performance; separate symbol-build owner
rust-cargo-build with existing profiling profile. Scope exact unchanged
common return_literal→Datastore.execute/transient session/readonly transaction/
analysis/constant/outcome/readonly commit. Source scout candidates are allocations
of empty BindingTypes, control cloning and unconditional timestamps; no source
cost is assumed causal before readable-symbol own-process capture. Preserve all
lexical types, snapshots/savepoints, cancellation/deadlines, metadata budgets,
LIVE publication/control timing and ordinary query results. No new dependencies,
registry/WAL/worker/executor; keep fixed binary profiles for paired evidence.
Root sole writer; bounded scout/reviewer read-only. coverage.gaps=[].

C28-7 profile decision: normalized RETURN capture resolves Rust/source. New
with_runtime Instant subtree1.143% capture and commit clock0.653%; lexical
BindingTypes default allocation resolved under transient session, constructor
malloc subtree0.274%. These candidates do not explain full30% latency alone.
Do NOT remove control.started: restrict_timeout must preserve original origin
for named/late TIMEOUT. Unit lexical-empty owner=rust-ownership: encapsulate
map reads/clones, then lazy Option<Arc> default with no entry allocation; retain
rich types, reactive provenance, shadowing/savepoint sharing and pre-copy
admission. Unit readonly-commit-clock owner=rust-concurrency: only mutations or
change_journal.intents can produce writes/Full capture in any root. Start BLOCK
origin at unchanged commit entry for all such transactions, skip clock for
true read-only path. Any live publication retains absolute deadline/cancellation
and initial origin across retries. No public API/codec/dependency change.
Helpers refactoring(move before behavior), rust-verify; root sole writer,
bounded read-only review. Paired common measurements required; no speedup claim.
coverage.gaps=[].

C28-8 causal literal comparison TaskBrief: fixed CPU0 reversed-order diagnostic
confirms residual1.130–1.145x ordinary RETURN1. Discovery owner rust-performance;
symbol-build owner rust-cargo-build. Reuse attached managed baseline exact
196d21b and unchanged fixture. Build artifacts only under workspaceD. Compare
readable before/current subtrees with identical opt3/ltofalse/cgu16/debug2;
latency stays paired fatLTO/cgu1/debug0. Do not conflate profiles, remove
required timeout origin, alter source query semantics or hide CI failures.
Control-scalar/Arc clone and other fixed-work candidates need exact lifetime
scope before any implementation. Root sole writer; read-only scout/reviewer.
coverage.gaps=[].

C28-8 baseline profile comparison: source-resolved helper control.clone subtree
current1.258% capture and absent in baseline helper; readonly candidate path
reads observed root before commit lock then current root under lock, baseline
uses only locked root. Profile percentages are normalized, not elapsed latency.
Two implementation units: session-control owner rust-ownership, preserve owned
per-statement lifetime through closure nonce cleanup, mutably borrow in explicit/
autocommit helpers, infer flag only for autocommit, retain named timeout through
metadata export/ACK/commit. Mechanical mutable-borrow signatures keep clones
until no-default gate. Readonly-root owner rust-concurrency: only empty mutations
AND empty targeted journal intents guarantee no publisher in ANY concurrent
root; locked validation and interruption checks still required. All potential
FULL/no-op/live/control writers retain old retry/reservation path and entry origin.
Exact baseline/current pair/artifacts retained. Root sole writer, bounded
read-only review; no new modules/WAL/worker/dependency or public API changes.
coverage.gaps=[].

C28-9 empty accounting TaskBrief/ProfileStack: C28-8 fixed CPU0 RETURN residual
mean1.063–1.064, upper1.074–1.100, so5% still open. Current profile/paths show
prepare_live_acks even when empty and metadata snapshot BarrierAccount acquired
for ordinary commands while callback uses it only for metadata_only. Owner
rust-ownership: use Option accounts, retaining every nonempty pre-copy admission
and owner lifetime through evaluation/ACK publication. No cancellation or
budget bypass for real payloads; zero-byte reserve already always true.
First mechanical Option<BarrierAccount> representation remains always Some
until check. Then initialize metadata account only metadata_only and return
empty ACK with None; existing full/metadata/resource/authority gates required.
Do NOT change statement clock origin or tighten deadline silently. Root sole
writer; bounded read-only reviewer; performance experiments retain negative
results and profiles. No new API/dependencies/WAL/worker/executor. coverage.gaps=[].


C28-10 workload budget TaskBrief/ProfileStack: C28-9 release PASS11m02;
fixed CPU0 RETURN pairs mixed (mean1.027 upper1.037 PASS; mean1.078 upper1.105
FAIL), so common5% remains OPEN. All24 default100K/1M class LIVE workloads
reject registration on work1M. Owner rust-performance: add separately named
opt-in `FG28_BUDGET=large-work` workload using existing LiveOptions work100M,
retaining scratch256MiB/queue defaults. No production default or kernel change.
Keep default matrix and exact full-commit oracle; successful registration alone
is not a measured commit. Named Criterion group isolates budgeted estimates.
Rule constructs: explicit configuration parsing/errors, bounded owned handles,
pre-timing oracle/fault validation; helper rust-verify after measured trigger.
Scope: one register caller/benchmark docs, no SDK/storage changes. Acceptance:
benchmark check/scoped fmt, invalid-profile rejection, default+large-work smoke,
1M bounded registration/commit probe. No new dependency/worker/WAL. Root writer;
read-only scout confirmed existing range coverage; coalescing deferred until
actual scratch failure reproduced. coverage.gaps=[].


C28-11 dependency range TaskBrief/ProfileStack: C28-10 explicit work100M/scratch
256MiB 1M probe reproduces registration rejection on dependency scratch32MiB.
RootRead full global CLASS scan records BaseClassRange before pull; create,
set/remove base property and delete stamp that range. Owner rust-concurrency:
retain one conservative class-range read instead of covered same-class BaseAtom
keys in private LIVE recorder. Lookup borrowed from the exact root; no global
SSI change, no graph-range coalescing, no missing-record suppression. Schema,
dictionary, functions, bindings and out-of-domain point/FETCH keys remain.
RefCell borrow ends before external observer; existing work/control checks stay
in row reader. RuleQuery: own-refcell-interior, own-borrow-over-clone. Boundary
already encapsulated in private Reads; add only its coverage predicate and
observer guard. No new revisions/worker/dependency, no raised default budgets.
Scope: query recorder plus affected dependency/cancellation assertions and
meaningful class/point/FETCH/graph/SSI regression. Acceptance: focused tests,
full lib/backend, no-default check/fmt/Clippy delta, read-only fresh review,
same1M budgeted probe. Root writer; prior scoped scout supplied range proof.
coverage.gaps=[].

C28-11 first focused gate45 PASS/3FAIL exposed metadata-range origin mismatch:
RootRead schema lookup also records BaseClassRange. Mere key existence is not
scan authority. Private Reads map now has a per-key actual-scan flag (mechanical
representation check PASS); schema observations leave it false. Existing root
scan cursor notifies recorder only after global scan construction, graph/point
calls retain exact BaseAtom keys. Flag shares admitted key memory;192 bytes/key
still covers map node plus returned vector; no new serialized revision/field.
Constructor default has no observer, PURE/other consumers unchanged. Existing
scoped/point/FETCH assertions remain strict; coalescing tests must pass.

C28-11 repaired focused48/full1156/backend10+36+16 PASS and source review PASS.
Final admission refinement reserves32 additional bytes per logical key for the
boolean leaf value array/alignment, alongside prior192 tree+returned-vector
estimate; does not rely on unchanged legacy estimate covering a new value.
No markers are stored separately or serialized. Re-run final gates on this
source before optimized probe; prior32MiB capture failure retained as baseline.


C28-12 session clock TaskBrief/ProfileStack: C28-11 fixed CPU0 RETURN pairs
remain mixed mean1.021/upper1.035 PASS and1.077/upper1.090 FAIL. Current-only
with_runtime clock is source-backed; older profile locates its route, does not
prove present latency share. Scoped discovery traced the sole production
restrict_timeout call to parsed Statement.timeout; nested blocks/functions/
MATCH/import use checked_child invocation clocks, independent of started.
Owner rust-concurrency: reuse session's already captured elapsed origin ONLY
when execution options and parsed statement both have no timeout. Timed paths
and generic new/with_runtime preserve their old origin; no late restart.
Representation stays Instant; no public API/ABI or block/publication origin
changes. First extract shared private initializer with optional supplied origin,
generic callers always None/check. Then expose session-specific constructor
which forces None for every timed case and replaces only main-loop constructor.
RuleQuery: own-copy-small, own-borrow-over-clone; helper rust-performance for
same fixed-CPU paired measurement, rust-verify for timing/cancel/control suites.
Acceptance: origin/nested-deadline unit regression, all options/session/query/
FULL/BLOCK/import full gates, backend/check/fmt/Clippy and fresh source review.
Root writer; one bounded scout supplies closed-world caller proof; no new deps.
coverage.gaps=[].


C28-13 controlled common measurement TaskBrief/ProfileStack: run4 completed
72cells/17m15/0timeouts, common5/9. Literal/point CIs broad in free scheduler,
whereas CPU0 diagnostic means0.0%/+3.2% with last upper5.18%. Owner
rust-performance: isolate all3 common workloads/all3 cardinalities on identical
owned-child affinity1 for exact baseline/current binaries, current/base/base/
current order and same50samples/10s profile. No assumption about CPU core class,
no power/priority/host changes, no Rust or production runner defaults changed.
Extend only transient target diagnostic harness with checked Rows/Filter and
900s owned-child deadline; preserve every earlier result/output. Scope controls
are experimental inputs, not proof of a speedup. Helper rust-verify checks
exit/hash/mask, independent mean/CI pairs and malformed outputs before verdict.
Default full run remains negative evidence; budgeted1M family unchanged.
No source optimization selected. coverage.gaps=[].


C28-14 large-work evidence TaskBrief/ProfileStack: controlled common14/18 CI
pairs PASS, all means≤5%; strict gate stillOPEN, no source patch justified by
process noise alone. Owner rust-performance: complete existing isolated SDK
large-work100M profiles for100K/1M scan/FETCH/LIVE LET in RAM/Sync, preserving
scratch256MiB and queue defaults. Same full oracle and checked budgets; existing
before source rejects1M dependency scratch and is retained as capacity baseline.
Per-process deadline explicit3600s for superlinear100-subscriber case, other
cases900s; no production/user timeout change. Preserve measured timing separately
from registration/evaluation rejection and process timeout. No Cargo/source edits
or host changes during measurement. Primary root SourceC28-12 frozen and allcheck
preserved645files. No invented speedup for failed-before workloads. coverage.gaps=[].


C28-15 registration evaluator TaskBrief/ProfileStack: C28-14 RAM100Kscan100
owned process timed out at900.21s during registration (no latency result).
10registrations+firstmutation counter65 proves triangular candidate evaluations
55+10 (SDK initial reads not included in that counter). Owner rust-concurrency:
normal commit-only new-definition selection, guarded by ONLY RegisterLive,
empty source intents/operations, unchanged actual primary/schema/capture inputs,
empty binding catalog, existing definitions SINCE Now. VersionStore.same_primary
alone is unsafe before publication stamps replayed revisions. Future timestamp
SINCE has latent compatibility activation counterexample; all non-Now fall back.
Keep catalog admission/all-definition bounds, new activation/backfill, fresh
prepared arrays, live_history::prepare, FCW/SSI/retry/BLOCK order. No broad
incremental DML or new revision/cache. First thread explicit All/Pending target
through existing publication helper (all callers All/check), then normal commit
eligible path Pending. Root sole writer; bounded scout+reviewer. Scope tests:
sequential counters+queue/history, mixed DML, bindings, future SINCE/function fault,
historical registration, root race; full backend/resource/lint gates. No new deps,
worker,WAL or public API. coverage.gaps=[].

Новый лимит пользователя: пять новых source-optimization attempts; C28-15 = 1/5.
После пятой прекращаются оптимизации, незакрытые gates фиксируются и продолжается
остальная closure-работа. Повторные проверки одной правки не увеличивают счётчик.

C28-16 comparator experiment TaskBrief/ProfileStack, новая попытка **2/5**:
owner rust-performance; measured SDK point16 revision lookup, actual per-node
call to general ConflictKey comparator in fresh instructions. Coding constructs:
private transparent BTree key, inline scalar BaseAtom comparison, original Ord
fallback; no Hash/Eq/ConflictKey/public/storage contract change. Map domain
routing/COW/restamp and admission stay in VersionStore. A private wrapper avoids
manually duplicating23-case enum ordering; no new dependency/cache/worker.
Rules perf-profile-first/opt-inline-small; no forced inlining/cold classification.
Scope versions.rs/tests.rs + evidence/docs. Root sole writer; scout/reviewer
read-only. Test all variants/payloads/pairs/transitivity/layout against original
Ord, version namespace and independent map/COW/rebase, full lib/backends/lint;
require generated fast-path evidence and same-profile paired point/ordinary
query measurements. Reject/revert if no material measured benefit. coverage.gaps=[].

C28-17 AST layout TaskBrief/ProfileStack, новая попытка **3/5**:
owner rust-performance; exact compiler sizes StatementKind928→960/Statement968→1000,
LIVE SELECT960; matching parser program instructions memcpy968→1000 atline116.
Existing LetValue::LiveSelect usesBox; use one parser-boundary Box for direct
StatementKind variant too, reuse it forLET (no double box). Caller scope only
syn/stmt/mod.rs,live.rs,let.rs; borrowed analyzer coercion, mode/options/spans/
EXPLAIN/timeout retained. Coding boundary owned AST/new development public syntax
payload; LiveQuery SDK/Value/RecordId/typedIR/persistent descriptors unchanged.
Public manual construction of new LIVE ASTvariant now acceptsBox. Direct LIVE
parse/clone adds one cold allocation; normal query width must shrink. No arenas,
newdeps, macros, unsafe code or new executor. Rule perf-profile-first; size guard
plus focused direct/LET/EXPLAIN/error/depth/analysis, full lib/backends/check/lint,
same-profile paired RETURN/point/DML. Root sole writer, read-only scout/reviewer
proposal PASS. Reject/revert absent material measured benefit. coverage.gaps=[].

C28-18 single-key forwarding TaskBrief/ProfileStack, новая попытка **4/5**:
owner rust-performance, coding exact inline-always-rare/perf-profile-first.
Fresh instructions show atom() passes fixed BaseAtom through a real helper
call; baseline also calls it, so added-call cause is not claimed. Only private
one-line record_read wrapper receivesinline(always), exposing key namespace
to existing generic record_reads/version routing. No mutex/entry/snapshot/SSI/
missing/last_base_read changes, no newstruct/cache/API/dependency. Scope one
transaction/mod.rs annotation; no mirror test for annotation. Existing full
read-committed/FCWSSI/snapshot/import and backend gates, lint localreason only
iftriggered; releasefatCGU1 before/after and code-size/instruction evidence
required. Proposal review PASS, root solewriter. Codegrowth/ICache risk measured;
reject/revert absent stable benefit. coverage.gaps=[].

C28-19 exclusive readset TaskBrief/ProfileStack, ПОСЛЕДНЯЯ попытка **5/5**:
owner rust-concurrency/performance boundary split: concurrency owns exclusive
borrow and shared policy; rust-performance owns existing measured lockcost and
releaseA/B decision. Fresh record_read subtree lock/unlock1.9207%/1.7040%total
(+alternate0.365/0.214), Selfseconds confirmed. Direct privateMutex readset,
GraphView borrows&Transaction; get_mut from atom&mutself excludes concurrent
sharedreaders. First extract record_revisions(snapshot,map,keys) and characterize
unchanged sharedpath; then exclusivehelper usesget_mut, atom changesonlycall.
No skiptracking/revision/default/lastcache/snapshot/root/namespace changes.
New focused first-observed/shared-exclusive/savepoint/refresh/SSI tests stay
separate from unrelated dirty transaction/tests.rs. Full/backend/check/fmt/lint,
ownedsameprofile pairedpoint/RETURN/DML required. Root solewriter; proposalreview
PASS. No unsafe/newdependencies/cache/worker/executor. After this attempted
change+verification optimizations STOP regardlessgate; only correctness repairs,
revert, remaining evidence/closure work. Reject/revert absent stable benefit.
coverage.gaps=[].
