# Отчёт по этапу 21: durable PARAM, именованные UDF и оставшийся язык

**Дата:** 2026-09-29  
**Статус:** U0–U12 завершены.

## 1. Реализованный срез

- Durable `PARAM` и overloads `fn::name` принадлежат единому MVCC
  `SchemaCatalog` и общему `next_schema_id`; `FunctionId` является typed
  `SchemaId`.
- `FGDC v10` хранит parameter comments и function source, signature, resolved
  return type, effect, `COST`, `MAXDEPTH` и comment. Typed IR/bytecode не
  персистируются.
- DDL/INFO поддерживают signature qualifiers, `IF NOT EXISTS`, `OVERWRITE`,
  version advance и identity preservation.
- Overload resolution использует `(max rank, sum ranks)` для exact, lossless
  numeric, `number` и precision-losing numeric candidates; одинаковый лучший
  rank является ambiguity.
- Effect analysis транзитивен. `PURE`/`STABLE`/`IMPURE`, `COST`, deterministic
  restrictions, savepoint rollback и bounded direct/mutual recursion проходят
  через общий analyzer/executor.
- Concrete cache ключуется `(FunctionId, SchemaVersion, schema_epoch)` и лениво
  восстанавливает typed body после reopen. `tv::algo` использует тот же UDF без
  второго registry.
- Реализованы cooperative `SLEEP`, runtime-only Unicode matcher, mutable lexical
  array/set helpers, object construction, UUIDv7/ULID/`rand::id`, ICU4X
  `COLLATE`/`NUMERIC`, spill keys и cursor fingerprint validation.

## 2. Correctness evidence

Проверены parameter precedence/reopen, runtime-only value rejection,
overload/named-argument ambiguity, transitive effects, COST, recursion,
savepoint restoration, cache invalidation, `tv::algo`, Unicode matcher,
mutable lvalue restrictions, random-ID deterministic rejection, ICU ordering,
spill parity и cursor fingerprint. RAM/local и обе I/O feature configurations
входят в общую library matrix этапа 20.

После последних runtime fast paths повторены focused schema/UDF tests, включая
bounded recursion; все завершились успешно. Финальные команды:

```text
cargo test --lib --no-default-features -- --test-threads=1
cargo test --lib --no-default-features --features native-async-io -- --test-threads=1
cargo check --lib --no-default-features
cargo check --lib --no-default-features --features native-async-io
cargo +nightly fmt --all -- --check
```

## 3. Методика U11

Использованы тот же Stage-19 baseline, profiling profile и exact common
EXE/PDB, что в отчёте этапа 20. Stage-21 binary:

```text
target/profiling/deps/gql_stage20_21-302c454d54725998.exe/.pdb
```

Каждый microprobe собирал 100 Criterion samples. UDF/closure сравнивались не
одиночным top-level statement, где доминируют общий parse/analyze/transaction
startup, а одинаковым `FOR 0..64`, поэтому ratio измеряет retained runtime call
path. Functional tests отдельно подтверждают, что повторные calls не
перепарсивают и не переанализируют function body.

## 4. Результаты U11

| Probe | Current | Control | Ratio | Gate | Итог |
|---|---:|---:|---:|---:|---|
| Query без UDF: `RETURN 1` | 3,5776 µs | 3,5880 µs Stage 19 | 0,9971× | ≤1,05× | PASS |
| Durable parameter | 6,1286 µs | 6,1375 µs execute parameter | 0,9986× | ≤1,05× | PASS |
| Cached UDF, 64 calls | 45,346 µs | 53,049 µs session closure | 0,8548× | ≤1,15× | PASS |
| ICU numeric collation | 24,192 µs | 22,885 µs binary | 1,0571× | informational | OBSERVED |

Small ICU input не создал и не удержал query-run files; это проверяет сам
benchmark harness. Большие spill/no-spill результаты побайтно совпали в
correctness matrix, а generated sort keys учитываются shared barrier budget.

Matcher хранит один shared input, regex и текущие offsets; успешные matches не
накапливаются в retained collection. Поэтому retained memory остаётся
`O(input + regex)`, а не `O(matches × input)`; Unicode/zero-length progress и
clone-shared state покрыты tests.

## 5. Найденные и устранённые hot paths

Первичный single-call probe показал лишний UDF overhead. Source-level profile и
call-path review привели к четырём точечным изменениям:

1. Direct non-recursive named UDF теперь использует cached `NamedInvoke`, а не
   создаёт новую session closure на каждый call.
2. Пустые durable/session binding maps используют fast path без лишнего merge.
3. Горячие `RETURN`/`SELECT` parser branches проверяются до редких statements.
4. Non-recursive UDF без `MAXDEPTH` не захватывает общий recursion-depth map;
   catalog validation уже запрещает такие recursion cycles.

После изменений 64-call UDF быстрее эквивалентной session closure на 14,5%.
Семантика recursion, effects, savepoints и schema invalidation не менялась.

## 6. VTune

Valid user-mode profiles:

- `target/gql-stage21/vtune-current-return`;
- `target/gql-stage21/vtune-baseline-return`.

Они разрешили Rust symbols/source lines. Current profile: CPU 9,807 s,
`GqlSession::execute_internal` 2,7%, `Parser::peek_keyword` 2,2%; baseline:
CPU 9,816 s и `execute_internal` 2,4%. Aggregate Criterion ratio 0,9971×
подтверждает отсутствие общей regression и instrumentation overhead >2%.

`target/gql-stage21/vtune-current-cached-udf` помечен **INVALID/SKIP**: старый
benchmark wait loop истёк на Stage-20 setup до целевого UDF workload. Этот
result не использован как evidence. Harness теперь ждёт terminal job по
монотонному 60-second deadline, но повторный UDF VTune не требовался после
устойчивого 100-sample call gate и отсутствия нового hotspot в common profile.

Сводка чисел находится в `target/gql-stage21/u11-summary.csv`; Criterion logs и
VTune results сохранены рядом в `target/gql-stage21/`.

## 7. Закрытие U12

- Performance gates query/parameter/UDF пройдены; ICU overhead зафиксирован как
  стоимость явно запрошенной Unicode semantics, а не regression binary sort.
- `FGDC v10`/`FGFC v6` refreeze подтверждён после Stage-21 surface.
- Plan, living specs, module AGENTS и `full_plan.md` синхронизированы.
- Graphify успешно обновлён.

Strict workspace Clippy остаётся внешним debt `Q-14`; lint levels не
ослаблялись и новый runtime surface не получил скрытых blanket suppressions.
