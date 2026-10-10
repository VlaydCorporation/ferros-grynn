# LIVE runtime guidance

Публичный `live` владеет logical builders, checked identities/options/cursors
и owned result/metadata types. Registry, automatic evaluation, publication,
queues и ACK принадлежат datastore; physical codecs принадлежат storage.

- Builders работают без GQL-текста; lowering использует обычный analyzer/typed IR.
- LiveJoin/LiveJoinKind и with_joins/joins удерживают ordered source recipes,
  checked aliases и ON shape. Result participants остаются в declaration order.
  Source/ON/nested expressions входят в общий DAG и record incarnation walkers.
  Descriptor readiness сама по себе не разрешает пропуск execution validation.
  Graph-wide RHS исполняется одной raw relation; class typing variants не
  являются отдельными JOIN outputs. LEFT Unmatched типизируется как NONE;
  никакой подстановки class0 overload. GROUP/window и multiplicity общие.
- LiveQuery with_alias/alias удерживают explicit source qualifier. Qualified
  Field path имеет отдельные segments alias/field; literal dots не разделяются.
  Alias не входит в result identity и не заменяет unique synthetic capture owner.
  Nested одинаковые aliases сохраняют независимые frozen parameter frames.
- GROUP BY/ALL имеют optional ordered logical keys; Aggregate identity отделена
  от projection. Key children входят в captures, DAG и bounded codec traversal.
- ONLY хранит private boolean, default false, с with_only/only SDK. Shared SELECT
  проверяет cardinality после общего window/payload evaluation; empty result не
  создаёт NONE identity. Graph union и nested scalar shape общие. Исторический
  proof требует complete domain, changed-row replay для ONLY запрещён.
- ORDER имеет owned Name/Expression keys и собственные direction/nulls enums;
  LIMIT/START Offset хранят logical expressions. Names сохраняют ordinary alias
  precedence; expression fields source-qualified. ORDER выбирает membership,
  net delivery остаётся в canonical identity order без rank-only events.
  Все clause children входят в captures/DAG/history proof; window history требует
  complete domain, changed-row replay недопустим. Bounds проверяются точно до pull.
- OMIT хранит ordered checked literal paths и исполняется общим SELECT после
  projection, до FETCH. Borrowed accessor не копирует paths; удаление payload id
  или group key не меняет event identity. Missing/non-object paths — обычный no-op.
- Array/Set/Object и numeric unary recipes используют те же typed semantics;
  ordered object keys сохраняются до ordinary last-value construction. Durable
  tags и count/depth admission принадлежат storage, а не Rust discriminants.
- Set хранит ordered inputs с duplicates; capture/dependency/history walkers
  обходят каждый input до ordinary evaluator deduplication.
- Access/Index сохраняют ordinary field/index/null-safe semantics; record-valued
  bases читаются через snapshot dependency recorder, а не из primary при resume.
- Conditional использует ordinary truthiness/short-circuit. Condition/then/else
  проходят capture, dependency и bounded codec validation целиком; inactive branch
  не обходит PURE/runtime-capture contract. Historical row-only proof требует все
  три children, не только выбранную ветвь.
- Cast хранит symbolic target Text и value child; ordinary type-name analyzer
  и evaluator сохраняют conversion semantics/errors. Общий admission проверяет
  bytes/work до UTF-8/scan/copy и generic nesting <=64 до recursive type parsing.
- Range хранит optional start/end и inclusive_end; обычный runtime kernel
  проверяет bounds без enumeration. Capture/DAG/history consumers обходят оба
  present endpoints; bounded evaluator допускает shells и invalid diagnostics.
- Case сохраняет optional single-evaluation operand, ordered непустые branches
  и optional ELSE; captures/dependencies/history proof проверяют все children,
  evaluation остаётся ordinary first-match/short-circuit.
- Ordinary parameters frozen; reactive parameters связаны по LiveBindingId.
- Scalar binary recipes включают ordinary arithmetic/quantified/spatial operators;
  Pipeline относится к traversal IR. Truthy — composition Not(Not(value)), без
  нового durable tag; synthetic depth/work учитывается до allocation.
- Cursor обозначает confirmed whole-commit position; issued delivery token отделён.
- LiveInfo открывает owned snapshot/start/publication bounds, frozen source owners,
  PURE versions и authoritative history/capture epochs. Absent history — None;
  last_stamp/purge floor не являются first available group. Physical refs скрыты.
  Metadata vectors/names и epoch capacity допускаются до copies; SDK/GQL используют
  общую field projection. GQL operand и INFO читают supplied statement root.
- CallWithOptions сохраняет symbolic type/DISTINCT/optional names и ordered values;
  ordinary analyzer владеет signature/effect/version mapping. Existing Call/tag 6
  неизменён; modifier tag 19 имеет checked flags. Все tuple values проходят
  captures/dependencies; historical complete-domain proof ограничен count/sum.
- Value/RecordId/AtomOccurrenceRef не заменяются storage DTO или physical IDs.
- GraphRecord выбирает один qualified occurrence с effective properties и
  negative membership reads. ClassId record owner frozen; graph capture policy
  независима от CLASS feed. Scoped SELECT/FETCH и saved-result resume общие.
- GraphExpr выбирает весь occurrence domain выбранного графа, без containment
  expansion. Class namespace читается в snapshot; scope разрешается один раз для
  root и nested union. Nested query parameters удерживают свой capture frame.
- Runtime-only captures отклоняются до регистрации. Executable IR не сериализуется.
- Whole commit не режется; потеря истории/fault всегда явные.
- `presentation/` владеет bounded native DIFF/PATCH и явным JSON export.
  Event::output показывает подготовленный immutable mode; before/after всегда
  точны. Field paths хранят literal segments, PATCH — escaped JSON Pointer.
  Arrays атомарно replace; native values не приводятся к JSON неявно.
  Snapshot publication и saved-result replay вызывают один renderer до issue;
  prepared outputs не сериализуются в physical history и входят в queue reserve.
- Result identities принадлежат private result_identity module с прежним public
  LiveIdentity re-export. Composite — flat LiveIdentityPart sequence: primary
  source, JOIN participants/explicit Unmatched в declaration order, затем checked
  u64 SPLIT positions. Graph display names не влияют на Ord/Hash. Shape validation
  общая с storage; не использовать scan/output ordinals или CBO order как identity.
- Ожидание не удерживает transaction snapshot; Drop отсоединяет consumer.
- Terminal/fused polling не означает остановку capture. close выполняет durable
  control commit и становится идемпотентным только после успешного stop; failed
  commit сохраняет cursor/state. DISCONNECT consumer тоже допускает явный close.
- LiveInfo.bindings — revisions прямых dependencies текущего snapshot;
  LiveBatch.bindings — revisions, использованные для конкретной evaluation.
- LiveStatistics — payload-free process counters/current consumer gauges;
  reset-on-open, не durable authority. Admitted work/reserve не означают RSS
  или известный partial work failed kernels. First issue/ACK исключают repeats;
  actual logical history bytes включают newly published SINCE backfill и controls.
- Normative contract: `docs/gql_live_stage_28_plan.md`; stage OPEN до всех gates.
