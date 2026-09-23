---
type: "query"
date: "2026-09-12T00:23:43.957213+00:00"
question: "Добавить точный layout label entry page и typed node/edge label key в combined statistics"
contributor: "graphify"
outcome: "useful"
source_nodes: ["encode_label_dictionary()", "decode_label_dictionary()", ".statistics()"]
---

# Q: Добавить точный layout label entry page и typed node/edge label key в combined statistics

## Answer

Expanded via graph vocab: label statistics dictionary entry combined edge node graph page kind. LabelDictEntryPage получил exact offsets и независимые dense Node/Edge ID chains. label_stats.fgb сохраняется единым файлом, но arithmetic combined_label_id заменён ключом kind plus label_id; DegreeHistogram использует ту же label identity.

## Outcome

- Signal: useful

## Source Nodes

- encode_label_dictionary()
- decode_label_dictionary()
- .statistics()