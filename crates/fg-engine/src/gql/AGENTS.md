# GQL module guidance

Этот файл дополняет корневой `AGENTS.md` для `crates/fg-engine/src/gql`.

## Архитектурная цепочка

`gql` сохраняет один pipeline:

```text
src/syn parse -> analysis -> logical IR -> canonical rules -> physical optimizer
              -> execution -> datastore adapter
```

- Lexical BindingTypes default имеет lazy empty representation; declaration
  maps и rich types сохраняют immutable sharing и pre-copy admission.
  Ordinary shadowing маскирует reactive provenance, empty export не теряет
  outer declarations. Control.started сохраняет original named TIMEOUT origin;
  readonly commit без mutations/intents не требует LIVE timing origin.
- `analysis/mod.rs` владеет name/type resolution и semantic errors;
  `analysis/live.rs` — checked syntax-to-owned LIVE recipes и private bounded preflight;
- `ir.rs` — typed logical plan, не storage plan;
- `optimizer/` — единственный crate-private rule/CBO optimizer, physical plans,
  fixed-point estimates/costs, deterministic plan encoding и EXPLAIN projection;
- `execution/adapter/mod.rs` — единственная граница к transaction snapshot;
- `execution/cursor/mod.rs` — pull-based relational cursors;
- `execution/eval.rs` — вычисление typed expressions;
- `execution/mod.rs` — orchestration и statement results;
- `analysis/event.rs` — EVENT DDL и source-preserving effect validation;
  `execution/event.rs` и `execution/adapter/event.rs` используют обычные closure,
  expression, control-flow и DML kernels с текущим transaction reader. Schema
  definitions, dispatch/outbox/import lifecycle и durable payloads остаются у
  schema/datastore/storage. Executable handler IR не сериализуется.
  `ExecutionControl` владеет общим `RuntimeBudget`: вложенные handlers и UDF
  делят loop gas и depth; только независимый top-level statement сбрасывает gas.
  Пользовательский код не запускается в commit replay или под commit mutex.
- `execution/schema/live.rs` — регистрация frozen runtime captures и bounded metadata.
  LIVE LET использует тот же SDK registry: reactive slot хранит identity, value
  разрешается через snapshot/SSI с admission до clone. BindingTypes удерживает
  reactive flavor отдельно от Value; closure захватывает identity, ordinary
  shadow маскирует flavor. SINCE/BUFFER/ACK inputs используют общий RootRead.
  Root ordinary `IN GRAPH $scope` имеет private registration expression: bounded
  command RootRead разрешает GraphRef/String и наблюдает catalog dependencies,
  затем существующий GraphClass/GraphRecord reifies до SDK validation/publication.
  Typed scope не сериализуется. Tracked scope не замораживается как ordinary capture;
  Registration IR хранит frozen scopes отдельно по declaration ordinal, не в
  shared Option: primary0, JOIN1..N. Analyzer допускает exact capture capacity
  до allocation; deterministic visitor проходит все retained expressions.
  Executor проверяет ordered distinct source captures и declaration bounds до
  scope evaluation. Каждый RHS имеет собственный frozen scope; computed String
  RHS удерживает отдельный last-success GraphId/name anchor по ordinal.
  class/record computed scopes используют GraphClassExpr/GraphRecordExpr recipes.
  Root scope разрешается один раз до row pull через тот же RootRead. Успешные
  String operand и GraphId публикуются вместе; неизменное имя сохраняет identity
  при rename/recreate, fault оставляет последнюю успешную пару. Nested graph
  expressions сохраняют ordinary snapshot name resolution как read dependencies.
  SDK GraphExpr использует class union qualified occurrences, с одним scope
  resolution на invocation. Shared nested scope хранится в Empty wrapper и
  нормализует pre-admitted input copies; frozen nested parameters не берутся из
  внешнего capture frame. Aggregate inputs сохраняют class-specific overloads.
- Grouped LIVE использует обычный SELECT grouping/sort/reduction. Graph class
  union объединяет raw rows, CASE сохраняет class-specific key/argument plans;
  projected key subtrees используют обычное group_expr_equivalent. Key identity
  не зависит от output alias. Group clauses входят в frozen/reactive captures.
  Aggregate outer constructors сохраняют kind/length/names/optional flags/types;
  каждый child объединяется borrowed, per-class overload остаётся в row CASE.
  Общий admit_special_copy допускает CASE buffer и Cast target до allocation/clone
  в merge и materialization. Lazy IF/CASE исполняется только ordinary evaluator.
  Shared transaction-aware Field evaluation после materialization base сохраняет
  existing RecordFields, включая ordinary closure/scalar traversal RecordId lookup.
  Snapshot и SSI относятся и к binding value, и к прочитанной base property;
  bounded RootRead delegation и qualified occurrence resolver сохраняются.
  Private record-field traits передают optional caller control. Transaction
  adapter допускает selected properties/id/label до clone в reactive pool;
  bounded RootRead использует собственный account. Borrowed point_parts отделён
  от complete row construction; scalar field projection не копирует unrelated
  properties. Shared field_from_atom сохраняет effective/Hidden/base semantics.
  INFO удерживает vector account до завершения projection, GQL REMOVE передаёт
  caller budgets/checks. LIMIT/offset и MATCH record keys используют snapshot fields;
  Fulltext/KNN проверяют dynamic query types до rows. Query analyzer/terms и PURE
  analyzer argument используют bounded scratch/work; per-document search policy
  отдельна. INFO/SHOW CHANGES scalar operands используют тот же QuerySnapshot/SSI;
  image walker сохраняет depth checks и per-step caller work/cancellation,
  retained account переживает lookup borrow. Остальные input seams L9b требуют
  отдельного evidence.
  START AFTER выбирает session cursor через statement fields/control; direct
  supplied Slot/Constant spool освобождается даже при early eval cancellation,
  computed expression не выполняется повторно ради cleanup.
  Top-level timeout preparation errors и SELECT evaluation errors используют
  общий borrowed direct-input retirement helper до возврата ошибки.
  EXPLAIN wrapper получает нормализованный statement timeout без нового IR.
  Common direct Select/Explain->Select retirement dispatcher используется
  top/block preparation и statement-entry control/depth guards; computed
  continuation expression не выполняется повторно для cleanup.
  Import item timeout metadata типизируется на одном preflight Analyzer/root
  с inherited BindingTypes. Shared borrowed duration lookup фиксирует значения
  до expected-stamp lease; nested locals разрешаются при invocation. Sparse
  typed/frozen metadata допускается до allocation общим retained account.
  Item checked child сохраняет platform overflow error; END publication
  удерживает исходный caller options deadline.
  MATCH MAXDEPTH сохраняет typed lexical operand и open-range provenance;
  snapshot-bound validation выполняется до planner/row pull. Explicit max независим;
  CBO/cursor/direct EXPLAIN используют один bound. Outer embedded estimates до
  invocation используют checked upper bound с warning; GraphResult EXPLAIN
  recursively сохраняет source warnings. MATCH GRAPH также удерживает typed operand;
  snapshot GraphRef-only validation предшествует demand/CBO/open. Build/return/traverse
  consumers сохраняют qualified domain. Outer estimates не фабрикуют GraphId;
  deferred tensor scope/format — DYNAMIC. Physical encoding v8 включает resolved
  GraphId, direct EXPLAIN показывает resolved/deferred binding.

  Direct INFO/KILL используют catalog-only snapshot; LIVE close проходит узкий
  control preflight/savepoint без подготовки data writer, включая import lease.
  Session допускает consumer/handle capacity до WAL и exposes handle после commit.
  ACK требует retained session consumer и ранее выданную whole-commit delivery;
  fixed cursor cache меняется после commit. Runtime closure effects сохраняют
  declared PURE/STABLE ceiling и explicit READ ONLY.
- `root_catalog.rs`, `execution/root_read.rs` — read-only adapters к одному
  immutable candidate root для PURE/LIVE; обычная transaction внутри commit evaluator
  не открывается. SELECT/FETCH используют те же kernels и порядок ошибок.
- LIVE dependency recorder различает schema-only range observation и actual
  global CLASS scan. Только успешный root scan разрешает coalescing same-class
  BaseAtom keys; marker private и входит в admission существующего key. Point
  и graph-scoped reads без global scan сохраняют точные BaseAtom dependencies;
  membership/overlay/incidence, schema/functions/bindings не coalesce-ятся.
  RootRead default constructor не включает scan callback у иных consumers.
- `live_lowering/` — Rust logical recipes → обычный analyzer, owned result identities
  и bounded dependency recorder. Graph-wide агрегат объединяет class inputs до
  reduction, сохраняя per-class overloads. SDK nested SELECT независим от outer row;
  textual correlated SELECT сохраняет прежнюю семантику.
  Polymorphic JOIN использует private JoinedTypingVariants/AtomUnion и bounded
  SELECT family collapse до reduction. WildcardVariants выбирает прежний
  projector; ON видит только declaration prefix. NONE state и source tags не
  изменяют публичные fields/identity. Root и nested unions используют те же
  GROUP/window kernels; scratch/work проверяются до copies/allocations.
  Primary source alias хранится отдельно от synthetic lexical capture owner.
  Qualified GQL Field сохраняет source prefix в recipe; borrowed
  source_field_segments — общая authority lowering и row-only history proof.
  SDK record sources используют BoundRecord class/literal slot metadata;
  ordinary analyzer создаёт exact ScanTarget для root/nested/JOIN. Global WHERE
  restriction и primary-only post-analysis rewrite не применяются. Rich-key
  literals допускаются до clone; mutable/reactive/wrong-class slots отклоняются.
  Explicit alias имеет precedence, class-name qualifier применяется только
  при отсутствии одноимённого declared property; foreign navigation требует
  полной coverage и не становится row-local после удаления prefix.
  Owned Access/Index lowering использует ordinary expressions, null-safe flag
  не теряется; source qualification сохраняется отдельно от record navigation.
  Owned Conditional lowering использует обычный IF evaluator; capture/DAG walkers
  обходят обе ветви, historical fast path допускается только при proof всех children.
  Owned Set использует ordinary evaluator; ordered inputs с duplicates проходят
  bounded lowering, captures и dependency/history proof до deduplication.
  Owned Cast использует ordinary type-name analyzer/evaluator; shared admission
  ограничивает bytes/work/type copies и generic nesting до recursion/allocation.
  Owned Case сохраняет optional operand и ordered branches; empty branch list
  отклоняется до ordinary analysis, все children проходят capture/DAG validation.
  Owned Range сохраняет оба optional endpoints и inclusive_end; ordinary kernel
  проверяет domain/order. Bounded evaluator допускает endpoint shells и invalid
  diagnostics до rendering через общий scalar pool с CAST.
  Общий nullable type widening идемпотентен; repeated NONE/NULL в CASE/Array/Set
  не создают новый Option вокруг уже optional inferred type.
  OMIT/FETCH recipes используют общий checked literal-path converter и SELECT
  projection kernel: OMIT до FETCH, identity отдельно от payload. Shared OMIT
  traversal проверяет cancellation/work/depth без нового executor; no-op paths
  не добавляют внешние reads. Исторический proof учитывает work обоих path lists.
  Root SELECT и grouped class union используют общий record-preserving pager:
  LIMIT/START OFFSET разрешаются точно до row pull, anchors сохраняют source/group
  identity. LIMIT 0 проверяет graph scope и фиксирует read dependency; skipped
  records учитывают bounded work/cancellation. SELECT subqueries используют тот
  же value wrapper. START expression допускается до optimizer/union plan clone.
  Row-local class union сохраняет original per-class projection/ORDER keys и
  применяет один global window через QuerySorter; unordered окна останавливают
  projection/FETCH после global limit. Nested maximum применяется после START.
  Grouped ORDER rebases top-level projected alias за padded input/class tag;
  aggregate ORDER использует existing per-class merge_expression. Unchanged
  clauses сохраняют прежний streaming путь. Retained START AFTER остаётся session
  operation; owned LIVE ORDER/LIMIT/START Offset recipes используют тот же kernel.
  ONLY использует общий final cardinality checker после window/projection/FETCH:
  streaming cap2 остаётся после global offset, barriers вычисляются полностью.
  Per-class union pull не применяет ONLY; qualified compact-row anchor берётся
  из logical id при пустом stable с admission до copy. Nested wrapper сохраняет
  ONLY flag и scalar shape, complete-domain history учитывает неизменных competitors.
  LIVE SPLIT recipes/bindings/DAG используют общий WHERE/SPLIT preparation;
  single-source stable anchor содержит original id, clause positions и отдельный
  output ordinal. Последний не входит в Composite identity. Graph union применяет
  один global window; aggregate union готовит WHERE/SPLIT до padding/grouping.
  Expansion проверяет complete row/key growth, work и cancellation до clone;
  historical changed-row replay запрещён, complete-domain proof остаётся обязательным.
  Root JOIN kernel фиксирует logical source offsets/scopes на copied plan;
  source tuple/SPLIT suffix идёт после прежнего unique output ordinal, не меняя
  Row/transient row layout. Ordered union ставит global tie ordinal перед suffix.
  Generic prepared input общий для ordinary finishing и aggregate union.
  Shared combine_rows допускает каждый product до clone, включая CROSS без
  predicates. ON-source discovery использует полный typed visitor; enumeration
  и scheduling разделяют equality-INNER prefix boundary, suffix следует original
  syntactic steps. Root zero-window проверяет все joined scopes на frozen copy.
  Bare Name keys сохраняют projected alias/input precedence, Expression keys —
  source qualification. GQL bind_query и общий DAG walker посещают все operands;
  history с окном требует полного source domain, не только изменённых строк.
  Scalar binary recipes сохраняют ordinary operator semantics и short-circuit;
  syntax Truthy нормализуется в два admitted Not nodes. Общий operand/product
  guard использует bounded RootRead либо reactive scalar pool; quantified loops
  проверяют cancellation, сохраняя eager validation invalid later members.
  CallWithOptions сохраняет symbolic type/DISTINCT/ordered optional names до
  ordinary signature mapping. Shared bounded compiler signature guard допускает
  types/names до ranking/result clone и direct/transitive compilation; existing
  compiler RAII budget переиспользуется. DISTINCT deep copies используют общий
  scalar admission. Historical spill context читает empty reconstructed graph
  catalog; eligibility по-прежнему исключает graph/foreign input operations.
  Static GraphRecord source проходит scoped exact point/FETCH с qualified
  identity и negative membership dependencies; record class owner frozen.
  Graph source policy и saved-result history не наследуют CLASS feed.
  Internal Empty source сохраняет optional typed graph reference при нуле classes.
  Effect/demand/fingerprint/copy visitors не теряют scope. Streaming LIMIT0 и
  materialized COUNT/nested paths валидируют graph через общий helper против
  того же snapshot, без metadata rows и без фиктивного class. Encoding v9.
  SLEEP и nested TIMEOUT удерживают typed lexical duration до invocation;
  borrowed binding lookup сохраняет snapshot/SSI без copy invalid payloads.
  Общий converter проверяет fixed nanoseconds/calendar/negative/overflow;
  checked child не отключает deadline при Instant overflow. PURE block учитывает
  effect timeout operand; single-return closure не теряет timeout metadata.
  Ordinary top-level TIMEOUT lower-ится до declaration/shadowing и разрешается
  до mutating writer inference. Caller-owned control сохраняет construction
  anchor/minimum deadline до autocommit export/admission/publication; clone
  не обновляет clock. Import preflight и EXPLAIN normalization описаны выше.
  Embedded MATCH удерживает private LogicalDuration до общей visitor boundary;
  converter читает переданный invocation snapshot до planner/open. Child control
  живёт до terminal visitor/graph assembly check, tv::match разрешает его для
  каждого seed. Отсутствие clause сохраняет прежний control path.
- `mod.rs` — публичный facade и re-exports syntax types.

## Инварианты

- Не обходить analysis прямым исполнением AST.
- `bindings.rs` хранит private immutable lexical type metadata независимо от
  runtime Value. LET/ALTER LET проверяют dynamic members до slot write в normal
  и PURE executor; annotations сохраняются между calls и в TRY savepoints.
  Session export допускается до consuming commit, transaction locals не
  экспортируются при explicit COMMIT/CANCEL. Это не durable LIVE LET registry.
- LIVE возвращает canonical Record/Occurrence/Aggregate identity отдельно от
  projection. Missing/negative FETCH, schema namespace и concrete PURE versions
  входят в зависимости; compiler/rows/dependencies разделяют work budget.
  Empty graph сохраняет стандартные atom columns; VALUE eligibility вложенного
  SELECT не зависит от числа classes. Копии nested typed plans учитываются до clone.
- IR и executor не зависят от page/WAL DTO; storage доступен только через
  transaction/query adapter.
- Optimizer получает только read-only `PlannerSnapshot`: catalog/index metadata,
  exact counters и востребованные histograms. `DatabaseRoot`, `DataFileId`,
  страницы и physical addresses через эту границу не проходят.
- Executor исполняет выбранные physical operators. Resource fallback допустим
  только внутри заранее выбранного семейства (например, in-memory hash join в
  partitioned hash join), но не как скрытый повторный planner.
- Stage-26 analytics и ALL paths вызывают публичные tensor/analytics kernels через
  один statement snapshot. Branch/terminal eligibility проверяется до minimum;
  bounded PURE fields/COST сохраняют SSI и общий work budget. Перенос captured
  contributions в named Path сохраняет left-to-right Number reduction без повторного COST.
  Barrier accounts покрывают retained buffers и scratch до allocation, включая
  capacity growth overlap и trace phase. Enumeration quota учитывает seed bulk;
  её исчерпание не запрещает обычную аналитику.
- `tv::kHopEstimate` выбирает только compatible Ready Neighbourhood SKETCH в physical planner.
  Adapter проверяет captured binding и выполняет bounded exact NODE/ATOM fallback; обычный
  `tv::kHop` не читает sketch. Metadata/PlanHash содержат logical identity/version/projection,
  не physical pages. Bulk и seed inclusion сохраняются; private invalidation исключает artifact.
- HOP/WEIGHT `LandmarkShortest` получает только logical generation metadata и Number bounds через
  adapter. Exact bounded cursor сохраняет полные path states и canonical ties; улучшенный
  prefix не блокируется visited-set. Unbound target и scoped paths используют exact fallback.
  Ready metadata собирается только по PlannerDemand; binding/version/revision/projection
  входят в PlanHash. WEIGHT completion bound округляется вниз и включает потери runtime
  reduction на оставшихся шагах; terminal rows ждут proof против pending bounds, включая
  zero-weight/canonical ties. Scoped weighted fallback использует тот же completion-bound
  proof и сохраняет qualified target identity. MATCH `COST fn::name` использует
  обычный versioned PURE evaluator: unary additive COST допускает compatible artifact,
  `(edge, prefix:number)` возвращает вклад hop и остаётся ON_DEMAND. Prefix относится к
  текущему segment. Global `IN GRAPH ... WITH COST` задаёт default linear connector segments;
  local COST переопределяет его. Fixed Expand сохраняет edge aliases/uniqueness и вызывает
  общий PURE contribution helper с нулевым segment prefix, включая scoped/hyper/AtomWalk.
  Cost вычисляется и без path projection. Identity/version/stateful flag входят в PlanHash version 6.
- `path::cost(path, fn::name)` использует общий versioned PURE COST resolver и contribution helper.
  Перед property reads проверяются origin первого graph, snapshot и все qualified occurrences.
  Stateful prefix начинается с нуля для всего Path; reduction потоковый и расходует один
  expansion на connector. Standalone/SELECT/traversal closure получают тот же query reader/SSI.
  Recursive static COST использует общую MAXDEPTH cycle validation и current/override resolution.
  Одноаргументный stored cost сохраняет работу без живого snapshot.
- `tv::shortestPath` сохраняет текущий NODE seed-local domain, в том числе внутри TRAVERSE ATOM.
  Compatible Ready NODE/OUT/HOP artifact без labels ускоряет точную длину через reopening A*;
  второй BFS сохраняет прежний FIFO/connector порядок равных путей. Runtime сверяет graph
  scope binding; Stale/private/cross-level metadata ведёт к exact fallback. RecordId source/target
  разрешаются одним membership resolver. Seed bulk сохраняется. Adjacency/frontier/batch
  admission выполняется до роста соответствующих buffers; Path estimator включает steps и
  public identifiers, shared references учитываются консервативно.
- `tv::weightedShortestPath`/`distanceTo` используют Ready NODE/OUT/WEIGHT logical binding.
  Shared Dijkstra сохраняет distance/node queue и первый равный predecessor. Strict ALT completion
  refinement допускается только после all-edge validation и numeric proof, исключающего
  немонотонные casts и masked arithmetic errors; иначе тот же loop работает полностью.
  Adjacency, weights, bound cache, frontier и path reconstruction получают admission.
- TRAVERSE `WITH COST` захватывает тот же versioned PURE key. Navigation сохраняет exact
  traverser prefix без обязательного trace; custom weighted seed search начинает NODE segment
  с prefix 0 и переиспользует full-state MATCH cursor с OUT incidence, включая undirected edges.
  Mode/filters и canonical path ties сохраняются. Unary additive COST допускает compatible
  NODE/OUT/function artifact; stateful всегда exact. Physical hash включает COST/constraints.
  Bulk сравнивает numeric prefix representation, поскольку UDF может наблюдать тип/scale.
- Canonical shortest cursors допускают hop/canonical extension перед allocation/cloning.
  Общий helper использует fallible exact reserve; уже admitted state не начисляется повторно.
  Identity heap estimator заимствует RecordId, не создавая временные boxed Values.
- PURE physical builders читают occurrence/base fields из captured immutable root через
  обычный typed evaluator; nested named calls сохраняют тот же field resolver.
  Maintenance cancellation check наследуется child blocks/calls. Compiler/cache и initial
  runtime frames получают admission до allocation/clone; growing collections/temporaries
  остаются отдельным открытым resource gate.

- Canonical rules не меняют bag semantics, LEFT/CROSS barriers, effective
  properties, порядок наблюдаемых ошибок и floating reduction order.
- Forced index hint является контрактом: каждый указанный source использует
  совместимый Ready index либо получает planning error до чтения строк.
- `DETERMINISTIC` использует canonical bytes и versioned RapidHash V3; Debug,
  `DefaultHasher` и iteration order hash maps не являются частью PlanHash.
- Публичный `QueryResult` содержит `Outcome<Value>` и стабильные
  `RecordId`/`AtomRef`, никогда внутренние IDs.
- Ошибка получает правильную execution phase и source `TextSpan`; внешние
  ошибки редактируются системой `gql_status`.
- Сохранять детерминированный порядок statements, rows, bindings и projection.
- Новая конструкция добавляется вертикально: grammar/token -> AST -> analysis
  -> IR -> execution -> backend tests. Пустые variants «на будущее» запрещены.
- Core DDL проходит этой же цепочкой и изменяет транзакционный catalog через
  datastore adapter. `CLASS` задаёт namespace и immutable atom kind;
  `INFO FOR DB|CLASS|INDEX|PARAM|FUNCTION|JOB(S)` читает transaction-visible
  definitions либо immutable maintenance projection.
- Core DML выполняет общий preflight целей и `EXPECT` по pre-image до первой
  мутации. Direct DML адресует global atom по `RecordId = class:id`, а
  graph-scoped DML читает effective properties.
- `ATTACH`, `DETACH`, class-aware `RELATE ... IN GRAPH`, graph-local `LABEL` и
  `RESET` проходят typed IR. Alias, class и label — независимые понятия;
  выводить class из label запрещено.
- `MutateStatement.label` и соответствующее поле IR намеренно имеют форму
  `Option<Option<_>>`: внешний `None` означает отсутствие clause и сохранение
  прежнего label, `Some(None)` — явный `LABEL NONE`, `Some(Some(_))` — замену.
- MATCH сначала выбирает global exact/class кандидатов, затем пересекает их с
  graph-local label/membership indexes. Старый `(alias:label)` и `FROM graph`
  не являются compatibility grammar.
- MATCH связывает `AtomOccurrenceRef`, а не голый `AtomRef`: каждая column
  сама несёт свой graph occurrence context. Один `row.graph` не может быть
  источником истины для cross-level путей, AtomWalk и `WITHIN`.
- StandardPath, AtomWalk, hyper, nesting и shortest проходят один typed
  MATCH pipeline и один physical-plan family. Не добавлять второй executor
  или скрытый выбор алгоритма в cursor: mode, bounds, objective и filters
  фиксируются physical plan.
- `TRAVERSE` проходит тот же typed pipeline и переиспользует MATCH, graph cache,
  `PathTraceArena`, optimizer и query spill. Traversal VM остаётся concrete
  sync pull executor; второй matcher, универсальный traversal trait и отдельная
  async runtime запрещены.
- `RETURN GRAPH` и `BUILD GRAPH` проходят один concrete
  `GraphAssemblyAccumulator`: source читается pull-потоком без промежуточного
  `Vec<Value>`, edge/containment closure добавляет только необходимые atoms, а
  публичный `MaterializedGraph` не удерживает transaction, snapshot или page
  guard. Обычный `graph::inner` возвращает `GraphRef`; копирование всегда явно.
- Именованный `BUILD GRAPH` передаёт уже проверенный owned graph одной bulk
  mutation через datastore adapter. Executor не создаёт base atoms, storage
  generations или отдельную публикацию на каждый atom.
- Language closure принадлежит создавшей её `GqlSession`, учитывается вместе с
  retained typed plan и captures в общем лимите 256 МиБ и не сериализуется.
  Mutation допустима только в явном `tv::side_effect`; schema mutation во время
  traversal запрещена.
- Named `fn::` UDF всегда разрешается анализатором в точный `(FunctionId,
  SchemaVersion)` overload. Effect проверяется транзитивно; IMPURE call использует общий
  transaction savepoint, recursion — общий control budget и declared `MAXDEPTH`.
  Typed IR на диск не записывается, а builtin/UDF/algorithm registry не объединяются.
- Maintenance statements понижаются в typed IR до обращения к coordinator. Unsupported
  option обязан завершиться до durable enqueue. Stale index не может стать approximate read
  path: planner выбирает exact scan, а forced hint возвращает planning error.
- Unicode `COLLATE`/`NUMERIC` формирует ICU4X sort key внутри общего barrier budget; version и
  configuration входят в ORDER fingerprint. Runtime-only matcher/closure не spill-ятся.
- Bulk является только внутренней оптимизацией: equality включает current,
  trace, sacks и env, а observable path либо impure side effect отключает
  небезопасное слияние. Resource exhaustion fail-closed и не выдаёт partial
  traversal result.
- Подготовка traversal VM fallible: Limit/Skip разрешают count через общий exact
  Number → u64 и отклоняют неверное значение до VM row pull, в том числе при пустом
  source. Source к этому моменту уже может быть вычислен. Silent zero fallback
  запрещён; limit(0) сохраняет ранний обрыв.
- Все рассмотренные topology branches считаются в `max_path_expansions`;
  frontier/history/trace учитываются в statement barrier budget. При исчерпании
  бюджета path execution fail-closed и не выдаёт partial path.
- Mode 3 materialization возвращает owned effective graph; mutable write-back
  выполняется только явной delta flush operation.

## Спецификации и проверка

Поведение задают `docs/gql_spec.md` и утверждённые части
`docs/gql_tech_spec.md`; при конфликте не угадывать. Минимальная проверка:
parser/semantic/execution tests одной конструкции, RAM/local parity при
storage access, затем `cargo check -p fg-engine --lib --no-default-features` и rustfmt.

LIVE EXPLAIN использует один command selector для Schema/LET wrappers и fixed-root
QueryRead metadata. PlanHash v10 не исполняет UDF; current results/queue/ACK bounds
не подменяют logical identity. Registration-only inputs записываются существующим
Analyzer parameter resolver в optional bounded mask; named captures сохраняют
соответствие при reorder/shadowing. Не вводить второй Expr walker или executor.
Transient encoding/hash accounts освобождаются до execution; owned plan/output
допускаются отдельно. INFO остаётся read-only, ANALYZE использует обычный pipeline один раз.

## Root streaming admission

SELECT graph terminal — cold owned payload в AST/IR. Его Box shell допускается
analyzer до allocation и учитывается отдельно при копировании typed SELECT;
уменьшение `size_of::<LogicalSelect>()` не освобождает от pointee accounting.
MATCH terminal, WITHIN owner, LET BUILD GRAPH и RELATE также хранят cold payload
в Box. Analyzer допускает новые оболочки до allocation; MATCH/TRAVERSE/closure
удерживают их суммарный размер для допуска реальных deep copies. Вложенные
analyzers объединяют уже допущенные размеры без повторного начисления. Нулевые
SDK spans не отменяют этот учёт. Общий graph assembler сохранён; typed IR
не является durable representation.

Direct root SELECT Class callback удерживает transient BarrierAccount от cursor.next
до завершения projection/visitor. Retained results до insertion отдельно учитывают
value и qualified identity heap. Materializing Class scan сохраняет такой scope
до окончания RowCollector::push: RowSet/QuerySorter отдельно удерживает rows,
а spill EncodeContext допускается в retained account до construction. Остальные
Nested SELECT, index pages, row unions и GROUP/JOIN escaping values остаются под
прежней conservative policy. Scope восстанавливает enclosing account
при EOF/early return/error/cancellation; не сбрасывать общий retained/read budget
и не делить admission token между infallible deep Row clones. Work limits сохраняются.
