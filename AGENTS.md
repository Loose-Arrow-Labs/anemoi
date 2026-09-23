# Anemoi Execution Rules

---

## 1. What Anemoi Is

Anemoi is a local-first inference governance layer for heterogeneous AI systems.

Anemoi provides:

- runtime selection
- residency governance
- continuity preservation
- execution economics
- deterministic policy evaluation
- structured decision explanations
- telemetry for scheduling decisions

Anemoi decides:

```text
What should execute?
Where should it execute?
Should execution happen now?
What resources should remain resident?
What is the cheapest acceptable path?
Why was that decision made?
```

---

## 2. What Anemoi Is Not

Anemoi is not:

- an inference runtime
- a model host
- a LiteLLM clone
- an OpenRouter clone
- a provider gateway in core v1
- an agent framework
- a memory system
- a RAG platform
- a vector database
- a training system
- a tool orchestrator

Runtimes execute. Anemoi decides.

---

## 3. Current Repository State

Repository evidence as of 2026-06-09:

- The active Rust workspace is defined in `Cargo.toml`.
- Rust crates live under `crates/anemoi-*`.
- The example Anemoi policy config is `config/anemoi.example.yaml`.
- The repository is Rust-only; the former managed-code surface was removed as
  explicit cleanup under issue #129.
- Do not reintroduce a parallel non-Rust product surface as incidental work.

---

## 4. Locked Product Boundary

Anemoi owns:

- request-to-domain-to-roster-to-residency-group scheduling
- model residency state normalization
- runtime inspection through adapters
- policy scoring
- continuity fallback
- background staging decisions
- structured explanations
- decision telemetry

Anemoi does not own:

- model execution internals
- model weights
- prompt planning
- agent memory
- retrieval
- training
- provider account management
- live infrastructure mutation

---

## 5. Target Rust Crate Boundaries

```text
anemoi-core        # domain types, config, residency states, decisions
anemoi-runtime     # runtime adapter trait and runtime inspection adapters
anemoi-policy      # deterministic scheduling, scoring, continuity behavior
anemoi-telemetry   # decision logs and runtime/event telemetry
anemoi-daemon      # axum local control-plane API
anemoi-cli         # operator commands
anemoi-mcp         # minimum MCP control-plane adapter
```

Rules:

- Domain crates should not perform network I/O.
- Runtime-specific protocol details belong in `anemoi-runtime`.
- Policy scoring belongs in `anemoi-policy`.
- Telemetry persistence belongs in `anemoi-telemetry`.
- API and CLI surfaces should orchestrate existing services, not duplicate
  policy logic.
- MCP tools should adapt to existing services, not duplicate scheduler,
  runtime, or telemetry logic.
- Do not introduce provider-specific payloads into `anemoi-core`.

---

## 6. Scheduling Model

Scheduling target is not:

```text
request -> model
```

Scheduling target is:

```text
request -> domain -> roster -> residency group -> profile -> runtime
```

Every decision must produce an explanation with reasons and rejected options
where relevant.

---

## 7. Residency States

Use the established residency vocabulary:

- `cold`
- `loading`
- `warm_cpu`
- `partial`
- `hot_gpu`
- `serving`
- `draining`
- `evicting`
- `failed`

Do not invent alternate state names unless the existing model cannot represent
a real, reviewed runtime observation.

---

## 8. Continuity Policy

Anemoi should prefer responsive degraded execution over unexplained blank waits
when policy allows.

The first proof behavior is:

```text
large model would cold-load
small worker is already hot
interactive latency budget is tight
=> select hot worker now
=> stage large model in background when allowed
=> explain the decision
```

Do not mark continuity behavior as complete unless tests validate the decision
action, selected model, staged model, and explanation reason.

---

## 9. Runtime Adapter Rules

Adapters may inspect or hand off to runtimes. They must not become policy
engines.

Initial adapter priority:

1. `MockRuntimeAdapter`
2. `LlamaSwapAdapter`
3. `OllamaAdapter`
4. `LlamaCppAdapter`

The mock adapter is the default for deterministic tests.

---

## 10. Configuration Rules

- Keep config explicit and reviewable.
- Prefer YAML for local policy configuration.
- Do not hardcode private hostnames, tokens, or environment-specific paths in
  production code.
- Treat cloud execution as future, policy-controlled, and opt-in.
- Do not commit secrets.

---

## 11. Testing Policy

Tests should prioritize policy behavior before runtime integration.

Required early coverage:

- config parsing
- candidate generation
- hot resident reuse
- cold-load avoidance
- continuity background staging
- runtime unavailable behavior
- memory pressure scoring
- explanation completeness
- decision log persistence
- API/CLI smoke behavior where relevant

Core policy tests should be single-purpose: one rule, one behavior, one reason
to fail.

### Tests Must Observe Behavior

Green `fmt`, `test`, and `clippy` measure shape, not behavior. A change can pass
all three and still deliver nothing. These rules close that gap and are the
definition of done for any prompt that claims a working feature.

- **Assert the observed value, not that a call returned.** `assert!(x.is_ok())`
  or `assert!(x.is_some())` as the only assertion is not a passing test — it
  cannot tell a working implementation from a broken one. Record something, read
  it back, and `assert_eq!` the value you expected. Negative assertions
  (`is_err`, `is_none`) are allowed when refusal or absence *is* the behavior
  under test (for example, a safety gate that must deny).
- **Durability requires a restart round-trip.** A claim that data survives
  restart is proven only by writing through one store instance, dropping it, and
  opening a *fresh* instance over the same file or URL to read the value back. A
  test that reads from an in-memory cache held by the same instance proves
  nothing about durability.
- **The feature must be reachable from the real binary.** If the only caller of
  a new code path is a `#[cfg(test)]` helper, the feature does not exist for
  operators. Wire it into the daemon or CLI entry point and prove it through that
  entry point. Production code never lives behind a public test-only function.
- **External issue schemas are checklists.** When a prompt references an issue
  that defines a schema (for example issue #12 `resident_events`), every required
  column or field must exist and be exercised by a test that reads it back. A
  schema that is documented but not populated and queried is incomplete.
- **Grade before landing.** Before flipping a roadmap row to `Passing`, re-read
  the acceptance criteria and confirm each one is met by a behavioral test, not
  by the gates being green. If you cannot point to the test that would fail if
  the feature regressed, the feature is not done.

`anemoi-guard` (run in CI) enforces the mechanically checkable subset of the
above: no `pub fn` inside a `#[cfg(test)]` module, and no test whose only
assertion is `is_ok`/`is_some`. Suppress a genuinely intentional exception with a
comment containing `anemoi-guard:allow` and a one-line reason. Reaching for the
escape hatch instead of asserting a value is a code smell — prefer the real
assertion.

---

## 12. Validation Expectations

For Rust changes, run the strongest applicable checks:

```powershell
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p anemoi-guard -- crates
```

If Rust tooling is unavailable, report that explicitly.

Do not run live runtime or infrastructure commands unless the user explicitly
asks for live validation in the current task.

---

## 13. Live Runtime Safety

Live runtime work is allowed only when explicitly requested.

Before live runtime changes:

- confirm the runtime owner
- confirm host path and config path
- capture read-only current state
- create rollback snapshots before editing live config
- make one change at a time
- validate before continuing
- record remaining `Needs validation` items

Never move secrets, model credentials, private prompts, transcripts, generated
user data, or host-only sensitive values into Git.

---

## 14. Reasoning Context

Each task must build understanding from:

- `AGENTS.md`
- `README.md`
- `CONTRIBUTING.md`
- `Cargo.toml`
- `config/anemoi.example.yaml`
- relevant crate source
- current working tree state

Use repository evidence over assumptions. Use `TBD`, `Unknown`, or `Needs
validation` instead of guessing.

If required context is missing and the gap is material:

- **STOP** execution
- report the missing information or conflict
- do not guess

Reasonable implementation-level assumptions are allowed when they stay within
prompt scope, do not violate documented architecture, and do not invent new
product behavior.

---

## 15. Anti-Patterns

Do not:

- turn Anemoi into an inference runtime
- turn Anemoi into a provider gateway before core residency policy works
- hide scheduling reasons
- make policy scoring probabilistic in core v1
- duplicate scheduling logic across API, CLI, and policy crates
- reintroduce retired product surfaces without an explicit architecture task
- weaken local-first or security constraints
- claim a runtime adapter is complete because a mock test passed

---

## 16. Prompt Completion Discipline

### Promotion Rule

A prompt is not complete until all of the following are true:

- The procedure or design document exists.
- The required test names declared in `docs/build_prompts/` exist by exact
  name in the crate test output.
- `cargo test --workspace` passes with those tests present.
- Any safety gate, opt-in flag, or permission boundary described in the
  prompt is enforced by code, not only by documentation.
- `docs/test_roadmap.md` status is updated to `Passing` in the same commit.
- The work is committed. Uncommitted work does not exist.

Documentation alone does not close a prompt.

### No Skipped Prompts

Do not begin prompt N+1 while prompt N is incomplete. Do not leave gaps in
the prompt sequence. If a prompt is deferred, mark it `Deferred` in the
roadmap with a reason.

### Required Test Names Are Declared Upfront

Each build prompt in `docs/build_prompts/` must list exact test function
names under `## Required Tests` before implementation begins. Those exact
names must appear in `cargo test --workspace` output to pass. Renaming or
omitting a required test name is a promotion failure.

### Commit Cadence

Each prompt lands as one or more commits. Do not start a new prompt while
a prior prompt has uncommitted scaffolding. Use `git status` to confirm
before starting.

### Roadmap Is Source of Truth

`docs/test_roadmap.md` is the canonical record of what is done and what is
pending. Updating its status is part of the prompt's commit — not a
separate follow-up. A prompt whose tests pass but whose roadmap still says
`Pending` is not promoted.

### Verification Before Claiming Done

Before marking a prompt `Passing`, run:

```powershell
cargo fmt --check
cargo test --workspace
```

Confirm the prompt's required test names appear by name in the output.
`cargo test` passing is necessary but not sufficient — the named tests must
be present.

### Safety Gates Must Be Code-Enforced

If a prompt describes a permission boundary, opt-in flag, or approval gate
(example: `ANEMOI_ENABLE_LIVE_EXECUTE=1`), that gate must be enforced in
daemon or CLI code. A gate that exists only in documentation provides no
protection.

### Branch Discipline

Work on branches, not main. Each branch covers one prompt or one coherent
fix. See Section 19 for the full issue workflow.

---

## 17. Delivery Rules

- Every change must reference a GitHub issue. If none exists, stop and create
  one or request direction before proceeding.
- Implementation work happens on a branch named
  `issue/<issue-number>-<short-name>`.
- Do not make agent-written changes directly on `main`. Use an issue branch.
- One issue, one branch, one PR — unless a human explicitly directs otherwise.
- PRs that fully satisfy an issue must use `Closes #N` in the PR body so
  GitHub auto-closes the issue on merge. Use `References #N` only for related
  or partial work that should not auto-close.

---

## 18. Local Agent Safety Policy

Treat the primary checkout as protected and recoverable. Agent-written changes
belong on an issue branch, not directly on `main`, unless the user explicitly
permits it.

### 18.1 Baseline Capture

Before any agent-written edit batch, capture:

```powershell
git status --short --branch
git rev-parse HEAD
git diff --stat
```

If the working tree is dirty with unrelated work, do not edit in place. Use a
clean sibling worktree or stop and report the conflict.

### 18.2 Protected Files

These files require an explicit user request before editing:

- `AGENTS.md`
- `CONTRIBUTING.md`
- `README.md`
- `.github/workflows/*`

Protected does not mean never edit. It means edits must be intentional,
scoped, and called out in the commit message.

`Cargo.toml` and `Cargo.lock` are not protected, but edits must stay within
the crate scope of the current prompt and be noted in the PR body.

### 18.3 Safety Checks

After every edit batch and before committing, verify:

```powershell
git diff --check
git status --short --branch
```

Also scan for conflict markers and encoding corruption:

```powershell
rg -n "<<<<<<<|=======|>>>>>>>" --glob "!target/**"
```

If conflict markers, mojibake, or unexpected broad file churn appear — stop.
Do not ask the same agent context to repair its own corrupted output. Restore
from the baseline or discard the worktree.

### 18.4 Timeout and Corruption Rules

Any timeout makes the run suspect. After a timeout, run safety checks and
inspect changed files before continuing. If the work is not coherent, discard
and restart from a clean state.

### 18.5 Task Size

Match task size to risk.

Lower-risk tasks for agent execution:

- single-file edits
- focused tests for one rule
- config changes within a single crate
- adding a build prompt document

Higher-risk tasks that need explicit review and isolation:

- repo-wide renames or restructuring
- workflow or CI changes
- governance file changes (`AGENTS.md`, `CONTRIBUTING.md`)
- any change touching multiple crate boundaries at once
- continuation after a timeout or partial failure

---

## 19. Issue Workflow

All agent work must satisfy these requirements.

### 19.1 Starting State

Before starting any issue work, determine whether this is new issue work or
continuation of an existing branch.

For all issue work:

- confirm current branch
- confirm `origin` remote
- confirm working tree status
- identify the GitHub issue number and expected branch name

For continuation of an existing issue branch:

- stay on or switch to the existing branch
- do not recreate the branch from `main`
- do not abandon existing branch work to satisfy a fresh-start rule
- inspect branch status, upstream, and uncommitted changes before editing

For new issue work:

- start from updated `main`
- pull `origin/main` immediately before creating the issue branch
- create the branch in a clean checkout

If the current checkout is dirty with unrelated changes, use a clean sibling
worktree from updated `main` rather than disturbing the existing checkout.

### 19.2 Branching

Create one branch per issue:

```
issue/<issue-number>-<short-name>
```

If the correct issue branch already exists locally or remotely, use it instead
of creating a duplicate. If multiple plausible branches exist for the same
issue, stop and report the options.

### 19.3 Execution

For each issue:

1. Confirm the prompt is narrowly scoped and has declared required test names.
2. Review scope, acceptance criteria, dependencies, and relevant build prompt
   doc in `docs/build_prompts/`.
3. Implement only that prompt.
4. Avoid unrelated changes.
5. Run the required validation: `cargo fmt --check` and `cargo test --workspace`.
6. Confirm the required test names appear by name in `cargo test` output.
7. Update `docs/test_roadmap.md` to `Passing` in the same commit.
8. Open a PR targeting `main` with `Closes #N` in the body.
9. Stop after the PR is open unless explicitly asked to continue.

### 19.4 Stop Conditions

Stop and report instead of improvising when:

- the prompt scope is materially unclear or contradictory
- the current branch does not match the expected issue branch
- multiple existing branches appear to belong to the same issue
- required test names have not been declared upfront
- required context, dependency, or approval is missing
- protected files need edits without explicit user approval
- `cargo test --workspace` fails and cannot be resolved within prompt scope
- safety checks show conflict markers, corruption, or unexpected broad churn
- proceeding would require an architecture violation

When stopping, report: the issue number, current branch state, the conflict or
missing information, impacted files, and the recommended next action.

### 19.5 Cleanup After Merge

After a branch is merged:

- delete the completed branch locally: `git branch -d <branch>`
- delete the completed branch on GitHub: `git push origin --delete <branch>`
- remove any sibling worktree used for that issue
- verify the surviving `main` checkout contains the merged commit before
  deleting a worktree

---

## 20. Priority Order

When requirements conflict, apply this order:

1. Respect prompt scope
2. Satisfy acceptance criteria and required test names
3. Preserve architecture (AGENTS.md crate boundaries, anti-patterns)
4. Preserve workflow integrity (issue branch, roadmap update, commit cadence)
5. Then implement functionality

---

## Final Principle

Continuity > silence
Explanation > magic
Policy > improvisation
