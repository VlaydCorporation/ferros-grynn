# AGENTS.md

This file provides guidance and essential context for AI assistants when working with Ferros-Grynn codebase.

## Project Overview

Ferros-Grynn is a **metagraph** engine with embedded data storage its own GQL query language, and a system for interacting with other databases.
The graph model is universal: it aims to represent metagraphs, hypergraphs, multigraphs, trees, DAGs and plain graphs uniformly (including their oriented, undirected, weighted, probabilistic, and attributed versions), supporting every kind of incidence (vertex–vertex, vertex–edge, edge–edge), graphs nested inside atoms (nodes/edges), and cross-level incidence between nested graphs. 
It ships its own storage layer (Direct I/O with an optional Async I/O wrapper) and a query language (GQL) that blends Cypher, Gremlin and SurrealQL — SQL-like queries, graphs-as-tables, path matching, and imperative traverser-based pipelines.

Most design docs, code comments, and `docs/disk_storage_spec.md` are written in **Russian** — preserve that language when editing them.


## Project Structure

```text
src/                         # Public SDK facade and disconnected legacy sources
crates/fg-memory/src/         # Allocator, heap estimates and allocation accounting
crates/fg-syntax/src/syn/     # Lexer, tokens, AST, parser and parser configuration
crates/fg-runtime/src/        # Runtime values, graph families, ECS, math and CowVec
crates/fg-value-codec/src/values/storage/  # Internal DTOs and canonical codecs
crates/fg-columnar/src/values/columnar/   # PropertyBatch, Arrow and optional DataFusion
crates/fg-engine/src/         # Datastore, storage, GQL, stats, LIVE, changefeed, bridges
crates/fg-gql-status/src/     # Project-wide statuses and diagnostics
crates/fg-test-support/       # Existing assertions for tests and benchmark setup
crates/ferros-grynn-macros/   # Production procedural macros
crates/fg-storage-engine-prototypes/  # Test-only storage experiments and probes
crates/{ferntree,storekey,surrealmx,surrealdb}/  # Read-only reference trees
ferros-grynn-bench/          # Benchmarks consuming the public facade
docs/                        # Specifications, plans and reports
tests/                       # Facade integration tests
```

New project-wide modules are allowed when a reusable responsibility has more
than one real consumer. For example, shared declarative macros may eventually
belong in `src/macros`; do not create an empty module tree in advance.

## Module guidance

Read the nearest module-specific instructions before editing a large production
area:

- [`crates/fg-engine/src/storage/AGENTS.md`](crates/fg-engine/src/storage/AGENTS.md) — local pages, indexes, WAL,
  checkpoint/recovery and Direct I/O;
- [`crates/fg-engine/src/stats/AGENTS.md`](crates/fg-engine/src/stats/AGENTS.md) — transactional counters and
  bulk-built planner histograms;
- [`crates/fg-engine/src/datastore/AGENTS.md`](crates/fg-engine/src/datastore/AGENTS.md) — catalogs, MVCC,
  transactions and backend composition;
- [`crates/fg-runtime/src/graph/AGENTS.md`](crates/fg-runtime/src/graph/AGENTS.md) — runtime graph families,
  views, paths and algorithms;
- [`src/values/AGENTS.md`](src/values/AGENTS.md) — runtime values, storage DTOs
  and columnar conversion;
- [`crates/fg-syntax/src/syn/AGENTS.md`](crates/fg-syntax/src/syn/AGENTS.md) — lexer, tokens, AST and parser;
- [`crates/fg-engine/src/gql/AGENTS.md`](crates/fg-engine/src/gql/AGENTS.md) — analysis, logical IR and execution;
- [`crates/fg-gql-status/src/AGENTS.md`](crates/fg-gql-status/src/AGENTS.md) — statuses,
  diagnostics, errors and wire DTOs;
- [`src/collections/AGENTS.md`](src/collections/AGENTS.md) — internal and
  reusable collections;
- [`src/asynchronous/AGENTS.md`](src/asynchronous/AGENTS.md) — reusable async
  and concurrency components;
- [`crates/fg-runtime/src/ecs/AGENTS.md`](crates/fg-runtime/src/ecs/AGENTS.md) — AtomId-keyed component and
  materialized-property sidecars;
- [`crates/fg-runtime/src/math/AGENTS.md`](crates/fg-runtime/src/math/AGENTS.md) — reusable mathematical
  operations;
- [`crates/fg-memory/src/AGENTS.md`](crates/fg-memory/src/AGENTS.md) — allocators, allocation
  tracking and memory accounting;
- [`src/unsafed/AGENTS.md`](src/unsafed/AGENTS.md) — reusable unsafe
  primitives and their proof obligations;
- [`src/utils/AGENTS.md`](src/utils/AGENTS.md) — small global utilities.

`src/sys.rs` is a file module, so its responsibility is recorded here rather
than in a synthetic directory: it observes the process and host environment
(system/cgroup memory, CPU, load and available parallelism). Memory policy and
allocator accounting remain in `crates/fg-memory/src`.


## Repository state — read this first

The repo is mid-migration (branch `mvp`). Verify the exact feature/target slice
you touch rather than assuming every configuration is complete.

- The **root crate `ferros-grynn`** is the public SDK facade. `src/lib.rs`
  re-exports the original concrete types from their production owners above.
  Engine internals and storage DTOs remain outside the SDK. Internal owners
  never depend on the facade, including in tests; benchmarks and prototypes
  continue to consume it. Technical `__internal` namespaces are not SDK exports.
  `src/asynchronous`, `src/unsafed`, `src/sys.rs`, `src/utils` and the remaining
  legacy collections stay disconnected; production `CowVec` belongs to runtime.
  Disconnected code needs review and normal gates before it can be wired in.
- `crates/fg-meta` is a raw, unverified implementation example. It is not a
  specification or behavioral source of truth and needs the same review as any
  other reference before ideas or code are reused.
- `crates/fg-storage-engine-prototypes` owns test-only BufferPool/transport
  experiments.
- `crates/storekey`, `crates/surrealmx`, `crates/ferntree` and
  `crates/surrealdb` are local study/reference trees, not root dependencies or
  workspace members. Their useful areas are summarized in
  `docs/reports/future_capability_analysis_stage_1_6.md` §§7.3–7.6.
  The workspace members are the facade, the production owners listed above,
  `fg-test-support`, macros, storage prototypes and benchmarks. The
  removed `crates/configuration` must not be treated as an available
  implementation source.

Current facade and production-owner sources, current tests and the normative specifications control
production behavior. Reference trees may supply hypotheses, algorithms or
test ideas only after their assumptions have been verified against those
contracts.


## Architecture

### Allocator & platform I/O (compile-time selected)
- **Global allocator**: `mimalloc-pprof` on MSVC-compatible targets and
  `tikv-jemallocator` on supported non-MSVC Unix targets, selected in
  `crates/fg-memory/src`.
- **Direct I/O** lives in `crates/fg-engine/src/storage`: platform file access, page I/O, the
  production `HybridReadEngine`, WAL and checkpoint/recovery use one on-disk
  contract. The engine combines an atomic resident arena with validated sync
  Direct reads, sequential scatter/prefetch bypass and optional frame-IoRing
  for known sparse windows; mmap remains a benchmark control.

### Graph core (`crates/fg-runtime/src/graph`)
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
- `runtime` in `fg-runtime` — the public Rust SDK types, collections, identifiers and
  runtime-only values; graph types are owned by `crates/fg-runtime/src/graph` and compatibly
  re-exported through the values surface where required.
- `storage` in `fg-value-codec` — internal DTOs, tags, conversion contexts and canonical codecs.
- `columnar` in `fg-columnar` — PropertyBatch and Arrow/DataFusion materialization; runtime remains independent of it.

### MVCC & persistence (`crates/fg-engine/src/datastore`, `crates/fg-engine/src/storage`)

The production datastore owns immutable MVCC roots, FCW/SSI validation,
predicate tracking and RAM/local backend composition. Storage owns the
database-level frame WAL, segment rotation, checkpoint/recovery and physical
pages. Production behavior is governed by current specifications and root
tests. Read
`docs/disk_storage_spec.md` and the two module `AGENTS.md` files before
touching MVCC, WAL or checkpoint code.

### Status & error system (`crates/fg-gql-status/src`)
This is the project-wide diagnostics/error mechanism — **not** just query errors. The core unit is a `Status` (GQLSTATUS-style: `Condition` + classification). Statuses are **code-generated** by the `define_status_codes!` proc macro (in `crates/ferros-grynn-macros/src/status_codes.rs`) from declarative descriptions with `template`/`params`/`subcondition`/`descriptor`/`hint`. Wrapping chain: `Status` → `DiagnosticRecord` → `ErrorState` → `FerrosGrynnError`. Both error *and* notification statuses exist, and statuses can be chained. When adding a new status, add it to the macro invocation rather than hand-writing a variant.

### Proc macros (`crates/ferros-grynn-macros`)
- `#[derive(Measurable)]` — heap-usage estimation; annotate fields with `#[m(add)]` / `#[m(add_m)]`.
- `#[derive(StructureAccumulator)]` — builds an accumulator for JSON-like `Map<String, AnyValue>`; requires implementing `AccumulatorBuilder`.
- `define_status_codes!` — see the status system above.

### Query language (`crates/fg-engine/src/gql`, `crates/fg-syntax/src/syn`)
`crates/fg-syntax/src/syn` is the syntax front end (`lexer`, `token`, `parser`, AST and statement
nodes). `crates/fg-engine/src/gql` preserves the `parse -> analyze -> logical IR -> execute`
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

Toolchain: builds, tests and coverage use **stable Rust** (CI pins Rust 1.98.1), with **edition 2024**. A **nightly** toolchain is required only for `cargo fmt` — `.rustfmt.toml` sets `unstable_features = true`. `.cargo/config.toml` passes `--cfg tokio_unstable` globally and static-CRT link args for the `x86_64-pc-windows-msvc` target. Do not use nightly to compile the production crate.

```
# Build lib
just build

# Full code verification (REQUIRED before commits)
just all-check

# Run the complete workspace suite, including owner unit tests and doctests
just test

# Run a focused engine test
cargo test --locked -p fg-engine --no-default-features <name> -- --test-threads=1

# Workspace benchmarks (criterion + divan)
cargo bench -p ferros-grynn-bench

```

`cargo test` and `cargo test -p ferros-grynn` select only the facade and its
integration tests. The default workspace package remains `ferros-grynn`.
Core Clippy and coverage explicitly select the moved production owners.

The release/bench profiles use `lto = "fat"`, `codegen-units = 1` — full optimized builds are slow; prefer `cargo check`/debug builds while iterating.

## Code Quality Rules

- Use types errors from `crates/fg-gql-status/src` for fallible APIs, don't use `anyhow` - it erases errors; NEWER use `.expect()` or another panics.
- Never use `.unwrap_or_default()` when debugging - it masks errors.
- Follow the Clippy coding rules noted in `Cargo.toml` and their configurations in the `clippy.toml` file.
- Completely cover code (includes private modules) with comments, including Panics, Errors and Safety sections, as required by Clippy.
- Performance matters: think about impact of every change.
- Don't add dependencies without confirmation.

## Eternal limits, not a checklist

If the two principles conflict, choose the one that reduces the future cost of changes in this particular codebase more.

THE HARD RULE is to refactor the code according to the principle first, and only then change the behavior.

1. Separation of Concerns — one category of work per part of the system: UI / domain / persistence / infra. The basic principle.
2. Encapsulation / Information Hiding is a small stable contract, the internals are hidden.
3. High Cohesion + Loose Coupling — what changes together must live together; independent parts communicate through narrow interfaces.
4. DRY — one authoritative representation for each knowledge, not for each similar line. Don't overdo it with DRY.
5. KISS is the simplest architecture that works. Complexity is a long—term tax.
6. Single Responsibility is one reason to change.
7. Depend on Abstractions — policy does not depend on details; both depend on contracts.
8. YAGNI — no speculative features, frameworks and hooks "for later".
9. Composition over Inheritance — assemble a system from components, rather than grow fragile hierarchies.
10. Open/Closed, but with discipline — expand the system across stable boundaries and only where the same change has already been needed twice.

Also useful: Law of Demeter · fail fast / illegal states unrepresentable · optimize for deletion · Unix-the principle of "do one thing well" + composition.

Treat it as a limitation. If common sense requires you to break the slogan, break it.

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

When the user types `/graphify`, use the installed graphify skill or instructions before doing anything else.

Rules:
- For codebase questions, first run `graphify query "<question>"` when graphify-out/graph.json exists. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than GRAPH_REPORT.md or raw grep output.
- Dirty graphify-out/ files are expected after hooks or incremental updates; dirty graph files are not a reason to skip graphify. Only skip graphify if the task is about stale or incorrect graph output, or the user explicitly says not to use it.
- If graphify-out/wiki/index.md exists, use it for broad navigation instead of raw source browsing.
- Read graphify-out/GRAPH_REPORT.md only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).
