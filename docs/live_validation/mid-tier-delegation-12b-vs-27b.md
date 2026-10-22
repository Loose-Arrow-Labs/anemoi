# Mid-Tier Delegation Procedure: 12B versus 27B

Measures whether a weaker executor with a protected context window beats a stronger one that runs
out of context.

## Why This Exists

A `qwen3.8-27b-co` executor at 70,000 context died mid-task on issue #189 after reading one 1,473
line file. It had written the implementation and had not yet written tests. The run reported
`DONE_EXIT_0` and looked like a success.

On a 20 GB card the arithmetic is unforgiving. A 27B weighs 14.63 GB, so its only possible
co-resident partner is a 2B at 1.40 GB, and that pairing misses by roughly 50 MiB once KV and compute
buffers are counted. A 12B weighs 6.26 GB and leaves ample room for a real delegation tier.

So the question is whether a 12B that can offload large reads beats a 27B that cannot. This has been
argued from intuition and never measured.

## Permission Boundary

Allowed:
- Create throwaway git worktrees and branches.
- Run the delegated executor against a real open issue.
- Run the full gate suite locally.
- Load models via the runner and via `tools/delegate/delegate check-pair`.
- Read `/running`, `/logs`, `/metrics` on the llama-swap host.

Not allowed:
- Merging any experiment branch.
- Editing llama-swap configuration or restarting the service.
- Calling `GET /unload`. It is a destructive GET that evicts every resident model.

Before any arm, confirm nothing else is using the GPU:

```
curl -s -H "Authorization: Bearer $ANEMOI_LLAMA_SWAP_AUTH_TOKEN" http://llama-swap.home.arpa/running
```

`check-pair` loads models and will evict whatever is resident. Do not run it against a busy GPU.

## Required Inputs

| Input | Source |
|---|---|
| Repo | `C:/Users/Alex Lucero/source/repos/anemoi` |
| Runner | `tools/delegate/delegate run <task-file> --repo <dir>` |
| Model selection | `EXECUTOR_MODEL` and `DELEGATE_MODEL` in `tools/delegate/config.sh` |
| Backend | `http://llama-swap.home.arpa`, header `Authorization: Bearer $ANEMOI_LLAMA_SWAP_AUTH_TOKEN` |
| Baseline test count | `cargo test --workspace`, captured per worktree before each arm |
| Baseline test names | `cargo test --workspace -- --list`, sorted, captured per worktree |
| Baseline OpenAPI | `GET /openapi.json` from a running daemon, captured before any arm |

## The Task Under Test

Issue #159: extract the hardcoded OpenAPI document out of `crates/anemoi-daemon/src/lib.rs` into
`crates/anemoi-daemon/src/openapi.rs`.

| Anchor | Approximate line |
|---|---|
| `pub fn openapi_document()` | 5924 |
| `openapi()` handler | 5920 |
| route registration | 5122 |
| existing coverage | 2154, 2171 |
| file length | 7,055 lines |

Chosen because it is context-heavy but mechanically simple. The executor must locate a function
inside a very large file, then move it. That separates running out of context from lacking
capability, which a harder task would confound.

It also has an unusually clean correctness gate. Because the change is a pure move, the
`/openapi.json` response must be byte-identical before and after.

## Arms

Same issue, same baseline commit, each arm in its own worktree.

| Arm | Executor | Context | Delegation tier |
|---|---|---|---|
| A | `qwen3.8-27b-co` | 70,000 | none |
| B | `qwen3.8-27b-dflash` | 140,000 | none |
| C | `gemma-4-12b-it-qat-co` | per config | `qwen3.5-2b-mtp-co`, co-resident |

Arm A is the configuration that failed on issue #189. It may fail again. That is a result, not a
problem, and the run should not be rescued.

Arm C requires `check-pair` to pass first, confirming both models stay resident together. Its task
file must name the specific subtasks worth delegating. An executor will not discover the
`delegate_subtask` tool on its own, and a run where it silently never delegates looks like success
while proving nothing. The natural delegation here is asking the small model to summarize a region
of the large file rather than reading it directly.

## Evidence Table

| Measurement | Arm A | Arm B | Arm C |
|---|---|---|---|
| Completed or died at ceiling | TBD | TBD | TBD |
| `/openapi.json` byte-identical | TBD | TBD | TBD |
| `cargo fmt --check` | TBD | TBD | TBD |
| `cargo build --workspace` | TBD | TBD | TBD |
| `cargo test --workspace` | TBD | TBD | TBD |
| `cargo clippy -D warnings` | TBD | TBD | TBD |
| `anemoi-guard` | TBD | TBD | TBD |
| Test count vs baseline | TBD | TBD | TBD |
| Test names removed | TBD | TBD | TBD |
| Peak context used | TBD | TBD | TBD |
| Delegations attempted | n/a | n/a | TBD |
| Delegations succeeded | n/a | n/a | TBD |
| Delegate output usable | n/a | n/a | TBD |
| Wall time | TBD | TBD | TBD |

Delegation figures come from `delegations.jsonl` in the run folder.

## Interpretation Rules

- The orchestrator runs every gate itself. A gate result read from the executor's report is not
  evidence.
- `DONE_EXIT_0` does not mean the work is complete. Issue #189 exited 0 with the tests unwritten.
  Check the artifact.
- An empty `pi.log` is not evidence of an idle run. Judge progress by file modification times and
  `git status`, which is what `tools/delegate/delegate status` reports.
- A byte difference in `/openapi.json` is a failure regardless of whether the gates pass.
- Arm C delegating zero times is an inconclusive arm, not a passing one. Rerun with a task file that
  names the subtasks more directly.
- If arm B succeeds where arm A failed, context was the binding constraint and the delegation tier is
  unnecessary for this class of work.

## Success Criteria

Every arm reaches a terminal state, all gates are run by the orchestrator, and the evidence table has
no `TBD` cells. The experiment succeeds regardless of which arm wins.

## Limitations

- One task. A mechanical extraction may not represent work where a 12B's weaker reasoning matters.
- One card, 20 GB. The VRAM arithmetic that forces this trade does not apply to larger hardware.
- Peak context is inferred from the session rather than measured directly by the runtime.
- Prompt caching is enabled only on `qwen3.8-27b-co`, so arms are not equal on prefill cost. Record
  this rather than correcting for it.

## Next Prompt

If a bigger window beats the delegation tier, the tiered design in anemoi issues #191 and #193 loses
much of its motivation, and the cheaper answer is to avoid eviction rather than recover from it. If
the 12B with a tier wins, it justifies the residency work in issue #190 and the VRAM-aware selection
in issue #188.
