# src/application: the use cases

The application layer runs the role graph. It depends only on
`crate::domain` and `crate::ports` (ADR-007) and never imports an adapter:
`main.rs` builds the adapters and passes them in. It does no I/O, with one
exception: the executor reads a role's `context_files` from the repo
(`read_context_files`, with path and size checks).

| Module | Use case |
| :--- | :--- |
| `executor.rs` | runs one role. Before the model call, the role's tools (`gather-context`, `run-tests`) add real results to the prompt. The call goes through the `ModelClient` port, the reply is parsed into the verdict envelope, the content is written through `ArtifactStore`, and after a `continue` or `done` verdict the edit tools (`apply-patch`, `write-file`, `search-replace`) run. A rejected reply or a failed edit tool is retried (`retries`) |
| `orchestrator.rs` | walks the role graph from `run.start`. Routes on each verdict, enforces `max_steps` and each role's `max_loop`, writes action-log events and the status snapshot through `StateStore` and `Clock`, and stops at a terminal state |
| `report.rs` | renders the report from the snapshot and the action log: status (`PASSED`, `FAILED`, `REQUIRES_HUMAN_REVIEW`), steps, retries, block reason, the role trail and the time per role |

`executor::execute` and `orchestrator::run_graph` are generic over the port
traits, so tests pass in the in-memory doubles from `ports.rs` and run with
no network or filesystem.

The executor also owns the contract with the model: `OUTPUT_CONTRACT` (and,
for `search-replace` roles, the edit format) is appended to each role's
system prompt and asks for one JSON envelope. The parser accepts markdown
fences, prose around the object, `<think>` blocks from thinking models, and
common JSON mistakes. A reply that the model did not finish never reaches
the parser: the provider adapter rejects it, and the executor retries.
