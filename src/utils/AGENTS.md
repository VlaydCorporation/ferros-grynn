# Utils module guidance

Этот файл дополняет корневой `AGENTS.md` для `src/utils`.

## Назначение и границы

`utils` — глобальные небольшие переиспользуемые **safe** utilities, которые не
принадлежат одному предметному модулю. По мере роста они разделяются на
предметные подмодули, а не накапливаются в одном `mod.rs`/`utils.rs`.

- Сначала использовать std и уже имеющийся helper.
- Не переносить сюда domain semantics GQL, graph, storage или datastore ради
  сокращения одного call site.
- Utility получает как минимум два реальных consumer-а либо остаётся у
  предметного владельца.
- Public/fallible границы используют типизированные ошибки `gql_status`; строки
  ошибок допустимы только у маленького crate-private parsing helper до
  адаптации владельцем.
- Unsafe и общая математика принадлежат соответственно `unsafed` и `math`.

## Карта подмодулей

- `mod.rs` — crate-private фасад и сгруппированные реэкспорты;
- `parse_bytes.rs` — `ParseBytes` для целых byte sizes с B/KiB/MiB/GiB
  suffixes и checked conversion.

Будущие helpers группировать по назначению (`bytes`, `paths`, `iter`, и т. п.)
только при реальном росте. Не создавать общий prelude или extension traits с
единственным случайным методом.

## Проверка

Для parser/converter utilities покрывать пустой и malformed input, whitespace,
case, zero, min/max, overflow и conversion failure. Изменение общего helper-а
проверять на всех consumers; удобство не должно скрывать ошибку через default
или panic.
