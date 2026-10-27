# ADR 0001: Freeze Anemoi

- **Status**: Accepted
- **Date**: 2026-10-27
- **Decider**: project owner

## Context

Anemoi was built to answer one constraint:

> Large local models are too slow to use for everything, and the GPU holds one
> large model at a time.

From that constraint came two jobs:

1. **Size routing.** Send each request to a small, medium or large tier
   (`small_swarm`, `medium_workers`, `large_context`), trading speed against
   quality per request.
2. **GPU arbitration.** Track residency, price cold loads, and keep something
   answering instead of a blank wait (`prefer_degraded_response_over_silence`).

Both jobs were sound for the hardware and engines available when the project
started.

## What changed

Measured on the operator's host (pantheon Strata evaluation, short prompt):

| Engine | Model | Decode | Runs | VRAM |
|---|---|---|---|---|
| Strata | Qwen3.8-Flash-Next | 68.90 tok/s (±1.2%) | n=4 | 19,763 MiB |
| llama.cpp | Qwen3.8-Flash-Next | 39.92 tok/s (±3.5%) | n=5 | 19,541 MiB |

Of the 1.73x ratio, about 1.12x comes from Strata's lower-bit quantization
rather than the engine. Strata's first load took 15 s; a second start took
70 s; llama.cpp's cold load took about 182 s.

Operator-reported, not re-measured here: a single large MoE model now runs at
about 70–80 tok/s on real tasks, and day-to-day use (pi, OpenCode) is "set one
model and forget it." Anemoi has not been on the request path for that traffic
in a long time.

Other facts that removed job 2:

- Voice runs on edge devices, not on this GPU.
- Ariadne's embedder and reranker run on CPU only.
- No workload currently needs to share the card with the main model.
- llama-swap already performs model swaps when a client asks for a different
  model ID.

## Decision

Freeze Anemoi. Do not build new routing, residency, or swarm features.

- **Job 1 (size routing)** has no work: one model is fast and good enough, and
  small models (2B class) were never useful for chat or agent work.
- **Job 2 (GPU arbitration)** has no contention to arbitrate. The remaining
  real GPU risk is processes outside llama-swap (a leaked llama-server on
  2026-10-13; Strata, Ollama and ComfyUI started directly). That is fixed by
  host discipline, not by a separate service: llama-swap is the only process
  that starts GPU workloads, with Strata added as a llama-swap profile.
- **Agent swarms do not revive it.** On one GPU a swarm is either sequential
  subagents (value: context isolation) or parallel slots on the same model
  (`--parallel N`). Neither needs model selection. Swapping the orchestrator
  model out for smaller workers would unload the model doing the thinking and
  load models already judged too weak.
- **Reliability is a harness concern.** Tool loop, verification, context
  management, client timeouts and permissions belong to the harness (pi,
  OpenCode, Codex CLI, agent-core), pointed at llama-swap directly.

Freeze, not delete: the code, tests, CI history and design notes stay as
reference.

## Open measurements (do not block the freeze)

These were not run before this decision. If a result contradicts the
decision, revisit it.

- Strata decode and prefill at about 2.5K, 70K and 122K tokens of context.
  llama.cpp on the same model drops from 26 to 20 tok/s at about 70K.
- Quality at depth with Strata's quantization.
- Whether a 70 s second start is typical, which would make swaps expensive.
- Total throughput at `--parallel 1/2/4` on the MoE model, which decides
  whether parallel swarm workers are worth it.

## What would bring it back

Revisit this ADR if any of these become true:

- A second GPU or second host makes placement a real choice.
- Two or more models that genuinely differ (for example vision and coding,
  or a separate planning model) are needed in one workflow and cannot be
  loaded together.
- Several users or concurrent workloads contend for the same GPU and a
  queue in llama-swap or the harness is not enough.
- Depth measurements show the single model falling near or below about
  20 tok/s at working context, so a mid-size tier pays for itself.

## Consequences

- Open issues #205–#212 and other feature issues get closed with a link to
  this ADR (separate, operator-approved step).
- The README gets a "frozen" status note (separate step; README is a
  protected file).
- Unfinished prompt 33 work (intent classification) is preserved on
  `issue/125-intent-classification` and not merged.
- Lessons that carry forward: deterministic, explained decisions; adapter
  boundaries ("runtimes execute, something else decides"); behavior-observing
  tests with restart round-trips.

## Lesson

Anemoi was a correct answer to a real constraint, and an engine change removed
the constraint. Record the constraint each system depends on, so its
disappearance is noticed instead of discovered after months of unused work.
