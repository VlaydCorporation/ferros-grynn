# Расширенная таксономия графов

Этот документ фиксирует graph family Ferros-Grynn, их структурные инварианты и допустимые
преобразования. Языковой контракт операторов находится в
[gql_spec.md](gql_spec.md) §3.9, §15–§17; способы хранения и materialization — в
[gql_tech_spec.md](gql_tech_spec.md) §10–§11. Модель identity, membership и graph-local
инстансинга утверждена этапом 8.1 в [rework_plan.md](rework_plan.md).

## 1. Базовая модель

Универсальная модель:

```text
G = ⟨V, MV, E, ME⟩
```

- `V` — обычные вершины;
- `MV` — метавершины с вложенными графами;
- `E` — обычные, multi- и hyper-edges;
- `ME` — meta-edges с внутренними и transition-графами.

Пусть $N = V \cup MV$, $R = E \cup ME$, $A = N \cup R$. Единственная endpoint incidence
задаётся массивами `source(r), target(r) ⊆ A` каждого $r \in R$ и допускает V–V, V–E, E–V
и E–E. Hyperedge — n-арность тех же массивов, а metaedge — ортогональная возможность иметь
внутренний/transition-граф. Универсальный `Graph` может сочетать directed/undirected, multi-,
hyper-, meta- и cross-level topology. Цена универсальности — отсутствие части
специализированных инвариантов и layout-оптимизаций.

### 1.1. Глобальный атом и graph occurrence

Атом является сущностью базы данных, а не собственностью одного графа. Его стабильная публичная
identity — `RecordId = class:id`; `CLASS` фиксирует единственный kind `NODE` или `EDGE` и схему
base properties. Один атом может не входить ни в один граф либо одновременно иметь occurrences
в нескольких графах. Базовые свойства при этом хранятся один раз.

Graph occurrence — независимое graph-local состояние глобального атома:

- membership и локальный `AtomId`;
- необязательная label, не меняющая `CLASS`;
- для edge — endpoint topology, orientation, weight и transition graph;
- sparse property overlay.

Поэтому один глобальный edge может соединять разные endpoint'ы в разных графах. Это две
occurrences одной relation entity, а не две identity. Structural invariant проверяется для
каждого графа по его occurrences; наличие глобального edge само по себе не создаёт incidence.

Эффективные свойства occurrence вычисляются как `overlay value -> value`, `Hidden -> field
absent`, `overlay absent -> base value`. Внутренний `Hidden` не равен пользовательским `none` или
`null`; `RESET` удаляет override и восстанавливает наследование текущего base value. Ссылка на
атом не создаёт membership: в graph scope member читается как effective view, non-member — как
base view.

`GraphRef` является отдельной ссылкой каталога (`GraphId`, имя, family и storage mode) и не
кодируется фиктивным `RecordId`. `GraphId` всегда ненулевой; database-level данные не образуют
«системный граф».

## 2. Graph family

| Family       | Допустимая topology                         | Обязательный инвариант                  |
|--------------|---------------------------------------------|-----------------------------------------|
| `Graph`      | Любые атомы и incidence базовой модели      | Только целостность ссылок               |
| `Metagraph`  | `Graph` с meta-атомами и вложенными графами | Валидная вложенность и cross-level refs |
| `Hypergraph` | Вершины, hyperedges и incidence             | Нет meta-атомов и cross-level incidence |
| `Dag`        | Вершины и обычные directed edges            | Нет self-loop и directed cycles         |
| `Forest`     | Вершины и обычные edges                     | Каждый компонент является деревом       |
| `Tree`       | Один компонент `Forest`                     | Связность и `\|E\| = \|V\| - 1`         |
| `RootedTree` | `Tree` с выбранным корнем                   | Один корень и согласованная orientation |

`Tree`, `RootedTree`, `Forest` и `Dag` относятся к классической vertex-edge topology. Наличие
hyperedge, meta-атома, edge-to-edge endpoint или cross-level incidence делает
прямое сужение в эти family недопустимым.

`SIMPLE`, `BIPARTITE` и `COMPLETE` — проверяемые ограничения topology, а не самостоятельные
runtime family. Они могут применяться к совместимой family без обещания отдельного физического
layout.

`Subgraph` — не ещё одна storage family, а самодостаточный materialized effective result
выбранных occurrences и incidence. Constraint `RETURN GRAPH <T>` валидирует `Subgraph` и строит
точный `T`; materialization копирует необходимые base/overlay properties и не удерживает
membership или snapshot исходной базы.

## 3. Ориентация деревьев

```text
TreeOrientation =
    Undirected
  | OutArborescence
  | InArborescence
```

Unrooted `Tree` не имеет родителя и потомка. `RootedTree` добавляет root; directed-вариант также
фиксирует направление. `Forest` может хранить несколько компонентов и roots.

## 4. Матрица преобразований

### 4.1. Расширяющие read-only views

Эти представления не требуют проверки topology и не обещают смену owned layout:

```text
RootedTree → Tree → Forest → Graph
RootedTree/Tree/Forest directed → DagView → GraphView
Dag → GraphView
Hypergraph → GraphView
Metagraph → GraphView
```

### 4.2. Owned-преобразования

| Из                       | В                 | Условие                                                           | Стоимость                         |
|--------------------------|-------------------|-------------------------------------------------------------------|-----------------------------------|
| `RootedTree`             | `Tree` / `Forest` | Всегда                                                            | `O(1)` при общей forest family    |
| `Tree`                   | `Forest`          | Всегда                                                            | `O(1)` при общей forest family    |
| directed `Tree`/`Forest` | `Dag`             | Всегда                                                            | `O(V+E)`                          |
| undirected `Tree`        | `Dag`             | Явно заданы root и orientation                                    | `O(V+E)`                          |
| любая узкая family       | `Graph`           | Всегда                                                            | `O(V+E)` для owned general layout |
| `Dag`                    | `Tree`            | Один root, indegree остальных = 1, связность, `\|E\| = \|V\| - 1` | validation + rebuild              |
| `Graph`                  | `Dag`             | Только classic directed edges, нет циклов/self-loop               | validation + rebuild              |
| `Graph`/`Forest`         | `Tree`            | Classic topology и tree invariant                                 | validation + rebuild              |
| `Graph`                  | `Hypergraph`      | Только vertex-hyperedge topology                                  | validation + rebuild              |
| `Graph`                  | `Metagraph`       | Валидны meta/cross-level invariants                               | validation + rebuild              |

Все сужающие owned-преобразования являются `TryFrom`-подобными. GQL не выполняет их неявно:
constraint у `RETURN GRAPH`/`BUILD GRAPH` является явным запросом на validation и rebuild.

## 5. Mutable и Frozen

Каждая owned family имеет два состояния:

```text
T<Mutable> --freeze--> T<Frozen>
T<Frozen>  --thaw----> T<Mutable>
```

`Mutable` оптимизирован для изменения и обязан сохранять инвариант family после каждой
подтверждённой операции. `Frozen` оптимизирован для чтения; query results всегда frozen.
`freeze`/`thaw` могут перестраивать adjacency и индексы за `O(V+E)`.

`LiveGraph<Frozen>` публикует последовательность неизменяемых revisions. Явный
`LiveGraph<Mutable>` при пересечении incoming change с local dirty-set приостанавливает
подписку и требует ручного conflict resolution; remote delta не применяется молча.

## 6. Режимы доступа

- Full in-memory — owned `T<Mutable/Frozen>` с COW MVCC; текущий RAM-backend не включает WAL и
  не обещает durability после завершения процесса. Встроенный datastore при этом использует
  нормализованный database root: глобальные атомы отдельно от graph occurrences; standalone SDK
  graph остаётся owning materialization.
- Disk-backed — `GraphRef` и transaction-bound `GraphReadView<'tx>`; это не materialized graph.
- Hybrid — `query_to_memory::<T, State>` строит owned effective family и освобождает source
  snapshot. Mutable flush может изменять только graph-local topology/overlay и никогда не
  превращается в неявный base update.

`SnapshotId` идентифицирует версию, но не владеет graph data. Отдельный публичный
`GraphSnapshot` не вводится.
