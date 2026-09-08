# Datastore module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/datastore`.

## Назначение

`datastore` — публичный composition root для RAM/local backends, каталогов,
MVCC-транзакций и query adapter. Он координирует домены, но не владеет
page/WAL layout (`storage`), graph algorithms (`graph`) или синтаксисом (`syn`).

## Карта владельцев

- `options` — backend, I/O, checkpoint и transaction options;
- `schema` — graph/table/field/index definitions и validation;
- `root` — immutable MVCC root, catalogs и version histories;
- `backend` — in-memory/local selection и durable publication adapter;
- `transaction/conflict` — read/write/predicate sets, FCW и SSI validation;
- `transaction/mutation` — table/graph mutation views;
- `transaction/query` — `QuerySnapshot` и execution adapter;
- `transaction/graph_view` — snapshot-bound `GraphRead`;
- `transaction/mod.rs` — lifecycle, commit/rollback и public transaction API.

## Инварианты

- Commit сначала валидирует limits/conflicts, затем делает local state durable,
  и только потом публикует новый root.
- Read-only/query results возвращают runtime `Value`/`RecordId`, не storage DTO
  и не `AtomId`.
- Serializable predicate tracking покрывает scans и graph/catalog phantoms;
  FCW остаётся отдельной write/write гарантией.
- Активные snapshot-и регистрируются и освобождаются на всех success/error/drop
  путях; age/write-set limits не обходятся вспомогательным API.
- Текущий global commit serialization является явной простой границей. Не
  заменять её lock-free MVCC без профиля bottleneck и полного anomaly suite.
- Таблица с `graph() == None` принадлежит системному `GraphId = 0`; не создавать
  для неё фиктивный runtime-граф. Удаление именованного графа атомарно удаляет
  его таблицы, а системные таблицы не затрагивает.
- DDL изменяет тот же транзакционный `SchemaCatalog`, что и Rust API. Secondary
  index имеет одно наблюдаемое состояние `Ready`, `Stale` или
  `PreparedForRemoval`; состояние участвует в выборе access path.

## Проверка

Для изменения запускать узкие RAM и local сценарии, затем SSI/FCW, limit,
reopen/recovery и query tests затронутого пути. Backend-neutral поведение
проверять на обоих backends; disk failure не должен публиковать RAM root.
