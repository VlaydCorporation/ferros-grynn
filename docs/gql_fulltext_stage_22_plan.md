# Этап 22. Durable analyzers и Fulltext BM25

**Статус: выполняется.** Этот документ является нормативным и dependency-ordered
планом этапа 22 из [`full_plan.md`](full_plan.md). Итоговые evidence сохраняются в
[`reports/gql_fulltext_stage_22_report.md`](reports/gql_fulltext_stage_22_report.md).

## 1. Граница этапа

Этап добавляет durable analyzer definitions, детерминированный span-aware
pipeline, одно-полевый Fulltext BM25, match slots, score/highlight built-ins,
versioned base/exact delta и существующий maintenance lifecycle. Query result
остаётся точным: отсутствующий, stale либо повреждённый secondary binding
переходит на exact scan; stale posting никогда не выдаётся пользователю.

`FGDC v10`, `FGFC v6` и `STORAGE_FORMAT_VERSION=1` остаются изменяемыми
development identifiers до закрытия `Q-40`. Layout меняется без migration и
compatibility decoder. `FGFC` не получает новые kinds: fulltext base/delta
регистрируются как `DataFileKind::Index`.

Не входят phrase/fuzzy/prefix query DSL, stop-word catalog, multi-field BM25,
FST, Roaring, второй runtime и dynamic analyzer/index registry.

## 2. Analyzer

Публичные schema-типы: `AnalyzerId`, `SchemaAnalyzer`, `AnalyzerTokenizer` и
`AnalyzerFilter`. `AnalyzerId` использует общий `next_schema_id`.

Pipeline выполняется в строгом порядке:

1. один проход built-in tokenization либо versioned PURE UDF;
2. filters в порядке объявления;
3. term/span/resource validation;
4. final tokens.

`FUNCTION` и `TOKENIZERS` взаимоисключающие. Без обоих весь непустой input —
один token. Несколько tokenizers совместно задают границы одного прохода, их
порядок не влияет на результат. UDF обязан иметь exact signature
`fn::name(string) -> array<object>` и возвращать объекты
`{term: string, start: int, end: int}` с half-open Unicode-scalar span исходной
строки. Filter меняет term, но сохраняет span; n-gram children наследуют span
родителя.

`mapper` хранится inline:

```grynql
FILTERS mapper({
    "driven": "drive",
    "drives": "drive",
    "stop_word": null
})
```

Строка заменяет term, `null` удаляет. Runtime path/file reads запрещены.
`search::analyze(analyzer, text)` возвращает final `array<string>` без spans.

`OVERWRITE`/`ALTER` сохраняют `AnalyzerId`, увеличивают `SchemaVersion` и одним
catalog commit переводят зависимые Fulltext definitions на новую analyzer/UDF
version, увеличивают definition version и делают их `Stale`. Удаление analyzer
или function запрещено при наличии ссылки.

## 3. Fulltext

```grynql
DEFINE INDEX books_title
ON CLASS book
FIELDS title FULLTEXT ANALYZER english
BM25(1.2, 0.75)
HIGHLIGHTS
SCOPE BASE;
```

Definition принимает ровно одно объявленное string-поле. `k1` finite и `>0`,
`b` finite и в `0..=1`; defaults `1.2/0.75`. Missing/null не совпадают.
String, давшая ноль terms, входит в `doc_count`, но не в postings.

Операторы:

- `field @@ "query"` — OR уникальных analyzed terms;
- `field @n@ "query"` — тот же match с statement-local slot `1..=65535`;
- `field @AND@ [..]` — все distinct terms;
- `field @OR@ [..]` — хотя бы один term.

Пустой analyzed query даёт `false`. Несколько совместимых definitions требуют
`WITH INDEX`; отсутствие definition — semantic error. `WITH NOINDEX`, stale и
auto-fallback используют ту же definition для exact scan.

Built-ins: `search::score(n)`, `search::highlight(open, close, n)` и
`search::analyze`. Slot живёт только в top-level statement. Highlight требует
`HIGHLIGHTS`, объединяет overlapping spans и не экранирует tags. BM25 считает
в `f64`; ties разрешаются по public stable identity.

## 4. Durable layout и публикация

В текущем `FGDC v10` header offset 44 становится `analyzer_count`; analyzer
section следует за functions. Analyzer entry хранит ID/version/revision/name,
tokenizer bits, ordered filters, exact `FunctionId + SchemaVersion`, comment и
canonical inline mapper. Class-index entry получает tagged kind payload и две
optional physical bindings: immutable base и exact delta.

`FGFT v1` — page-framed side-file. Header фиксирует logical IDs/versions,
scope/class/field, BM25/flags/counts/roots/build revision и implementation
fingerprint. Dictionary детерминирован; RapidHash V3 lookup всегда проверяет
полные UTF-8 bytes. Posting order: term, затем scope-specific 64-bit atom
address; posting хранит `term_freq`, `doc_len` и optional sorted scalar spans.
Большие offsets продолжаются checksummed pages. Delta хранит полную
replacement/tombstone запись документа и exact statistics adjustments.

Файлы generation-local:

```text
ft_<ordinal:04>.fgidx
ft_delta_<ordinal:04>.fgidx
```

Bulk build переиспользует существующий bounded external sort и пишет готовые
stable heap pages потоково. `Vec<AlignedPage>` всего результата и перемещение
заполненных over-aligned stack values запрещены.

## 5. MVCC, planning и maintenance

- `Immediate`: old/new text анализируются в transaction; exact delta
  публикуется с data/catalog/WAL commit.
- `Deferred`: тот же commit помечает definition `Stale`; planner выполняет
  exact scan, worker строит новое generation.
- `CONCURRENTLY`, rebuild и compact используют Stage-20 jobs/coordinator.
- Analyzer/UDF dependency change создаёт один coalesced rebuild job.
- Base/delta swap — один commit; crash оставляет только unreachable generation.
- Scan/index routes регистрируют одинаковый conservative SSI range.

Physical nodes: `FulltextScan` и `FulltextIndexScan`. Forced incompatible/stale
index ошибочен до row pull; automatic route показывает fallback в EXPLAIN.
`EXPLAIN ANALYZE` считает analyzer/index versions, documents, terms, postings,
delta records, score/highlight work, fallback и spill.

## 6. Порядок реализации

| Шаг | Зависит от | Результат                                            |
|-----|------------|------------------------------------------------------|
| F0  | 20–21      | Normative sync, dependency pins, `Q-40`, baselines   |
| F1  | F0         | Analyzer schema/dependencies, FGDC section и goldens |
| F2  | F1         | DEFINE/ALTER/REMOVE/INFO AST → IR → execution        |
| F3  | F1–F2      | Built-ins/UDF/mapper/spans/cache и `search::analyze` |
| F4  | F1         | Fulltext options/catalog/bindings/validation         |
| F5  | F3–F4      | FGFT writer/reader, external-sort and continuations  |
| F6  | F3–F4      | Operators, slots, exact scan, BM25/highlights        |
| F7  | F5–F6      | Index lookup, AND/OR, delta and scan parity          |
| F8  | F5–F7      | Immediate/deferred/concurrent/compact/recovery       |
| F9  | F7–F8      | Optimizer, hints, SSI and EXPLAIN                    |
| F10 | F9         | Correctness matrix                                   |
| F11 | F10        | Performance report, status/AGENTS/Graphify closure   |

## 7. Gates

Correctness covers Unicode NFC/NFKC, ASCII/case/n-grams/Snowball, tokenizer
combinations, mapper bytes, UDF Unicode spans and invalid shapes, dependency
versioning/removal/reopen, BM25 oracle, all operators/slots, multibyte and
overlapping highlights, base/effective scope, scan/base/delta/compact parity,
rollback/SSI/deferred/concurrent/crash, malformed pages/checksums and RAM/local/
Sync/NativeAsync equality.

Performance requires non-search regression `<=5%`; selective Ready Fulltext at
`>=100K` documents must reach `>=3x` scan throughput or remain open with a
profiled bottleneck. Builder/query memory stays within the 256 MiB budget plus
bounded page buffers; immediate update never rewrites base; instrumentation is
`<=2%`. Profiling uses `--profile profiling`, exact EXE/PDB and
`D:\Programming\Tools\Intel\oneAPI\vtune\2026.4`; artifacts live under
`target/gql-stage22/`.
