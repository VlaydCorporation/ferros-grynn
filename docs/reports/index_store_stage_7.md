# Отчёт по этапу 7: базовый индексный контур

Дата: 2026-09-08  
Статус: завершён

## 1. Граница этапа

Реализован только этап 7 `docs/full_plan.md`:

- standard/unique scalar B+tree и graph-wide primary B+tree;
- существующий order-preserving codec скалярных ключей;
- immutable Label indexes и одно-страничный Count index;
- bounded external sort и bottom-up packing полного поколения;
- crash-safe публикация индексных side-files;
- корректный scan fallback при недоступном secondary index.

Incremental B+tree mutation, DDL/GQL surface, наблюдаемый index state, сжатие страниц и
потоковая запись готового поколения не реализовывались: они принадлежат последующим этапам и
зарегистрированы как `Q-05`, `Q-18`–`Q-20`.

## 2. Нормативный контракт

`docs/disk_storage_spec.md` §§20.1–20.4 и 20.13 синхронизированы с format-v1:

- scalar index сравнивает полные typed order-preserving keys побайтово;
- graph-wide primary использует отдельный canonical `RecordId` codec и не объявляет его байтовый
  порядок пользовательским порядком значений;
- standard leaf хранит упорядоченные posting lists, unique/primary отвергает повторный ключ;
- Label side-file принадлежит точному `(AtomKind, label_id)` и хранит плотную возрастающую цепочку
  `DiskAtomRef`;
- Count side-file содержит ровно одну page 0 и обновляется в той же WAL-транзакции, что и
  membership;
- secondary index является перестраиваемым access path: его отсутствие или повреждение не меняет
  результат запроса, но primary/base corruption остаётся фатальным.

Compression/fill-factor разделы явно оставлены кандидатами, а не альтернативным прочтением уже
записанных страниц.

## 3. Реализация

### 3.1. B+tree и внешняя сортировка

Полная сборка primary и scalar indexes использует один `ExternalIndexSorter`. Он удерживает не
более 256 МиБ логических записей одного run, применяет `sort_unstable`, пишет выровненные private
runs через существующий `DirectFile` и сливает до 64 входов стандартным `BinaryHeap`. При большем
числе runs выполняются промежуточные проходы с тем же fan-in.

Scratch находится в `tmp/index-sort/<session_uuid>`. Владелец session удаляет его при успехе,
ошибке или unwind; startup удаляет хвост процесса, завершившегося до RAII cleanup. Private run
никогда не становится catalog-reachable.

Финальные runs образуют replayable sorted stream:

- primary/unique packer проходит его один раз и одновременно проверяет строгую уникальность;
- standard packer первым проходом вычисляет непрерывный диапазон leaf pages, вторым пишет листья
  и posting chains;
- внутренние уровни строятся снизу вверх общей функцией из `(page_no, min_key)`;
- root хранится в каталоге, отдельная metapage не создаётся.

Повторная пара `(key, DiskAtomRef)` считается повреждённым входом и не маскируется dedup-ом.

### 3.2. Label и Count

Для каждого реально встреченного node/edge label полное поколение создаёт отдельный
`idx/lbl_node_<id>.fgidx` или `idx/lbl_edge_<id>.fgidx`. Codec проверяет kind, строгий порядок,
нулевые reserved bytes, полную заполненность промежуточных страниц и плотный `next_page`.
Reader выдаёт записи visitor-у и не материализует весь файл.

`TableIndexKind::Count` допускает пустой field list; в одной таблице может быть не более одного
Count index. Каталог кодирует его стабильным kind byte `2`. Page codec проверяет table identity,
LSN order, page type и нулевой хвост. При membership delta изменившийся exact count ставится одной
page frame; WAL writer записывает `last_updated_lsn`, равный LSN этой frame, до повторного checksum.

Любое изменение label membership атомарно устанавливает `label_index_stale`. До полной COW-
пересборки label lookup выбирает обычный hot-slot scan с residual check.

### 3.3. Публикация и деградация

Новое UUID-поколение сначала получает base files, primary, scalar, Label и Count side-files. Все
файлы синхронизируются до единственного catalog/frame-WAL commit, который делает новые
`DataFileId` и roots достижимыми. Unique violation или ошибка сборки завершают commit до
публикации.

При reopen exact set secondary `DataFileId` выводится из каталога. Missing, unopenable,
short/truncated или checksum-corrupt secondary file исключается из sealed registry, но schema
definition сохраняется. Query использует primary/table scan. WAL recovery и checkpoint также не
пытаются backfill уже отсутствующий перестраиваемый side-file.

Primary index не входит в degradable set. Regression сначала проверяет fallback после checksum-
повреждения и truncation scalar indexes и удаления Label file, затем повреждает `primary.fgidx` и
проверяет обязательный отказ reopen.

Недоступный scalar index дополнительно не разрешает delta mutation: решение опирается на durable
schema, а не на неполный physical registry. Это закрывает риск тихой записи без сопровождения
объявленного unique index.

## 4. Решение по зависимостям

Новые зависимости не добавлены. Для точного контракта нужны уже существующие `DirectFile`,
страничное выравнивание, database-owned scratch, typed storage errors и startup cleanup;
`sort_unstable` и `BinaryHeap` из стандартной библиотеки закрывают собственно сортировку и merge.
Подключение закомментированного `ext-sort` не устранило бы storage-specific run I/O и lifecycle,
поэтому без измеренного выигрыша оно только добавило бы второй контракт временных файлов.

## 5. Correctness evidence

Проверены:

- round-trip и corruption checks для primary, standard/unique, Label и Count pages;
- многолистовые Label chains и неверный atom kind;
- tiny-budget multi-run merge, total ordering и RAII cleanup;
- подача external runs непосредственно в standard packer;
- strict duplicate rejection для unique и повторной standard posting pair;
- сохранение Label/Count после commit, abort, delta и reopen;
- Count `2 → 1` после membership delta и WAL recovery;
- Label stale fallback после смены label;
- одинаковые query results после потери/corruption secondary indexes;
- запрет delta mutation при объявленном, но физически недоступном scalar index;
- фатальность checksum corruption primary index.

## 6. Closure ledger повторного ревью

| ID      | Решение | Исправление                                                                           | Evidence                                           | Остаточный риск                                                  |
|---------|---------|---------------------------------------------------------------------------------------|----------------------------------------------------|------------------------------------------------------------------|
| `S7-R1` | принято | `unreachable!()` в ветке Count заменён типизированной storage error                   | focused compile/tests и повторный просмотр ветки   | ветка остаётся defensive invariant, штатный Count выходит раньше |
| `S7-R2` | принято | существующий fallback regression дополнен повреждением primary после закрытия readers | `local_graph_table_and_edge_survive_reopen` — PASS | диагностика stale secondary пока не видна пользователю (`Q-19`)  |

Повторное read-only ревью после исправлений: `PASS`, новых блокирующих замечаний в границе этапа 7
нет.

## 7. Проверки

Пройдены:

- targeted `rustfmt +nightly --check` всех Rust-файлов этапа;
- `cargo check --lib --no-default-features`;
- `cargo check --lib --no-default-features --features native-async-io`;
- `cargo test --lib --no-default-features storage::graph_index:: -- --test-threads=1` — 11/11;
- `cargo test --lib --no-default-features local_graph_table_and_edge_survive_reopen -- --test-threads=1` — 1/1;
- `cargo test --lib --no-default-features -- --test-threads=1` — 328 passed, 7 ignored;
- `cargo test --lib --no-default-features --features native-async-io -- --test-threads=1` —
  330 passed, 8 ignored.

Общий `cargo +nightly fmt --all -- --check` остаётся `FAIL (pre-existing)`: единственный diff
находится в не относящемся к этапу пользовательском `src/graph/live.rs`. Полный Clippy остаётся
заблокирован накопленным baseline `Q-14`; этот результат не выдан за успешный gate и чужие файлы
не форматировались.

## 8. Открытые измеримые потолки

- `Q-05`: point update пока bulk-перестраивает primary, а scalar index ведёт к COW generation;
  incremental page-WAL и bulk threshold сравниваются на этапе 9.
- `Q-18`: sorted input bounded, но output pages, одна posting group и сгруппированные Label
  memberships ещё удерживаются в памяти; последовательный generation writer относится к этапу 10.
- `Q-19`: fallback корректен, но до `INFO`/`EXPLAIN` пользователь не видит `ready/stale` и причину
  деградации; contract добавляется на этапах 8 и 14.
- `Q-20`: prefix/block compression и fill factor не имеют доказанного выигрыша и не входят в
  format-v1 stage-7 layout.

Ни один из этих пунктов не выполнен наперёд и не помечен закрытым.

## 9. Correction appendix: файловый layout и identity индекса

Дата корректировки: 2026-09-08

Статус: завершено (`C0`–`C6`)

Первоначальный результат этапа выше сохранён как исторический снимок. Корректировка устраняет
расхождение `Q-21` между ним и возвращённым нормативным format-v1 contract:

- `GraphSuperblock` записывается только в `superblock.fgb`;
- каждый из шести изменяемых data-файлов получает собственный одно-страничный пустой freelist;
- `FGFC v4` хранит `companion_of: Option<DataFileId>` в прежнем 32-байтовом fixed header и
  проверяет same-graph, same-generation и one-to-one binding двумя проходами;
- декодер принимает только `FGFC v4`; промежуточный `FGFC v3` получает
  `FG-51S01` и не мигрируется;
- публичный `IndexId` отделяет логическую identity определения от физического `DataFileId`, но
  использует прежний общий schema clock и прежние durable `u32` байты `FGDC v4`;
- full-image и COW rebuild связывают физический index generation по `IndexId`, после чего
  сверяют name, kind, ordered fields и `SchemaVersion`;
- Standard и Unique совместно сортируются по `IndexId` и получают плотные имена
  `idx/std_<ordinal:04>.fgidx` и `idx/uniq_<ordinal:04>.fgidx`; общий B+tree codec и внутреннее
  имя `scalar` сохранены без появления третьего `TableIndexKind`;
- delta commit заменяет только `superblock.fgb`, сохраняя прежние data/freelist identities и
  bindings; обычный page/index hot path freelist-страницы не читает.

Golden/negative coverage проверяет байты и round-trip `FGFC v4`, явный отказ от v3, truncation,
reserved bytes, invalid/missing/cross-graph/duplicate companion, неправильное имя и поколение.
Reopen regression проверяет наличие ровно канонических имён, размер каждого freelist 16 KiB,
раздельные Standard/Unique результаты, сохранение `IndexId` после reopen и COW rebuild,
secondary scan fallback и фатальность повреждения `primary.fgidx`. Delta/WAL/checkpoint и
crash-before-publication покрыты полной storage/datastore матрицей.

### 9.1. Ресурсная цена и reopen

Разделение добавляет на live graph пять страниц относительно объединённого варианта: 80 KiB и
пять registrations. Воспроизводимый ignored-пробник `local_file_layout_reopen_diagnostic` на
текущей машине (5 warm runs, debug test profile) дал:

| Графы | Median | Min | Max |
|------:|-------:|----:|----:|
| 1 | 11,397 ms | 9,757 ms | 18,175 ms |
| 100 | 626,703 ms | 613,290 ms | 840,728 ms |

Эти абсолютные значения не являются paired-сравнением с неподдерживаемым FGFC v3 и не
приписывают всю разницу freelist-файлам. Они подтверждают отдельный потолок eager registry:
100 пустых графов означают около 1800 generation files и 600 проверяемых freelist roots.
Оптимизация lazy handle opening зарегистрирована как `Q-22`; объединение freelists не является
допустимым способом её решения.

### 9.2. Итоговый gate

- `cargo test --lib --no-default-features storage::catalog -- --test-threads=1` — 7/7;
- `cargo test --lib --no-default-features storage::graph_index -- --test-threads=1` — 11/11;
- `cargo test --lib --no-default-features datastore::tests -- --test-threads=1` — 43/43;
- `cargo check --lib --no-default-features` — PASS;
- `cargo check --lib --no-default-features --features native-async-io` — PASS;
- `cargo test --lib --no-default-features -- --test-threads=1` — 331 passed, 8 ignored;
- `cargo test --lib --no-default-features --features native-async-io -- --test-threads=1` —
  333 passed, 9 ignored;
- correction-specific read-only review — PASS, блокирующих замечаний нет;
- correction-specific `git diff --check` — PASS.

Общий `cargo +nightly fmt --all -- --check` по-прежнему блокируется только чужим
`src/graph/live.rs`. Общий Clippy остаётся `FAIL (pre-existing Q-14)`: 2135 накопленных ошибок;
в изменённых correction-specific диапазонах диагностик Clippy нет. Эти два результата не выданы
за успешные gates, а посторонние dirty-файлы не форматировались.
