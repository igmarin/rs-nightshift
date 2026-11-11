# Cross-Community Bridge Nodes: `Error` and `path`

Source graph: `graphify-out/graph.json` (built at commit `93395361`, current HEAD).
Graph: 1,391 nodes · 3,128 links · 83 communities. Link relations are `imports_from`, `references`, `contains`, etc., each tagged EXTRACTED/INFERRED/AMBIGUOUS with a source file:line.

Betweenness centrality (from `GRAPH_REPORT.md`, "Suggested Questions"):
- `Error`: **0.152** — highest in the graph
- `path`: **0.093** — second-highest

Both nodes are *structural bridges*: they sit in one community but carry links into many others, so removing either would disconnect more of the graph than removing most other nodes.

## 1. `Error` — the shared error type

| Field | Value |
|-------|-------|
| Node id | `src_error_mod_error` |
| Label | `Error` |
| Community | #15 "Config and Git Errors" |
| Source | `src/error/mod.rs:39` |
| Type | `ast` (Rust struct/enum declaration) |
| Links | 46 total (35 in · 11 out) |
| Distinct *other* communities bridged | **30** |

`Error` is the project's shared error root. `src/error/mod.rs` also defines its subtypes in the same file, each of which lands in its own community:

| Subtype | Node id | Community | Source |
|---------|---------|-----------|--------|
| `Error` | `src_error_mod_error` | Config and Git Errors | `src/error/mod.rs:39` |
| `ConfigError` | `src_error_config_configerror` | btreemap | `src/error/config.rs:15` |
| `GitError` | `src_error_git_giterror` | Config and Git Errors | `src/error/git.rs:14` |
| `ArtifactError` | `src_error_artifact_artifacterror` | Artifact Errors | `src/error/artifact.rs:14` |
| `ProviderError` | `src_error_provider_providererror` | Slugify and Provider Errors | `src/error/provider.rs:16` |
| `Context` | `src_error_mod_context` | Config and Git Errors | `src/error/mod.rs:58` |
| `RoleGraph` | `src_error_mod_rolegraph` | Config and Git Errors | `src/error/mod.rs:64` |
| `Artifact` | `src_error_mod_artifact` | Config and Git Errors | `src/error/mod.rs:54` |

### How it bridges (sample links)

Every fallible module imports `Error` (or a subtype), creating an `imports_from` link from that module's community into `Error`'s community:

| Link | Relation | Source (file:line) | Other-end community |
|------|----------|--------------------|---------------------|
| `src_adapters_capabilities` → `Error` | `imports_from` | `src/adapters/capabilities.rs:16` | Patch and Search Capabilities |
| `src_adapters_context` → `Error` | `imports_from` | `src/adapters/context.rs:4` | Codegraph and Graphify Probes |
| `src_adapters_artifact_store` → `Error` | `imports_from` | `src/adapters/artifact_store.rs:3` | Filesystem Artifact Store |
| `src_doctor_mod` → `Error` | `imports_from` | `src/doctor/mod.rs:259` | Doctor Test Fakes |
| `Error` → `src_error_provider_providererror` | `references` | `src/error/mod.rs:42` | Slugify and Provider Errors |
| `Error` → `src_error_git_giterror` | `references` | `src/error/mod.rs:46` | Config and Git Errors |
| `Error` → `src_error_config_configerror` | `references` | `src/error/mod.rs:50` | btreemap |
| `Error` → `src_error_artifact_artifacterror` | `references` | `src/error/mod.rs:54` | Artifact Errors |

### Communities touched (count of links into/out of each)

```
10x  Config and Git Errors        (own community + intra-error-module links)
 3x  btreemap
 2x  Patch and Search Capabilities
 2x  Git Patch Application
 2x  Scripted LLM Test Client
 2x  Artifact Errors
 2x  Slugify and Provider Errors
 1x  Doctor Test Fakes
 1x  Filesystem Artifact Store
 1x  Codegraph and Graphify Probes
 1x  Ollama Provider Client
 1x  factory.rs
 1x  Ollama HTTP Mock Helpers
 1x  OpenAI-Compatible Provider
 1x  Filesystem State Store
 1x  Executor JSON Repair
 1x  Orchestrator Error Tests
 1x  Artifact Store Module Split
 1x  QA Status Tests
 1x  artifacts/state.rs
 1x  Model Catalog Client
 1x  Config Parsing and Validation
 1x  Application Wiring
 1x  Default Model and URL Config
 1x  Legacy Pipeline Tests
 1x  PM Role Story Completion
 1x  Artifact and Context Ports
 1x  QA Report and Status
 1x  Tech Lead Role
 1x  Writer Role Artifacts
```

### Interpretation

`Error`'s high betweenness (0.152) comes from being the single error type that *every* failure-handling module depends on. It is not itself "complex" — it's a narrow struct/enum definition — but it occupies a choke point: the error module is at the bottom of the dependency stack and is imported by executor, orchestrator, factory, git, openai, ollama, capabilities, artifacts, doctor, techlead, pm, qa, writer, and more. Any change to `Error` (adding a variant, changing `Display`, altering `From` impls) ripples across all 30 foreign communities.


## 2. `path` — the filesystem-path concept

| Field | Value |
|-------|-------|
| Node id | `path` |
| Label | `path` |
| Community | #3 "path" |
| Source | none — external concept node (`type: external`, `source_file: ""`) |
| Type | `external` (concept/rationale node, not an AST declaration) |
| Links | 21 total (21 in · 0 out) |
| Distinct *other* communities bridged | **17** |

`path` is an external concept node (no Rust declaration; created from documentation/rationale text rather than code). All 21 of its links are inbound `imports_from` — modules import `std::path::Path`/`std::path::PathBuf` or otherwise reference path handling, and the graph coalesces those into the single `path` concept node.

### How it bridges (sample links)

| Link | Relation | Source (file:line) | Other-end community |
|------|----------|--------------------|---------------------|
| `src_pipeline` → `path` | `imports_from` | `src/pipeline.rs:257` | Legacy Pipeline Tests |
| `src_adapters_artifact_store` → `path` | `imports_from` | `src/adapters/artifact_store.rs:5` | Filesystem Artifact Store |
| `src_adapters_capabilities` → `path` | `imports_from` | `src/adapters/capabilities.rs:20` | Patch and Search Capabilities |
| `src_adapters_context` → `path` | `imports_from` | `src/adapters/context.rs:5` | Codegraph and Graphify Probes |
| `src_adapters_git` → `path` | `imports_from` | `src/adapters/git.rs:7` | Git Patch Application |
| `src_adapters_ollama` → `path` | `imports_from` | `src/adapters/ollama.rs:294` | Ollama Provider Client |
| `src_adapters_providers_factory` → `path` | `imports_from` | `src/adapters/providers/factory.rs:176` | factory.rs |
| `src_adapters_providers_ollama` → `path` | `imports_from` | `src/adapters/providers/ollama.rs:102` | Ollama HTTP Mock Helpers |

### Communities touched (count of links into `path`)

```
3x  Patch and Search Capabilities
2x  Git Patch Application
2x  path                       (own community)
1x  Legacy Pipeline Tests
1x  Filesystem Artifact Store
1x  Codegraph and Graphify Probes
1x  Ollama Provider Client
1x  factory.rs
1x  Ollama HTTP Mock Helpers
1x  Test Environment Helpers
1x  Filesystem State Store
1x  Executor JSON Repair
1x  Orchestrator Error Tests
1x  Config Parsing and Validation
1x  Default Model and URL Config
1x  Artifact and Context Ports
```

### Interpretation

`path`'s betweenness (0.093) comes from *every* module that touches the filesystem importing `std::path`. Like `Error`, it's a narrow, low-level dependency, but because path handling is universal across CLI args, artifact stores, git patches, provider URLs, config loading, and context gathering, the `path` concept node collects inbound links from 17 communities. Its links are all inbound (it's a leaf concept that nothing re-exports), so it's a "sink" bridge rather than a two-way hub like `Error`.


## Side-by-side comparison

| | `Error` | `path` |
|---|---------|--------|
| Node id | `src_error_mod_error` | `path` |
| Community | #15 Config and Git Errors | #3 path |
| Source | `src/error/mod.rs:39` (ast) | concept node, no source (external) |
| Total links | 46 (35 in · 11 out) | 21 (21 in · 0 out) |
| Other communities bridged | **30** | **17** |
| Betweenness centrality | **0.152** (highest) | **0.093** (2nd) |
| Direction | two-way (references subtypes + imported by modules) | pure sink (all inbound) |
| Kinds of dependents | error handling everywhere | filesystem/path handling everywhere |

### Key takeaway

Both bridges derive their centrality from being *low-level, universal dependencies* rather than from business-logic importance:

- `Error` is the **widening** bridge — it both references its own subtypes and is imported by 30 communities, so it couples error semantics across the whole system.
- `path` is the **narrowing** bridge — it's a pure concept sink collecting `std::path` imports from 17 communities; nothing depends on `path` as a node, so it doesn't couple behavior, just a shared type.

### Suggested follow-up

- Trace the exact import edge `Error` → `executor.rs` (Executor JSON Repair) to confirm whether `execute()` really constructs `Error` variants directly or only references them.
- Compare `Error` against the next bridge after `path` to confirm the drop-off in betweenness (0.152 → 0.093 → ?).
- Check whether any of the 30 communities `Error` touches could depend on a narrower error interface instead of the full `Error` type, to reduce coupling.

Generated by `/graphify query` traversal of `graphify-out/graph.json`; link/sample details verified against the graph JSON directly.

