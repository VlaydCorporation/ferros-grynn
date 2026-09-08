---
type: "query"
date: "2026-09-07T14:46:56.393949+00:00"
question: "Почему graph_stats.fgb, label_stats.fgb и prop_stats.fgb объединены в statistics.fgb?"
contributor: "graphify"
outcome: "useful"
source_nodes: ["statistics_label_page_range()", ".statistics()"]
---

# Q: Почему graph_stats.fgb, label_stats.fgb и prop_stats.fgb объединены в statistics.fgb?

## Answer

Expanded via graph vocabulary: statistics, stats, histogram, label, property, file, page, layout, codec, graph. The unified file was selected for one versioned checksummed image, one DataFileKind and simpler atomic validation, not because a larger file reduces page reads. Direct I/O reads individual pages. The current open path eagerly decodes every statistics range, so selective workloads can pay for unused property histograms. The original three-file contract should not have been replaced without explicit approval and measurement; separate graph, label and property files plus lazy loading are a viable correction, with a shared revision for atomic publication.

## Outcome

- Signal: useful

## Source Nodes

- statistics_label_page_range()
- .statistics()