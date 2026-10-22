You are a senior Rust engineer working on Anemoi, a local-first inference governance layer.

The code is checked out for you at:

    C:/Users/Alex Lucero/source/repos/anemoi-exec-189

Git worktree on branch `issue/189-vram-introspection`, open as PR #192. A reviewer found one defect
left. Fix it.

RULES:
1. Work ONLY inside that worktree. Do not touch any sibling directory.
2. Stay on the current branch. Do NOT `git checkout main`, `git pull`, or create a branch.
3. Do NOT merge, do NOT modify `.github/`, do NOT force-push.
4. Do NOT delete, rename, disable or skip any existing test. No `#[allow(...)]` to quiet clippy.
5. No servers, no long-lived processes, no network calls in tests.
6. The only file you should need is `crates/anemoi-runtime/src/llama_swap/metrics.rs`, about 400
   lines. Read it fully. Do not go exploring.

## THE DEFECT

A previous fix made the total/used pair all-or-nothing, but at the **aggregate** level. That is the
wrong granularity for GPU samples, which are per-device.

`parse_sample_line` returns `(name, value)` and throws the label block away. So GPU samples are
summed without regard to which device reported them:

```rust
"llamaswap_gpu_memory_total_bytes" => add_gpu_sample(&mut vram_total_bytes, ...),
"llamaswap_gpu_memory_used_bytes"  => add_gpu_sample(&mut vram_used_bytes,  ...),
```

Consider a two-GPU scrape where device 1's *used* sample is missing or malformed:

- `vram_total_bytes = total0 + total1`
- `vram_used_bytes  = used0`

Both are `Some`, so the aggregate completeness check passes. The snapshot then reports the full
capacity of both cards against the usage of one. VRAM pressure reads far lower than reality, and the
scheduler can pick a cold load that does not fit.

This is the same class of fault the aggregate check was added to prevent. It was fixed one level too
high.

Note: the host this runs on has a single GPU, so this is not reachable there today. It is reachable
on any multi-GPU host. Fix it anyway.

## REQUIRED BEHAVIOR

Preserve the device identity of GPU samples and only aggregate matching sets.

The device is the `id` label, for example:

```
llamaswap_gpu_memory_used_bytes{id="0",name="NVIDIA RTX 4000 Ada Generation",uuid="GPU-..."} 2.0699938816e+10
```

Either of these is acceptable, your judgement:

- **Per-device pairing.** Aggregate only devices that reported a valid total *and* a valid used
  sample. A device missing either half contributes nothing to both sums.
- **Set equality.** Track which device ids produced a valid total and which produced a valid used.
  If the two sets differ, report the whole VRAM pair as `None`.

Prefer whichever is simpler to read. Say in your report which you chose and why.

Constraints on the change:

- Host RAM samples are unlabeled and must keep working exactly as now.
- A GPU sample whose `id` label is absent or unparseable must not silently join another device's
  bucket. Treat it as its own unusable entry, or reject it. Do not guess.
- Keep the existing protections intact: values at or above 2^64 rejected before the cast, checked
  accumulation with the sticky overflow flag, `#` comment lines skipped, unknown reported as `None`
  and never as a fabricated zero.
- Do not change `RuntimeMemorySnapshot`, do not touch `adapter.rs`, do not touch `pressure.rs`.

## REQUIRED TESTS

Add to the existing `#[cfg(test)] mod tests` block in the same file:

- two devices, both halves present: totals and used both sum across devices, as today
- two devices, device 1's used sample **missing**: the VRAM pair is not a full total against a
  partial used. Assert the specific numbers your chosen design produces, and make the assertion
  message say why
- two devices, device 1's used sample **malformed** (for example `not_a_number`): same outcome
- a GPU sample with **no `id` label**: does not corrupt another device's figures
- single device, both halves present: unchanged from today
- host RAM pair still parses while a GPU pair is incomplete

## DEFINITION OF DONE

Run each yourself from the worktree root and show the real output:

    cargo fmt --check
    cargo build --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo run -p anemoi-guard -- crates

BASELINE: `cargo test --workspace` currently reports **344 passing, 0 failed, 0 ignored** on this
branch. Your total must be greater than 344.

## COMMIT AND PUSH

Header must match `type(scope): subject`, no trailing period, scope `runtime`.
Example: `fix(runtime): pair GPU memory samples by device id`

When every gate passes: commit, then `git push origin issue/189-vram-introspection`.
Do NOT open a new pull request. The branch already has PR #192.

If any gate fails, do NOT push. Fix it, or report the blocker.

## VERIFY THIS SPEC

I checked that `parse_sample_line` discards the label block and that GPU samples accumulate without
device identity. Verify it yourself before acting. If anything above is wrong, say so in your report
rather than inventing a change to match it.

## REPORT

- WHAT I VERIFIED ABOUT THE DEFECT
- WHICH DESIGN I CHOSE, per-device pairing or set equality, AND WHY
- WHAT I CHANGED, file by file
- HOW A DEVICE MISSING ONE HALF IS NOW HANDLED, quoting the relevant lines
- HOW AN UNLABELLED GPU SAMPLE IS HANDLED
- THE FULL OUTPUT OF EVERY GATE
- BEFORE AND AFTER TEST TOTALS
