# Исследование порчи первого байта `AlignedPage` на Windows MSVC

Дата: 2026-09-23.

## 1. Итог

Проблема реальна, но прежнее объяснение «Windows не умеет надёжно держать
`#[repr(align(16384))]` на стеке» было слишком широким.

Доказано следующее:

- в nightly ASan-сборке `x86_64-pc-windows-msvc` первый byte заполненной
  `NodeHot`-страницы менялся с `0x46` на `0x00` до Direct I/O, WAL и resident
  arena;
- сбой воспроизводился в полном graph-image builder и исчезал при
  инициализации уже выделенного конечного `Vec<AlignedPage>` через `&mut`;
- адрес конечного frame до и после encode не менялся и удовлетворял
  `addr % 16384 == 0`;
- замена `mimalloc` на `std::alloc::System` не устраняла исходный сбой;
- Miri не обнаружил UB на чистом builder-пути;
- простой автономный пример возврата `Result<Vec<AlignedPage>>` под тем же
  ASan не воспроизвёл порчу;
- диагностический `eprintln!` внутри проблемной closure менял codegen и убирал
  проявление;
- disassembly прежнего by-value wrapper показывал два полных 16 KiB copy через
  ASan wrappers, динамическое выравнивание stack и ASan fake-stack; новый
  `&mut AlignedPage` initializer этих page-wide return copies не имеет.

Поэтому наиболее точная текущая классификация — **codegen-sensitive дефект в
сочетании Rust nightly + LLVM/MSVC ASan + возврат большого over-aligned значения
по значению в сложном builder-контексте**. Ошибка компилятора или sanitizer
instrumentation правдоподобна, но пока не доказана минимальным reproducer.
Прикладной UB также не доказан: выполненный Miri-сценарий чист, но один успешный
запуск Miri не является доказательством soundness.

Принятое исправление минимально: bulk-builder сначала выделяет все конечные
frames, затем заполняет их in-place. `repr(align(16384))`, page codec, checksum,
Direct I/O и allocator не меняются.

## 2. Что именно повреждалось

До исправления точный ASan-тест
`occurrence_image_round_trips_identity_topology_labels_and_overlay` получал
`NodeHot` magic:

```text
ожидалось: [46, 48, 4e, 01] = b"FHN\x01" = 0x014e4846 LE
получено:  [00, 48, 4e, 01]                 = 0x014e4800 LE
```

Та же ошибка поднималась в полном тесте
`one_wal_transaction_publishes_atom_store_graph_and_memberships`, но
дополнительные probes локализовали её раньше I/O: страница уже была повреждена
непосредственно после построения массива hot pages.

Локальный page codec, `AlignedPage::reset`, `seal` и валидация отдельно
проходили. Это исключило Direct I/O, WAL, cache publication и checksum reader из
необходимых условий воспроизведения.

## 3. Среда

```text
host: x86_64-pc-windows-msvc
stable:  rustc 1.98.0, LLVM 22.1.8
nightly: rustc 1.100.0-nightly (2026-09-01), LLVM 23.1.0
Miri:    0.1.0 (nightly 5db7f4be8)
cargo-asm: 0.2.63
```

ASan EXE импортирует динамический runtime и, среди прочего, symbols:

```text
__sanitizer_cov_trace_pc_guard_cleanup__dll
__sanitizer_cov_pcs_cleanup__dll
__sanitizer_cov_8bit_counters_cleanup__dll
```

Standalone LLVM 23 runtime в
`D:\Programming\SDKs\LLVM\lib\clang\23\lib\windows` этих cleanup exports не
содержит. Runtime из MSVC toolset 14.50 содержит их. Поэтому одинаковый major
LLVM не гарантирует ABI-совместимость ASan DLL с конкретным Rust-linked EXE.
Ошибка `procedure entry point ... cleanup__dll was not found` была ошибкой
выбора runtime, а не выводом анализатора о коде.

Рабочий запуск в исследованной среде:

```powershell
$asan = 'D:\Programs\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.50.35717\bin\Hostx64\x64'
$env:PATH = "$asan;$env:PATH"
```

Microsoft документирует, что MSVC ASan DLL устанавливаются рядом с конкретным
compiler toolset, а не в `System32`; это согласуется с наблюдением:
[AddressSanitizer known issues and limitations](https://learn.microsoft.com/en-us/cpp/sanitizers/asan-known-issues?view=msvc-170#addresssanitizer-runtime-dll-locations).

## 4. Матрица экспериментов

| Проверка                                          | До in-place fix |    После fix | Вывод                                                  |
|---------------------------------------------------|----------------:|-------------:|--------------------------------------------------------|
| Stable native, exact image builder                |            PASS |         PASS | Обычная MSVC-сборка сама по себе не воспроизводит сбой |
| Nightly ASan, локальный page codec                |            PASS |         PASS | Codec не является достаточной причиной                 |
| Nightly ASan, чистый graph-image builder          |            FAIL |         PASS | I/O/WAL не нужны для сбоя                              |
| Nightly ASan, полный WAL/publication test         |            FAIL |         PASS | Исправлен исходный пользовательский сценарий           |
| Nightly ASan, отдельный final-buffer address test |  не существовал |         PASS | Конечный адрес стабилен и выровнен                     |
| Nightly ASan + `System` allocator                 |            FAIL | не требуется | `mimalloc` не является причиной                        |
| Miri, чистый graph-image builder                  |            PASS |         PASS | В выполненной ветке UB не найден                       |
| Автономный small ASan repro                       |            PASS |         PASS | Общий compiler reproducer пока не получен              |
| Диагностический print в проблемной closure        |            PASS | не требуется | Симптом чувствителен к codegen/layout                  |

Финальный отдельный ASan-тест проверяет два соседних frame и выводит для каждого
адрес до encode, после encode и при входе в consumer loop:

```text
Hot buffer 0: before=0x131342ba4000, after=0x131342ba4000,
              in_loop=0x131342ba4000, mod=0
Hot buffer 1: before=0x131342ba8000, after=0x131342ba8000,
              in_loop=0x131342ba8000, mod=0
```

Чтобы сам вывод не маскировал codegen-sensitive симптом, allocation,
инициализация, loop и assertions находятся в отдельном `#[inline(never)]` helper
без logging. Внешний test печатает только уже возвращённые числовые адреса.

После этого тем же EXE прошли три exact-теста: address/alignment regression,
чистый image builder и полный WAL/publication path.

## 5. Проверка alignment

Rust требует хранить значение alignment `n` только по адресу, кратному `n`, а
size типа обязан быть кратен alignment. `#[repr(C, align(16384))]` является
нормальным способом повысить alignment; это не advisory-аннотация:
[Rust Reference: type layout](https://doc.rust-lang.org/stable/reference/type-layout.html#size-and-alignment).

В исследовании проверялись адреса:

- сразу после выделения конечного `Vec<AlignedPage>`;
- после `initialize_node_hot_page(&mut frame, ...)`;
- перед последующей slot mutation/validation.

Ни изменения адреса, ни нарушения `addr % 16384 == 0` не обнаружено. Поэтому
гипотеза «allocator вернул невыровненную память» опровергнута для
воспроизводившегося пути.

`Box<T>` и `Vec<T>` обязаны запрашивать `Layout` типа и возвращать правильно
выровненное allocation; например, `Box::into_raw` документирует properly
aligned pointer: [Rust `Box`](https://doc.rust-lang.org/stable/std/boxed/struct.Box.html#method.into_raw),
[Rust `Layout`](https://doc.rust-lang.org/stable/std/alloc/struct.Layout.html).
Однако выражение `Box::new(encode()?)` не гарантирует отсутствие промежуточного
by-value значения до помещения в allocation. Поэтому `Box` стабилизирует
конечный адрес, но не является общим доказательством устранения проблемного
codegen-пути.

## 6. Аудит unsafe и Miri

До точки первого изменения magic использовались safe page encoders. Raw slice
conversion в `AlignedPage::slice_as_bytes[_mut]` не участвовал в чистом builder
reproducer. До повреждения не найден reachable raw write, выход за page bounds
или aliasing mutable references.

Команда Miri:

```powershell
cargo +nightly miri test --locked --lib --no-default-features `
  storage::graph_pages::image::tests::occurrence_image_round_trips_identity_topology_labels_and_overlay `
  -- --exact --nocapture --test-threads=1
```

Результат: `1 passed`, 27.27 s.

Это снижает вероятность UB в исполненной ветке, но не закрывает вопрос
полностью. Miri прямо указывает, что отсутствие найденного UB не доказывает
soundness и что Windows platform APIs поддерживаются слабее; кроме того, Miri —
интерпретатор, он не выполняет проблемный LLVM machine code:
[Miri limitations](https://github.com/rust-lang/miri/blob/master/README.md#limitations).

## 7. Что показал machine-code анализ

`cargo asm` смог собрать инструментированный target, но не нашёл private
`new_node_hot_page` по имени в выбранном library artifact. Это зафиксировано как
`SKIP` именно для symbol lookup cargo-show-asm, а не как отсутствие анализа.
Точный EXE/PDB был исследован через `llvm-pdbutil` и `llvm-objdump`.

Удалённый после исправления by-value wrapper имел:

- `__chkstk`/stack probe примерно на `0xffe8` bytes;
- динамическое округление `rsp` по маске `-0x4000`;
- ASan fake-stack bookkeeping;
- вызов initializer;
- две page-wide операции размером `0x4000` через ASan memcpy wrappers: сначала
  между temporaries, затем в payload возвращаемого `Result`.

Новый `initialize_node_hot_page(&mut AlignedPage, ...)` принимает адрес
окончательного frame от caller. Его собственный обычный stack frame мал
(`sub rsp, 0xe8`; ASan metadata около `0x1a0`), и возврат не копирует 16 KiB
страницу.

Это не называет точную неверную instruction, но даёт механическое объяснение,
почему in-place API одновременно:

1. убирает наблюдавшийся чувствительный путь;
2. сокращает stack pressure;
3. исключает две полные копии страницы;
4. сохраняет тот же on-disk результат.

Windows использует stack probing для больших frames; назначение `__chkstk`
описано Microsoft: [`__chkstk` routine](https://learn.microsoft.com/en-us/windows/win32/devnotes/-win32-chkstk).

## 8. Compiler bug или UB

### Аргументы в пользу compiler/sanitizer codegen defect

- порча проявляется только в ASan-инструментированном сложном контексте;
- logging меняет результат без изменения данных;
- адреса удовлетворяют языковому alignment contract;
- Miri проходит;
- allocator replacement не меняет исходный результат;
- in-place API устраняет page-wide return copies и сбой.

В LLVM существуют ошибки того же общего класса — неверное сохранение alignment
при оптимизации stack moves, например
[llvm-project#208092](https://github.com/llvm/llvm-project/issues/208092).
Это только аналогия: там другой минимальный пример и другой симптом, поэтому
она не доказывает тождество причин.

### Почему compiler bug ещё не доказан

- автономный dependency-free `Result<Vec<repr(align(16384))>>` test проходит;
- точная неверная instruction или ASan shadow transition не локализована;
- Rust Unstable Book на 2026-09-23 не включает
  `x86_64-pc-windows-msvc` в официальный список поддерживаемых Rust ASan targets:
  [sanitizer compiler flag](https://doc.rust-lang.org/beta/unstable-book/compiler-flags/sanitizer.html#addresssanitizer);
- Microsoft предупреждает об ограничениях partially sanitized executable и
  interposition/runtime: [MSVC ASan limitations](https://learn.microsoft.com/en-us/cpp/sanitizers/asan-known-issues?view=msvc-170).

Корректная формулировка: **приложение больше не проходит через наблюдавшийся
опасный codegen shape; upstream-ready root cause пока отсутствует**.

## 9. Зависимость от Windows и allocator

Это не доказанная проблема «любого Windows allocator»:

- stable Windows MSVC тест проходил;
- `mimalloc` и `System` одинаково воспроизводили прежний ASan-сбой;
- оба allocators выдали корректно выровненный конечный frame;
- порча возникала до обращения к Direct I/O.

Следовательно, менять global allocator или добавлять custom page allocator нет
оснований. Такой change не исправлял воспроизведение и расширял бы unsafe и
операционный риск.

## 10. Как решают aligned Direct I/O buffers

Windows требует контролировать alignment buffer address, offset и длины для
unbuffered I/O. Microsoft рекомендует page-aligned `VirtualAlloc` либо
`_aligned_malloc`, а фактическую границу сверять с устройством:
[File buffering](https://learn.microsoft.com/en-us/windows/win32/fileio/file-buffering).

RocksDB не снимает alignment с логических блоков «для удобства». Его Direct I/O
слой использует отдельный `AlignedBuffer`, хранит его достаточно долго для
sync/async completion и возвращает slices, заимствующие этот storage:
[RandomAccessFileReader](https://github.com/facebook/rocksdb/blob/main/file/random_access_file_reader.h),
[Direct I/O notes](https://github.com/facebook/rocksdb/wiki/Direct-IO).

PostgreSQL AIO design также направляет Direct I/O непосредственно между
устройством и долговечными shared-buffer frames, а lifetime I/O state держит в
shared memory:
[PostgreSQL AIO design](https://github.com/postgres/postgres/blob/master/src/backend/storage/aio/README.md).

Общий практический паттерн тот же, что принят здесь: **долговечный конечный
frame/buffer выделяется первым, I/O или codec пишет непосредственно в него,
а заимствования не переживают владельца**.

## 11. Рассмотренные варианты

### Оставить `repr(align(16384))` и писать in-place — выбран

Плюсы: нет нового unsafe, allocator или copy; сохраняется Direct I/O contract;
убираются большие return temporaries. Три ASan regressions и Miri прошли.

### `Box<AlignedPage>`

Подходит для одиночной долгоживущей metadata page и гарантирует alignment
allocation. Не гарантирует, что `Box::new(make_page())` не создаст сначала
большой temporary. Для bulk path хуже конечного `Vec`/arena и создаёт allocation
на каждую страницу.

### Caller-owned aligned stack buffer

Языково допустим: caller может создать aligned storage и передать `&mut`.
Однако 16 KiB на страницу увеличивает stack pressure, а массив страниц для
реального builder всё равно должен пережить функцию. Это годится для маленькой
локальной операции, но не является разумным bulk-storage design.

### Снять явный alignment

Не принято. `[u8; 16384]` с alignment 1 нельзя безопасно передавать как Direct
I/O buffer без отдельной проверки/копии. Тогда потребуется второй aligned
transport buffer или отдельный arena type, что добавит memcpy и lifetime
сложность. Рассматривать только если будущий измеренный выигрыш от разделения
logical page и transport frame превысит эту стоимость.

### `MaybeUninit`, placement-like API или custom allocator

Не принято. Обычный `Vec<AlignedPage>` уже выделяет память с нужным `Layout`.
Unsafe initialization не даёт преимущества для 16 KiB zeroed page, зато
создаёт proof obligations для partial initialization и panic paths.

### `VirtualAlloc` arena

Технически корректен и полезен, если нужны reserve/commit, большой стабильный
address range или direct scatter into frames. Для текущего builder это лишний
Windows-only wrapper: стандартный allocation уже выровнен, а причина не в
allocator. Возвращаться только при измеренной потребности resident/transport
arena, а не как обход этого дефекта.

## 12. Внесённое исправление

`NodeHot` и `EdgeHot` builders больше не возвращают заполненный
`AlignedPage` по значению. Теперь:

1. `Vec<AlignedPage>` полностью выделяется и заполняется нулевыми frames;
2. после завершения allocation каждый final slot передаётся в
   `initialize_node_hot_page`/`initialize_edge_hot_page` по `&mut`;
3. tombstones, header и checksum кодируются уже по окончательному адресу;
4. дальнейшие slot updates происходят по тому же адресу.

Отдельный test
`final_buffers_preserve_addresses_alignment_and_headers` проверяет:

- адрес до/после initialization;
- `addr % align_of::<AlignedPage>() == 0` в обеих точках;
- отсутствие перемещения;
- точный magic;
- checksum/type validation до и после следующей slot mutation.

## 13. Оставшийся риск и следующий минимальный шаг

В `src/storage` остаются by-value encoders для одиночных superblock, freelist,
adjacency, statistics и index pages. Массово переписывать их без
воспроизведения не следует: это расширило бы diff и могло изменить performance.

Следующий dependency-ordered аудит:

1. запускать ASan regression для каждого реально используемого bulk builder;
2. при воспроизведении переводить общий owner на `encode_into(&mut AlignedPage)`,
   а не латать отдельные callers;
3. сохранить failing EXE/PDB и exact source revision;
4. уменьшить один failing case до standalone crate без project dependencies;
5. только после стабильного reproducer подавать issue в rustc/LLVM с IR,
   assembly и runtime export matrix.

### 13.1. Результат первого bulk-builder аудита

Аудит выполнен 2026-09-24 на
`rustc 1.100.0-nightly (6bb1652a0 2026-09-22), LLVM 23.1.1` с тем же runtime
MSVC 14.50. Реально используемыми bulk builders считались multi-page owners,
достижимые из `local_store::image::build_generation`; одиночные superblock/freelist
codecs в этот шаг не включались.

Первый запуск дал новый воспроизводимый сбой:

```text
storage::atom_store_image::tests::atom_store_image_round_trips_global_ids_primary_and_properties
page 1: page magic 0x01504800 does not match type PropLargeHeap
```

Перед изменением сохранены точные failing EXE/PDB в
`target/aligned-page-investigation/failing-atom-store-715ea68f/`. Source state:

```text
HEAD: 715ea68f7319f173d003356758f70802b1a72c90
atom_store_image.rs: f71d6c60094c0911ffac122313e91b77d5c1f11af91ec2300fc2e2bb6e1d73e6
property_page.rs:    557c7df802d494f0f43de5371c2a063121881c60f16b400a2d791f4b5735af99
property_dictionary.rs: dbda86718a1224decc7b3b784162a9651b7be0567dd8ce628e9ac85796897170
```

Одиночный `large_property_chain_round_trips_and_validates_links` под ASan
проходил. Сбой требовал более сложного owner-пути: заполненные страницы
возвращались как `Vec<AlignedPage>`, переносились в
`Vec<(PageNo, AlignedPage)>`, сортировались и затем снова извлекались по
значению. Частичное устранение только внутреннего return temporary убрало
повреждение magic, но тот же AtomStore test начал получать
`property pages are not contiguous`. Это локализовало оставшийся чувствительный
shape в owner, а не в page codec.

Общий `property_page` owner и оба его потребителя переведены на in-place
заполнение:

- `PropertyPageBuilder` хранит только metadata и пишет в caller-owned final
  frame через `&mut AlignedPage`;
- large property/overlay chains добавляют нулевые slots прямо в итоговый
  `Vec<AlignedPage>` и инициализируют полученный slice на месте;
- AtomStore и graph overlay больше не используют
  `Vec<(PageNo, AlignedPage)>`, сортировку или извлечение заполненных страниц по
  значению.

Итоговая ASan-матрица:

| Production owner                              | Regression                                                                                                         |      Результат |
|-----------------------------------------------|--------------------------------------------------------------------------------------------------------------------|---------------:|
| Catalog page chain                            | `exact_header_and_multi_page_round_trip`                                                                           |           PASS |
| Property dictionary                           | `root_entry_and_hash_pages_have_exact_offsets`                                                                     |           PASS |
| AtomStore/property store/primary              | `atom_store_image_round_trips_global_ids_primary_and_properties`                                                   | PASS после fix |
| Graph occurrence/adjacency/labels/overlay     | `occurrence_image_round_trips_identity_topology_labels_and_overlay` и `sparse_overlay_round_trips_hidden_and_null` |           PASS |
| Base и graph-local statistics                 | `base_statistics_round_trip_keeps_split_ranges` и `split_statistics_are_exact_and_auxiliary_files_are_lazy`        |           PASS |
| Standard/Unique/Label/Count scoped indexes    | `scoped_images_round_trip_base_effective_class_label_and_count`                                                    |           PASS |
| Final file image                              | `file_image_preserves_over_aligned_page_bytes`                                                                     |           PASS |
| Полный `local_store::image::build_generation` | `commit_survives_close_and_reopen` и `normalized_atom_model_survives_restart_checkpoint_and_reopen`                |           PASS |

Таким образом, пункт 1 аудита выполнен для текущих production bulk builders, а
обнаруженный owner прошёл пункт 2. Минимизация в standalone crate и upstream
issue в этом шаге не выполнялись.

Rudra не запускался: `cargo-rudra` в среде отсутствует, а upstream repository
[Rudra](https://github.com/sslab-gatech/Rudra) архивирован и привязан к старому
nightly toolchain. Для текущего safe builder-path он не даёт более сильного
сигнала, чем уже выполненный Miri/ASan/assembly анализ.

## 14. Команды финальной проверки

```powershell
$env:RUSTFLAGS = '-Zsanitizer=address'
cargo +nightly test --locked `
  --target x86_64-pc-windows-msvc `
  --target-dir target\aligned-page-investigation\asan-final `
  --lib --no-default-features --features native-async-io `
  storage::graph_pages::hot::tests::final_buffers_preserve_addresses_alignment_and_headers `
  --no-run
```

Затем exact EXE, напечатанный Cargo, запускался с MSVC 14.50 ASan runtime для:

```text
storage::graph_pages::hot::tests::final_buffers_preserve_addresses_alignment_and_headers
storage::graph_pages::image::tests::occurrence_image_round_trips_identity_topology_labels_and_overlay
storage::graph_store::tests::roundtrip::one_wal_transaction_publishes_atom_store_graph_and_memberships
```

Все три теста прошли по одному, без ASan diagnostics.
