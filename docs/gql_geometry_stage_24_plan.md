# Этап 24 — Geometry R-tree

Статус: в работе. Этап закрывается только после всех correctness, resource и performance gates.
Зависимости: этапы 7 и 20. Открытые расширенные gates этапов 22–23 не блокируют этот срез.

## Нормативный контракт

- Собственные публичные `Geometry`/`Position`; `geo = 0.33.1`, default features выключены,
  используется как внутренний validator и kernel. Eq/Ord/Hash и canonical encoding сохраняются.
- WGS-84, две конечные координаты longitude/latitude в диапазонах ±180/±90.
  Топология плоская: сегменты линейны в lon/lat, антимеридиан не оборачивается.
  Пересекающую его область представляют разделёнными частями MultiPolygon.
- INSIDE/geo::within — OGC Within; граница точки не внутри. INTERSECTS включает касание.
  OUTSIDE и NOTINSIDE — отрицание Within после проверки missing/null, которые не совпадают.
  geo::distance поддерживает только две Point и возвращает геодезическое расстояние в метрах.
- Пустые MultiPoint/LineString/MultiLineString/Polygon/MultiPolygon/Collection валидны и не
  получают MBR. Непустые линии содержат минимум две различные позиции. Непустые кольца
  содержат минимум четыре позиции и явно замкнуты до создания `geo::Polygon`; OGC validation
  проверяет самопересечения, holes и MultiPolygon. Пустое кольцо допустимо только как единственное
  внешнее кольцо пустого Polygon. Общий предел: глубина 64, суммарно 1 000 000 позиций/элементов.
- Validation допускает до 128 MiB retained/scratch памяти. Для overlapping Collection
  predicates используется bounded arrangement исходных f64 coordinates без quantized union;
  лимит pairwise work — 16 000 000 comparisons, выход даёт typed resource error.
- ALLINSIDE/ANYINSIDE/NONEINSIDE применяются к Array/Set слева; пустые дают true/false/true.
  Каждый элемент проверяется, включая элементы после уже определившегося результата.
- Definition: одно geometry-поле, optional TYPE, BASE/EFFECTIVE scope, общие IndexId/version/state.
  Fingerprint включает definition, scope, TYPE, kernel/model version и fill policy.
- STR base: собственные pages, запись MBR(f64×4)+row/child(u64) = **40 байт**,
  capacity **408**, fill 0.95; стабильные ties по row identity. Пустой root — NULL.
  Сортировки X, затем stripe/Y используют общий bounded external sorter; страницы пишутся потоком.
- Base immutable; точная WAL-delta хранит replacement MBR/tombstone по row identity.
  Delta исключает заменённые base entries. Geometry для refinement читается из того же snapshot.
  Immediate DML обновляет delta; Deferred атомарно делает definition Stale.
- R-tree допускается для положительных INSIDE/INTERSECTS; обязательный exact post-filter.
  OUTSIDE, отрицания и distance используют exact scan. Forced несовместимый/stale index ошибочен
  до row pull. Повреждённый secondary даёт автоматический fallback, forced — typed error.
- Bulk ANALYZE хранит bounded sample 1024 MBR; planner учитывает candidates/refinement.
  EXPLAIN показывает binding, scope, post-filter, delta и причину fallback.
- Development layout меняется на месте по Q-40, без compatibility decoders.

## Порядок

| Шаг | Результат                                                                  |
|-----|----------------------------------------------------------------------------|
| G0  | Синхронизация specs/layout и исходные scan/DML measurements                |
| G1  | Общий validator на conversion/persistence/predicate/build boundaries       |
| G2  | GEOMETRY TYPE через parser/AST/analysis/IR/execution и публичный index API |
| G3  | Exact spatial oracle, operators, conversions и geo builtins                |
| G4  | Bounded STR page builder и selective reader                                |
| G5  | Exact delta, Immediate/Deferred, jobs, compact и COW publication           |
| G6  | Spatial planner/access и обязательный exact refinement                     |
| G7  | MBR sample, estimates, EXPLAIN/counters                                    |
| G8  | Полная проверка и evidence report                                          |

## Gates

Все семь видов, holes/boundaries/empties/malformed rings, finite/range/depth limits;
scan/index parity, MBR false positives, BASE/EFFECTIVE, private/old snapshots, SSI,
delta/tombstones/compact, abort/reopen/checkpoint, concurrent build/cancel/overflow/crash,
reserved bytes/lengths/cycles, budgets до allocation/write. RAM, local Sync и NativeAsync
проверяются раздельно. Focused tests, `cargo check --lib --no-default-features`, nightly fmt,
Clippy; при закрытии полный datastore correctness gate и затронутые integration targets.
Paired workloads 10K/100K/1M, warm/cold, mutation/rebuild; latency/page reads/WAL/peak memory,
непрофильная regression ≤5%. Отчёт: `reports/gql_geometry_stage_24_report.md`.
