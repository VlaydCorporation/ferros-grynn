# Values module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/values`.

## Границы

- `runtime` — публичный Rust SDK и не зависит от storage engine, Arrow или DataFusion.
- `storage` — внутренние DTO, `TypeTag`, encode/decode contexts и canonical codec.
- `columnar` — будущая пакетная материализация Arrow/DataFusion; она не заменяет runtime-типы.
- `ValueKind`, `Type` и `TypeTag` решают разные задачи. Не переносить `TypeTag` в `runtime`.
- `option<T>` существует только как `Type`; runtime-отсутствие представлено `Value::None`.

## Инварианты runtime

- Расширять `#[non_exhaustive] Value` только одновременно с реализацией типа, `ValueKind`,
  `Type::of`, `Eq`/`Ord`/`Hash`, `Display`, conversions и тестов.
- Порядок `Value` задаётся только `ValueKind::stable_rank`; порядок вариантов enum не является
  контрактом.
- `none` и `null` — разные обычные sentinel. Трёхзначную логику и `OrdValue` не возвращать.
- `Number` сравнивается точно между `Int`, `Float` и `Decimal`; `-0.0 == 0.0`, все NaN
  канонически равны. Любое изменение `Eq` обязано одновременно сохранять `Ord` и `Hash`.
- Сужающие и потенциально неточные преобразования реализуются только через `TryFrom`.
  Silent-default conversions запрещены.
- Публичный fallible runtime API возвращает `FerrosGrynnResult`. Не вводить локальные
  `thiserror`-enum; value-статусы принадлежат `Domain::Value`, а ошибки внешних crates
  сохраняются только как redacted `technical_source`.
- `Text` — единственная публичная runtime-обёртка строк; `CompactString` используется только
  как её внутреннее представление. Прямые зависимости остальных values-типов от `compact_str`
  запрещены.
- `Object` остаётся публичным `BTreeMap<Text, Value>`. `PropertyMap` — отдельный
  однословный sorted `ThinVec<(PropertyKeyId, Value)>`: empty не аллоцирует, non-empty имеет
  один contiguous buffer. Не возвращать tagged-pointer/ложный inline hybrid без benchmark evidence.
- Не выдавать `DerefMut` или внешнее внутреннее представление, если оно позволяет обойти
  инварианты.
- `RecordIdPart` допускает только int/string/uuid/ulid/array/object; вложенные array/object
  проверяются рекурсивно с лимитом глубины.
- Runtime-only значения сравниваются по `RuntimeId`, а storage-конвертер обязан отклонять их
  структурированной ошибкой.

## Graph surface

- Корневой `ferros_grynn::graph` реэкспортирует `values::runtime::graph`.
- `Graph<Mutable>` не помещается в `Value`; `Value::Graph` принимает только `Graph<Frozen>`.
- Графовые значения сравниваются по `(identity, revision)`, не по содержимому или изоморфизму.
- Текущий general graph foundation поддерживает node/edge и edge/edge incidence. Tree, Forest,
  Dag, Hypergraph и Metagraph нельзя реализовывать как номинальные обёртки над `Graph`;
  их отдельные storage families относятся к этапу 4 `docs/plan.md`.
- Внутренний `AtomId` не является публичным GQL ID. На границе query/storage используется
  `RecordId`/`GraphRef`.
- Production-раскладка `AtomId` — `gen(23) | kind(1) | slot(40)`; старую u31-раскладку
  прототипа `fg-meta` не переносить.

## Проверка

После изменений:

```text
cargo +nightly fmt --check
cargo check
cargo clippy --all-targets
cargo test
```

Новые ветви логики покрывать unit-тестами. Законы `Eq`/`Ord`/`Hash`, канонический порядок
коллекций и cross-number comparison дополнительно покрывать property-based тестами.
