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
- `root` — единый immutable commit image AtomStore + graphs + logical versions;
- Geometry definitions проверяют TYPE при BASE/EFFECTIVE DML; RAM root удерживает
  immutable STR generations, а local transaction закрепляет physical generation
  одновременно с logical snapshot при begin и Read Committed refresh.
- `index_store` — Base и Effective index images, class/label/count lookup и
  проверка Unique;
- `conflict` — независимые `BaseAtom`, `GraphMembership`, `GraphOverlay` и
  `GraphIncidence` conflict keys;
- `transaction` — private root, FCW/SSI validation, bounded fanout preflight и
  replay непересекающихся writes поверх актуального root;
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
- Commit под одним mutex сначала проверяет FCW, structural requirements, SSI и
  fanout, затем строит полный новый root и публикует его одной заменой `Arc`.
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
- `LocalOptions` является единственной construction authority локального
  backend. `local_storage_status()` только наблюдает effective budgets и
  pressure; environment variables не переопределяют параметры экземпляра.
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

Проверять focused schema/value/root/transaction/query tests, RAM/local parity,
reopen/recovery в обеих I/O feature-конфигурациях и effective-index/predicate
regressions. Ошибка commit никогда не должна менять опубликованный root.
