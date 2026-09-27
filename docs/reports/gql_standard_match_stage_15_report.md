# Отчёт этапа 15: полный StandardPath `MATCH`

**Дата:** 2026-09-27

**Статус:** M0–M8 завершены; StandardPath исполняется через общий
`parse → analyze → logical IR → optimizer → pull executor` pipeline.

## 1. Итог

Этап добавил полный исполнимый профиль StandardPath без второго MATCH executor и без
изменения durable storage formats:

- обязательный и `OPTIONAL MATCH` с общей семантикой aliases;
- connected и disconnected patterns, повтор alias как equality join;
- `REPEATABLE ELEMENTS`, `DIFFERENT EDGES` и `DIFFERENT ATOMS`;
- inline class/record/label/property/`WHERE` constraints;
- четыре направления incidence и topology-kind filtering;
- bounded/open variable ranges, нормализуемые до `FG_PATH_MAX_DEPTH = 30`;
- `WALK`, `TRAIL`, `ACYCLIC`, `SIMPLE`;
- канонический BFS `SHORTEST` и Dijkstra `WEIGHTED SHORTEST`;
- именованные пути и `path::{atoms,nodes,edges,length,cost}`;
- binding- и projection-level `DISTINCT`, общий ORDER/spill;
- physical MATCH plan, `EXPLAIN [FULL|ANALYZE]`, budgets, timeout и cancellation.

Legacy-формы `MATCH OPTIONAL`, graph source через `FROM` и colon-alias не сохранены.

## 2. Реализация

### 2.1. Occurrence и path contract

`AtomOccurrenceRef` разделяет глобальную identity атома и его graph occurrence. Equality,
ordering и hash используют `{GraphId, AtomRef}`. Runtime получил `Value::AtomOccurrence` и
`Type::AtomOccurrence`; durable codecs отклоняют этот runtime-only type. Transient query-run
codec кодирует occurrence/path явно и сохраняет scope при spill.

`PathStep` хранит endpoint occurrence и optional connector occurrence. Поэтому path identity,
canonical tie-break и `path::*` больше не зависят от materialization-local `AtomId`.

### 2.2. Parser, analysis и IR

MATCH parser разделён на bounded submodules для bindings, constraints, ranges и projections.
Typed analysis выполняет alias unification, проверяет node/edge domain, порядок inline
references, локальное переопределение global path axes и конечность range до execution.

Logical IR различает seed, fixed expand и path expand. Physical plan фиксирует алгоритм и
содержит `MatchSeed`, `MatchJoin`, `FixedExpand`, `PathEnumerate`,
`BreadthFirstShortest`, `DijkstraShortest`, optional/distinct/project/order operators.
Executor алгоритм повторно не выбирает.

### 2.3. Enumeration и shortest

Bounded DFS учитывает mode-history, expansion budget и общий statement barrier account.
Trace создаётся лениво только для наблюдаемого path. Shortest не перечисляет все paths:
BFS/Dijkstra поддерживают канонический predecessor/tie-break, depth и необходимую history для
`TRAIL`/`ACYCLIC`/`SIMPLE`. Weight использует существующую exact numeric matrix;
отсутствующее значение равно `1`, negative/non-finite weight возвращает typed error.

## 3. Correctness evidence

| Gate | Результат |
|---|---:|
| Parser suite | 15 passed |
| GQL execution suite | 96 passed, 0 failed, 4 ignored |
| Full library, `--no-default-features` | 443 passed, 0 failed, 6 ignored |
| Full library, `native-async-io` | 445 passed, 0 failed, 7 ignored |
| Local reopen, sync | passed |
| Local reopen, native async | passed |

Focused tests покрывают syntax rejection, selectors, directions, topology, ranges, all match/path
modes, OPTIONAL, два DISTINCT, repeated aliases, disconnected products, inline predicates,
weighted errors, SSI membership phantoms, budgets и EXPLAIN metrics.

`generated_small_graphs_match_the_path_mode_and_shortest_oracle` сравнивает executor с
независимым exhaustive oracle на восьми генерируемых малых графах, для всех source/target пар,
четырёх path modes и shortest. Расхождений нет.

## 4. Performance и профилирование

Paired profiling runs выполнены одним probe на одинаковом Stage-14 checkout и текущем коде,
15 повторов на workload. Артефакт текущих результатов:
`target/gql-stage15-16/match_probe_after_projection_fix.csv`; нормализованное сравнение —
`target/gql-stage15-16/match_performance_comparison.csv`.

| Workload | Stage 14 median, ns | Stage 15 median, ns | Изменение | Gate |
|---|---:|---:|---:|---:|
| class seed | 5 819 600 | 4 617 100 | −20,7% | PASS |
| label seed | 3 961 800 | 2 852 800 | −28,0% | PASS |
| one-hop | 13 271 200 | 12 431 300 | −6,3% | PASS |
| bounded TRAIL | 15 382 100 | 13 926 000 | −9,5% | PASS |

Первый measurement показал +29,8% на class seed. Проверка call path и VTune выявили два
не семантических расхода:

1. `SeedCursor` повторно проверял kind/class/membership/label после того, как datastore adapter
   уже гарантировал этот contract. Полная проверка оставлена только для prebound и meta bindings.
2. `MatchProjectCursor` всегда клонировал binding row и occurrence values ради hidden ordering,
   даже когда запрос не имел ORDER и не был deterministic. Для обычной projection восстановлен
   прямой fast path.

Профилирование собрано только `--profile profiling`; Rust symbols/source lines разрешены по
точным парам:

- current: `target/profiling/deps/match_performance_probe-adde8fcfabccb129.{exe,pdb}`;
- Stage 14: `target/gql-stage15-16/stage14-target/profiling/deps/match_performance_probe-51cb3a6a26255d04.{exe,pdb}`.

VTune captures и exports сохранены в `target/gql-stage15-16/vtune-seed-fix*` и
`target/gql-stage15-16/vtune-stage14*`. После root-cause fixes все обязательные Stage-15
regression gates имеют запас.

## 5. Resource contract

`max_path_expansions` по умолчанию равен `1_000_000` и fail-closed через generated status.
Frontier, mode history и trace учитываются общим statement budget; они не spill-ятся. DISTINCT и
ORDER используют существующий Stage-13 spill. Deadline/cancellation проверяются до source pull,
на frontier pop и периодически внутри branch loop. Частичный path после ошибки не публикуется.

## 6. Ограничения

- `COST fn`, PATH-index и специализированные reachability indexes остаются этапам 21/25.
- Traversal VM, `FAST`, `KEEP LEVELPATH` и map/reduce принадлежат этапу 17.
- `RETURN GRAPH` принадлежит этапу 18.
- Clippy с `--cap-lints warn` компилирует весь library target; строгий запуск остаётся
  заблокирован существующим workspace-wide lint debt (4675 diagnostics до MATCH closure).
  Этот долг не маскируется успешными MATCH tests/checks и не исправлялся несвязанным diff.
