---
type: "query"
date: "2026-09-08T12:44:56.877663+00:00"
question: "Почему superblock.fgb, раздельные freelist-файлы и std/uniq имена были заменены?"
contributor: "graphify"
outcome: "corrected"
correction: "Восстановить приоритет спецификации: superblock.fgb и per-file freelists; для scalar filenames отдельно утвердить безопасную схему именования, не менять нормативный контракт молча."
source_nodes: ["freelist.rs", "superblock.rs", "ScalarIndexEntry"]
---

# Q: Почему superblock.fgb, раздельные freelist-файлы и std/uniq имена были заменены?

## Answer

Проверка текущего кода и спецификации: graph.fgb является только необоснованным переименованием GraphSuperblock; единый freelist.fgb сейчас лишь пустой placeholder и его формат без DataFileId не способен заменить per-file freelists; scalar_<ordinal>.fgidx не объединяет индексы физически, но без утвержденного решения заменяет самодокументируемые std/uniq имена непрозрачным ordinal. Этап 7 ошибочно подогнал нормативную таблицу под минимальный backend вместо регистрации расхождения.

## Outcome

- Signal: corrected
- Correction: Восстановить приоритет спецификации: superblock.fgb и per-file freelists; для scalar filenames отдельно утвердить безопасную схему именования, не менять нормативный контракт молча.

## Source Nodes

- freelist.rs
- superblock.rs
- ScalarIndexEntry