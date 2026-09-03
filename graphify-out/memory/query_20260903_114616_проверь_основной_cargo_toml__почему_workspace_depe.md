---
type: "query"
date: "2026-09-03T11:46:16.236203+00:00"
question: "Проверь основной Cargo.toml: почему workspace dependencies не находятся?"
contributor: "graphify"
outcome: "dead_end"
source_nodes: ["Dependencies", "surrealmx/CARGO.md"]
---

# Q: Проверь основной Cargo.toml: почему workspace dependencies не находятся?

## Answer

Expanded from original query via vocab: [cargo, workspace, manifest, dependencies, toml, package, metadata]. Traversal returned unrelated reference-tree nodes and did not represent the current root manifest; direct manifest inspection found dependencies misplaced under workspace.dev-dependencies and workspace.target tables instead of workspace.dependencies.

## Outcome

- Signal: dead_end

## Source Nodes

- Dependencies
- surrealmx/CARGO.md