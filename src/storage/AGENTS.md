# Storage module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/storage`.

## Назначение и границы

`storage` — crate-private владелец локального on-disk состояния. Нормативный
контракт задаёт `docs/disk_storage_spec.md`; текущая реализация ориентирована
на синхронный Windows GA-core. Публичные операции БД принадлежат `datastore`,
а runtime-типы — `values` и `graph`.

- Не выдавать наружу страницы, LSN, физические atom ID или storage DTO.
- Не использовать `src/collections/radixdb` как второй storage engine.
- Не переносить layout из `fg-meta` без сверки с production-спецификацией.
- Native async I/O ограничен frame-oriented IoRing для заранее известного
  sparse-окна; синхронный Direct I/O остаётся fallback для singleton miss.
- Test-only storage-engine прототипы принадлежат
  `crates/fg-storage-engine-prototypes`, а не `src/storage`. Скрытый non-default
  `prototype_api` открывает этому workspace-крейту только измерительные
  production-типы и функции под feature `storage-engine-prototype-api`; feature
  не входит в GA public API, не выбирает production-поведение и не должен
  становиться runtime selector. Измерительные операции не добавлять как методы
  публичного `Datastore`.

## Карта владельцев

- `direct_io` — платформенные aligned Direct-I/O файлы;
- `page_io` — синхронный Direct transport и Windows frame-IoRing;
- `page` и `page_cache` — page layout, exact-version cache key и проверки;
- `hybrid_engine` — R8 Atomic Arena, source locators, read scopes, router,
  transient budget, scatter/P1 и sparse IoRing;
- `buffer_pool` — прежний Clock-Pro, доступный только тестам и prototype-control;
- `image_io` — staged/WAL/home resolution через production HybridReadEngine;
- `wal` — codec, published state, writer/segments, recovery и checkpoint;
- `catalog` — `FGFC v6` physical registry и bounded `FGDC v14` page-chain;
  Stage29 source-only EVENT descriptors и `.feo` invocation segments сохраняются
  в explicit codecs; pending corruption требует recovery error. `FGLG v1` owned
  graph images и `FGLB v2` graph deltas не расширяют общий StorageValue codec.
  Graph checkpoint reference включён в обычный LIVE ownership/reachability;
  cold recovery сверяет family/validators с recipe и source watermark bounds.
- `history_codec` — общий bounded historical Value codec для changefeed images
  и canonical LIVE descriptors; dictionaries не обращаются к current primary.
- `live_descriptor` — versioned logical recipes, FGLD/FGLC validation; executable
  IR и consumer queues не сериализуются. FGDC v14 хранит definitions/options/ACK.
  LIVE-query header — 160 bytes; checked capture owner имеет kind/reserved/id и
  optional successful String scope-name length/payload (UTF-8, cap 128 bytes).
  Options byte bit0 обозначает nonempty root RHS anchor tail после primary
  scope-name, перед diagnostic: count/u32 owner/reserved4/GraphId/u32 name length/
  UTF-8 1..128. Owners sorted distinct positive и относятся к computed JOIN;
  conversion/capture/decode учитывают retained vector/names до copies. Fault
  удерживает last-success pairs; generation/reopen не исполняют recipes для них.
  Static source equality и отсутствие scope-name обязательны. Recovery читает
  durable owner/name pair без user evaluation; FGLD source tags 5/6/7 держат
  GraphClassExpr/GraphRecordExpr/GraphExpr в общем bounded recipe codec.
  Query body содержит GROUP tag после filter: 0=absent, 1=ALL, 2=BY с positive
  checked u32 count и bounded expression children. Unknown tags, zero BY count,
  lengths/depth/reserved bytes проверяются до allocation; Q-40 layout inplace.
  После projection идут ORDER count/keys (Expression/Name, checked flags/reserved),
  optional LIMIT/START Offset expressions, затем ordered OMIT и FETCH path lists (checked u32 counts,
  1..64 literal nonempty/NUL-free UTF-8 segments). Empty query FGLD — 67 bytes,
  минимальная LIVE catalog entry — 227 bytes; canonical decode не читает primary.
  Final query flags: output в bits 0–1 (3 invalid), ONLY в bit 2, SPLIT tail в bit 3,
  bit 4 — checked source alias tail после SPLIT, bit 5 — ordered nonempty JOIN
  tail, bits 6–7 zero. JOIN codec держит positive count, kind/reserved bytes,
  checked alias/source/optional ON; INNER/LEFT требуют ON, CROSS запрещает ON.
  Alias — u64 byte
  length и nonempty/NUL-free UTF-8; shared runtime validation, owned/work admission
  и canonical decode обязательны. Nonempty SPLIT tail содержит positive checked count u32 и ordered
  expressions; count/remaining bytes/depth/work/scratch проверяются до allocation.
  Empty SPLIT сохраняет default encoded bytes/minima. Typed IR не сериализуется.
- `live_history` — FGLB whole-commit computed results, exact qualified values/Path,
  binding revisions и typed fault diagnostics; primary records не нужны для replay.
  `history_codec/pages` владеет общими FCFS/FCFE pages с typed CHANGEFEED/LIVE owner;
  catalog references двух namespaces не смешиваются. Source reader и retention
  проверяют owner до trusted bounds и optional corruption/GAP conversion.
  Consumer restoration ограничивает whole control 64 KiB до continuation
  allocation и во время owned diagnostic decode, сохраняя общий page budget.
  Generic wire codec не является разрешением на oversized queue control.
  Composite identity tag3 использует flat checked parts count>=2: Record0,
  Occurrence1, Unmatched2, SPLIT u64 LE3. Primary source обязателен, source slots
  перед positions; shared runtime shape authority проверяется до writer bytes.
  Decoder допускает count/work/owned capacity до allocation и не читает primary.
- `job_catalog` — checksummed metadata-COW `FGJB v1` с `main/.prev` recovery;
- `quarantine` — immutable streaming `FGQR v1`; файл не является positional page source;
- `changefeed` — explicit `FCFS`/`FCFE` pages, `FGCB` whole-commit NET/FULL payload,
  independent historical dictionaries и inverse before-diffs. `.fcf` имеет kind
  `Changefeed=0x17`, database-only `WalBackfilled`; publication остаётся в local_store.
  Purge floor и file unreference публикуются до unlink. Partial first segment
  сохраняет physical bounds; retained_page/groups задают доступный prefix.
  Weak registry maps всех смен generations защищают `.fcf` от удаления до
  освобождения pinned readers; cleanup attempts ограничены и повторяются idle worker.
  Codec admission учитывает owned runtime conversion, geometry validation scratch
  и complete B-tree nodes. Filled AlignedPage строятся в окончательных slots.
- `atom_store_superblock` — exact database-global AtomStore ceilings/counts и checkpoint tuple;
- `property_dictionary` — append-only database-global `PropertyKeyId ↔ UTF-8` pages и hash lookup;
- `metadata_cow` — общий checkpoint-протокол `.tmp`/main/`.prev`, не используемый обычным commit;
- `graph_pages` — graph-local `NodeHotPage`/`EdgeHotPage` occurrences,
  topology, labels, sparse overlays and scoped statistics;
- `atom_page`/`atom_store_image` — database-global `AtomHotPage`, base
  properties, global primary и reverse memberships;
- `graph_index` — order-preserving primary и общий внутренний B+tree codec для
  раздельных Standard/Unique контрактов, bounded Direct-I/O
  external sort, bottom-up packing, immutable Label arrays и single-page Count;
- `geometry_index` — общий STR page builder/walker и exact replacement delta;
  resident RAM pages и local WAL/COW files используют один checked layout.
- `reachability_index`/`neighbourhood_index`/`path_index` — immutable graph-owned header/array sections, full definition
  descriptor, canonical contiguous roots и aggregate decode admission; status остаётся в catalog.
- `derived_index` — borrowed array/record sections, checked ordinal chains,
  reference bounds и reserved bytes для snapshot graph artifacts.
- `edge_endpoint_index` — engine-owned qualified postings, shared Unique B+tree
  с concrete leaf/header и snapshot identity. Header page 0 и topology публикуются
  одной WAL/COW generation; lookup возвращает только connector/side, не query values.
  Retained tree получает header WAL-version на каждый global commit timestamp,
  включая catalog-only publication; old pins сохраняют прежнюю версию.
- `store` — open/load, transaction views, commit и startup GC;
- `local` — адаптер локального datastore.

## Инварианты

- Scalar Base postings декодируются как generation-aware `GlobalAtomId`,
  Effective — как graph-local `DiskAtomRef`. Нормализованные kind/slot
  разрешаются по root pinned snapshot, а не по текущему root datastore.
- Checkpoint backfill использует WAL-owned FrameReadScope с одним last-segment
  handle. Shared prune guard удерживается через физические чтения/home sync;
  scope завершается до publish_checkpoint exclusive gate. File закрывается до
  guard, segment switch сначала закрывает прежний handle. Singleton и scoped
  reads используют один seek/exact-read/page/LSN validator; target/order,
  checkpoint thresholds и durable publication не меняются.
- WAL durable publication предшествует видимости page frames; incomplete tail
  repair не должен маскировать полную corruption.
- Catalog-only publication сохраняет immutable physical statistics binding:
  statistics files/layouts не меняются, reads используют текущую snapshot boundary.
  Это обязательно для deferred root после reopen→LIVE ACK/CLOSE→mutation.
  Full generation publication сбрасывает прежний statistics binding.
- У LIVE handoff successful WAL path использует заранее reserved RapidHashMap
  slots, version Vecs и publication buffer; prune gate берётся перед writer.
  Backend empty-history handles/COW и retirement metadata допускаются до barrier.
  LIVE generation cleanup выполняется перед admission либо существующим idle
  maintenance; pinned paths/markers и сохранённая Vec capacity входят в budget.
- Checkpoint публикуется атомарно и не удаляет данные, удерживаемые активным
  snapshot.
- Sparse Base update сохраняет file identities и immutable property prefix;
  append address учитывает WAL-only tail. Physical plan строится отдельно от
  full generation и публикуется тем же source/history WAL barrier.
  Base lifecycle без memberships использует final replay slots, включая
  Create→Delete и Node/Edge generation reuse. Hot pages, allocator ceilings,
  Primary, scalar/Count и child class counters входят в один commit. Reserved
  freelist companions не являются allocator authority (§15 disk specification)
  и сохраняют bytes/IDs: reopen восстанавливает free slots из Hot tombstones.
  Lifecycle candidate содержит полные statistics до WAL; после публикации
  прежний lazy owner снимается, captured old snapshots сохраняют собственный.
- Stable Node/Edge overlays имеют отдельные final replay targets, не Base slots.
  Mixed Base+overlay готовит один Effective before/after переход на graph/global
  pair. При неизменных identity и structure Hot slots меняют выбранные поля;
  overlay append начинается после WAL-visible
  tail. Slots одной страницы декодируются до первой mutation и затем seal-ятся
  вместе. Graph superblocks используют актуальную database checkpoint authority
  и устанавливаются в runtime только после общего WAL commit. Effective presence
  обновляет точные label counters; Edge weight сохраняет topology/transition.
  Один LabelPlan разделяет source/final dense namespaces: Hot targets включают
  explicit mutations и postings имён со сдвинутыми ID. Selected arrays объединяют
  весь проверенный canonical source с bounded explicit insertions без полной
  повторной сортировки. Namespace change заменяет Labels/graph/label statistics,
  сохраняя property statistics и остальные identities; bindings устанавливаются
  без fallible work после WAL. Plain isolated NODE ATTACH/DETACH существующих
  Base atoms в standalone root Graph имеет отдельный sparse publisher: allocator/
  Hot, forward/reverse membership, class arrays, Effective indexes и exact stats
  публикуются вместе. Последний physical empty class source сохраняет authority
  после cold reopen. Plain EDGE ATTACH/DETACH существующего Base в том же
  standalone Graph с local NODE endpoints публикует explicit EdgeHot и touched
  NodeHot/overflow, сохраняя n-ary role order и multiplicity. Generation-aware
  adjacency merge проверяет source chains до WAL; старые chains сохраняются до
  rebuild для pinned snapshots. Plain existing-Base NODE+EDGE memberships имеют
  общий per-graph incidence publisher: explicit lifecycle и derived endpoints
  объединяются в один NodeHot pass; один header содержит обе capacities/counts.
  Before/final generations разделены, CASCADE/reuse не переносит старую adjacency
  новому NODE. Логический CASCADE planner всё ещё ищет NODE incidences обходом;
  bounded physical publication не доказывает устранение этого CPU scan.
  Global NODE/EDGE create/delete/reuse с plain memberships использует те же Base и
  incidence builders: final replay содержит каждый before/final membership и
  local Hot target, один WAL включает обе allocator authorities. Stable Base
  edits вместе с membership DML обновляют inherited Effective scopes и label
  counters других graphs; raw Primary/Base postings проверяют полные global
  generations независимо от нормализации query adapter. Fully transient global
  NODE/EDGE допускается при consumed global/local generations и отсутствующих обоих
  membership sides; новые empty class buckets сохраняют physical authority для
  следующего warm ATTACH. Metadata проверяется один раз на GraphId, consumed
  local witnesses — отдельно по NODE/EDGE, не более двух scans explicit incidence.
  EDGE topology ограничена root-local NODE/EDGE endpoints; transient NODE endpoint может
  исчезнуть в том же commit. Pure transient EDGE не пишет NODE Hot/adjacency.
  Cold empty source проверяется до сохранения. LABEL-only NODE/EDGE lifecycle
  в том же standalone Graph использует source/final namespaces без клонирования
  dictionaries: старые Hot metadata проверяются, final label ID ставится вместе
  с allocator/adjacency. Explicit membership owner формируется incidence renderer
  один раз; stable namespace suffix меняет только остальных shifted owners.
  Final dictionary ceiling входит в incidence header даже без stable suffix,
  включая переход3→1. При local/global reuse старый label не переносится новому
  owner. Connected NODE CASCADE удаляет connector membership, сохраняя его Base;
  старые physical Hot/Base и label arrays остаются доступны после Passive.
  Lifecycle NODE/EDGE overlay в том же standalone Graph использует один
  graph-local append allocator вместе со stable metadata. Старый payload
  проверяется canonical bytes принятого root и physical format validators;
  чтение ограничено старым graph header ceiling даже при доступном WAL tail.
  Full local/global generation определяет owner: новый owner не наследует ref,
  same-owner reuse требует проверенного source и равенства overlay. Tombstones
  и fully transient owners не получают payload. Hot writes backing удерживается
  в shared envelope до nested adjacency/overlay codecs, включая capacity excess.
  Plain EDGE rewire при прежних full local/global identities и local NODE
  endpoints использует общий source/final target classifier и incidence renderer.
  Старые роли проверяются до WAL; removals/additions сохраняют multiplicity.
  Structural EDGE получает один final Hot slot, включая label/weight/overlay;
  metadata другого NODE объединяется с уже подготовленными adjacency frames.
  Weight-only сохраняет прежний metadata path. Pure rewire не меняет capacities/
  counts и parent SubgraphDir; containment/qualified endpoints остаются fallback.
  При уже существующей endpoint capability root-local EDGE ATTACH/DETACH/rewire
  сохраняет AtomStore/Hot/arena и unrelated graph files. Меняется только endpoint
  tree затронутого graph при смене canonical connector-side postings; exact shared
  builder создаёт unreachable side-file, catalog/runtime bindings входят в общий
  WAL commit. Прежний header проверяется до output; retained trees получают только
  новую header revision. NODE adjacency содержит только NODE endpoints. First
  capability использует тот же selected publisher при строгом additive schema
  classifier; accepted catalog capability/runtime bindings должны отсутствовать.
  Новая capability сохраняет header/empty leaf даже при пустых final postings;
  retirement применяется только к прежнему physical file. Любые остальные schema
  changes и прежние artifact Arc guards сохраняют fallback. Cleanup распознаёт только
  graph-owned `ee_endpoint.fgidx`; pins защищают прежний tree. Selected sort имеет
  отдельный reserve merge readers/keys/metadata и serial worker, общий all-graph
  cleanup vector учитывается до allocation. Это logical accounting, не RSS quota.
  Plain standalone Graph/Dag/Forest/Tree/RootedTree/Hypergraph/Metagraph используют общий family
  predicate и incidence renderer; final replay family/root/orthogonal validators
  остаются до persistence. В Metagraph меняются только plain local NODE/EDGE;
  существующие containers, children и arena сохраняются. Membership DML дочернего
  plain graph обновляет также
  SubgraphDir каждого непосредственного parent: все shared slots получают четыре
  final child counters в общем GraphUpdatePlan и одной WAL publication. Deep-only
  ancestors не имеют соответствующей direct entry. Source chain, page ceiling,
  unique slots, container generations и старые child counters проверяются до WAL;
  сохраняются entry order, links, page count и file identities. Parent header при
  count-only patch не меняется. Общий бюджет учитывает reusable decoder/page
  backing, validation trees, retained pages и WAL copies. Format-v1 entries все
  живые: codec отклоняет live_count != entry_count до allocation. Обе production
  eligibility проверки используют один pure PointUpdatePlan contract без отдельного
  child guard. GEN_MAX retirement, specialized
  buckets и containment structural DML сохраняют
  full fallback.
  Subgraph — runtime result family; catalog validator не допускает её как durable graph.
  Namespace admission включает prepared buffers, codec scratch, selected metadata
  и private WAL staging copies. Это logical envelope; общий WAL publication
  workspace, registry и retained catalogs не объявляются allocator/RSS bound.
- Checkpoint COW superblock берётся из exact ticket frame с его PageFrame LSN.
  Runtime ceilings остаются latest; checkpoint tuple устанавливается только
  после durable database authority. Отсутствующий frame не разрешает записать
  after-target binding в home metadata.
- Before prune backfill включает trusted current и pinned retired regular files.
  Same physical ID сохраняет registration Arc между catalog refreshes;
  path/role/native mode mismatch фатален. Retirement хранит removed file keys
  независимо от directory reachability; unlink проверяет pins всей очереди.
- Pinned physical snapshot допускает home page LSN до своего `visible_lsn`:
  passive checkpoint может backfill/prune после capture. Direct/IoRing bypass
  проверяет тот же page identity/checksum/ceiling, что resident admission.
- После COW publication прежняя UUID-generation удаляется в runtime только
  после освобождения exact physical snapshots, удерживающих старый registry.
  Cleanup failure не отменяет уже опубликованный commit: generation остаётся
  pending, счётчик ошибки наблюдаем, checkpoint/следующий commit повторяет
  удаление.
  Sparse class COW может сохранять прежние Hot roots и отдельный class root;
  число live directories не константа, authority задаёт catalog reachability.
- Direct I/O сохраняет alignment, полный page transfer и платформенную
  семантику ошибок.
- On-disk числа, magic, tags и checksums меняются только вместе со
  спецификацией, golden vectors и recovery tests.
- `IndexId` — стабильная логическая identity определения из общего schema
  clock; `SchemaVersion` — версия определения; `DataFileId` — identity
  физического COW-файла; ordinal используется только в безопасном локальном
  filename. Binding индекса разрешается по `IndexId`, не по имени.
- `GraphSuperblock` хранится только в `superblock.fgb`. В текущем codec ровно
  шесть изменяемых graph data-файлов имеют отдельные `freelist_*.fgb`, а binding
  хранится в `FGFC v6.companion_of` вместе с `DataSpaceId`. Standard и Unique называются
  соответственно `std_<ordinal:04>.fgidx` и `uniq_<ordinal:04>.fgidx`; `scalar`
  допустим только как внутреннее имя общей реализации.
- `AtomHotPage` хранит только database-global class, RecordId и
  base properties. `NodeHotPage`/`EdgeHotPage` хранят membership occurrence,
  `GlobalAtomId`, label/overlay и topology. Точные offsets задают
  `docs/disk_storage_spec.md` §§10–11.
- Собираемый target использует только `FGDC v13`/`FGFC v6`, `DataSpaceId::Database` для AtomStore
  и не создаёт системный graph; `GraphId` каждого graph ненулевой. AtomStore и
  graph occurrences публикуются одним WAL transaction и открываются по одной
  snapshot boundary. Missing/unopenable/checksum-corrupt secondary переводится в `Stale` и даёт scan
  fallback, а primary/base corruption остаётся фатальной.
- `FGDC v13` хранит topology constraint bits, optional rooted-tree `RecordId`,
  public owner root и dense arena slots. Несколько slots могут указывать на один
  child `GraphId`, но `(scope, child)` уникален, containment cycle запрещён, а
  name map содержит только public graphs. Он также хранит durable parameter/function
  sections и `IndexMaintenanceMode`; `FGDC v9` и `FGFC v5` не принимаются и не мигрируются.
- `JobCatalog` регистрируется ровно как `admin/jobs.fgb`; quarantine — только как
  `admin/quarantine/<uuid>.fgq`. Оба auxiliary kind не открываются через `PageManager`.
- Bulk builders на Windows MSVC должны сначала выделять окончательные
  `Vec`-slots/arena и только затем заполнять 16 KiB `AlignedPage` через `&mut`.
  В ASan-инструментированном большом builder-path наблюдалась codegen-sensitive
  порча первого byte при возврате уже заполненной страницы по значению, хотя
  проверенные адреса оставались выровненными. `Box` стабилизирует адрес
  конкретного call site, но сам по себе не исключает временное stack-значение;
  границы доказанного описаны в
  `docs/reports/aligned_page_corruption_investigation.md`.
- Immutable external-sort B+tree generations заполняют leaves до capacity.
  Selected scalar images используют тот же streaming builder и namespace ordinal
  authority `std_<ordinal:04>` / `uniq_<ordinal:04>`; неизменные bindings сохраняются.
  При замене отдельного scalar file в retained UUID directory retirement учитывает
  exact file pins всех batches. Recovery удаляет только canonical orphan scalar
  files после проверки recovered catalog; неизвестные имена и symlinks сохраняются.
  Global Primary fallback использует тот же allocation clock и отдельную
  explicit binding. Orphan `primary.fgidx` удаляется только в canonical global
  AtomStore UUID directory; registry handles удерживают retired Primary по
  точному path. LocalQuerySnapshot сам по себе не удерживает Primary handle.
  Base property replacements используют production scalar leaf mutator при
  стабильных separators/page count/posting layout. Borrowed Standard metadata
  проверяется до чтения external postings; внешние postings затронутого leaf
  требуют selected streaming fallback. Fallback выбирается отдельно для binding,
  все leaf pages и новые files публикуются одним WAL commit. Changed bindings
  получают новые decoded owners до WAL даже при прежних file/root; старые owners
  и их caches не очищаются. Leaf-only publication не меняет FileCatalog.
  Fallback перестраивает только затронутый индекс. Primary mutator проверяет
  ordering и exact removal address, включая generation, во всех затронутых
  leaves до разрешения fallback. Q-05 остаётся открытым для остальных DML и
  полной backend/performance matrix. Параметры существующего mutable builder
  `0.90` для primary/Unique и `0.80` для Standard не являются новым disk tag;
  прежние microbenchmarks не
  подтверждают performance текущего production commit.
- Любая входная длина/счётчик проверяется до allocation; ошибки проходят через
  `gql_status` без раскрытия локальных путей и сырых страниц.
- Graph-owned definitions хранятся в FGDC v13 отдельно от availability/build revision,
  checked canonical descriptor и полные bytes обязательны наряду с fingerprint. GraphIndex
  durable job имеет target tag 5; graph-wide target и class index target не смешиваются.
- Ready Reachability/Neighbourhood/LANDMARK_SSSP binding удерживает один immutable файл своей проекции. Независимый
  commit сохраняет DataFileId; reopen проверяет maps/CSR/labels против того же snapshot.
  Ошибка secondary decode исключает artifact и делает logical definition Stale.
- Tensor binding использует только FileKind/PageType TensorCache. AUTO descriptor
  сохраняется полностью; reopen проверяет записанный concrete CSR/DENSE format,
  canonical sections, metadata и durable graph incidence revisions. Checkpoint LSN
  не определяет актуальность. Publication/retirement используют общие WAL/COW paths.
  Recovery использует maintenance budget открывающего datastore. Tensor restore
  учитывает encoding reservation ровно один раз и отдельно проверяет все stored cells.

## Проверка

Changefeed `.fcf` — WalBackfilled pages; primary/feed/catalog публикуются одним
TxHandle. Catalog bounds авторитетны независимо от доступности segment bytes.
Dedicated history snapshots удерживают WAL visibility и shared file handles;
optional segment corruption даёт GAP, resource/catalog failures остаются errors.

Минимум для изменения: узкий unit/integration test владельца, `cargo check
--lib --no-default-features`, `cargo +nightly fmt --check`. Для WAL/page/layout
добавлять reopen/recovery или golden-vector проверку; для platform I/O —
соответствующую Windows/Unix конфигурацию.

LIVE FGLD borrowed descriptor view переиспользует owned canonical bytes и validation.
Optional caller work/cancellation проходит через existing value preflight до copies;
borrowed RecordId source допускается до owning envelope clone. Canonical framing не
зависит от EXPLAIN, typed IR не сериализуется и runtime types остаются владельцами API.
