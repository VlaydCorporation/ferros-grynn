# LIVE module guidance

Этот файл дополняет корневой `AGENTS.md` для `crates/fg-engine/src/live`.

Модуль владеет публичными owned recipes, identities, options, cursors, logical
images/deltas и SDK adapter. Query analysis/assembly остаются в `gql`, immutable
root/FCW/SSI/publication/queues — в `datastore`, explicit codecs/recovery — в
`storage`, concrete family layouts и COW — в `graph`.

- `graph_image` хранит qualified logical identities, named effective properties,
  topology, containment и stable source mappings. Snapshot-local AtomId не
  является identity delta или durable source. Shared logical scopes хранятся
  один раз; разные owned views обязаны иметь согласованное содержимое.
- Capture/restore/merge используют существующие byte/work controls. Admission
  предшествует deep copies, включая owner routes, SDK name registry и mutable
  snapshots. Family validators и containment проверяются до принятия candidate.
- `graph_subscription` использует обычный LIVE consumer, целые commit deltas и
  durable ACK. Applied-but-unconfirmed token сохраняется при ошибке ACK;
  duplicate delivery не применяет graph revision повторно.
- Mutable polling сравнивает canonical dirty targets. Overlap и structural merge
  failure дают owned `LiveGraphConflict`; решение принадлежит caller. Pending
  conflict запрещает bypass через resume и удерживает apply/ACK. Ошибка resolution
  сохраняет conflict и прежний graph. GAP/fault сохраняют last agreed revision;
  resnapshot всегда явный и создаёт новую incarnation.
- `next_event` и `live_graph_select!` используют один consumer и один borrowed
  event future. Losing caller selector branch отменяет только ожидание, сохраняя
  delta и ACK. Модуль не добавляет executor, runtime или competing subscriptions.
- `flush(owned Transaction)` делегирует source preflight/mutation/commit в
  datastore. Exact checkpoint возвращается из commit candidate; source effects
  не откатываются из-за последующей SDK restore/ACK ошибки. Post-flush edits
  сравниваются с correct local boundary, а не с результатом нового root lookup.
- Sparse SDK property handles сохраняют смысл между revisions независимо от
  source dictionary IDs. Новые/derived/неоднозначно mapped targets отклоняются
  до source mutation. Runtime graph values не расширяют StorageValue codec.
