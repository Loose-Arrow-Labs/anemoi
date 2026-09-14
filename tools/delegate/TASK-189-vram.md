You are a senior Rust engineer working on Anemoi, a local-first inference governance layer.

The code is checked out for you at:

    C:/Users/Alex Lucero/source/repos/anemoi-exec-189

It is a git worktree already on branch `issue/189-vram-introspection`, based on current `main`.
Work there and only there. Use forward slashes. `cargo` is on PATH.

RULES:
1. Work ONLY inside C:/Users/Alex Lucero/source/repos/anemoi-exec-189. Do not touch the sibling
   directory `anemoi`.
2. You are ALREADY on the right branch. Do NOT `git checkout main`, do NOT `git pull`, do NOT create
   a branch, never commit to `main`.
3. Do NOT merge, do NOT modify `.github/`, do NOT force-push.
4. Do NOT delete, rename, disable or skip any existing test. Do NOT add `#[allow(...)]` to silence
   clippy. If clippy complains, fix the code.
5. Do NOT run long-lived processes (no `cargo run` of the daemon, no servers). Only the gates below.
6. Do NOT make network calls to the live runtime. Everything here is unit-testable with static
   fixture strings; write tests that way.

## DEFINITION OF DONE

Run each yourself from the worktree root and show the real output:

    cargo fmt --check
    cargo build --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo run -p anemoi-guard -- crates

BASELINE: `cargo test --workspace` currently reports **330 passing, 0 failed, 0 ignored**.
Your total must be >= that. A drop means you removed or broke tests.

## YOUR TASK

Populate the memory field of the llama-swap runtime snapshot from the runtime's Prometheus metrics.

Right now Anemoi's resource-pressure logic is starved: the types and the scoring helper already
exist, but the adapter never fills them in, so every value is `None`.

### The relevant code

- `crates/anemoi-runtime/src/llama_swap/adapter.rs` — 277 lines, read it fully, it is small.
  - `async fn inspect(&self)` around line 185 builds the `RuntimeSnapshot`.
  - There are **two** `RuntimeMemorySnapshot::default()` sites:
    - one around line 203, on the **unhealthy** path (`/health` failed) — LEAVE THIS AS DEFAULT.
      If the runtime is down we have no memory evidence, and `None` is the honest answer.
    - one around line 238, on the **healthy** path — this is the one to populate.
- `crates/anemoi-core/src/types/runtime.rs` around line 86 defines the target:
  ```rust
  pub struct RuntimeMemorySnapshot {
      pub vram_total_mb: Option<u64>,
      pub vram_used_mb: Option<u64>,
      pub ram_total_mb: Option<u64>,
      pub ram_used_mb: Option<u64>,
  }
  ```
  It already has `pressure_percent()` which divides `vram_used_mb` by `vram_total_mb`. Do not change
  this struct or that method; just supply real values.

### The data source

llama-swap serves Prometheus text at `/metrics` on the same base URL the adapter already uses
(build the URL the same way `inspect` builds `/health`, via `self.base_url.join(...)`).

Relevant lines, exactly as observed live:

```
llamaswap_memory_total_bytes 101195972608
llamaswap_memory_used_bytes 59613642752
llamaswap_gpu_memory_used_bytes{id="0",name="NVIDIA RTX 4000 Ada Generation",uuid="GPU-bae34a68-8f58-1b83-c77b-9ba515d4f298"} 2.0699938816e+10
llamaswap_gpu_memory_total_bytes{id="0",name="NVIDIA RTX 4000 Ada Generation",uuid="GPU-bae34a68-8f58-1b83-c77b-9ba515d4f298"} 2.14695936e+10
```

Note carefully:
- Host RAM metrics have **no labels**. GPU metrics **have labels** in `{...}`.
- Values are **not always plain integers**. The GPU values above are in scientific notation
  (`2.0699938816e+10`). A parser that only handles `u64` will silently drop them. Parse as `f64`
  and convert.
- There may be **more than one GPU**. Sum across all `id=` devices rather than assuming `id="0"`.
- The struct fields are **megabytes**, the metrics are **bytes**. Convert (use 1024*1024).
- Lines beginning with `#` are HELP/TYPE comments and must be skipped.

### Required behavior

1. Fetch `/metrics` during `inspect()` on the healthy path.
2. Parse the four values above and populate `RuntimeMemorySnapshot` in MB.
3. **A metrics failure must not make a healthy runtime look unavailable.** Follow the pattern already
   used a few lines above for `inspect_models()`: `unwrap_or_else` with a `tracing::warn!`, then
   carry on with `RuntimeMemorySnapshot::default()`. A missing metric is `None`, not zero — zero
   would read as "0 MB used", which is a lie the scheduler would act on.
4. Keep the parsing in a small, separately testable function. Do not inline it into `inspect()`.

### Tests

Unit tests over static fixture strings — no network. Cover at least:
- the exact four-line fixture above parses to the right MB values
- scientific-notation values are not lost
- two GPUs sum correctly
- missing metrics yield `None`, never `Some(0)`
- `#` comment lines and unrelated metric lines are ignored

## Commit and PR (CI enforces this)

- Commit header MUST be `type(scope): subject`, no trailing period. Use scope `runtime`.
  Example: `feat(runtime): populate memory snapshot from llama-swap metrics`
- Allowed types: feat|fix|refactor|perf|test|docs|style|chore|ci

WHEN AND ONLY WHEN EVERY GATE PASSES: commit, `git push -u origin issue/189-vram-introspection`, and
open a PR with `gh pr create` using a body FILE (never inline a long body). The body must contain
what changed file by file, the full output of every gate, before/after test totals, and any factual
errors you found in this spec. Reference issue #189.

If any gate fails, do NOT push. Fix it, or report the blocker.

## Delegation

You have a `delegate_subtask` tool backed by a much smaller, faster model. It cannot see your files,
your conversation, or the repo, so anything you send must be fully self-contained. It is weak — treat
its output as a draft to check, never as truth.

For this task it is worth using for exactly one thing: given the four raw metric lines pasted
inline, ask it to list the metric names present. Use that only as a cross-check against your own
reading of the fixture. Do NOT delegate the parsing logic, the Rust code, or any decision.

If it returns something wrong or useless, say so in your report — that is useful signal.

## IMPORTANT — verify this spec

This spec is authoritative about INTENT but may be wrong about the code. Verify every factual claim
against the actual source before acting. Line numbers are approximate.

In particular I claim there are exactly two `RuntimeMemorySnapshot::default()` call sites and that
the first is on the unhealthy path. Check that yourself with grep before editing, and if the code
differs, say so explicitly rather than inventing a change to match my description.

## REPORT

- WHAT THE SPEC ASKED FOR
- WHAT I FOUND IN THE CODE (including anything the spec got wrong)
- WHAT I CHANGED, file by file
- WHETHER delegate_subtask WAS USED, AND WHETHER ITS OUTPUT WAS ANY GOOD
- WHAT I DELIBERATELY DID NOT DO, AND WHY
- THE FULL OUTPUT OF EVERY GATE
- BEFORE/AFTER TEST TOTALS
- THE PR URL
