# Этап 20. Durable maintenance и admin surface

**Статус: J0–J14 завершены, 2026-09-29.** План является нормативным срезом
этапа 20; correctness/performance evidence и принятые resource-policy решения
приведены в
[`reports/gql_admin_stage_20_report.md`](reports/gql_admin_stage_20_report.md).

## 1. Контракт

- Один `MaintenanceCoordinator` на datastore владеет FIFO worker на
  `std::thread + Mutex + Condvar`; второй runtime и параллельные тяжёлые jobs не
  создаются.
- `CONCURRENTLY` возвращает UUIDv7 `JobId` только после durable enqueue в
  `FGJB v1`. `INFO FOR JOB[S]` и `KILL` работают через тот же ledger.
- `DEFINE INDEX ... CONCURRENTLY`, `REBUILD INDEX`, `ANALYZE` и `COMPACT`
  строят недостижимый candidate, синхронизируют его и публикуют одним commit.
- `DEFER` разрешён только Standard/Count. Первая затрагивающая DML атомарно
  переводит definition в `Stale` и удаляет physical candidate; planner всегда
  выполняет exact scan, а forced hint получает typed stale error.
- Последующие commits попадают в bounded `IndexPendingQueue`. Catch-up строит
  полный snapshot candidate; новая relevant revision заставляет retry, а
  переполнение очереди коалесцируется в full rebuild. Очередь намеренно не
  durable: после crash persistent `Stale` восстанавливает полный rebuild.
- Crash reconciliation различает `Queued`, interrupted build/catch-up и
  `Publishing`; `Ready` подтверждается только schema binding с точным
  fingerprint.
- Atomic Import поддерживает `QUARANTINE`, `SORT_MEM` и `PARALLELISM`.
  `FGQR v1` публикуется только вместе с успешным import и читается streaming
  API без раскрытия path.
- `FILL_FACTOR` остаётся typed unsupported: diagnostic gate выявил постоянный
  рост page-set на `11–26%` при допустимом лимите `5%`; дальнейший поиск иной
  bounded/adaptive policy вынесен в `Q-39`. `CHANGEFEED=ON` и `EVENTS=ON`
  принадлежат этапам 27 и 29.

## 2. Несовместимый cutover

`STORAGE_FORMAT_VERSION` остаётся `1`, однако catalog payloads принимаются
только как `FGDC v10` и `FGFC v6`. Миграции и test-only compatibility с v9/v5
нет. `FGFC v6` добавляет `JobCatalog=0x15`,
`ImportQuarantine=0x16`, роли `MetadataCow`/`AuxiliaryImmutable`, canonical
paths `admin/jobs.fgb` и `admin/quarantine/<uuid>.fgq`. Auxiliary files не
являются positional page files.

## 3. Порядок реализации

| Шаг     | Зависит от | Результат                                              |
|---------|------------|--------------------------------------------------------|
| J0      | 19         | Синхронизация GQL/disk/grammar/tech и cutover          |
| J1      | J0         | `FGDC v10`, `FGFC v6`, `FGJB v1`, `FGQR v1`            |
| J2      | J1         | Durable job core и один worker                         |
| J3      | J2         | Admin AST → analysis → IR → execution                  |
| J4      | J2         | Общие sort/statistics/COW builders                     |
| J5      | J3–J4      | Concurrent index build/rebuild и atomic swap           |
| J6      | J5         | Stale fallback, bounded pending queue и catch-up       |
| J7      | J4–J5      | `ANALYZE` и compact operations                         |
| J8      | J1, J3     | Quarantine callback/GQL payload и atomic publication   |
| J9      | J5–J8      | Recovery, cancellation и publication race              |
| J10     | J4, J8     | Resource options и независимый fill experiment         |
| J11–J14 | J6–J10     | Observability, correctness/performance gates и closure |

## 4. Gate

Обязательны golden/malformed bytes всех четырёх formats; отказ v9/v5;
enqueue/build/publish crash boundaries; cancellation до и после publication;
old-Ready preservation при rebuild; exact stale fallback; concurrent DML;
quarantine cleanup/reopen/corruption; RAM/local и Sync/NativeAsync equality;
bounded sorter memory и отсутствие auxiliary handles в page cache.

