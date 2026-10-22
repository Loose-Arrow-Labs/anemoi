You are a senior Rust engineer working on Anemoi, a local-first inference governance layer.

Worktree (already on branch `issue/189-vram-introspection`, based on `main`):

    C:/Users/Alex Lucero/source/repos/anemoi-exec-189

This is a CONTINUATION. A previous run implemented the feature and ran out of context before writing
tests. The implementation is complete and correct. Your job is to finish it.

RULES:
1. Work ONLY inside that worktree. Do not touch the sibling directory `anemoi`.
2. You are already on the right branch. Do NOT `git checkout main`, `git pull`, or create a branch.
3. Do NOT merge, do NOT modify `.github/`, do NOT force-push.
4. Do NOT delete, rename, disable or skip any existing test. No `#[allow(...)]` to quiet clippy.
5. Do NOT run servers or long-lived processes. Only the gates below.
6. No network calls. The tests here are pure string parsing.

## CRITICAL — CONTEXT BUDGET

The previous run died by reading a large file. Do not repeat it.

**DO NOT OPEN `crates/anemoi-runtime/src/tests.rs`. It is 1473 lines and will exhaust your context
window before you finish.** You do not need it. All your tests go inline in `metrics.rs`.

Read only these:
- `crates/anemoi-runtime/src/llama_swap/metrics.rs` (110 lines) — the code under test
- `crates/anemoi-runtime/src/util.rs` (27 lines) — only if you need `bytes_to_mb`

That is enough. Do not go exploring.

## WHAT ALREADY EXISTS (do not rewrite it)

`crates/anemoi-runtime/src/llama_swap/metrics.rs` defines:

```rust
pub(crate) fn parse_llama_swap_memory_metrics(text: &str) -> RuntimeMemorySnapshot
```

plus private helpers `parse_sample_line` and `find_label_block_end`. It already handles scientific
notation, per-GPU summation, label blocks with quoted values, `#` comments, and rejects negative or
non-finite samples. `crates/anemoi-runtime/src/llama_swap/adapter.rs` already calls it from
`inspect_memory()` and degrades to `RuntimeMemorySnapshot::default()` on failure.

**The feature works. Do not redesign it.** If you find a genuine bug while writing tests, fix it and
say so explicitly in your report — but do not refactor working code for taste.

## YOUR TASK

1. Add an inline `#[cfg(test)] mod tests { ... }` block at the END of
   `crates/anemoi-runtime/src/llama_swap/metrics.rs`.
2. Run `cargo fmt` (formatting currently FAILS `cargo fmt --check` — fixing it is part of the job).

Cover at least these cases, all as static `&str` fixtures inside the test module:

- The real four-line sample parses to the right MB values:
  ```
  llamaswap_memory_total_bytes 101195972608
  llamaswap_memory_used_bytes 59613642752
  llamaswap_gpu_memory_used_bytes{id="0",name="NVIDIA RTX 4000 Ada Generation",uuid="GPU-bae34a68"} 2.0699938816e+10
  llamaswap_gpu_memory_total_bytes{id="0",name="NVIDIA RTX 4000 Ada Generation",uuid="GPU-bae34a68"} 2.14695936e+10
  ```
  (bytes / 1024 / 1024, integer division — compute the expected values yourself and assert exact numbers)
- Scientific notation is not dropped: the GPU fields must be `Some`, not `None`.
- Two GPU devices (`id="0"` and `id="1"`) sum rather than overwrite.
- Absent metrics yield `None`, never `Some(0)`. Assert this explicitly — a zero would read to the
  scheduler as "0 MB used", which is a lie it would act on.
- `#` HELP/TYPE comment lines and unrelated metric lines are ignored.
- A malformed value (e.g. `llamaswap_memory_used_bytes not_a_number`) is skipped rather than panicking.

## DEFINITION OF DONE

Run each yourself from the worktree root and show real output:

    cargo fmt --check
    cargo build --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo run -p anemoi-guard -- crates

BASELINE: `cargo test --workspace` currently reports **330 passing, 0 failed, 0 ignored**. Your total
must be GREATER than 330, because you are adding tests. If it is still 330, your tests are not being
compiled — check that the `mod tests` block is inside `metrics.rs` and gated with `#[cfg(test)]`.

## Commit and PR (CI enforces this)

- Commit header MUST be `type(scope): subject`, no trailing period, scope `runtime`.
  Example: `feat(runtime): populate memory snapshot from llama-swap metrics`
- One commit for the whole feature is fine; the working tree already contains the implementation.

WHEN AND ONLY WHEN EVERY GATE PASSES: stage the implementation AND your tests, commit,
`git push -u origin issue/189-vram-introspection`, and open a PR with `gh pr create` using a body
FILE (never inline a long body). The body must contain what changed file by file, the full output of
every gate, before/after test totals, and any factual errors you found in this spec.
Reference issue #189.

If any gate fails, do NOT push. Fix it, or report the blocker.

## Delegation

You have a `delegate_subtask` tool backed by a much smaller, faster model. It cannot see your files
or context, so anything you send must be self-contained, and it is weak — never trust it with logic.

Worth one use here: paste the four raw metric lines above and ask it to return only the numeric
values, one per line. Use that purely as a second pair of eyes against your own transcription before
you compute expected MB values. Do NOT delegate the arithmetic, the Rust, or any decision.

If it returns garbage, say so in your report — that is useful signal about whether the tier is worth
keeping.

## REPORT

- WHAT I CHANGED, file by file
- WHETHER delegate_subtask WAS USED, AND WHETHER ITS OUTPUT WAS ANY GOOD
- ANY BUG FOUND IN THE EXISTING IMPLEMENTATION
- THE FULL OUTPUT OF EVERY GATE
- BEFORE/AFTER TEST TOTALS
- THE PR URL
