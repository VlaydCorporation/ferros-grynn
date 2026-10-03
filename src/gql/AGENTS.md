# GQL module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/gql`.

## Архитектурная цепочка

`gql` сохраняет один pipeline:

```text
src/syn parse -> analysis -> logical IR -> canonical rules -> physical optimizer
              -> execution -> datastore adapter
```

- `analysis.rs` владеет name/type resolution и semantic errors;
- `ir.rs` — typed logical plan, не storage plan;
- `optimizer/` — единственный crate-private rule/CBO optimizer, physical plans,
  fixed-point estimates/costs, deterministic plan encoding и EXPLAIN projection;
- `execution/adapter.rs` — единственная граница к transaction snapshot;
- `execution/cursor.rs` — pull-based relational cursors;
- `execution/eval.rs` — вычисление typed expressions;
- `execution/mod.rs` — orchestration и statement results;
- `mod.rs` — публичный facade и re-exports syntax types.

## Инварианты

- Не обходить analysis прямым исполнением AST.
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
- Все рассмотренные topology branches считаются в `max_path_expansions`;
  frontier/history/trace учитываются в statement barrier budget. При исчерпании
  бюджета path execution fail-closed и не выдаёт partial path.
- Mode 3 materialization возвращает owned effective graph; mutable write-back
  выполняется только явной delta flush operation.

## Спецификации и проверка

Поведение задают `docs/gql_spec.md` и утверждённые части
`docs/gql_tech_spec.md`; при конфликте не угадывать. Минимальная проверка:
parser/semantic/execution tests одной конструкции, RAM/local parity при
storage access, затем `cargo check --lib --no-default-features` и rustfmt.
