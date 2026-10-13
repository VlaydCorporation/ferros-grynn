# Bridge module guidance

Этот модуль владеет публичными owned bridge identities, object-safe async port,
query orchestration и адаптерами. Нормативный контракт: `docs/gql_spec.md` §30.1
и `docs/gql_bridges_stage_30_plan.md`.

- SDK types остаются в `surreal`; общий port принимает только runtime values.
- Registry/MVCC принадлежат `datastore`, immutable pages/WAL — `storage`.
- Receiving GQL всегда проходит существующий синхронный executor. Native SDK
  extension сохраняет оригинальный `Query`, bindings и await semantics.
- Внешний FETCH идёт через общий walker до ORDER/pagination. Не переносить его
  в постобработку и не переисполнять expressions после ожидания.
- После локального commit worker ожидает только async native IO; Ready commit
  выполняется этим же blocking worker. Не добавлять вложенный `spawn_blocking`
  в commit dispatcher: runtime может иметь один blocking поток.
- `Pending` и owned reference сохраняют неизвестный внешний исход. Resume не
  повторяет локальный RELATE, Events или changefeed. Background retries отсутствуют.
- Новые conversions обязаны быть прямыми и lossless. Display/JSON coercion,
  скрытый импорт atoms/CLASS и перезапись локальной topology запрещены.
- SDK3.3.4 проверяется stable toolchain; nightly нужен для форматирования.
  Реальные Mem/RocksDB/link/reopen gates находятся в `tests/bridges_surreal.rs`.
  Runtime-only clients/credentials не сохраняются в каталогах.
