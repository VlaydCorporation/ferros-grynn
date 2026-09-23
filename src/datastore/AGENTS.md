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
- `identity` — `DataSpaceId::Database` либо ненулевой `GraphId` для будущего WAL;
- `schema` — `ClassSchema`, `GraphSchema` и типизированные durable identities;
- `root` — единый immutable commit image AtomStore + graphs + logical versions;
- `index_store` — Base и Effective index images, class/label/count lookup и
  проверка Unique;
- `conflict` — независимые `BaseAtom`, `GraphMembership`, `GraphOverlay` и
  `GraphIncidence` conflict keys;
- `transaction` — private root, FCW/SSI validation, bounded fanout preflight и
  replay непересекающихся writes поверх актуального root;
- `store` — datastore owner, ID clocks и один commit mutex;
- `options`/`consts` — transaction и будущие local-storage resource limits.

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

## Проверка

Проверять focused schema/value/root/transaction/query tests, RAM/local parity,
reopen/recovery в обеих I/O feature-конфигурациях и effective-index/predicate
regressions. Ошибка commit никогда не должна менять опубликованный root.
