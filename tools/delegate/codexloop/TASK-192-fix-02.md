You are a senior Rust engineer working on Anemoi, a local-first inference governance layer.

The code is checked out for you at:

    C:/Users/Alex Lucero/source/repos/anemoi-exec-189

It is a git worktree on branch `issue/189-vram-introspection`, open as PR #192. The branch contains a
working feature plus one round of fixes. An automated reviewer found two more defects. Fix both.

RULES:
1. Work ONLY inside that worktree. Do not touch any sibling directory.
2. Stay on the current branch. Do NOT `git checkout main`, `git pull`, or create a branch.
3. Do NOT merge, do NOT modify `.github/`, do NOT force-push.
4. Do NOT delete, rename, disable or skip any existing test. No `#[allow(...)]` to quiet clippy.
5. Do NOT run servers or long-lived processes. Only the gate commands below.
6. No network calls in tests. Use static fixtures.
7. `crates/anemoi-daemon/src/lib.rs` is over 7000 lines. Do NOT read it end to end. You do not need
   it for this work; the context below is already verified.

## DEFECT 1: incomplete metric pairs produce a false "empty runtime"

File: `crates/anemoi-runtime/src/llama_swap/metrics.rs`

`parse_llama_swap_memory_metrics` maps each metric independently. If a scrape carries
`llamaswap_gpu_memory_total_bytes` but the matching `..._used_bytes` is missing or malformed, the
result is `vram_total_mb: Some(...)` with `vram_used_mb: None`. The same applies to the host RAM pair.

This is not harmless. Verified downstream in `crates/anemoi-policy/src/pressure.rs`:

- `fn current_pressure(used: Option<u64>, total: Option<u64>)` at about line 201 does
  `let used = used.unwrap_or(0);` at about line 204
- `fn projected_pressure(...)` does the same at about line 219

So a total with no used reads as **zero percent used**. The scheduler concludes an already-full
runtime is empty and can pick a cold load that cannot fit.

`None` means unknown and is handled safely. A fabricated zero is acted on. That is the bug.

### Required behavior

A total/used pair must be all-or-nothing. If either half of a pair is absent or was rejected as
malformed, **both** fields for that pair become `None`.

The two pairs are independent of each other: VRAM (`vram_total_mb`, `vram_used_mb`) and host RAM
(`ram_total_mb`, `ram_used_mb`). A broken VRAM pair must not discard valid RAM values, and vice
versa.

Do not change `pressure.rs`. Its `unwrap_or(0)` is only reachable when a total exists, and this fix
makes that state unreachable for incomplete scrapes. Fixing it at the source is correct.

## DEFECT 2: the metrics probe can consume the whole reconciliation budget

File: `crates/anemoi-runtime/src/llama_swap/adapter.rs`, around line 258, in `inspect()`.

The `/metrics` fetch added by this branch is optional; failure degrades to
`RuntimeMemorySnapshot::default()`. But it inherits the adapter's default request timeout, and these
numbers are verified:

| Fact | Value | Location |
|---|---|---|
| Adapter default request timeout | `Duration::from_secs(5)` | `adapter.rs` about line 41 |
| Reconciliation snapshot TTL | `DEFAULT_RECONCILIATION_TTL_MS = 5000` | `anemoi-daemon/src/lib.rs:36` |
| Reconciliation tick interval | `TTL / 2`, so 2500 ms | `serve()`, about line 5738 |
| Adapters inspected | **serially**, `for (runtime_id, adapter) in &self.runtimes { adapter.inspect().await }` | about line 1355 |

So a `/metrics` endpoint that hangs burns a full 5 seconds inside one adapter's `inspect()`. That
exceeds the tick interval and equals the whole TTL, so cached state goes stale and every runtime
later in the loop is delayed, on every tick.

### Required behavior

Bound the optional metrics probe to a deadline **well below** the reconciliation TTL, so a hanging
metrics endpoint cannot starve the tick. Roughly one second is sensible; pick a value, define it as a
named constant with a comment explaining the relationship to the TTL, and do not silently rely on the
adapter's general timeout.

Degrade exactly as now when the deadline is hit: a warning and `RuntimeMemorySnapshot::default()`,
never an error that makes a healthy runtime look unavailable.

Keep it simple. A timeout around the existing call is enough. Do not restructure `inspect()` into
concurrent probes unless that is genuinely simpler, and do not add a dependency.

## REQUIRED TESTS

Add to the existing `#[cfg(test)] mod tests` blocks in the files you change.

For defect 1:
- total present, used missing, yields `None` for **both** fields of that pair
- total present, used malformed (for example `not_a_number`), yields `None` for both
- a broken VRAM pair leaves a valid RAM pair intact
- fully valid input still parses to the same values as before

For defect 2, if the timeout is testable without network, test it. If it is not testable without a
live endpoint, say so plainly in your report rather than writing a test that does not exercise it.

## DEFINITION OF DONE

Run each yourself from the worktree root and show the real output:

    cargo fmt --check
    cargo build --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo run -p anemoi-guard -- crates

BASELINE: `cargo test --workspace` currently reports **338 passing, 0 failed, 0 ignored** on this
branch. Your total must be greater than 338.

## COMMIT AND PUSH

Header must match `type(scope): subject`, no trailing period, scope `runtime`.
Example: `fix(runtime): drop incomplete memory pairs and bound the metrics probe`

When every gate passes: commit, then `git push origin issue/189-vram-introspection`.
Do NOT open a new pull request. The branch already has PR #192.

If any gate fails, do NOT push. Fix it, or report the blocker.

## VERIFY THIS SPEC

Every factual claim above was checked against this worktree before writing, including the
`pressure.rs` line numbers and the timing constants. Verify anyway. If anything is wrong, say so in
your report rather than inventing a change to match it.

## REPORT

- WHAT I VERIFIED ABOUT EACH DEFECT
- WHAT I CHANGED, file by file
- HOW AN INCOMPLETE PAIR IS NOW HANDLED, quoting the relevant lines
- WHAT DEADLINE I CHOSE FOR THE METRICS PROBE, AND WHY
- WHETHER THE TIMEOUT IS COVERED BY A TEST, and if not, why not
- THE FULL OUTPUT OF EVERY GATE
- BEFORE AND AFTER TEST TOTALS
