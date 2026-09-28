# Этап 19. Windows GA-core gate — итоговый evidence

**Статус на 2026-09-29: ✅ W0–W9 завершены.** Windows GA-core принят с
`STORAGE_FORMAT_VERSION = 1`, `FGDC v9` и `FGFC v5`. Отложенная поверхность
этапов 20+ и maintenance-долг `Q-14` не включены в GA-profile.

## 1. Release inventory

- production crate: `ferros-grynn`, branch `mvp`, dirty checkout сохранён;
- `rustc 1.98.1 (48a229cea 2026-09-01)`, `cargo 1.98.1`;
- core format до W4: `STORAGE_FORMAT_VERSION = 1`, `FGDC v9`, `FGFC v5`;
- release blockers этапа: `Q-04`, `Q-06`, `Q-11`, `Q-13`, `Q-20`, `Q-22`,
  `Q-34`, `Q-36` и public/lint freeze `Q-14`;
- `Q-09` и возможности этапов 20+ остаются вне GA-core.

Shell-wrapper Graphify на этом checkout завершался `Failed to canonicalize
script path`. Запуск установленного пакета через его module entrypoint успешно
выполнил query и финальный `graphify update .`; graph output актуализирован.

## 2. W1 — runtime resources

Старый file registry теперь служит retirement token после COW publication.
Exact physical snapshot удерживает его `Arc`; cleanup хранит `Weak` marker и
удаляет поколение после release. Ошибка удаления после publication наблюдаема,
но не превращает durable commit в rollback, и повторяется при следующем
maintenance/COW проходе.

`LocalOptions` является единственной authority. `LocalStorageStatus` выводит
resident/transient/metadata budgets, locator/source counts, зарегистрированные
и реально открытые handles, cache pressure, WAL backlog и retirement pressure.
Process-wide `SURREAL_MEMORY_THRESHOLD` удалён.

Focused evidence:

- `repeated_cow_commits_retire_unpinned_generations_at_runtime`;
- `physical_snapshot_pins_replaced_generation_until_maintenance`;
- `cleanup_failure_does_not_undo_commit_and_is_retried`;
- `atom_and_graph_cow_churn_keeps_only_live_generations`.

`Q-04` и `Q-13` закрыты. После W1 `Q-11` был передан в W7: пробный R8 fast
path не прошёл paired gate и был удалён, а не оставлен как недоказанная
сложность. Итоговое решение `Q-11` приведено в §8.1.

## 3. W2 — Serializable concurrency

Commit replay повторно выделяет private `GlobalAtomId` и graph-local `AtomId`
из актуального root под commit lock и переводит topology/conflict keys. Это
устраняет физические collision aborts, не ослабляя logical FCW/SSI.

Пять paired повторов для 1/2/4/8 disjoint writers дали нулевое число aborts во
всех `16/32/64/128` операциях. Same `RecordId` и same membership по-прежнему
дают serialization failure. `Q-06` закрыт.

## 4. W3 — reopen и startup

### 4.1. Lazy handles (`Q-22`)

Registry eagerly проверяет catalog identity, canonical path и длину файла, но
native positional handle создаёт только первый read через coalesced `OnceLock`.

Шесть AB/BA пар для 100 графов:

| Метрика | Результат lazy/eager |
|---|---:|
| total reopen median | `0,987×` |
| registry phase median | `0,443×` |
| open handles | `1107 / 1710` |

Это resource/handle improvement; статистически значимое ускорение общего reopen
не заявляется. `Q-22` закрыт без обратного объединения freelists.

### 4.2. Durable statistics и lazy explicit indexes (`Q-36`)

Reopen загружает durable statistics, строит только engine-owned class/label
lookups и оставляет Standard/Unique/Count за immutable physical side-files.
Первый query валидированно декодирует точную physical version и кэширует её.
Перед первой mutation полный mutable index store перестраивается; тот же guard
работает в commit/Read-Committed replay. Generation writer fail-closed отклоняет
deferred binding, поэтому пустой index image не может быть опубликован.

Median reopen (три warm повтора):

| atoms | 0 indexes | 1 index | 8 indexes |
|---:|---:|---:|---:|
| 1K | 42,978 ms | 43,476 ms | 44,449 ms |
| 100K | 5 808,359 ms | 5 762,442 ms | 5 823,917 ms |
| 1M | 60 641,965 ms | 58 369,646 ms | 55 245,116 ms |

На 1M statistics phase равна `2,241/2,401/2,916 ms`, engine-owned lookup
rebuild — `1 421,892/1 246,004/1 291,362 ms`. Количество secondary indexes
больше не влияет на startup. Остаточный bottleneck — materialization AtomStore:
`59 227,488/57 116,187/53 867,609 ms`; это отдельная наблюдаемая граница, а не
основание заранее добавлять новый runtime representation.

Correctness evidence:

- `atomic_import_streams_every_index_family_and_reopens`: Standard, Unique,
  Count и effective index reads после reopen; handle открывается по demand;
  первая mutation и второй reopen сохраняют результат;
- local-store gate: `11 passed`, `2 ignored` diagnostics;
- index-store gate: `2 passed`;
- scoped-index-image gate: `1 passed`.

`Q-36` закрыт.

## 5. W4 — format freeze (`Q-20`)

Ignored diagnostic собран только через `--profile profiling`. Exact artifacts:

- `target/profiling/deps/ferros_grynn-2170861548b6ea93.exe`;
- same-stem `ferros_grynn-2170861548b6ea93.pdb`.

15 paired pack-layout проходов, 100 000 production-encoded keys:

| Workload | full/prefix pages | full/prefix used bytes | full/prefix median CPU | CPU ratio |
|---|---:|---:|---:|---:|
| common string | `290 / 94` | `4 712 716 / 1 513 693` | `155 600 / 1 139 100 ns` | `7,32×` |
| composite | `314 / 75` | `5 113 772 / 1 207 395` | `159 900 / 1 578 200 ns` | `9,87×` |
| mixed string | `290 / 217` | `4 712 716 / 3 531 888` | `129 300 / 655 000 ns` | `5,07×` |

Prefix подтверждает size ceiling, но не проходит полный GA gate: нужен второй
leaf codec и отсутствует paired evidence point/range reads, mutation/rebuild,
recovery и p99. Block compression дополнительно требует полной распаковки,
отсутствующих production configuration/discriminator и новой прямой dependency.
Поэтому frozen core v1 сохраняет единственный full-key codec; оба draft-кандидата
удалены. Одновременно удалён ложный draft LZ4 contract для property pages:
production всегда отклонял `COMPRESSED`.

Freeze manifest: `docs/storage_core_v1_freeze_manifest.md`. Focused evidence:

- page header/checksum/type: `7 passed`;
- `FGDC v9`/`FGFC v5`/`FGC1`: `13 passed`;
- canonical value/`FGAR`: `10 passed`;
- B+tree/Label/Count: `15 passed`, один unrelated diagnostic ignored;
- database/AtomStore/graph superblocks: `7 passed`;
- property dictionary: `9 passed`; AtomHot: `2 passed`; graph pages/statistics:
  `8 passed`; property heap/large chain: `2 passed`.

CSV: `target/windows-ga-core-stage19/q20_key_layout_profiling.csv`. `Q-20`
закрыт.

## 6. W5 — crash/recovery и bounded fuzz

Матрица выполнена exact debug test binary:

- `target/debug/deps/ferros_grynn-2a7755fa933cd086.exe`;
- same-stem PDB имеет ту же дату сборки `2026-09-28 18:41:08`.

Результаты:

- все `storage::wal::tests`: `26 passed`;
- pre-publication failure matrix: `1 passed`;
- orphan/startup cleanup: `2 passed`;
- AtomStore и graph authorized checkpoint-COW: `1 + 1 passed`;
- parser: `4096` cases, ChaCha seed `19001901`, pass;
- canonical value decoder: `4096` inputs `0..512` bytes, ChaCha seed
  `19001902`, pass.

Ни один target не завершился panic, OOM или partial publication. Новый fuzz
framework не добавлялся: использованы существующие bounded proptest strategies
и deterministic malformed corpus W4. Полный manifest:
`target/windows-ga-core-stage19/w5_crash_fuzz_evidence.txt`.

## 7. W6 — differential equality и soak

Production integration matrix прошла на обеих local-конфигурациях:

- RAM: `3 passed`;
- Sync local: `15 passed`;
- NativeAsync local: `16 passed`, включая отдельный IoRing-сценарий;
- internal local-store: `11 passed`, два тяжёлых diagnostic probes ignored.

Gate обнаружил нарушение §19: W3 временно декодировал auxiliary statistics при
reopen. Исправленный lifecycle загружает только roots и сохраняет exact physical
bindings. Planner запрашивает только необходимые parts; checksum failure optional
part превращается в missing-statistics fallback. Writer перед первой mutation
строго гидратирует все parts, поэтому corruption отклоняется до изменения private
root и публикации. Regression одновременно проверяет успешный label-sensitive
MATCH и отсутствие попытки записать новый atom после отказа.

На exact `target/debug/deps/ferros_grynn-2a7755fa933cd086.exe` выполнено `20`
повторов каждого набора: `12` Serializable tests, `7` checkpoint tests и два
точечных retirement/snapshot tests — всего `420 passed`, `0 failed`. Полный
manifest: `target/windows-ga-core-stage19/w6_differential_soak_evidence.txt`.

NativeAsync gate также нашёл compile-only дефект sparse physical cursor:
`Arc::clone(direct)` исправлен на `Arc::clone(&direct)`. После исправления обе
feature-конфигурации прошли.

## 8. W7 — performance (`Q-11`, `Q-34`)

### 8.1. Resident engine и общий budget (`Q-11`)

Повторные production-prototype измерения использовали текущий код без нового
fast path. Для 100 млн операций, hot set/pool `4096` страниц и 15 повторов:

| Путь | Median | p50 | максимальный p99 |
|---|---:|---:|---:|
| generic R8 hit | `74,90 ns/op` | `72 ns` | `138 ns` |
| epoch/resolved hit | `40,61 ns/op` | `39 ns` | `75 ns` |

Оба результата проходят minimum `80/45 ns`; generic path не достигает только
stretch-цели `70 ns`. Максимальный RSS delta составил `770 048/806 912` bytes.

Multi-file sweep на 2 млн операций, dataset `65536` и pool `4096` дал
window/mmap throughput `1,19× / 1,12× / 1,13×` для `1/4/8` файлов. В длинном
500-million-operation sweep с pool `64` получены `0,98× / 0,97× / 0,96× /
0,96× / 0,95×` mmap для `1/2/4/8/12` threads — выше обязательного `0,80×`.

При `P=4096` transient budget равен `288` страницам: resident + transient =
`4384` страницы, `68,5 MiB` или `107,03125%` pool. Metadata выводится отдельно
через `LocalStorageStatus`. Global epoch/directory, общий multi-file budget и
ручной `buffer_pool_pages` не стали измеренным blocker, поэтому sharding,
per-file quotas и auto-sizing не добавлены. `Q-11` закрыт.

### 8.2. One-statement и SELECT (`Q-34`)

На CPU 0 выполнено 15 AB/BA повторов текущего temporary-session driver против
прежнего direct control. Paired ratios:

```text
1.0656 1.0442 1.0457 1.0647 1.0505
1.0502 1.0497 1.0200 1.0534 1.0326
1.0251 1.0494 1.0413 1.0540 1.0698
```

Median `1,0497×` проходит обязательный gate `≤1,05`. В hot path оставлены только
необходимые транзакционные действия: temporary execution не создаёт cursor
runtime, empty closure registry не берёт write lock, внутренний phase timer и
commit timer включаются только для `EXPLAIN ANALYZE`, parameter fingerprint —
только для `DETERMINISTIC`.

Простой SELECT проверен 15 AB/BA парами (`FG_SELECT_SAMPLES=101`, CPU 0):
current median `2 256 900 ns`, Stage-12 baseline `2 388 600 ns`, ratio medians
`0,9449×`, paired median `0,9493×`, range `0,8710–0,9936×`. Регрессии нет.

VTune 2026.4 использовал exact same-stem artifacts
`ferros_grynn-2170861548b6ea93.exe/.pdb`. Software Hotspots экспортировал Rust
symbols и source lines; заметны `GqlSession::execute_internal`, analyzer,
allocator/copy и benchmark clock costs. SELECT capture не показал нового
session/control hotspot: верхние доли — `memcpy 19,2%`, pointer write `6,0%`,
comparison `5,4%`, `Box::new 4,7%`, drop `Value 4,6%`. Hardware Hotspots —
`SKIP`: sampling driver недоступен без administrative setup. Software evidence:
`target/windows-ga-core-stage19/vtune_q34_loop_sw` и `vtune_select_sw_02`.
`Q-34` закрыт.

## 9. W8 — public freeze

- `Datastore::prototype_clear_resident_pages` удалён из production surface.
  Cold-cache measurement остаётся только в скрытом non-default
  `storage-engine-prototype-api`, который не входит в GA API.
- Двойные `Option` не заменены новым enum: обе формы имеют две реальные оси.
  DML label хранит `clause omitted / LABEL NONE / replacement`; graph storage
  хранит `stale AtomId / live atom without value / live value`. Эти контракты
  теперь описаны у полей и accessors.
- Удалены неиспользуемые legacy keyword-lists; compatibility syntax и новый
  registry не добавлялись.
- GA GrynnQL surface соответствует реализованным этапам 8–18: graph-first DDL
  и DML, Atomic Import, transactions/control flow, SELECT/optimizer,
  StandardPath/nested MATCH, traversal VM и graph results/build. Возможности
  этапов 20+ остаются typed unsupported до side effects.

`Q-14` сужен. Workspace formatting синхронизирован, временный метод `Datastore`
удалён, semantic double-options объяснены. Остаётся самостоятельный накопленный
documentation/privacy-layout Clippy debt: strict run дал `5321 errors` и `337
warnings`; основные классы — missing docs (`1689` fields, `933` functions, `907`
methods, `477` constants, `412` variants) и избыточная `pub(crate)` visibility в
private modules. Линты не ослаблялись и не маскировались локальными `allow`.

## 10. W9 — closure gate

Полный gate после исправления cursor regression:

| Команда | Результат |
|---|---|
| `cargo test --locked --lib --no-default-features -- --test-threads=1` | `499 passed`, `9 ignored` |
| `cargo test --locked --lib --no-default-features --features native-async-io -- --test-threads=1` | `501 passed`, `10 ignored` |
| `cargo check --locked --lib --no-default-features` | pass |
| предыдущая команда с `--features native-async-io` | pass |
| `cargo +nightly fmt --all -- --check` | pass; только deprecated rustfmt-option warnings |
| `cargo clippy --locked --lib --no-default-features -- --cap-lints warn` | pass; debt зафиксирован выше |
| `cargo check --locked -p fg-storage-engine-prototypes --lib --features native-async-io` | pass |

Full library gate нашёл преждевременное удаление exact index-seek cursor. Scan
останавливался по `maximum` до исключения anchor posting group, поэтому
`LIMIT+1` мог вернуть ровно одну видимую строку и ошибочно закрыть replayable
token. Общий query-index path теперь читает anchor group отдельно для full,
equality и range scans, а limit применяет к строго следующим keys. Focused
unique/duplicate-posting regressions и обе полные матрицы прошли.

`graphify update .` выполнен установленным module entrypoint. W0–W9 закрыты;
следующий dependency-ordered этап — 20.
