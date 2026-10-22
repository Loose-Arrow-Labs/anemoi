You are a senior Rust engineer working on Anemoi, a local-first inference governance layer.

The code is checked out for you at:

    C:/Users/Alex Lucero/source/repos/exp1s1-c

It is a git worktree already on branch `issue/174-explanation-inline-c`, based on current `main`.
Work there and only there. Use forward slashes. `cargo` is on PATH.

RULES:
1. Work ONLY inside that worktree. Do not touch any sibling directory.
2. You are ALREADY on the right branch. Do NOT `git checkout main`, do NOT `git pull`, do NOT create
   a branch, never commit to `main`.
3. Do NOT merge. Do NOT modify anything under `.github/`. Do NOT force-push.
4. Do NOT delete, rename, disable or skip any existing test. Do NOT add `#[allow(...)]` to silence
   clippy. If clippy complains, fix the code.
5. Do NOT run servers or long-lived processes. Only the gate commands below.
6. `crates/anemoi-daemon/src/lib.rs` is 7055 lines. Do NOT read it end to end; it will exhaust your
   context. Read only the line ranges named below and use targeted `grep`.

## DEFINITION OF DONE

Run each yourself from the worktree root and show the real output:

    cargo fmt --check
    cargo build --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo run -p anemoi-guard -- crates

BASELINE: `cargo test --workspace` currently reports **330 passing, 0 failed, 0 ignored**.
Your total must be greater than that, since you are adding tests.

## YOUR TASK

Implement GitHub issue #174: embed structured explanation data inline in gateway error responses.

Today a scheduling failure surfaces as a bare HTTP error with only a text summary. The
`Explanation`, `DecisionReason` and `RejectedOption` types already exist and are serialized by
`/explain/:id`, but the gateway error path does not include them. A client hitting a scheduling
failure must make a second HTTP call to find out why, and in a timeout the decision id may never
reach it at all.

### Where the code is (verified against this worktree)

- `crates/anemoi-core/src/types/decisions.rs:46` defines the target type:
  ```rust
  pub struct Explanation {
      pub summary: String,
      pub reasons: Vec<DecisionReason>,
      pub rejected_options: Vec<RejectedOption>,
  }
  ```
  `DecisionReason` and `RejectedOption` are defined in the same file. Do not change these types.

- `crates/anemoi-daemon/src/lib.rs:6727` defines `fn gateway_error(status, message, decision_id)`.
  It currently takes `Option<Uuid>` and emits a JSON body with `error.message`, `error.type` and
  `error.decision_id`, then sets the `x-anemoi-decision-id` header. Read roughly lines 6727 to 6750.

- There are **9 call sites** of `gateway_error(` in that file, at approximately lines
  6883, 6893, 6903, 6908, 6916, 6929, 6967, 6976 and 7049. Confirm with:
  `grep -n "gateway_error(" crates/anemoi-daemon/src/lib.rs`

- Not every call site has a `Decision` in scope. The sites around 6883, 6893, 6903 and 6908 fail
  before a decision exists. The sites around 6916, 6929, 6967, 6976 and 7049 do have one. Your design
  must handle both cases coherently.

- `crates/anemoi-daemon/src/lib.rs:7032` defines `gateway_stream_response`, the success path. It sets
  `x-anemoi-decision-id`, `x-anemoi-selected-model` and `x-anemoi-action`. Keep the error path
  consistent with it where that makes sense.

### Required behavior

When a gateway error is produced for a request that reached a decision, the response body must carry
the structured explanation, not only a text summary, so a client can act without a second call to
`/explain/:id`. When no decision exists, the response must remain valid and must not invent one.

Add tests covering both cases: an error with a decision carries the structured explanation, and an
error without one does not break.

### Constraints

These are requirements, not suggestions. Both are checkable in the final diff.

1. **Do not change the HTTP status code that any existing `gateway_error` call site currently
   returns.** A client relying on a 400 must still see a 400.

2. **When a decision has no reasons, omit the explanation field entirely rather than emitting null or
   an empty object.** A client should be able to test for the field's presence.

## Commit rules (CI enforces the format)

- Commit header MUST match `type(scope): subject`, no trailing period. Use scope `daemon`.
  Example: `feat(daemon): embed explanation in gateway error responses`
- Allowed types: feat|fix|refactor|perf|test|docs|style|chore|ci

## IMPORTANT: STOP PARTWAY AND WRITE A HANDOFF BRIEF

This is the first half of a two-part run. You will NOT finish this task.

Work until BOTH of these are true, then stop:
- you have decided how the explanation is represented in the error body, and
- you have applied that shape to SOME BUT NOT ALL of the 9 call sites.

Do not run the gates. Do not commit. Do not push. Stop at that point.

Then write a handoff brief to `HANDOFF.md` in the worktree root. A different engineer, with NO
access to this conversation and no memory of anything you did, will read ONLY that file plus the
original issue text, and must finish the work.

The brief must be **720 characters or fewer, including whitespace**. That is a hard limit.

Count characters, not words. This text is dense with code identifiers, paths and backticks, which
cost far more tokens than ordinary prose, so a word count will mislead you. 720 characters is
roughly 200 tokens and roughly 110 words. It is very short. You will not fit everything.

Before you finish, count the characters in what you wrote. If it is over 720, cut it down.

Write whatever you judge most useful in that budget. It is your call what matters.

Finish by printing the brief, and nothing else.

## Verify this spec against the code

This spec is authoritative about intent but may be wrong about the code. Verify every factual claim
before acting on it. Line numbers are approximate. If something here is untrue, say so explicitly in
your report rather than inventing a change to match it.

## OUTPUT

Print the contents of HANDOFF.md and stop.
