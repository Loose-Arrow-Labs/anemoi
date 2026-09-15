# Distillation Handoff Loss Procedure

Measures what a delegated executor loses when context is handed off as a written brief instead of
carried in full.

## Why This Exists

KV cache cannot move between models. Its values come from model-specific learned projections, so a
smaller model cannot consume a larger one's cache. Any handoff between tiers must therefore pass a
summary rather than real context. The cost of that has never been measured, and it decides whether
multi-tier delegation is worth building at all.

## Permission Boundary

Allowed:
- Create throwaway git worktrees and branches.
- Run the delegated executor against real open issues.
- Run the full gate suite locally.
- Read `/running`, `/logs`, `/metrics` on the llama-swap host.

Not allowed:
- Merging any experiment branch. Output is evidence, not a feature.
- Editing llama-swap configuration or restarting the service.
- Loading or evicting models outside what the runner does on its own.

## Prerequisite

PR #192 must be merged first. All worktrees branch from a main that includes it, so every arm shares
one baseline.

## Required Inputs

| Input | Source |
|---|---|
| Repo | `C:/Users/Alex Lucero/source/repos/anemoi` |
| Executor model | `qwen3.8-27b-co` via provider `prometheus-llama-swap`, 70,000 context |
| Runner | `tools/delegate/delegate run <task-file> --repo <dir>` |
| Backend | `http://llama-swap.home.arpa`, header `Authorization: Bearer $ANEMOI_LLAMA_SWAP_AUTH_TOKEN` |
| Baseline test count | `cargo test --workspace`, captured per worktree before each arm |
| Baseline test names | `cargo test --workspace -- --list`, sorted, captured per worktree |

## Stage 1 Steps, issue #174, method validation

Embeds the `Explanation` struct inline in gateway error responses.
`Explanation { summary, reasons, rejected_options }` is in
`crates/anemoi-core/src/types/decisions.rs` around line 46. There are 9 call sites of
`gateway_error(` in `crates/anemoi-daemon/src/lib.rs`.

This stage exists only to confirm the measurement can detect anything. It is deliberately simple and
is not representative of production work. No conclusion about real delegation should rest on it.

The executor must not read `crates/anemoi-daemon/src/lib.rs` end to end. It exceeds 7,000 lines and
will exhaust a 70,000 token window. Supply line anchors and greps.

### Planted constraints, stage 1

State these in the first half's task file only. Never repeat them in the handoff brief and never
restate them to the second session. Their survival is the primary measurement.

1. Do not change the HTTP status code that any existing `gateway_error` call site currently returns.
2. When a decision has no reasons, omit the explanation field entirely rather than emitting null or
   an empty object.

Stopping point for arms B and C: after the response shape is decided and applied to some but not all
call sites.

## Stage 2 Steps, issue #188, production style

Selects co-resident models by VRAM headroom rather than colocation permission alone. Spans three
crates: `anemoi-core` for the footprint data model, `anemoi-runtime` for reporting headroom, and
`anemoi-policy` for scheduler rejection plus a new `DecisionReason`.

This is the real test. Early decisions ripple. How footprint is represented, how headroom is read,
and whether the scheduler rejects or prefers a smaller candidate are all settled early and constrain
every later file.

### Planted constraints, stage 2

First half only, never repeated.

1. Footprint must be a single `vram_estimate_mib` field on the model profile, never computed from
   separate weights, KV and compute-buffer fields. A computed model implies precision that is not
   available.
2. Headroom must be read from the live snapshot at decision time and never cached across decisions,
   because the executor's KV grows during a run and a value from even a minute ago is wrong.

Constraint 2 matters most. It encodes a measured finding, that co-residency headroom is dynamic
rather than fixed, and it is the kind of subtle rationale a short brief is most likely to drop. Watch
for a second half that caches headroom for convenience.

Stopping point for arms B and C: after the data model lands in `anemoi-core` but before the scheduler
consumes it.

## Arms

Run all three per stage. Same issue, same baseline commit, each arm in its own worktree so arms
cannot contaminate each other.

| Arm | Procedure |
|---|---|
| A, control | One session, no interruption, runs to completion. |
| B, thin brief | Stop at the stopping point. Executor writes a brief of 400 tokens or fewer. A fresh session receives only that brief plus the issue text, and finishes. |
| C, rich brief | Identical to B, brief up to 2,000 tokens. |

## Evidence Table

| Measurement | Stage 1 A | Stage 1 B | Stage 1 C | Stage 2 A | Stage 2 B | Stage 2 C |
|---|---|---|---|---|---|---|
| Constraint 1 survived | TBD | TBD | TBD | TBD | TBD | TBD |
| Constraint 2 survived | TBD | TBD | TBD | TBD | TBD | TBD |
| Consistency | TBD | TBD | TBD | TBD | TBD | TBD |
| `cargo fmt --check` | TBD | TBD | TBD | TBD | TBD | TBD |
| `cargo build --workspace` | TBD | TBD | TBD | TBD | TBD | TBD |
| `cargo test --workspace` | TBD | TBD | TBD | TBD | TBD | TBD |
| `cargo clippy -D warnings` | TBD | TBD | TBD | TBD | TBD | TBD |
| `anemoi-guard` | TBD | TBD | TBD | TBD | TBD | TBD |
| Test count vs baseline | TBD | TBD | TBD | TBD | TBD | TBD |
| Test names removed | TBD | TBD | TBD | TBD | TBD | TBD |
| Files rewritten by second half | TBD | TBD | TBD | TBD | TBD | TBD |
| Wall time | TBD | TBD | TBD | TBD | TBD | TBD |

Consistency means, for stage 1, how many of the 9 `gateway_error` call sites received the new
treatment. For stage 2 it means whether the footprint model is applied the same way across all three
crates.

## Interpretation Rules

- The orchestrator runs every gate itself. A gate result read from the executor's report is not
  evidence.
- A dropped constraint is the signal. A slightly different but correct implementation is not loss.
- If arm A also drops a constraint, the constraint was badly stated and that stage is void. Rerun
  with a clearer one rather than reporting a false positive.
- An arm that passes all gates while dropping a constraint is the most important case to report. It
  means distillation loss is invisible to CI.
- A test count equal to baseline after work that should add tests means the tests were never
  compiled, not that none were needed.

## Success Criteria

The experiment succeeds when every arm reaches a terminal state, all gates are run by the
orchestrator, and the evidence table has no `TBD` cells. It succeeds whether or not distillation loss
is found. A negative result is a result.

## Limitations

- One executor model. Findings may not transfer to a different model.
- One stopping point per stage. Loss may depend on where the handoff falls.
- Stage 1 is not production-shaped and cannot support conclusions on its own.
- Brief sizes of 400 and 2,000 tokens are arbitrary and untested as thresholds.

## Next Prompt

If distillation loss is found, it constrains anemoi issue #193 (KV cache as a transportable artifact)
and issue #191 (interrupted work continuation), since both assume a handoff can carry enough state.
If no loss is found, multi-tier delegation becomes considerably more attractive and issue #190
(residency ownership) should be revisited with that in mind.
