# Contributing To Anemoi

Anemoi is intentionally narrow at the core. Contributions should strengthen the
local inference governance loop rather than broaden the project into a provider
gateway, runtime, agent framework, or application platform.

## Current Repository State

As of 2026-06-09, Anemoi's active product surface is the Rust workspace under
`crates/anemoi-*`. The older parallel service surface has been removed; do not
reintroduce a second product stack without an explicit architecture issue.

## Ground Rules

- Preserve the boundary: Anemoi decides; runtimes execute.
- Keep runtime-specific protocol details out of `anemoi-core`.
- Keep policy scoring in `anemoi-policy`.
- Keep adapters in `anemoi-runtime`.
- Keep telemetry persistence in `anemoi-telemetry`.
- Keep API and CLI surfaces thin.
- Do not introduce provider-gateway behavior before residency governance works.
- Do not add cloud execution as a default path.
- Do not commit secrets, private prompts, runtime tokens, or host-only paths.
- Use repository evidence over assumptions.
- Mark unresolved runtime, migration, or policy questions as `Needs validation`.

## Development Priorities

Work should serve the first complete governance loop:

```text
load config
inspect runtimes
normalize residency
receive request
generate candidates
score candidates
choose reuse / load / deny / stage
record decision
explain decision
```

When choosing between feature breadth and policy clarity, prefer policy clarity.

## Architecture Boundaries

| Crate | Responsibility |
|---|---|
| `anemoi-core` | Domain types, config, residency states, decisions, explanations. |
| `anemoi-runtime` | Runtime adapter trait, mock adapter, and runtime inspection adapters. |
| `anemoi-policy` | Deterministic scheduler, scoring, and continuity fallback behavior. |
| `anemoi-telemetry` | Recent in-memory decisions and optional append-only JSONL logging. |
| `anemoi-daemon` | Axum local control-plane API. |
| `anemoi-cli` | Operator commands such as `status`, `decide`, `explain`, and `residents`. |
| `anemoi-mcp` | Minimum MCP adapter over existing core, daemon, and telemetry services. |

Rules:

- Domain crates should not perform network I/O.
- Runtime adapters should not make policy decisions.
- Phase one has no required database. SQLite and database-backed analytics are
  future work.
- CLI commands should orchestrate the daemon/core services rather than
  reimplementing scheduler logic.
- API handlers should expose decisions, not hide them behind opaque routing.
- MCP handlers should adapt to existing services and must not duplicate
  scheduler, runtime, or telemetry logic.

## Testing Policy

Policy tests should come before runtime integration tests.

Use `docs/test_roadmap.md` as the prompt-aligned gate list. Later scaffolding
can exist, but a prompt is not accepted until its named tests and earlier
invariants pass.

Required early tests:

- `avoids_cold_large_model_when_small_worker_is_hot`
- config parsing succeeds for `config/anemoi.example.yaml`
- unknown domains are rejected deterministically
- unavailable runtimes produce rejected options
- hot residents score higher than cold candidates under interactive budgets
- memory pressure affects scoring
- explanations include reasons for the selected path
- decision logs default to memory and can optionally append JSONL

Use single-purpose tests by default. End-to-end tests are useful only when they
prove the whole governance loop.

## Validation

Before handoff for Rust changes, run:

```powershell
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

If the task touches only documentation, run the strongest practical text checks
and inspect the changed Markdown.

If Rust tooling is unavailable, report it explicitly.

## Commit Guidelines

Use Conventional Commits:

```text
type(scope): subject
```

Allowed types:

- `feat`
- `fix`
- `refactor`
- `perf`
- `test`
- `docs`
- `style`
- `chore`
- `ci`

Suggested scopes:

- `core`
- `runtime`
- `policy`
- `telemetry`
- `daemon`
- `cli`
- `config`
- `docs`
- `repo`

Examples:

```text
feat(policy): stage large model behind hot worker
fix(runtime): normalize ollama resident model names
docs(repo): clarify repository surface
```

## Pull Requests

PRs should include:

- the behavior changed
- the policy or runtime boundary affected
- tests added or updated
- validation run
- known limitations or `Needs validation` items

Use `Closes #123` only when the PR fully satisfies the issue. Use `References
#123` for partial or related work.

## Stop Conditions

Stop and ask for direction when:

- a task would turn Anemoi into an inference runtime
- a task would introduce provider-gateway behavior before core policy works
- a change would reintroduce a second product stack without explicit scope
- runtime behavior is ambiguous and would change product semantics
- validation fails outside the task scope
- live runtime changes would be required but were not explicitly requested

## Security

- No secrets in Git.
- Keep local-first behavior as the default.
- Bind local services to loopback unless exposure is explicitly approved.
- Treat cloud execution as future, policy-controlled, and opt-in.
- Avoid logging raw prompts or transcripts by default.
