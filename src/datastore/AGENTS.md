# Datastore module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/datastore`.

## Назначение

`datastore` владеет database-wide MVCC root, глобальным реестром атомов,
graph-local occurrences, каталогом схем и публичной транзакционной границей.
Он не владеет page/WAL layout (`storage`), graph algorithms (`graph`) или
синтаксисом (`syn`). Одна нормализованная модель работает для in-memory и
local backend; local publication сохраняет AtomStore, occurrences, memberships,
indexes и schema одним storage snapshot.

## Карта владельцев

- `atom_store` — global `RecordId → GlobalAtomId`, base properties и точный
  reverse membership index;
- `occurrence` — graph-local `AtomId`, label, topology и ленивый sparse overlay;
- EdgeEndpoint capability выделяется при первом local/qualified edge endpoint.
  Postings сохраняют полный containment path и actual source/target side; topology
  replacement/delete/BUILD GRAPH используют общий GraphRoot maintenance path.
- `identity` — `DataSpaceId::Database` либо ненулевой `GraphId` для будущего WAL;
- `schema` — `ClassSchema`, `GraphSchema` и типизированные durable identities;
- Session statement control имеет одного caller owner; explicit/autocommit
  execution helpers заимствуют его mutable до завершения export/ACK/commit.
  Per-statement named TIMEOUT не теряет origin или authority; inferred write
  access включён только для autocommit. True readonly commit (нет mutations и
  targeted intents) валидирует один current root под commit mutex; every
  possible FULL/live publisher сохраняет прежний retry/BLOCK origin.
- Main session loop использует existing elapsed origin только если ни parsed
  statement, ни execution options не содержат timeout. Timed statements и
  generic constructors сохраняют fresh origin до analysis; nested/function/
  import relative deadlines начинаются при invocation. Commit/BLOCK clocks
  не зависят от этого reuse.
- Пустой GQL ACK не создаёт BarrierAccount; непустой сохраняет pre-copy
  authority/control/byte admission и owner через commit/publication. Metadata
  snapshot account создаётся только для INFO LIVE/INFO LIVE LET/KILL/ACK;
  selector, pre-copy limits и error semantics остаются общими.
- Empty scalar-index replacement plans сохраняют shared lookup ownership;
  nonempty base changes остаются staged через fallible statistics preparation.
  Overlay/root/statistics/artifact mutations не пропускаются. No-deferred
  invalidation не делает COW; already-Stale deferred IDs продолжают возвращаться
  для bounded catch-up queue, index bindings копируются только при real removal.
- `versions` — exact FCW/SSI revisions с одним outer snapshot handle и независимыми
  inner COW domains для primary и LIVE control keys.
  atom(&mut Transaction) записывает readset через exclusive Mutex::get_mut;
  graph/shared&self readers сохраняютmutex. Одна private record_revisions policy
  хранит first-observed snapshot version для всех namespaces. Refresh/savepoint
  сохраняют прежние map/cache rules; exclusive access не ослабляет FCW/SSI. ACK/CLOSE не копируют per-atom
  revisions; import restamp меняет только unpublished primary stamps и переносит
  уже committed LIVE controls без изменения. Admission предшествует COW.
- `root` — единый immutable commit image AtomStore + graphs + logical versions;
  Primary replay и operation-image changefeed replay используют один immutable
  before-root. Journal только из existing-key Base setters меняет eager dirty
  metadata и сохраняет lazy auxiliary maps. Перед остальными exact counter
  deltas, включая ReadCommitted replay, before-root гидратируется; FCW/SSI остаются
  привязаны к original current, ACK/close не требуют data hydration.
  First ON import segment гидратирует working statistics до retained before-root;
  spool admission и savepoints остаются у changefeed_import, OFF origin не удерживает.
- Registration-only commit может вычислять только pending definitions при
  неизменных actual primary/schema/capture inputs, пустых source intents и
  bindings и прежних definitions SINCE Now. Historical/future SINCE, mixed DML
  и binding changes сохраняют full evaluation. Catalog admission, capture
  metadata, activation, FCW/SSI и publication не пропускаются. Identity clocks
  не отделяют schema при равных обоих clocks; statistics revision не делает
  COW без touched counters. Replay сравнивает hydrated before-root, FCW/SSI —
  исходный current root. Full-generation fallback после lazy setters загружает
  unchanged auxiliary maps по timestamp опубликованного source, сохраняя eager
  revisions и staleness; новый candidate timestamp ещё не имеет physical binding.
- `live_catalog` — snapshot-owned logical LIVE identities/recipes; registry
  mutations используют обычные FCW/SSI replay и transaction savepoints. Consumer
  queues не принадлежат immutable root. Исторические catalogs удерживаются одним
  outer Arc с независимыми inner COW, без удорожания обычного root clone новым Arc.
  Capture owner сохраняется отдельно как последняя успешная ChangefeedSource;
  publication/backfill читают его без повторного evaluation logical recipe.
  Reactive bindings сохраняют frozen inputs, stable source owners, PURE versions
  и exact latest values. DAG/descriptor traversal общий с storage validation в
  private `live::dependencies`; user evaluation идёт до commit mutex и проверяется
  повторно вместе с root/FCW/SSI перед existing WAL barrier.
- Stage29 Events definitions принадлежат schema; dispatch/outbox/import spool —
  transaction. Outbox и completion marker публикуются тем же root/WAL barrier.
  `live_graph_flush` проверяет полный accepted source image, qualified mappings и
  FCW requirements при любом isolation до mutation; удаление closure не удаляет
  child source data. Exact graph feedback возвращается из commit candidate,
  без нового primary-root lookup. SDK принимает его через ordered polling/ACK.
- `live_observation` владеет constant-size process counters. Attempts/admitted
  reserve/work наблюдаются вне commit lock; successful publication/ACK — после
  backend barrier. Scalar summary собирается до очистки local RAM history и
  включает fresh historical ranges по previous last_stamp/purge floor.
  Consumer first-issue count и gauges читаются под existing queue mutex.
  Replay использует independent existing observed-reader scope, включая errors;
  cache/WAL page requests не выдаются за physical disk I/O.
- `live_history` применяет final source capture policy к целому commit, сохраняет
  private computed envelopes и capture epochs. Registry/version и source policy
  сверяются при recovery; volatile payload не выдаётся за durable coverage.
  Изменение epochs/last recorded stamp отмечает existing LiveCatalogRange;
  purge использует тот же predicate. Owned INFO читает его вместе с definition
  и direct bindings. Source result changes не делают definition отдельным FCW
  write ради metadata. SDK/GQL проецируют один root; GQL не refresh-ит statement.
  Dynamic class/record graph scopes публикуют успешную capture owner/name pair
  с результатом. String operand удерживает GraphId до явной смены значения;
  fault не перепривязывает owner/name и не отклоняет source DML. Pending activation,
  future SINCE и reopen используют тот же durable anchor; nested graph inputs
  остаются ordinary read dependencies.
- `source_history` — один bounded fixed-root NET/FULL walker для transaction reads
  и historical LIVE preparation. Public changes() сохраняет transaction borrow и
  age/error fusion; commit evaluator не открывает вложенную transaction.
- Обычный commit и import END используют один LIVE preparation hook вне commit
  mutex. END сохраняет accepted-item spool для bounded retry, переносит latest
  control metadata и назначает свежий final stamp; provisional data revisions
  restamp-ятся, уже committed CLOSE и пользовательские Path/values — нет.
  Writer lease допускает только самостоятельные CLOSE/ACK control commits;
  DML, FULL no-op, schema/capture mutations и maintenance остаются запрещены.
  Timestamp фиксируется до source/LIVE encoding и не меняется в backend barrier,
  даже если он равен времени предыдущего commit.
- `live_queue` — StoreInner-owned transient consumers, отдельный issued delivery
  и pending tail; snapshots и typed IR в очереди не удерживаются. Publication
  предварительно допускает fanout outside commit mutex и сверяет root/consumer
  revisions под ним. ACK сохраняется обычной catalog mutation до освобождения
  capacity. Pending SDK attachment активируется вместе с registration; Weak
  handle не продлевает logical datastore lifetime. GAP/DISCONNECT/BLOCK применяются
  к per-query и общему retained ceiling; control slots оплачены при attachment.
  CommitOptions переиспользует QueryCancellation; GQL передаёт исходный deadline
	в normal commit и import END. Ни ожидание capacity, ни user evaluation после
  WAL barrier не допускаются. Existing-incarnation resume фиксирует cutoff/tail
  под одним commit guard и использует persisted ACK; lazy saved-result decode
  не удерживает primary root/transaction. Historical head оплачивается отдельно
  от live tail, но входит в общий session ceiling. EOF/error/Drop освобождают pins
  и holdback; decode error fuses handle без продвижения ACK. Computed TTL/floors
  публикуются тем же PurgeChangefeed worker с captured epoch policy; exhausted
  partial segment удаляется до publication. Fresh-registration SINCE — L8c gate.
- Geometry definitions проверяют TYPE при BASE/EFFECTIVE DML; RAM root удерживает
  immutable STR generations, а local transaction закрепляет physical generation
  одновременно с logical snapshot при begin и Read Committed refresh.
- `index_store` — Base и Effective index images, class/label/count lookup и
  проверка Unique;
  occurrence replacement готовит engine-owned label transition вместе с
  explicit keys, даже при отсутствии explicit indexes. Insert/replacement/
  detach/reopen используют общий label posting updater; пустые buckets
  удаляются, чтобы dense durable label IDs совпадали с source labels.
  Write-set admission labels включает journal, occurrence и отдельно owned
  lookup/statistics keys. Final audit сохраняет оба before/after targets;
  общие keys допускают консервативный повторный charge без обхода maps.
- `conflict` — независимые `BaseAtom`, `GraphMembership`, `GraphOverlay` и
  `GraphIncidence` conflict keys;
- `transaction` — private root, FCW/SSI validation, bounded fanout preflight и
  replay непересекающихся writes поверх актуального root;
  Query scalar record-field lookup сохраняет ensure_live и point SSI reads,
  выбирает borrowed atom/occurrence и использует shared field_from_atom.
  Selected property и id/label допускаются callback policy до copy; whole
  properties row для scalar dereference не строится. Public DTO не расширяются.
- Session/transaction lexical types разделяют immutable `gql::bindings`
  metadata, не schema catalog и не public Value wrappers. TRY восстанавливает
  types вместе с values; explicit transaction locals исчезают при COMMIT/CANCEL.
  Autocommit готовит metadata export до WAL и заменяет session map после success.
  Reactive flavor хранится в этой же metadata; UUID frame удерживает LIVE LET
  identity. Borrowed value lookup фиксирует statement snapshot и SSI key до
  admission/clone, сохраняет paused diagnostic/cause. Registry mutation и
  lexical declaration откатываются совместно; session detach не удаляет identity.
- `store` — datastore owner, ID clocks и один commit mutex;
- `options`/`consts` — transaction, import и maintenance resource limits;
- `maintenance` — один durable FIFO coordinator, job reconciliation, cancellation и
  bounded недолговечная очередь deferred-index invalidations.

Final maintenance counters сначала обновляются в coordinator state и
персистируются вместе с обязательным переходом `Publishing`; отдельный
metadata-COW только ради progress запрещён, поскольку crash до publication
всё равно требует rebuild candidate.

## Инварианты

- `RecordId = class:id` задаёт ровно один immutable class; отдельного table
  membership и system graph не существует.
- Один base atom может не входить ни в один граф либо иметь по одному occurrence
  в нескольких графах. Reverse memberships обновляются вместе с occurrences.
- Base properties хранятся один раз. Effective value равен graph-local
  `Value`, отсутствует при `Hidden` и иначе наследует актуальное base value.
- Отсутствующий overlay не аллоцируется. `RESET` удаляет последнюю пустую overlay
  map и возобновляет наследование.
- Write/write validation использует точный logical target. Membership и
  incidence не объединяются с base/overlay в одну грубую graph version.
- Base mutation до изменения private root резервирует fanout только по reverse
  memberships данного атома. Commit повторяет расчёт на актуальном root; выход
  за `max_write_set_bytes` ничего не публикует.
- Snapshot/SI читают один immutable root. Read Committed обновляет root между
  вызовами API и replay-ит собственные непересекающиеся writes. Serializable
  write transaction консервативно abort-ится при изменении прочитанного key.
- Commit под одним mutex повторно проверяет FCW, structural requirements, SSI,
  fanout и captured root, затем публикует подготовленный root одной заменой `Arc`.
  Reactive user evaluation выполняется вне этого mutex.
- Новые global/local slots, выбранные private snapshot, являются временными
  физическими адресами. Commit replay повторно выделяет их из актуального root
  и переводит topology/conflict keys; логическая коллизия `RecordId` или
  membership по-прежнему завершается serialization failure.
- Публичные read results владеют `RecordId`, `ClassId`, `AtomKind` и
  `PropertyMap`; crate-private `GlobalAtomId` наружу не выходит.
- Predicate/range phantoms, scope-aware indexes, durable publication и graph
  DML используют тот же normalized root. Не возвращать ради новых операций
  удалённый TABLE/system-graph контур.
- `CREATE`/`UPDATE`/`UPSERT`/`DELETE` без graph scope работают с global atom;
  `ATTACH`/`DETACH`/`RELATE ... IN GRAPH`, labels, topology и overlays меняют
  только graph-local occurrence, если операция явно не задаёт Base scope.
- `query_to_memory` владеет copied effective graph. Mutable materialization
  сохраняет origin mapping и flush-ит только явным вызовом; `Drop` не пишет.
- Named `BUILD GRAPH` использует одну `InstallBuiltGraphTree` mutation: она
  отображает только существующие global atoms, сохраняет effective overlays и
  устанавливает root с owned internal children атомарно. Internal graphs имеют
  одного public owner root, скрыты из обычного catalog scan и удаляются вместе
  с заменяемым/удаляемым root.
- Family, rooted-tree root и orthogonal topology validators проверяются одним
  shared validator после transaction replay под commit lock. Прямой Rust path
  не может обойти тот же durable contract, который проверяет GrynnQL.
- Local `PhysicalTraversalCursor` фиксирует exact storage snapshot, graph
  generation и bindings. Он разрешает staged/WAL/home версии до transport,
  возвращает GQL только owned logical values и после первой private mutation
  данного graph до конца statement использует materialized private root.
- Physical и materialized traversal routes регистрируют одинаковые SSI
  predicates; page guards, `DataFileId` и storage DTO не проходят transaction
  query adapter.
- `Transaction::endpoint_tensor` фиксирует один QuerySnapshot для всего LOCAL/ROOTED
  чтения; GraphDefinition, membership/overlay/incidence predicates сохраняются.
  Node/edge metadata readers не materialize-ят effective properties до properties().
- Tensor cache удерживает полную ATOM incidence и durable revision vector всех
  qualified dependencies. Topology/membership/labels/containment/weights исключают
  generation из private reads сразу; old roots сохраняют прежний immutable artifact.
  `endpoint_tensor_using_index` проверяет forced binding до чтения; обычный SDK
  lookup использует exact on-demand fallback в общем memory/work budget.
- `Datastore::rebuild_graph_index` и `rebuild_graph_index_concurrently` используют
  существующий maintenance coordinator. Tensor planes строятся существующим
  external sorter; qualified dictionaries/topology остаются bounded scratch.
- `LocalOptions` является единственной construction authority локального
  backend. `local_storage_status()` только наблюдает effective budgets и
  pressure; environment variables не переопределяют параметры экземпляра.
  `open_local` передаёт maintenance limits в recovery тем же scoped external-sort
  context; нехватка бюджета secondary исключает generation, сохраняя primary data.
- Deferred Standard/Count index становится `Stale` атомарно с primary mutation.
  Stale binding не является query candidate; forced hint ошибочен, обычный planner
  использует exact scan. Потеря недолговечной pending queue после crash означает full rebuild,
  а не повторное использование stale rows.
- Maintenance publication сверяет target/version/source revision под commit lock.
  Durable job ledger не является доказательством готовности binding; `Ready` публикуется только
  вместе с exact physical binding. Один datastore исполняет не более одного тяжёлого job.
- Durable PARAM и FUNCTION используют общий schema clock. Function overload identity включает
  declared parameter types; compiled cache ключуется `FunctionId`, version и schema epoch.
- Graph-owned `GraphIndex` использует тот же IndexId/version/state и обычный FCW/SSI replay.
  NODE/ATOM, direction, labels, artifact budget и function versions входят в canonical fingerprint.
  Materialized definition остаётся Stale до publication полного artifact; ON_DEMAND side-file
  не создаёт. Function mutation атомарно обновляет captured versions и делает definition Stale.
- Reachability и Neighbourhood artifacts удерживаются immutable root через Arc. Membership, topology и
  edge-label mutation атомарно делают свои materialized definitions Stale и исключают artifact;
  старый snapshot сохраняет прежние labels. Обычная base/overlay property mutation без
  изменения relation не инвалидирует эти artifacts.
- `graph_path` хранит immutable directed LANDMARK_SSSP distances и full definition descriptor.
  HOP/WEIGHT/PURE COST builds используют общий incidence kernel и maintenance publication.
  Weight mutation исключает WEIGHT generation; base/overlay/occurrence property mutation
  консервативно исключает PURE COST generations. Function mutation захватывает новые
  transitive versions; rebuild перепроверяет dependencies под существующим commit lock.
  Query adapter возвращает logical metadata/Number bounds, никогда таблицы или страницы.

## Проверка

Changefeed capture использует final policy, compact operation journal и единый
ReplayRemap; NET/FULL images не восстанавливаются из текущих primary records.
ON import удерживает один admitted before-root и accepted-item journal на private
segment. Seal переносит replay images в bounded spool до очистки mutations.
Transient replay/journal charges не накапливаются в retained spool; fatal seal
error отравляет import session и запрещает END. OFF сохраняет компактные intents.
`Transaction::changes` читает committed root, исключает private writes, проверяет
historical FULL coverage до row pull. History pin не сканирует graph bindings.
Changefeed catalog/history/segment handles сгруппированы одним immutable root
binding, но сохраняют независимый inner COW. Read accessors не отделяют root;
mutable accessors сначала отделяют binding, затем только выбранную map.
Maintenance сравнивает catalog, а dedicated WAL/file pins живут отдельно.
Retention публикует prefix floor и partial-segment position catalog-only под
тем же commit lock/WAL barrier. Один worker просыпается для idle TTL; private
import lease блокирует purge publication. Active source сохраняет пустой floor.

Проверять focused schema/value/root/transaction/query tests, RAM/local parity,
reopen/recovery в обеих I/O feature-конфигурациях и effective-index/predicate
regressions. Ошибка commit никогда не должна менять опубликованный root.

SubscriptionHandle разделяет fused transport/polling от confirmed durable capture
stop. DISCONNECT/read error могут detach/fuse consumer; explicit close всё ещё
публикует close_live. Только successful barrier устанавливает close idempotence.
LiveInfo direct-binding revisions принадлежат текущему snapshot; used revisions
принадлежат сохранённому LiveBatch, эти два смысла не смешиваются.
