# Этап 10. Atomic Import

## 1. Назначение и статус

Этот документ является dependency-ordered планом реализации `BEGIN IMPORT`.
Текущий прогресс этапа отмечается только в [full_plan.md](full_plan.md); здесь
фиксируются принятый контракт и критерии завершения.

Нормативные источники:

- [gql_spec.md](gql_spec.md) §7.11 — наблюдаемая семантика;
- [gql_grammar.md](gql_grammar.md) — точный синтаксис;
- [gql_tech_spec.md](gql_tech_spec.md) — frontend/execution boundary;
- [disk_storage_spec.md](disk_storage_spec.md) §20.13 — external sort,
  bottom-up build и атомарная публикация side-files.

Этап не считается завершённым до RAM/local/recovery/resource gates и закрытия
`Q-17`/`Q-18`. Распознанные, но неподдерживаемые возможности отслеживает
`Q-33`; они не входят в Windows GA-profile.

## 2. Нормативный контракт

### 2.1. Atomic COW staging

- `BEGIN IMPORT` фиксирует один исходный snapshot и создаёт private COW root.
- Читатели до публикации продолжают видеть старый snapshot.
- Import владеет database-wide writer lease; read-only transactions разрешены,
  конкурирующие writers получают типизированный `ImportInProgress`.
- `SEGMENT SIZE` ограничивает число успешно применённых logical items в одном
  внутреннем transaction segment. Segment не является видимым commit или
  recovery point.
- `END IMPORT` строит недостижимое UUID-поколение, синхронизирует его и
  публикует одним catalog/WAL commit.
- Ошибка, cancellation, panic или раннее завершение отбрасывает staging. `Drop`
  никогда не публикует данные.
- После crash недостижимые поколения и external-sort sessions удаляются
  startup-GC; resume этапом 10 не предоставляется.
- `COMMIT EVERY`, `FG_IMPORT_COMMIT_ROWS`, rollback усечением и
  `FG_IMPORT_NO_SLOT_REUSE` не имеют compatibility path.

### 2.2. Statements и graph scope

Исполняемый профиль этапа 10 принимает `CREATE`, `UPDATE`, `UPSERT`, `DELETE`,
`ATTACH`, `DETACH` и `RELATE`. Import является единственным top-level statement;
вложенный import и import внутри открытого `Transaction` запрещены. DDL,
`INFO`, read statements, отдельные `RETURN` и `ASSERT` в body запрещены.

`FOR`, `IF/ELSE`, `TRY/CATCH` и `THROW` нормативно допустимы, но подключаются
только после реализации самих statements на этапе 12. До этого они завершаются
typed unsupported. После их подключения пойманная языком ошибка не передаётся
`ON VIOLATION`; политика import видит только непойманное item-level нарушение.

`INTO GRAPH` требует существующий graph. Он подставляется в graph-local DML без
явного scope; явное имя другого graph отвергается analysis до staging. Base
`CREATE`/`UPDATE`/`UPSERT` остаются database-global и не создают membership.
Base property update наблюдаем во всех наследующих memberships. Scoped
`DELETE CASCADE` выполняет preflight и отвергается, если затрагивает другой
graph.

Внутренний DML не материализует result rows: отсутствие `RETURN` эквивалентно
`RETURN NONE`, а result-producing `RETURN` является semantic error. Наружу
возвращается только `ImportReport`.

### 2.3. Options

| Option | Этап 10 |
|---|---|
| `INDEXES=DEFERRED` | Default. Primary identity и correctness-critical lookup поддерживаются, secondary/Label/Count/effective indexes строятся перед publication. |
| `INDEXES=MAINTAINED` | Incremental maintenance в private root. |
| `CONSTRAINTS=DEFERRED` | Graph-family и deferred Unique проверяются перед publication. |
| `CONSTRAINTS=IMMEDIATE` | Проверка после каждого logical item. |
| `ON VIOLATION=ABORT` | Первое item-level нарушение отбрасывает весь import. |
| `ON VIOLATION=SKIP` | Нарушивший item атомарно откатывается; требуется явный `REJECT LIMIT > 0`. |
| `SEGMENT SIZE=N` | Положительное число applied items; default `FG_IMPORT_SEGMENT_ITEMS=1_000_000`. |
| `EVENTS=OFF`, `CHANGEFEED=OFF` | Поддерживаются. |
| `QUARANTINE`, `EVENTS=ON`, `CHANGEFEED=ON`, `FILL_FACTOR`, `SORT_MEM`, `PARALLELISM` | Распознаются, но дают typed unsupported до writer lease. |

Item-level violations включают duplicate `RecordId`, schema/type/kind mismatch,
missing atom/endpoint/membership, duplicate membership, локально неверную
topology и immediate Unique. Syntax/semantic program errors, I/O/checksum,
resource exhaustion, cancellation, internal invariant и непойманный `THROW`
всегда прекращают import. Ошибка deferred whole-graph/Unique validation не
приписывается одной записи и отбрасывает весь staging.

Обычный `END` проверяет delta и все hard constraints. `WITH VALIDATION`
дополнительно перепроверяет весь затронутый AtomStore, graph и rebuilt
side-files, включая ранее существовавшие данные.

## 3. Публичный Rust API

Callback API переиспользует существующие методы `Transaction` и не вводит
дублирующий `ImportMutation`:

```rust,ignore
datastore.import(options, |session| {
	for input in inputs {
		session.apply_item(|transaction| {
			// Все связанные DML одного logical item.
			Ok(())
		})?;
	}
	Ok(())
})?;
```

`apply_item` является атомарной границей `SKIP`/`ABORT`; successful items
автоматически группируются в segments. Публичны `ImportOptions`,
`ImportReport`, `ImportItemOutcome`; physical staging остаётся crate-private.
`ImportReport` содержит processed/applied/skipped counts, число segments,
totals и bounded details нарушений, index/phase timings, выбранные modes и
признак full validation. Локальные filesystem paths в result/error запрещены.

## 4. Dependency-ordered реализация

| Шаг | Зависит от | Результат |
|---|---|---|
| I0. Status sync | — | Синхронизировать living docs, `full_plan` и реестр. |
| I1. Normative contract | I0 | Утвердить grammar, options, atomicity, graph scope, validation и report. |
| I2. Datastore core | I1 | Private root, writer lease, segment rotation, callback API и statuses. |
| I3. RAM vertical slice | I2 | Parser → analysis → IR → `Datastore::execute` → текущие DML → report. |
| I4. Local COW staging | I2 | Unreachable generation, Direct writer, sync ordering и один publication commit. |
| I5. Streaming index output | I4 | Последовательная запись B+tree/Label/posting pages; механизм `Q-18`. |
| I6. Bounded statistics | I2 | Один existing external-sort run/merge для property histograms; механизм `Q-17`. |
| I7. Rebuild/validation | I3–I6 | Deferred rebuild, Unique/family checks, full validation и publication. |
| I8. Cleanup/recovery | I4, I7 | RAII/startup cleanup и crash-point tests без resume. |
| I9. Local end-to-end | I7, I8 | RAM/local/reopen/recovery equality для всех текущих DML. |
| I10. Correctness gate | I9 | Atomicity, rejection, concurrency, corruption и result gates. |
| I11. Resource/performance gate | I10 | RSS, WAL, write amplification, throughput и profiling. |
| I12. Closure | I11 | Report, закрытие `Q-17/Q-18`, отметка этапа 10 и Graphify. |

I5 и I6 независимы после своих зависимостей. Первый проверяемый checkpoint —
I0–I3: полный RAM vertical slice; local возвращает typed unsupported до writer
lease и не создаёт файлов. Этот checkpoint не закрывает этап 10.

## 5. Gates

Correctness gate проверяет top-level/transaction restrictions, graph-scope
injection, all-or-nothing ABORT, bounded SKIP, deferred final failure, old
reader snapshot, writer lease, failures во всех publication phases, startup
cleanup, Rust/GQL equivalence и RAM/local/reopen equality.

Resource gate требует:

- bounded segment bookkeeping и не более 1000 detailed violations;
- fixed 256 MiB external-sort run budget;
- отсутствие `O(index pages)`, `O(labeled atoms)`, `O(max posting group)` и
  `O(rows × property fields)` import-private buffers;
- direct streaming output и одну final WAL publication;
- не более 5% необъяснённой regression streaming builder;
- `DEFERRED` не медленнее `MAINTAINED` более чем на 3% на large indexed import,
  иначе default не утверждается и этап остаётся открытым.

Final pages выделяются в стабильных heap slots до encode; возврат заполненных
over-aligned `AlignedPage` по значению не используется. Профилируемые тесты
собираются только с `--profile profiling` и exact EXE/PDB.
