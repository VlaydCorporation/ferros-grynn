# Этап 30. Двусторонние Bridges и durable mappings

Статус: **реализован B0–B9**; собственные gates проверены в scope evidence report.
Performance gate этапа29 и общий conformance gate этапа31 остаются открытыми.

Текущие результаты и ограничения проверок:
[gql_stage30_bridge_evidence.md](reports/gql_stage30_bridge_evidence.md).

## Нормативный контракт

- Атомы, identity и topology принадлежат Ferros-Grynn. Внешняя БД хранит
  дополнительные свойства и relation recipes; cross-database atoms отсутствуют.
- `BridgeData` содержит `RecordOrClass`, `ExternalQuery`, `InternalQuery`.
  Query-scoped источники не создают CLASS или atoms. InternalQuery всегда read-only,
  включая транзитивные эффекты UDF; source возвращает один результат.
- Без mapping результат привязывается к `$bridge`; mapping принимает Object и
  напрямую связывает его поля. Bridge bindings применяются последними;
  одинаковые target names двух источников отклоняются до receiving execution.
- `Bridge` — открытый object-safe Send + Sync port; Future и owned Value не
  раскрывают SDK/storage types. Существующий sync executor сохраняется.
- `Datastore::query` предоставляет async bridge orchestration. Наш extension trait
  расширяет native Surreal Query. Credentials и clients не сериализуются.
- Named runtime adapters доступны через operation-level `WITH BRIDGE alias`.
- Реестр принадлежит datastore и сохраняется через MVCC/WAL/checkpoint. Subject
  разделяет global atom properties и graph occurrence relation. Уникальность
  обеих сторон действует в пределах external database и role.
- Mapping reference включает datastore UUID и уникальный MappingId. Состояния:
  Pending резервирует стороны, Ready подтверждает действие, Tombstoned сохраняет
  диагностику удалённого subject. Внешние records не удаляются автоматически.
- RELATE использует только локальные atoms. Local mutation и Pending reservation
  атомарны. External mirror выполняется после commit с заранее сохранённой
  identity; endpoint records не создаются. Local result и external outcome
  возвращаются раздельно, вместе с mapping reference.
- Явный resume читает сохранённую external identity: совпавшая запись подтверждает
  mapping, отсутствующая создаётся, несовместимая даёт typed conflict. Resume не
  повторяет RELATE, Events или atom changefeed.
- FETCH выполняется до ORDER/pagination и сохраняет local fields. Внешние данные
  помещаются в `external[alias]` вместе с database identity и mapping reference.
  Missing mapping/record, collision поля external и unsupported conversion
  являются typed errors; local fields не перезаписываются.
- RETURN/BUILD GRAPH разрешают external `{id,in,out}` recipes в существующие
  локальные atoms и используют общий assembler/family validators. Отсутствующий
  atom требует явного импорта. Named BUILD сохраняет atomic bulk publication.
- Нет общей atomicity/local+external snapshot, automatic retry, network backend,
  distributed transactions или durable LIVE/EVENT bridge recipes.
- Catalog cutover без compatibility decoder/миграции; новые versions явно
  отклоняют прежние catalogs. Существующие page/WAL bytes не переопределяются.

## Порядок реализации

| Шаг | Срез                                                                     |
|-----|--------------------------------------------------------------------------|
| B0  | Контракт, current checkout baseline, dependency/feature proof            |
| B1  | Shared FETCH/assembly/commit seams до добавления поведения               |
| B2  | Public identities, bidirectional registry, bounded durable codec/indexes |
| B3  | Atomic reservation, confirm, tombstone, resume, failure recovery         |
| B4  | Общий port, builders, source bindings, fake contract tests               |
| B5  | SDK 3.3.4, optional Mem/RocksDB, exact conversions, native extension     |
| B6  | WITH BRIDGE RELATE vertical slice и post-commit effects                  |
| B7  | Shared FETCH, bounded blocking/async request-reply, ordering             |
| B8  | Relation normalization в общем RETURN/BUILD assembler                    |
| B9  | Diagnostics, examples, correctness/resource/performance gates, Graphify  |

## Acceptance

Базовые tests: обоих направлений/mapping shapes/errors; обе уникальные стороны,
concurrent reservations и чужие refs; commit/abort/reopen/crash/resume; nested
FETCH/collections/order/pagination; graph identities/incidence/family constraints;
cancel/deadline/admission/drop. RAM/local Sync/NativeAsync, Mem/RocksDB и совместные
features проверяются отдельно. Обычные запросы сравниваются с baseline при 5% gate;
lookup/reservation проверяются на 10K/100K/1M без scans для point lookup.

Baseline: current production `2a7f465`, stable 1.98.1, library no-default —
1334 passed, 0 failed, 10 ignored. SDK compile spike 3.3.4: Mem, RocksDB и union
PASS на stable, defaults disabled. Nightly diskann 0.56.0 compile failure отдельно;
nightly используется для formatter. Реальные runtime/link/reopen проверки Mem,
RocksDB и объединения features прошли; точные команды и evidence указаны в отчёте.

Открытый performance gate этапа 29 не закрывается этим этапом; этап 31 также
остаётся отдельным conformance gate. Unrelated dirty work сохраняется.

## Использование реализованного API

`bridge-surreal-mem` и `bridge-surreal-rocksdb` включают общий optional adapter
`bridge-surreal`. SDK 3.3.4 использует отключённые defaults. Асинхронное исполнение
требует Tokio runtime с time driver и blocking pool; обычные `execute` остаются
синхронными. Полные compile-checked примеры двух extension traits находятся в
rustdoc `ferros_grynn::bridge`; runtime fixtures — в `tests/bridges_surreal.rs`.

Следующий фрагмент предполагает существующие локальные classes `person`, `link`,
граф `graph`, atoms `person:a`, `person:b` и внешние records `profile:a`, `profile:b`.
Сначала регистрируется durable descriptor, затем runtime client:

```rust
use ferros_grynn::bridge::{
    BridgeDatabaseOptions, BridgeEntityRef, BridgeMappingSubject, SurrealBridge,
};

fn some() {
  let mut tx = store.begin();
  let database = tx.register_bridge_database(
    BridgeDatabaseOptions::new("sdb", "surrealdb", "ns", "db")?
            .map_class("person", "profile")?
            .map_graph(graph.id(), "relations")?,
  )?;
  for id in ["a", "b"] {
    tx.register_bridge_mapping(
      BridgeMappingSubject::Atom(format!("person:{id}").parse()?),
      BridgeEntityRef { database, record: format!("profile:{id}").parse()? },
    )?;
  }
  tx.commit()?;
  let metadata = store.bridge_database("sdb")?.ok_or_else(|| {
    ferros_grynn::gql_status::FerrosGrynnError::bridge_mapping_error("descriptor absent")
  })?;
  store.attach_bridge(database, SurrealBridge::new(client, metadata).await?)?;
}
```

Class/table correspondence задаёт допустимый namespace, но не создаёт atom mapping
для каждой записи. Существующие соответствия регистрируются явно, как в примере.
После reopen descriptor и mappings сохраняются; client подключается заново.

```rust
fn some() {
  let reply = store.query(
    "RELATE (person:a)-[link:r]->(person:b) IN GRAPH g WITH BRIDGE sdb; \
     SELECT id AS edge FROM link:r IN GRAPH g FETCH edge WITH BRIDGE sdb;"
  ).execute().await?;
  // Local results and external action outcomes must both be inspected.
  for effect in &reply.effects {
    let reference = effect.mapping;
    // Persist this owned reference when the external outcome is unknown.
    // An explicit later call can resume without repeating local RELATE:
    // store.resume_bridge_mapping(reference).await?;
  }
}
```

Query-only data передаются через `.use_bridge(source, BridgeData::ExternalQuery(...))`
в `$bridge`, через `RecordOrClass(source, parameter)` в указанный parameter либо через
Object field mapping. Обычные `.bind(Object)` объединяются; bridge assignments
применяются последними. Native `client.query(...).use_bridge(FerrosBridge::new(store),
BridgeData::InternalQuery(...)).await` исполняет read-only GQL source перед native query.

External relation recipes передаются типизированным массивом в `$recipes`:

```sql
RETURN GRAPH {nodes: [], edges: $recipes} WITH BRIDGE sdb;
BUILD GRAPH imported FROM $recipes WITH BRIDGE sdb;
```

Все recipe identities должны иметь Ready mappings в выбранной БД. Отсутствующий
локальный atom требует отдельного явного импорта. FETCH возвращает локальную запись
с `external.sdb = {database, mapping, properties}` до ORDER и pagination.
