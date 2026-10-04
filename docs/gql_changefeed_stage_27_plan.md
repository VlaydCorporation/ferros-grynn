# Этап 27 — Durable Changefeed

## 1. Статус и границы

Нормативный контракт этапа 27 `full_plan.md`. Зависимости: этапы 9, 19, 20.
Открытые gates этапов 24–26 не блокируют работу. Этап 27 в реализации;
закрытие допускается только по evidence полного C10 gate.

Срезы выполняются C0 → C1 → … → C10, после correctness gate предыдущего.
LIVE registry, очереди, ACK, Events и LiveGraph относятся к этапам 28–29.
Query pipeline, snapshot/SSI, `Value`, `RecordId`, graph references и Path
сохраняются. Q-40: development format изменяется на месте, без compatibility
decoders. Новые dependencies, второй WAL и второй maintenance worker запрещены.

## 2. Нормативные правила

| ID    | Правило                                                                                                                                                                                                                                                                                                          |
|-------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| CF-01 | `Versionstamp` — existing database-global durable `commit_ts`, один на commit. Checked ordinals различают entries; дедупликация выполняется по source/stamp/ordinal внутри фиксированного read mode. Sparse stamps не означают GAP.                                                                              |
| CF-02 | Финальный validated catalog и transactional config changes определяют policy для ВСЕХ DML commit, включая операции до включения FULL. Финальный DROP подавляет DML и публикует GAP/control transition прежнего feed.                                                                                             |
| CF-03 | `CHANGEFEED duration [INCLUDE ORIGINAL] [FULL]`; duration положителен, имеет фиксированное число nanoseconds, не содержит years/months. Equivalent fixed spans имеют одинаковую policy.                                                                                                                          |
| CF-04 | NET — один before/after на identity за commit, без net equality. FULL — успешные logical operations в порядке выполнения, включая targeted no-op; zero matches, failed operation и abort событий не создают. Internal setters/cascade effects принадлежат outer operation.                                       |
| CF-05 | FULL read проверяет requested historical coverage до row pull. NET-only coverage не позволяет выдумывать промежуточные операции и возвращает typed error.                                                                                                                                                        |
| CF-06 | Default storage: полный after и обратимый before-diff; INCLUDE ORIGINAL: полный before. Create имеет absent before, delete — absent after. Decoding не читает текущие primary records.                                                                                                                           |
| CF-07 | CLASS identity — global RecordId; GRAPH identity — qualified occurrence. Images сохраняют named properties, overlay decisions, labels, weight, topology, ordered qualified endpoints/containment route и graph references. Local AtomId/GlobalAtomId не экспортируются.                                          |
| CF-08 | GRAPH scope локален. Base mutation fanout перепроверяется по current reverse memberships; masked effective values не порождают property event. No-op FULL применяется к явно targeted identity. Containment не создаёт recursive source.                                                                         |
| CF-09 | SINCE stamp exclusive, SINCE timestamp inclusive. UTC committed_at persisted/nondecreasing. Cursor содержит durable database UUID, source, mode и последний целиком прочитанный stamp; не является QueryCursor или WAL LSN.                                                                                      |
| CF-10 | Reader фиксирует upper snapshot boundary. LIMIT считает whole commit batches/GAP items; commit не режется. EOF может продвинуть cursor через stamps без событий. Возвращаемые images owned, reader удерживает snapshot/pins.                                                                                     |
| CF-11 | Expired/suppressed/corrupt known history даёт explicit GAP с известными bounds и доступным хвостом. Unknown bounds обозначаются явно; authority corruption без доказуемых bounds — typed error.                                                                                                                  |
| CF-12 | DROP/REMOVE сохраняют историю/tombstone до TTL. Active name wins; retired name разрешается только однозначно; ChangefeedId выбирает старый source. Config updates сохраняют source id. После полного retirement повторное включение получает новый id. Старый cursor получает terminal FeedExpiredOrRetired GAP. |
| CF-13 | Import config frozen: current schema DDL ban и writer lease сохраняются. CHANGEFEED option default OFF. ON accepted-item history переживает segment seals; final validation/END публикует один commit. Failed/Skip/Quarantine item откатывает свою историю. OFF маркирует подавленные события GAP.               |
| CF-14 | Primary, feed pages, catalog и bounds публикуются одним TxHandle/TxCommit/barrier. Feed-only no-op проходит writer/SSI validation. Uncertain WAL result запрещает преждевременный unpublished-file cleanup.                                                                                                      |
| CF-15 | Capture metadata/images/read buffers/spool bounded до allocation/write. Feed frames делят existing 256 MiB staged-WAL cap с primary/catalog. Ошибка не публикует partial result; silently dropped events запрещены.                                                                                              |
| CF-16 | .fcf — registered WalBackfilled files, existing Changefeed PageTypes. Catalog является authority; filename/checkpoint LSN — нет. Checked lengths/counts/references/reserved bytes, common checksum и bounded continuation codec обязательны.                                                                     |
| CF-17 | Target segment 64 MiB, rotation между commit groups. Purge публикует floor/unreferences до unlink; pins защищают old snapshots. Existing worker получает bounded purge action и timed idle wake; job coalescing учитывает kind, а не только Database target.                                                     |

## 3. Rust API и GQL

Public domain принадлежит `changefeed`; transaction capture/publication — datastore;
pages/codecs/file registration — storage. Нет публичных physical DTO.

Rust: validated `ChangefeedConfig`, `ChangefeedSource`/`ChangefeedSelector`,
`ChangefeedId`, `Versionstamp`, `ChangefeedCursor`, owned `ChangeBatch`/
`ChangeEntry`/`ChangeImage`/`ChangefeedGap`. Transaction exposes configure/drop/info/
changes; pull reader exposes next_batch/cursor. Datastore exposes retention purge
через existing job API; CommitResult exposes versionstamp. Cursor encode/decode
работает с existing Bytes, без новой Value variant.

`ChangefeedReadOptions::new(ChangefeedSince)` выбирает exclusive Versionstamp,
inclusive UTC Timestamp либо validated Cursor. Default — NET от stamp 0, whole-item
limit без ограничения, 256 MiB scratch и 1M work units. `with_mode`/`with_limit`/
`with_budgets` задают те же параметры Rust и GQL reader. FULL preflight покрывает
весь выбранный interval до snapshot upper bound независимо от LIMIT; читает headers,
не копирует dictionaries/images. Resource exhaustion — error до выдачи строк.
Reader borrowing запрещает mutation/commit транзакции до его освобождения; Read
Committed фиксируется на создании reader. Private writes не являются committed history.
Cursor у timestamp read до первого pull отсутствует; EOF может продвинуть его до
snapshot stamp. После ошибки reader прекращает pull, сохраняя последний cursor.

```gql
DEFINE CLASS Item TYPE NODE CHANGEFEED 7d INCLUDE ORIGINAL FULL;
ALTER CLASS Item CHANGEFEED 7d FULL;
ALTER CLASS Item DROP CHANGEFEED;
SHOW CHANGES FOR CLASS Item SINCE $since FULL LIMIT 100;
SHOW CHANGES FOR GRAPH $graph SINCE $since;
SHOW CHANGES FOR CHANGEFEED $id SINCE $cursor;
INFO FOR CHANGEFEED $id;
```

DEFINE/ALTER GRAPH получают те же CHANGEFEED clauses. SHOW выдаёт whole commit
или GAP; stamp преобразуется через exact existing Number::from(u64).

## 4. Срезы и acceptance matrix

| Срез | Работа                                                                      | Gate / evidence                                                                          |
|------|-----------------------------------------------------------------------------|------------------------------------------------------------------------------------------|
| C0   | Normative sync, baseline snapshot и scan/DML/import baseline                | Contract review, baseline datastore gate; performance measurements записываются отдельно |
| C1   | Definitions, UUID, capture metadata, source ids/cursor, configure/drop/info | CF-01/03/09/12; SDK/cursor malformed inputs и metadata reopen                            |
| C2   | Compact operation boundaries/target intents и savepoint accounting          | CF-02/04/08/15; SDK/GQL equality fast paths, rollback, no-feeds fast path                |
| C3   | Validated replay images, NET/FULL, import spool/options                     | CF-02/04–08/13; independent operation oracle, imports через несколько seals              |
| C4   | .fcf registration и checked physical codecs                                 | CF-06/07/15/16; roundtrip, corruption, malformed lengths/refs и overflow                 |
| C5   | Unified WAL publication/recovery                                            | CF-14/16; normal/catalog-only/feed-only/import, uncertain outcome и orphan recovery      |
| C6   | Snapshot pull reader, SINCE, cursor и GAP                                   | CF-05/09–12; whole-commit LIMIT, retained tail, wrong origin/mode/source                 |
| C7   | Rotation/TTL/floor/retirement, existing worker                              | CF-12/15/17; idle purge, pins, cancellation/reopen и kind-aware coalescing               |
| C8   | Grammar → AST → analysis → IR → execution                                   | SDK/GQL differential parity; CLASS/GRAPH/id selectors и import ON/OFF                    |
| C9   | Explain/PlanHash/counters                                                   | Stable binding/mode/bounds; bytes/pages/work/scratch/GAP/purge counters                  |
| C10  | Full correctness/resource/lifecycle/performance/tooling closure             | Russian evidence report, specs/AGENTS/registry/full_plan и graphify update               |

Все graph families, overlays, hyperedges, edge-endpoints, nested/shared graphs
и qualified references входят в differential tests. Сравнение image учитывает
canonical stored representation: Number Eq не заменяет проверку numeric tags.

Examples: 0→10, enable FULL, 10→20 публикует FULL обеих операций и NET 0→20;
0→10→0 публикует две FULL entries и ноль NET entries. No-op-only FULL commit
получает новый stamp. Настоящий read-only/zero-target commit событий не создаёт.

## 5. Evidence и tooling

После каждого среза: focused tests, cargo check --lib --no-default-features,
targeted nightly rustfmt и Clippy с отделением pre-existing debt. C10: full lib/
datastore correctness, affected integration/doc suites, RAM/local Sync/NativeAsync
в отдельных targets. Фильтр datastore::tests с нулём tests не является evidence.

Paired 10K/100K/1M: capture NET/FULL, ON/OFF import, warm/cold reads, mutation,
rotation и purge. Latency/page reads/WAL/feed bytes/peak memory сохраняются;
непрофильные query/DML regressions ≤5%. До commit: just all-check, защита dirty
changes от unrelated formatter rewrites. Любой открытый gate оставляет этап OPEN.

## 6. Implementation routing

C0 contract owner: specs. C1 caller contract owner: rust-api-design; explicit
durable metadata/cursor encoding owner: rust-serialization. Capture/replay и
publication phases route separately, без загрузки future LIVE profiles.
Main agent — sole writer; read-only scout/reviewer проверяют bounded slices.
RuleSet C1: api-newtype-safety (ids), api-parse-dont-validate (retention/cursor),
api-non-exhaustive (evolving results), api-builder-must-use (config options),
api-common-traits (owned boundary types). Coverage gaps должны быть пустыми
перед Rust edits; verification отдельно принадлежит rust-verify.

C2 owner: rust-database для operation boundaries, savepoints и final-policy
validation. Triggered helper refactoring выделяет stateful replay с одним
ReplayRemap, сохраняя прежние errors/ordering и одноразовую statistics publication.
Journal хранит identities/versions/ranges; изображений и root snapshots в нём нет.
Coverage: journal ownership/bounds и rollback — database; replay extraction —
refactoring. Future physical publication/retention profiles остаются deferred.

C7 ProfileStack: purge floor/closed-source lifecycle — rust-database owner;
checked catalog fields — rust-serialization owner; timed wake, cancellation и
pin retirement — rust-concurrency owner. Existing worker/Condvar/TxHandle остаются
единственными владельцами этих protocols. Lock order: coordinator state не
удерживается во время idle purge, commit lock предшествует local state/WAL.
Retirement metadata задаёт первый доступный commit page и число пропущенных
groups в первом retained segment; floor может лежать внутри физического файла.
Reader не возвращает expired prefix. Unlink требует полного unreference и
освобождения pins; bounded passes продолжаются тем же worker.

C8 ProfileStack: source grammar/semantic rules — specs owner; owned syntax and
typed query boundary — rust-api-design owner; mutation/read classification and
snapshot SDK adapter — rust-database owner. Coding coverage: api-parse-dont-validate,
api-non-exhaustive и explicit typed errors. SHOW не обходит analysis; CLASS/GRAPH
history names разрешаются через feed catalog. GQL image projection не читает storage
DTO и учитывает retained rows и next-batch scratch в общем query barrier budget.

C9 ProfileStack: telemetry units, bounded cardinality и EXPLAIN — rust-observability
owner; owned Rust observation types — rust-api-design owner; snapshot binding и
publication counters — rust-database owner. RuleSet: obs-structured-fields,
obs-no-sensitive-data, api-non-exhaustive. Reader counters измеряют logical page
requests, whole items, inspected groups и admitted scratch, не disk misses/RSS.
EXPLAIN не исполняет UDF для binding; dynamic operands остаются runtime-bound.
Images/properties, file paths и WAL identities в telemetry не попадают.

History request descriptor хеширует bounded analyzer-owned UTF-8 spelling operands
(без offsets), runtime slots, private function catalog и committed database/snapshot.
Он чувствителен к formatting и не является normalized relation fingerprint.
Block bodies и builtin options входят в descriptor без recursive IR hashing;
Полный bounded function descriptor фиксирует private versions и bodies, в том числе
rollback/redefine с одинаковой version. Catalog обходится без клонирования коллекции;
тексты не выводятся в telemetry. Hash work считает UTF-8/canonical bytes и bounded
slot/type traversal до buffer allocation. Path, Geometry и Error cause trees проходят
тот же checked depth/work walk. Deferred operand не
допускает преждевременную semantic error последующего operand/catalog lookup.

C10 ProfileStack: measurement/workload equivalence — rust-performance owner;
command matrix и nonzero backend selection — rust-verify owner; независимые
NET/FULL/projection/lifecycle assertions — rust-testing owner. Root остаётся sole
writer. Профилирование и optimizations допускаются после сопоставимого замера;
общая lint debt не исправляется ради changefeed. Criterion common probe использует
один исходник для C0/current, 64 mutations и 16 point reads на фоне 10K/100K/1M.
Import observation отдельно проверяет полный результат либо atomic rejection;
single observation не объявляется statistical performance gate. Admitted scratch
и physical feed/page bytes не подменяют измеренный peak RSS или WAL bytes.
Executable public examples — rust-documentation owner (`doc-examples-section`,
`doc-errors-section`). Compile-fail reader lifetime имеет самостоятельный test; прежний
CSR compile-fail не считается evidence этого контракта.

Observed C10 hotspot в `DatabaseRoot::change_base_property` допускает bounded
refactoring: все fallible index/statistics checks завершаются до primary mutation;
`Arc::make_mut` отделяет shared snapshot/savepoint, но переиспользует unique private
AtomStoreRoot между setters. Ownership этой перестановки — refactoring, измерение
и принятие performance результата — rust-performance. Public behavior, storage
layout, ошибки и replay ordering сохраняются; focused и полный gates обязательны.
Capture Criterion содержит растущую историю; start/end groups/bytes выводятся явно.
Import probe проверяет exact whole-commit/GAP и images; на 100K/1M только typed
resource rejection допустим, с неизменными stamp/metadata и отсутствием primary/history.
`ttl_to_purge_complete_ns` включает TTL DDL и ожидание worker; first-read не означает
OS-cold I/O. Retained WAL footprint не является cumulative bytes written.

Observed ON import per-item COW допускает второй bounded refactoring: один
admitted before-root и compact accepted-item journal удерживаются до private
segment seal. Seal выполняет ordered replay до очистки mutations; accepted images
переносятся в прежний spool. OFF, item rollback и writer lease сохраняются.
Root/spool/journal входят в общий ON admission; seal error отравляет import session
и запрещает END publication даже после перехвата ошибки пользователем. Ownership
root lifetime — refactoring; admission/publication — rust-database; comparative
timing — rust-performance. Segment replay не меняет execution ordinals и FULL no-op.
Purge COW version map использует существующий conservative tree-allocation helper
с фактическим sizeof typed `(ConflictKey, u64)`: эти entries не владеют payloads.
Generic width128 искусственно отклонял 1M primary versions под default256MiB.
Existing tree overhead/admission/cancellation сохраняются; owner — rust-database.

Controlled RETURN1 pairs с одинаковой CPU affinity сохраняют >5% intervals.
Допускается отдельный reversible эксперимент над внутренним root ownership:
catalog/history/segments группируются одним `Arc<ChangefeedRoot>`; внутри остаются
три независимых Arc. Read accessors не клонируют и не выполняют COW; mutable
accessors сначала отделяют binding, затем существующий inner COW выбранной map.
Maintenance сравнивает только catalog, dedicated WAL/file pins сохраняются,
storage DTOs/bytes, clocks, UUID и публичный SDK не меняются. Новый outer binding
покрывается прежними fixed metadata reserves; накопительный spool charge не
добавляется. Decision owner — refactoring; coding — rust-ownership
(`own-arc-shared`, `own-clone-explicit`); paired measurement — rust-performance;
bounded source reviewer и full/backend gates проверяют сохранённый контракт.
Изменение оставляется только при подтверждённом выигрыше в той же paired нагрузке.

### C10 rule-to-test additions

| Rules       | Независимая boundary evidence                                                                                                                                |
|-------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------|
| CF-02/04/06 | `tests/changefeed_oracle.rs`: 256 exhaustive optional-record transitions, NET/FULL, identity/value/order и final INCLUDE ORIGINAL                            |
| CF-04/07/08 | `tests/changefeed_lifecycle.rs`: actual shared/hyper/edge-owner routes, cascade, writeback и BUILD GRAPH logical groups                                      |
| CF-04/10/14 | Lifecycle TRY/abort/zero-match, fixed ReadCommitted reader и Serializable FULL-only cycle                                                                    |
| CF-07/14/16 | Separate local Sync/NativeAsync qualified image targets: exact graph identities и owned images после checkpoint/reopen                                       |
| CF-14       | `changefeed_local_uncertain_wal_full_noop_recovers_one_complete_commit`: four barrier failure points, poisoned retry, old-or-whole commit и repeated reopen  |
| CF-15       | `changefeed_wal_cap_is_shared_with_primary_and_catalog`: shared staged counter, replacement и rejection на simulated previously admitted cap                 |
| CF-10/14/15 | `changefeed_binding_detaches_selectively`: старый catalog и sharing sibling maps сохраняются; payload/file lifetime проверяют existing reader/backend suites |

Последние два fixtures не объявляются power-loss process crash test или измерением
реальной allocation/write 256 MiB. Публикация bounds/image/stamp проверяется отдельно
от физического cap; исходные import/spool/continuation/resource tests сохраняются.
