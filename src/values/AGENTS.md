# Values module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/values`.

## Границы

- `runtime` — публичный Rust SDK и не зависит от storage engine, Arrow или DataFusion.
- `storage` — внутренние DTO, `TypeTag`, encode/decode contexts и canonical codec.
- `columnar` — пакетная материализация Arrow/DataFusion; она не заменяет runtime-типы и
  feature-gated отдельно от них.
- `ValueKind`, `Type` и `TypeTag` решают разные задачи. Не переносить `TypeTag` в `runtime`.
- `option<T>` существует только как `Type`; runtime-отсутствие представлено `Value::None`.

## Карта подмодулей

- `runtime/value.rs`, `kind.rs`, `types.rs` — основной `Value`, стабильный
  порядок видов и type descriptions;
- `runtime/number.rs`, `temporal.rs`, `geometry.rs`, `vector.rs`, `regex.rs` —
  предметные scalar/runtime values;
- `runtime/array.rs`, `object.rs`, `set.rs`, `range.rs`, `bytes.rs`, `text.rs` —
  runtime collections и owning wrappers;
- `runtime/id.rs`, `record_id.rs`, `property_dictionary.rs`,
  `property_map.rs` — идентификаторы и компактные property structures;
- `runtime/conversions.rs`, `formatting.rs`, `laws.rs`, `closure.rs` —
  conversions, presentation, law tests и runtime-only callable values;
- `storage/tags_refs.rs` — stable tags и storage references;
  `dto.rs` — внутренние storage DTO; `context.rs` — conversion contexts;
  `order_key.rs` — order-preserving index keys;
- `storage/codec/encode.rs`, `decode.rs`, `envelope.rs` — canonical payload,
  bounded decoding и versioned archive envelope;
- `columnar/arrow.rs` — Arrow materialization;
  `columnar/datafusion.rs` — feature-gated DataFusion adapter.

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
- `PropertyDictionary` выдаёт плотные `PropertyKeyId` в порядке интернирования и разрешает ключи
  в обе стороны; on-disk `LabelDictionary` остаётся ответственностью storage layer.
- Не выдавать `DerefMut` или внешнее внутреннее представление, если оно позволяет обойти
  инварианты.
- `RecordIdPart` допускает только int/string/uuid/ulid/array/object; вложенные array/object
  проверяются рекурсивно с лимитом глубины.
- Runtime-only значения сравниваются по `RuntimeId`, а storage-конвертер обязан отклонять их
  структурированной ошибкой.

## Columnar и ECS

- `PropertyBatch` всегда хранит Arrow validity и отдельный presence bitmap: `presence=0` —
  `none`, `presence=1 && valid=0` — `null`.
- DataFusion видит скрытые `__fg_presence_<key_id>` поля; не передавать ему голый `RecordBatch`,
  если оператор должен различать `none` и `null`.
- До реализации `StorageValue` native Arrow conversion поддерживает scalar-набор; nested и
  runtime-only значения отклоняются доменным статусом. Не кодировать их через `Display`.
- ECS использует только полный graph `AtomId`, отдельные node/edge sparse pages и проверку
  generation после sparse lookup. Собственный `Entity` allocator запрещён.
- Materialized property pool ключуется `(PropertyKeyId, TypeId)` и не смешивается с обычным
  component pool по `TypeId`.

## Graph surface

- Корневой `ferros_grynn::graph` владеет graph SDK; `values::runtime::graph`
  остаётся совместимым реэкспортом.
- `Graph<Mutable>` не помещается в `Value`; `Value::Graph` принимает только `Graph<Frozen>`.
- Графовые значения сравниваются по `(identity, revision)`, не по содержимому или изоморфизму.
- General graph поддерживает node/edge, edge/edge, hyper-, meta- и cross-level incidence.
  `Tree`/`RootedTree`/`Forest`, `Dag` и `Hypergraph` используют отдельные storage families;
  не заменять их номинальными обёртками над `Graph`. `Metagraph` разделяет general layout как
  явно зафиксированное исключение.
- Общий алгоритмический API принимает `GraphRead` и обязан давать одинаковую семантику для
  full in-memory, transaction-bound disk view и hybrid materialization. `query_to_memory`
  освобождает исходный guard после копирования; mutable hybrid сохраняет origin mapping и
  dirty-set для явного write-back.
- `GraphWorkspace` остаётся ECS-sidecar и не входит в `Value`, equality, codec или durability.
  Derived pools привязаны к `(identity, revision)` и инвалидируются после graph mutation.
- Публичный `Path` содержит стабильные `AtomRef`, а не `AtomId`; разрешение свойств всегда
  проверяет origin identity и snapshot. `LiveGraph` не подменяет статический `Graph<Frozen>` и
  требует явного conflict resolution для пересечения remote/local dirty-set.
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
