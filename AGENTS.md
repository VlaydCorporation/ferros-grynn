# AGENTS.md

This file provides guidance and essential context for AI assistants when working with Ferros-Grynn codebase.

## Project Overview

Ferros-Grynn is a **metagraph** engine with embedded data storage its own GQL query language, and a system for interacting with other databases.
The graph model is universal: it aims to represent metagraphs, hypergraphs, multigraphs, trees, DAGs and plain graphs uniformly (including their oriented, undirected, weighted, probabilistic, and attributed versions), supporting every kind of incidence (vertex–vertex, vertex–edge, edge–edge), graphs nested inside atoms (nodes/edges), and cross-level incidence between nested graphs. 
It ships its own storage layer (Direct I/O with an optional Async I/O wrapper) and a query language (GQL) that blends Cypher, Gremlin and SurrealQL — SQL-like queries, graphs-as-tables, path matching, and imperative traverser-based pipelines.

Most design docs, code comments, and `docs/implementation_notes.md` are written in **Russian** — preserve that language when editing them.


## Project Structure
```
src/                          # Main Rust SDK source files
docs/                         # Document and specification files
ferros-grynn-bench/           # Ferros-Grynn benchmarks
crates/ferros-grynn-macros/   # Procedural macros
tests/                        # Integration tests
```


## Repository state — read this first

The repo is mid-migration (branch `mvp`). Do not assume the main crate builds end-to-end.

- **`crates/fg-meta`** (`ferros-grynn-meta`, a *separate* Cargo workspace with its own `Cargo.lock`) is the **mature reference implementation** / prototype. It contains the working graph core, traversal algorithms, MVCC, persistence, and I/O subsystem. Consult it to understand how a subsystem is intended to work before (re)implementing it in the main crate. It is being either merged into the main crate or promoted to a standalone core crate.
- The **root crate `ferros-grynn`** is where the production version is being rebuilt. In `src/lib.rs` **most top-level modules are commented out**; currently only `values` compiles. When you add code, expect to un-comment and fix modules incrementally — don't assume a module referenced in a `use` is actually wired in.
- `crates/storage_engine` and `crates/configuration` are new scaffolding crates for the migration (not yet in the workspace `members`, may not build yet). `crates/ferros-grynn-macros` and `ferros-grynn-bench` are the actual workspace members.

When a subsystem exists in both `fg-meta` and `src/`, the `src/` version is the target and `fg-meta` is the source of truth for behavior.


## Architecture

### Allocator & platform I/O (compile-time selected)
- **Global allocator**: `mimalloc` on MSVC targets, `tikv-jemallocator` elsewhere (set in `src/lib.rs`).
- **Direct I/O** is the storage foundation: `O_DIRECT`/`F_NOCACHE` on Unix, `FILE_FLAG_NO_BUFFERING` on Windows. Async I/O is a wrapper over `io_uring`/`tokio-uring` on Linux and a custom Tokio <-> Windows `IoRing` bridge on Windows. In `fg-meta` the only public I/O types are `SyncIo` and `AsyncIo`; page size and `BufferPool` are shared. The new home for this is `crates/storage_engine` (`io`, `layout`, `store` submodules).

### Graph core (`fg-meta`, target: `src/values/runtime/graph`)
- Data model `MG = ⟨V, MV, E, ME⟩` (Basu–Blanning + Chernenko–Gapanyuk extensions). Stored **struct-of-arrays (SoA)**.
- **`AtomId`** is the central handle. The production layout is `gen(23) | kind(1) | slot(40)` per `docs/disk_storage_spec.md` §9; `fg-meta` still uses the legacy `gen(32) | kind(1) | slot(31)` prototype layout. Slots are handed out by a `SlotAllocator`. Nodes and edges are both "atoms". `MetaGraph` is the container; `traversal` holds the algorithms (BFS/DFS, topo sort, SCC/WCC, Dijkstra, Bellman–Ford with negative-cycle detection, MST, cuts, binary-lifting LCA); `subgraph` extracts typed subgraphs (`TreeSubGraph`, `ForestSubGraph`, `DagSubGraph`, `HyperSubGraph`) plus generic `SubGraph`; `matrix` provides `AdjacencyMatrix`, `IncidenceMatrix`, `MetaAdjacencyMatrix`, `IncidenceTensor` (via `nalgebra`).
- An **ECS layer** (`SparseSet`, `AnyPool`, component registries, `join!`-style iterators) attaches components (positions, PageRank, feature vectors, weights…) to atoms for analytics/ML.

### Values (`src/values`, the one module currently active in the main crate)
The property/value type system. Read `src/values/AGENTS.md` before changing it.
- `runtime` — the public Rust SDK types, collections, identifiers, runtime-only values, and graph
  foundation (re-exported as the crate's `graph` module).
- `storage` — internal on-disk DTOs, tags, conversion contexts, and codecs.
- `columnar` — future Arrow/DataFusion batch materialization; runtime remains independent of it.

### MVCC & persistence (`fg-meta`)
Serializable Snapshot Isolation (**SSI**) with O(1) conflict indices (`atom_readers`/`atom_writers`) and predicate locking via an `AtomMeta` label/property cache for exact phantom matching. Persistence adds a **WAL with segment rotation** (`{lsn:016x}.wal`), streaming O(1)-RAM snapshot write, single-pass deferred REDO recovery, and inline compaction at checkpoint (carry-forward of only active-tx entries). The codec is generic: `BytesCodec` (identity) or `ClosureCodec<T>` (`Arc<dyn Fn>`, cloneable, supports capturing closures — needed for crash recovery). `implementation_notes.md` documents the exact algorithms, invariants, and complexity table — read it before touching MVCC/WAL/checkpoint code.

### Status & error system (`src/gql_status`)
This is the project-wide diagnostics/error mechanism — **not** just query errors. The core unit is a `Status` (GQLSTATUS-style: `Condition` + classification). Statuses are **code-generated** by the `define_status_codes!` proc macro (in `crates/ferros-grynn-macros/src/status_codes.rs`) from declarative descriptions with `template`/`params`/`subcondition`/`descriptor`/`hint`. Wrapping chain: `Status` → `DiagnosticRecord` → `ErrorState` → `FerrosGrynnError`. Both error *and* notification statuses exist, and statuses can be chained. When adding a new status, add it to the macro invocation rather than hand-writing a variant.

### Proc macros (`crates/ferros-grynn-macros`)
- `#[derive(Measurable)]` — heap-usage estimation; annotate fields with `#[m(add)]` / `#[m(add_m)]`.
- `#[derive(StructureAccumulator)]` — builds an accumulator for JSON-like `Map<String, AnyValue>`; requires implementing `AccumulatorBuilder`.
- `define_status_codes!` — see the status system above.

### Query language (`src/gql`, `src/syn`)
Early-stage. `src/syn` is the syntax front end (`lexer`, `token`, `parser`, and its own `error` with source-location rendering). `src/gql` holds the AST/expression/dataset/traversal scaffolding. This is designed against the specs below and will be reworked after the specs settle.


## Specs (`docs/`) — design authority

These `.md` files are the design authority for sensitive subsystems; when implementing those areas, treat the spec as the contract and keep code in sync.
- `docs/gql_spec.md` — the query language as a whole; influences the entire architecture (kept separate for that reason). **Approve this before large GQL work.**
- `docs/disk_storage_spec.md` — on-disk storage format (page types, `RecordId`/`DiskAtomRef`, WAL, checkpoint/recovery, indexes).
- `docs/gql_tech_spec.md` — technical implementation of the query engine (planner contracts, degradation modes, resource budgets). Explicitly noted as needing rework *after* `gql_spec.md` and `disk_storage_spec.md` are approved — don't treat it as current.
- `docs/gql_grammar.md` — EBNF grammar (not yet filled in).
- `docs/graphs.md` — the graph taxonomy the engine must cover.


## Conventions

- All comments in the code must be in English.
- Formatting is enforced by `.rustfmt.toml`: **hard tabs**, block indentation, crate-granularity imports grouped `StdExternalCrate`, `use_small_heuristics = "Off"`. Always run `cargo +nightly fmt` and `cargo clippy` after edits.
- `slotmap`/`slot-cell` back atom storage; `smallvec` (alpha 2.0) for inline adjacency; `rapidhash`/`rustc-hash` for hashing; `parking_lot`/`dashmap`/`crossbeam` for concurrency; `rayon` for data parallelism.
- Serialization uses `binrw` (disk layout) and `rkyv`; the graph binary codec lives in `fg-meta`'s `serial` module.


## Common Commands

Toolchain: **Rust edition 2024** (needs Rust 1.85+). A **nightly** toolchain is required for `cargo fmt` — `.rustfmt.toml` sets `unstable_features = true`. `.cargo/config.toml` passes `--cfg tokio_unstable` globally and static-CRT link args for the `x86_64-pc-windows-msvc` target.

```
# Build lib
cargo build

# Full code verification (REQUIRED before commits)
just all-check

# Run all tests
cargo test

# Run a single test by substring
cargo test <name>

# Workspace benchmarks (criterion + divan)
cargo bench -p ferros-grynn-bench

```

Working inside the prototype (its own workspace — cd into it):
```
cd crates/fg-meta
cargo test
cargo test --features async-uring       # enable io_uring / tokio-uring async I/O
cargo bench --bench bench_graph         # graph benchmarks
```
`fg-meta` feature flags: `iouring`, `async-uring` (implies `iouring`), `async-ioring`, `spdk` (all default off; SPDK/async paths are Linux-only).

The release/bench profiles use `lto = "fat"`, `codegen-units = 1` — full optimized builds are slow; prefer `cargo check`/debug builds while iterating.

## Code Quality Rules

- Use types errors from `src/gql_status` for fallible APIs, don't use `anyhow` - it erases errors; NEWER use `.expect()` or another panics.
- Never use `.unwrap_or_default()` when debugging - it masks errors.
- Follow the Clippy coding rules noted in `Cargo.toml` and their configurations in the `clippy.toml` file.
- Performance matters: think about impact of every change.
- Don't add dependencies without confirmation.

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

When the user types `/graphify`, use the installed graphify skill or instructions before doing anything else.

Rules:
- For codebase questions, first run `graphify query "<question>"` when graphify-out/graph.json exists. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than GRAPH_REPORT.md or raw grep output.
- Dirty graphify-out/ files are expected after hooks or incremental updates; dirty graph files are not a reason to skip graphify. Only skip graphify if the task is about stale or incorrect graph output, or the user explicitly says not to use it.
- If graphify-out/wiki/index.md exists, use it for broad navigation instead of raw source browsing.
- Read graphify-out/GRAPH_REPORT.md only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).
