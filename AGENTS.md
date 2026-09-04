# AGENTS.md

This file provides guidance and essential context for AI assistants when working with Ferros-Grynn codebase.

## Project Overview

Ferros-Grynn is a **metagraph** engine with embedded data storage its own GQL query language, and a system for interacting with other databases.
The graph model is universal: it aims to represent metagraphs, hypergraphs, multigraphs, trees, DAGs and plain graphs uniformly (including their oriented, undirected, weighted, probabilistic, and attributed versions), supporting every kind of incidence (vertex–vertex, vertex–edge, edge–edge), graphs nested inside atoms (nodes/edges), and cross-level incidence between nested graphs. 
It ships its own storage layer (Direct I/O with an optional Async I/O wrapper) and a query language (GQL) that blends Cypher, Gremlin and SurrealQL — SQL-like queries, graphs-as-tables, path matching, and imperative traverser-based pipelines.

Most design docs, code comments, and `docs/implementation_notes.md` are written in **Russian** — preserve that language when editing them.


## Project Structure

```text
src/
  asynchronous/  # Reusable async/concurrency components; disconnected until audited
  collections/   # Reusable collections and data structures
  datastore/     # Datastore composition, catalogs, MVCC and transactions
  ecs/           # AtomId-keyed runtime sidecar components and property columns
  gql/           # Semantic analysis, logical IR and query execution
  gql_status/    # Project-wide statuses, diagnostics, outcomes and errors
  graph/         # Public runtime graph families, views, paths and algorithms
  math/          # Project-wide reusable mathematical operations
  memory/        # Allocator/tracking, heap accounting and resource policy
  storage/       # Local pages, indexes, WAL, recovery and Direct I/O
  syn/           # GrynnQL lexer, tokens, syntax tree and parser
  unsafed/       # Reusable unsafe primitives; disconnected until audited
  utils/         # Small project-wide utilities, grouped by subject as they grow
  values/        # Runtime values, storage representation and columnar adapters
  sys.rs         # System environment: process, CPU, memory and host observations
docs/                         # Specifications, plans and reports
ferros-grynn-bench/           # Ferros-Grynn benchmarks
crates/ferros-grynn-macros/   # Production procedural macros
crates/fg-storage-engine-prototypes/  # Test-only storage experiments and probes
crates/{ferntree,storekey,surrealmx,surrealdb}/  # Read-only reference trees
tests/                        # Integration tests
```

New project-wide modules are allowed when a reusable responsibility has more
than one real consumer. For example, shared declarative macros may eventually
belong in `src/macros`; do not create an empty module tree in advance.

## Module guidance

Read the nearest module-specific instructions before editing a large production
area:

- [`src/storage/AGENTS.md`](src/storage/AGENTS.md) — local pages, indexes, WAL,
  checkpoint/recovery and Direct I/O;
- [`src/datastore/AGENTS.md`](src/datastore/AGENTS.md) — catalogs, MVCC,
  transactions and backend composition;
- [`src/graph/AGENTS.md`](src/graph/AGENTS.md) — runtime graph families,
  views, paths and algorithms;
- [`src/values/AGENTS.md`](src/values/AGENTS.md) — runtime values, storage DTOs
  and columnar conversion;
- [`src/syn/AGENTS.md`](src/syn/AGENTS.md) — lexer, tokens, AST and parser;
- [`src/gql/AGENTS.md`](src/gql/AGENTS.md) — analysis, logical IR and execution;
- [`src/gql_status/AGENTS.md`](src/gql_status/AGENTS.md) — statuses,
  diagnostics, errors and wire DTOs;
- [`src/collections/AGENTS.md`](src/collections/AGENTS.md) — internal and
  reusable collections;
- [`src/asynchronous/AGENTS.md`](src/asynchronous/AGENTS.md) — reusable async
  and concurrency components;
- [`src/ecs/AGENTS.md`](src/ecs/AGENTS.md) — AtomId-keyed component and
  materialized-property sidecars;
- [`src/math/AGENTS.md`](src/math/AGENTS.md) — reusable mathematical
  operations;
- [`src/memory/AGENTS.md`](src/memory/AGENTS.md) — allocators, allocation
  tracking and memory accounting;
- [`src/unsafed/AGENTS.md`](src/unsafed/AGENTS.md) — reusable unsafe
  primitives and their proof obligations;
- [`src/utils/AGENTS.md`](src/utils/AGENTS.md) — small global utilities.

`src/sys.rs` is a file module, so its responsibility is recorded here rather
than in a synthetic directory: it observes the process and host environment
(system/cgroup memory, CPU, load and available parallelism). Memory policy and
allocator accounting remain in `src/memory`.


## Repository state — read this first

The repo is mid-migration (branch `mvp`). Verify the exact feature/target slice
you touch rather than assuming every configuration is complete.

- The **root crate `ferros-grynn`** is the production target. Its active topology
  is defined by `src/lib.rs`: collections, values, graph, datastore, storage,
  GQL, statuses, ECS, math, memory, system information and utilities are
  connected. `asynchronous`, `unsafed` and `collections/roaring_graphs` remain
  disconnected; code in those paths is not production evidence until it is
  reviewed, prepared for production and wired into normal checks.
- `crates/fg-meta` is a raw, unverified implementation example. It is not a
  specification or behavioral source of truth and needs the same review as any
  other reference before ideas or code are reused.
- `crates/fg-storage-engine-prototypes` owns test-only BufferPool/transport
  experiments.
- `crates/storekey`, `crates/surrealmx`, `crates/ferntree` and
  `crates/surrealdb` are local study/reference trees, not root dependencies or
  workspace members. Their useful areas are summarized in
  `docs/reports/future_capability_analysis_stage_1_6.md` §§7.3–7.6.
  `crates/ferros-grynn-macros`, `crates/fg-storage-engine-prototypes` and
  `ferros-grynn-bench` are the actual additional workspace members. The
  removed `crates/configuration` must not be treated as an available
  implementation source.

Current `src/`, current tests and the normative specifications control
production behavior. Reference trees may supply hypotheses, algorithms or
test ideas only after their assumptions have been verified against those
contracts.


## Architecture

### Allocator & platform I/O (compile-time selected)
- **Global allocator**: `mimalloc-pprof` on MSVC-compatible targets and
  `tikv-jemallocator` on supported non-MSVC Unix targets, selected in
  `src/memory`.
- **Direct I/O** lives in `src/storage`: platform file access, page I/O, the
  production `HybridReadEngine`, WAL and checkpoint/recovery use one on-disk
  contract. The engine combines an atomic resident arena with validated sync
  Direct reads, sequential scatter/prefetch bypass and optional frame-IoRing
  for known sparse windows; mmap remains a benchmark control.

### Graph core (`src/graph`)
- Nodes and edges are atoms stored in family-specific **struct-of-arrays (SoA)**
  layouts. `Graph`, `Forest`/`Tree`, `Dag`, `Hypergraph`, `Metagraph` and
  `Subgraph` retain separate structural invariants.
- **`AtomId`** is the materialization-local handle. The production layout is
  `gen(23) | kind(1) | slot(40)` per `docs/disk_storage_spec.md` §9.
  Public/snapshot-stable references use `AtomRef`, `GraphRef` and `RecordId`.
- `GraphRead` is the shared read surface for in-memory, disk-bound and hybrid
  views. `traversal` owns BFS/DFS, topology/connectivity, shortest paths,
  spanning structures, cuts and binary-lifting LCA.
- The separate **ECS layer** attaches derived components to atom IDs for
  analytics without changing graph durability or equality.

### Values (`src/values`)
The property/value type system. Read `src/values/AGENTS.md` before changing it.
- `runtime` — the public Rust SDK types, collections, identifiers and
  runtime-only values; graph types are owned by `src/graph` and compatibly
  re-exported through the values surface where required.
- `storage` — internal on-disk DTOs, tags, conversion contexts, and codecs.
- `columnar` — future Arrow/DataFusion batch materialization; runtime remains independent of it.

### MVCC & persistence (`src/datastore`, `src/storage`)

The production datastore owns immutable MVCC roots, FCW/SSI validation,
predicate tracking and RAM/local backend composition. Storage owns the
database-level frame WAL, segment rotation, checkpoint/recovery and physical
pages. Production behavior is governed by current specifications and root
tests. Read
`docs/implementation_notes.md` and the two module `AGENTS.md` files before
touching MVCC, WAL or checkpoint code.

### Status & error system (`src/gql_status`)
This is the project-wide diagnostics/error mechanism — **not** just query errors. The core unit is a `Status` (GQLSTATUS-style: `Condition` + classification). Statuses are **code-generated** by the `define_status_codes!` proc macro (in `crates/ferros-grynn-macros/src/status_codes.rs`) from declarative descriptions with `template`/`params`/`subcondition`/`descriptor`/`hint`. Wrapping chain: `Status` → `DiagnosticRecord` → `ErrorState` → `FerrosGrynnError`. Both error *and* notification statuses exist, and statuses can be chained. When adding a new status, add it to the macro invocation rather than hand-writing a variant.

### Proc macros (`crates/ferros-grynn-macros`)
- `#[derive(Measurable)]` — heap-usage estimation; annotate fields with `#[m(add)]` / `#[m(add_m)]`.
- `#[derive(StructureAccumulator)]` — builds an accumulator for JSON-like `Map<String, AnyValue>`; requires implementing `AccumulatorBuilder`.
- `define_status_codes!` — see the status system above.

### Query language (`src/gql`, `src/syn`)
`src/syn` is the syntax front end (`lexer`, `token`, `parser`, AST and statement
nodes). `src/gql` preserves the `parse -> analyze -> logical IR -> execute`
pipeline and uses a transaction adapter for storage access. Extend it through
vertical slices after reconciling the specifications below.


## Specs (`docs/`) — design authority

These `.md` files are the design authority for sensitive subsystems; when implementing those areas, treat the spec as the contract and keep code in sync.
- `docs/gql_spec.md` — the query language as a whole; influences the entire architecture (kept separate for that reason). **Approve this before large GQL work.**
- `docs/disk_storage_spec.md` — on-disk storage format (page types, `RecordId`/`DiskAtomRef`, WAL, checkpoint/recovery, indexes).
- `docs/gql_tech_spec.md` — technical implementation of the query engine (planner contracts, degradation modes, resource budgets). Explicitly noted as needing rework *after* `gql_spec.md` and `disk_storage_spec.md` are approved — don't treat it as current.
- `docs/gql_grammar.md` — EBNF grammar (not yet filled in).
- `docs/graphs.md` — the graph taxonomy the engine must cover.
- Use the directory `docs/reports/` to save progress reports if you create them. 


## Conventions

- All comments in the code must be in English.
- Formatting is enforced by `.rustfmt.toml`: **hard tabs**, block indentation, crate-granularity imports grouped `StdExternalCrate`, `use_small_heuristics = "Off"`. Always run `cargo +nightly fmt` and `cargo clippy` after edits.
- Current production choices include `smallvec` for inline graph adjacency,
  `thin-vec` for `PropertyMap`, `rapidhash` for selected hash tables,
  `parking_lot` for locks and `rkyv` for the value envelope. Both use of an
  existing crate and a manual implementation require an explicit fit argument;
  route the current decision through `rust-workflow` and follow its selected
  owner profile rather than treating an unused manifest entry as a commitment.
- Disk pages/WAL and canonical `StorageValue` bytes are explicit versioned
  formats. New derive-based serialization must not silently replace them.
- Before reusing code from an unverified module such as `asynchronous`,
  `collections` or `unsafed`, audit its contract, errors, safety, cancellation,
  platform behavior and tests as applicable, then bring the selected slice
  under ordinary production formatting, Clippy and test gates.


## Common Commands

Toolchain: **Rust edition 2024** (needs Rust 1.85+). A **nightly** toolchain is required for `cargo fmt` — `.rustfmt.toml` sets `unstable_features = true`. `.cargo/config.toml` passes `--cfg tokio_unstable` globally and static-CRT link args for the `x86_64-pc-windows-msvc` target.

```
# Build lib
just build

# Full code verification (REQUIRED before commits)
just all-check

# Run all tests
cargo test

# Run a single test by substring
cargo test <name>

# Workspace benchmarks (criterion + divan)
cargo bench -p ferros-grynn-bench

```

The release/bench profiles use `lto = "fat"`, `codegen-units = 1` — full optimized builds are slow; prefer `cargo check`/debug builds while iterating.

## Code Quality Rules

- Use types errors from `src/gql_status` for fallible APIs, don't use `anyhow` - it erases errors; NEWER use `.expect()` or another panics.
- Never use `.unwrap_or_default()` when debugging - it masks errors.
- Follow the Clippy coding rules noted in `Cargo.toml` and their configurations in the `clippy.toml` file.
- Completely cover code (includes private modules) with comments, including Panics, Errors and Safety sections, as required by Clippy.
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
