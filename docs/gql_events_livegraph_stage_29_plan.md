# Этап 29. Events и интеграция LiveGraph

Статус: **OPEN**. Этот документ фиксирует утверждённый контракт этапа 29;
наличие определения или focused PASS не означает завершения этапа.
Зависимости: 12, 20, 27, 28. Открытые performance/tooling gates 27–28
учитываются отдельно и не блокируют correctness-срезы.

## 1. Владельцы и границы

Schema владеет EventId, definitions и версиями; datastore — operation dispatch,
outbox, savepoints, FCW/SSI и publication; GQL — единственным analyzer/evaluator;
storage — bounded descriptors, physical history и recovery. Graph владеет
runtime families, COW и conflict state. LIVE SDK содержит owned logical recipes,
delivery и adapter contract, не pages, executable IR или storage DTO.

Используются существующие evaluator, transaction savepoints, maintenance worker,
resource controls, COW и GraphAssemblyAccumulator. Нового runtime, отдельного
incremental query executor, dependencies или подключения всего asynchronous нет.
Сначала отделяются существующие operation boundaries, затем добавляется поведение.

## 2. Events

### EV-01. Definitions и исполнение

Поддерживаются DEFINE/ALTER/REMOVE EVENT ON CLASS/GRAPH, OVERWRITE/IF NOT EXISTS,
нормативные ALTER clauses и самостоятельный Rust SDK. EventId выделяется общим
schema clock. Definitions используют source/logical descriptors, stable target
identities и SchemaVersion; executable IR не сериализуется.

WHEN допускает только read-only effects. THEN использует обычные expressions,
blocks, DML, UDF и control flow. Управление внешней transaction, IMPORT и запуск
административных jobs внутри EVENT запрещены. INFO CLASS/GRAPH показывает Events;
INFO/KILL JOB поддерживает async invocations.

### EV-02. Граница изменения и контекст

CLASS получает CREATE/UPDATE/DELETE/RELATE согласно логической операции.
GRAPH получает CREATE при появлении occurrence, DELETE при удалении, UPDATE при
изменении effective image и RELATE при создании связи. Учитываются memberships,
overlays, labels, topology и effective changes через base atom.
Equality no-op не вызывает EVENT. Внутренние index/storage mutations не создают
дополнительные вызовы; существующие Changefeed operation ordinals сохраняются.

Targets обходятся в каноническом порядке; definitions — по стабильному EventId.
Контекст фиксируется для соответствующего логического изменения, до effects
следующих handlers. Доступны $event, $before, $after, $value и $input.
Для GRAPH RELATE before/after равны null; value содержит edge и qualified
source/target endpoints по направлению ребра. Input сохраняет caller payload.

### EV-03. Sync и rollback

Sync handlers исполняются после мутации в той же transaction. Исходная мутация
и её handlers составляют одну rollback boundary. TRY/CATCH восстанавливает
данные, lexical state, event metadata и pending outbox entries. Вложенные
изменения проходят тот же dispatch; sync recursion использует общий control-depth.
Commit replay не запускает handlers повторно. Definitions, predicates и handler
reads участвуют в FCW/SSI. Пользовательский код не выполняется под commit mutex.

### EV-04. Durable outbox

ASYNC WHEN вычисляется в исходной transaction. Invocation сохраняет принятую
версию EVENT, необходимый source closure UDF, context и stable invocation identity.
ALTER/REMOVE меняет последующие вызовы; существующие задания отменяются KILL JOB.
Outbox и primary/Changefeed/LIVE effects публикуются одним WAL commit.
Transactional root является authority; admin job ledger — восстанавливаемой проекцией.

Payload использует versioned WAL-backed side-format и FGDC v14. Действует
development-format policy Q-40, без compatibility decoders. Обычный StorageValue
format и pages/WAL frame contract не переопределяются.

Maintenance worker обрабатывает due attempts последовательно, чередуя их с
maintenance jobs. FG_ASYNC_EVENT_PROCESSING_INTERVAL default — 5 секунд.
Bounded parallelism рассматривается отдельно в `Q-42` ([full_plan](full_plan.md));
исследование не блокирует correctness/performance/tooling closure этапа 29.
Каждая попытка открывает новую transaction с актуальными данными; event context
остаётся историческим. Effects и completion marker публикуются атомарно.
Recovery проверяет marker перед повторным исполнением; pending corruption
является ошибкой, а не потерей задания.

RETRY default 0, диапазон 0..=255: это число дополнительных attempts.
Retry не меняет identity/depth. Async root depth 0; каждый причинный вызов EVENT
увеличивает depth. MAXDEPTH default 3, диапазон 0..=16; ограничения предков
сохраняются через sync/async переходы. Превышение возвращает typed error.

Pending ceiling — 10 000 invocations и 64 MiB на datastore, дополнительно
действуют transaction limits. Admission предшествует allocation/publication;
переполнение отклоняет исходную операцию. INFO показывает state, attempts, depth
и bounded diagnostic. KILL отменяет pending либо кооперативно прерывает attempt.
Exactly-once для внешних effects не обещается; caller использует идемпотентность.

### EV-05. Import и AtomWalk

ImportEventsMode Off/On независим от CHANGEFEED; default Off. ON выполняет sync
handlers внутри item boundary и удерживает async invocations в bounded private
spool до успешного END. SKIP/QUARANTINE откатывает полный item; abort/failed END
не публикует его invocations. Автогенерация V–V/V–E/E–V/E–E использует обычный
RELATE и общий incidence validator, включая qualified cross-level endpoints.

Qualified connector фиксирует выбранные logical owner/terminal identities на
границе attach/topology change. Rebinding owner или BUILD overwrite не может
неявно направить существующую или pending связь на другой RecordId/GraphId при
совпадении физических slots. Явная topology change либо detach/reattach принимает
новый маршрут; equality no-op не меняет это решение. Transient transaction anchors
переживают import seals и откатываются вместе с item/TRY; durable topology format
не получает executable descriptors.

RELATE endpoint reference принимает `RecordId`, `AtomRef` либо `AtomOccurrenceRef`.
Для occurrence вне connector graph задаётся explicit descriptor
`{ occurrence: @qualified_occurrence, owners: [@owner_occurrence, ...] }`.
Каждый owner принадлежит текущему уровню маршрута; terminal принадлежит child
graph последнего owner. Empty owners разрешён только для local occurrence.
Event `source_paths`/`target_paths` сохраняют ровно эти ordered owners; при двух
владельцах одного child маршрут не выводится из одного terminal occurrence.
Node/edge wrappers `<(...)>`/`<[...]>` проверяют kind; qualified occurrences не
auto-attach в connector graph. Unqualified global node references сохраняют
обычное auto-attachment RELATE, edge references требуют существующей topology.

## 3. LiveGraph

### LG-01. Families и failure atomicity

LiveGraph<State, G = Graph<State>>, LiveReadGuard и LiveUpdate сохраняют public
paths/default general Graph; sealed family contract охватывает существующие
families, включая Subgraph. Layouts и validators остаются раздельными.
Pending delta, base, local dirty state и queue меняются только после успешной
подготовки candidate. Automatic adapter использует qualified logical identities;
AtomId остаётся materialization-local. Old guards сохраняют прежнюю revision.
Это правило распространяется на ручной callback adapter: перед pending/queue
admission проверяется полный remote candidate. Paused callbacks проверяются
поверх уже принятого remote tail, а не поверх conflicted local copy. Один COW
tail удерживается до окончания очереди; повторное проигрывание всей цепочки
ради проверки каждого нового callback не требуется.

### LG-02. Общий RETURN GRAPH

SELECT получает terminal RETURN GRAPH через существующие AST, analyzer и
LogicalReturnGraph; LIVE использует тот же путь. SDK recipe задаёт family,
validators, ROOT, nodes и edges. JOIN/SPLIT/grouping, несколько sources и reactive
bindings сохраняют ordinary SELECT semantics. Допустимость определяется общей
graph assembly, без дополнительного ограничения одним graph scope.

Terminal selectors SELECT вычисляются по обычным projected rows после
WHERE/JOIN/SPLIT/GROUP/OMIT/FETCH/ORDER/window; имена — aliases projection.
SELECT ONLY проверяет cardinality выбранного row window до assembly. Scalar
subquery с terminal RETURN GRAPH возвращает один owned graph value. Qualified
occurrence values сохраняют свои graph scopes; global edge без occurrence
не получает выдуманную topology.

Начальная и каждая последующая revision используют GraphAssemblyAccumulator,
endpoint/containment closure и общие family/topology validators. Dependency
tracking включает closure-required atoms, topology, membership и properties.
Topology-only change учитывается даже при неизменной строковой projection.
Evaluation читает один candidate root до WAL; dependencies перепроверяются.

### LG-03. History, adapter и ACK

History содержит отдельные owned logical graph images/deltas: qualified
identities, properties, topology, family/validators/root. Value::Graph остаётся
runtime-only; общий StorageValue codec не расширяется. Начальный checkpoint и
whole-commit deltas допускаются и сохраняются существующей LIVE publication.
Retention сохраняет checkpoint для первого доступного replay interval;
недоступная coverage выражается GAP, не чтением актуальной primary data.

Первая реализация использует общий graph assembly и canonical diff результата.
Logical image содержит root/source/derived scope identities, AtomRef, ordered
qualified endpoint/owner routes, effective properties с owned names, отдельные
validators каждого scope и source ClassId/snapshot Versionstamp для write-back.
Computed scope определяется canonical owner route; runtime UUID используется
только для deduplication внутри одного capture. Локальные AtomId не сохраняются.
Canonical diff сравнивает native value representation через общий same_value.

Принятые graph checkpoints принадлежат transactional root, независимо от
CHANGEFEED policy. Каталог хранит компактную ссылку на отдельный whole-group
файл общего LIVE transport; замена checkpoint публикуется тем же WAL commit.
Owned checkpoints всех active/paused/closed incarnations ограничены 256 MiB
на datastore; publication и recovery используют одну проверяемую оценку.
Один decoder имеет отдельный scratch budget. Source mapping сохраняет ClassId
и durable Versionstamp принятого снимка. Само изменение watermark не считается
изменением graph content; flush сравнивает effective image и owner routes
внутри explicit transaction, затем применяет обычную FCW/SSI validation.

Computed envelope использует `FGLB v2`: kind 0 — rows, 1 — GAP, 2 — fault,
3 — graph data. Graph data содержит binding revisions, base watermark,
presence предыдущего image и length-prefixed FGLG before/after. Canonical targets
восстанавливаются общим diff, header count сверяется с целым delta. Даже пустой
graph delta имеет одну data entry для admission; 64-KiB control cap применяется
только к GAP/fault. Compatibility decoder FGLB v1 не добавляется по Q-40.

Checkpoint side-format `FGLG v1` имеет 32-byte little-endian header: marker/version/
reserved (8), dictionary length (8), body length (8), RapidHash v3 checksum (8).
Checksum охватывает весь envelope с обнулённым собственным полем. Dictionary
самодостаточна; decode не читает current primary. Counts, tags, presence flags,
scope/atom ordering, property handles, closure и containment проверяются до
restoration. Family validators применяются при построении candidate. Qualified
recipe literals/parameters используют `FGLV v1` внутри `FGLD v2`; scalar canonical
bytes и общий StorageValue codec сохраняют свой контракт. Executable IR не кодируется.
Datastore::subscribe_live_graph и LiveGraphSubscription<State, G> предоставляют
polling, cursor, close, materialization и явный переход в Mutable. Subscription
incarnation сохраняет result identity; watermark использует Versionstamp.
ACK следует за принятием целого delta. Ошибка ACK сохраняет applied/unconfirmed
token; повтор delivery не применяет delta повторно.

### LG-04. Mutable conflict protocol

Disjoint delta применяется автоматически. Dirty intersection приостанавливает
application и ACK; conflict содержит incoming/local changes, qualified targets
и remote revision. Waiting использует bounded LIVE queues.
Если два отдельно valid disjoint changes вместе нарушают family/topology/
containment, SDK также выдаёт typed Conflict с diagnostic. Resource exhaustion
сохраняет обычный error path и не превращается в решение о конфликте.

Rust SDK предоставляет branch-based ожидание по образцу `tokio::select!`:
отдельно выбираются обновление, конфликт, GAP/fault и завершение, с возможностью
совместного ожидания внешней отмены или другой asynchronous operation. Conflict
является отдельным typed событием; его ветка получает incoming/local changes,
qualified conflicts и remote revision и возвращает управление пользователю.
Выбор `accept_remote`/`keep_local`/`discard_local`/`rebase` остаётся явным.
Ожидание cancellation-safe: отмена или проигравшая ветка не теряет pending
conflict/delta, не подтверждает его ACK и не выбирает resolution автоматически.
Несколько веток одного subscription используют одного consumer, без competing
reads и без дублирующего применения delta. Новый runtime и зависимости не нужны.

Конкретная SDK surface — `LiveGraphSubscription::next_event()` и
`live_graph_select!`. Macro передаёт один event future выбранному caller selector;
его `Conflict(Arc<LiveGraphConflict>)` branch получает frozen conflict boundary.
Пример использования (обработчики принадлежат приложению):

```rust,ignore
ferros_grynn::live_graph_select! {
    select = tokio::select;
    event = subscription.next_event() => {
        Ok(LiveGraphNext::Conflict(conflict)) => show_conflict(conflict),
        Ok(LiveGraphNext::Updated { revision }) => render(revision),
        Ok(other) => handle_lifecycle(other),
        Err(error) => report(error),
    },
    command = commands.recv() => handle_command(command),
    _ = cancellation.cancelled() => stop_waiting(),
}
```

Conflict notification выдаётся один раз; `conflict()` сохраняет сведения до
resolution. Следующее ожидание допускает внешние commands/cancellation и не
создаёт busy loop на удерживаемом delta. `keep_local` захватывает актуальные local
decisions, включая edits после получения notification. Interned property keys
сохраняют bounded name registry между revisions. Разные owned views одного
logical child scope обязаны иметь согласованное содержимое; расхождение
отклоняется до application/ACK вместо потери одного из локальных вариантов.

Accept_remote/discard_local принимают remote state и очищают local changes;
keep_local сохраняет local decisions поверх нового remote base; rebase строит
явный merged candidate. Каждый resolution проверяет family/topology/containment.
Failure сохраняет прежнее состояние и конфликт; resume его не обходит.
Flush использует explicit transaction, qualified mapping, FCW/SSI и общий
validator. Неоднозначный write-back mapping отклоняется до записи.
SDK `flush(owned Transaction)` возвращает успешный `CommitResult` только после
source barrier. Exact graph checkpoint берётся из того же commit candidate;
его материализация и ACK выполняются ordered polling. Ошибка после source commit
в `next()` не означает rollback уже подтверждённого `CommitResult`. Другой
datastore, unmapped/new atoms, derived scopes и stale effective images отвергаются
до source mutation. При всех isolation modes accepted dependencies являются FCW
requirements; ReadCommitted preflight/staging использует один statement root.
Если post-flush edits образуют невалидную topology вместе с committed checkpoint,
polling выдаёт тот же typed `Conflict` с diagnostic и сохраняет last agreed
revision. Resolution не откатывает уже опубликованные source effects. Принятие
более раннего delivery очищает старые local decisions; последующий checkpoint
не трактует отброшенные значения как новые edits. Явный rebase сохраняет только
решения, внесённые callback поверх incoming image.
Post-flush local edits сохраняются отдельно от ранее queued remote changes.
Container bindings устанавливаются до final qualified connector anchors; equal
physical topology сохраняет explicit logical reselection после child rebinding.
Исчезновение child scope из result closure не удаляет child source graph/atoms.
`resnapshot()` явно закрывает старый capture и возвращает новую frozen
incarnation с текущим результатом. Old materializations/read guards остаются
доступны; caller выбирает дальнейший переход новой подписки в Mutable.
GAP/fault сохраняет последнюю revision; resnapshot всегда явный.

## 4. Dependency-ordered implementation и evidence

| Срез    | Зависит от  | Результат                                      | Gate                                           |
|---------|-------------|------------------------------------------------|------------------------------------------------|
| E0      | 12/20/27/28 | Нормативный contract, baseline и fit arguments | Contract/source audit                          |
| E1      | E0          | Общий operation owner без изменения поведения  | Journal/savepoint/import characterization      |
| E2      | E1          | DDL, SDK, schema/descriptor persistence        | Parser/analysis/SDK/reopen                     |
| E3      | E2          | Sync dispatch, context, rollback, FCW/SSI      | RAM/local, no-op/fanout/recursion              |
| E4      | E3          | Durable outbox и общий worker                  | Crash/recovery/retry/cancellation/resources    |
| E5      | E4          | Import ON/OFF и AtomWalk generation            | Item/END rollback и incidence parity           |
| G0      | E5          | Family runtime и atomic resolution             | All-family/failure tests                       |
| G1      | G0          | Ordinary/LIVE RETURN GRAPH                     | Shared assembly oracle и dependencies          |
| G2      | G1          | Graph history, SDK adapter, ACK/resume         | Recovery/retention/duplicate/ACK failures      |
| G3      | G2          | Qualified mutable resolution/write-back        | Conflict/validator/FCW/SSI oracle              |
| Closure | E5/G3       | Full correctness/resource/performance/tooling  | Evidence report и отсутствие открытых gates 29 |

RAM и local integration targets разделены; local проверяет Sync/NativeAsync.
Graph oracle после каждого commit — ordinary SELECT + RETURN GRAPH на той же
revision. Проверяются все families/validators, multi-source/JOIN/SPLIT, closure,
topology-only changes, registration races, source retirement и old guards.
Codecs имеют golden vectors, malformed/unknown version/checksum и crash tests.

Paired performance baseline — текущий checkout. Disabled Events/no LiveGraph
не регрессируют ordinary query/DML/import более чем на 5%; отдельно измеряются
handler fanout, worker lag, graph assembly/delta и retained memory.
Focused checks предшествуют full lib/backend correctness, scoped nightly fmt
и Clippy. Перед commit обязателен just all-check с защитой unrelated dirty work.
Global lint debt и долги 27–28 отделяются от change-specific evidence.

Обновляются specs, conformance matrix, module guidance, full_plan и русский
reports/gql_events_livegraph_stage_29_report.md. Graphify update выполняется
после code changes. Bridges, Linux, новый runtime/dependencies и дальнейшие
оптимизации baseline этапа 28 не входят в scope.

При нехватке места на диске для build artifact исполнитель сохраняет точную
команду, состояние изменений и точку продолжения в evidence report и прекращает
работу по прямому указанию пользователя; артефакты и чужие изменения не удаляются.
