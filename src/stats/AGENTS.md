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
- Degree/property histograms заменяются только полным bulk scan и могут быть
  устаревшими; это влияет лишь на качество будущего плана.
- Границы property buckets используют order-preserving codec из `values`.
- Любые счётчики и размеры проверяются до allocation или арифметики адресов.
