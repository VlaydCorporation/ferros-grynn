# Changefeed runtime guidance

Public SDK config, cursor и owned historical images принадлежат этому модулю.
Capture/replay/MVCC принадлежат datastore; .fcf/WAL/codecs принадлежат storage.
Не экспортировать pages, WAL LSN, GlobalAtomId, local AtomId или storage DTO.
Historical properties имеют owned names; topology — qualified logical references
с containment route. Runtime Number Eq не заменяет canonical representation equality.

Нормативный контракт: docs/gql_changefeed_stage_27_plan.md, CF-01–CF-17.
Cursor использует persisted database UUID + source + mode + whole-commit stamp;
QueryCursor, session id и process-local transaction id для resume не подходят.
Все errors проходят через gql_status; checked input limits применяются до allocation.
Reader заимствует transaction, удерживает один snapshot и выдаёт целые batches/GAP.
FULL coverage проверяется до первого элемента. Timestamp cursor остаётся None до
доказанного inclusive boundary; owned results доступны после завершения reader.

`binding()` показывает committed capture policy/revision и фиксированные bounds,
не private configuration и не обещание historical FULL coverage. `statistics()`
принадлежит одному reader и включает preflight; page_reads — logical requests,
scratch — admitted bytes, не disk misses/RSS. Process diagnostics сбрасываются
при open; они не являются durability acknowledgement.
