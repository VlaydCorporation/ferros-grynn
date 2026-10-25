# Statistics module guidance

`stats` хранит внутренний MVCC-контракт статистики планировщика: точные base
class, graph-effective class и label counters, degree distributions и property
histograms. Он не
выбирает планы и не владеет физическими страницами: `datastore` публикует
снимки, а `storage/graph_pages` кодирует их в `stats/graph_stats.fgb`,
`stats/label_stats.fgb` и `stats/prop_stats.fgb`. Root загружается при открытии;
degree, label и property части читаются независимо и лениво через точный
транзакционный snapshot.

- Простые счётчики изменяются одной транзакционной дельтой при commit.
  `has_revision_updates` наблюдает exact base/graph touched flags; immutable
  publication не делает COW при их отсутствии, при наличии сохраняет прежние
  revision stamps и очистку flags.
- Degree/property histograms заменяются только полным bulk scan и могут быть
  устаревшими; это влияет лишь на качество будущего плана.
  При final revision publication удаляется только degree histogram,
  чья node-label отсутствует в окончательных exact label counters. Промежуточное
  исчезновение имени при swap не инвалидирует distribution; global и surviving
  histograms, их buckets и analyzed_revision сохраняются. Storage может
  механически перекодировать dense label IDs при смене словаря, без пересчёта
  распределения и без присвоения старых buckets другому имени.
- Границы property buckets используют order-preserving codec из `values`.
- Geometry distribution хранит до 1024 MBR и отдельную nonempty population;
  bulk ANALYZE учитывает BASE/EFFECTIVE overlays. Sample не меняет query correctness.
- ANALYZE использует общий `math::HyperLogLog`, как Neighbourhood SKETCH. Hash domain
  задаёт consumer; merge допускается только при одинаковой precision/domain.
- Любые счётчики и размеры проверяются до allocation или арифметики адресов.
- Topology replacement меняет exact node-incidence totals в общей delta. После
  delete conservative degree maximum ограничен новым exact total; ANALYZE
  восстанавливает exact maximum и distributions.
