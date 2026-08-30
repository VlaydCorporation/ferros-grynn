# Аналитический отчёт по этапу 1.6

**Дата среза:** 30 августа 2026 года.
**Область:** корневой crate `ferros-grynn`, production-дерево `src/`, прямые
зависимости корневого [`Cargo.toml`](../../Cargo.toml) и локальные reference-копии,
прямо перечисленные в пункте 1.6 [`full_plan.md`](../full_plan.md).

## 1. Результат и метод

После обновления lockfile команда
`cargo metadata --format-version 1 --locked --all-features --offline` успешно
разрешает 815 пакетов. У корневого package 156 прямых деклараций, которые дают
155 уникальных package edges: `mimalloc-pprof` объявлен и как target dependency,
и как target dev-dependency. Транзитивные зависимости ниже не перечисляются:
они принадлежат прямым crates и отдельно не выбираются проектом.

Проверка включала:

- фактическое подключение модулей из [`src/lib.rs`](../../src/lib.rs), а не
  только наличие файлов;
- поиск импортов каждого прямого crate во всём `src`, tests, benchmark package
  и proc-macro package с последующей ручной проверкой ложных совпадений;
- чтение текущих владельцев parser, values, datastore, storage и graph;
- Graphify query по связям модулей и последующую сверку с исходниками;
- чтение локальных reference-копий SurrealDB token/collections/AST/types,
  `storekey`, `surrealmx` и `ferntree`.

В рамках 1.6 production Rust-код, `Cargo.toml`, lockfile и форматы хранения не
изменялись. Выводы ниже являются решениями для будущих отдельных изменений, а
не разрешением подключать всё найденное.

Главный вывод: текущий manifest одновременно содержит рабочее ядро и большой
список предполагаемых технологий. Сохранять dependency «на будущее» не нужно:
crate следует подключать в том вертикальном срезе, где появился первый реальный
consumer и проверка. Ни один из исследованных внешних подходов не следует
переносить целиком.

## 2. `logos` и lexer

Текущий [`src/syn/lexer/mod.rs`](../../src/syn/lexer/mod.rs) — небольшой
handwritten UTF-8 byte scanner. Он уже сохраняет `TextSpan`, `gap_before`,
comments, escapes, quoted identifiers, parameters и greedy graph operators, а
ошибки создаёт через project status system. Lexer выдаёт eager token buffer,
которым владеет один parser state.

SurrealDB [`token`](../../crates/surrealdb/surrealdb/token) применяет `logos` для
хорошо локализованных token categories и вынесен в отдельный crate, чтобы
generated code меньше влиял на core compilation. При этом сложный GQL lexer в
SurrealDB не сводится целиком к одному derive: context-sensitive literals и
ошибки всё равно требуют ручной логики.

**Решение:** сейчас не мигрировать и удалить неиспользуемую dependency `logos`.
Переоценка нужна после grammar freeze либо если token catalog/ручные branches
измеримо разрастутся. Условие миграции — equivalence suite для comments,
escapes, quoted IDs, parameters, Unicode byte spans, whitespace adjacency,
multi-character operators и всех error statuses плюс lexer benchmark. Если
`logos` победит, generated token layer следует изолировать, а не менять AST и
parser одновременно.

## 3. `VecMap` и `VecSet`

SurrealDB [`VecMap`](../../crates/surrealdb/surrealdb/collections/src/vec_map.rs)
и [`VecSet`](../../crates/surrealdb/surrealdb/collections/src/vec_set.rs) хранят
sorted unique elements в `Vec`, используют linear search на малом размере и
binary search далее, а вставка остаётся `O(n)`. Реализации также несут SurrealDB
revision/storekey contracts, которые Ferros-Grynn не нужны.

В Ferros-Grynn уже есть три подходящих domain representations:

- `PropertyMap` — sorted `ThinVec`, компактный empty header и canonical order;
- runtime `Object`/`Set` — `BTreeMap`/`BTreeSet` с устойчивым порядком;
- graph adjacency — `SmallVec`, где inline cardinality является частью
  измеренного layout.

**Решение:** общий `VecMap`/`VecSet` не копировать. Для нового read-heavy малого
domain container можно повторить сам pattern «sorted vector», но только после
benchmark конкретной cardinality. Порог SurrealDB `64` не переносится без
измерения. `ThinVec` выбирать ради дешёвого empty representation, `SmallVec` —
только когда стабильно малая cardinality окупает увеличенный object size.
Backend-generic trait над `Vec`/`SmallVec`/`ThinVec` не нужен.

## 4. Top-level statements и macro AST

Текущая syntax model уже отделяет [`Program`](../../src/syn/ast/mod.rs) и
[`StatementKind`](../../src/syn/stmt/mod.rs) от expression nodes, а pipeline
сохраняет `parse -> analysis -> IR -> execute`.
Это выполняет полезную часть идеи SurrealDB `TopLevelExpression`: lifecycle и
statement constructs не смешиваются с ordinary expression evaluation.

SurrealDB [`core/src/sql/ast.rs`](../../crates/surrealdb/surrealdb/core/src/sql/ast.rs)
имеет crate-private top-level discriminator для transactions, access/live и
других команд. Отдельный [`surrealdb-ast`](../../crates/surrealdb/surrealdb/ast)
— гораздо более тяжёлая arena/SoA model с generated node catalog, `NodeId`,
visitors/visualization и type-dispatch machinery, включая unsafe assumptions.
Она оправдана размером SurrealDB AST, но не текущим небольшим owned tree.

**Решение:** не копировать оба слоя. Новую top-level команду добавлять обычным
variant в существующий `StatementKind` вместе со всем vertical slice. Arena и
macro-generated catalog рассматривать только после измеренной проблемы clone,
memory или compile time. Из SurrealDB позже можно позаимствовать exhaustive
visitor/debug generation, если ручное повторение реально появится; `TypeId`/
unsafe registry переносить не следует.

## 5. Macro type system наподобие `SurrealValue`

SurrealDB derive генерирует `kind_of`, `is_value`, `into_value` и `from_value`
для structs/enums и поддерживает serde-подобные rename/tag/content/untagged/
skip/wrap strategies. Это удобно для большого внешнего DTO surface, но также
создаёт сложные recursive/representation rules.

Ferros-Grynn уже разделяет:

- runtime `Value` и `Type`;
- internal `StorageValue`/DTO;
- canonical bytes и archive envelope;
- публичные typed errors.

**Решение:** derive сейчас не создавать. Он легко сольёт независимые runtime,
typing и durability contracts. Возможный будущий derive допустим только для
SDK DTO `<-> runtime Value`, после стабилизации conversion semantics и появления
нескольких реальных повторяющихся DTO. Generated code должен возвращать
`FerrosGrynnError`, не касаться storage codec и иметь round-trip, kind и
compile-fail tests. Текущие scalar conversion macros уже покрывают фактическое
повторение.

## 6. Что позаимствовать из `storekey`

`storekey` задаёт order-preserving, не self-describing encoding: terminators и
zero escaping для variable bytes, transforms для signed integers/floats,
borrowed decode и derives. Его главный контракт — lexicographic order encoded
bytes совпадает с logical order; цена — жёсткая привязка persisted key schema к
порядку/типам полей.

В [`values/storage/order_key.rs`](../../src/values/storage/order_key.rs) уже есть
domain-specific codec для Ferros-Grynn numbers, decimal, duration, IDs, records
и ranges с project errors. Второй универсальный key format создаст
неоднозначность disk spec.

**Полезно позаимствовать только проверки и законы:** property law
`a < b => encode(a) < encode(b)`, boundaries для zero escaping, отрицательных
чисел, infinities/NaN, compound prefixes и schema-evolution warning. Borrowed
decode добавлять только если index scan действительно должен восстанавливать
keys без allocation. Derive для persisted key допустим лишь после явного
version/frozen-schema contract. Сам crate и его wire format не подключать.

## 7. Что позаимствовать из `surrealmx`

`surrealmx` — отдельная in-memory transactional KV architecture: version
chains, logical clock/oracle, SI/SSI range tracking, merge queues, savepoints,
tracked GC, transaction pooling и optional AOL/snapshot/compression. Production
Ferros-Grynn уже имеет immutable database root, global commit serialization,
FCW/SSI predicates и собственные WAL/checkpoint/recovery formats. Перенос
oracle/queue/persistence заменил бы архитектуру, а не улучшил один owner.

**Полезно позаимствовать как test ideas:**

- anomaly suite G0/G1/PMP/P4/G-single/G2 и concurrent scan phantom cases;
- savepoint law: rollback отменяет writes, но консервативно сохраняет уже
  выполненные reads/scans для SSI;
- version GC сохраняет floor-visible version и корректно собирает tombstones;
- recovery clock стартует выше максимальной persisted version;
- manual maintenance/worker shutdown tests без sleep-based assumptions.

Не переносить AOL/snapshot format, merge queue, transaction pool или preset
options. Они конфликтуют с текущим database-level WAL и простым commit model.
Lock-free/MVCC redesign допустим только после профиля, где global commit lock —
подтверждённый bottleneck.

## 8. Что позаимствовать из `ferntree`

`ferntree` — concurrent in-memory B+tree с optimistic lock coupling, hybrid
latches, fence/sample keys, epoch reclamation, bidirectional range iterators и
существенным unsafe surface. Операции атомарны на уровне key, но crate не даёт
cross-key transaction/durability. Это несовместимо с disk-page B+tree под WAL и
database transactions Ferros-Grynn.

**Полезно позаимствовать test matrix:** split/merge/root-height/cascading cases,
forward/reverse range boundaries, delete/insert churn, iterator vs clear,
oracle comparison с `BTreeMap`, fuzz и loom/race stress. Если позднее появится
OLC in-memory scalar index, дополнительно полезны fence-key validation и явный
contract, что optimistic callback может выполняться повторно.

Сам implementation и dependency не переносить. Рассматривать crate только как
optional in-memory index после измеренного bottleneck, с отдельным safety audit
unsafe/epoch reclamation. Для durability или transaction manager он не
подходит.