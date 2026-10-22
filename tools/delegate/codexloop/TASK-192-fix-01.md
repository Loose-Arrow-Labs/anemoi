You are a senior Rust engineer working on Anemoi, a local-first inference governance layer.

The code is checked out for you at:

    C:/Users/Alex Lucero/source/repos/anemoi-exec-189

It is a git worktree on branch `issue/189-vram-introspection`, which is open as PR #192. The branch
already contains a working feature. Your job is to fix a defect an automated reviewer found in it.

RULES:
1. Work ONLY inside that worktree. Do not touch any sibling directory.
2. Stay on the current branch. Do NOT `git checkout main`, do NOT `git pull`, do NOT create a branch.
3. Do NOT merge, do NOT modify `.github/`, do NOT force-push.
4. Do NOT delete, rename, disable or skip any existing test. No `#[allow(...)]` to quiet clippy.
5. Do NOT run servers or long-lived processes. Only the gate commands below.
6. No network calls. This is pure string parsing and arithmetic; test it with static fixtures.

## FILE UNDER REPAIR

`crates/anemoi-runtime/src/llama_swap/metrics.rs` (about 230 lines). Read the whole file, it is
small. Do not go exploring elsewhere.

## THE DEFECT

A reviewer found this, and it is correct. Two related problems in the same file.

**Problem 1, the cast saturates instead of rejecting.**

`parse_sample_line` ends with roughly:

```rust
let value = ...parse::<f64>().ok()?;
if !value.is_finite() || value < 0.0 {
    return None;
}
Some((name, value as u64))
```

The doc comment above that function claims malformed samples cannot "become a bogus `Some(0)` or a
saturated `u64::MAX`". That is not true. In Rust, `f64 as u64` **saturates**. A finite value at or
above 2^64 becomes `u64::MAX` rather than being rejected. Verified: `1e30 as u64` yields
18446744073709551615, which as megabytes is 17592186044415 MB of fabricated capacity.

The scheduler acts on these numbers. A fabricated capacity is worse than a missing one, because
`None` means unknown and a wrong number means confidently wrong.

**Problem 2, the per-GPU sum is unchecked.**

In `parse_llama_swap_memory_metrics`, GPU samples accumulate with plain `+`, roughly:

```rust
vram_total_bytes = Some(vram_total_bytes.unwrap_or(0) + value_bytes);
```

Unchecked addition panics in debug builds and wraps in release. Two large samples can overflow it.
A daemon that panics while parsing a metrics response is a worse failure than reporting unknown.

## REQUIRED BEHAVIOR

1. Reject any value that cannot be represented exactly as a `u64` **before** casting. A value at or
   above 2^64 must yield `None`, exactly like a negative or non-finite one.
2. Make the accumulation checked. On overflow, the field becomes `None` (unknown) rather than
   panicking or wrapping.
3. Make the doc comment true. Either it accurately describes what the code rejects, or it is
   reworded. Do not leave a comment that promises a guarantee the code does not provide.

Keep the change minimal and local to this file. Do not redesign the parser, do not rename public
items, do not touch the adapter.

## REQUIRED TESTS

Add these to the existing `#[cfg(test)] mod tests` block at the end of the same file. Cover:

- a finite value at or above 2^64 (for example `1e30`) yields `None` for that metric, and
  specifically **not** `u64::MAX`
- two GPU device samples that would overflow when summed leave the field `None` rather than
  panicking or wrapping
- the existing good cases still parse correctly, so the fix did not break normal operation

## DEFINITION OF DONE

Run each yourself from the worktree root and show the real output:

    cargo fmt --check
    cargo build --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo run -p anemoi-guard -- crates

BASELINE: `cargo test --workspace` currently reports **336 passing, 0 failed, 0 ignored** on this
branch. Your total must be greater than 336.

## COMMIT AND PUSH

CI enforces the commit format. Header must match `type(scope): subject`, no trailing period, scope
`runtime`. Example: `fix(runtime): reject metric values that overflow u64`

When every gate passes: commit, then `git push origin issue/189-vram-introspection`.

Do NOT open a new pull request. The branch already has PR #192; pushing updates it.

If any gate fails, do NOT push. Fix it, or report the blocker.

## VERIFY THIS SPEC

The defect description above is from a reviewer and I believe it is correct, but verify it against
the actual code before acting. If any claim here is wrong, say so explicitly in your report rather
than inventing a change to match it.

## REPORT

- WHAT I VERIFIED ABOUT THE DEFECT, and whether the description was accurate
- WHAT I CHANGED, file by file
- HOW THE OVERFLOW IS NOW REJECTED, quoting the relevant lines
- THE FULL OUTPUT OF EVERY GATE
- BEFORE AND AFTER TEST TOTALS
