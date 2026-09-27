# Ferros-Grynn GQL — Спецификация языка запросов

**Версия:** 0.3-draft
**Статус:** проектирование
**Нормативная область:** семантика языка, типы, транзакционное поведение и наблюдаемые
результаты. Точный принимаемый синтаксис задаёт [gql_grammar.md](gql_grammar.md), physical
storage/durability — [disk_storage_spec.md](disk_storage_spec.md), реализацию —
[gql_tech_spec.md](gql_tech_spec.md).

---

## Утверждённый профиль этапов 15–16 — полный graph-first MATCH

**Статус профиля:** `approved-for-implementation`. Этот раздел заменяет исторический
P0–P2-профиль для `MATCH`. Dependency-ordered реализацию и полный gate задаёт
[`gql_match_stages_15_16_plan.md`](gql_match_stages_15_16_plan.md).

Каноническая форма:

```grynql
[OPTIONAL] MATCH
    [DISTINCT]
    [ATOM]
    [REPEATABLE ELEMENTS | DIFFERENT EDGES | DIFFERENT ATOMS]
    [HYPER | META | METAVERTEX | METAEDGE]
    [CONSTRAINTS <path_constraint ...>]
    pattern [, pattern ...]
    [CONTAINS ...]
    IN GRAPH graph_ref
    [WHERE expression]
    [MAXDEPTH depth]
    [TIMEOUT duration]
    RETURN [DISTINCT] projection
    [ORDER BY ...];
```

Нормативные правила:

1. Optionality записывается только как `OPTIONAL MATCH`; `MATCH OPTIONAL` не принимается.
2. Источник задаётся только `IN GRAPH graph_ref`, где `graph_ref` — имя либо параметр типа
   `GraphRef`; историческая MATCH-форма `FROM graph` не принимается.
3. `OPTIONAL MATCH DISTINCT` сочетает независимые outer-match и binding-dedup semantics.
   `RETURN DISTINCT` отдельно дедуплицирует projected values.
4. `DIFFERENT EDGES` является default match mode. Mode распространяется на именованные и
   anonymous endpoint/connector positions всех patterns clause.
5. Selector позиции — class, exact `RecordId` либо `*`; `AS` вводит alias, `LABEL` задаёт
   graph-local label. Повтор alias является equality join. Inline object задаёт null-safe
   equality над effective properties; inline `WHERE` может читать только уже связанные aliases.
6. Открытые path ranges нормализуются до конечного max: `MAXDEPTH`, иначе
   `FG_PATH_MAX_DEPTH = 30`. Max больше 30 отклоняется до row pull. Default path mode — `TRAIL`;
   `WALK`, `ACYCLIC` и `SIMPLE` имеют различимую семантику из §15.
7. Global и local constraints имеют независимые оси mode, objective и AND-combined filters.
   Локальная ось заменяет одноимённую global axis; повтор mode/objective в scope ошибочен.
8. `SHORTEST` и `WEIGHTED SHORTEST` возвращают один канонический путь на source–target пару.
   Используются BFS и Dijkstra; weight складывается exact numeric matrix, отсутствующий равен
   единице, negative/non-finite даёт typed runtime error.
9. Public atom binding является `AtomOccurrenceRef = { AtomRef, GraphRef }`. `AtomRef` остаётся
   global durable identity; occurrence задаёт effective-property и topology scope. Durable fields
   occurrence не принимают.
10. `path::atoms/nodes/edges/length/cost` возвращают occurrence-aware значения. Trace создаётся
    только при наблюдаемом path либо необходимом objective.
11. Этап 16 активирует `ATOM`, `HYPER ... CONTAINS`, `META`/`METAVERTEX`/`METAEDGE`,
    `WITHIN` и cross-level incidence. Meta atom остаётся opaque: обычный traversal не входит во
    внутренний graph автоматически.
12. `max_path_expansions` и общий statement memory budget ограничивают paths, hyper assignments
    и containment. Timeout/cancellation не публикуют partial path.

## Исторический профиль P0–P2 (GrynQL MVP)

**Статус профиля:** `superseded`. Старый vertical slice и его удалённый синтаксис сохранены
только в исторических отчётах. Настоящая спецификация не определяет compatibility grammar:
принимается исключительно утверждённый профиль этапов 15–16 выше.

## Утверждённая коррекция модели данных — этап 8.1

**Статус профиля:** `implemented`. Шаги A0–A16 завершены 2026-09-22; evidence
зафиксирован в [graph_first_atom_model_report.md](reports/graph_first_atom_model_report.md).
Исторический TABLE-layout больше не входит в собираемый crate и сохраняется только
как evidence в отчётах. При конфликте этого
раздела с последующими draft-примерами правила этапа 8.1 имеют приоритет. Ключевое слово
`TABLE` и compatibility aliases не принимаются. Точный синтаксис профиля задаёт
[gql_grammar.md](gql_grammar.md) «Грамматика коррекции 8.1», физическую модель —
[disk_storage_spec.md](disk_storage_spec.md) «Целевая ревизия этапа 8.1».

### Модель identity и владения

```text
Database
├── ClassCatalog
│   └── CLASS: namespace RecordId + NODE/EDGE kind + schema + base indexes
├── AtomStore
│   └── RecordId -> global atom kind + base properties
└── GraphCatalog
    └── Graph
        ├── memberships: global atom -> graph-local AtomId
        ├── optional labels
        ├── edge topology / weight / nested-graph bindings
        └── sparse property overlays
```

Нормативные правила:

1. `RecordId = class:id` идентифицирует ровно один глобальный атом в пределах базы данных.
   Имя `CLASS` является первой, классовой частью identity, а `id` уникален внутри этого class.
2. `CLASS` является одновременно namespace идентификаторов, schema-контрактом и источником
   прямого scan. Каждый class неизменно имеет kind `NODE` либо `EDGE`; `ANY`, inheritance и
   multi-class atoms отсутствуют.
3. Каждый record class является атомом. Property-record — обычный `NODE` atom, который может не
   иметь ни одного graph membership. Отдельного SQL-like table storage не существует.
4. Base properties принадлежат глобальному атому. Один атом может иметь ноль, один или несколько
   graph memberships без копирования base properties.
5. Graph membership создаёт graph-local occurrence. Она содержит локальный `AtomId`, optional
   label, sparse overlay и, для edge, topology, weight и nested/transition binding. Один global
   edge может иметь разные occurrences в разных графах.
6. Class и label независимы: class стабилен и задаёт identity/kind/schema; optional label задаёт
   роль occurrence только в одном графе. В одной occurrence допускается не более одной label.
7. `GraphRef` — самостоятельная каталожная ссылка по ненулевому `GraphId`, имени, `GraphKind` и
   storage mode. Фиктивный `graph:<name>` `RecordId` не создаётся.
8. `RecordId` database-local. Cross-database qualification определяется будущим Bridge-профилем
   и не меняет identity внутри одной БД.

### Base и effective properties

Эффективное значение свойства в graph scope определяется строго:

| Состояние overlay   | Наблюдаемое значение  |
|---------------------|-----------------------|
| конкретное `Value`  | graph-local override  |
| внутренний `Hidden` | поле отсутствует      |
| записи нет          | актуальное base value |

`Hidden` не является пользовательским `NONE` или `NULL`. `UNSET field` в graph scope записывает
`Hidden`; `RESET field` удаляет запись overlay и снова включает наследование base value.
Unscoped `UPDATE` изменяет base atom и потому наблюдаем всеми memberships, кроме полей с override
или `Hidden`. Изменение обязано либо пройти полный preflight всех затронутых effective Unique
indexes и resource limits, либо завершиться до первой видимой мутации; частичный commit запрещён.

Record reference не создаёт membership. При разыменовании в graph scope member возвращает
effective view, а atom без membership — base view. Ссылка за пределами graph scope всегда
возвращает base view.

### DDL

```gql
DEFINE CLASS person TYPE NODE SCHEMAFULL;
DEFINE CLASS friendship TYPE EDGE SCHEMALESS;
DEFINE FIELD email ON CLASS person TYPE string;

DEFINE INDEX by_email ON CLASS person
  FIELDS email UNIQUE SCOPE BASE;

DEFINE INDEX by_email_social ON CLASS person
  FIELDS email UNIQUE SCOPE EFFECTIVE IN GRAPH social;
```

- `TYPE NODE|EDGE` обязателен и immutable; имя class также immutable.
- `REMOVE CLASS` разрешён только при отсутствии atoms и schema/index/reference dependencies.
- Standard, Unique и Count indexes обязаны явно выбирать `SCOPE BASE` либо
  `SCOPE EFFECTIVE IN GRAPH graph_ref`; implicit scope запрещён.
- Base index строится над глобальными atoms class. Effective index является полным graph-local
  индексом только по memberships выбранного графа и их effective values.
- Primary `RecordId` index принадлежит AtomStore и не объявляется пользователем. Membership,
  class/label и adjacency indexes принадлежат движку.

### Direct DML и membership

```gql
CREATE person:alice SET name = "Alice";
UPDATE person:alice SET name = "Alice Smith";

ATTACH person:alice TO GRAPH social LABEL Employee;
UPDATE person:alice IN GRAPH social LABEL Moderator SET nickname = "Ali";
UPDATE person:alice IN GRAPH social LABEL NONE;
UPDATE person:alice IN GRAPH social UNSET private_note;
UPDATE person:alice IN GRAPH social RESET private_note;

DETACH person:alice FROM GRAPH social;
DETACH person:alice FROM GRAPH social CASCADE;
DELETE person:alice;
DELETE person:alice CASCADE;
```

- `CREATE`, unscoped `UPDATE`/`UPSERT`/`DELETE` работают с global base atom.
- `UPDATE ... IN GRAPH` требует membership и меняет только overlay/label. Graph-scoped
  `UPSERT` и `DELETE` отсутствуют.
- `ATTACH` создаёт node occurrence; edge occurrence создаётся только через `RELATE`.
- `DETACH` по умолчанию использует `RESTRICT` и отклоняется при graph-local incidence.
  `CASCADE` удаляет затронутые edge occurrences, но не global edge atoms.
- `DELETE` по умолчанию использует `RESTRICT` при любом membership или reference. `CASCADE`
  атомарно удаляет memberships, incidences и overlays и применяет объявленные `ON DELETE`
  policies после bounded preflight.
- Удаление graph удаляет его occurrences, topology, overlays и effective indexes, но не global
  atoms.

### RELATE и MATCH

```gql
RELATE
  (person:alice)
  -[friendship AS e LABEL CloseFriend]->
  (person:bob)
IN GRAPH social;

RELATE
  (person:alice)
  -[friendship:alice_bob AS e LABEL CloseFriend]->
  (person:bob)
IN GRAPH social;

MATCH
  (person AS v1 LABEL Employee)
  -[friendship AS e LABEL CloseFriend]->
  (* AS v2 LABEL Moderator)
IN GRAPH social;
```

- `[class]` в `RELATE` генерирует новый edge `RecordId`; `[class:id]` создаёт отсутствующий или
  использует существующий global edge atom соответствующего `EDGE` class.
- Endpoints и edge автоматически получают membership в выбранном graph, если его ещё нет.
  Созданный base edge не остаётся после ошибки topology/family validation.
- `SET`, `CONTENT`, `WEIGHTED` и `LABEL` в `RELATE` всегда graph-local. Повторный `RELATE` одного
  edge в том же graph является конфликтом; rewire выражается `DETACH` + `RELATE` в одной
  транзакции.
- В `MATCH` первая позиция endpoint/edge body — class, exact `RecordId` либо `*`; `AS` вводит
  alias, а `LABEL` — независимый graph-local predicate. `* AS v LABEL X` является label-only
  lookup. Class + label выполняются как пересечение либо меньший posting stream с residual check.

### Direct reads и режимы

```gql
SELECT * FROM person;
SELECT * FROM person IN GRAPH social;
SELECT * FROM person:alice;
SELECT * FROM person:alice IN GRAPH social;
```

Unscoped source читает AtomStore/base view. `IN GRAPH` требует membership и возвращает effective
view. Mode 1 использует ту же нормализованную модель database root; Mode 2 читает AtomStore и
graph occurrences через disk engine; Mode 3 материализует effective graph в самостоятельный
owned result. Mutable Mode 3 flush может менять graph-local topology/overlay, но никогда не
выполняет неявный base update.

### Обязательные наблюдаемые сценарии

- Два graph могут разделять один node `RecordId`; base update виден обоим, overlay — только
  владельцу occurrence, а `RESET` восстанавливает актуальное base value.
- Один edge `RecordId` может иметь разные topology/weight/label в двух graph.
- Unattached property-record доступен прямым `SELECT` и через record reference.
- Class, label и class+label scan дают одинаковый результат с индексом и корректным fallback.
- `RESTRICT` не оставляет частичной мутации; `CASCADE` либо проходит целиком, либо отклоняется до
  публикации по resource limit/conflict/reference policy.
- Один snapshot наблюдает согласованную комбинацию base atom, membership, overlay и topology.

## Содержание

1. [Цели языка](#1-цели-языка)
2. [Дизайн-принципы](#2-дизайн-принципы)
3. [Модель данных и термины](#3-модель-данных-и-термины)
4. [Общий синтаксис](#4-общий-синтаксис)
5. [Система типов](#5-система-типов)
6. [Выражения и операторы](#6-выражения-и-операторы)
7. [Управляющие операторы](#7-управляющие-операторы)
8. [DML: мутации](#8-dml-мутации)
9. [DDL: DEFINE / REMOVE / ALTER](#9-ddl-define--remove--alter)
10. [Clauses](#10-clauses)
11. [SELECT](#11-select)
12. [MATCH — паттерн-матчинг](#12-match--паттерн-матчинг)
13. [Traversal Pipeline](#13-traversal-pipeline)
14. [Семантика биндингов](#14-семантика-биндингов)
15. [Семантика путей](#15-семантика-путей)
16. [Граф как результат запроса](#16-граф-как-результат-запроса)
17. [Транзакционная модель](#17-транзакционная-модель)
18. [Функциональная и объектная системы](#18-функциональная-и-объектная-системы)
19. [Индексы](#19-индексы)
20. [Анализаторы текста](#20-анализаторы-текста)
21. [События и Live-запросы](#21-события-и-live-запросы)
22. [Контракты планировщика](#22-контракты-планировщика)
23. [Формальная семантика](#23-формальная-семантика)
24. [Формальная система типов / правила вывода](#24-формальная-система-типов--правила-вывода)
25. [Грамматика (EBNF)](#25-грамматика-ebnf)
26. [Logical Plan (IR)](#26-logical-plan-ir)
27. [Правила оптимизатора](#27-optimizer-rule-based--cost-based)
28. [Примеры](#28-примеры)
29. [Настройки хранилища](#29-настройки-хранилища)
30. [Система мостов](#30-система-мостов)
31. [Соответствие кодовой базе](#31-соответствие-кодовой-базе)
32. [Известные риски](#32-известные-риски)

---

## 1. Цели языка

Ferros-Grynn GQL — гибридный язык запросов для графового движка Ferros-Grynn. Он совмещает три режима работы:

- **Декларативный** — SQL-like CRUD.
- **Паттерн-матчинг** — MATCH для поиска топологических подграфов.
- **Императивный traversal** — pipeline-обход с накопленным состоянием.

Все три режима компонуются в один запрос. 
Язык обеспечивает единообразную работу со всеми типами атомов: вершинами, рёбрами, мета-вершинами и мета-рёбрами.

**Ключевые требования:**

| Требование          | Описание                                                                            |
|---------------------|-------------------------------------------------------------------------------------|
| Единая модель       | Вершины, рёбра, метаатомы и пути — first-class citizens                             |
| Типы графов         | Ориентированные, неориентированные, взвешенные, мультиграфы, гиперграфы, мета-графы |
| ACID                | Serializable (SSI) по умолчанию; SI / Read Committed — явный opt-in                 |
| Типизация           | Статическая там, где возможно; динамическая для schemaless-данных                   |
| Компонуемость       | Подзапросы, `WITH`, `LET`, функции, pipeline                                        |
| Оптимизируемость    | Семантика допускает планирование, индексирование и переписывание                    |

**Комбинация парадигм**

Ferros-Grynn это не реляционная и не графовая БД в классическом понимании, это гибрид.
Основная сущность БД — атом. 
Из атома можно получить данные, если знать его id.
Можно получать данные и группы атомов, как в классическом SQL.
Атомы можно найти, используя pattern-matching, как в графовых БД, или используя Traversal Pipeline
как в движках типа TinkerPop.
Поверх этой модели накладывается GraphView — система работы с представлениями графов.

---

## 2. Дизайн-принципы

### 2.1. Прагматичность
Простые запросы — короткие. Сложная модель данных не должна ломать синтаксис.

### 2.2. Явность
Язык не скрывает важные семантические детали: направление обхода, кардинальность результата, дедупликацию, режим мутации. 
Если оператор ведёт себя по-разному в зависимости от контекста — это явно зафиксировано в спецификации.

### 2.3. Композиционность
Любая часть языка пригодна как:
- самостоятельный запрос;
- подзапрос;
- как источник traversal pipeline;
- выражение в `LET`, `RETURN`, `DEFINE FUNCTION`, `EVENT`.

### 2.4. Оптимизируемость
Семантика допускает логическое переписывание, предикатный pushdown и выбор физического оператора без изменения результата. 
Побочные эффекты явно маркируются и не переставляются планировщиком.

### 2.5. Предсказуемость
Одинаковые конструкции в разных местах языка ведут себя одинаково.

---

## 3. Модель данных и термины

### 3.1. Граф

**Граф** — именованный каталожный объект, содержащий graph-local occurrences глобальных атомов,
их incidence, optional labels, overlays и собственные свойства. Стабильная identity задаётся
ненулевым `GraphId`; имя уникально в пределах базы и используется для разрешения `GraphRef`.
Удаление графа не удаляет глобальные атомы.

### 3.2. Атом

**Атом** — адресуемая сущность базы данных. Он имеет `RecordId = class:id`, неизменный
`AtomKind::{Node, Edge}` и base properties. Optional label, topology, weight и overlay принадлежат
не атому вообще, а его occurrence в конкретном графе.

| Тип occurrence | Обозначение                             | Описание                                             |
|----------------|-----------------------------------------|------------------------------------------------------|
| `vertex`       | `(person AS n LABEL Employee)`          | Node-class atom в роли вершины данного графа         |
| `edge`         | `[friendship AS e LABEL CloseFriend]`   | Edge-class atom как incidence данного графа          |
| `metavertex`   | `<(person AS n LABEL Container)>`       | Node occurrence с вложенным подграфом                |
| `metaedge`     | `<[transition AS e LABEL CrossLevel]>`  | Edge occurrence с transition-подграфом               |

Рёбра могут иметь несколько конечных точек (гиперрёбра). 
Рёбра и мета-рёбра могут соединять атомы любого типа, в том числе другие рёбра (edge-to-edge).

### 3.3. Свойства

**Свойство** (Property) — пара ключ–значение в base atom, graph overlay или самом graph. Ключ —
строка; значение — любой тип из системы типов GQL ([§5](#5-система-типов)). Graph-effective
чтение применяет override/`Hidden`/inheritance правила утверждённой коррекции 8.1.

### 3.4. Класс

**Класс** (`CLASS`) — namespace первой части `RecordId`, schema-контракт и прямой scan source.
Каждый class фиксирует kind `NODE` либо `EDGE`; имя и kind immutable. Class не владеет отдельным
SQL-like хранилищем: записи всех classes находятся в общем AtomStore. Optional graph label не
меняет class.

### 3.5. Запись

**Запись** (`record`) — публично адресуемый глобальный атом. Запись имеет идентификатор вида
`class:id` и base properties. Любая запись принадлежит ровно одному class, но может иметь ноль
или несколько graph memberships.

### 3.6. Путь

**Путь** — последовательность чередующихся вершин и рёбер: $`v_0 e_1 v_1 e_2 v_2 \cdots e_n v_n`$, 
где каждое ребро $`e_i`$ соединяет $`v_{i-1}`$ и $`v_i`$. 
Путь — first-class тип данных в GQL.

### 3.7. Идентификаторы записей

Запись идентифицируется парой `class:id`, где:
- `class` — имя class и namespace принадлежности;
- `id` — внешний идентификатор.

Внешний идентификатор может быть записан в виде:
- буквенно-цифрового текста в формате Unicode,
- массива буквенно-цифровых текстов,
- объектов,
- встроенных функций генерации идентификаторов,
- функции создания идентификаторов записей на основе значений.

Примеры:
```
company:surrealdb
company:w6xb3izpgvz4n0gow6q7
reaction:`🤪`
weather:['London', d'2025-02-14T01:52:50.375Z']
```

Если `id` часть не указана пользователем, она будет сгенерирована автоматически.

ID доступен как системное поле `id` и автоматически входит в единый unique primary B+tree
AtomStore. `id` часть не обязана быть уникальной по всей базе данных, но обязана быть уникальной
внутри своего `class`.

Внутренний `GlobalAtomId` адресует atom в database store, а graph-local `AtomId` — его occurrence.
Оба являются implementation detail, проходят generation check и никогда не экспонируются в
GrynQL.

#### Random IDs

При создании записи без явного указания `id` части её присваивается случайный ULID идентификатор.

Идентификаторы можно также сгенерировать при помощи ряда встроенный функций, которые обеспечивают криптографическую защиту:
- `rand::id()` - charset: `abcdefghijklmnopqrstuvwxyz0123456789` (default);
- `ulid()`;
- `uuid()` - UUIDv7.

### Text and Numeric IDs

Текстовые идентификаторы могут содержать любые символы таблицы Unicode.
Чтобы создать идентификатор со сложными символами, они заключаются в обратные кавычки ``` `` ```.

```
CREATE article:`8424486b-85b3-4448-ac8d-5d51083391c7`;

CREATE `Artykuł`:100;
```

Для `id` части также можно использовать числа.
Числовые значения, находящиеся в интервале `[-9223372036854775808; 9223372036854775807]` будут обработаны как
64-битное целое число, в противном случае - как строка.

#### Array-based IDs

Идентификаторы записей могут состоять из массивов и даже объектов. 
Такой идентификатор записи чаще всего используется, когда есть одно или два поля, 
которые будут использоваться для поиска записей в диапазоне записей, что значительно повышает производительность. 
В отличие от использования `WHERE` для фильтрации, которое предполагает class scan.

Идентификаторы могут хранить массивы значений, в том числе вложенные массивы или объекты. 
В одном массиве могут храниться значения разных типов.

```
CREATE weather:['Лондон', d'2025-02-13T05:00:00Z'];
```

#### Record Ranges

При выборке данных можно использовать диапазоны записей по `id`.
Запросы по диапазону идентификаторов записей извлекают записи в естественном порядке сортировки по идентификаторам, 
что позволяет избежать полного class scan. Это эффективнее, чем фильтрация с помощью `WHERE`.

```
-- Выберите все записи о людях с идентификаторами в заданном диапазоне
SELECT * FROM person:1..1000;

-- Выберите все записи для определенного местоположения, включая их
SELECT * FROM temperature:['Лондон', NONE]..=['Лондон', ..];

-- Выберите все записи о температуре с идентификаторами меньше максимального значения
SELECT * FROM temperature:..['Лондон', '2022-08-29T08:09:31'];

-- Выберите все записи о температуре с идентификаторами, превышающими минимальное значение
SELECT * FROM temperature:['Лондон', '2022-08-29T08:03:39']..;

-- Выберите все записи о температуре с идентификаторами в указанном диапазоне
SELECT * FROM temperature:['Лондон', '2022-08-29T08:03:39']..['Лондон', '2022-08-29T08:09:31'];

```

Основная причина высокой производительности при работе с диапазонами записей заключается в том, что база данных заранее знает,
в какой области искать записи для запроса.

Это можно продемонстрировать на примере того, что происходит, когда один запрос на диапазон записей охватывает все записи в базе данных. 
В приведенном ниже примере создаются 10 000 player-записей с идентификатором на основе массива, который начинается с 'mage', 
что позволяет использовать их в запросе на диапазон записей, а также поле под названием class, которое также является 'mage' 
и будет использоваться в предложении `WHERE` для сравнения производительности.

```
FOR $_ IN 0..10000 {
    CREATE player:['mage', rand::id()] SET class = 'mage';
};

LET $_ = SELECT * FROM player:['mage', NONE]..['mage', ..];
LET $_ = SELECT * FROM player WHERE class = 'mage';
```

Однако, что в этом случае запрос по диапазону записей лишь немного более эффективен. 
Это связано с тем, что оба запроса в конечном итоге перебирают 10 000 записей, с той лишь разницей, 
что запрос с `WHERE` предложением также проверяет, совпадает ли значение class 'mage'.

Но если количество player-записей увеличится до большего числа классов, разница в производительности будет гораздо заметнее. 
В этом случае запрос на диапазон записей по-прежнему перебирает относительно небольшую область из 10 000 записей, 
в то время как во втором запросе нужно перебрать в десять раз больше записей в дополнение к `WHERE` условию в верхней части.

```
FOR $_ IN 0..10000 {
  CREATE player:['mage', rand::id()]      SET class = 'mage';
  CREATE player:['barbarian', rand::id()] SET class = 'barbarian';
  CREATE player:['rogue', rand::id()]     SET class = 'rogue';
  CREATE player:['bard', rand::id()]      SET class = 'bard';
  CREATE player:['sage', rand::id()]      SET class = 'sage';
  CREATE player:['psionic', rand::id()]   SET class = 'psionic';
  CREATE player:['thief', rand::id()]     SET class = 'thief';
  CREATE player:['paladin', rand::id()]   SET class = 'paladin';
  CREATE player:['ranger', rand::id()]    SET class = 'ranger';
  CREATE player:['cleric', rand::id()]    SET class = 'cleric';
};

LET $_ = SELECT * FROM player:['mage', NONE]..['mage', ..];
LET $_ = SELECT * FROM player WHERE class = 'mage';
```

#### Building record IDs

Чтобы создать идентификатор из двух значений, можно использовать функция `type::record`.
Она принимает два аргумента:
1. строка с именем `class`;
2. любой допустимый для `id` тип данных.

### 3.8. Единая endpoint-инцидентность

Пусть $N = V \cup MV$ — вершины, $R = E \cup ME$ — рёбра, а $A = N \cup R$ — все атомы.
Каждое ребро $r \in R$ хранит две стороны одного топологического отношения:
`source(r)` и `target(r)`, элементы которых принадлежат $A$. Отдельного механизма
инцидентности для связи рёбер с рёбрами нет.

| Вид endpoint'ов  | `source(r)`   | `target(r)`   | Режим пути             |
|------------------|---------------|---------------|------------------------|
| V–V              | vertex        | vertex        | `StandardPath`         |
| V–E              | vertex        | edge          | `AtomWalk`             |
| E–V              | edge          | vertex        | `AtomWalk`             |
| E–E              | edge          | edge          | `AtomWalk`             |
| Hyper, смешанный | atom[]        | atom[]        | по kind'ам endpoint'ов |
| Cross-level      | atom уровня k | atom уровня m | как выше               |

`source` и `target` — структурные стороны направленного ребра, а не предметные роли.
Для неориентированного ребра существует одно множество endpoint'ов; физически оно хранится
в `source`, а `target` пуст. Для двунаправленного ребра обе стороны заданы, но переход
разрешён в обоих направлениях.

Путь всегда проходит через само связывающее ребро. Связь E–E имеет вид
$e_1, r, e_2$, где $r$ — отдельное ребро, endpoint'ами которого являются $e_1$ и $e_2$.
Формы, соединяющие два атома без промежуточного ребра, не существуют. V–E, E–V и E–E
доступны только в `AtomWalk` (§15.2); обычное V–V-подмножество доступно в `StandardPath`.

Для обратного поиска поддерживается `EdgeEndpointIndex`: по edge-атому он возвращает
рёбра, в чьём `source` или `target` этот атом находится. Индекс не вводит новую
инцидентность и может быть полностью восстановлен из endpoint'ов.

**Кросс-уровневые рёбра** соединяют конкретные атомы той же endpoint-инцидентностью независимо от уровня вложенности подграфов.
Ребро так же хранит прямые ссылки на конечные атомы; флаг `CROSS_LEVEL` указывает, что хотя бы один endpoint принадлежит другому подграфу.


#### 3.8.1. Метки, свойства и предметная семантика

`source` и `target` — единственные структурные стороны endpoint-инцидентности. GrynQL не
задаёт встроенных предметных ролей участников и не резервирует для них отдельный enum.

Предметный смысл задаётся пользовательской схемой: label связывающего ребра определяет
вид отношения, а его свойства — дополнительные данные отношения. Для гиперребра `head`
и `tail` также описывают только направление обхода, а не предметные роли endpoint'ов.
Конкретные способы моделирования финансовых, юридических и иных доменов находятся вне
этой спецификации.

#### 3.8.2. MetaEdge и transition-подграф

MetaEdge — ребро с возможностью хранения внутреннего `transition_graph` (подграфа).
Ребро остаётся связью между атомами (`source`/`target` не изменяются), но несёт дополнительную структуру:
граф, описывающий «поведение» или «протокол» данной связи.

Пример: ребро `[:SEND_MESSAGE]` между двумя компонентами может хранить
подграф шагов протокола (handshake, retry, ack). Это не меняет того, что ребро соединяет два компонента.

Доступ к transition-подграфу: `graph::inner(e)` для MetaEdge.

Наличие edge-endpoint'а само по себе не делает ребро MetaEdge. MetaEdge отличается
внутренним подграфом и/или `transition_graph`; его endpoint'ы используют тот же механизм.

#### 3.8.3. Кросс-уровневые рёбра и LevelPath

Ребро помечается флагом `CROSS_LEVEL`, если хотя бы один его endpoint находится в подграфе, отличном от графа, содержащего ребро.
Семантика ребра не изменяется: оно соединяет конкретные атомы из `source` и `target`.

**LevelPath** — опциональная структура, записываемая в свойства атомов через `KEEP LEVELPATH`. 
Содержит упорядоченную цепочку мета-вершин от самой верхней мета-вершины, содержащей endpoint, до самого endpoint:
`[mv_top, mv_middle, ..., leaf_atom]`. Путь идёт от сверху вниз: от endpoint на высшем уровне до endpoint на нижнем уровне.    
LevelPath не является traversal-механизмом. Это метаданные атома, позволяющие понять иерархию вложенности.

```
-- Stage 17: найти endpoints и затем создать связь с явным LevelPath
MATCH (Function AS inner_a) WITHIN*[1..7] <(Module AS mv1)>,
      (Class AS inner_b) WITHIN*[1..7] <(Package AS mv2)>
IN GRAPH catalog
RETURN inner_a, inner_b, mv1, mv2;

RELATE (inner_a)-[CALLS AS e]->(inner_b) KEEP LEVELPATH;

-- e.levelpath = [inner_a, mv1, mv2, inner_b]
-- inner_a.levelpath = [mv1]
-- inner_b.levelpath = [mv1, mv2]
```

`KEEP LEVELPATH` **поддерживается только для бинарных кросс-уровневых связей**
(`source_count = 1`, `target_count = 1`).
Для кросс-уровневых гиперрёбер используйте функцию `graph::level_path(edge)` вручную для каждого endpoint.

#### 3.8.4. Матричное представление инцидентности

**$B_{ep}$ — Endpoint-тензор (atom $\times$ edge $\times$ side)**

$B_{ep} \in \left\{ 0; 1 \right\}^{|A| \times |R| \times 2}$:
- $B_{ep}[a, r, 0] = 1$, если $a \in \operatorname{source}(r)$;
- $B_{ep}[a, r, 1] = 1$, если $a \in \operatorname{target}(r)$.

Для неориентированного ребра логическое представление устанавливает обе компоненты для
каждого endpoint'а, даже если дисковая запись хранит единое множество только в `source`.
Тензор охватывает V–V, V–E, E–V, E–E, hyper- и cross-level-инцидентность.

**C — Atom Adjacency Matrix**

- $C[a_1, a_2] = \displaystyle\sum_r B_{ep}[a_1, r, 0] \cdot B_{ep}[a_2, r, 1] \cdot \text{weight}(r)$
- $C[a_1, a_2] \neq 0 \Longleftrightarrow \exists r: a_1 \in \operatorname{source}(r) \wedge a_2 \in \operatorname{target}(r)$.

Суммируется при мультирёбрах (взвешенная граф-матрица смежности).

Полная atom $\times$ atom смежность восстанавливается из $B_{ep}$:
- $C[a_1, a_2] = \operatorname{OR}_r (B_{ep}[a_1, r, 0] \; \operatorname{AND} \; B_{ep}[a_2, r, 1])$

Полная atom $\times$ atom матрица смежности $C = C_{vv} + C_{ve} + C_{ev} + C_{ee}$:
- $C_{vv}[v_1, v_2]$ — через V-V рёбра (nodes $\times$ nodes)
- $C_{ve}[v, e]$ — через V-E рёбра (nodes $\times$ edges)
- $C_{ev}[e, v]$ — через E-V рёбра (edges $\times$ nodes)
- $C_{ee}[e_1, e_2]$ — через E-E endpoint-рёбра (edges $\times$ edges)

$B_{ep}$ позволяет строить любую из этих подматриц фильтрацией по типу атома.

**Реализация в Rust**: `EndpointTensor` вынесен в
[implementation_plan.md](implementation_plan.md) §3.5 «Тензорное представление».

**Соотношение с существующими матрицами:**

| Структура         | Тензор                  | Охват                                            |
|-------------------|-------------------------|--------------------------------------------------|
| `AdjacencyMatrix` | $C_{vv}$                | node $\times$ node, взвешенная                   |
| `IncidenceMatrix` | $B_{ep}$ (только nodes) | node $\times$ edge, {Source, Target, Both}       |
| `IncidenceTensor` | $B_{ep}$ (только nodes) | node $\times$ edge $\times$ side                 |
| `EndpointTensor`  | $B_{ep}$ (все атомы)    | **atom $\times$ edge $\times$ {Source, Target}** |

`IncidenceMatrix` и `IncidenceTensor` сохраняются для обратной совместимости.
`EndpointTensor` — полное представление единого механизма.

### 3.9. Ограничения типов графов

Граф или подграф может иметь структурные ограничения. Они задаются при определении графа или применяются к именованным подграфам.

#### 3.9.1. Виды ограничений

| Тип           | Инвариант                                                               | Применимость                       |
|---------------|-------------------------------------------------------------------------|------------------------------------|
| `SIMPLE`      | Нет петель, нет мультирёбер между одной парой атомов                    | Совместимая graph family           |
| `TREE`        | Связный, ацикличный, \|E\| = \|V\|−1                                    | Классическая vertex-edge топология |
| `ROOTED_TREE` | `TREE` + выделен корень; для directed-вида задана ориентация            | Классическая vertex-edge топология |
| `FOREST`      | Каждый компонент связности является деревом                             | Классическая vertex-edge топология |
| `DAG`         | Directed, без self-loop и циклов; parallel edges допустимы              | Классическая vertex-edge топология |
| `BIPARTITE`   | Вершины делятся на два непересекающихся множества                       | Классическая vertex-edge топология |
| `COMPLETE`    | Каждая пара вершин соединена                                            | Классическая vertex-edge топология |
| `HYPER`       | Vertex-hyperedge incidence без meta-атомов и cross-level incidence      | Hypergraph family                  |
| `META`        | Допускаются meta-атомы, вложенные графы и cross-level incidence         | General/metagraph family           |

#### Разделение деревьев 
- `TREE` — свободное (unrooted) дерево — чисто структурный инвариант: связный ациклический граф, |E| = |V| − 1.
У такого дерева нет понятия «родитель» вообще — это симметричная структура. Любая вершина потенциально может быть выбрана корнем при обходе, 
но пока корень не выбран явно, отношения «родитель/потомок» не определены математически — есть только «сосед».
- `ROOTED_TREE` — корневое дерево — это свободное дерево + ровно одна выделенная вершина-корень. 
Только после этого выбора возникают производные понятия: родитель, потомок, глубина, предки, листья.
- `FOREST` — лес деревьев — это граф, в котором каждый компонент связности является деревом (`TREE` или `ROOTED_TREE`).

`TREE`, `ROOTED_TREE`, `FOREST` и `DAG` не применяются напрямую к произвольной
multi-hyper-meta-топологии. Сначала должна успешно выполниться явная проверка/конвертация в
классическую vertex-edge family. Ограничения, не противоречащие друг другу, могут
комбинироваться.

Ограничения задаются через `DEFINE GRAPH`:
```
DEFINE GRAPH my_dag TYPE DAG [VALIDATE ON_MUTATION | DEFERRED];
DEFINE GRAPH my_tree TYPE ROOTED_TREE ROOT (person:root);
```

#### 3.9.2. Время и стоимость проверки ограничений

**`VALIDATE ON_MUTATION` (по умолчанию):**
Ограничение проверяется при каждой операции, изменяющей топологию (RELATE, DELETE).
Нарушение → ABORT текущей транзакции с ошибкой `ConstraintViolation`.

| Ограничение   | Алгоритм при добавлении ребра           | Сложность        |
|---------------|-----------------------------------------|------------------|
| `SIMPLE`      | Lookup в adjacency index                | O(log n)         |
| `DAG`         | DFS от target → поиск пути до source    | O(\|V\| + \|E\|) |
| `TREE`        | Union-Find: одна компонента             | O(α(\|V\|))      |
| `ROOTED_TREE` | Union-Find: одна компонента + in-degree | O(α(\|V\|))      |
| `BIPARTITE`   | BFS 2-coloring                          | O(\|V\|)         |
| `COMPLETE`    | Счётчик рёбер vs ожидаемое количество   | O(1)             |

Удаление ребра из typed `TREE`/`ROOTED_TREE`, разрывающее связность, отклоняется. Деградация
`Tree → Forest` допустима только как явное преобразование с изменением типа результата; mutable
операция не меняет graph family молча.

**`VALIDATE DEFERRED`:**
Ограничение не проверяется при каждой мутации. Явная проверка:
```
ASSERT INVARIANTS [FOR GRAPH @graph_name];
COMMIT WITH VALIDATION;  -- проверка при коммите
```

#### 3.9.3. Преобразования graph family

GQL не выполняет неявных преобразований между graph family. Аннотация
`RETURN GRAPH <type>` или `BUILD GRAPH <type>` после сборки валидирует топологию и создаёт
точный запрошенный тип.

Без повторной проверки допустимы только расширяющие представления:

```text
ROOTED_TREE → TREE → FOREST → GRAPH
directed TREE/FOREST → DAG view → GRAPH view
DAG → GRAPH view
HYPER → GRAPH view
META → GRAPH view
```

Owned-конвертация между разными family может перестраивать внутреннее представление за
`O(V+E)`. Сужающие направления (`GRAPH → DAG`, `GRAPH/DAG/FOREST → TREE` и аналогичные)
являются fallible и возвращают `ConstraintViolation`, если инвариант не выполнен.
Undirected `TREE → DAG` требует явно выбрать корень и направление. `DAG → TREE` допустим,
только если существует один корень, каждая прочая вершина имеет indegree 1, граф связен и
`|E| = |V| - 1`. Полная матрица приведена в [graphs.md](graphs.md).

#### 3.9.4. Traversal и ограничения

Тип графа **не влияет на поведение traversal**. Traversal с `WALK` по DAG-графу допустим. 
Ограничение — структурный контракт на топологию графа, не на запросы к нему.

Исключение: `MATCH ... CONSTRAINTS <ACYCLIC>` — это ограничение на **результирующий путь** traversal, не на граф целиком.

---

## 4. Общий синтаксис

### 4.1. Идентификаторы

- Имена классов, графов, полей, индексов, функций — идентификаторы: начинаются с буквы, содержат буквы, цифры и `_`.
- Параметры начинаются с `$`: `$name`, `$limit`.
- Пользовательские функции используют пространство имён `fn::`: `fn::calc_score(...)`.
- Встроенные функции — пространство имён по категории: `math::`, `string::`, `time::`, `array::`, `geo::`, `search::`, `graph::`, etc.
- Зарезервированные слова нельзя использовать как идентификаторы.

### 4.2. Литералы

```
42                   -- int
3.14                 -- float
"hello"              -- string (двойные кавычки)
'hello'              -- string (одинарные кавычки)
true                 -- bool
false                -- bool
null                 -- null
none                 -- none
[1, 2, 3]            -- array
set[1, 2, 3]         -- set
{a: 1, b: 2}         -- object
```

Строки поддерживают escape-последовательности в стиле JSON. 
Многострочные строки — через `"""..."""`.

### 4.3. Комментарии

```
-- Однострочный комментарий
/* Многострочный комментарий */
```

### 4.4. Блоки

`{ ... }` — блок из последовательности операторов. 
Последнее выражение блока является результатом блока, если контекст это допускает.

### 4.5. Разделители

Операторы разделяются `;`. Последний `;` в блоке или запросе опционален.

### 4.6. Выражения

Выражение может содержать:
- литералы;
- поля и idioms;
- вызовы функций;
- подзапросы;
- traversal pipeline;
- условные выражения;
- замыкания;
- агрегации.

### 4.7. Семантика ошибок

| Класс          | Описание                                                                    |
|----------------|-----------------------------------------------------------------------------|
| Синтаксическая | Нарушение грамматики                                                        |
| Семантическая  | Нарушение правил языка (неопределённый alias, нарушение binding scope)      |
| Типовая        | Несовместимые типы в выражении                                              |
| Транзакционная | Конфликт, abort, timeout                                                    |
| Планировочная  | Ошибка при построении плана (например, отсутствие индекса при `WITH INDEX`) |
| Runtime        | Деление на ноль, переполнение, отсутствие записи при разыменовании          |

---

## 5. Система типов

### 5.1. Примитивные типы

| Тип        | Описание                                                                                      |
|------------|-----------------------------------------------------------------------------------------------|
| `bool`     | Логический                                                                                    |
| `int`      | 64-битное знаковое целое                                                                      |
| `float`    | 64-битное число с плавающей запятой (IEEE 754)                                                |
| `decimal`  | Десятичное число фиксированной точности: 28 значащих цифр, масштаб 0..28, без NaN/∞           |
| `number`   | Числовой тип без явного указания; разрешается в `int`, `float` или `decimal` на этапе анализа |
| `string`   | UTF-8 строка                                                                                  |
| `bytes`    | Массив байт                                                                                   |
| `datetime` | Дата и время с часовым поясом (RFC 3339)                                                      |
| `duration` | Временной интервал                                                                            |
| `null`     | Значение существует, но не определено                                                         |
| `none`     | Значение отсутствует (поле не существует)                                                     |
| `any`      | Динамический тип — принимает любое значение                                                   |

**Разделение `null` / `none`:**
- `null` означает, что поле существует и явно установлено в «ничего».
- `none` означает, что поле или результат отсутствуют (поле не определено в схеме или не вернулось из запроса).
- `none` не является подтипом никакого типа `T`; `null` принадлежит любому nullable-типу.
- Оба sentinel являются обычными значениями и не вводят третье логическое состояние:

  ```text
  none = none   → true
  null = null   → true
  none = null   → false
  null = 1      → false
  null != 1     → true
  bool(none)    → false
  bool(null)    → false
  ```

  `AND`, `OR`, `NOT`, сравнения, `IS NULL`, `IS NONE` и предикаты фильтра всегда возвращают
  `bool`. В `WHERE` оба sentinel поэтому ведут себя как `false`, а не как `UNKNOWN`.

#### Агрегаты и `null` / `none`

Агрегатные функции **пропускают** и `null`, и `none` — они не участвуют в вычислении и не влияют на счётчики. Это соответствует поведению SQL и SurrealQL и позволяет считать агрегаты по разреженным schemaless-данным без предварительной очистки.

| Выражение               | Семантика                                                                               |
|-------------------------|-----------------------------------------------------------------------------------------|
| `count()`               | Число строк в группе. Считает **все** строки, включая те, где все поля `null`/`none`    |
| `count(expr)`           | Число строк, где `expr` **не** `null` и **не** `none`                                   |
| `count(DISTINCT expr)`  | Число различных не-`null`/не-`none` значений `expr`                                     |
| `sum(expr)`             | Сумма не-`null`/не-`none` значений                                                      |
| `avg(expr)`             | `sum(expr) / count(expr)` — делитель считает только учтённые значения                   |
| `min(expr)`/`max(expr)` | Минимум/максимум по не-`null`/не-`none` значениям                                       |
| `array::group(expr)`    | Собирает не-`null`/не-`none` значения; `null` сохраняется, если передан явным литералом |

**Результат на пустом входе** (группа пуста или все значения пропущены):

| Агрегат                  | Результат |
|--------------------------|-----------|
| `count()`, `count(expr)` | `0`       |
| `sum(expr)`              | `0`       |
| `avg`, `min`, `max`      | `none`    |

`sum` возвращает `0`, а не `none`, потому что сумма пустого множества — аддитивная единица; `avg`/`min`/`max` на пустом множестве не определены, поэтому дают `none` (отсутствие результата), а не `null`.

**Группировка.** `GROUP BY` различает `null` и `none` как **две разные группы**: `null` — «поле есть, значение не задано», `none` — «поля нет». Обе группы попадают в результат наравне с остальными. Если такое различие не нужно, нормализуйте ключ: `GROUP BY field ?? null`.

**Сортировка.** Позиция обеих групп в `ORDER BY` управляется `NULLS FIRST` / `NULLS LAST` (по умолчанию `NULLS LAST`, [§10.5](#105-order-by)); `none` сортируется рядом с `null`.

```
-- age задан не у всех
SELECT count() AS rows,          -- все строки
       count(age) AS with_age,   -- только те, где age задан
       avg(age)   AS mean_age    -- делитель = with_age, не rows
FROM person GROUP ALL;
```

### 5.2. Составные типы

| Тип                       | Описание                                                                                      |
|---------------------------|-----------------------------------------------------------------------------------------------|
| `array<T>`                | Типизированный массив произвольной длины                                                      |
| `array<T, N>`             | Массив фиксированной длины N                                                                  |
| `set<T>`                  | Множество уникальных значений типа T                                                          |
| `object`                  | Произвольный объект ключ–значение; ключ — строка, значение — `any`, если явно не типизировано |
| `option<T>`               | `T` или `None`                                                                                |
| `range<T>`                | Диапазон значений типа T; поддерживает числа и строки в лексикографическом порядке            |
| `record<T>`               | Ссылка на глобальный атом класса T                                                            |
| `record<T1 \| T2>`        | Ссылка на глобальный атом одного из перечисленных классов                                     |
| `closure<(T1, ...) -> U>` | Тип функции–замыкания                                                                         |
| `regex`                   | Регулярное выражение                                                                          |
| `geometry`                | Геометрический объект (GeoJSON)                                                               |
| `error`                   | Сериализованный JSON-представление типа `FerrosGrynnError`                                    |

Тип `bag<T>` — internal only для результатов некоторых запросов, на клиенте он материализуется как `array<T>` с сохранением дубликатов.

`option<T>` — только статический тип. В runtime `Some(v)` представлен самим `v`, а отсутствие
представлено `none`; отдельного варианта значения `option` и отдельного дискового тега нет.

### 5.3. Утилитарные типы

| Тип           | Описание                                                                                       |
|---------------|------------------------------------------------------------------------------------------------|
| `computed<T>` | прозрачный тип, лениво-вычисляемые значения (применимо, например, к time::now())               |
| `formatter`   | значение-форматтер для темпоральных значений (любой формат, поддерживаемый Rust крейтом Jiff)  |
| `matcher`     | Специальный тип, предназначенный для поиска совпадений по строкам                              |
| `cursor`      | Непрозрачный курсор пагинации ([§10.6](#106-limit--start)); клиент не разбирает его содержимое |

### 5.4. Доменные типы

| Тип              | Описание                                                                          |
|------------------|-----------------------------------------------------------------------------------|
| `vertex<T?>`     | Атом–вершина; T — метка (опционально)                                             |
| `metavertex<T?>` | Атом–метавершина с вложенным подграфом                                            |
| `edge<T?>`       | Атом–ребро                                                                        |
| `hyperedge<T?>`  | Гипер–ребро                                                                       |
| `metaedge<T?>`   | Атом–метаребро с transition-подграфом                                             |
| `atom<T?>`       | Любой атом (vertex, edge, meta*)                                                  |
| `path`           | Путь в графе                                                                      |
| `subgraph`       | Самодостаточный материализованный результат с сохранением выбранной инцидентности |
| `graph`          | Универсальный граф: multi-/hyper-/meta-граф и cross-level incidence               |
| `tree`           | Классический vertex-edge граф с инвариантом дерева                                |
| `rooted_tree`    | `tree` с выбранным корнем и ориентацией                                           |
| `forest`         | Набор классических деревьев                                                       |
| `dag`            | Классический directed vertex-edge ациклический граф                               |
| `hypergraph`     | Vertex-hyperedge граф без meta-атомов и cross-level incidence                     |
| `metagraph`      | Универсальный граф с meta-атомами и вложенными графами                            |

### 5.5. Иерархия типов (частичная)

```
any
├── null, none
├── bool
├── number
│   ├── int
│   ├── float
│   └── decimal
├── string
├── bytes
├── datetime, duration
├── array<T>, set<T>, range<T>, object
├── option<T>
├── closure<...>
├── regex
├── geometry
├── record<T>
└── graph types
    ├── atom<T>
    │   ├── vertex<T>, metavertex<T>
    │   └── edge<T>, hyperedge<T>, metaedge<T>
    ├── path
    ├── subgraph
    └── graph families
        ├── graph, metagraph
        ├── hypergraph
        ├── dag
        └── forest
            └── tree
                └── rooted_tree
```

Это иерархия допустимых структур, а не обещание общего физического представления. Узкие типы
имеют собственные инварианты и могут иметь специализированное хранение. `SIMPLE`, `BIPARTITE` и
`COMPLETE` являются дополнительными проверяемыми ограничениями, но не отдельными runtime-типами.
Полная таксономия и допустимые преобразования зафиксированы в [graphs.md](graphs.md).

### 5.6. Приведение типов

Автоматическое (неявное): `int → decimal`, `int → float`; `decimal → float` — только если
контекст явно ожидает `float`, с предупреждением о возможной потере точности.
Явное (только через cast): `<U> expression`. Например: `<int> 3.14` → `3`.
Сужающие преобразования (`decimal → int`, `float → int`) требуют явного cast.

**`float → decimal` не является расширением и неявно не выполняется.** `float` (IEEE 754 binary64) достигает ≈1.8·10³⁰⁸, тогда как `decimal` ограничен ≈7.92·10²⁸ ([§5.1](#51-примитивные-типы)); кроме того, `float` допускает `NaN` и `±∞`, которых у `decimal` нет. Поэтому преобразование возможно только явным cast `<decimal> f` и является **fallible**: при выходе за диапазон или при `NaN`/`±∞` возвращается ошибка `NumericOutOfRange`, а не молча усечённое значение.

Обратное направление `decimal → float` допустимо неявно только в указанном выше контексте, но
помечается как потенциально теряющее точность (28 десятичных цифр не укладываются в 53 бита
мантиссы).

Тип `number` разрешается в конкретный из `int`/`float`/`decimal` на этапе анализа ([§18.7](#187-перегрузка)); если в одном выражении встречаются `float` и `decimal`, разрешение завершается ошибкой — их следует привести явно, выбрав нужную семантику (точность против диапазона).

### 5.7. Idioms — навигация по полям

Idiom — выражение навигации по вложенным полям объекта:

```
person.name
article.author.email
node.props.weight
edge.props["custom-key"]
```

Idioms поддерживают:
- вложенный доступ через `.`;
- доступ по строковому ключу через `["key"]`;
- безопасный null-пропагирующий доступ через `?.`: `person?.address?.city` возвращает `none`, если любой промежуточный объект `none`/`null`;
- деструктуризацию объектов: `address.{city, country}`;
- фильтрацию массивов объектов через `WHERE`: `address[WHERE active = true]`;
- использование в `WHERE`, `LET`, `RETURN`, `DEFINE FIELD`, `DEFINE INDEX`.

### 5.8. Замыкания

Замыкание (`closure`) — это анонимная функция, которую можно присвоить в переменную через `LET`:
```
LET $clos = || {...}

LET $clos = |$x, $y| {...}

LET $clos = |$x: int, $y: string| -> geometry {...}
```
Они могут иметь аргументы и возвращать значения через `RETURN` внутри блока.

Замыкания не требуют определения через `DEFINE FUNCTION` и не имеют пространств имён.

### 5.9. Регулярные выражения

> **Отложено до этапа 21.** Синтаксис типа остаётся зарезервированным, но cast,
> `string::matches`, stateful matcher и mutable collection helpers не входят в
> исполнимый профиль этапов 11–12.

Следующий пример фиксирует целевой, но пока неисполнимый контракт этапа 21.

Regex поддерживают стандартный синтаксис регулярных выражений, создаются путём каста строки к типу `regex` и применяются двумя способами:
- через функцию `string::matches`, которая возвращает все совпадения,
- через создание `matcher` объекта как, например, в Java. 
```
LET $reg = <regex> "colou{3}r";
string::matches("colouuur", $reg); -- true

LET $original = "Егор Алла Александр";
LET $matcher = regex::matcher($original, "А.+а");
LET $matches = [];
WHILE (regex::next($matcher)) {
    array::push($matches, string::slice($original, $matcher.start, $matcher.end));
}
-- $matches = ["Алла", "Алекса"];
```
Объект типа `matcher` имеет два публичных целочисленных поля: `start` и `end`, - указывающие на начало и конец совпадения в исходной строке.
Применение `regex::next` обновляет `start` и `end` на основе внутреннего состояния `matcher`-объекта.

### 5.10. Вычисляемые выражения

Вычисляемые (computed) выражения не хранятся, а вычисляются непосредственно в момент обращения к ним.
```
DEFINE FIELD accessed_at ON CLASS user COMPUTED time::now();

CREATE user:one;
SELECT * FROM ONLY user:one; // 01:53:04

SLEEP 1s;

SELECT * FROM ONLY user:one; // 01:53:05
```

Тип `T` поля, помеченного как `COMPUTED` помечается типом-обёртвой `computed<T>`.
При явном вызове `computed::compute(...)` или при автоматическом его вызове во время выборки возвращается вычисленное значение типа `T`.

Важно уточнить, что computed значение может зависеть от другого computed значения только в том случае,
если эта цепочка не образует циклов. 
При определении полей через `DEFINE FIELD` строится граф computed значений и, если в нём обнаружится цикл, операция падает с ошибкой.

### 5.11. Типы путей

GQL различает три типа путей с разной семантикой:

#### 5.11.1. StandardPath

Классическое чередование: $vertex_0, edge_1, vertex_1, edge_2, ..., edge_n, vertex_n$.

Инварианты (при индексации с нуля):
- Чётные позиции (0, 2, 4, ...) — только вершины (vertex или meta-vertex).
- Нечётные позиции (1, 3, 5, ...) — только рёбра.
- Каждое ребро $e_i$ соединяет $vertex_{i-1}$ и $vertex_i$ через endpoint-инцидентность.

Длина пути: количество рёбер (= количество шагов).

Используется по умолчанию в MATCH, TRAVERSE, RETURN GRAPH.

#### 5.11.2. AtomWalk

Расширение StandardPath: $atom_0, edge_1, atom_1, edge_2, ..., edge_n, atom_n$.

Отличия от StandardPath:
- в endpoint-позициях разрешены любые атомы из $A = V \cup MV \cup E \cup ME$;
- связывающие рёбра остаются в нечётных позициях и не исчезают из пути;
- edge-атом может быть endpoint'ом одного ребра и связывающим ребром другого шага.

Включается ключевым словом `ATOM` в `MATCH`, `TRAVERSE`, `RELATE` и других операторах,
которые принимают графовый паттерн:
```
MATCH ATOM <(Person AS a)>~[ANNOTATES AS e1]~><[FRIEND AS e_mid]>
           ~[OWNED_BY AS e2]~><(Person AS b)>
IN GRAPH social
RETURN a, e1, e_mid, e2, b;
```

Синтаксис AtomWalk:
- `~[r]~>` / `<~[r]~` — направленный шаг через явное ребро `r`;
- `~[r]~` — неориентированный шаг;
- `<~[r]~>` — двунаправленный шаг;
- `<(Class AS v)>` — node endpoint, `<[Class AS e]>` — edge endpoint; внутри обёртки
  сохраняется та же форма selector/`AS`/`LABEL`, что и для StandardPath.

Kind endpoint'а определяется только внутренними delimiters. Class selector не является label:
graph-local label указывается отдельно, например `<(Person AS x LABEL Employee)>`.

Каждый endpoint в операции с ключевым словом `ATOM` обязан использовать одну из этих двух
обёрток. Тильда и явное связывающее ребро `[Class AS alias]` либо `[* AS alias]`
обязательны, поэтому AtomWalk визуально отличается от StandardPath. Форм `~~`, `~>`,
`~[]~>` и других анонимных связей нет.

Длина пути: количество связывающих рёбер (= количество шагов), как в StandardPath.

Пример: E–E incidence-путь:
```
-- flow связывает два edge-атома как source и target
MATCH ATOM <[TRANSFER AS e1]>~[FLOW AS flow]~><[TRANSFER AS e2]>
IN GRAPH payments
RETURN e1, e2;
```

Пример: V–E endpoint-путь:
```
-- Вершина alice соединена endpoint-ребром с ребром friendship
MATCH ATOM <(Person AS alice)>~[ANNOTATES AS annotation]~><[FRIEND AS friendship]>
IN GRAPH social
RETURN alice, annotation, friendship;
```

#### 5.11.3. LevelPath

Не traversal-механизм. Цепочка мета-вершин, описывающая иерархию вложенности от внешнего уровня до конкретного атома:
$`\text{LevelPath} = [mv_1, mv_2, ..., mv_n, leaf_atom]``$, где $mv_1 \supset mv_2 \supset \dots \supset mvₙ \supset leaf_atom$ (вложенность).

Опционально записывается в свойства ребра через `KEEP LEVELPATH` при создании или выборке кросс-уровневого ребра (см. §3.8.3).
Тип поля — `array<atom>`.

`LevelPath` не участвует в traversal, к нему нельзя применять path-функции `path::nodes()`, `path::edges()`, `path::length()`. 
Только прямой доступ как к свойству: `e.levelpath[0]`, `array::len(e.levelpath)`.

---

## 6. Выражения и операторы

### 6.1. Арифметика

| Оператор           | Описание             |
|--------------------|----------------------|
| `+`, `-`, `*`, `/` | Базовая арифметика   |
| `%`                | Остаток от деления   |
| `**`               | Возведение в степень |

Деление на ноль — runtime error, если не обёрнуто в `TRY {...}`.

### 6.2. Сравнение

| Оператор                                                                                      | Описание                                                                                                                                                                                                           |
|-----------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `=`, `IS` / `!=` `IS NOT`                                                                     | Равенство / неравенство                                                                                                                                                                                            |
| `<`, `>`, `<=`, `>=`                                                                          | Сравнение                                                                                                                                                                                                          |
| `!`, `NOT` / `&&`, `AND` / `\|\|`, `OR`                                                       | Логические: инверсия / И / ИЛИ                                                                                                                                                                                     |
| `!!`                                                                                          | not-not оператор (вдойное применение `!`), определяет истинность значения                                                                                                                                          |
| `??`                                                                                          | Null Coalescing Operator: проверка на не NONE и не NULL                                                                                                                                                            |
| `?:`                                                                                          | Truthy Coalescing Operator: проверка на правдивость                                                                                                                                                                |
| `?=`                                                                                          | Равен ли какой-то элемент коллекции значению                                                                                                                                                                       |
| `*=`                                                                                          | Равны ли все элементы коллекции значению                                                                                                                                                                           |
| `IN` / `NOT IN` / `ALLIN` / `ANYIN` / `NONEIN`                                                | Принадлежность коллекции: элемент внутри / элемент не внутри / все эелементы внутри / хотя бы один элемент внутри / ни один элемент не внутри. Также можно использовать для проверки  наличия ключа внутри объекта |
| `INSIDE` / `NOTINSIDE ` / `ALLINSIDE ` / `ANYINSIDE ` / `NONEINSIDE`, `OUTSIDE`, `INTERSECTS` | Для geometry-типов                                                                                                                                                                                                 |
| `CONTAINS`, `CONTAINSNOT`, `CONTAINSANY`, `CONTAINSALL`                                       | Проверка вхождения (для коллекций и строк)                                                                                                                                                                         |
| `"text" @@ "index"`                                                                           | Полнотекстовый поиск                                                                                                                                                                                               |
| `<\| K, metric? \|>`, ``<\| K, KF \|>``                                                       | KNN-поиск с задаваемой метрикой / KNN-поиск с HNSW                                                                                                                                                                 |

Любое значение считается _правдивым_, если оно не равно `none`, `null` или значению по умолчанию своего типа данных.
Значение по умолчанию — пустое значение типа: пустая строка, коллекция или объект, числовой ноль.

Примеры проверок:
```
-- ??
SELECT * FROM NULL ?? 0 ?? false ?? 10;
-- Output: 0

-- ?:
SELECT * FROM NULL ?: 0 ?: false ?: 10;
-- Output: 10
```

Сравнения являются двухзначными. `none` и `null` равны только самим себе; сравнение на
равенство с любым другим значением возвращает `false`, а на неравенство — `true`.

Для `ORDER BY`, `DISTINCT`, `set`, ключей и публичного `Ord` Rust SDK действует единый
канонический порядок вариантов:

```text
none < null < bool < number < string < duration < datetime
     < ulid < uuid < array < set < object < geometry < bytes
     < record < graph_ref < regex < range
     < atom < path < subgraph < graph < closure
```

Внутри одного варианта коллекции и пути сравниваются лексикографически; объекты — по
канонически отсортированным парам ключ–значение. Графовые значения сравниваются по
`identity + revision`, а не по содержимому или изоморфизму. `option<T>` не имеет отдельного
ранга: `Some(v)` сравнивается как `v`, отсутствие — как `none`.

### 6.3. Полнотекстовый поиск

Оператор `"text" @@ "index"` проверяет, находится ли текст в полнотекстовом индексе.

Если внутри оператора указать числовую ссылку, то к ней можно обратиться для выделения и вычисления оценки совпадения:
```
SELECT id,
		search::highlight('<b>', '</b>', 1) AS title,
		search::score(1) AS score
FROM book
WHERE title @1@ 'rust web'
ORDER BY score DESC;

[
	{
		id: book:1,
		score: 0.9227996468544006,
		title: '<b>Rust</b> <b>Web</b> Programming'
	}
]
```

Для удобства проверки в нескольких индексах в операторе можно указать `AND` или `OR`, а затем передать массив индексов:
```
-- With @AND@, @OR@
SELECT * FROM document WHERE text @AND@ ["personal", "rare"];
SELECT * FROM document WHERE text @OR@ ["personal", "nice weather today"];

##
-- Without @AND@, @OR@
SELECT * FROM document WHERE text @@ "personal" AND text @@ "rare";
SELECT * FROM document WHERE text @@ "personal" OR text @@ "nice weather today";
```

### 6.4. KNN

```
<| @neighbors [ , @distance | EF @value ] |>
```

Для поиска ближайших соседей используется синтаксис `<| N |>`, где N - количество искомых соседей.

`DISTANCE` в форме `<|K, DISTANCE|>` — metavariable выражения расстояния, а не keyword и не
переименование DDL `DIST`. Можно передать built-in distance либо функцию/замыкание подходящей
сигнатуры. Термин distance намеренный: пользовательская функция не обязана быть математической
метрикой.
```
<|2,EUCLIDEAN|>

<|5,MANHATTAN|>

LET $my_metric = |x: float, y: float| -> float { math::abs(x - y) };
CREATE pts:3 SET point = [8,9,10,11];
SELECT id FROM pts WHERE point <|2,$my_metric|> [2,3,4,5];
```

HNSW метод рекомендуется для очень больших объёмов данных, где важна скорость обработки и допустима некоторая потеря точности.
Вместо метрики указывается размер динамического списка кандидатов поиска, влияющий на точность и скорость поиска:
```
CREATE pts:3 SET point = [8,9,10,11];
DEFINE INDEX mt_pts ON pts FIELDS point HNSW DIMENSION 4 DIST EUCLIDEAN EFC 150 M 12;
SELECT id FROM pts WHERE point <|10, EF 40|> [2,3,4,5];
```

`<|K|>` использует default EUCLIDEAN. Planner может выбрать HNSW/DiskANN только при точном
совпадении index `DIST`, element type, dimensions, scope и field; иначе выполняется exact scan с
тем же distance. `<|K, EF n|>` явно требует ready ANN index: `WITH INDEX` выбирает его, без hint
допустим ровно один compatible candidate; отсутствие и неоднозначность дают typed planning
errors. Выбранный approximate path и индекс отображаются в `EXPLAIN`.

### 6.5. Условные выражения

```
-- IF как выражение
IF age >= 18 THEN "adult" ELSE "minor" END

-- CASE-выражение
CASE status
  WHEN "active" THEN 1
  WHEN "pending" THEN 2
  ELSE 0
END

-- Ternary (сокращение)
status = "active" ? 1 : 0
```

### 6.6. Подзапросы в выражениях

Скалярный подзапрос в скобках: `(SELECT count() FROM person)`.  
Для проверки существования:
`NOT array::is_empty(MATCH (Person AS a {name: $name}) IN GRAPH $graph RETURN a)`.

Во втором примере планировщик обязан применять оптимизацию: проверять размер массива, не материализуя записи.

---

## 7. Управляющие операторы

### 7.1. LET

Объявляет локальную переменную в текущем scope.

```
LET $var [: @type] = @expression;
```

Правила:
- Имена разрешаются в порядке `block → transaction → session/program → параметры execute`.
- Вне транзакции переменная живёт до конца программы либо persistent `GqlSession`; временная
  session `Datastore::execute` уничтожает её после возврата.
- Внутри транзакции binding живёт до `COMMIT`, `CANCEL` или непойманной ошибки и наружу не
  экспортируется. Binding блока, цикла и `CATCH` уничтожается при выходе из блока.
- Вложенный scope может скрыть внешний binding; повторное объявление в одном scope ошибочно.
  `$_` является discard-binding и не сохраняется.
- Параметры одного `execute` неизменяемы; имя, уже занятое session/transaction binding,
  отклоняется до исполнения.
- Выражение может быть любым: литерал, подзапрос, pipeline, closure, IF-ELSE.
- `LET` может кешировать промежуточные результаты и использоваться повторно.

```
LET $friends = MATCH (Person AS a { name: "Alice" })-[FRIEND]->(Person AS f)
               IN GRAPH social RETURN f;
LET $count: int = (SELECT count() FROM person WHERE age > 18);
```

### 7.2. RETURN

Возвращает результат выражения, блока или функции.

```
RETURN @expression [AS @alias];
```

В теле функции `RETURN` завершает выполнение. В теле запроса — одно `RETURN` на верхнем уровне. Несколько `RETURN` допустимы только внутри ветвей `IF/ELSE`.

### 7.3. IF / ELSE

```
IF @condition { @expression; .. }
   [ ELSE IF @condition { @expression; .. } ] ...
   [ ELSE { @expression; .. } ]
```

`IF` является выражением и может быть использован там, где ожидается значение. При отсутствии `ELSE` результатом является `none`.
Условие использует единую truthiness-семантику §6.2.

### 7.4. Циклы

Итерация. Используется для административных задач, скриптов, миграций. Для запросной логики предпочтительны traversal и агрегации.

```
FOR @item IN @iterable {
    @statements
}

WHILE (@condition) {
    @statements
}
```

`BREAK` и `CONTINUE` работают только внутри `FOR` / `WHILE`.

`FOR` принимает array в исходном порядке, set в каноническом порядке `Value` и конечный
integer range: `..` исключает конец, `..=` включает. Открытый range и иной тип дают typed
runtime error. Значением цикла является значение последней завершённой итерации либо `none`;
результаты всех итераций не накапливаются. На один top-level statement действуют общий предел
`1_000_000` loop iterations и control depth `20`, если API options не задают более строгие
значения.

### 7.5. BEGIN / COMMIT / CANCEL

По умолчанию каждый top-level statement выполняется в собственной автотранзакции. Программа
полностью разбирается и структурно проверяется до первого исполнения, но семантический анализ
происходит непосредственно перед каждым statement по текущему published/private schema root.
Поэтому DDL виден следующему statement. Ошибка автотранзакции откатывает только текущий
statement; уже опубликованные statements программы сохраняются.

`BEGIN` открывает явную транзакцию; `COMMIT` фиксирует её; `CANCEL` откатывает. Default —
`SERIALIZABLE READ WRITE`. Поддерживаются `READ COMMITTED`, `SNAPSHOT`, `SERIALIZABLE` и
`READ ONLY`. Начиная с этапа 14 исполнимы `BEST_EFFORT` и строгий `DETERMINISTIC`;
`FAST` распознаётся, но возвращает typed unsupported до открытия транзакции. Сочетание
`READ COMMITTED + DETERMINISTIC` запрещено, поскольку не фиксирует один snapshot всей
транзакции.

```
BEGIN;
  UPDATE person:alice SET name = "Alissa";
  RELATE (person:alice)-[:FRIEND]->(person:bob);
COMMIT;
```

Непойманная ошибка внутри явной транзакции немедленно откатывает её целиком. Вложенный `BEGIN`
и `COMMIT/CANCEL` без активной транзакции ошибочны. `Datastore::execute` требует закрытой
транзакции к концу вызова и откатывает незавершённый `BEGIN`; persistent `GqlSession` может
продолжать транзакцию между вызовами, а её `Drop` всегда выполняет rollback. `READ COMMITTED`
фиксирует новый root ровно один раз в начале каждого top-level statement; `SNAPSHOT` и
`SERIALIZABLE` удерживают один root до завершения транзакции.

Коммит с валидацией дополнительно перепроверяет весь private root — AtomStore, ссылки,
graph-family invariants и derived bindings — общей реализацией validator:
```
COMMIT WITH VALIDATION;
```

`Transaction::execute` не принимает transaction statements и `BEGIN IMPORT`. Непойманная
GrynnQL-ошибка отменяет эту transaction; прямые Rust mutation methods сохраняют текущую
statement-atomic семантику.

### 7.6. EXPLAIN

`EXPLAIN` является только префиксным оператором. Plain и `FULL` строят план без исполнения,
row pull, writer lease и выделения durable identifiers. `ANALYZE` выполняет statement ровно один
раз с обычной транзакционной семантикой, но заменяет его пользовательский result планом и
runtime-метриками.

```
EXPLAIN [ ANALYZE | FULL ] [ FORMAT TEXT | JSON ] @statement
```

- `EXPLAIN` — normalized/re-written логический план.
- `EXPLAIN FULL` — физический план, estimates, стоимости, selected indexes, fallbacks и resource
  warnings.
- `EXPLAIN ANALYZE` — физический план с реальной статистикой выполнения. Успешная DDL/DML в
  autocommit публикуется; внутри explicit transaction изменяет private root до `COMMIT`; ошибка
  использует обычные rollback/abort rules. `SELECT` полностью потребляется и continuation не
  создаёт.

Поддерживаются SELECT, текущий исполнимый MATCH, CREATE/UPDATE/UPSERT/DELETE,
ATTACH/DETACH/RELATE/UPDATE GRAPH и DEFINE/ALTER/REMOVE. Transaction statements, Atomic Import,
control flow, вложенный `EXPLAIN` и ещё не реализованные statements отклоняются структурно.
`FORMAT TEXT` является default; `FORMAT JSON` возвращает structured object с versioned
`PlanHash`, snapshot, mode/seed, stable preorder node IDs, logical/physical operators,
cardinality ranges, cost components, ordering, index bounds, statistics freshness и warnings.
`ANALYZE` дополнительно выводит actual rows/pulls, elapsed observations, spill, accounted memory,
affected rows и publication state; elapsed timings не входят в deterministic replay guarantee.

### 7.7. THROW

Создаёт generated status `UserThrown` со строковым сообщением пользователя.

```
THROW @message
```

Ошибка перехватывается ближайшим `TRY/CATCH`; непойманный `THROW` прекращает программу и
откатывает активную явную транзакцию либо весь Atomic Import. Повторный выброс безопасной
проекции ошибки записывается как `THROW $e.message`.

### 7.8. SLEEP

Используется для приостановки выполнения запросов.

```
SLEEP @time
```

Это может быть полезно:
- при ожидании фоновой операции, запущенной с `CONCURRENTLY` (см. [§9.6](#96-define-index));
- для регулирования количества выполняемых операций.

### 7.9. TRY-CATCH

Оператор используется для отлавливания runtime ошибок при выполнении запроса.

```
TRY { @statements } [ CATCH |$e| { @statements } ]
```

`CATCH |$e| { … }` — специальный handler с одним block-local binding типа `error`, а не общая
closure. До `TRY` создаётся savepoint private root, конфликтных/read sets, reservations,
accounting, mutation log и lexical scopes. При catchable runtime error состояние полностью
восстанавливается до запуска handler. Ошибка handler передаётся внешнему `TRY` либо прекращает
транзакцию.

Перехватываются value/type/domain errors, item-level DML/schema/constraint violations,
отсутствующие или duplicate identity/membership, graph-family mutation errors, `THROW` и
`ASSERT INVARIANTS`. Parse/analysis/planning errors, budgets, timeout/cancellation,
serialization conflicts, I/O/checksum, resource exhaustion и internal invariants не
перехватываются. Значение `error` является runtime-only безопасной проекцией diagnostic:
`code`, `message`, `condition`, `classification`, `category`, `domain`, `causes`; storage codec
его отклоняет.

### 7.10. ASSERT

Команда `ASSERT` проверяет выполнение ограничений, зависящих от контекста, и при их невыполнении выбрасывает ошибку.

#### ASSERT INVARIANTS

Немедленно проверяет ограничения на тип графа.

```
ASSERT INVARIANTS [ FOR GRAPH @graph_name ] [ REPORT @number ];
```

`REPORT @number` ограничивает число подробно описанных нарушений в ответе (по умолчанию `FG_IMPORT_MAX_REPORTED_VIOLATIONS`); общее количество нарушений по классам возвращается всегда.
Допустим диапазон `0..=FG_IMPORT_MAX_REPORTED_VIOLATIONS`. `FOR GRAPH` проверяет только
указанный graph и связанные atom references; без scope — все видимые graphs private root.
Успех возвращает object с полями `checked_graphs`, `violation_total`,
`violation_totals` и `violation_details`; счётчики и список нарушений равны нулю.
Нарушение — catchable typed error с полными totals и ограниченными details.

### 7.11. BEGIN IMPORT / END IMPORT — массовая загрузка

Режим массовой загрузки с атомарной COW-публикацией. 

Внутри эпохи импорта используются те же операторы DML, но движок переключается на bulk-путь: 
индексы отсоединяются и строятся заново в конце, проверки ограничений откладываются, события и changefeed по умолчанию отключаются.

`BEGIN IMPORT` фиксирует исходный
snapshot и создаёт private root; читатели продолжают видеть прежний snapshot. Ни один
внутренний segment не является видимым commit. После успешного `END IMPORT` недостижимое
UUID-поколение синхронизируется и публикуется одним catalog/WAL commit. Любая ошибка до
publication отбрасывает staging целиком; rollback усечением опубликованных файлов не
используется.

```
BEGIN IMPORT [ INTO GRAPH @graph ] [ WITH ( @option = @value [, ...] ) ];
    @statements
END IMPORT [ WITH VALIDATION ];
```

| Опция          | По умолчанию                     | Контракт этапа 10                                                                                                                                                                                                       |
|----------------|----------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `INDEXES`      | `DEFERRED`                       | `DEFERRED` перестраивает secondary/Label/Count/effective indexes в конце; primary identity и correctness-critical lookup сопровождаются всегда. `MAINTAINED` использует обычное incremental сопровождение private root. |
| `CONSTRAINTS`  | `DEFERRED`                       | `DEFERRED` проверяет graph-family и deferred Unique перед publication; `IMMEDIATE` — после каждого logical item. Schema/type/kind/reference checks не откладываются.                                                    |
| `EVENTS`       | `OFF`                            | Этап 10 принимает только `OFF`; `ON` распознаётся и возвращает typed unsupported.                                                                                                                                       |
| `CHANGEFEED`   | `OFF`                            | Этап 10 принимает только `OFF`; `ON` распознаётся и возвращает typed unsupported.                                                                                                                                       |
| `ON VIOLATION` | `ABORT`                          | `ABORT` отбрасывает весь import; `SKIP` атомарно пропускает только item-level нарушение. `QUARANTINE` пока typed unsupported.                                                                                           |
| `REJECT LIMIT` | отсутствует                      | Для `SKIP` обязателен явный положительный предел; нарушение `limit + 1` отбрасывает весь import.                                                                                                                        |
| `SEGMENT SIZE` | `FG_IMPORT_SEGMENT_ITEMS`        | Applied items во внутреннем transaction segment; не commit и не recovery point.                                                                                                                                         |
| `FILL_FACTOR`  | fixed writer policy              | Синтаксис зарезервирован; явное значение пока typed unsupported.                                                                                                                                                        |
| `SORT_MEM`     | fixed 256 MiB run budget         | Синтаксис зарезервирован; явное значение пока typed unsupported.                                                                                                                                                        |
| `PARALLELISM`  | последовательная baseline-сборка | Синтаксис зарезервирован; явное значение пока typed unsupported.                                                                                                                                                        |

**Зачем отдельный режим.** Построчная вставка сопровождает каждый индекс отдельно: это случайные обращения к B+tree-страницам, кратная запись и объёмный WAL. Bulk-путь вместо этого собирает ключи, сортирует их вне памяти и строит каждый индекс снизу вверх, записывая каждую страницу ровно один раз ([§20.13 disk_storage_spec](disk_storage_spec.md)). Смежность записывается один раз с уже известной финальной степенью, поэтому overflow-цепочки сразу нужного размера и непрерывны.

**Почему default — `CONSTRAINTS DEFERRED`.** Проверка `VALIDATE ON_MUTATION` для `DAG`
выполняет обход на каждое добавляемое ребро — `O(E · (V + E))`; отложенная проверка выполняет
один проход `O(V + E)`. `IMMEDIATE` остаётся поддерживаемым явным режимом для небольших
импортов. Отложенная whole-graph/Unique ошибка не приписывается одному item и всегда
отбрасывает весь staging, даже при `ON VIOLATION SKIP`.

**Завершение.** `END IMPORT` перестраивает deferred indexes/statistics и обязательно
проверяет import delta и hard constraints. `WITH VALIDATION` дополнительно перепроверяет весь
затронутый AtomStore, graph и rebuilt side-files, включая pre-existing data. Результат —
структурированный `ImportReport`: processed/applied/skipped, segments, totals и bounded details
нарушений, modes, full-validation flag и phase/index timings. Локальный путь в result/error не
возвращается.

**Scope и откат.** `INTO GRAPH` требует уже существующий graph, подставляет его в
graph-local DML без явного scope и запрещает явное обращение к другому graph до начала
выполнения. Base DML остаётся database-global и не создаёт membership. При любой ошибке
старый root остаётся единственным видимым состоянием; одинаковая all-or-nothing семантика
действует для AtomStore и существующего graph.

Import обязан быть единственным top-level statement и не выполняется внутри открытого
`Transaction`. Исполнимый профиль этапа 12 принимает `CREATE`, `UPDATE`, `UPSERT`, `DELETE`,
`ATTACH`, `DETACH`, `RELATE`, `FOR`, `IF/ELSE`, `TRY/CATCH` и `THROW`. `WHILE`, `LET`,
`ASSERT`, result-producing `RETURN` и transaction statements рекурсивно отвергаются до writer
lease. Отсутствующий DML `RETURN` означает `RETURN NONE`.

Простой top-level DML является одним import-item; каждая итерация top-level `FOR` — отдельным
item; top-level `IF` или `TRY` — одним item; вложенный control-flow остаётся частью текущего
item. Языковой `TRY` использует вложенный savepoint и имеет приоритет над `ON VIOLATION`.
Только непойманное item-level нарушение поступает этой политике; непойманный `THROW`, timeout,
cancellation и resource failure всегда отбрасывают import целиком. Успешный no-op учитывается
`SEGMENT SIZE`.

```
BEGIN IMPORT INTO GRAPH social WITH ( ON VIOLATION = SKIP, REJECT LIMIT = 10000 );
    FOR $row IN $people { CREATE person CONTENT $row; };
    FOR $e IN $edges { RELATE (person:$e.from)-[:FRIEND]->(person:$e.to); };
END IMPORT WITH VALIDATION;
```

---

## 8. DML: мутации

### 8.1. CREATE

Создаёт новые записи или атомы.

```    
CREATE @target [, @target ...]
    [ CONTENT @object
      | SET @field = @value, [, @field = @value ... ]
    ]
    [ RETURN NONE | RETURN @statement_param, ... | RETURN VALUE @statement_param ]
    [ EXPECT @condition ]
    [ TIMEOUT @duration ]
;
```

`target` — имя class, `class:id` либо `class:{ ... }` с параметрами генерации. Создаваемый
record всегда является global base atom; graph membership этой операцией не создаётся.

- `CONTENT` — задаёт запись целиком; `READONLY`-поля игнорируются.
- `SET` — точечная инициализация отдельных полей.
- `RETURN NONE` — ничего не возвращать; `RETURN @statement_param` — возвращает объект; `RETURN VALUE field` — вернуть скалярное значение конкретного поля.
- Без `RETURN` возвращает весь созданный объект.
- `|target:n|` — генератор диапазона целевых вставок.

```
-- Создать запись
CREATE person CONTENT { name: "Alice", age: 30 };

-- Массовое создание
CREATE |person:5| CONTENT { name: "Alice", age: 30 };

-- Создать с явным ID
CREATE person:alice SET name = "Alice", age = 30;
```

### 8.2. UPDATE

```    
UPDATE @target [, @target ...]
    [ IN GRAPH @graph ]
    [ LABEL @label | LABEL NONE ]
    [ CONTENT @object
      | MERGE @object
      | PATCH @object
      | REPLACE @object
      | [ SET @field = @value, [, @field = @value ... ] 
      | UNSET @field [, @field ...]
      | RESET @field [, @field ...] ]
    ]
    [ WHERE @condition ]
    [ RETURN NONE | RETURN BEFORE | RETURN AFTER | RETURN DIFF | RETURN @statement_param, ... | RETURN VALUE @statement_param ]
    [ EXPECT @condition ]
    [ TIMEOUT @duration ]
;
```

Семантика изменяющих режимов:

| Режим           | Описание                                                   |
|-----------------|------------------------------------------------------------|
| `CONTENT`       | Полная замена; `READONLY`-поля игнорируются                |
| `REPLACE`       | Полная замена; ошибка при попытке изменить `READONLY`-поле |
| `MERGE`         | Поверхностное слияние; изменяет только перечисленные поля  |
| `PATCH`         | JSON Patch (RFC 6902) или эквивалентная патч-семантика     |
| `SET` / `UNSET` | Детерминированные точечные изменения                       |
| `RESET`         | Только в graph scope: удалить override и наследовать base  |

Без `IN GRAPH` изменяется base atom. С `IN GRAPH` существующий membership обязателен;
изменяются только sparse overlay и/или optional label. В graph scope `UNSET` записывает
внутренний `Hidden`, а не удаляет base property. `CONTENT`/`REPLACE` применяются к effective
представлению, но результат сохраняется как минимальный overlay относительно base.

`RETURN`:
- `NONE` — ничего не возвращать;
- `BEFORE` — вернуть старый объект;
- `AFTER` — новый объект (по умолчанию, как и без `RETURN`);
- `DIFF` — вернуть список изменений в виде объекта;
- `@statement_param` — конкретные значения с именем поля;
- `VALUE @statement_param` — калярное значение конкретного поля.

### 8.3. UPSERT

Вставляет запись, если её нет, или обновляет существующую.

```
UPSERT @target [, @target ...]
    [ CONTENT @object
      | MERGE @object
      | PATCH @object
      | REPLACE @object
      | [ SET @field = @value, [, @field = @value ... ] 
      | UNSET @field [, @field ...] ]
    ]
    [ WHERE @condition ]
    [ RETURN NONE | RETURN BEFORE | RETURN AFTER | RETURN DIFF | RETURN @statement_param, ... | RETURN VALUE @statement_param ]
    [ EXPECT @condition ]
    [ TIMEOUT @duration ]
;
```

Критерий существования — идентификатор записи или условие `WHERE`. 
Если критерий не задан явно, семантика зависит от физического плана — это запрещено: `WHERE` или явный ID обязательны для `UPSERT`.
`UPSERT ... IN GRAPH` отсутствует: создание global atom и membership выражается отдельными
`UPSERT`/`ATTACH` либо `RELATE`.

### 8.3.1. UPDATE GRAPH

Catalog-owned свойства именованного графа изменяются отдельно от atom DML:

```grynql
UPDATE GRAPH @sales
SET properties.owner = "risk", properties.reviewed = true
RETURN AFTER;

UPDATE GRAPH @sales
UNSET properties.region
RETURN DIFF;
```

Разрешены только paths внутри `properties`. `SET` использует существующие idiom/update helpers;
`null` хранится, `none` запрещён. `UNSET` удаляет path. Default return — `NONE`; доступны
`BEFORE`, `AFTER` и `DIFF`. Оператор создаёт новую catalog/schema revision при сохранении
`GraphId` и physical bindings, участвует в MVCC, FCW/SSI и local catalog/WAL publication, но не
перестраивает graph generation или `GraphSuperblock`. В Atomic Import этапа 13 этот оператор
запрещён preflight-анализом.

### 8.4. DELETE

```
DELETE @targets
    [ CASCADE ]
    [ WHERE @condition ]
    [ RETURN NONE | RETURN BEFORE | RETURN AFTER | RETURN DIFF | RETURN @statement_param, ... ]
    [ EXPECT @condition ]
    [ TIMEOUT @duration ]
;
```

Обычный `DELETE` удаляет global base atom и использует `RESTRICT`: наличие membership или
reference является ошибкой. Явный `CASCADE` удаляет все graph occurrences, incidences и overlays
и применяет schema `ON DELETE` policies как одну транзакционную мутацию после bounded preflight.

Мягкое удаление (soft delete) реализуется через события:

```
DEFINE EVENT archive_person ON CLASS person WHEN $event = "DELETE" THEN {
    CREATE deleted_person SET
        data = $before,
        deleted_at = time::now()
};
```
Это событие вызывается при удалении записи из ```person``` и использует ```$before```
для записи удалённого пользователя в архив.

### 8.5. RELATE

Создаёт graph-local edge occurrence, используя global edge atom:

```gql
RELATE
  (person:alice)
  -[friendship AS e LABEL CloseFriend]->
  (person:bob)
IN GRAPH social
  [ WEIGHTED @value ]
  [ CONTENT @object | SET @field = @value [, ...] ]
  [ RETURN NONE | BEFORE | AFTER | DIFF | @field [, ...] | VALUE @field ]
  [ EXPECT @condition ]
  [ TIMEOUT duration ];
```

`[friendship]` выбирает `EDGE` class и генерирует новый `RecordId`; форма
`[friendship:alice_bob]` создаёт отсутствующий или использует существующий global edge atom.
`AS` вводит alias результата, `LABEL` задаёт optional graph-local label. Endpoint'ы являются
exact `RecordId` либо ранее связанными atom values. Они и edge автоматически получают membership
в `social`, если его нет. `CONTENT`, `SET`, `WEIGHTED` и `LABEL` изменяют только эту occurrence.

По умолчанию endpoint'ы должны иметь `NODE` kind. `RELATE ATOM` расширяет их домен до любого
атома и сохраняет тильда-синтаксис `~[...]~>`, `<~[...]~`, `~[...]~`, `<~[...]~>` с явными
`<(...)>`/`<[...]>`. Несовпадение class kind или graph family является semantic error до
публикации; созданный этой операцией base edge при ошибке также откатывается.

Если выбранный edge уже имеет occurrence в том же graph, возвращается conflict. Изменение
topology выражается `DETACH edge FROM GRAPH graph` и `RELATE ... IN GRAPH graph` в одной
транзакции.

**Направление** задаётся стрелками вокруг метки ребра:

| Синтаксис                | Топология                   |
|--------------------------|-----------------------------|
| `(a)-[edge_class]->(b)`  | Ориентированное ребро a → b |
| `(a)<-[edge_class]-(b)`  | Ориентированное ребро b → a |
| `(a)-[edge_class]-(b)`   | Неориентированное ребро     |
| `(a)<-[edge_class]->(b)` | Двунаправленное ребро       |

**Гиперрёбра** — n-арная форма того же endpoint-механизма. `ATOM` и `HYPER`
ортогональны: `RELATE HYPER` допускает только vertex endpoint'ы, а `RELATE ATOM HYPER` —
любые атомы.

```
RELATE HYPER [edge_class AS e LABEL E]
  TARGETS [a=(entity:a), b=(entity:b), c=(entity:c)]
  [ AS DIRECTED [ (head: [a], tail: [b, c]) ]
   | AS UNDIRECTED
   | AS BOTH [ (head: [a], tail: [b, c]) ]
  ]
  [ ORDER BY TAIL | ORDER BY TARGETS ];

RELATE ATOM HYPER [edge_class AS e LABEL E]
  TARGETS [a=<(entity:a)>, link=<[relation:link]>, c=<(entity:c)>]
  [ AS DIRECTED [ (head: [a, link], tail: [c]) ]
   | AS UNDIRECTED
   | AS BOTH [ (head: [a, link], tail: [c]) ]
  ]
  [ ORDER BY TAIL | ORDER BY TARGETS ]
;
```

В терминологии Ferros-Grynn `head` — входная сторона (`source`), `tail` — выходная
сторона (`target`). Обе стороны являются неотсортированными множествами, поэтому направленное гиперребро может
иметь несколько `head`. Если разбиение не задано, первый элемент `TARGETS` становится
единственным `head`, остальные — `tail`.

Порядок порождения веток без `ORDER BY` не определён. При явном `tail` предложение
`ORDER BY TAIL` сохраняет его порядок; без явного разбиения `ORDER BY TARGETS` использует
порядок `TARGETS`: для directed/both — после исключения неявного `head`, для undirected —
целиком. Эти формы взаимоисключающие и задают
порядок endpoint'ов ребра, а не сортировку результирующих строк.

Для гиперрёбер с `AS BOTH` семантика аналогична `AS DIRECTED`, но обход разрешён и в обратную сторону.

`WEIGHTED @value` задаёт числовой вес ребра — обязательный атрибут для взвешенных / темпоральных / вероятностных графов.

Для темпоральных графов значением веса должно быть типа `duration`, в противном случае — `number`.

```
-- Ориентированное ребро с весом
RELATE (person:alice)-[friendship LABEL FRIEND]->(person:bob)
  IN GRAPH social WEIGHTED 1.0 SET since = "2020-01-01";

-- Гиперребро
RELATE HYPER [coauthoring LABEL COAUTHOR]
  TARGETS [person:alice, person:bob, person:carol]
  IN GRAPH publications AS UNDIRECTED ORDER BY TARGETS SET paper = paper:xyz;

-- Смешанное гиперребро: два source-атома и два target-атома
RELATE ATOM HYPER [activation LABEL ACTIVATES]
  TARGETS [sensor=<(sensor:temperature)>, guard=<[relation:guard]>,
           alarm=<(alarm:main)>, audit=<[relation:audit]>]
  AS DIRECTED (head: [sensor, guard], tail: [alarm, audit])
  IN GRAPH automation ORDER BY TAIL;
```

### 8.6. ATTACH / DETACH

```gql
ATTACH @record_id TO GRAPH @graph [ LABEL @label ];
DETACH @record_id FROM GRAPH @graph [ CASCADE ];
```

`ATTACH` принимает только global `NODE` atom и создаёт одну membership; повторное присоединение
является конфликтом. Edge occurrence создаётся `RELATE`. Обычный `DETACH` использует `RESTRICT`
при любой graph-local incidence. `CASCADE` удаляет затронутые edge occurrences, но сохраняет их
global edge atoms и все occurrences в других graphs.

### 8.7. INFO

Возвращает метаданные системы, базы, class, graph или индекса.

```
INFO FOR [
	SYSTEM
	| DB
	| CLASS @class
    | GRAPH @graph
    | INDEX @index ON CLASS @class
];
```

#### ```INFO FOR SYSTEM```
```
{
  system: {
    available_parallelism: 14,
    cpu_usage: 0.3816290497779846,
    load_average: [
      1.2734375,
      1.68310546875,
      1.9189453125
    ],
    memory_allocated: 13900485,
    memory_usage: 136314880,
    physical_cores: 14,
    threads: 32
  },
}
```

#### ```INFO FOR DB```
```
{
  analyzers: {},
  configs: {},
  functions: {},
  graphs: {},
  models: {},
  params: {},
  classes: {
      person: 'DEFINE CLASS person TYPE NODE SCHEMALESS'
  }
}
```

#### ```INFO FOR CLASS @class```
```
{
  definition: 'DEFINE CLASS user TYPE NODE SCHEMALESS',
  events: {},
  fields: {
      name: 'DEFINE FIELD name ON user TYPE string'
  },
  indexes: {},
  lives: {},
  classes: {}
}
```

#### ```INFO FOR INDEX @index ON CLASS @class```
```
-------- Query 1 --------
{ 
  building: {
      initial: 0,
      pending: 0,
      status: 'indexing', 
      updated: 0
  }
}

-------- Query 2 --------
{ 
  building: {
      initial: 100,
      pending: 20,
      status: 'indexing', 
      updated: 0
  }
}

-------- Query 3 --------
{ 
  building: {
      initial: 100,
      pending: 4,
      status: 'indexing', 
      updated: 16
  }
}

-------- Query 4 --------
{
  definition: 'DEFINE INDEX by_email ON CLASS user FIELDS email UNIQUE SCOPE BASE',
  state: 'ready',
  system: false,
  building: {
      status: 'ready'
  }
}
```

Для синхронно построенных базовых индексов `state` принимает `ready`, `stale` или
`prepared_for_removal`; `building.status` повторяет это состояние для совместимости с будущим
асинхронным build progress. Отсутствующий, неоткрываемый или checksum-corrupt secondary index
остаётся в schema catalog со `state: 'stale'`, а запрос использует scan fallback. Повреждение
primary/base storage остаётся фатальным и не маскируется `INFO`.

### 8.8. SHOW CHANGES

Возвращает историю изменений (changefeed). 
Требует предварительного определения changefeed через `DEFINE`.

```
SHOW CHANGES FOR CLASS @class_name
	SINCE @timestamp | @versionstamp
	[ LIMIT @number ]
;
```

### 8.9. KILL

```
KILL @id;
```

Прерывает выполнение live-запроса по идентификатору.

```
KILL $query_id;
```

### 8.10. REBUILD INDEX

Перестраивает индекс.
Применяется при деградации качества, в частности для HNSW-индексов после большого числа обновлений.

```
REBUILD [
	INDEX [ IF EXISTS ] @name ON CLASS @class
]
```

---

## 9. DDL: DEFINE / REMOVE / ALTER

### 9.1. DEFINE CLASS

```gql
DEFINE CLASS [ OVERWRITE | IF NOT EXISTS ] @name
	TYPE NODE | EDGE
	[ SCHEMAFULL | SCHEMALESS ]
	[ CHANGEFEED @duration [ INCLUDE ORIGINAL ] ]
    [ COMMENT @string ];
```

`TYPE` обязателен. Имя и kind class immutable: `OVERWRITE` может менять schema mode,
changefeed/comment и зависимые definitions, но не переименовывать class и не менять `NODE` на
`EDGE` либо обратно. `SCHEMALESS` разрешает необъявленные поля; `SCHEMAFULL` требует
`DEFINE FIELD`. Отдельные `AS SELECT` classes не вводятся: query result/materialized graph
выражаются существующими query и `BUILD GRAPH` контрактами.

Текстовая часть `RecordId.class` непосредственно определяет class. Отдельного data membership в
`SchemaCatalog` нет; membership относится только к участию global atom в graph.

### 9.2. DEFINE FIELD

```
DEFINE FIELD [ OVERWRITE | IF NOT EXISTS ] @name ON CLASS @class
	[ TYPE @type [ FLEXIBLE ] ]
	[ REFERENCE 
		[ ON DELETE REJECT | 
			ON DELETE CASCADE | 
			ON DELETE IGNORE |
			ON DELETE UNSET | 
			ON DELETE THEN @expression ]
	]
	[ DEFAULT [ALWAYS] @expression ]
  [ READONLY ]
	[ VALUE @expression ]
	[ ASSERT @expression ]
  [ COMMENT @string ]
;
```

Для computed поля:
```
DEFINE FIELD [ OVERWRITE | IF NOT EXISTS ] @name ON CLASS @class
	COMPUTED @expression
	[ TYPE @type ]
  [ COMMENT @string ]
```

- `FLEXIBLE`: в `SCHEMAFULL` class каждый `object` по умолчанию имеет жёсткую схему, определённую с помощью вложенных `DEFINE FIELD`; `FLEXIBLE` позволяет ослабить это правило
- `VALUE`: вычисляется только при изменении (создании или обновлении), но не пересчитывается во время `SELECT` запроса, который не изменяет запись; игнорирует попытки присвоить любое другое значение.
- `COMPUTED`: computed-значение, вычисляемое только при выборе данных и их возврате клиенту.
- `DEFAULT`: значение по умолчанию при создании записи, если оно `none` (но не `null`); добавление `ALWAYS` задаёт это поведение и для обновления записи.
- `ASSERT`: предикат целостности; нарушение — runtime error.
- `READONLY`: поле нельзя изменить после создания.
- Операторы определения полей выполняются в алфавитном порядке имён полей.
- Доступ к другим полям записи осуществляется по их именам; доступ к значению текущего поля - через параметр `$value`, ко всей текущей записи - через параметр `$this`.
- Возможно также установить тип для поля `id`, выбирая из всех доступных вариантов.
- `ASSERT` и `DEFAULT` работают для поля `id`, но `DEFAULT ALWAYS` недопустим.
- `VALUE`, `REFERENCE`, `COMPUTED`, `READONLY`, `FLEXIBLE` и неключевые `TYPE` предложения запрещены для `id`, что также относится к `ALTER FIELD` операторам.
- Поле, являющееся ссылкой на запись (тип `record`, `option`, `array>`, и т. д.) может быть определено как `REFERENCE`: любая связанная с записью сущность сможет определить собственное поле типа `references`, которое будет учитывать входящие ссылки.

### 9.3. DEFINE PARAM

Глобальный параметр базы данных.

```
DEFINE PARAM [ OVERWRITE | IF NOT EXISTS ] $@name VALUE @value
    [ COMMENT string ]
;
```

### 9.4. DEFINE FUNCTION

```
DEFINE [ PURE | STABLE | IMPURE ] [ COST ] FUNCTION 
    [ OVERWRITE | IF NOT EXISTS ] 
    fn::@name ( [ ( @argument:@type ... ) ] )  [-> @return_type]
    { [@query] [ RETURN @returned ] }
    [ COMMENT @string ]
;
```

- `PURE` — функция детерминирована, не имеет побочных эффектов; планировщик может переставлять и кешировать вызовы.
- `STABLE` (подкласс PURE) — функция детерминирована в рамках одного запроса, но не между запросами (например, `time::now()` закреплено на время запроса).
- `IMPURE` — функция имеет побочные эффекты; планировщик фиксирует порядок.
- По умолчанию, если не указано: `IMPURE`.

### 9.5. DEFINE EVENT

События позволяют задать пользовательскую логику, которая выполняется при создании, обновлении или удалении записи.
Эти события автоматически запускаются в рамках текущей транзакции после изменения данных в записи,
предоставляя доступ к состоянию записи до `$before` и после `$after` изменения.

```
DEFINE EVENT 
    [ OVERWRITE | IF NOT EXISTS ] 
    @name ON CLASS @class
    [ ASYNC [ RETRY @n ] [ MAXDEPTH @n ] ]
    WHEN @expression 
    THEN @expression
;
```

Переменные контекста:
- `$event` — тип события: `CREATE`, `UPDATE`, `DELETE`, `RELATE`; можно использовать внутри `WHEN`.
- `$before` — состояние записи до изменения.
- `$after` — состояние после изменения.
- `$value` — запись после (для CREATE/UPDATE) или до (для DELETE).
- `$input` — данные, поступившие на вход оператора.

Без `ASYNC` событие выполняется синхронно в рамках текущей транзакции. 
Это обеспечивает согласованность, но может привести к увеличению задержки при операциях записи, 
если логика событий сложна или требует больших ресурсов.

С `ASYNC` событие выполняется независимо от вызвавшей транзакции, с настраиваемыми повторами (`RETRY`) и ограничением глубины рекурсии (`MAXDEPTH`).
Асинхронные события обрабатываются с интервалом, зависящим от настройки `FG_ASYNC_EVENT_PROCESSING_INTERVAL`, которая по умолчанию установлена на 5 секунд.
Уменьшение этого интервала сократит задержку между изменением документа и появлением связанных с ним событий, но при этом приведет к более частым опросам со стороны фонового работника.

Еще замечаний об особенностях асинхронных событий:
- Атомарность: событие ставится в очередь в рамках той же транзакции, что и изменение документа. Если транзакция завершается сбоем, событие не ставится в очередь.
- Согласованность: асинхронные события выполняются в рамках отдельной транзакции, не связанной с исходным изменением. Они учитывают состояние базы данных на момент выполнения.
- Порядок обработки: события обычно обрабатываются в том порядке, в котором они были созданы, хотя в рамках одного пакета возможна параллельная обработка.

Предложение `MAXDEPTH` используется для указания максимального количества повторных вызовов асинхронного события. 
Число, следующее за ним, может варьироваться от 0 до 16. 
По умолчанию для `MAXDEPTH` установлено значение 3, поскольку события, определяемые другими событиями, 
которые приводят к созданию записи, на более высоких уровнях могут быстро выйти из-под контроля.

Предложение `RETRY` подходит для событий, которые могут завершиться неудачей, но могут завершиться успехом при последовательных попытках.

### 9.6. DEFINE INDEX

Используется для определения индексов для оптимизации производительности запросов.
Индекс может состоять из одного или нескольких полей class и обеспечивать соблюдение ограничения уникальности.
Если вы не хотите, чтобы индекс обеспечивал соблюдение ограничения уникальности, то выбранные вами поля для индекса
должны иметь высокую степень кардинальности, то есть большое разнообразие данных в записях индексируемого class.

```
DEFINE INDEX [ OVERWRITE | IF NOT EXISTS ] @name
    ON CLASS @class
    (
        COUNT
      | ( FIELDS | COLUMNS ) @fields
        [ UNIQUE
          | FULLTEXT ANALYZER @analyzer [ BM25 [(@k1, @b)] ] [ HIGHLIGHTS ]
          | HNSW DIMENSION @dimension [ TYPE @type ] [ DIST @distance ] [ EFC @efc ] [ M @m ]
          | DISKANN DIMENSION @dimension [ TYPE @type ] [ DIST @distance ] [ DEGREE @degree ] [ L_BUILD @l_build ] [ ALPHA @alpha ] [ HASHED_VECTOR ]
          | GEOMETRY [ TYPE @geometry_type ]
        ]
    )
    SCOPE BASE | SCOPE EFFECTIVE IN GRAPH @graph
    [ COMMENT @string ]
    [ CONCURRENTLY ]
    [ DEFER ]
    [ WITH ( @build_option = @value [, ...] ) ]
;
```

#### Системные (неявные) индексы

Standard/Unique/Count index обязан явно выбрать scope. `BASE` индексирует global base values;
`EFFECTIVE IN GRAPH` строит полный индекс по memberships и effective values одного graph.

Часть индексов создаётся и поддерживается движком автоматически; их не нужно (и нельзя) объявлять через `DEFINE INDEX`, но они видны в `INFO FOR CLASS` / `INFO FOR INDEX` и участвуют в планировании наравне с пользовательскими:

| Индекс          | Назначение                                                                | Когда создаётся                                                                 |
|-----------------|---------------------------------------------------------------------------|---------------------------------------------------------------------------------|
| `primary`       | Глобальный `RecordId → atom`; обеспечивает уникальность `id` внутри class | Один engine-owned index AtomStore                                               |
| `class`         | Перечисление global atoms одного class                                    | Каталожная identity при `DEFINE CLASS`                                          |
| `membership`    | `GlobalAtomId → graph-local occurrence` и обратный список occurrences     | Engine-owned при создании graph/AtomStore                                       |
| `label`         | Перечисление occurrences по graph-local label                             | Каталожная identity при создании graph; entries заполняются лениво              |
| `edge-endpoint` | Инцидентность вида V–E / E–V / E–E для режима `ATOM` (AtomWalk)           | При первом ребре, чей endpoint сам является ребром                              |
| `adjacency`     | Смежность `out`/`in`/`both`                                               | Каталожная identity при создании graph; физически встроена в hot/overflow pages |

Удалить системный индекс нельзя; `REBUILD INDEX` к ним применим.

`CONCURRENTLY` — строит индекс без блокировки base class или выбранного effective graph view.
Оператор вернется немедленно, и вы сможете отслеживать процесс создания индекса, выполнив оператор `INFO`.

`CONCURRENTLY` — общий модификатор длительных обслуживающих операций, а не только построения индекса. Он применим к `DEFINE INDEX`, `REBUILD INDEX` и операциям уплотнения (`COMPACT`). Семантика везде одинакова: оператор ставит фоновое задание и возвращает управление немедленно; ход выполнения виден через `INFO`, задание прерывается через `KILL` по его идентификатору. Обычные запросы (`SELECT`, `MATCH`, `TRAVERSE`) не выполняются с `CONCURRENTLY` — для них есть `TIMEOUT` и отмена на уровне сессии.

Процесс индексирования состоит из двух этапов: первичного и обновления.

1. Начальный этап: На этом этапе Ferros-Grynn индексирует все существующие записи. Количество проиндексированных записей отображается в свойстве `initial`. Пока выполняется этот этап, все новые вставки, обновления и удаления отслеживаются как `pending`.
2. Этап обновления: после завершения начального этапа Ferros-Grynn приступает к индексированию ожидающих обработки записей, накопленных на начальном этапе. На этом этапе:
   - Количество `initial` остается стабильным.
   - По мере обработки этих записей значение `pending` должно постепенно уменьшаться, однако оно может временно увеличиться, если во время индексации будут внесены новые изменения.
   - Свойство `updated` указывает на количество ожидающих индексации записей на данном этапе.

После завершения обоих этапов статус индекса меняется на `ready`, что означает, что индекс автоматически обновляется в рамках той же транзакции, которая выполняет вставку, обновление или удаление записей.

`DEFER` — откладывает обновление индекса.

Обновление индексов в Ferros-Grynn происходит синхронно во время операций с документами. 
Это обеспечивает мгновенную согласованность, при которой все операции чтения возвращают результат последней записи. 
Однако это может стать узким местом при параллельном приеме больших объемов данных, приводя к увеличению задержки и конфликтам при записи, 
особенно при использовании полнотекстовых или векторных индексов.

В этом случае можно использовать предложение `DEFER`, если приемлема согласованность в конечном счёте, 
то есть ситуация, при которой при чтении в течение короткого периода могут возвращаться устаревшие данные, 
но в конечном счёте данные будут соответствовать последней записи. 
Индекс с этим предложением будет помещён в постоянную фоновую очередь, что позволит разделить приём данных и индексацию.

Поскольку уникальные индексы гарантируют, что записи, противоречащие индексу, существовать не будут, 
предложение `UNIQUE` нельзя использовать вместе с `DEFER`.

Типы индексов:
#### Standard (non-unique) index
Индекс без каких-либо специальных условий позволяет индексировать атрибуты,
которые могут иметь неуникальные значения, что способствует эффективному извлечению данных.
Неуникальные индексы помогают индексировать часто встречающиеся в запросах данные,
которые не требуют уникальности, например теги категорий или индикаторы статуса.

#### Unique index
Обеспечивает уникальность каждого значения в индексе.
Уникальный индекс помогает обеспечить уникальность записей, предотвращая дублирование таких полей,
как идентификаторы пользователей, адреса электронной почты и другие уникальные идентификаторы.

#### Count index 
Этот индекс применяется для подсчёта записей class в выбранном scope.
Он используется вместе с `count()` функцией и `GROUP ALL` внутри запроса.
Без Count index `count()` перебирает atoms base class либо memberships effective graph view.
Поскольку Count index объявляется для class/scope целиком, он не использует `FIELDS` / `COLUMNS`.

#### Full-text search (FULLTEXT) index
Обеспечивает эффективный поиск по текстовым данным, поддерживая расширенные
функции сопоставления текста, такие как поиск по контексту и выделение ключевых слов.
- Ключевое слово и `FULLTEXT` включают полнотекстовый поиск по указанному столбцу.
- `BM25`: алгоритм ранжирования, используемый для оценки релевантности.
- `HIGHLIGHTS`: Позволяет выделять ключевые слова в результатах поиска при использовании функции `search::highlight`.
- `FIELDS`: индекс полнотекстового поиска можно использовать только для одного поля за раз. 
  Чтобы использовать полнотекстовый поиск для нескольких полей, используйте отдельный `DEFINE INDEX` оператор для каждого из них.

#### Индексы векторного поиска (Vector search indexes):
Векторные поисковые индексы поддерживают эффективные операции поиска k-ближайших соседей (kNN)
и приблизительного поиска ближайшего соседа (ANN), которые играют ключевую роль в поиске совпадений
в сложных многомерных наборах данных и типах данных.

При определении векторного индекса с помощью `HNSW` вы можете указать типы, в которых будет храниться вектор.
Предложение `TYPE` является необязательным и может использоваться для указания типа данных вектора.

Поддерживаются следующие типы данных: `F64`, `F32`, `I64`, `I32`, `I16`. Векторы по умолчанию имеют тип F64.

HNSW (иерархически управляемый малый мир) — метод, использующий графовый подход для эффективной навигации и поиска в многомерных пространствах.
Несмотря на то что это приблизительный метод, он обеспечивает высокую производительность при оптимальном
соотношении скорости и точности, что делает его идеальным для работы с очень большими наборами данных.

`DIMENSION` задаёт размерность; `TYPE` — формат данных; `DIST` — метрика: `COSINE`, `EUCLIDEAN`, `MANHATTAN`, `MINKOWSKI` или пользовательская функция-метрика.

Параметр `M` — максимальное количество подключений на элемент.
Он определяет максимальное количество двунаправленных связей (соседей) для каждого узла на каждом уровне графа, кроме самого нижнего. 
Этот параметр определяет связность и общую структуру сети. Чем выше значение MM, тем точнее поиск, но тем больше используется памяти и тем дольше строится сеть.

Параметр `EFC` (конструкция EF) расшифровывается как "коэффициент исследования при построении." 
Этот параметр определяет размер динамического списка кандидатов на роль ближайших соседей на этапе построения графа. 
Чем больше значение efConstruction, тем тщательнее проводится построение, что повышает качество и точность поиска, но увеличивает время построения. 
Значение по умолчанию составляет 150.

Параметр `M0` — максимальное количество подключений на самом низком уровне.
Аналогично `M`, но только для нижнего слоя (базового слоя) графа. Этот слой содержит фактические точки данных. 
Значение `M0` часто удваивают по сравнению со значением `M`, чтобы повысить производительность поиска и связность базового слоя за счет увеличения потребления памяти.

Параметр `LM` — множитель для генерации уровней.
Используется для определения максимального уровня ll для нового элемента при его вставке в иерархическую структуру. 
Используется в формуле $l \leftarrow \lfloor -\ln{U(0, 1)} \cdot mL \rfloor$, где $U(0, 1)$ — равномерно распределенная случайная величина в диапазоне от 0 до 1. 
Этот параметр влияет на распределение элементов по разным уровням, а значит, на общий баланс и эффективность структуры поиска.

Вы можете указать только `TYPE`, `M` и `EFC`. Ferros-Grynn автоматически вычисляет `M0` и `LM`, 
используя наиболее подходящее значение. Если значения не указаны, `M` и `EFC` устанавливаются на 12 и 150 соответственно.

Векторный поиск `HNSW` по умолчанию использует ограниченный кэш памяти, что снижает скачки потребления памяти и повышает стабильность работы под нагрузкой. 
Размер кэша по умолчанию составляет 256 МБ, его можно изменить с помощью настройки `FG_HNSW_CACHE_SIZE`.

#### DiskANN

[DiskANN](https://papers.nips.cc/paper_files/paper/2019/file/09853c7fb1d3f8ee67a61b6bf4a7f8e6-Paper.pdf)
индексы обеспечивают на основе дисковых данных построение приблизительных графов ближайших
соседей; реализация сверяется с официальным [Microsoft/DiskANN](https://github.com/microsoft/DiskANN).
Они предназначены для очень больших наборов данных для встраивания, где хранение полного графа в оперативной памяти (как в случае с HNSW) нецелесообразно: 
векторы и структура графа сохраняются в KV-хранилище и распределяются по ограниченному кэшу, что позволяет пожертвовать некоторой задержкой ради гораздо большего рабочего набора.

Параметр `DIST` (default `EUCLIDEAN`): Для `DISKANN` действительны только `EUCLIDEAN`, `COSINE`, `INNER_PRODUCT` и `COSINE_NORMALIZED`.

Параметр `TYPE` (default `F32`): Должно быть одно из `F32`, `F16`, `I8`, `U8`; `COSINE_NORMALIZED` дополнительно требует `F32` или `F16`.

Параметр `DEGREE` (default 64): Целевая максимальная степень графа (> 0).

Параметр `L_BUILD` (default 100): Размер списка поиска при построении (> 0).

Параметр `ALPHA` (default 1.2): Параметр отсева в DiskANN.

`HASHED_VECTOR` допустим только для `DISKANN` и меняет representation точного vector dictionary,
но не ANN graph, distance, ranking или recall. Без флага lookup использует полные canonical
vector bytes; с флагом — `RapidHashV3(bytes, seed=0)` и bounded collision range с обязательным
сравнением полных bytes. `-0.0` канонизируется в `+0.0`, NaN/infinity отклоняются. Изменение
флага создаёт новую `SchemaVersion` и COW rebuild. Hash collision никогда не считается equality,
а exhaustion collision-scan budget возвращает resource error.

В запросах используется **та же** форма оператора `KNN`, что и в `HNSW`: `<| K |>`,
`<| K, DISTANCE |>` или `<| K, EF SEARCH_LIST_SIZE |>`. Если метрика совместима с индексом,
оптимизатор может выбрать `DISKANN`; `EF` задаёт размер динамического списка кандидатов поиска.

HNSW и DiskANN имеют разные immutable versioned base side-files и общий exact WAL-delta contract
с inserts/deletes/tombstones. Несовместимый fingerprint делает индекс stale и требует rebuild;
planner использует exact fallback. Формат и recovery определены в `disk_storage_spec.md` §20.6.

**Модель памяти и размер кэша**:

В то время как граф `HNSW` может храниться в оперативной памяти, `DISKANN` использует собственный
disk-resident side-file с bounded page cache. Граф, его списки смежности и векторы элементов с
полной точностью сохраняются в этом side-file и exact delta.
Только ограниченный кэш в оперативной памяти содержит «горячий» рабочий набор, в то время как «холодные» узлы и векторы передаются из KV-хранилища по запросу. 
Таким образом, резидентная память индекса `DISKANN` ограничена размером кэша, а не размером набора данных, что делает его подходящим для задач, требующих больше памяти, чем доступно в системе.

Векторы хранятся и ищутся с полной точностью, без квантования произведения или других видов сжатия. 
Единственным фактором, влияющим на размер каждого вектора, является выбранный `TYPE`.

Кэш — это единый бюджет, общий для всех индексов `DISKANN` в процессе. 
По умолчанию он составляет 256 МБ и может быть ограничен с помощью настройки `FG_DISKANN_CACHE_SIZE`. 
Значение представляет собой целое число байт.

#### Path Index

Ускоряет запросы с переменной длиной пути `[E*min..max]->` и shortest-path запросы между произвольными парами вершин.

```
DEFINE INDEX [ OVERWRITE | IF NOT EXISTS ] @name
    ON [ GRAPH ] @graph
    PATH
    [ WEIGHTED | COST fn::@function ]
    [ EDGES @edge_label ]
    [ ALGORITHM LANDMARK_SSSP(@n) 
        | PATTERN_CACHE 
        | ON_DEMAND 
    ]
    [ DIRECTION OUT | IN | BOTH ]
    [ PATTERNS @pattern_list ]
    [ COMMENT @string ]
    [ CONCURRENTLY ]
    [ DEFER ]
    WITH ( MAX_BYTES = @bytes )
;
```

**Алгоритмы:**

- `LANDMARK_SSSP(n)`, `n=1..=256`, хранит направленные расстояния от и до landmarks и применяет
  admissible ALT lower bound
  `max(0, max_l(d(l,t)-d(l,v)), max_l(d(v,l)-d(t,l)))`. Запрос остаётся точным A* с повторным
  открытием улучшенных states. Без modifier индексирует hop count; `WEIGHTED` — встроенный
  неотрицательный finite edge weight; `COST fn` — ту же versioned PURE `(edge)->number`
  additive функцию. Stateful cost поддерживается только ON_DEMAND.

- `PATTERN_CACHE` кэширует exact binding rows, named paths и bag multiplicity для конечного
  списка parameter-free bounded `PATTERNS`. Cache identity включает alpha-normalized IR,
  GraphId, cost model, literals и versioned dependencies. Он не хранит `SubgraphDir`; `RETURN
  GRAPH` выполняет обычную materialization. Любое зависимое data/schema/function изменение
  атомарно делает definition stale.

- `ON_DEMAND` — BFS/Dijkstra при каждом запросе. `FG_PATH_MAX_DEPTH` применяется. Рекомендуется для изменяемых графов.

**Автоматический выбор:** ON_DEMAND.

#### Reachability index

Предвычисляет и кэширует достижимость между атомами графа.
Ускоряет запросы вида «достижима ли B из A?» и «все вершины, достижимые из A».

```
DEFINE INDEX [ OVERWRITE | IF NOT EXISTS ] @name
    ON [ GRAPH ] @graph
    REACHABILITY
    [ EDGES @edge_label ]
    [ ALGORITHM TREE_COVER
        | GRAIL [ (@k) ]
        | FERRARI [ (@max_ranges) ]
        | BFL [ (@bits) ]
        | CHAIN_COVER
        | ON_DEMAND 
    ]
    [ DIRECTION OUT | IN | BOTH ]
    [ COMMENT @string ]
    [ CONCURRENTLY ]
    [ DEFER ]
    [ WITH ( MAX_BYTES = @bytes ) ]
;
```

`EDGES @edge_label` ограничивает индекс конкретным типом рёбер; без — индексируются все рёбра.

**Алгоритмы:**

- `TREE_COVER` — DFS-интервальные метки на остовном дереве. 
  Каждая вершина получает метку `[in_ts, out_ts]`. 
  Достижимость B из A: `in_A ≤ in_B AND out_B ≤ out_A`.
  Пространство O(n), запрос O(1), без false positives.
  Допустим только для TREE/ROOTED_TREE/FOREST; generic DAG отклоняется.
  Инвалидируется при мутациях.

- `GRAIL(k)` — k детерминированно seeded DFS-обходов, каждый порождает `[in_ts,out_ts]`.
  Нарушение containment хотя бы в одном измерении даёт `No`; только прохождение всех k даёт
  `Maybe` и exact traversal. `k ∈ [1,32]`, default 2.
  Возможны false positives (гарантирует отсутствие false negatives).
  Применяется к DAG; для графов с циклами требуется предварительное сжатие по SCC.
  Качество (доля false positives) сильно зависит от k: чем больше k, тем меньше ложных срабатываний, но больше размер индекса.

- `FERRARI(max_ranges)`, default 4, range `1..=255`, хранит bounded exact/approximate interval
  cover. Exact hit даёт `Yes`, miss `No`, approximate hit `Maybe` с guided exact traversal.

- `BFL(bits)`, default 160, range `64..=4096` кратно 8, хранит propagated Bloom labels SCC,
  а не landmarks. Нарушение `out(target) ⊆ out(source)` либо `in(source) ⊆ in(target)` даёт
  `No`; иначе `Maybe` и exact traversal. False negatives запрещены.

- `CHAIN_COVER` хранит deterministic chain decomposition и dense
  `lowest_reachable_position[chain]`; lookup точный O(1), storage
  `O(component_count × chain_count)`. Для explicit выбора обязателен `MAX_BYTES`.

- `ON_DEMAND` — нет предобработки; каждый запрос выполняет BFS/DFS. 
  O(1) хранилище, O(V+E) запрос. Единственный корректный вариант для графов с частыми мутациями топологии.

Для произвольного directed graph сначала вычисляются SCC и condensation DAG. Ответ индекса —
`Yes|No|Maybe`; `Maybe` всегда запускает exact bounded traversal.

**Автоматический выбор алгоритма** (без ALGORITHM):

| Тип/бюджет                      | Выбранный алгоритм |
|---------------------------------|--------------------|
| TREE / ROOTED_TREE / FOREST     | TREE_COVER         |
| Прочие, CHAIN_COVER помещается  | CHAIN_COVER        |
| Иначе FERRARI(4) помещается     | FERRARI(4)         |
| Иначе BFL(160) помещается       | BFL(160)           |
| Ни один side-file не помещается | ON_DEMAND          |

`AUTO` требует `WITH (MAX_BYTES=n)`; explicit algorithm не подменяется молча при нехватке
бюджета. Staleness, а не сам факт mutability, определяет возможность использования snapshot
artifact.

#### Neighborhood index

Предвычисляет k-хоп окрестности вершин. 
Ускоряет `k_hop_neighbourhood(a, k)` и MATCH-паттерны вида `[E*1..k]->`.

```
DEFINE INDEX [ OVERWRITE | IF NOT EXISTS ] @name
    ON [ GRAPH ] @graph
    NEIGHBOURHOOD(@k)
    [ EDGES @edge_label ]
    [ ALGORITHM EXACT 
        | SKETCH 
        | LANDMARK(@n) 
        | ON_DEMAND 
    ]
    [ DIRECTION OUT | IN | BOTH ]
    [ COMMENT @string ]
    [ CONCURRENTLY ]
    [ DEFER ]
;
```

Параметр `@k` — максимальный предвычисленный радиус (int ≥ 1).
k = 1 — инцидентность, бесплатно (данные из NodeHotSlot adjacency, не требует индекса).

**Алгоритмы:**

- `EXACT` — явные списки соседей для каждой вершины на каждом расстоянии 1..k. 
  Запрос O(1). Пространство экспоненциально растёт с k и avg_degree.
  Применение: k ≤ 3, разреженные графы (avg_degree ≤ 10).

- `SKETCH` — приближённый подсчёт через HyperLogLog. 
  Отвечает на «сколько вершин в k-окрестности?» с погрешностью ~2%. 
  Не поддерживает точное перечисление членов окрестности. Пространство O(n·k·64) байт.

- `LANDMARK(n)` — n landmark-вершин выбираются эвристически (высокая степень, центральность). 
  Для них: точные окрестности. Для остальных: BFS с ранним отсечением через landmark-расстояния. 
  Компромисс между памятью и точностью.

- `ON_DEMAND` — BFS при каждом запросе. O(V+E) запрос.

**Автоматический выбор:**

| k   | avg_degree | Алгоритм                  |
|-----|------------|---------------------------|
| 1   | любой      | (adjacency, без хранения) |
| 2   | ≤ 10       | EXACT                     |
| 2   | > 10       | ON_DEMAND                 |
| ≥ 3 | ≤ 5        | EXACT                     |
| ≥ 3 | > 5        | ON_DEMAND                 |

### 9.7. DEFINE ANALYZER

```
DEFINE ANALYZER [IF NOT EXISTS | OVERWRITE] name
    [ FUNCTION fn::custom_fn ]
    [ TOKENIZERS tokenizers ]
    [ FILTERS filters ]
    [ COMMENT string ]
;
```

Подробнее — в §20.

### 9.8. DEFINE GRAPH

```
DEFINE GRAPH [IF NOT EXISTS | OVERWRITE] @name
    [ TYPE @type ]
    [ ROOT @root_atom | ROOTS @root_atoms_array ]
    [ VALIDATE ON_MUTATION | DEFERRED ]
    [ CONTENT @object ]
    [ COMMENT @string ]
;
```

Создаёт именованный граф. `CONTENT` обязан вычисляться в object и задаёт catalog-owned
пользовательские свойства графа. При создании можно указать один из типов графов.
В случае tree, rooted_tree и forest можно указать корневые атомы.
Также можно определить в какой момент времени будет производиться проверка ограничений типа графа: во время мутации или во время выборки.

### 9.9. REMOVE

```
REMOVE [
	DATABASE [ IF EXISTS] @name
	| EVENT [ IF EXISTS ] @name ON CLASS @class
	| FIELD [ IF EXISTS ] @name ON CLASS @class
	| INDEX [ IF EXISTS ] @name ON ( CLASS @class | GRAPH @graph )
    | ANALYZER [ IF EXISTS ] @name
    | FUNCTION [ IF EXISTS ] fn::@name
    | PARAM [ IF EXISTS ] $@name
	| CLASS [ IF EXISTS ] @name
	| GRAPH [ IF EXISTS ] @name
]
;
```

### 9.10. ALTER

Оператор ALTER используется для изменения глобальных параметров, конфигураций сущностей, событий, определений схемы и индексов.

Формы ниже являются утверждённым контрактом полного draft-профиля; их implementation остаётся
dependency-ordered по активному плану. Точный синтаксис задаёт `gql_grammar.md` §9.10.

Каждый `ALTER` содержит хотя бы одно действие. `[...]` после группы действий ниже означает
повтор группы; повтор одного свойства и несовместимые действия для него являются семантической
ошибкой.

#### ALTER ANALYZER

Используется для изменения существующего анализатора.

```
ALTER ANALYZER [ IF EXISTS ] @name
  (
      FUNCTION fn::@function | DROP FUNCTION
    | TOKENIZERS @tokenizer, ... | DROP TOKENIZERS
    | FILTERS @filter, ... | DROP FILTERS
    | COMMENT @string | DROP COMMENT
  ) [...]
;
```

#### ALTER DATABASE

Используется для сжатия текущей базы данных.

```
ALTER DATABASE COMPACT;
```

#### ALTER EVENT

Используется для изменения существующего определенного события.

```
ALTER EVENT [ IF EXISTS ] @name ON CLASS @class
  (
      ASYNC [ RETRY @retry ] [ MAXDEPTH @max_depth ] | DROP ASYNC
    | WHEN @condition | DROP WHEN
    | THEN @action, ... | DROP THEN
    | COMMENT @string | DROP COMMENT
  ) [...]
;
```

#### ALTER FIELD

Используется для изменения или полного удаления условий поля class.

```
ALTER FIELD [ IF EXISTS ] @name ON CLASS @class
(
    DROP TYPE |
    DROP FLEXIBLE |
    DROP READONLY |
    DROP VALUE |
    DROP ASSERT |
    DROP DEFAULT |
    DROP COMMENT |
    DROP REFERENCE |
    FLEXIBLE |
    READONLY |
    REFERENCE |
    TYPE @type |
    VALUE @value |
    ASSERT @expression |
    DEFAULT [ ALWAYS ] @expression |
    COMMENT @string
) [...]
[ WITH MIGRATION ] [ CONCURRENTLY ]
;
```

#### ALTER FUNCTION

Используется для изменения существующей функции

```
ALTER FUNCTION [ IF EXISTS ] fn::@name
  (
      ( [ $argument: @type, ... ] )
    | -> @type | DROP RETURNS
    | { @query ... }
    | COMMENT @string | DROP COMMENT
  ) [...]
;
```

#### ALTER INDEX

Используется для изменения существующего индекса

```
ALTER INDEX @name ON ( CLASS @class | GRAPH @graph )
    ( COMMENT @string | PREPARE REMOVE | DROP COMMENT )
;
```

`PREPARE REMOVE` изменяет индекс, чтобы подготовить его к удалению.
Этот оператор позволяет перейти к этапу, на котором индекс выведен из эксплуатации (подготовлен к удалению), но еще не удален. 
На этом этапе `SELECT` запросы вместе с предложением `EXPLAIN` позволяют отслеживать производительность запросов без использования индекса.
Если удаление индекса больше не требуется, его можно восстановить в работоспособном состоянии с помощью оператора `REBUILD INDEX`.

#### ALTER PARAM

Используется для изменения существующего параметра

```
ALTER PARAM [ IF EXISTS ] $name
  ( VALUE @value | COMMENT @string | DROP COMMENT ) [...]
;
```

Не поддерживает `DROP VALUE`, так как параметр без значения существовать не может.

#### ALTER CLASS

Изменяет существующий class без изменения его имени или `NODE|EDGE` kind.

```
ALTER CLASS [ IF EXISTS ] @name
    (
        DROP COMMENT
      | DROP CHANGEFEED
      | COMPACT
      | SCHEMAFULL
      | SCHEMALESS
      | CHANGEFEED @duration
      | COMMENT @string
    ) [...]
[ WITH MIGRATION ] [ CONCURRENTLY ]
;
```

`COMPACT` выполняет сжатие base/effective index и AtomStore-страниц выбранного class.
Порядок действий не превращает один statement в последовательность промежуточных схем.

`ALTER CLASS` и `ALTER FIELD` принимают `[WITH MIGRATION] [CONCURRENTLY]`. Без migration все
существующие данные проверяются bounded до publication; нарушение ничего не меняет. Migration
разрешает только standard casts, DEFAULT fill, VALUE recompute и ASSERT/REFERENCE validation,
строит недостижимые generations и после повторной revision check публикует их одним DB commit.

#### ALTER GRAPH

```text
ALTER GRAPH [IF EXISTS] @graph
  ( TYPE @graph_constraint
  | ROOT @record
  | ROOTS @array
  | DROP ROOTS
  | VALIDATE ON_MUTATION
  | VALIDATE DEFERRED
  | COMMENT @string
  | DROP COMMENT ) [...]
;
```

TYPE/root actions только валидируют текущую topology; скрыто удалять edges, переписывать roots
или выполнять topology migration запрещено. `ALTER INDEX`, `REMOVE INDEX` и `INFO FOR INDEX`
имеют симметричные `ON CLASS`/`ON GRAPH` формы. Maintenance `COMPACT` не меняет SchemaVersion.

Один ALTER публикует одну catalog revision. Каждый непосредственно изменённый schema object
увеличивает собственный SchemaVersion ровно один раз; неизменившиеся dependencies — нет.
Duplicate/conflicting actions и revision race отклоняются до atomic publication; partial ALTER
невозможен.

### 9.11. ANALYZE

```text
ANALYZE (
    CLASS @class (BASE | EFFECTIVE IN GRAPH @graph)
  | GRAPH @graph
)
[FULL | APPROXIMATE [SAMPLE @count]]
[CONCURRENTLY]
;
```

Операция всегда читает полный source snapshot. FULL даёт exact counters/NDV и unsampled
histograms; APPROXIMATE сохраняет exact counters, но использует HLL p=12 и deterministic
reservoir (default 30000, `SAMPLE` range `1024..=1_000_000`). Default — FULL до 32 visible pages,
иначе `APPROXIMATE SAMPLE 30000`. Foreground ждёт и при revision race возвращает conflict;
CONCURRENTLY немедленно возвращает job ID, а race перепланирует job. DML не ждёт auto-analyze.

---

## 10. Clauses

### 10.1. WHERE

Фильтрация по булевому предикату. Работает в `SELECT`, `UPDATE`, `DELETE`, `UPSERT`, `MATCH`, `TRAVERSE`, а также в подзапросах и traversal-шагах, где это семантически допустимо.

```
WHERE age >= 18 AND status = "active"
```

### 10.2. FROM

Источник данных stage-13:

- class/record: `FROM person`, `FROM person:alice`, включая `IN GRAPH @name`;
- один именованный граф: `FROM GRAPH @sales [AS g]`;
- явный список графов: `FROM GRAPHS @sales, @fraud` либо
  `FROM GRAPHS [@sales, @fraud] AS g`;
- все видимые графы: `FROM GRAPHS [AS g]`;
- relation subquery: `FROM (SELECT ...) AS alias`.

Явный список графов сохраняет заданный порядок; `GRAPHS` без списка использует `GraphId` order.
Каждый граф образует отдельную исходную строку. Derived source всегда требует alias. Старые `,`
как union sources и `&` как cross join удалены: нетопологический cross product выражается
`CROSS JOIN`, а топологические отношения — `MATCH`/`TRAVERSE`.

### 10.3. WITH

Явный контроль доступа к индексам, отключение индексов и использования пользовательских функций стоимости.

Без hint optimizer сравнивает class/membership scan с одним совместимым secondary index path на
source. Stage 14 не пересекает secondary indexes: несколько имён после `WITH INDEX` являются
разрешённым candidate set, а не набором одновременно исполняемых итераторов.

```
[ WITH [ NOINDEX                         -- запретить user secondary indexes
         | INDEX [@source.]@index, ...   -- разрешённые candidates по sources
         | COST @const_fn       -- использовать пользовательскую функцию стоимости пути
       ]
]
```

Каждый явно названный source обязан получить один совместимый Ready-index из своего набора;
неqualified имя допустимо только при однозначном source. Forced hint проверяется до row pull.
`WITH COST @const_fn` применимо только для будущих `MATCH`/`TRAVERSE` cost-функций.

### 10.4. GROUP

Группировка и агрегация.

```
GROUP [ALL | BY @expressions]
```

`GROUP ALL` — сворачивает все строки в одну; применяется для глобальных агрегатов без группировки.

Aggregate без `GROUP` и `GROUP ALL` образуют одну группу. Projected non-aggregate expression
обязан входить в group key; `null` и `none` являются разными keys. `GROUP` и `ORDER BY` могут
использоваться вместе и всегда выполняются именно в этом порядке.

### 10.5. ORDER BY

Позволяет упорядочивать их по нескольким полям и вложенным полям.
Используйте ```ORDER BY```, чтобы указать список имен полей, разделенных запятыми,
по которым следует упорядочивать итоговые записи.

С помощью ключевых слов ```ASC``` и ```DESC``` можно указать, в каком порядке следует сортировать результаты:
по возрастанию или по убыванию.
Ключевое слово ```COLLATE``` можно использовать для сортировки текста в строковых значениях по правилам Юникода,
чтобы обеспечить единообразную сортировку в разных регистрах и на разных языках.
Наконец, ```NUMERIC``` можно использовать для корректной сортировки текста, содержащего числовые значения.

Также стоит отметить, что ```COLLATE``` игнорирует порядок символов в Юникоде.
Например, по умолчанию буква «á» стоит после «z» (при сортировке по Юникоду),
но при использовании ```COLLATE``` «á» стоит перед «z».

```
[ ORDER [ BY ] 
	@field [ COLLATE ] [ NUMERIC ] [ ASC | DESC ] [ NULLS FIRST | NULLS LAST ], ...
	| RAND()
	[ NULLS FIRST | NULLS LAST ]
]
;
```

- `NULLS FIRST` / `NULLS LAST` — позиция null-значений (по умолчанию `NULLS LAST`) для сортируемого поля.
- Если сортируемых полей несколько, можно указать позиции null-значений глобально.
- В этапе 14 `ORDER RAND()` является специальным physical ordering operator. Его ключ —
  versioned RapidHash V3 от statement seed и canonical stable row key. В `DETERMINISTIC` seed
  входит в replay tuple; в `BEST_EFFORT` создаётся новый process-local statement seed.
- `COLLATE` и `NUMERIC` распознаются, но остаются typed unsupported до этапа 21 и отклоняются до
  чтения source.

Применим только после `GROUP`, если он есть.

### 10.6. LIMIT / START

Ограничение и смещение.

```
LIMIT @number [ START [ AT ] @offset | START AFTER @cursor ]
```

Две формы позиционирования — offset и курсор.

#### `START AT @offset` — смещение

Классическое смещение: пропустить `@offset` строк. Простое, но имеет две известные проблемы:
- стоимость O(offset) — пропущенные строки всё равно вычисляются;
- «съезд» при конкурентных вставках/удалениях: вставка перед окном сдвигает границы, и строка может быть показана дважды или пропущена.

Пригодно для небольших смещений и для интерфейсов с произвольным переходом на страницу N.

#### `START AFTER @cursor` — keyset-курсор

Продолжение с позиции, зафиксированной предыдущей страницей. Стоимость не зависит от глубины пагинации, и результат устойчив к конкурентным изменениям вне окна.

`@cursor` — **непрозрачное** runtime-only значение типа `cursor`, которое клиент не должен
разбирать или конструировать. Оно возвращается как `QueryResult.continuation` и кодирует:
- значения ключей сортировки последней выданной строки,
- `stable_id` последней строки (для разрыва ничьих),
- `SnapshotId` запроса,
- хеш нормализованного запроса и `ORDER BY`.

```
-- Первая страница
SELECT id, name FROM person ORDER BY created_at DESC LIMIT 50;
-- В ответе: { result: [...], cursor: 'c:01KRMD...' }

-- Следующая страница
SELECT id, name FROM person ORDER BY created_at DESC LIMIT 50 START AFTER $cursor;
```

Правила:
- Курсор действителен только для запроса с **тем же** `ORDER BY` и тем же набором фильтров; иначе — ошибка `InvalidCursor`. Хеш запроса в курсоре обеспечивает эту проверку.
- Требуется детерминированный полный порядок. Если `ORDER BY` не задан или не является полным (ключи сортировки не различают строки), движок **дополняет** его каноническим `RecordId = (class, id)`. Без полного порядка позиция курсора не определена.
- По умолчанию курсор продолжает чтение **в том же снимке** (`SnapshotId` внутри курсора): страницы одной ленты консистентны между собой. Если снимок уже собран сборщиком версий (старше `max_snapshot_age`), возвращается `SnapshotExpired`; повторить с `START AFTER @cursor READ COMMITTED` — тогда чтение продолжится в свежем снимке с тем же ключом позиции, ценой возможного появления/исчезновения строк.
- Usable cursor создаёт только постоянная `GqlSession`; `Datastore::execute` не сохраняет
  межвызовное состояние. Курсор не персистентен и не переносим между session, базами и процессами.

Курсор рекомендуется как основная форма пагинации для больших выборок; `START AT` оставлен для совместимости и небольших смещений.

### 10.7. FETCH

Жадное или контролируемое разворачивание ссылок на записи.

Используется для получения связанных records или данных из других classes в рамках одного запроса.
Это особенно удобно, когда нужно собрать данные, связанные отношениями (ссылки на записи),
без необходимости выполнять несколько отдельных запросов.

- Эффективность: с помощью предложения ```FETCH``` можно сократить количество запросов,
  необходимых для получения связанных данных. Это может повысить производительность,
  особенно при работе со сложными моделями данных с множеством связей.
- Упрощенные запросы. Эта функция упрощает запросы, позволяя указывать связанные данные,
  которые будут извлекаться непосредственно в рамках одного запроса.
- Снижение нагрузки. Получение связанных данных в рамках одного запроса сокращает количество обращений к базе данных,
  что может уменьшить задержку и повысить общую скорость работы приложения.
- Согласованность. Получая связанные данные за один раз, вы гарантируете их согласованность и актуальность
  на момент получения, что снижает риск расхождений, которые могут возникнуть
  при получении данных с помощью отдельных запросов.

Вместо `record<T>` в результате возвращается полная запись.

```
FETCH @fields
```

### 10.8. SPLIT

Разворачивание массивов в строки результата.

Используется для разделения результатов запроса по определенному полю, особенно при работе с массивами.
Это полезно в тех случаях, когда нужно рассматривать каждый элемент массива как отдельную строку в наборе результатов.
Это может быть особенно полезно при анализе данных, когда нужно работать с отдельными элементами массива по отдельности.

```
SPLIT [ON] @field
```

```
CREATE user SET
    name = "John Doe",
    emails = ["john@example.com", "doe@example.com"];

-- Split the results by each value in the emails array
SELECT * FROM user SPLIT emails;

-- Output:
[
	{
		emails: 'john@example.com',
		id: user:unjgil312jvvxfbdj706,
		name: 'John Doe'
	},
	{
		emails: 'doe@example.com',
		id: user:unjgil312jvvxfbdj706,
		name: 'John Doe'
	}
]
```

### 10.9. OMIT

Исключение полей из результата.

Применяется к уже построенной проекции; hidden provenance/order key не изменяется.

```
OMIT @fields
```

### 10.10. TIMEOUT

Ограничение времени выполнения оператора.

```
TIMEOUT @duration
```

При превышении — транзакционная ошибка с откатом.

### 10.11. CONSTRAINTS

Определяет ограничения пути. Применим только к `MATCH` и `TRAVERSE`. 
Подробнее в [12.7](#127-path-constraints).

### 10.12. CONTAINS

Если рассматривать `CONTAINS` как оператор, то он выполняет проверку, принадлежит ли значение справа коллекции слева.

```
$array CONTAINS $elem;
$object CONTAINS $key;
-- etc.
```

Немного в стороне стоит случай в `MATCH`-запросах с гипер-рёбрами.
Поскольку гипер-рёбра могут соединять более двух атомов, то семантика `CONTAINS` схожа с семантикой оператора:
он проверяет, принадлежат ли атомы справа гипер-ребру слева.
Ключевое отличие от операторной версии в том, что для ориентированного гиперребра можно указать сторону:
- `HEAD @atoms` — искать в `source`;
- `TAIL @atoms` — искать в `target`;
- если модификатор поиска не указан, то поиск производится по всему массиву атомов ориентированного гипер-ребра.

### 10.13. KEEP LEVELPATH

```
KEEP LEVELPATH [ FOR @aliases_array ]
```

Записывает LevelPath в свойства создаваемого атома. Уровневый путь могут иметь не только рёбра, но и вершины.
Применимо как при создании атомов, так и при выборке атомов или путей в `MATCH` / `TRAVERSE`.

При `KEEP LEVELPATH`:
- если атом — вершина (мета-вершина) `v`, то в `v.levelpath` записывается цепочка от самого верхнего уровня вложенности графов
до текущего атома-вершины по вложенным подграфам;
- если атом — кросс-уровневое ребро `e`, то в `e.levelpath` записывается цепочка мета-вершин от endpoint на самом верхнем уровне вложенности
до endpoint на самом нижнем уровне вложенности, то есть набор мета-вершин, сквозь которые ребро `e` соединяет один endpoint с другим;
- если атом — кросс-уровневое гипер-ребро `he`, то оператор игнорируется и свойство `levelpath` не будет создано.
- если атом — НЕ кросс-уровневое ребро, то оператор игнорируется и свойство `levelpath` не будет создано. 
- если атом — НЕ кросс-уровневое ребро и НЕ атом-вершина, то оператор игнорируется и свойство `levelpath` не будет создано. 

Если атом (вершина или endpoint ребра) находится на верхнем уровне (не вложен), соответствующий LevelPath — пустой массив `[]`.

При использовании `KEEP LEVELPATH [ FOR @aliases_array ]` можно указать массив алиасов, для которых сохранять уровневый путь.
В противном случае уровневые пути будут сохранены для всех допустимых сущностей, участвующих в запросе.

### 10.14. EXPECT — условная запись

```
EXPECT @condition
```

Предусловие мутации: если `@condition` ложно, оператор **не применяется**, а транзакция прерывается со статусом `ConditionFailed`. Применим к `CREATE`, `UPDATE`, `UPSERT`, `DELETE`, `RELATE`.

Отличие от `WHERE`: `WHERE` **фильтрует** цели (не подошло — просто ничего не изменено, молча), `EXPECT` **требует** выполнения условия (не подошло — ошибка). Это разные задачи, поэтому клаузы независимы и могут сочетаться.

`@condition` вычисляется по **pre-image** — состоянию записи до изменения (то же, что `$before` в событиях). Для `CREATE` pre-image пуст, поэтому осмысленны только предикаты существования.

```
-- Оптимистическая блокировка (compare-and-set)
UPDATE person:alice SET name = "Alissa", version = 2
EXPECT version = 1;

-- Создать, только если записи ещё нет
CREATE person:bob CONTENT { name: "Bob" }
EXPECT NOT EXISTS;

-- Удалить, только если статус позволяет
DELETE order:123 EXPECT status = "draft";

-- Создать ребро, только если его ещё нет
RELATE (person:alice)-[:FRIEND]->(person:bob)
EXPECT NOT EXISTS;
```

`NOT EXISTS` / `EXISTS` — сокращения для проверки наличия целевой записи (или ребра для `RELATE`), доступные только внутри `EXPECT`.

**Семантика при нескольких целях.** Если оператор затрагивает несколько записей, условие проверяется для **каждой**; невыполнение хотя бы для одной прерывает весь оператор — частичное применение исключено.

**Зачем это нужно.** Без `EXPECT` compare-and-set выражается только через полноценную транзакцию с перечитыванием и полагается на уровень изоляции. `EXPECT` даёт то же самое одним оператором и в автотранзакции: атомарная проверка-и-запись, надёжная независимо от уровня изоляции. Аналог — conditional expressions в DynamoDB.

---

## 11. SELECT

`SELECT` — основной декларативный оператор чтения.

```
SELECT 
    [ ONLY ]
    VALUE @field | @fields [ AS @alias ] [ OMIT @fields ... ]
    FROM @source [ @joins ]
    [ WITH [ NOINDEX | INDEX @indexes ... ]]
    [ WHERE @conditions ]
    [ SPLIT [ ON ] @field, ... ]
    [ GROUP [ ALL | [ BY ] @expression, ... ] ]
    [ OMIT @fields ... ]
    [ FETCH @fields ... ]
    [ ORDER BY @expression [ ASC | DESC ] [ NULLS FIRST | LAST ], ... ]
    [ START [ AT ] @offset | START AFTER @cursor [ READ COMMITTED ] ]
    [ LIMIT [ BY ] @limit ]
    [ TIMEOUT @duration ]
    [ TEMPFILES ]
;
```

`SELECT ONLY` — ошибка, если запрос возвращает более одной строки (защита от случайного использования там, где ожидается единственная запись).

Stage-13 source/shape pipeline фиксирован как `source/join → WHERE → SPLIT → GROUP → projection
→ OMIT → FETCH → ORDER → START → LIMIT → ONLY`. Обычный SELECT возвращает `array<object>`,
`SELECT VALUE` — `array<Value>`.

Atom row содержит зарезервированные `id`, `class`, `kind`, `label` (только graph scope) и
`properties`. Объявленное `SCHEMAFULL`-поле читается знакомой формой `name`/`alias.name`, которая
эквивалентна `properties.name`; явная форма работает для любого свойства. Не объявленное в catalog
динамическое `SCHEMALESS`-свойство требует `properties.<name>`. Graph row содержит `id`, `name`,
`kind`, `storage_mode`, `revision`, `node_count`, `edge_count` и единый `properties` object;
пользовательские свойства графа не разворачиваются в корень.

Поддерживаются `INNER`/`LEFT`/`CROSS JOIN`; `JOIN` означает `INNER`. Первые два требуют `ON`,
`CROSS` запрещает `ON`. Falsey/`null`/`none` не образуют match, LEFT unmatched binding равен
`none`, bag semantics сохраняется. Derived, scalar, correlated scalar и single-column collection
subqueries разделяют snapshot/deadline/budget внешнего statement; scalar cardinality равна
0 → `none`, 1 → value/object, >1 → `SubqueryCardinalityViolation`.

Aggregates stage-13: `count`, `sum`, `avg`, `min`, `max`, `array::group`,
`math::variance_pop/stddev_pop/variance_samp/stddev_samp`. Variance использует Welford/Chan;
bare `variance/stddev` не вводятся. Barrier memory общая для join/group/order/distinct, default
256 MiB; превышение создаёт checked transient spill, `TEMPFILES` начинает с него сразу.

`DEFINE GRAPH ... CONTENT <object>` и `UPDATE GRAPH @name SET/UNSET properties.<path>` изменяют
catalog-owned properties через MVCC и не перестраивают graph generation/`GraphSuperblock`.

`TEMPFILES` начинает barrier-операторы с disk runs, не ожидая заполнения memory budget.
Каталог задаётся `QueryExecutionOptions::temporary_directory`; при его отсутствии local backend
использует database-owned `tmp/query`, а in-memory backend — отдельный каталог system temp.

`SELECT` может использоваться:
- как самостоятельный запрос;
- как источник в `FROM (SELECT ...)`;
- как источник traversal pipeline;
- как выражение в `LET`.

В `SELECT` запросе в качестве элемента `@fields` может выступать:
- идиома class source из `@targets`;
- `LET` параметры;
- вычисляемые выражения

Примеры:
```
-- Поля name и address определены в class person.
-- name.first это идиома.
-- name.last это алиас, который будет добавлен в поле name результата
-- string::upper(address) + "!!!" это вычисляемое выражение
SELECT
	properties.name.first,
	"Morgan Hitchcock" AS name.last,
	string::upper(properties.address) + "!!!" AS angry_address
FROM person;

-- Output
[
	{
		angry_address: '1 BAGSHOT ROW!!!',
		name: {
			first: 'Tobie',
			last: 'Morgan Hitchcock'
		}
	}
]
```

#### Расширенные примеры запросов:
```
-- Select nested objects/values
SELECT properties.address.city FROM person;

-- Select one item from an array
SELECT properties.address.coordinates[0] AS latitude FROM person;

-- Select unique values from an array
SELECT array::distinct(properties.tags) FROM article;

-- Select unique values from a nested array across an entire class
SELECT array::group(properties.tags) AS tags FROM article GROUP ALL;

-- Use mathematical calculations in a select expression
SELECT
	(( properties.celsius * 1.8 ) + 32) AS fahrenheit
	FROM temperature;

-- Return boolean expressions with an alias
SELECT properties.rating >= 4 as positive FROM review;

-- Select manually generated object structure
SELECT
	{ weekly: false, monthly: true } AS `marketing settings`
FROM user;

-- Select filtered nested array values
SELECT properties.address[WHERE active = true] FROM person;

-- Use the result of a subquery as a returned field
SELECT *, (SELECT * FROM events WHERE properties.type = 'activity' LIMIT 5) AS history FROM user;

-- Restructure objects in a select expression after `.` operator
SELECT properties.address.{city, country} FROM person;
```

#### Поддержка диапазона записей по идентификатору
```
-- Select all person records with IDs between the given range
SELECT * FROM person:1..1000;

-- Select all records for a particular location, inclusive
SELECT * FROM temperature:['London', NONE]..=['London', time::now()];

-- Select all temperature records with IDs less than a maximum value
SELECT * FROM temperature:..['London', '2022-08-29T08:09:31'];

-- Select all temperature records with IDs greater than a minimum value
SELECT * FROM temperature:['London', '2022-08-29T08:03:39']..;

-- Select all temperature records with IDs between the specified range
SELECT * FROM temperature:['London', '2022-08-29T08:03:39']..['London', '2022-08-29T08:09:31'];
```

Использование диапазона записей более эффективно, чем `WHERE`, поскольку не требует полного class scan.

---

## 12. MATCH — паттерн-матчинг

### 12.1. Семантика

`MATCH` выполняет поиск топологических паттернов в графе. 
Формально — поиск отображения паттерна в граф: `P → G`. 
Возвращает мультимножество биндингов переменных, соответствующих паттерну.

**Две независимые оси ограничений.** Их нельзя путать — они отвечают на разные вопросы:

| Ось                      | Вопрос                                                                  | Где задаётся                                                                               |
|--------------------------|-------------------------------------------------------------------------|--------------------------------------------------------------------------------------------|
| **Match mode** (морфизм) | Могут ли **два разных алиаса** паттерна связать **один и тот же** атом? | Модификатор `MATCH` ([§12.4](#124-match-mode--морфизм-и-кардинальность)) — на весь `MATCH` |
| **Path mode**            | Могут ли элементы повторяться **внутри одного пути**?                   | `CONSTRAINTS <...>` (глобально) или `[E ... ]` (локально), [§12.7](#127-path-constraints)  |

Match mode ограничивает отображение переменных, path mode — форму пути. Паттерн фиксированной длины не имеет path mode, но всегда имеет match mode; и наоборот, путь переменной длины имеет path mode, а его отношение к остальным алиасам — вопрос match mode.

По умолчанию действует `DIFFERENT EDGES` (edge-isomorphism): атомы-вершины могут повторяться, атомы-рёбра — нет. Это совпадает с поведением Cypher по умолчанию и гарантирует завершаемость.

`MATCH` описывает шаблон, который должен быть найден в графе.
Он отвечает на вопрос: "какие подграфы соответствуют этому паттерну?"

`MATCH` может:
- выступать как самостоятельный запрос;
- быть источником в `FROM (MATCH ...)` или в pipeline;
- быть использован в `LET $var = MATCH ...`.

`MATCH` должен возвращать:
- найденные соответствия паттерну;
- набор binding'ов переменных узлов и рёбер;
- при необходимости — путь целиком как отдельное значение.

### 12.2. Синтаксис

```
[OPTIONAL] MATCH
    [ DISTINCT ]
    [ ATOM ]
    [ REPEATABLE ELEMENTS | DIFFERENT EDGES | DIFFERENT ATOMS ]
    [ HYPER | META | METAVERTEX | METAEDGE ]
    [ CONSTRAINTS <@constraint ...> ]
    @pattern [ , @pattern ... ]
    [ CONTAINS ... ]
    IN GRAPH @graph_ref
    [ WHERE @condition ]
    [ MAXDEPTH @depth ]
    [ TIMEOUT @duration ]
    RETURN [ DISTINCT ] @projection
    [ ORDER BY ... ]
;

@pattern ::= [ @path_name = ] @relations_pattern
```

`WITH INDEX` принадлежит этапу 25, `WITH COST` — этапу 21, а `KEEP LEVELPATH` — этапу 17;
они не являются скрытыми compatibility clauses профиля этапов 15–16.

### 12.3. Синтаксис паттернов

#### Вершины

```
(*)                              -- любой node endpoint без alias
(Person)                         -- endpoint класса Person
(Person AS p)                    -- класс и alias
(Person AS p LABEL Employee)     -- graph-local label
(Person AS p { age: 18 })        -- inline equality над effective property
(Person AS p WHERE p.age > 18)   -- inline predicate
(person:alice AS p)              -- точный RecordId и alias
```

#### Рёбра

```
[TYPE]               -- connector edge класса TYPE
-[TYPE AS e]->       -- ориентированный connector с alias
<-[TYPE AS e]-       -- обратный ориентированный connector
-[TYPE AS e]-        -- неориентированный connector
<-[TYPE AS e]->      -- двунаправленный connector
-[* AS e]->, -[]->, -> -- connector без ограничения класса
-[TYPE*]->           -- открытая длина 1..effective_max
-[TYPE*1..3]->       -- длина 1..3 включительно
-[TYPE*..5]->        -- длина 1..5
-[TYPE*3]->          -- ровно три шага
```

#### Гиперрёбра

Гиперрёбра нельзя выразить стандартной нотацией стрелок. Для них используется специальный синтаксис:

```
-- HEAD и TAIL фильтруют source- и target-стороны
MATCH HYPER [TYPE AS e]
CONTAINS HEAD [(TYPE AS a), (TYPE AS b)], TAIL [(TYPE AS c)]
IN GRAPH topology
RETURN e, a, b, c;

-- ATOM и HYPER ортогональны: endpoint'ом может быть edge-атом
MATCH ATOM HYPER [TYPE AS e]
CONTAINS HEAD [<(TYPE AS a)>, <[GUARD AS guard]>],
         TAIL [<(TYPE AS c)>, <[AUDIT AS audit]>]
IN GRAPH topology
RETURN e, a, guard, c, audit;
```

Или через классическую семантику с проверкой на `HYPER`

```
MATCH (TYPE AS a)-[TYPE AS e]->(TYPE AS b)
IN GRAPH topology
WHERE type::is<hyperedge>(e)
RETURN a, e, b;
```

Или как фильтр по набору участников:

```
MATCH HYPER [TYPE AS e] CONTAINS [(TYPE AS a)]
IN GRAPH topology
RETURN e, a;
```

#### Мета-атомы

Мета-вершина в паттерне:

```
-- Найти метавершину и атом внутри неё
MATCH METAVERTEX (Module AS m)
IN GRAPH catalog
RETURN m, graph::inner(m) AS subgraph;
```

Работа с внутренними атомами мета-вершины — через функции `graph::inner(m)`, `graph::nodes(m)`, `graph::edges(m)`.

Мета-ребро в паттерне:
```
MATCH METAEDGE (* AS a)-[TYPE AS me]->(* AS b)
IN GRAPH catalog
RETURN me, graph::inner(me);
```

`METAEDGE` фильтрует связывающее ребро по наличию внутреннего/transition-графа. Связь
edge-to-edge не делает ребро метаребром и выражается только полным AtomWalk-паттерном.

#### Переход по вложенным графам

Мета-атомы способны хранить вложенные графы, к атомам которых тоже необходимо получать доступ в запросах.
Делать это через `graph::inner` неудобно, так как это переключает контекст и заставляет использовать подзапросы.
Решение этой проблемы - оператор `WITHIN`.

```
@any_inner_atom WITHIN @meta-atom
```

Эта запись означает, что `@any_inner_atom` вложен в `@meta-atom`.

Цепочка `... WITHIN ... WITHIN ...` читается слева направо, от листа к корню вложенности.

Произвольная вложенность без перечисления промежуточных уровней задаётся через `*`:
```
@any_inner_atom WITHIN*[ min..max ] @meta-atom
```
и означает "где-то внутри вложенных графов".

Указание глубины поиска опционально. Если верхняя граница `max` не указана, используется настройка `FG_WITHIN_MAX_DEPTH`.

#### Дополнительные возможности

Помимо clause-фильтров `HYPER`, `METAVERTEX` и `METAEDGE` есть ещё `META` (подразумевает любой мета-атом).

### 12.4. Match mode — морфизм и кардинальность

#### Match mode (морфизм)

Определяет, могут ли **разные алиасы** паттерна связать один и тот же атом. Применяется ко **всему `MATCH`** — то есть ко всем перечисленным через запятую паттернам сразу (как relationship-uniqueness в Cypher).

| Модификатор                          | Описание                                                                                                         |
|--------------------------------------|------------------------------------------------------------------------------------------------------------------|
| `DIFFERENT EDGES` (**по умолчанию**) | Никакие два алиаса-ребра не связывают одно ребро. Алиасы-вершины могут совпадать. Edge-isomorphism, как в Cypher |
| `REPEATABLE ELEMENTS`                | Ограничений нет — полный гомоморфизм: любой алиас может связать любой атом                                       |
| `DIFFERENT ATOMS`                    | Все алиасы (и вершины, и рёбра) связывают попарно различные атомы — полный изоморфизм                            |

```
-- Две разные связи между одной парой людей
MATCH (Person AS a)-[KNOWS AS r1]->(Person AS b)-[KNOWS AS r2]->(Person AS a)
IN GRAPH social
RETURN r1, r2;              -- r1 ≠ r2 гарантировано по умолчанию

-- Разрешить r1 = r2 (найдёт петли KNOWS)
MATCH REPEATABLE ELEMENTS (* AS a)-[KNOWS AS r1]->(* AS b)-[KNOWS AS r2]->(* AS a)
IN GRAPH social RETURN r1, r2;

-- Треугольник из трёх различных людей и трёх различных рёбер
MATCH DIFFERENT ATOMS (Person AS a)-[KNOWS]->(Person AS b)-[KNOWS]->(Person AS c)-[KNOWS]->(Person AS a)
IN GRAPH social
RETURN a, b, c;
```

`DIFFERENT ATOMS` заменяет прежний модификатор `MATCH ISOMORPHIC`; его нельзя выразить через path constraints, потому что паттерны, перечисленные через запятую, не образуют единого пути.

#### Кардинальность

| Модификатор            | Описание                                               |
|------------------------|--------------------------------------------------------|
| `MATCH` (по умолчанию) | Возвращает все совпадения (мультимножество)            |
| `OPTIONAL MATCH`       | Сохраняет строки, у которых часть паттерна не найдена  |
| `MATCH DISTINCT`       | Убирает дублирующиеся биндинги по всему набору алиасов |

Оси независимы и комбинируются: `OPTIONAL MATCH DISTINCT DIFFERENT ATOMS ...`.

### 12.5. Правила биндингов

- `(Person AS a)` создаёт биндинг `a: node-occurrence<Person>`.
- `[FRIEND AS r]` создаёт биндинг `r: edge-occurrence<FRIEND>`.
- Один алиас в одном паттерне означает один и тот же объект: `(a)-->(b), (b)-->(c)` — `b` одинаков в обоих совпадениях.
- Биндинги живут в scope `MATCH`; `WITH` / `LET` создают новый scope.

### 12.6. Семантика пути переменной длины

```
MATCH (* AS a)-[E*min..max]->(* AS b) IN GRAPH graph RETURN a, b
```

- Возвращает **все** пути длины ∈ `[min, max]`, по умолчанию `TRAIL`-семантика.

```
MATCH path = (Person AS a)-[FRIEND*1..3]->(Person AS b)
IN GRAPH social
WHERE a.name = "Alice"
RETURN path;
```

- При использовании `SHORTEST` модификатора границы можно опускать:
  - если не указан `min`, но указан `max`, алгоритм попытается найти кратчайший путь не больше `max`;
  - если не указан `max`, но указан `min`, алгоритм попытается найти кратчайший путь не меньше `min`;
  - если не указаны ни `min`, ни `max`, алгоритм попытается найти самый короткий путь;
  - если указаны и `min`, и `max`, алгоритм попытается найти кратчайший путь не меньше `min` и не больше `max`.
- `path` как именованная переменная доступна функциям: `nodes(path)`, `edges(path)`, `length(path)`, `atoms(path)`.
- Если `max` не указан явно, запрос будет использовать ограничения по приоритету: `MAXDEPTH` clause или `FG_PATH_MAX_DEPTH` настройку. 

### 12.7. Path constraints

Определяет ограничения путей (path constraints) для оператора `MATCH`.

- Ограничения перечисляются в `<...>` через пробел после оператора `CONSTRAINTS` для всего `MATCH`.
- Или ограничения могут задаваться для отдельного пути внутри без запятых `[:r*... constraints...]`
- Порядок модификаторов не имеет значения.
- **Локальные ограничения пути имеют приоритет над глобальными** (см. [§15.3](#153-модификаторы-путей)).

Доступные модификаторы ограничений описаны в [15.3](#153-модификаторы-путей).
Ограничения на *инъективность алиасов* — это отдельная ось, см. [§12.4](#124-match-mode--морфизм-и-кардинальность).

```
MATCH CONSTRAINTS <SIMPLE SHORTEST>
      path = (Person AS a)-[Friend]->(Person)
IN GRAPH social
WHERE a.name = "Alice"
RETURN path
```
или
```
MATCH path = (Person AS a)-[Friend SIMPLE SHORTEST]->(Person)
IN GRAPH social
WHERE a.name = "Alice"
RETURN path
```

### 12.8. Расширения MATCH

#### 12.8.1. `IN GRAPH` — источник MATCH

В отличие от Neo4j в базе данных Ferros-Grynn нет глобального графа, вместо этого в базе данных может храниться множество графов, 
каждый из которых имеет независимые `GraphId` и каталожное имя.

Граф-источник указывается через `IN GRAPH`:
```
IN GRAPH graph_ref
```

`graph_ref` является именем графа либо параметром типа `GraphRef`. Он разрешается в одном
statement snapshot до начала row pull. Arbitrary graph-expression и результат подзапроса не
являются MATCH source.

Внутренний graph мета-атома доступен как `graph::inner(occurrence)`. Связь inner graph с
владельцем сопоставляется через `WITHIN`; ordinary adjacency traversal не меняет graph scope
неявно.

#### 12.8.2. MATCH ATOM — AtomWalk-паттерн

Включает AtomWalk-режим (§15.2). В endpoint-позициях допускаются любые атомы, но каждый
шаг по-прежнему содержит отдельное связывающее ребро.

Синтаксис:
```
MATCH ATOM @atomwalk_pattern ...
```

Операторы инцидентности в AtomWalk:
- `<(a)>~[e]~><(b)>` — endpoint Source→Target
- `<(a)>~[e]~<(b)>` — endpoint ненаправленный
- `<(a)><~[e]~<(b)>` — endpoint Target←Source
- `<[e1]>~[r]~><[e2]>` — E–E через связывающее ребро `r`

Классические дефисы и стрелки в AtomWalk-паттерне не принимаются: тильда является
обязательным визуальным маркером расширенного endpoint domain.

---

## 13. Traversal Pipeline

### 13.1. Идея

Traversal pipeline нужен для сценариев, где запрос строится как последовательность преобразований.
Traversal отвечает на вопрос: "как пошагово пройти по графу и преобразовать промежуточный набор элементов?".

Traversal — потоковая обработка набора Traverser'ов. 
Каждый Traverser несёт:

```
Traverser = {
    current: atom | value,   -- текущий элемент
    path: Path?,             -- накопленный путь (включается по запросу)
    sack: Value?,            -- локальное состояние (для накопления)
    env: Bindings            -- биндинги из MATCH / LET
    bulk: u64                -- количество одинаковых traverser'ов (компрессия)
}
```

Ключевая оптимизация: ```bulk``` (как в Gremlin) — позволяет агрегировать одинаковые состояния без дублирования.

#### Условие слияния (bulk equality)

Два traverser'а сливаются в один с суммированием `bulk` **только если равны все компоненты их состояния**, а не только `current`:

```
mergeable(t1, t2) ⟺ t1.current == t2.current
                  ∧ t1.path    == t2.path
                  ∧ t1.sack    == t2.sack
                  ∧ t1.env     == t2.env
```

Из этого следуют три практических правила:

1. **Трекинг пути отключает bulking.** У двух traverser'ов, пришедших в одну вершину разными путями, `path` различается, поэтому они не сливаются. Как только в pipeline появляется `tv::path()` или именованный путь, компрессия по `bulk` для этой ветви исчезает, и кардинальность растёт как без неё. Поэтому путь трекается **лениво**: пока он не запрошен, он не материализуется и не мешает слиянию.
2. **Именованный sack отключает bulking** ровно в той же мере: traverser'ы с разным накопленным состоянием — разные. Глобальный `sack`, одинаковый у всех, слиянию не мешает.
3. **Агрегаты и side-effect умножаются на `bulk`.** `tv::count()` прибавляет `bulk`, а не единицу; `tv::sum(f)` прибавляет `f × bulk`; `tv::side_effect(...)` применяется с учётом кратности. Шаг, который «считает traverser'ы», обязан считать именно `bulk`, иначе компрессия изменит результат — это главный источник ошибок при реализации шагов.

**Инвариант корректности.** Для любого шага `s` результат обязан совпадать с результатом на развёрнутом потоке:

$$[\![s]\!](\{(t, bulk)\}) \equiv [\![s]\!](\underbrace{\{t, t, \dots, t\}}_{bulk})$$

Шаг, не удовлетворяющий этому равенству, обязан явно разворачивать `bulk` перед работой (`bulk = 1` для каждого traverser'а). Разворот обязателен для шагов, вносящих различие между экземплярами: `tv::path()`, ветвление с побочными эффектами, любые шаги, чей результат зависит от порядкового номера traverser'а.

Pipeline начинается с исходного набора и последовательно применяет шаги:
- выбрать стартовые элементы;
- перейти по рёбрам;
- отфильтровать;
- преобразовать;
- сгруппировать;
- собрать результат.

Пайплайн берет поток Traverser'ов (путешественников), сгенерированных левой частью выражения, 
и пропускает их через цепочку шагов (Steps) Traversal Machine.
Каждый шаг: `Stream<Traverser> → Stream<Traverser>`.

### 13.2. Pipeline-оператор

`|>` — лево-ассоциативный pipe-оператор.

```
@source_statement
  |> @step1(...)
  |> @step2(...)
  |> @step3(...)
```

Источник — любой statement, возвращающий набор атомов или записей: `SELECT`, `MATCH`, `TRAVERSE`, литеральный массив.

### 13.3. TRAVERSE

`TRAVERSE` — явный старт traversal pipeline.

```
TRAVERSE [ CONSTRAINTS <@path_constraints> ] @source
    |> step(...)
    |> step(...)
;

TRAVERSE ATOM [ CONSTRAINTS <@path_constraints> ] @atom_source
    |> step(...)
    |> step(...)
;

@source ::=
    FROM @graph |> @source_step    -- Граф-источник, за которым обязательно следует шаг, генерирующий исходный набор (см. "Навигация" в §13.4)
    | FROM @record_id           -- Начать с конкретного атома
    | FROM MATCH @pattern       -- Использовать MATCH как источник (смешанный стиль)
    | FROM SELECT ...           -- Использовать SELECT как источник (смешанный стиль)

@atom_source ::=
    FROM <(@endpoint_ref)>      -- Начать с vertex-атома
    | FROM <[@endpoint_ref]>    -- Начать с edge-атома
    | FROM MATCH @atom_pattern
    | FROM SELECT ...
```

В `TRAVERSE ATOM` прямой стартовый атом всегда указывает свой kind: `FROM <(@endpoint_ref)>`
для vertex или `FROM <[@endpoint_ref]>` для edge. Например,
`TRAVERSE ATOM FROM <[relation:depends]> |> tv::out('CAUSES')`. Запросы и графовые выражения
в роли источника дополнительной обёртки не требуют.

Глобальные `CONSTRAINTS` применяются ко всем шагам, которые порождают путь. Для шага без
собственного path mode действует `TRAIL`; локальный mode/objective/filter шага переопределяет
соответствующую глобальную составляющую. Несовместимые objective или несколько path mode на
одном уровне являются ошибкой semantic analysis, а не правилом «последний победил».

#### Роль TRAVERSE и отличия от SELECT и MATCH

| Оператор   | Парадигма       | Состояние   | Применение                                                        |
|------------|-----------------|-------------|-------------------------------------------------------------------|
| `SELECT`   | Реляционная     | Нет         | Таблично-ориентированные запросы, аггрегирующая область видимости |
| `MATCH`    | Паттерн-матчинг | Нет         | Декларативный поиск подграфов                                     |
| `TRAVERSE` | Императивный    | Есть (sack) | Алгоритмы, накопление, сложная итерация                           |

TRAVERSE нужен когда:
- Необходимо состояние на traverser (sack для накопления значений)
- Нужны повторения обходов с нетривиальным условием остановки
- Применяется `side_effect()` или алгоритмы
- Требуется ветвление потока

Пример: PageRank внутри подграфа
```
TRAVERSE FROM graph::inner(mv)
    |> tv::V()
    |> tv::pagerank(damping=0.85, iterations=20)
    |> tv::order("rank", "desc")
    |> tv::limit(10);
```

Пример: накопление весов пути через sack
```
TRAVERSE FROM dependency_graph
    |> tv::V(["Package"])
    |> tv::init_named_sack("weights_sum", 0.0)
    |> tv::repeat(
            tv::out(["DEPENDS_ON"])
            |> tv::named_sack("weights_sum", |acc, current| acc + current.weight)
       ).until(|traversals| traversals.current.is_leaf)
    |> tv::get_named_sack("weights_sum")
RETURN weights_sum;
```

### 13.4. Traversal steps

Все шаги принадлежат пространству имён `tv::`. Допускается вызов без префикса, если нет конфликтов.

#### Навигация

| Шаг                       | Описание                                                           |
|---------------------------|--------------------------------------------------------------------|
| `tv::V([labels])`         | Стартовый набор: все вершины (с опциональной фильтрацией по метке) |
| `tv::E([labels])`         | Стартовый набор: все рёбра                                         |
| `tv::out([labels])`       | Перейти к соседям по исходящим рёбрам                              |
| `tv::in([labels])`        | Перейти к соседям по входящим рёбрам                               |
| `tv::both([labels])`      | Перейти в обоих направлениях                                       |
| `tv::outE([labels])`      | Перейти к исходящим рёбрам (а не соседям)                          |
| `tv::inE([labels])`       | Перейти к входящим рёбрам                                          |
| `tv::bothE([labels])`     | Перейти ко всем инцидентным рёбрам                                 |
| `tv::inV()`               | Перейти от ребра к его источнику                                   |
| `tv::outV()`              | Перейти от ребра к его назначению                                  |
| `tv::hyper([label])`      | Перейти к гиперребру или от него                                   |
| `tv::meta_inner([label])` | Перейти к внутреннему графу мета-вершины                           |

#### Фильтрация

| Шаг                       | Описание                                                           |
|---------------------------|--------------------------------------------------------------------|
| `tv::hasattr(key)`        | Оставить только те, у которых есть атрибут с именем `key`          |
| `tv::has(key, value)`     | Оставить только те, у которых `atom.key = value`                   |
| `tv::has(key, predicate)` | Оставить только те, у которых `atom.key` удовлетворяет `predicate` |
| `tv::hasLabel(label)`     | Фильтр по метке атома                                              |
| `tv::where(closure)`      | Фильтрация произвольным замыканием `\|t\| -> bool`                 |
| `tv::dedup()`             | Убрать дубликаты по `current` по id                                |
| `tv::dedup_by(field)`     | Убрать дубликаты по `current` по `field`                           |
| `tv::simplePath()`        | Отфильтровать traverser'ы с повторяющимися вершинами в пути        |
| `tv::cyclicPath()`        | Оставить только traverser'ы с циклом в пути                        |

#### Преобразование

| Шаг                                 | Описание                                               |
|-------------------------------------|--------------------------------------------------------|
| `tv::map(closure)`                  | 1→1 преобразование: `\|t\| -> new_value`               |
| `tv::flatMap(closure)`              | 1→N преобразование                                     |
| `tv::values([fields])`              | Проекция: извлечь значение поля как новый `current`    |
| `tv::select(alias1 [, alias2 ...])` | Выбрать ранее сохранённые биндинги из env              |
| `tv::match(pattern)`                | Выбрать по паттерну                                    |
| `tv::as(alias)`                     | Сохранить текущий `current` в `env` под именем `alias` |
| `tv::project(fields)`               | Структурированная проекция в объект                    |

#### Состояние

| Шаг                                  | Описание                                                                                                       |
|--------------------------------------|----------------------------------------------------------------------------------------------------------------|
| `tv::init_sack(initial)`             | Инициализировать глобальный `sack`                                                                             |
| `tv::init_named_sack(name, initial)` | Инициализировать именованный `sack`                                                                            |
| `tv::sack(closure)`                  | Модифицировать локальное состояние; closure `(sack, current) -> sack` накапливает значение                     |
| `tv::named_sack(name, closure)`      | Модифицировать локальное состояние именованного `sack`; closure `(sack, current) -> sack` накапливает значение |
| `tv::get_sack()`                     | Вернуть текущий глобальный `sack` как `current`                                                                |
| `tv::get_named_sack(name)`           | Вернуть именованный `sack` как `current`                                                                       |
| `tv::side_effect(name, closure)`     | Обновить глобальное состояние запроса (агрегаты, счетчики)                                                     |
| `tv::path()`                         | Вернуть накопленный путь как `current`                                                                         |
| `tv::emit()`                         | Выдать текущий traverser в результат и продолжить обход                                                        |

#### Контроль потока

| Шаг                                   | Описание                                             |
|---------------------------------------|------------------------------------------------------|
| `tv::limit(n)`                        | Обрезать поток                                       |
| `tv::skip(n)`                         | Пропустить первые n                                  |
| `tv::order(field [ASC\|DESC])`        | Упорядочить (materializing шаг)                      |
| `tv::group(fields)`                   | Сгруппировать (materializing шаг)                    |
| `tv::repeat(pipeline).times(n)`       | Повторить ровно n раз                                |
| `tv::repeat(pipeline).until(closure)` | Повторять, пока closure возвращает false             |
| `tv::repeat(pipeline).emit()`         | Выдавать промежуточные результаты                    |
| `tv::branch(step1, step2, ...)`       | Разветвить поток: один traverser → несколько         |
| `tv::choose(closure, step1, step2)`   | Условное ветвление                                   |
| `tv::coalesce(step1, step2, ...)`     | Попробовать шаги по очереди; вернуть первый непустой |

#### Агрегация

| Шаг                                 | Описание                                  |
|-------------------------------------|-------------------------------------------|
| `tv::count()`                       | Количество traverser'ов                   |
| `tv::sum(field)`                    | Сумма значений поля                       |
| `tv::min(field)` / `tv::max(field)` | Минимум / максимум                        |
| `tv::avg(field)`                    | Среднее                                   |
| `tv::fold()`                        | Сворачивает поток в массив                |
| `tv::unfold()`                      | Разворачивает массив в поток traverser'ов |

#### Алгоритмы

**Пути и расстояния**

| Шаг                                               | Описание                                                                            |
|---------------------------------------------------|-------------------------------------------------------------------------------------|
| `tv::shortestPath(target)`                        | Кратчайший путь до target по числу рёбер (BFS)                                      |
| `tv::weightedShortestPath(target, weight_field?)` | Взвешенный кратчайший путь (Dijkstra); требует неотрицательных весов                |
| `tv::bellmanFord(target, weight_field?)`          | Взвешенный кратчайший путь с отрицательными весами; сообщает об отрицательном цикле |
| `tv::allShortestPaths(target)`                    | Все кратчайшие пути до target                                                       |
| `tv::distanceTo(target, weight_field?)`           | Только длина/стоимость, без материализации пути                                     |
| `tv::kHop(k)`                                     | Множество вершин ровно на расстоянии k                                              |

**Структура графа**

| Шаг                                      | Описание                                                           |
|------------------------------------------|--------------------------------------------------------------------|
| `tv::topologicalSort()`                  | Топологическая сортировка (только для ациклического подграфа)      |
| `tv::stronglyConnectedComponents()`      | Компоненты сильной связности (SCC)                                 |
| `tv::weaklyConnectedComponents()`        | Компоненты слабой связности (WCC)                                  |
| `tv::condensation()`                     | Конденсация по SCC — DAG из компонент                              |
| `tv::minimumSpanningTree(weight_field?)` | Минимальное остовное дерево / лес                                  |
| `tv::findCuts()`                         | Точки сочленения и мосты                                           |
| `tv::lca(a, b)`                          | Наименьший общий предок (binary lifting; требует корневого дерева) |
| `tv::hasCycle()`                         | Наличие цикла                                                      |
| `tv::reachable(target)`                  | Достижимость; использует reachability-индекс, если он `ready`      |

**Аналитика и центральности**

| Шаг                                             | Описание                                           |
|-------------------------------------------------|----------------------------------------------------|
| `tv::pagerank(damping: float, iterations: int)` | PageRank                                           |
| `tv::degreeCentrality(direction?)`              | Центральность по степени                           |
| `tv::betweennessCentrality(sampling?)`          | Центральность по посредничеству (Brandes)          |
| `tv::closenessCentrality()`                     | Центральность по близости                          |
| `tv::triangleCount()`                           | Число треугольников на вершину                     |
| `tv::clusteringCoefficient()`                   | Коэффициент кластеризации                          |
| `tv::labelPropagation(iterations: int)`         | Выделение сообществ распространением меток         |
| `tv::louvain(resolution?)`                      | Выделение сообществ (модулярность, Louvain)        |
| `tv::algo(name, config)`                        | Запуск произвольного зарегистрированного алгоритма |

Алгоритмы, требующие полного обзора графа (`SCC`, `WCC`, `pagerank`, `louvain`, центральности), являются **барьерными** ([§13.8](#138-классы-шагов)): они материализуют поток целиком и не могут быть слиты с соседними шагами.

### 13.5. Sack

Sack — это объект-состояние, собираемый в процессе traversal pipeline.
У каждого traverser'а есть своё собственное локальное sack-состояние, которое можно синхронизировать с остальными или объединить и вернуть.

Существует два типа sack:
1. `Global sack`, или просто `sack`, — это свойство, которое есть у всех traverser'ов по умолчанию.
2. `Named sack` — это именованные sack-объекты, которых не существует при начале обхода, но их можно создавать вручную.

Над sack можно проводить несколько операций:
- Во-первых, его можно и нужно инициализировать. Если `global sack` может накапливаться без инициализации, 
  то попытка получить доступ к `named sack` без предварительной инициализации завершится ошибкой.
- Состояние `sack` можно модифицировать с помощью замыкания.
- Состояние `sack` можно получить. Например, внутри другой функции или вернуть как результат обхода.

### 13.6. Семантика результата

Traversal возвращает `Stream<Traverser>`, который преобразуется в итоговый тип через терминирующий шаг:

- `tv::values([fields])` → массив скалярных значений;
- `tv::path()` → массив путей;
- `tv::project(...)` → массив объектов;
- `tv::count()` → число;
- `tv::fold()` → один массив.

Без явного терминирующего шага traversal возвращает `array<atom>`.

### 13.7. Материализация

Некоторые шаги требуют полной материализации потока перед продолжением: `tv::order(...)`, `tv::group(...)`, `tv::fold()`. 
Планировщик обязан учитывать это при оценке стоимости.

Materialization происходит только на:
- barrier steps;
- явный сворачивающий шаг ```tv::fold()```;
- boundary (RETURN).

### 13.8. Классы шагов

#### Map (1 → 1)
```values(), map()```
- не изменяет кардинальность;
- не требует materialization.

#### FlatMap (1 → N)
```out(), in(), both()```
- порождает новые traverser’ы;
- ключевая точка взрыва кардинальности.

#### Filter (1 → 0/1)
```has(), where()```
- отбрасывает traverser’ы

#### Barrier (N → N)
```group(), order(), dedup(), fold()```
- требует materialization;
- блокирует поток.

#### Side-effect
```side_effect(), sack()```
- изменяет состояние, но не поток

#### Control flow
```repeat(), branch(), choose()```
- формирует execution graph (нелинейный)

### 13.9. Path propagation

При каждом шаге: ```new_path = old_path + edge + node```.

Оптимизация: хранить path как ```Path = persistent structure (rope / arena index)``` -> O(1) append.

### 13.10. Parallel execution

Traversal VM должна поддерживать:
1. Data parallelism ```partition traversers across threads```;
2. Step parallelism (ограниченно).
   Только для:
   - stateless steps;
   - pure functions.

### 13.11. Cost hotspots

Traversal дорогой в:
- fan-out (out() на high-degree nodes);
- repeat();
- path tracking.

Обязательные оптимизации:
- adjacency index;
- degree-aware planning;
- path pruning.

---

## 14. Семантика биндингов

### 14.1. Базовая модель

Биндинг-среда (binding environment): `Env = Map<Alias, Value>`, 
где `Value ∈ {atom, path, scalar, collection<Value>}`.

### 14.2. Правила

| Правило          | Описание                                                                             |
|------------------|--------------------------------------------------------------------------------------|
| Объявление       | `(Person AS a)` → `Env[a] = vertex<Person>`                                             |
| Повторный alias  | `(a)-->(b), (b)-->(c)` → `Env[b]` одинаков в обоих — это constraint, не присваивание |
| Scope            | `MATCH` создаёт новый scope; `WITH` / `LET` ограничивают или расширяют scope         |
| MATCH result     | Возвращает `Bag<Env>` (мультимножество биндинг-сред)                                 |
| Traversal result | Возвращает `Stream<Traverser>`, где `Traverser.env = Env`                            |
| OPTIONAL         | Если часть паттерна не найдена: `Env[несовпавший_alias] = none`, строка сохраняется  |
| DISTINCT         | Применяется к `Env-set` по ключу alias'ов                                            |

### 14.3. Shadowing

Внутренний alias перекрывает внешний в том же scope. При передаче через `WITH` — только явно перечисленные биндинги.

```
WITH a, b   -- binding' = projection(binding, {a, b})
```

---

## 15. Семантика путей

### 15.1. StandardPath

Формально: $Path = (v_0, e_1, v_1, e_2, \dots, e_n, v_n)$, где:
- $v_i$ — вершины (vertex или meta-vertex);
- $e_i$ — рёбра, соединяющие $v_{i-1}$ и $v_i$ через endpoint-инцидентность;
- каждое $e_i$ является endpoint-инцидентным с обеими соседними вершинами.

Длина пути: n (количество рёбер).

Стоимость пути: $cost(P) = \displaystyle\sum_i \text{weight}(e_i)$. Отсутствующий weight
равен exact numeric `1`. Явный negative либо non-finite weight на рассматриваемом connector
edge является typed runtime error и не подменяется единицей. При `SHORTEST` каждый шаг стоит
единицу, поэтому cost равен числу connector steps.

### 15.2. AtomWalk

Формально: $W = (a_0, r_1, a_1, r_2, a_2, \dots, r_n, a_n)$, где:
- $a_k \in A = V \cup MV \cup E \cup ME$ — endpoint-атомы;
- $r_k \in E \cup ME$ — явные связывающие рёбра;
- $a_{k-1} \in \operatorname{source}(r_k)$ и $a_k \in \operatorname{target}(r_k)$
  для прямого направленного шага (с учётом обратного/двунаправленного/неориентированного режима).

Таким образом, AtomWalk отличается от StandardPath только расширенным доменом endpoint'ов.
Длина: n (количество связывающих рёбер).

### 15.3. Модификаторы путей

#### Path mode
| Режим     | Условие                                            |
|-----------|----------------------------------------------------|
| `WALK`    | Без ограничений (повторы вершин и рёбер допустимы) |
| `TRAIL`   | Связывающие рёбра не повторяются                   |
| `ACYCLIC` | Endpoint-атом не повторяется                        |
| `SIMPLE`  | Endpoint не повторяется, кроме одного совпадения последнего endpoint с первым |

`ACYCLIC` запрещает возврат к началу, а `SIMPLE` допускает один замыкающий шаг. Оба режима
контролируют endpoint history независимо от match mode. Повтор mode в одном constraint scope
является semantic error.

> Path mode ограничивает **один путь**. Инъективность между разными алиасами паттерна — это **match mode**, отдельная ось ([§12.4](#124-match-mode--морфизм-и-кардинальность)); полный изоморфизм по всем алиасам задаётся через `MATCH DIFFERENT ATOMS`, а не через path mode.

#### Область действия и приоритет

Path-ограничения задаются на двух уровнях:
- **глобально на весь запрос** — `CONSTRAINTS <...>` после modifiers `MATCH` и до pattern list;
- **локально на конкретный путь** — внутри `[E* ... ]`.

**Локальные ограничения имеют приоритет над глобальными.** Если у пути задан собственный path mode, глобальный `CONSTRAINTS` для этого пути игнорируется; для всех остальных путей запроса продолжает действовать глобальный. Objective и filter разрешаются по тому же правилу, независимо друг от друга.

```
MATCH CONSTRAINTS <SIMPLE>
      path1 = (* AS a)-[E*1..5]->(* AS b), -- наследует global SIMPLE
      path2 = (* AS c)-[E*1..5 WALK]->(* AS d)
IN GRAPH social
RETURN path1, path2;
```

#### Path objective
| Тип                 | Ограничение                                        |
|---------------------|----------------------------------------------------|
| `SHORTEST`          | Минимальный по числу рёбер                         |
| `WEIGHTED SHORTEST` | Минимальный по сумме весов рёбер                   |

В AtomWalk стоимость шага задаёт связывающее ребро $r_k$. Вес edge-атома в endpoint-позиции
не прибавляется отдельно.

#### Path filter
| Тип            | Ограничение                            |
|----------------|----------------------------------------|
| `WEIGHTED`     | Только по взвешенным рёбрам            |
| `META`         | Только по мета-атомам                  |
| `METAVERTICES` | Только по мета-вершинам и любым рёбрам |
| `METAEDGES`    | Только по мета-рёбрам и любым вершинам |
| `HYPEREDGES`   | Только по гипер-рёбрам                 |
| `CROSS_LEVEL`  | Только по кросс-уровневым рёбра        |

По умолчанию: `TRAIL` mode без objective и filter ограничений.

### 15.4. Переменная длина

`[E*min..max]` означает: `path::length(path) ∈ [min, max]`.

### 15.5. Стоимость пути

$```cost(path) = \sum_i weight(e_i)```$

Пользовательская функция стоимости: `DEFINE COST FUNCTION fn::my_cost(edge, accumulated_cost)`.
Её можно использовать тремя способами:
- для определённого пути `[E ... COST fn::my_cost]`;
- как модификатор всего запроса `MATCH ... WITH COST fn::my_cost`;
- как второй аргумент функции стоимости: `path::cost($path, $cost_fn)`

### 15.6. Именованный путь и функции над ним

Найденный в запросе путь можно записать в alias:
```
MATCH path = (Person AS a)-[FRIEND*1..3 SIMPLE]->(Person AS b)
IN GRAPH social
RETURN path, path::length(path), path::cost(path);
```

| Функция           | Тип результата  | Описание                               |
|-------------------|-----------------|----------------------------------------|
| `path::atoms(p)`  | `array<atom-occurrence>`   | Полная чередующаяся последовательность |
| `path::nodes(p)`  | `array<atom-occurrence>`   | Node endpoint occurrences              |
| `path::edges(p)`  | `array<atom-occurrence>`   | Connector edge occurrences             |
| `path::length(p)` | `int`                       | Количество connector steps             |
| `path::cost(p)`   | `number`                    | Exact сумма весов либо длина            |

Именованный или возвращаемый путь отслеживается с начала соответствующего обхода. Если путь
не именован, не возвращается и не используется `tv::path()`/path-функциями, исполнитель вправе
не создавать trace.

`RETURN path` материализует самодостаточную неизменяемую структуру: `SnapshotId`,
упорядоченные шаги с `AtomOccurrenceRef`, направление и exact `Number` cost. Внутренний
`AtomId` в результат не попадает. `path::nodes`, `path::edges`, `path::atoms`,
`path::length` и `path::cost` работают без живого storage snapshot.

Свойства атомов не копируются в каждый шаг автоматически. Каждый occurrence уже несёт точный
`GraphRef`, а path — `SnapshotId`; молчаливый переход к latest revision запрещён. Если
исторический snapshot уже недоступен, lazy property resolution возвращает `SnapshotUnavailable`.
Чтобы сохранить автономный результат, запрос должен явно спроецировать нужные свойства либо
выполняться над материализованным `query_to_memory`-графом.

Кванторы по элементам пути:
```
WHERE ALL(v IN nodes(path) WHERE v.age > 18)
WHERE ANY(e IN edges(path) WHERE e.weight > 5)
WHERE NONE(a IN atoms(path) WHERE a.deleted = true)
```

### 15.7. Фильтрация по пути

```
MATCH path = (* AS a)-[E*1..5 SIMPLE]->(* AS b)
IN GRAPH graph
WHERE ALL(n IN nodes(path) WHERE n.age > 18)
  AND path::length(path) < 4
RETURN path;
```

Кванторы по элементам пути: `ALL(x IN path::nodes(p) WHERE pred)`, `ANY(x IN path::nodes(p) WHERE pred)`, `NONE(...)`.

### 15.8. Идентичность путей

Два пути равны, если совпадает полная чередующаяся последовательность endpoint-атомов и
связывающих рёбер. `DISTINCT` по путям применяется к этой структуре.

### 15.9. Семантика обхода гипер-рёбер

Гиперребро использует тот же endpoint-механизм, что бинарное ребро, но одна или обе его
стороны содержат несколько атомов. `HYPER` задаёт арность, `ATOM` независимо расширяет
домен endpoint'ов с вершин до всех атомов.

#### 15.9.1. HyperDirected (topo = 3)

Пусть $S = \operatorname{source}(e)$ (`head`) и $T = \operatorname{target}(e)$ (`tail`).
Один шаг от любого $a_i \in S$ отдельно активирует ребро и порождает $|T|$ линейных
путей-веток — по одной на каждый $b_j \in T$. Наличие остальных элементов $S$ не требуется.

```
Граф: e = HyperDirected { source: [a, c], target: [b, d, f] }
Шаг из a:
Ветка 1: ... a, e, b ...
Ветка 2: ... a, e, d ...
Ветка 3: ... a, e, f ...
```

Ограничения:
- атомы $S$ не достигают друг друга через одно гиперребро;
- атомы $T$ не достигают друг друга через одно гиперребро;
- каждая ветка имеет обычную форму `endpoint, edge, endpoint`.

Для `AS BOTH` прямой переход использует правило $S \to T$, а обратный переход от любого
$b_j \in T$ порождает по одной ветке к каждому $a_i \in S$.

Каждая ветка - отдельный путь, как если бы из одной вершины выходило несколько рёбер.
Если хотя бы один endpoint ветки является ребром, паттерн обязан использовать `ATOM`, а
ветка имеет тип AtomWalk.

#### 15.9.2. HyperUndirected (topo = 4)

Пусть $U = \operatorname{source}(e)$ — единое множество неориентированных endpoint'ов.
Один шаг от $a_i \in U$ порождает $|U| - 1$ веток, по одной на каждый остальной атом.

```
Граф: e = HyperUndirected { source: [a, b, c] }
Шаг из a:
Ветка 1: ... a, e, b, ...
Ветка 2: ... a, e, c, ...

Шаг из b:
Ветка 1: ... b, e, a, ...
Ветка 2: ... b, e, c, ...

Шаг из c:
Ветка 1: ... c, e, a, ...
Ветка 2: ... c, e, b, ...
```

#### 15.9.3. Ограничения path-семантики для гиперрёбер

Ограничения path-mode применяются к каждой ветке независимо.
Возможность применять разные режимы к разным развилкам не предусмотрена, так переход через гипер-ребро — это атомарная (единичная) операция.

| Path mode | Семантика при гиперрёбрах                                                                                       |
|-----------|-----------------------------------------------------------------------------------------------------------------|
| `WALK`    | ветка может повторно пройти через то же гиперребро через другой вход — это разные шаги, порождающие разные пути |
| `TRAIL`   | Конкретное гиперребро посещается не более одного раза                                                           |
| `SIMPLE`  | Endpoint-атомы ветки не повторяются; следовательно, связывающее гиперребро также не повторяется                 |
| `ACYCLIC` | Ветка не возвращается к уже пройденному endpoint-атому                                                          |

Для k последовательных направленных гиперрёбер мощность выхода из одной входной ветки:
$P(depth=k) = \displaystyle\prod_{i=1}^{k} |\operatorname{target}(e_i)|$.
Для неориентированных гиперрёбер соответствующий множитель равен
$|\operatorname{source}(e_i)| - 1$.

Порядок веток без сохранённого endpoint-order не определён. `ORDER BY TAIL` при явном
`tail` и `ORDER BY TARGETS` без явного разбиения задают этот порядок при создании ребра
(§8.5); обычный query-level `ORDER BY` сортирует уже полученные строки.

#### 15.9.4. Одновременная активация всех source

Требование «перейти только после достижения всех $S$» не является путём в текущей модели:
такой результат растёт одновременно по длине и по ширине и должен хранить несколько
предшественников одного шага. Линейный `Path` его не аппроксимирует.

Отдельный многомерный `HyperPath` пока не вводится. Он может быть добавлен независимо,
когда будут определены его identity, length/cost, path modes, правила слияния веток и
конкретные операторы запроса. До этого `HyperDirected` имеет только попарную семантику выше.

### 15.10. Семантика обхода мета-атомов и кросс-уровневых рёбер

#### 15.10.1. Мета-вершина как непрозрачный узел

По умолчанию мета-вершина `mv` участвует в traversal как **единый непрозрачный атом** — её внутренний подграф не виден.

```
-- Стандартный обход: mv1 и mv2 — атомы
MATCH (Module AS mv1)-[DEPENDS_ON AS e]->(Module AS mv2)
IN GRAPH catalog
RETURN mv1, mv2;
```

#### 15.10.2. Доступ к внутреннему подграфу

Для работы с внутренним подграфом метавершины используются функции `graph::inner_*`:
```
-- Получить все внутренние вершины метавершины
MATCH METAVERTEX (Module AS mv)
IN GRAPH catalog
LET $inner = graph::inner_nodes(mv)
RETURN mv, $inner;
```

#### 15.10.3. Кросс-уровневые рёбра и переходы

Кросс-уровневое ребро соединяет атомы на разных уровнях вложенности. Если endpoint является
ребром, его обход требует `ATOM`, как и для любой V–E/E–V/E–E связи.

Свойства ребра могут содержать объект, содержащий пути по мета-вершинам от самого верхнего уровня до концов ребра.
Для этого используется предложение `KEEP LEVELPATH`, добавляющее в свойства ребра **уровневый путь** — набор ссылок на мета-вершины, 
начиная с мета-вершины одного конца кросс-уровневого ребра, заканчивая вершиной / мета-вершиной уровнями ниже другого конца этого ребра.

```
-- CROSS_LEVEL явно указывает на поиск кросс-уровневого ребра
MATCH CONSTRAINTS <CROSS_LEVEL>
      (Module AS mv)-[DEPENDS_ON AS e]->(Subsubmodule AS v)
IN GRAPH catalog
KEEP LEVELPATH
RETURN mv, v, e.levelpath;

-- Output
{
    "mv": 01KRMDFC2Q4ABBQ0WCE95T0W8W,
    "v": 01KRMDG8VQ278PE3BHYXHNHYR4,
    "levelpath": [01KRMDFC2Q4ABBQ0WCE95T0W8W, 01KRMDGVM5MTRG9KTW5YFQR9FJ, 01KRMDG8VQ278PE3BHYXHNHYR4]
                                                        ^^^
                   Некоторая мета-вершина типа Submodule в цепочке вложенности: Module -> Submodule -> Subsubmodule
}
```

Future: `DESCEND INTO @metavertex FROM @inner_atom` зарезервировано.

#### 15.10.4. Мета-ребро в traversal

Мета-ребро `me` — ребро с внутренним и/или `transition_graph`. Внешний traversal не
открывает этот граф автоматически: как связывающее ребро `me` использует обычные
`source`/`target`, а внутренний граф доступен через `graph::inner(me)`.

MetaEdge, как и любое другое ребро, может быть endpoint'ом другого ребра в AtomWalk:
```
MATCH ATOM <[TRANSITION AS me1]>~[DEPENDS_ON AS depends]~><[RESULT AS e2]>
IN GRAPH catalog
RETURN me1, depends, e2;
```

#### 15.10.5. V-E и E-V инцидентность в traversal

Когда конечной точкой ребра является другое ребро:
```
-- Ребро e соединяет вершину v с ребром e2
RELATE ATOM <(person:alice)>~[:ANNOTATES { label: "key" }]~><[friendship:alice_bob]>;
-- Traversal: от alice по ребру ANNOTATES достичь friendship
MATCH ATOM <(Person AS v)>~[ANNOTATES]~><[FRIEND AS target]>
IN GRAPH social
RETURN v, target;
```

Обратный переход использует тот же паттерн с обратным направлением.

---

### 15.11. LevelPath — не путь traversal

LevelPath (§5.11.3) хранится в свойствах ребра. Нет traversal по LevelPath.
Подъём по иерархии вложенности — только через свойства.

### 15.12. Семантика переходов в AtomWalk

AtomWalk обобщает endpoint-позиции шага на любые атомы. Во всех случаях переход выполняется
через отдельное ребро; различается только kind его endpoint'ов.

Для V-E и E-V поиска требуются связи (рёбра), которые не хочется создавать руками. Этот процесс можно легко автоматизировать. 
Как это сделать, описано в §21.4.1.

#### V → E (endpoint): вершина — конечная точка ребра

Сущность аннотирована некоторой связью. Переход означает: «перейти к самой связи, а не к её другому концу».

Применения: поиск рёбер с заданными свойствами; получение метаданных связи не через атрибуты, а как к объекту.

Пример:
```
-- ANNOTATES связывает alice с edge-атомом knows
MATCH ATOM <(Person AS alice)>~[ANNOTATES AS annotation]~><[KNOWS AS knows]>
IN GRAPH social
RETURN knows, knows.since;
```

#### E → V (endpoint): ребро — исходная точка, вершина — цель

Обратный переход: от связи к одному из её концов. Применяется когда
мы начинаем обход с ребра (например, найдено как результат предыдущего
шага) и хотим получить его endpoint.

Пример:
```
-- FROM_RELATION связывает edge-атом e с вершиной target
MATCH ATOM <[RELATION AS e]>~[FROM_RELATION AS link]~><(Person AS target)>
IN GRAPH social
RETURN e, link, target;
```

#### E → E: ребро соединяет два edge-атома

Переход E–E использует ту же тройку `source, edge, target`. Например, зависимость между
операциями является отдельным ребром `CAUSES`.

Применения: поиск зависимостей между операциями; анализ причинно-следственных цепей рёбер.

Пример:
```
MATCH ATOM <[EVENT AS e1]>~[CAUSES AS cause]~><[EVENT AS e2]>
IN GRAPH event_graph
RETURN e1, cause, e2;
```

## 16. Граф как результат запроса

### 16.1. Мотивация

Традиционные SQL запросы возвращают таблицы записей. 
GQL позволяет вернуть результат в виде пути или информации о вершинах и рёбрах.
Одной из уникальных особенностей Ferros-Grynn является возможность вернуть результат как граф — со всеми структурными свойствами:
```
-- Хочу дерево категорий, а не таблицу со ссылками
MATCH (Category AS c)-[CHILD AS child*]->(Category AS sub)
IN GRAPH categories_tree
RETURN GRAPH <tree> { nodes: [c, sub], edges: [child] };
```

Система типов не выполняет неявных преобразований graph family. Например, если собранный граф
является forest, а пользователь запросил `rooted_tree`, оператор завершается
`ConstraintViolation`. Аннотация `forest` принимает topology tree/rooted-tree, но возвращаемым
типом всё равно является точный `forest`, построенный в соответствующей family. Проверка и
перестроение выполняются **после** сборки результата; стоимость описана в §3.9.

### 16.2. RETURN GRAPH

Возвращает граф как результат запроса. Необязательный модификатор типа `@constraint` проверяет структуру результирующего графа.

```
RETURN GRAPH [ @constraint ] {
    nodes: [ @node_exprs ],
    edges: [ @edge_exprs ]
};
```

Возвращаемый тип:
- `subgraph` (`Subgraph<Frozen>` в Rust SDK), если `@constraint` не указан;
- точный frozen-тип, указанный `@constraint` (`Tree<Frozen>`, `Dag<Frozen>` и т. п.);
- если граф нарушает указанный тип → `ConstraintViolation` на уровне запроса.

`RETURN GRAPH` — терминальная проекция результата текущего запроса. Она полностью материализует
выбранную топологию и значения до возврата, после чего исходный transaction/disk snapshot
освобождается. Результат самодостаточен, неизменяем и не меняется вместе с базой. Ничего не
сохраняется в базе; значение существует, пока его держит клиент или сессия.

Транспортный sum-type не меняет контракт: вид варианта однозначно определяется синтаксисом
оператора. Пользователь не должен угадывать backend или ветку `GraphHandle`.

Примеры:
```
-- Вернуть граф без проверки типа
MATCH (Dept AS root)-[REPORTS_TO AS reports_to*]->(Emp AS emp)
IN GRAPH some_graph
RETURN GRAPH { nodes: [root, emp], edges: [reports_to] };

-- Вернуть с проверкой
MATCH (Package AS pkg)-[DEPENDS_ON AS dependency*]->(Package AS dep)
IN GRAPH dag_example
RETURN GRAPH <dag> { nodes: [pkg, dep], edges: [dependency] };
```

### 16.3. BUILD GRAPH

Явно строит именованный граф из результата запроса.

```
BUILD GRAPH 
    [ OVERWRITE | IF NOT EXISTS ]
    [ @constraint ] 
    [ @name ]
    FROM @source_query
    [ NODES @node_selector ]
    [ EDGES @edge_selector ]
    [ LIMIT NODES @n ]
    [ LIMIT EDGES @m ]
;
```
Модификатор типа `<@constraint>` — проверка после построения. Если граф не удовлетворяет типу, возвращается `ConstraintViolation`.

`BUILD GRAPH` — DDL/DML-гибрид: материализует и именует новый граф верхнего уровня.
После выполнения граф с именем `@name` появляется в каталоге графов наравне с графами, созданными через DEFINE GRAPH, 
и переживает транзакцию/сессию — он персистентен, пока не удалён явно (`REMOVE GRAPH @name`).
Если `@name` не указано — возвращается session-owned точный frozen-тип, заданный constraint
(без constraint — `Subgraph<Frozen>`). Он живёт как значение переменной в рамках сессии и не
попадает в постоянный каталог. Именованный `BUILD GRAPH` после успешного `COMMIT` возвращает
каталожную ссылку `GraphRef`; до коммита она не видна другим транзакциям.

#### Контракт результатов графовых операторов

| Операция                             | Результат                                              |
|--------------------------------------|--------------------------------------------------------|
| получение именованного графа         | `GraphRef` с известными graph kind и storage mode      |
| чтение `GraphRef` внутри запроса     | snapshot-bound read view                               |
| `MATCH ... RETURN atom`              | стабильный `RecordId`/atom ref, не внутренний `AtomId` |
| `MATCH path = ... RETURN path`       | самодостаточный структурный `path`                     |
| `RETURN GRAPH { ... }`               | самодостаточный `subgraph`                             |
| `RETURN GRAPH <type> { ... }`        | точный frozen graph type                               |
| именованный `BUILD GRAPH <type>`     | `GraphRef` после commit                                |
| анонимный `BUILD GRAPH <type>`       | session-owned frozen graph type                        |
| `graph::inner(meta)` внутри запроса  | snapshot-bound view                                    |
| возврат `graph::inner(meta)` клиенту | материализованный `subgraph`                           |

### 16.4. Ограничение глубины и граф-результаты

`FG_PATH_MAX_DEPTH` применяется **только к path-finding** (MATCH с `*` без явного max). 
RETURN GRAPH и BUILD GRAPH используют **graph-expansion mode**: не отслеживают пути, а собирают достижимые атомы.

Лимит в graph-expansion mode разделены по числу узлов и рёбер. 
Без явного указания лимитов используются настройки `FG_GRAPH_EXPANSION_MAX_NODES` и `FG_GRAPH_EXPANSION_MAX_EDGES`.
Это позволяет строить большие графы без ограничений, связанных с глубиной.

Примеры:
```
-- Построить деревья категорий как именованный граф
BUILD GRAPH <forest> categories_tree
FROM MATCH (Category AS root {root: true})-[CHILD AS child*]->(Category AS c)
     IN GRAPH taxonomy RETURN root, child, c
NODES [root, c]
EDGES [child];

-- Построить граф и проверить, что он DAG
BUILD GRAPH <dag> build_order
FROM MATCH (Task AS t)-[BEFORE AS b]->(Task AS t2)
     IN GRAPH tasks RETURN t, b, t2
NODES [t, t2]
EDGES [b];

-- Граф в LET для последующего traversal
LET $dep_graph = BUILD GRAPH <dag>
FROM MATCH (Package AS p)-[DEPENDS_ON AS e]->(Package AS d)
     IN GRAPH packages RETURN p, e, d
NODES [p, d] EDGES [e];

TRAVERSE FROM $dep_graph
|> tv::V(["Package"])
|> tv::out(["DEPENDS_ON"])
|> tv::topological_sort();
```

## 17. Транзакционная модель

### 17.1. Уровни изоляции

| Уровень            | Поддержка        |
|--------------------|------------------|
| Read Committed     | Да               |
| Snapshot Isolation | Да, явный opt-in |
| Serializable (SSI) | **По умолчанию** |

По умолчанию: **Serializable (SSI)**. Изоляция задаётся при `BEGIN`:

```
BEGIN;                                  -- SERIALIZABLE
BEGIN ISOLATION LEVEL SNAPSHOT;         -- явное понижение
BEGIN ISOLATION LEVEL READ COMMITTED;
BEGIN READ ONLY;                        -- см. ниже
```

Начальный sync persistent-режим ограничивает приватный atom-delta write-set одной транзакции
256 МиБ (`FG_TX_WRITE_SET_MAX_BYTES`). Это консервативная рекурсивная оценка owned payload, включая
properties, строки и составные `RecordId`, topology/adjacency и вложенные графы, а не
allocation-time heap quota. Она проверяется в начале `COMMIT`: после проверки liveness, но до
commit lock, conflict validation, кодирования страниц, durable I/O и публикации. Превышение
завершает транзакцию `FG-54F07`; уже выделенная transaction-private память освобождается при
отбрасывании транзакции, а видимое или durable состояние не меняется. Неограниченный private spill
не входит в текущий контракт. Atomic Import ограничивает private work
`SEGMENT SIZE` и byte-budget, но не публикует внутренние segments: видимый commit ровно
один и выполняется после успешного `END IMPORT`.

В Rust API обычная `mutate_graph` является bounded atom-delta операцией и не материализует локальный
граф целиком. Полная загрузка семейства доступна только через явно названную
`edit_graph_materialized`; прежняя `edit_graph` сохраняется как deprecated compatibility alias.
До реализации delta-maintenance scalar indexes вызов `mutate_graph` для такого графа возвращает
ошибку до durable I/O, а не переключается скрыто на дорогой путь.

#### Почему serializable по умолчанию

Это отклонение от привычного «SI по умолчанию» продиктовано доменом движка.

**1. Одной проверки против SI-снимка недостаточно для структурных инвариантов графа.**
`DEFINE GRAPH ... TYPE DAG | TREE | BIPARTITE | SIMPLE` ([§3.9](#39-ограничения-типов-графов))
проверяется и на мутации, и повторно на объединённом результате commit. Без второй проверки две
конкурентные транзакции, каждая добавляющая по ребру, независимо прошли бы локальную проверку:

```
Граф: DAG, есть путь a → b.
T1: RELATE (b)-[:E]->(c)   -- проверка: цикла нет ✓
T2: RELATE (c)-[:E]->(a)   -- проверка: цикла нет ✓
Обе коммитятся → появился цикл a → b → c → a. Граф больше не DAG.
```

Это классический write skew. Ferros-Grynn не разрешает ему ослабить graph family: commit обязан
слить непересекающиеся изменения поверх актуального root и повторно проверить структурный
инвариант при **любом** уровне изоляции. Serializable дополнительно защищает произвольные
инварианты приложения, которые движок не может вывести из типа графа.

**2. Цена serializable здесь мала.** Движок встраиваемый и однопроцессный: конкурентных
пишущих транзакций мало. Конфликты учитываются по graph/atom version, а phantom — точным
predicate matcher над изменившимися после snapshot атомами.

**3. Более слабый default не даёт полезной гарантии.** Если приложение действительно допускает
write skew, оно может явно выбрать SI/RC; молчаливо отдавать корректность ради гипотетического
выигрыша нельзя.

Современные движки, способные в serializable, идут тем же путём (CockroachDB, FoundationDB, YugabyteDB).

#### Понижение уровня

`ISOLATION LEVEL SNAPSHOT` остаётся доступен явно — для массовых загрузок и аналитики, где write
skew прикладных данных заведомо неприменим, а лишний учёт нежелателен. Встроенные структурные
инварианты graph family остаются жёсткими; `ASSERT INVARIANTS` нужен только для явно заданных
пользовательских ограничений, которые загрузчик решил отложить.

#### Транзакции только для чтения

`BEGIN READ ONLY` объявляет транзакцию не изменяющей данные. Immutable root с глобальным
`commit_ts` является логической частью *safe snapshot*: read-only транзакция сериализуется в
точке своего snapshot, не получает serialization abort и не вызывает откат писателей. В
persistent-режиме тот же непрозрачный snapshot также фиксирует durable `visible_lsn`; WAL-версия
видима только при `commit_lsn ≤ visible_lsn`. `commit_ts` и LSN — разные глобальные часы и друг с
другом не сравниваются. Максимальный возраст snapshot по умолчанию — 300 секунд; превышение
завершает транзакцию `FG-25F01`, чтобы удерживаемые roots и WAL watermark не блокировали очистку
истории. Это штатный режим аналитических обходов и пагинации
([§10.6](#106-limit--start)).

### 17.2. Автотранзакции

Каждый top-level statement вне явной транзакции выполняется в собственной автотранзакции.
Весь текст сначала проходит parse и recursive structural validation. Затем statement
анализируется по актуальному catalog и исполняется; его результат содержит собственные
`kind`, `elapsed` и `FerrosGrynnResult<Outcome<Value>>`. Analysis/runtime/commit error
останавливает программу и откатывает только текущий statement, не отменяя ранее опубликованные.

`Datastore::execute` использует временную session. Для долгоживущего контекста
`Datastore::session()` возвращает `GqlSession`, которая хранит program bindings и может
удерживать явную транзакцию между несколькими вызовами `execute`. Parse/structural error
возвращается внешним `FerrosGrynnResult` до первого statement; ошибка уже разобранного statement
помещается в его `QueryResult`.

### 17.3. GraphView

В рамках одного snapshot консистентно видятся global AtomStore и **все** графы, а не только один graph occurrence.
Serializable/SI удерживают snapshot на всю транзакцию; Read Committed фиксирует новый snapshot
на каждый statement, но не меняет его внутри statement:
- Все чтения `MATCH`, `SELECT`, traversal видят один snapshot — в том числе при обращении к нескольким графам в одном запросе ([§17.7](#177-транзакции-над-несколькими-графами)).
- Мутации (`CREATE`, `UPDATE`, `DELETE`, `RELATE`) видны внутри транзакции немедленно; вне — после `COMMIT`.
- Traversal не видит собственные мутации в рамках одного шага, если не задан режим `READ YOUR WRITES`.

`SnapshotId` непрозрачно идентифицирует MVCC-версию, но не является графом. Для persistent
backend он соответствует `StorageSnapshot { commit_ts, visible_lsn }`. Snapshot-bound
`GraphReadView<'tx>` — guard чтения с lifetime транзакции:

- для full in-memory он удерживает корень MVCC-версии;
- для disk-backed он содержит `GraphRef`, `SnapshotId` и доступ через `PageManager`; это не
  срез памяти и не копия графа;
- view и полученные из него borrowed atom/subgraph views не могут пережить транзакцию.

Самостоятельный публичный `GraphSnapshot`, смысл которого зависит от backend, не вводится.
`RETURN GRAPH` и `query_to_memory` материализуют собственное представление и не удерживают
исходный snapshot.

### 17.4. Конфликты

Обнаружение конфликтов работает над **глобальным** порядком коммитов базы данных, поэтому корректно и для транзакций, затрагивающих несколько графов.

| Тип                    | Поведение                                                              |
|------------------------|------------------------------------------------------------------------|
| Write-write            | Abort второй транзакции (first-committer-wins)                         |
| Read-write (SSI, деф.) | Abort, если validation не может сериализовать rw-антизависимость       |
| Phantom (SSI, деф.)    | Abort при изменении истины точного предиката на затронутой строке      |
| Read-write (явный SI)  | Не обнаруживается (возможен write skew, [§17.1](#171-уровни-изоляции)) |

### 17.5. Timeout

```
BEGIN;
  SELECT * FROM person TIMEOUT 10s;
COMMIT;
```

Или на уровне отдельного оператора. Эффективный deadline — более ранний из clause и
`QueryExecutionOptions`. `Datastore`, `GqlSession` и `Transaction` предоставляют
`execute_with_options`; `QueryCancellation` является cloneable cooperative token.

Проверка выполняется до и после statement/operator, перед каждым cursor pull, на loop backedge
и каждые 1024 элемента collection built-in. Синхронный системный I/O прерывается после возврата
вызова. Timeout/cancellation не перехватываются `TRY/CATCH`, прекращают дальнейшее исполнение и
откатывают активную явную транзакцию. Defaults: timeout отсутствует, control depth `20`, общий
loop budget одного top-level statement `1_000_000`, cancellation отсутствует.

### 17.6. ACID-гарантии

| Свойство    | Реализация                                                                                                              |
|-------------|-------------------------------------------------------------------------------------------------------------------------|
| Atomicity   | Режим 1: одна публикация immutable root; persistent-режим: frame-WAL + маркер `TxCommit`                                |
| Consistency | `ASSERT` на полях, схема, referential checks, инварианты типов графов                                                   |
| Isolation   | MVCC + SSI (по умолчанию) / SI / RC                                                                                     |
| Durability  | Только persistent-режим: buffered WAL `flush + sync_data` перед ответом; чистый in-memory режим живёт до конца процесса |

Атомарность не требует UNDO-фазы: изменения транзакции живут во фреймах WAL и становятся видимыми только вместе с маркером коммита, поэтому откат — это отбрасывание фреймов, а не обратное применение операций.

### 17.7. Транзакции над несколькими графами

Транзакция может содержать операторы, затрагивающие global atoms и произвольное число графов в пределах **одной базы данных**. Такая транзакция **атомарна**: либо применяются изменения во всех затронутых atoms, graph occurrences и каталоге схемы, либо ни в одном.

В in-memory режиме гарантия обеспечивается одной атомарной публикацией root базы; в persistent
режиме — единым журналом уровня базы данных с глобальным монотонным `LSN`. В обоих случаях
`commit_ts` глобален. Все чтения транзакции — в любом числе графов — выполняются на **одном**
снимке: `commit_ts` образуют единый логический порядок для MVCC/конфликтов, а persistent
`visible_lsn` задаёт единый durable-префикс WAL
([§17.4](#174-конфликты)).

Это не деталь реализации, а необходимое условие: если бы счётчики версий были локальными для
графа, они были бы попарно несравнимы, и предикат видимости `version.commit_ts ≤
snapshot.commit_ts` для версии из другого графа не имел бы смысла. Для disk page version
отдельно действует `frame.commit_lsn ≤ snapshot.visible_lsn`. Кросс-граф чтение
(несколько `MATCH ... IN GRAPH ...` в одном statement, `BUILD GRAPH x FROM MATCH ... IN GRAPH y`)
— штатная возможность языка, поэтому оба
глобальных порядка обязательны независимо от того, разрешена ли кросс-граф запись.

```
BEGIN;
  BUILD GRAPH <dag> build_order
    FROM MATCH (Task AS t)-[BEFORE AS b]->(Task AS t2)
         IN GRAPH tasks RETURN t, b, t2
    NODES [t, t2] EDGES [b];
  REMOVE GRAPH build_order_old;
COMMIT;   -- оба графа и каталог обновляются атомарно
```

`DEFINE GRAPH`, `BUILD GRAPH` и `REMOVE GRAPH` участвуют в транзакции наравне с DML: созданный граф становится видимым в каталоге только после `COMMIT`, удалённый — исчезает из каталога тогда же.

**Граница домена.** Транзакция **не может** затрагивать более одной базы данных или более одного namespace. Такой запрос отвергается статически на этапе планирования с ошибкой `CrossDatabaseTransaction`. Это сознательное ограничение: база данных — единица журналирования и восстановления, и растягивать транзакцию за её пределы означало бы вводить двухфазный коммит между доменами, которые в одном процессе всё равно отказывают одновременно.

**Классы, атомы и графы.** Class membership задан первой частью `RecordId` и не является
отдельным изменяемым отношением. Глобальный атом и его base properties входят в database-level
snapshot независимо от графов. Graph membership, topology, label и sparse overlay входят в тот
же commit как graph-local state. Одна транзакция может атомарно изменить base atom и несколько
его occurrences, однако обязана проверить все затронутые base/effective constraints до
публикации. `GraphId = 0` зарезервирован за database-level `DataSpaceId::Database` и не обозначает
системный граф; пользовательские графы всегда имеют ненулевой `GraphId`.

### 17.8. Режимы исполнения и детерминизм

Порядок результата без `ORDER BY` не определён ([§23.5](#235-зафиксированные-инварианты), инвариант 4), а traversal, параллельное исполнение и cost-based выбор плана дополнительно вносят недетерминизм. Для сценариев, где нужна воспроизводимость — тесты, отладка, обучение моделей, сравнение прогонов — предусмотрены режимы исполнения.

```
BEGIN [ ISOLATION LEVEL @level ] [ READ ONLY ]
      [ EXECUTION MODE DETERMINISTIC | BEST_EFFORT | FAST ];
```

Режим задаётся на транзакцию. В исполнимом профиле этапа 14 поддержаны `BEST_EFFORT`
(default) и `DETERMINISTIC`. `FAST` возвращает typed unsupported до открытия транзакции и
принадлежит этапу 17.

| Режим                        | Гарантии                                                                                                                                                                                                                                                                                                                 |
|------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `BEST_EFFORT` (по умолчанию) | Результат детерминирован по содержимому, но порядок без `ORDER BY` не гарантирован; план может меняться между запусками                                                                                                                                                                                                  |
| `DETERMINISTIC`              | Строгая воспроизводимость при одинаковых snapshot, canonical parameters, user seed и engine build: стабильный порядок, канонический plan и фиксированный порядок floating-point reduction. Без `ORDER BY` результат доупорядочивается stable public identity. `time::now()` и другие недетерминированные/IMPURE функции отклоняются до чтения данных; scalar `rand()`/`uuid()` остаются неисполняемыми до этапа 21 |
| `FAST`                       | Максимум производительности, никаких гарантий порядка и стабильности плана                                                                                                                                                                                                                                               |

`DETERMINISTIC` допустим только при `SNAPSHOT` и `SERIALIZABLE`; `READ COMMITTED` запрещён.
Effective seed включает version, normalized query fingerprint, canonical parameter fingerprint,
`SnapshotId` и user seed из `QueryExecutionOptions::deterministic_seed`. `PlanHash` вычисляется
RapidHash V3 по versioned canonical physical-plan bytes: `DefaultHasher`, `Debug` output и порядок
итерации hash map для него не используются.

Детерминизм не бесплатен: он может требовать стабильной сортировки и ограничивает выбор
исполнения, поэтому default остаётся `BEST_EFFORT`. Технические детали —
[gql_tech_spec.md §15](gql_tech_spec.md).

Для повторного воспроизведения прогона достаточно набора: текст запроса, `SnapshotId`, seed, `PlanHash` и версия движка.

---

## 18. Функциональная и объектная системы

### 18.1. Классификация функций

| Класс       | Тип                       | Пример                |
|-------------|---------------------------|-----------------------|
| `scalar`    | $`(T_1, T_2, \dots) → U`$ | `string::upper(s)`    |
| `aggregate` | `Stream<T> → U`           | `count()`, `sum(x)`   |
| `traversal` | Шаг pipeline              | `tv::out(['FRIEND'])` |
| `query`     | Возвращает подзапрос      | UDF с `SELECT` внутри |
| `mutating`  | Побочные эффекты          | UDF с `CREATE` внутри |

### 18.2. Встроенные пространства имён

| Пространство | Исполнимо в этапе 12 | Отложено |
|---|---|---|
| `math::` | `abs`, `sqrt`, `pow`, `floor`, `ceil` | `stddev`, `variance` и агрегаты — этап 13 |
| `string::` | `upper`, `lower`, `len`, `contains`, `split`, `concat` | regex/stateful helpers — этап 21 |
| `time::` | `now`, `format`, `add`, `diff`; `now()` один на statement | оставшийся catalog — этап 21 |
| `array::` | `len`, `concat`, `distinct`, `first`, `flatten` | `group` — этап 13; `push/pop/clear` — этап 21 |
| `set::` | `len`, `union`, `intersect`, `diff`, `has` | `add/clear` — этап 21 |
| `type::` | `is<T>`, `to<T>` | — |
| `meta::` | `id`, `class`, `type` | — |
| `option::` | `is_some`, `is_none` | — |
| `graph::` / path / pipeline | — | этапы 16–17 |
| UDF / closures / `regex::` | — | этап 21 |
| `search::` | — | этап 22 |
| `geo::` | — | этап 24 |
| `computed::` | — | назначенный schema/query этап |

Этап 12 использует закрытый статический enum/signature table и обычный `match`: runtime
registry функций не создаётся. `array::distinct` сохраняет первое вхождение, `flatten` снимает
один уровень, `first([])` возвращает `none`, set-операции выдают канонически упорядоченный set.
Domain/overflow/type errors используют общую typed value-error систему. Контекстный
`decimal → float` добавляет notification о возможной потере точности.

### 18.3. Определение пользовательских функций

```
DEFINE PURE FUNCTION fn::calc_weight(a: record<person>, b: record<person>) -> float {
    LET $common = (SELECT count() FROM friendship
                   WHERE in = a.id AND out = b.id);
    RETURN $common / (a.friends_count + b.friends_count);
};
```

### 18.4. Замыкания

```
|$x| $x.age > 18                     -- bool замыкание
|$x, $acc| $acc + $x.weight             -- аккумулирующее замыкание
|$x| { LET $y = $x.age * 2; RETURN $y }  -- блочное замыкание
```

Тип: $`closure<(T_1, T_2, \dots) → U>`$.

### 18.5. Purity

- `PURE` функции: детерминированы, без побочных эффектов — планировщик может инлайнить, кешировать, переставлять.
- `IMPURE` функции: имеют побочные эффекты — планировщик фиксирует порядок.
- `STABLE` (подкласс PURE): детерминированы в рамках одного запроса, но не между запросами (например, `time::now()` закреплено на время запроса).

### 18.6. Безопасность

- Рекурсия требует явного ограничения: `MAXDEPTH n`.
- Бесконечные циклы запрещены: `FOR` и `tv::repeat` требуют `times(n)` или `until(condition)` с конечным критерием.

### 18.7. Перегрузка

Разрешена только для числового типа `number` — без необходимости дублировать реализацию для `int`, `float`, `decimal`. 
Разрешение типа `number` происходит на этапе анализа запроса. Иные перегрузки не разрешены.

### 18.8. Конструкторы объектов

Многие объекты поддерживают встроенные конструкторы: неявные или через каст.

| Тип объекта | Конструктор из другого типа                           |
|-------------|-------------------------------------------------------|
| `array`     | `set`, `string`                                       |
| `set`       | `array`                                               |
| `bool`      | `T \| null`, `option<T>`, any empty collection        |
| `int`       | `float`, `decimal`, `bool`, `number`                  |
| `float`     | `int`, `decimal`, `bool`, `number`                    |
| `decimal`   | `int`, `float`, `bool`, `number`                      |
| `number`    | `int`, `float`, `decimal`, `bool`                     |
| `bytes`     | `int`, `float`, `decimal`, `bool`, `number`, `string` |
| `string`    | `bytes`, `array<string>` (concatination)              |
| `datetime`  | `string`, `int` (Unix timestep)                       |
| `duration`  | `string`, `int` (Unix timestep)                       |
| `any`       | any type                                              |
| `object`    | `array<object>`, `set<object>`                        |
| `option<T>` | any `T`, `none`, `null`                               |
| `regex`     | `string`                                              |
| `geometry`  | `object`                                              |
| `formatter` | `string`                                              |

---

## 19. Индексы

Подробный синтаксис — в §9.6. Здесь — семантика.

### 19.1. Видимость и консистентность

- Primary и secondary индексы: транзакционно-консистентны (обновляются в той же транзакции).
- HNSW и FULLTEXT: eventually consistent опционально; транзакционная консистентность — при явном указании.
- `WITH INDEX source.index [, ...]` задаёт candidate set для каждого явно названного source.
  Планировщик обязан выбрать ровно один совместимый transaction-visible индекс в состоянии
  `Ready` из набора этого source. Одно неqualified имя допустимо только при однозначной
  принадлежности source.
- Missing, `stale`, prepared, wrong-scope или несовместимый forced index даёт typed planning
  error до row pull. Без hint та же ситуация наблюдаемо деградирует к корректному scan.
- `WITH NOINDEX` запрещает user secondary indexes, но не primary, class и membership structures.

### 19.2. Graph-специфические индексы
- Adjacency index — индекс смежности для ускорения `out/in/both`.
- Reachability index — предвычисленная достижимость.
- Neighbourhood index — индекс k-хоп окрестностей.

---

## 20. Анализаторы текста

Анализатор задаёт pipeline обработки текста при индексировании и поиске.
Он определяется названием и наборами токенов и фильтров.

### 20.1. DEFINE ANALYZER

```
DEFINE ANALYZER 
    [ OVERWRITE | IF NOT EXISTS ] @name 
    [ FUNCTION @function ] 
    [ TOKENIZERS @tokenizers ] 
    [ FILTERS @filters ] 
    [ COMMENT @string ]
;
```

- ```FUNCTION``` — позволяет определить пользовательскую функцию на входных данных.
- ```TOKENIZERS``` — токенизаторы отвечают за разбиение текста на отдельные токены в соответствии с набором инструкций.
- ```FILTERS``` — преобразуют токены для дальнейшей обработки и анализа.

### 20.2. Токенизаторы

| Токенизатор | Описание                                                             |
|-------------|----------------------------------------------------------------------|
| `blank`     | Разбивает по пробелам, табуляции, переводам строки                   |
| `camel`     | Разбивает при переходе к заглавной букве (camelCase)                 |
| `class`     | Разбивает при смене класса символов Unicode (цифры/буквы/пунктуация) |
| `punct`     | Разбивает по знакам пунктуации                                       |

Подробнее:
#### `blank`
Разбивает текст на токены, создавая новый токен при каждом появлении пробела, табуляции или символа новой строки. 
Это простой способ разделить текст на слова или фрагменты по пробелам.

```
DEFINE ANALYZER example_blank TOKENIZERS blank;
search::analyze("example_blank", "hello world");

-- Output
[
	'hello',
	'world'
]
```

#### `camel`
Используется для определения и создания токенов, когда следующий символ в тексте — заглавная буква. 
Это особенно полезно при обработке текста в стиле camelCase или PascalCase, распространенном в программировании, для разделения текста на осмысленные слова.

```
DEFINE ANALYZER example_camel TOKENIZERS camel;
search::analyze("example_camel", "helloWorld");

-- Output
[
	'hello',
	'World'
]
```

#### `class`
Разбивает текст на токены, определяя изменения (цифры, буквы, знаки препинания, пробелы) в классе символов Юникода. 
При смене класса символов создается новый токен, при этом различаются цифры, буквы, знаки препинания и пробелы. 
Это обеспечивает гибкую токенизацию на основе типов символов.

```
DEFINE ANALYZER example_class TOKENIZERS class;
search::analyze("example_class", "123abc!XYZ");

-- Output
[
	'123',
	'abc',
	'!',
	'XYZ'
]
```

#### `punct`
Генерирует токены, разбивая текст при встрече с символом препинания. 
Он подходит для токенизации предложений или разбиения текста на более мелкие части по знакам препинания.

```
DEFINE ANALYZER example_punct TOKENIZERS punct;
search::analyze("example_punct", "Hello, World!");

-- Output
[
	'Hello',
	',',
	'World',
	'!'
]
```

### 20.3. Фильтры

| Фильтр                | Описание                                                                                        |
|-----------------------|-------------------------------------------------------------------------------------------------|
| `nfc`                 | Unicode-нормализация к форме NFC (канонический состав)                                          |
| `nfkc`                | Unicode-нормализация к форме NFKC (совместимый состав; складывает лигатуры, надстрочные и т.п.) |
| `ascii`               | Заменяет диакритику на ASCII-эквиваленты                                                        |
| `lowercase`           | Приводит к нижнему регистру                                                                     |
| `uppercase`           | Приводит к верхнему регистру                                                                    |
| `edgengram(min, max)` | Генерирует префиксные n-граммы (для автодополнения)                                             |
| `ngram(min, max)`     | Генерирует n-граммы заданного диапазона                                                         |
| `snowball(language)`  | Стемминг Snowball (поддерживаемые языки: english, russian, french, italian и др.)               |
| `mapper(path)`        | Лемматизация по словарю                                                                         |

Подробнее:
#### `ascii`
Отвечает за обработку токенов путем замены или удаления диакритических знаков (акцентов и специальных символов) из текста. 
Он помогает стандартизировать текст, заменяя символы с диакритическими знаками на их базовые эквиваленты в кодировке ASCII, что делает его более удобным для различных задач по анализу текста.

```
DEFINE ANALYZER example_ascii TOKENIZERS class FILTERS ascii;
search::analyze("example_ascii", "résumé café");

-- Output
[
	'resume',
	'cafe'
]
```

#### ```edgengram(min,max)```
Используется для создания токенов, представляющих собой префиксы терминов.
Он генерирует последовательность токенов, которые постепенно формируют термин,
что может быть полезно для автозаполнения или поиска по частичным словам.

Он принимает два параметра min и max, которые определяют минимальное и максимальное количество символов в префиксе.

```
DEFINE ANALYZER example_edgengram TOKENIZERS class FILTERS edgengram(1,3);
search::analyze("example_edgengram", "apple banana");

-- Output
[
	'a',
	'ap',
	'app',
	'b',
	'ba',
	'ban'
]
```

#### ```mapper(path)``` 
Предназначен для лемматизации.

Лемматизация — это процесс приведения слов к их словарной форме. 
Механизм сопоставления позволяет пользователям указать собственный файл словаря, в котором термины сопоставляются с их словарными формами. 
Этот файл словаря затем используется анализатором Ferros-Grynn для стандартизации терминов при их индексации, что повышает согласованность поиска. 
Файлы с лемматизацией легко собрать и найти в интернете, что позволяет настраивать полнотекстовый поиск для небольших языков.

Файл, указанный в параметре mapper, должен иметь следующий формат:
- Каждая строка содержит пару терминов, разделённых табуляцией.
- Первый термин представляет собой каноническую (исходную) форму слова.
- Второй термин — это форма, которая должна быть сопоставлена с базовой формой.

Пример формата файла:
```
drive driven
drive drives
drive driving
drive drove
swim swam
swim swimming
swim swims
swim swum
```

При применении этого анализатора к тексту любое слово, соответствующее сопоставленному термину в файле словаря, 
перед индексацией заменяется на базовую форму. Это помогает обеспечить единообразие результатов поиска 
за счет объединения различных форм слова в единую стандартизированную запись.

С помощью этого пользовательского картографа на основе словаря вы можете контролировать индексацию неправильных форм 
и других вариантов терминов, делая поиск более предсказуемым и полным.

Маппер также можно использовать для выборочной фильтрации, если в файле, на который есть ссылка, есть два отдельных слова, разделенных табуляцией. 
Рассмотрим, например, следующий файл:
```
// error_filter.txt

NOT_FOUND	File_not_found
NOT_FOUND	Datei_nicht_gefunden
NOT_FOUND	Fichier_non_trouvé
TIMEOUT	Timed_out
TIMEOUT	Délai_expiré
TIMEOUT	Zeitüberschreitung
```
Анализатор, использующий один фильтр сопоставления, может применять этот лемматизатор для объединения многоязычных сообщений об ошибках в единый вывод.
```
DEFINE ANALYZER error_filter FILTERS mapper('error_filter.txt');

LET $messages = array::join(
   array::map(
      ["File not found", "Datei nicht gefunden", "Zeitüberschreitung"],
      |$word| string::replace($word, ' ', '_')
   ),
   ' '
)
search::analyze("error_filter", $messages);

-- Output
[
	'NOT_FOUND',
	'NOT_FOUND',
	'TIMEOUT'
]
```

Также здесь можно использовать pipeline-оператор для обработки массива:

```
LET $messages = $words
    |> array::map(|$w| string::replace($w, ' ', '_'))
    |> array::join(' ');
```

#### ```ngram(min,max)``` 
Используется для создания последовательности из n токенов на основе заданного фрагмента текста или речи. 
В зависимости от задачи это могут быть слоги, буквы, слова или пары оснований.

Фильтр принимает два параметра min и max, которые указывают, что нужно создать n-граммы от минимального до максимального размера.

```
DEFINE ANALYZER example_ngram TOKENIZERS class FILTERS ngram(1,3);
search::analyze("example_ngram", "apple banana");

-- Output
[
	'a',
	'ap',
	'app',
	'p',
	'pp',
	'ppl',
	'p',
	'pl',
	'ple',
	'l',
	'le',
	'e',
	'b',
	'ba',
	'ban',
	'a',
	'an',
	'ana',
	'n',
	'na',
	'nan',
	'a',
	'an',
	'ana',
	'n',
	'na',
	'a'
]
```

#### ```snowball(language)``` 
Применяет стемминг Snowball к токенам, сводя их к корневой форме, и преобразует регистр в нижний.
В качестве параметра для Snowball можно указать следующие поддерживаемые языки:
арабский, датский, голландский, английский, французский, немецкий, греческий, венгерский, итальянский,
норвежский, португальский, румынский, русский, испанский, шведский, тамильский, турецкий.

В будущем планируется возможность добавлять конфигурации для новых языков.

```
DEFINE ANALYZER english_snowball TOKENIZERS class FILTERS snowball(english);
DEFINE ANALYZER german_snowball TOKENIZERS class FILTERS snowball(german);

RETURN [
    search::analyze("english_snowball", "Looking at some running cats"),
    search::analyze("german_snowball", "Sollen wir was trinken gehen?")
];

-- Output
[
	[
		'look',
		'at',
		'some',
		'run',
		'cat'
	],
	[
		'soll',
		'wir',
		'was',
		'trink',
		'geh',
		'?'
	]
]
```

### 20.4. Pipeline анализатора

Обработка текста: `input → tokenizer(s) → filter(s) → (optional) custom fn → tokens`.

Отдельной стадии «нормализаторов» нет — нормализация выражается фильтрами. Для приведения Unicode к канонической форме предусмотрены фильтры `nfc` и `nfkc`; их следует применять **до** `lowercase`/`ascii`/`snowball`, иначе визуально одинаковые строки в разных формах записи дадут разные токены.

---

## 21. События и Live-запросы

### 21.1. LIVE SELECT

Непрерывная подписка на изменения. Возвращает живой поток событий изменений.

LIVE SELECT позволяет отслеживать создание, обновление и удаление atoms выбранных classes или
изменения graph-local occurrences.

```
[LET $id =] LIVE SELECT
	[
		[ VALUE ] @fields ... [ AS @alias ]
		| [ DIFF | PATCH ]
	]
	FROM @targets
	[ WHERE @conditions ]
	[ FETCH @fields ... ]
	[ SINCE @versionstamp ]
	[ ON OVERFLOW GAP | DISCONNECT | BLOCK ]
	[ BUFFER @number ]
;
```

Оператор ```LIVE SELECT``` можно использовать для выбора данных class или graph source в режиме
реального времени, в том числе с возможностью применения фильтров.

С практической точки зрения, когда вы выполняете ```LIVE SELECT``` запрос, запускается непрерывный сеанс,
в ходе которого фиксируются все последующие изменения данных в режиме реального времени.
Эти изменения немедленно передаются клиенту, благодаря чему клиент всегда получает актуальные данные.

Режимы результата: 
- полные записи; 
- `VALUE ...` — только изменённые поля; 
- `DIFF` — diff запись;
- `PATCH` - JSON Patch.

`$id` — идентификатор live-запроса; используется в `KILL`.

#### 21.1.1. Гарантии доставки

Движок встраиваемый и однопользовательский, но подписок может быть несколько. Поэтому приоритет — **гарантированная доставка и предсказуемый порядок**, а не пропускная способность.

| Свойство    | Гарантия                                                                                                                                                   |
|-------------|------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Порядок     | **Тотальный** по `versionstamp` в пределах подписки. `versionstamp` — глобальный монотонный счётчик, тот же, что у changefeed ([§21.3](#213-show-changes)) |
| Доставка    | **At-least-once** с подтверждением. Событие удаляется из очереди подписки только после `ack` клиента                                                       |
| Дубли       | Возможны при переподключении. Каждое событие несёт `versionstamp` — клиент делает дедупликацию по нему (идемпотентное применение)                          |
| Потери      | **Невозможны молча.** Любой разрыв последовательности явно помечается событием-разрывом (см. ниже)                                                         |
| Атомарность | События одной транзакции доставляются подряд и помечены общим `tx_id`; частичная видимость транзакции исключена                                            |
| Видимость   | Событие публикуется **после** коммита транзакции. Откатившиеся транзакции не порождают событий                                                             |

#### 21.1.2. Возобновление подписки

`SINCE @versionstamp` возобновляет подписку с указанной позиции вместо «с этого момента». Клиент хранит `versionstamp` последнего применённого события и передаёт его при переподключении:

```
LET $sub = LIVE SELECT DIFF FROM person WHERE active = true SINCE $last_seen;
```

История берётся из changefeed соответствующего class/graph source, поэтому `SINCE` работает лишь
в пределах `CHANGEFEED @duration`, заданного его DDL. Если запрошенная позиция уже вычищена,
возвращается `ChangefeedExpired` — клиенту следует перечитать состояние целиком и подписаться
заново. Без changefeed у source `SINCE` недоступен, и подписка начинается с текущего момента.

#### 21.1.3. Backpressure и переполнение

У каждой подписки — своя очередь, ограниченная `BUFFER @number` событий (по умолчанию `FG_LIVE_QUEUE_CAPACITY`). Медленный подписчик не должен ни блокировать писателя, ни терять события молча. Поведение при переполнении задаётся `ON OVERFLOW`:

| Режим                | Поведение                                                                                                                                                                                                                            |
|----------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `GAP` (по умолчанию) | Очередь усекается, подписке доставляется служебное событие `{ kind: "gap", from: @vs_last_delivered, to: @vs_now }`. Клиент знает точный диапазон пропущенного и может добрать его через `SHOW CHANGES ... SINCE @vs_last_delivered` |
| `DISCONNECT`         | Подписка закрывается со статусом `LiveSubscriberTooSlow` и последним доставленным `versionstamp`; клиент переподключается через `SINCE`                                                                                              |
| `BLOCK`              | Писатель ждёт освобождения места в очереди. Гарантирует отсутствие разрывов ценой замедления записи. Применять осознанно: одна зависшая подписка тормозит все мутации                                                                |

`GAP` выбран по умолчанию как единственный режим, который одновременно не теряет данные молча и не даёт подписчику влиять на скорость записи.

#### 21.1.4. Стоимость и ограничения

- Фильтр `WHERE` подписки вычисляется на каждой мутации подходящего class/graph source; сложные предикаты и `FETCH` удорожают запись. Предикат подписки вычисляется по IR (`filter_ir`), а не переразбирается.
- Состояние подписки (`live_id`, цель, `filter_ir`, режим вывода, последний подтверждённый `versionstamp`) персистентно и переживает перезапуск; сами очереди — в памяти и после перезапуска восстанавливаются через `SINCE`.
- Порядок между **разными** sources не гарантируется, если подписка не охватывает их одной целью: `versionstamp` глобален, но доставка идёт по подпискам независимо.

#### 21.1.5. Материализация live-графа

Сам `LIVE SELECT` возвращает поток событий, как указано выше. Rust SDK может применить этот
поток к материализованному `LiveGraph<State>`:

- `LiveGraph<Frozen>` публикует каждую согласованную revision атомарно; read-guard всегда видит
  одну неизменяемую версию, а ранее полученные guards продолжают читать прежнюю;
- обычный query result остаётся статическим `Graph<Frozen>` и сам не обновляется;
- `LiveGraph<Mutable>` создаётся только явным преобразованием и ведёт локальный dirty-set.

Для mutable live-графа непересекающиеся remote changes могут применяться автоматически. Если
incoming change пересекается с локальным dirty-set, подписка **приостанавливается**, remote delta
не применяется и не коммитится. Клиент получает отдельное conflict-событие с incoming changes,
local changes, множеством конфликтов и remote revision, после чего явно выбирает
`accept_remote`, `keep_local`, `discard_local`, `rebase`, `flush` или `resume`. После разрешения
движок повторно проверяет структурные инварианты graph family. Одновременная локальная и
query-based мутация без такого разрешения запрещена.

### 21.2. KILL

Завершает live-запрос по id.

```
KILL $query_id;
```

### 21.3. SHOW CHANGES

Требует `CHANGEFEED duration` в определении соответствующего CLASS или GRAPH.

```
SHOW CHANGES FOR CLASS @classname
    SINCE @timestamp | @versionstamp
    [ LIMIT @number ]
;
```

С помощью каналов изменений можно получать и синхронизировать изменения из Ferros-Grynn с внешними системами и платформами с помощью инструкции SHOW.
Инструкцию SHOW можно использовать для воспроизведения изменений, внесённых в class source.

Возвращает объект с `versionstamp` и `changes`, внутри которого описывается список событий:
`define_class`, `create`, `update`, ... с указанием имени / id.

### 21.4. События (DEFINE EVENT)

Подробный синтаксис — §9.5. 

Гарантии:
- Синхронные события (`без ASYNC`) — выполняются в рамках одной транзакции и могут привести к её откату при ошибке.
- Асинхронные события (`ASYNC`) — ставятся в очередь; при сбое — повторяются `RETRY @n` раз; глубина вложенности событий ограничена `MAXDEPTH`.
- Асинхронные события не имеют гарантии exactly-once по умолчанию; для at-least-once нужна явная идемпотентность.

#### 21.4.1. Автогенерация AtomWalk-рёбер

С помощью событий, прикреплённых к `RELATE` запросу, можно автоматически создавать рёбра для V-E и E-V инцидентности, 
полезные при AtomWalk режиме. Разумеется, V-V рёбра тоже можно создавать таким образом.

```
DEFINE EVENT 
    [ OVERWRITE | IF NOT EXISTS ] 
    @name ON GRAPH @graph 
    [ ASYNC [ RETRY @n ] [ MAXDEPTH @n ] ]
    WHEN $event = "RELATE" [ @additional_filters ]
    THEN {
        @relate_stmt
    }
;
```

Внутри WHEN и THEN блоков доступны следующие параметры:
- `$before` и `$after` недоступны (равны `null`), так как RELATE вносит изменения в граф, а не в запись.
- `$value` имеет следующие идиомы:
  - `$value.endpoints` — endpoint'ы созданного ребра; для directed-ребра это объект с
    массивами `source` и `target`, для undirected — единый массив `source`;
  - `$value.edge` - ребро, участвовавшее в запросе;
  - все перечисленные идиомы являются ссылками, следовательно, материализуются только по запросу (например, при фильтрации по типу или в `@relate_stmt`).

Внутри `@relate_stmt` можно создавать V–E, E–V и E–E связи, если их endpoint-атомы уже
разрешены из `$value` или внешнего scope. Каждая такая связь создаёт отдельное обычное ребро.

---

## 22. Контракты планировщика

### 22.1. Архитектура

```
           Query
             |
           Parse
             |
          Analyze
             |
           Split
         /       \
   Declarative  Traversal
       /           \
     AST       Traversal IR
      |             |
   Typed AST      Logical IR
       \           /
Optimize (rule-based + cost-based)
             |
        Logical Plan
             |
        Physical Plan
             |
         Execution
             |
           Result
```

### 22.2. Логические операторы

Минимальный набор IR:

| Оператор                                               | Описание                             |
|--------------------------------------------------------|--------------------------------------|
| `Scan(source, filter?)`                                | Чтение из источника                  |
| `IndexScan(index, key_range, filter?)`                 | Чтение через индекс                  |
| `Filter(predicate)`                                    | Фильтрация                           |
| `Project(exprs)`                                       | Проекция                             |
| `Join(type, condition)`                                | Соединение (Inner, Left, Semi, Anti) |
| `Expand(dir, labels?, node_alias, edge_alias?)`        | Одношаговое расширение в графе       |
| `PathExpand(dir, labels?, min, max, mode, path_alias)` | Многошаговый обход                   |
| `Aggregate(group_by, agg_exprs)`                       | Агрегация                            |
| `Sort(keys)`                                           | Сортировка                           |
| `Limit(n, offset)`                                     | Ограничение                          |
| `Dedup(keys)`                                          | Дедупликация                         |
| `Unwind(expr, alias)`                                  | Разворачивание массива (SPLIT)       |
| `TraversalStep(step)`                                  | Один шаг traversal VM                |

### 22.3. Правила оптимизации

Этап 14 применяет правила в одном неизменном порядке:

1. Constant folding только для уже типизированных pure expressions.
2. Разложение conjunction без перестановки expressions, способных вернуть ошибку.
3. Predicate pushdown к единственному source.
4. Projection pruning с учётом join/group/order/fetch/output dependencies.
5. Late materialization.
6. Safe LIMIT pushdown только при отсутствии join/group/split/order/correlated subquery/ONLY.
7. Count rewrite для доказанно точной формы `count()` над одним source.
8. Sort elimination только при полном совпадении index ordering, null placement и stable tie key.

Rule pass не может менять LEFT JOIN, bag multiplicity, graph-effective property semantics,
порядок наблюдаемых ошибок или floating-point reduction order. Перестановка разрешена только для
последовательной группы INNER equality joins с pure predicates. LEFT/CROSS, correlated subquery,
explicit order и side-effect являются semantic barriers. OPTIONAL/path rewrites и новые
path-алгоритмы принадлежат этапам 15–17.

### 22.4. Cost model

Cardinality задаётся диапазоном `{ lower, expected, upper }`. Cost использует versioned integer
fixed-point representation, чтобы выбор плана не зависел от platform-sensitive `f64`.
Компоненты: startup, sequential rows, random lookups, expression CPU, peak memory, expected spill
и uncertainty/resource risk. Веса калибруются microprobes.

Ключевые факторы:
- `node_count`, `edge_count`, `label_cardinality`, `edge_label_cardinality`.
- `degree_stats`: `avg_out_degree`, `avg_in_degree`, degree histogram (`histogram_in`, `histogram_out`).
- `property_selectivity` (из histogram или статистики).
- Для текущего MATCH: degree distributions и label counters; полный path cost остаётся этапам
  15–17.

Histogram считается fresh при `revision == analyzed_revision && dirty_rows == 0`. До
`max(1024, ceil(population × 0.10))` dirty rows он используется с расширенной uncertainty; после
порога выбирается conservative estimate и warning `statistics_stale`. Отсутствующая статистика
даёт conservative estimate и warning `statistics_missing`. Approximate sample только расширяет
uncertainty и не влияет на correctness. Residual predicate выполняется после любого index access.

Без degree distribution MATCH-планировщик деградирует.

#### Cardinality estimation
- degree distribution
- label statistics
- histogram по свойствам

#### Traversal planning
- reorder steps (если safe)
- push filters before expand
- collapse steps

### 22.5. Selectivity model
```selectivity(predicate) ∈ (0, 1]```

Примеры:
- label = Person -> |Person| / |nodes|
- age > 18 -> histogram-based

### 22.6. Scan cost
```cost_scan = N * row_cost``` или с индексом: ```cost_index = log(N) + k```

### 22.7. Expand cost
```rows_out = rows_in * avg_degree * selectivity(edge_label)```

```cost_expand = rows_in * avg_degree * edge_cost```

Уточнение:
```
effective_degree =
    avg_degree(label, direction)
    * selectivity(edge_label)
    * selectivity(edge_predicate)
```

### 22.8. Join cost
#### HashJoin:
$```cost = build + probe \approx N + M```$

#### IndexJoin (предпочтительно):
```cost = outer_rows * index_lookup_cost```

### 22.9. PathExpand cost
Самая опасная часть.

$```rows(depth=d) \approx rows\_in * (avg\_degree^d)```$

Итого: $```total\_rows \approx \sum (degree^d)```$

Ограничения:
```
if degree > 10 and depth > 4:
    explosion risk = HIGH
```

### 22.10. Path pruning factors
```
effective_degree *=
    path_mode_factor *
    predicate_factor *
    dedup_factor
```

Примеры:
| Mode     | Factor |
| -------- | ------ |
| Walk     | 1.0    |
| Trail    | ~0.7   |
| Simple   | ~0.3   |
| Shortest | ~0.05  |

### 22.11. Cost penalties
Штрафы:
- penalty_high_degree_nodes
- penalty_unbounded_path
- penalty_no_index
- penalty_materialization

### 22.12. EXPLAIN-контракт

| Команда                     | Результат                             |
|-----------------------------|---------------------------------------|
| `EXPLAIN statement`         | Логический план                       |
| `EXPLAIN FULL statement`    | Физический план + стоимости           |
| `EXPLAIN ANALYZE statement` | Runtime-статистика (выполняет запрос) |

Plain/FULL не читают rows и не получают writer lease. ANALYZE исполняет wrapped statement ровно
один раз и сохраняет его обычную autocommit/private-root/rollback семантику. JSON-контракт и
перечень explainable statements определены в §7.6. Суффиксной формы `statement EXPLAIN` нет.

### 22.12. Замечания по WITHIN

Оператора `WITHIN` не требует join в общем смысле — структура вложенности физически детерминирована:
`NodeKind::Meta { subgraph: GraphId }` даёт O(1) переход от мета-вершины к её GraphId, 
а `MetaGraph::subgraph(gid)` — O(1) доступ к самому подграфу.

Компиляция `(Label AS x) WITHIN (Module AS mv)` должна разворачиваться планировщиком не в абстрактный предикат,
а в Expand-подобную операцию (§26.2): "взять mv, спуститься в graph::inner(mv), отфильтровать по Label".
Отличие от `FROM graph::inner(mv)` только в том, что это выражается предикатом внутри единого паттерна, 
а не сменой контекста всего запроса, и даёт планировщику больше свободы: он может решить начать с более селективной стороны, 
используя как `graph::inner`, так и `graph::parent` при необходимости.

---

## 23. Формальная семантика

### 23.1. Базовые множества

| Обозначение                        | Описание                                      |
|------------------------------------|-----------------------------------------------|
| $G = \langle V, MV, E, ME \rangle$ | Граф (вершины, метавершины, рёбра, метарёбра) |
| $\Sigma$                           | Пространство значений                         |
| $\mathcal{R}$                      | Множество записей                             |
| $\mathcal{P}$                      | Множество путей                               |

### 23.2. Окружение выполнения

$$\Gamma = (Snapshot, Bindings, Params)$$

- $Snapshot$ — MVCC-снимок.
- $Bindings: Var \to \Sigma \cup \mathcal{R}$ — отображение переменных.
- $Params$ — параметры запроса.

### 23.3. Денотационная семантика

Запрос $Q$ — функция:

$$[\![Q]\!]: \Gamma \to \mathcal{M}$$

где $\mathcal{M} = Bag(Record \cup Value \cup Path \cup SubGraph)$ — мультимножество результатов.

### 23.4. Ключевые семантические правила

**MATCH — graph homomorphism:**

$$[\![MATCH\ P]\!](\Gamma) = \{(a, b, \ldots) \mid \text{гомоморфизм } P \to G \text{ при snapshot } \Gamma.Snapshot\}$$

**Traversal шаг — функция над потоком:**

$$[\![S \text{|>} step]\!] = step \circ [\![S]\!]$$

**Эквивалентность запросов:**

$$Q_1 \equiv Q_2 \Longleftrightarrow \forall \Gamma: [\![Q_1]\!](\Gamma) = [\![Q_2]\!](\Gamma)$$

**Корректность планировщика:**

$$\forall Q: [\![Q]\!] = [\![Plan(Q)]\!]$$

### 23.5. Зафиксированные инварианты

1. `MATCH` возвращает **мультимножество** биндингов (bag, не set).
2. Snapshot фиксирован на весь запрос: $`\forall op: op.snapshot = \Gamma.Snapshot`.$
3. Пути не дедуплицируются автоматически (нужен явный `DISTINCT`).
4. Без `ORDER BY` порядок результата не определён.
5. `null ≠ none`: `null ∈ T` для любого nullable T; `none ∉ T`.
6. `OPTIONAL MATCH` = left outer join: возвращает `option<T> = T | none`, строка сохраняется.
7. `MATCH` = гомоморфизм (не изоморфизм): два алиаса могут указывать на один атом, если это не запрещено явными ограничениями.
8. Traversal детерминирован, только если задан `ORDER BY` или `DETERMINISTIC` режим.

---

## 24. Формальная система типов / правила вывода
### 24.1. Общая модель типов
Типизация: ```Γ ⊢ expr : T```, где:
- Γ — контекст (Env + schema),
- T — тип.

### 24.2. Базовые правила
#### Литералы
```
Γ ⊢ 42 : int
Γ ⊢ 14.5 : float
Γ ⊢ "abc" : string
```

#### Идентификаторы
```
Γ(x) = T
-------------
Γ ⊢ x : T
```

#### Idioms
```
field(Person, name) = string
field(Person, age) = int

Γ ⊢ a : record<Person>
-----------------------------
Γ ⊢ a.name : string
Γ ⊢ a.age : int
```

### 24.3. SELECT
```
Γ ⊢ expr_i : T_i
------------------------
Γ ⊢ SELECT expr_i : array<object{...}>
```

### 24.4. Union types
```record<A | B>``` -> ```Γ ⊢ x : record<A> ∨ record<B>```

### 24.5. Optional
```OPTIONAL MATCH``` -> ```Γ ⊢ a : option<record<T>>```.
На уровне значений `Some(v) = v`, `None = none`; `option<T>` не создаёт отдельный runtime-вариант.

### 24.6. Type coercion
`int → float` и `int → decimal` разрешены автоматически.
`float → decimal`, `float → int` и `decimal → int` требуют явного fallible cast.
`decimal → float` допускается контекстно с предупреждением о потере точности.
Смешение `float` и `decimal` без явного выбора целевого типа является ошибкой типизации.

### 24.7. Type errors
Ошибки:
- обращение к несуществующему полю
- несовместимые типы в бинарных операциях
- неверный тип для traversal шага
- mismatch в alias binding

### 24.8. Вывод типов для pipeline
Pipeline типизируется как композиция:
```
Γ ⊢ S : Stream<T1>
Γ ⊢ step : T1 → T2
------------------------
Γ ⊢ S |> step : Stream<T2>
```

### 24.9. Правила вывода типов:

$$\frac{x: \tau \in \Gamma}{\Gamma \vdash x: \tau} \quad \text{(T-Var)}$$

$$\frac{\Gamma \vdash e: record\langle T \rangle \quad field(f, T) = \tau}{\Gamma \vdash e.f: \tau} \quad \text{(T-Field)}$$

$$\frac{\Gamma \vdash f : (T_1, \cdots, T_n ) \rightarrow U \quad \Gamma \vdash e_i : T_i}{\Gamma \vdash f(e_1, \cdots, e_n): U} \quad \text{(T-FunctionCall)}$$

$$\frac{\Gamma \vdash e_i : \tau_i}{\Gamma \vdash\ SELECT\ e_1, \cdots, e_n : record\{ f_i : \tau_i \}} \quad \text{(T-SELECT)}$$

$$\frac{\Gamma \vdash pattern: \Gamma'}{\Gamma \vdash MATCH\ pattern: bag(\Gamma')} \quad \text{(T-MATCH)}$$

$$\frac{\Gamma \vdash S: bag\langle T \rangle \quad step: T \to bag\langle U \rangle}{\Gamma \vdash S\ |>\ step: bag\langle U \rangle} \quad \text{(T-Traversal)}$$

$$\frac{\Gamma \vdash A : bag\langle T_1 \rangle \quad \Gamma \vdash B : \langle T_2 \rangle}{\Gamma \vdash A \Join B : bag \langle T_1 \cup T_2 \rangle} \quad \text{(T-Join)}$$

---

## 25. Грамматика (EBNF)

Смотри файл [gql_grammar.md](gql_grammar.md).

---

## 26. Logical Plan (IR)

IR должен быть:
- унифицированным для SELECT / MATCH / Traversal pipeline
- алгебраическим
- типизированным
- переписываемым (rewritable)
- платформенно-независимым

IR НЕ ДОЛЖЕН:
- хранить синтаксис
- зависеть от конкретного API

Примерная структура:
```
LogicalPlan =
    Node {
        op: LogicalOp,
        inputs: Vec<NodeId>,
        schema: Schema,
        props: LogicalProps
    }
    
Schema = {
    columns: Vec<Column>
}

Column = {
    name: Symbol,
    type: Type,
    nullable: bool
}

LogicalProps = {
    estimated_cardinality: Option<f64>,
    ordering: Option<Ordering>,
    distinct: bool,
    keys: Vec<Key>,
    path_info: Option<PathInfo>
}
```

### 26.1. Core Logical Operators

#### Scan:
```
Scan {
    source: Class | Graph | Index,
    alias: Symbol,
    filter: Option<Expr>
}
```

#### Project:
```
Project {
    expressions: Vec<(Expr, alias)>
}
```

#### Join:
```
Join {
    kind: Inner | Left | Semi | Anti,
    condition: Expr
}
```

#### Aggregate:
```
Aggregate {
    group_by: Vec<Expr>,
    aggregates: Vec<AggregateExpr>
}
```

### 26.2. Graph-specific Logical Operators
#### Expand (single-hop):
```
Expand {
    direction: Out | In | Both,
    endpoint_domain: VerticesOnly | Atoms,
    edge_labels: Option<Vec<Label>>,
    target_labels: Option<Vec<Label>>,
    alias_target: Symbol,
    alias_edge: Option<Symbol>
}
```
Семантика:
```
input:  stream of endpoint atoms
output: stream of (source_endpoint, edge, target_endpoint)
```

`VerticesOnly` реализует StandardPath, `Atoms` — AtomWalk. Оба режима используют один
`Expand`; гиперребро возвращает отдельную строку для каждой разрешённой ветки (§15.9).

#### ExpandEdge
Если нужно работать именно с рёбрами:
```
ExpandEdge {
    direction: Out | In | Both,
    labels: Option<Vec<Label>>,
    alias_edge: Symbol
}
```

#### PathExpand (variable-length)
```
PathExpand {
    direction: Out | In | Both,
    endpoint_domain: VerticesOnly | Atoms,
    edge_labels: Option<Vec<Label>>,
    min_depth: usize,
    max_depth: Option<usize>,
    mode: PathMode,                     // Walk | Trail | Simple | Acyclic
    objective: Option<PathObjective>,   // Shortest | WeightedShortest
    filters: Vec<PathFilter>            // Weighted | Meta | ...
    alias_path: Symbol,
    alias_end: Symbol
}
```

#### Unwind (SPLIT)
```
Unwind {
    expr: Expr,
    alias: Symbol
}
```

### 26.3. Примеры IR
#### MATCH
Query:
```
MATCH (Person AS a)-[FRIEND]->(Person AS b)
IN GRAPH social
WHERE a.age > 18
RETURN b.name
```

Logical Plan:
```
Scan(Person AS a)
 -> Filter(a.age > 18)
 -> Expand(Out, FRIEND, alias_node=b)
 -> Filter(label(b) = Person)
 -> Project(b.name)
```

#### Path
Query
```
MATCH p = (* AS a)-[F*1..3]->(* AS b)
IN GRAPH social
RETURN p
```

Logical Plan:
```
Scan(a)
 -> PathExpand(
        direction=Out,
        edge_labels=[F],
        min=1,
        max=3,
        mode=Trail,
        alias_path=p,
        alias_end=b
   )
```

#### Traversal
Query:
```
... |> out('FRIEND') |> values('name')
```

Logical Plan:
```
Expand(Out, FRIEND)
 -> Project(name)
```

---

## 27. Optimizer (Rule-based + Cost-based)
```
Optimizer =
    RuleEngine (logical rewrite)
    +
    CostEngine (plan selection)
```

Этап 14 реализует один crate-private concrete optimizer без публичного planner API, registry rules
или trait с одной реализацией. Typed logical IR остаётся источником семантики, а executor получает
явно выбранный physical plan. Resource fallback, например in-memory hash → partitioned hash,
разрешён только внутри заранее выбранного семейства алгоритма.

Физические source alternatives: `PointGet`, `ClassScan`, `GraphClassScan`,
`ScalarIndexScan`, `CountLookup`, `GraphCatalogScan`, `Derived`. Join alternatives:
`HashJoin`, `PartitionedHashJoin`, `BlockNestedLoop`, `IndexNestedLoop`. Write plan описывает
`TargetSelection`, `Mutation`, `ConstraintValidation`, `CatalogMutation`, `IndexBuild` и
`Publication`.

Scalar index поддерживает leftmost equality prefix, equality prefix плюс один range следующего
поля, конечный constant/parameter `IN` как несколько exact probes и full scan только для forced
hint либо точного ORDER. Skip-scan, bitmap/intersection и WCOJ не входят.

Для consecutive INNER equality joins с pure predicates применяется bounded Selinger DP до восьми
sources, затем deterministic greedy. LEFT/CROSS/correlated barriers не переставляются; cross
product рассматривается последним. INNER hash строит меньшую сторону, LEFT сохраняет left side,
а small outer с Ready right index допускает `IndexNestedLoop`.

### 27.1. Optimizer pipeline
1. Normalize typed logical IR.
2. Canonical rule passes в порядке §22.3.
3. Stats annotation из transaction-visible `PlannerSnapshot`.
4. Generate bounded physical alternatives.
5. Fixed-point cost evaluation.
6. Canonical `(cost, plan_hash)` selection.
7. Physical plan execution через sync pull operators.

Plan cache, adaptive hooks/re-planning, parallel/vectorized execution и runtime algorithm
switching не входят. Plan cache может быть возвращён только после измеренного planning bottleneck.

### 27.2. Важные инварианты
Оптимизатор НЕ должен менять path semantics, гарантированный порядок, bag multiplicity,
effective-property view, observable error order или двигать impure functions. Scan и index path
регистрируют одинаковый conservative SSI class/graph range, поэтому смена плана не ослабляет
FCW/SSI.

### 27.3. Самые критичные вещи
1. exact class/graph counters и freshness policy property/degree histograms;
2. корректный scan fallback при missing/stale index;
3. bounded join enumeration и canonical tie-break;
4. function purity и strict deterministic contracts.

---

## 28. Примеры

### 28.1. Базовая выборка

```
SELECT name, age
FROM person
WHERE age >= 18
ORDER BY age DESC
LIMIT 10;
```

### 28.2. Создание и связывание

```
CREATE person:alice CONTENT { name: "Alice", age: 30 };
CREATE person:bob CONTENT { name: "Bob", age: 25 };
RELATE (person:alice)-[:FRIEND { since: time::now() }]->(person:bob)
  WEIGHTED 1.0;
```

### 28.3. MATCH — простой паттерн

```
MATCH (Person AS a { name: "Alice" })-[FRIEND]->(Person AS f)
IN GRAPH social
RETURN f.name, f.age;
```

### 28.4. MATCH — путь переменной длины

```
MATCH path = (Person AS a { name: "Alice" })-[FRIEND*1..3 SIMPLE]->(Person AS b)
IN GRAPH social
-- WHERE a.id != b.id - не нужно, так как путь SIMPLE
RETURN DISTINCT b.name, path::length(path) AS hops
ORDER BY hops;
```

### 28.5. Traversal pipeline

```
-- Имена друзей Алисы
SELECT * FROM person WHERE name = "Alice"
  |> tv::out(["FRIEND"])
  |> tv::values(["name"])
  |> tv::dedup();
```

### 28.6. MATCH + LET + pipeline

```
LET $alice_articles = MATCH (Person { name: "Alice" })-[PUBLISH]->(Article AS a)
                      IN GRAPH knowledge RETURN a;

RETURN $alice_articles
  |> tv::in("PUBLISH")              -- соавторы
  |> tv::where(|p| p.name != "Alice")
  |> tv::out(["FRIEND"])              -- их друзья
  |> tv::values(["name"])
  |> tv::dedup();
```

### 28.7. Паттерн с несколькими путями

```
MATCH
  (Person AS alice { name: "Alice" })-[PUBLISH]->(Article AS art),
  (Person AS coauthor)-[PUBLISH]->(Article AS art),
  (Person AS coauthor)-[FRIEND]->(Person AS friend)
IN GRAPH knowledge
WHERE coauthor != alice -- сравнение записей по умолчанию всегда по id
RETURN array::distinct(friend.name) AS friends;
```
или
```
MATCH (Person AS alice {name: 'Alice'})-[PUBLISH]->(Article AS a)
              <-[AUTHOR]-(Person AS coauthor)
              -[FRIEND]->(Person AS friend)
IN GRAPH knowledge
WHERE coauthor != alice
RETURN DISTINCT friend.name as friends;
```

### 28.8. Взвешенный кратчайший путь

```
TRAVERSE FROM MATCH (Person AS p {name: 'Alice'})
                    IN GRAPH social RETURN p
  |> tv::weightedShortestPath(person:bob, "weight")
  |> tv::path();
```

### 28.9. Гиперребро

```
-- Создать гиперребро "соавторство"
RELATE HYPER [:COAUTHOR] TARGETS [person:alice, person:bob, person:carol]
  AS UNDIRECTED ORDER BY TARGETS SET paper = "paper:xyz";

-- Найти все соавторства Алисы
MATCH HYPER [COAUTHOR AS e] CONTAINS person:alice
IN GRAPH publications
RETURN e, e.paper;
```

### 28.10. Транзакция с откатом через событие

```
DEFINE EVENT prevent_delete_admin ON CLASS person
  WHEN $event = "DELETE" AND $before.role = "admin"
  THEN { THROW "Cannot delete admin user" };

BEGIN;
  DELETE person:admin_user;  -- вызовет событие → throw → CANCEL
COMMIT;
```

### 28.11. Определение UDF

```
DEFINE PURE FUNCTION fn::friend_score(a: record<person>, b: record<person>) -> float {
    LET $common = (
        SELECT count() FROM friendship
        WHERE in = a.id AND out = b.id
    );
    RETURN $common / math::max(1, a.degree + b.degree)
};

SELECT fn::friend_score(person:alice, person:bob) AS score;
```

---

## 29. Настройки хранилища

Общие настройки

| Имя                                  | Описание                                                                               | Значение по умолчанию |
|--------------------------------------|----------------------------------------------------------------------------------------|-----------------------|
| `FG_ASYNC_EVENT_PROCESSING_INTERVAL` | Интервал обработки асинхронных событий                                                 | 5 сек                 |
| `FG_HNSW_CACHE_SIZE`                 | Размер HNSW кеша                                                                       | 256 МБ                |
| `FG_PATH_MAX_DEPTH`                  | Глобальная максимальная длина путей графов                                             | 30                    |
| `FG_WITHIN_MAX_DEPTH`                | Глобальная максимальная длина поиска вложенностей в мета-атомах                        | 7                     |
| `FG_TRAVERSAL_READ_YOUR_WRITES`      | Включает traversal pipeline'у видимость его собственных изменений                      | false                 |
| `FG_TEMPFILES_PATH`                  | Каталог временных файлов: спиллинг сортировок в `SELECT` **и** при построении индексов | ""                    |
| `FG_GRAPH_EXPANSION_MAX_NODES`       | Глобальное максимальное количество узлов при построении графа                          | 1'000'000             |
| `FG_GRAPH_EXPANSION_MAX_EDGES`       | Глобальное максимальное количество рёбер при построении графа                          | 5'000'000             |
| `FG_DISKANN_CACHE_SIZE`              | Размер кэша индекса DiskANN                                                            | 268435456 (256 МБ)    |
| `FG_EXECUTION_MODE`                  | Режим исполнения по умолчанию ([§17.8](#178-режимы-исполнения-и-детерминизм))          | `BEST_EFFORT`         |
| `FG_MAX_SNAPSHOT_AGE_SEC`            | Возраст снимка, после которого он принудительно закрывается                            | 300                   |
| `FG_TX_WRITE_SET_MAX_BYTES`          | Максимальный atom-delta write-set одной sync-транзакции                                | 268435456 (256 МиБ)   |

Live-запросы и changefeed ([§21](#21-события-и-live-запросы))

| Имя                                | Описание                                            | Значение по умолчанию |
|------------------------------------|-----------------------------------------------------|-----------------------|
| `FG_LIVE_QUEUE_CAPACITY`           | Размер очереди событий на одну подписку             | 10'000                |
| `FG_LIVE_OVERFLOW_POLICY`          | Поведение при переполнении очереди подписки         | `GAP`                 |
| `FG_CHANGEFEED_PURGE_INTERVAL_SEC` | Периодичность очистки истекших сегментов changefeed | 3600                  |

Массовая загрузка и построение индексов ([§7.11](#711-begin-import--end-import--массовая-загрузка))

| Имя                                   | Описание                                                                         | Значение по умолчанию |
|---------------------------------------|----------------------------------------------------------------------------------|-----------------------|
| `FG_BULK_SORT_MEM`                    | Бюджет памяти на прогон внешней сортировки ключей                                | 256 МБ                |
| `FG_BULK_SORT_MERGE_FANIN`            | Степень слияния при k-way merge                                                  | 64                    |
| `FG_BULK_SORT_PARALLELISM`            | Число потоков генерации прогонов                                                 | число ядер            |
| `FG_INDEX_BTREE_FILL_FACTOR`          | Плотность заполнения листьев B+tree при bulk-сборке                              | 0.90                  |
| `FG_INDEX_BTREE_INTERNAL_FILL_FACTOR` | Плотность заполнения внутренних узлов B+tree                                     | 0.95                  |
| `FG_INDEX_RTREE_FILL_FACTOR`          | Плотность заполнения узлов R-tree при STR-упаковке                               | 0.95                  |
| `FG_INDEX_PENDING_MAX`                | Порог очереди `DEFER`, после которого индекс помечается `stale` и пересобирается | 1'000'000             |
| `FG_IMPORT_SEGMENT_ITEMS`             | Applied items во внутреннем private segment                                      | 1'000'000             |
| `FG_IMPORT_MAX_REPORTED_VIOLATIONS`   | Сколько нарушений описывается подробно в отчёте                                  | 1000                  |
| `FG_IMPORT_REJECT_LIMIT`              | Явный положительный предел для `ON VIOLATION SKIP`                               | отсутствует           |
| `FG_HNSW_BUILD_MEM`                   | Бюджет памяти на построение HNSW (отдельно от кэша обслуживания)                 | 2 ГБ                  |

---

## 30. Система мостов

Система мостов (или bridges) — это механизм, позволяющий Ferros-Grynn взаимодействовать с другими базами данных: 
подключаться к ним, выполнять запросы, извлекать / записывать данные и так далее, — а также расширяющий возможности некоторых операторов GQL.

С помощью моста можно как извлекать необходимые данные из других БД, так и записывать их.

При создании запроса к Ferros-Grynn можно использовать метод `.use_bridge<B: Bridge>(bridge: B, data: BridgeData)`, в который передаётся инстанс моста
и данные, представленные перечислением `BridgeData`.

Инстанс моста индивидуален для каждой БД, с которой может соединяться Ferros-Grynn, реализует `Bridge trait` 
и имеет возможности подключаться к своей БД всеми доступными для этой БД способами (локальное хранилище, подключение по сети и т.д.).

Перечисление `BridgeData` имеет следующие варианты:

```rust
use std::collections::HashMap;

enum BridgeData<'a> {
    /// Имя внешнего class/source либо ссылка на запись.
    /// Первый параметр - имя (+ ':id', если запись).
    /// Второй параметр - имя в GQL запросе.
    RecordOrClass(&'a str, &'a str),

    /// Кастомный запрос к БД.
    /// Опционально - карта сопоставлений результата запроса к внешней БД с параметрами запроса GQL.
    ExternalQuery(&'a str, Option<HashMap<&'a str, &'a str>>),

    /// Запрос к Ferros-Grynn.
    /// Опционально - карта сопоставлений результата запроса к Ferros-Grynn с параметрами запроса внешней БД.
    InternalQuery(&'a str, Option<HashMap<&'a str, &'a str>>),
}
```

### 30.1. SurrealBridge

Это мост к SurrealDB.

Помимо базового функционала моста, `SurrealBridge` расширяет операторы:
- `RETURN GRAPH`, `BUILD GRAPH`: строит графы из внешних relation-записей SurrealDB
  формата `{ id, in, out }`; class каждого внешнего `RecordId` переводится в
  явный Ferros-Grynn `CLASS` по bridge mapping, а не становится внутренней TABLE-моделью;
- `RELATE`: создаёт внешнюю relation-запись SurrealDB, но не создаёт сами endpoint-сущности;
  внутри Ferros-Grynn её identity по-прежнему задаётся `RecordId = class:id`.

## 31. Соответствие кодовой базе

> **Вынесено в [implementation_plan.md](implementation_plan.md) §1 «Соответствие кодовой базе».**
> Таблицы «Согласовано» и «Требуют согласования» перенесены в план и объединены с аналогичной
> таблицей `gql_tech_spec.md` §17 в единый источник: §1.1 — что реализовано / каркас / новое;
> §1.2 — открытые вопросы (`PropertyMap` ↔ `object`, публичная форма `RecordId`,
> `RELATE HYPER` / `MATCH HYPER`, метаатомы, ECS-компоненты).

---

## 32. Известные риски

### 32.1. Критические

| Риск                           | Описание                                                                                                                | Митигация                                                                                                                                                                              |
|--------------------------------|-------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Path explosion                 | `[E*1..N]` при больших N и высокой степени → экспоненциальный рост                                                     | Обязательное предупреждение планировщика при `degree > 10 AND depth > 4`; ограничение по умолчанию `max_depth = 30`                                                                    |
| Отсутствие degree statistics   | MATCH-планировщик не работает корректно без статистики степени                                                          | Статистика по меткам и степеням — обязательна для MVP; сбор при загрузке и инкрементально                                                                                              |
| Неопределённый критерий UPSERT | Без явного `WHERE` или ID семантика зависит от плана                                                                    | Синтаксически запрещено: `WHERE` или ID обязательны                                                                                                                                    |
| null / none путаница           | Тонкая разница между двумя типами отсутствия                                                                            | Явное описание в §5.1 и в документации; lint-предупреждения при смешивании                                                                                                             |
| Write skew при явном SI        | Две непересекающиеся мутации могут совместно нарушить структурный инвариант, если проверять только transaction snapshot | Commit сливает изменения поверх актуального root и повторно проверяет graph-family invariant при любом isolation level (§17.1); SSI дополнительно защищает пользовательские инварианты |

### 32.2. Умеренные

| Риск                               | Описание                                                   | Митигация                                                            |
|------------------------------------|------------------------------------------------------------|----------------------------------------------------------------------|
| Fan-out при traversal              | `tv::out()` на high-degree nodes → взрыв traverser'ов      | `tv::limit()` как ранний обрыв; degree-aware batching в планировщике |
| Неопределённость порядка traversal | Без `ORDER BY` результат нестабилен                        | Явная документация; lint-предупреждение при `DETERMINISTIC`-режиме   |
| Конфликт `MATCH` и traversal       | Разные исполнительные механизмы — риск смешанной семантики | Строгое разграничение в IR: MATCH → join engine; traversal → VM      |
| Асинхронные события                | Гарантии доставки и порядка не строгие                     | Явное требование идемпотентности; документация at-least-once         |
| Рекурсия в UDF                     | Потенциальный stack overflow                               | Обязательный `MAXDEPTH`; compile-time проверка наличия базы рекурсии |

### 32.3. Отложено / вне MVP

> **Вынесено в [implementation_plan.md](implementation_plan.md) §6 «Отложено / вне MVP».**
> Список отложенного (JIT-компиляция, SMT-верификация оптимизатора, параллельный recovery,
> graph-специфические индексы, distributed execution) объединён там со сводками из
> `gql_tech_spec.md` §18 и `disk_storage_spec.md`.
