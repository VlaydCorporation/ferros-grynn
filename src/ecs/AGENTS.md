# ECS module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/ecs`.

## Назначение и границы

`ecs` — runtime-sidecar для компонентов, привязанных к полным graph `AtomId`.
Он хранит производные, аналитические и временно материализованные данные
(позиции, ранги, признаки, workspaces, typed property columns), не меняя
каноническую graph/storage модель.

- У ECS нет собственного entity allocator: identity и generation всегда
  приходят из `graph::AtomId`.
- ECS не является источником истины для схемы, долговечных свойств, graph
  equality или storage recovery.
- Обычные components и materialized properties используют разные namespaces;
  одинаковый Rust `TypeId` не должен смешивать разные `PropertyKeyId`.
- Связь sidecar с графом должна учитывать `(identity, revision)` и очищаться или
  инвалидироваться после мутации/смены snapshot.

## Карта подмодулей

- `mod.rs` — `AnyPool`, type-erased registry `WorldComponents`, раздельные
  component/property namespaces и lifecycle над всеми pools;
- `sparse_set.rs` — generation-aware `SparseSet<T>` с отдельными sparse pages
  для node/edge slots и dense хранением значений;
- `property_column.rs` — `PropertyColumn<T>` и трёхсоставное состояние
  `none`/`null`/`value` через независимые presence/validity данные;
- `join.rs` — `JoinIter2`…`JoinIter8` и `join!`, выбирающие наименьший component
  pool как driver.

## Будущее использование

Подходящие области: PageRank/centrality/community labels, визуализация,
dense/sparse feature vectors, transient traversal marks, algorithm workspaces
и ML/analytics components. Persisted materialization требует отдельного
versioning/invalidation контракта и не добавляется как побочный эффект ECS.

## Проверка

Проверять node/edge separation, полную generation, stale IDs, remove/retain во
всех pools, property `none`/`null`/`value`, namespace isolation и join с
отсутствующими/разного размера pools. Изменение lifecycle дополнительно
проверять вместе с graph mutation и snapshot invalidation.
