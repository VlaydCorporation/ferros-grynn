# Statistics module guidance

`stats` хранит внутренний MVCC-контракт статистики планировщика: точные graph,
table и label counters, degree distributions и property histograms. Он не
выбирает планы и не владеет физическими страницами: `datastore` публикует
снимки, а `storage/graph_pages` кодирует их в `statistics.fgb`.

- Простые счётчики изменяются одной транзакционной дельтой при commit.
- Degree/property histograms заменяются только полным bulk scan и могут быть
  устаревшими; это влияет лишь на качество будущего плана.
- Границы property buckets используют order-preserving codec из `values`.
- Любые счётчики и размеры проверяются до allocation или арифметики адресов.
