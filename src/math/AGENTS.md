# Math module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/math`.

## Назначение и границы

`math` — единый дом переиспользуемых математических операций Ferros-Grynn.
Сюда относятся общие checked/exact операции, числовые helpers и будущие
алгоритмы, которые не принадлежат только graph, query, storage или ML слою.

- Предметная graph math остаётся в `graph`; GQL numeric semantics — в
  `values`/`gql`; page offset/layout arithmetic может вызывать `math`, но её
  on-disk контракт остаётся в `storage`.
- Не обобщать операцию на множество numeric traits без нескольких реальных
  типов-потребителей.
- Любая арифметика размеров, offsets, cardinality и budgets должна явно
  обрабатывать overflow, underflow, division by zero и нецелое деление.

## Карта подмодулей

- `mod.rs` — фасад и реэкспорт общих математических операций;
- `exact_ops.rs` — traits и функции checked add/subtract/multiply и exact
  unsigned/signed division с ошибками `gql_status`.

Новые предметные группы выделять в отдельные подмодули (`statistics`,
`distance`, `tensor` и т. п.) только после появления общей ответственности и
нескольких consumers; не превращать `mod.rs` в несвязанный набор функций.

## Проверка

Для каждой операции покрывать ноль, min/max, signed границы, overflow,
непредставимый результат и generic type families. Error mapping является
частью контракта: non-exact division не должна маскироваться под division by
zero. Для floating-point операций заранее фиксировать NaN, infinity, rounding
и determinism semantics.
