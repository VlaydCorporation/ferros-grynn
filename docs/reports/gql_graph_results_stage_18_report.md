# Отчёт этапа 18: self-contained graph results и Atomic `BUILD GRAPH`

Дата: 2026-09-28.

## 1. Итог

Этап G0–G14 завершён. Реализованы terminal `RETURN GRAPH`, анонимный owned
`BUILD GRAPH`, именованный atomic create/overwrite, durable graph contract,
`FGDC v9`, shared-child arena DAG, optimizer/EXPLAIN и bounded resource
accounting. Публичный graph result не хранит transaction, snapshot, page guard
или storage identity.

`Q-37` закрыт: один shared validator проверяет family, rooted-tree root и
`SIMPLE`/`BIPARTITE`/`COMPLETE` после transaction replay под commit lock.
Поэтому тот же контракт обязателен для GrynnQL и прямого Rust mutation path.

## 2. Реализованный срез

- `GraphTopologyConstraint(s)` добавлены в runtime metadata, `GraphSchema`,
  `GraphMetadata`, `GraphRead` и materialized families.
- Parser, AST, analysis и typed IR поддерживают canonical `RETURN GRAPH`,
  `BUILD GRAPH`, contract/root/selectors/limits и полный RHS `LET`.
- Один `GraphAssemblyAccumulator` потребляет MATCH/SELECT/TRAVERSE/direct source
  через pull visitors без промежуточного `Vec<Value>`.
- Assembly дедуплицирует occurrence identity, замыкает выбранные edges на
  endpoints и добавляет только необходимые containment chains. Несвязанные
  roots не flatten-ятся.
- Один source child отображается в один target child; несколько arena slots
  сохраняют разные paths к тому же `GraphId`.
- `InstallBuiltGraphTree` атомарно ставит root и owned children, используя
  только существующие database-global atoms. Effective properties переводятся
  в inherited/value/hidden overlays.
- `OVERWRITE` сохраняет public root `GraphId`, graph properties, family и
  system-index identities; прежние owned children удаляются каскадно.
- `EXPLAIN ANALYZE` показывает source rows, selected/closure/final atoms,
  deduplication, child/scopes и validation time.

## 3. Durable contract

`DatabaseCatalog` принимает только `FGDC v9`. Graph entry хранит constraint
bits, optional canonical `RecordId` root, `owner_root`, properties и dense arena
slots. Public name map не содержит internal graphs. Validator отвергает unknown
bits, invalid root/owner, чужие children, duplicate `(scope, child)` и
containment cycle. `FGDC v8` намеренно не мигрируется.

Local publication переиспользует существующий COW generation writer и один
catalog/WAL commit. Новый page/WAL/`GraphSuperblock` layout не вводился.
Dependency-first reopen восстанавливает owned children до public root; shared
child сохраняет одну physical identity после reopen, overwrite и remove.

## 4. Correctness evidence

Покрыты parser contract conflicts, empty/null/ambiguous inputs, Record/Atom/
Occurrence/Path/Graph extraction, exact endpoint closure, graphless assembly,
effective overlays, shared child и multi-owner scopes, root inference,
family/validator failures, read-only/import rejection, atomic create/IFNE/
overwrite, limits/cancellation, EXPLAIN effects и owned-after-drop.

Отдельные durability tests проверяют `FGDC v9` golden/round-trip и отказ v8,
unknown bits, malformed root/owner/scope/cycles, named build reopen и полный
shared-child lifecycle. Прямые Rust mutations, нарушающие family/root/SIMPLE,
отклоняются до публикации.

Финальные результаты:

| Gate | Результат |
|---|---:|
| focused graph-result | 17 passed |
| library, no default features | 499 passed, 6 ignored |
| library, `native-async-io` | 501 passed, 7 ignored |
| local datastore, no default features | 15 passed |
| local datastore, `native-async-io` | 16 passed |
| `cargo check`, обе конфигурации | PASS |

## 5. Resource и performance gate

Первый profiling run на 10 000 nodes / 9 999 edges выявил регрессию:
`BUILD GRAPH` median 53,220 ms против 37,655 ms у `query_to_memory`, ratio
`1,413×`. Причиной были четыре обхода плоского graph view: nested pre-scan,
copy, повторный count и heap estimate; `TransactionGraphView::node_ids` и
`edge_ids` дополнительно строили временные vectors.

Исправление находится в общем materialization path: flat-copy за один проход
одновременно обнаруживает nested binding и считает owned bytes. Nested graph
немедленно возвращается в hierarchy assembler. Отдельные count/estimate проходы
для точного `<GRAPH>` fast path удалены без ослабления limits.

Финальный paired profiling run, 9 повторов с ротацией порядка:

| Candidate | Median | Min | Max | Max RSS delta |
|---|---:|---:|---:|---:|
| `query_to_memory` | 40,3995 ms | 31,4736 ms | 45,4057 ms | 12 KiB |
| `BUILD GRAPH` | 41,3300 ms | 32,4825 ms | 49,5792 ms | 4 KiB |

Итоговый ratio `1,0230×`, то есть `BUILD GRAPH` медленнее на 2,30% и проходит
лимит 5%. CSV:
`target/gql-stage18/graph-result-01a0e7f1-0c7a-7b33-ae94-d10afff52e6a.csv`.

Точная profiling-пара:

```text
target/profiling/deps/graph_result_performance_probe-9b67fdff7fac1a40.exe
target/profiling/deps/graph_result_performance_probe-9b67fdff7fac1a40.pdb
timestamp UTC: 2026-09-28 12:13:23
```

VTune 2026.4 software Hotspots использовал эту пару и разрешил Rust symbols и
source lines. Capture и exports:

- `target/gql-stage18/vtune-graph-result-20260928/`;
- `target/gql-stage18/vtune-graph-result-20260928-functions.csv`;
- `target/gql-stage18/vtune-graph-result-20260928-source-lines.csv`.

На коротком 1 000-node capture основная видимая стоимость осталась в BTree
copy/drop и allocator; отдельного нового graph-assembly hotspot нет. Hardware
microarchitecture sampling недоступен без административно загруженного driver,
но user-mode collection, symbols и source resolution прошли.

## 6. Границы результата

- `Q-09` не закрывается: `BUILD GRAPH` не является origin-aware flush mutable
  Mode 3.
- `Q-04` runtime generation retirement остаётся этапу 19.
- Public graph/path/storage формы не дублировались, новых dependencies нет.
- Workspace-wide warning/lint debt не маскировался локальными allow.

## 7. Финальные tooling gates

- `cargo +nightly fmt --all -- --check` — PASS.
- Focused Clippy (`--lib --no-default-features --features native-async-io
  --no-deps`) остановлен существующим workspace-wide deny-debt: 5 233
  diagnostics, главным образом `self_named_module_files` и отсутствующая
  документация private items. Два относящихся к этапу 18
  `needless_pass_by_ref_mut` исправлены; повторные focused compile/tests
  прошли. Локальные `allow` не добавлялись.
- `graphify update .` — `SKIP`: установленный Windows launcher завершился с
  `Failed to canonicalize script path`; известный wrapper `graphifyy` в
  текущем окружении отсутствует. Существующие `graphify-out` artifacts не
  откатывались и не подменялись ручной генерацией.
