You are a senior Rust engineer working on Anemoi, a local-first inference governance layer.

The code is checked out for you at:

    C:/Users/Alex Lucero/source/repos/anemoi-exec-189

Git worktree on branch `issue/189-vram-introspection`, open as PR #192.

This is a **refactor with one behaviour fix**, not a new feature. Read the whole brief before
starting.

RULES:
1. Work ONLY inside that worktree. Do not touch any sibling directory.
2. Stay on the current branch. Do NOT `git checkout main`, `git pull`, or create a branch.
3. Do NOT merge, do NOT modify `.github/`, do NOT force-push.
4. **Do NOT delete, rename, weaken or skip any existing test.** They are the contract. Details below.
5. No `#[allow(...)]` to quiet clippy. No servers, no network calls in tests.
6. The only file to change is `crates/anemoi-runtime/src/llama_swap/metrics.rs`, 701 lines. Read it
   fully. Do NOT touch `adapter.rs`, `pressure.rs`, or `RuntimeMemorySnapshot`.

## WHY THIS REWRITE EXISTS

`parse_llama_swap_memory_metrics` accumulates while it scans. Every correctness rule discovered so
far has been bolted onto that running total as a separate guard: a sticky overflow flag, two
`BTreeSet`s of device ids, an exactness range check, a label-presence check. There are about 14 such
guard references in the file.

Three review rounds in a row found a hole where those guards fail to compose:

1. Pair completeness was checked at the aggregate level, but GPU samples are per-device.
2. Fixed with device-id set equality, but sets deduplicate while sums do not.
3. So two totals for `id="0"` plus one used sample gives a doubled total, matching sets, and a pair
   that looks complete. VRAM pressure reads artificially low and the scheduler can pick a cold load
   that does not fit.

Each guard is individually correct. The structure is what keeps failing. Patching a fourth guard onto
it is not the answer.

## THE NEW SHAPE

Two passes.

**Pass one, collect.** Walk the lines and build a map keyed by `(metric, device_id)`. Do not
accumulate anything yet. If a key appears twice, that is a duplicate: record it as such. A sample
that fails to parse, or a GPU sample with no usable `id` label, does not enter the map.

**Pass two, validate and aggregate.** With the whole scrape in hand, decide each field:

- VRAM is known only when the set of device ids with a valid total exactly equals the set with a
  valid used, **and** no key in either set was duplicated. Otherwise both VRAM fields are `None`.
- Host RAM is unlabeled, so it is a single total and a single used. Known only when both are present
  and neither was duplicated. Otherwise both RAM fields are `None`.
- Sum the per-device values with checked arithmetic. Overflow makes that field `None`.
- VRAM and RAM stay independent. A broken VRAM pair must not discard a valid RAM pair.

Deciding after collection is the point. Duplicates, missing halves, mismatched device sets and
overflow all become ordinary checks over complete data instead of four mechanisms that have to agree
with each other while scanning.

The sticky overflow flag and the two `BTreeSet`s should disappear. If you find yourself keeping them,
the rewrite has not happened.

## BEHAVIOUR THAT MUST NOT CHANGE

Everything below is already covered by tests. Preserve it:

- Values are parsed as `f64` so scientific notation survives, for example `2.0699938816e+10`.
- A value that is non-finite, negative, fractional, or at or above 2^64 is rejected. Never
  `Some(u64::MAX)`, never a fabricated `Some(0)`.
- Lines beginning with `#` and unrelated metrics are ignored.
- A malformed value is skipped rather than panicking.
- Byte counts convert to megabytes with the existing helper.
- Unknown is always `None`, never zero.

## THE ONE BEHAVIOUR CHANGE

Duplicate samples for the same metric and device id must invalidate that pair. This is the defect
above and is currently unhandled.

## TESTS

`crates/anemoi-runtime/src/llama_swap/metrics.rs` has **17 tests** in its `#[cfg(test)] mod tests`
block, from `real_four_line_sample_parses_to_expected_mb` through
`incomplete_gpu_pair_leaves_host_ram_pair_intact`.

**All 17 must still exist by exact name and must still pass, unmodified.** They encode every rule
three review rounds established. If the rewrite is correct they will pass untouched. If you feel the
need to edit one, stop and say so in your report, naming the test and why: that means either the
rewrite changed behaviour it should not have, or the test encoded something wrong. Do not quietly
adjust an assertion to fit new code.

Add tests for the duplicate case:

- two totals for `id="0"` and one used for `id="0"`: the VRAM pair is `None`, and specifically not a
  doubled total against a single used
- a duplicated host RAM total: the RAM pair is `None`
- a duplicate in the VRAM pair leaves a valid RAM pair intact

## DEFINITION OF DONE

Run each yourself from the worktree root and show the real output:

    cargo fmt --check
    cargo build --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo run -p anemoi-guard -- crates

BASELINE: `cargo test --workspace` currently reports **350 passing, 0 failed, 0 ignored**. Your total
must be greater than 350, and none of the 17 named tests may be missing.

## COMMIT AND PUSH

Header must match `type(scope): subject`, no trailing period, scope `runtime`.
Example: `refactor(runtime): parse memory metrics in two passes and reject duplicate samples`

When every gate passes: commit, then `git push origin issue/189-vram-introspection`.
Do NOT open a new pull request. The branch already has PR #192.

If any gate fails, do NOT push. Fix it, or report the blocker.

## REPORT

- WHAT THE OLD STRUCTURE WAS AND WHY IT KEPT FAILING, in your own words
- WHAT THE MAP IS KEYED BY, and how duplicates are detected
- WHICH GUARDS I WAS ABLE TO DELETE
- CONFIRMATION THAT ALL 17 EXISTING TESTS PASS UNMODIFIED, or exactly which one you changed and why
- THE FULL OUTPUT OF EVERY GATE
- BEFORE AND AFTER TEST TOTALS
