# Field Evidence: Hermes Agent Model Routing on a Single-GPU llama-swap Rig

## Provenance

This is **third-party field evidence, not an Anemoi validation run.** The
measurements below were produced by the Hermes agent harness driving a live
`llama-swap` instance on the operator's workstation on 2026-08-19, while that
agent tried to configure its own auxiliary-task model routing. Anemoi was not in
the path.

It is recorded here because the rig is the same class of target Anemoi governs
(single GPU, one large resident model, a matrix solver, many small models), and
because the agent's failures map onto specific gaps in Anemoi's current
scheduling model.

Treat every number as **observed on that host, that day**. Nothing here has been
reproduced through Anemoi. Items that could not be established are marked
`Needs validation`.

## The Rig

| Property | Value |
|---|---|
| GPU | RTX 4000 Ada, 20 GB VRAM |
| System RAM | 94–96 GB |
| CPU | 56 threads, dual socket |
| Runtime | `llama-swap` with a `matrix` colocation solver |
| Resident interactive model | `qwen3.8-27b-think`, ≈18 GB VRAM |
| Free VRAM headroom | ≈2.5 GB |

The harness exposes ~14 auxiliary task slots (title generation, approval,
context compaction, MCP tool routing, skill search, web extract, curator,
vision, …), each independently assignable to a model. That fan-out is the
routing problem: one interactive agent, many differently-shaped side tasks, one
GPU.

## What Was Measured

### 1. Residency dominates every other cost

Identical 40-token completion, probed across the routing decision space:

| Model | HTTP | Wall | Resident at probe time |
|---|---|---|---|
| `qwen3.8-27b-think` (main) | 200 | **8.4 s** | yes |
| `qwen3.5-2b-mtp` | 200 | 84 s | no |
| `qwen3.5-9b-mtp` | 200 | 116 s | no |
| `qwen3.5-4b-mtp` | 200 | 192 s | no |
| `qwen3.6-35b-a3b-mtp(-co)` | 200 | 140–164 s | no |
| others | 200 | 110–122 s | no |

A 2B model cost **10× more wall time than a 27B model** for the same answer,
purely because the 27B was resident and the 2B was not. Parameter count did not
order the results; residency did.

### 2. The incumbent survived; the auxiliary never stayed warm

Co-residency probe (`coreres2.py`), verbatim from the run:

```
=== baseline: 27B hot ===
  [27B] qwen3.8-27b-think: 157.7s
  [27B] qwen3.8-27b-think: 0.6s
  baseline 27B = 0.6s

=== qwen3.6-35b-a3b-mtp-co ===
  [call1(cold)] 8.9s
  [call2(hot?)] 365.0s
  [27B] qwen3.8-27b-think: 7.4s
  -> aux stayed hot: False | 27B still hot: True

=== qwen3.5-2b-mtp-co ===
  [call1(cold)] 208.0s
  [call2(hot?)] 513.7s
  [27B] qwen3.8-27b-think: 7.1s
  -> aux stayed hot: False | 27B still hot: True
```

An earlier run added `lfm2-5-350m-gpu` and `lfm2-5-350m-cpu`: both left the 27B
hot (7.1–7.2 s) and both failed to stay warm themselves (≈142 s on the second
call).

Two facts fall out, and they point in opposite directions:

- **Safety holds.** Nothing the harness routed to displaced the interactive
  model. The 27B answered in 7–8 s after every probe.
- **Auxiliary warmth does not hold.** Every auxiliary model paid a full load on
  its *second* call, 40–60× its first.

### 3. The second-call anomaly is mutual eviction, not slow inference

A 365 s second call to a model that answered in 8.9 s on its first call is not a
generation-speed result. Between the two auxiliary calls, the *main agent
generated a turn* — reloading the 27B and displacing the auxiliary model. The
auxiliary then reloaded from cold on call 2.

This is the load-bearing insight for Anemoi:

> On a single-resident rig, routing a **high-frequency** auxiliary task to a
> model **other than the incumbent** is net-negative regardless of how small that
> model is. Main-agent turns and auxiliary calls interleave, so each evicts the
> other. The auxiliary never amortizes its load, and the interactive path pays
> for the churn.

The corollary decides the routing policy: **frequency, not size, selects the
tier.** High-frequency short tasks belong on the incumbent model (zero
displacement) or on a genuinely non-contending CPU-resident model. Low-frequency
heavy tasks can afford a cold load because they fire rarely enough to amortize
it.

### 4. `-co` in a model name is not evidence of co-residency

The harness spent most of its reasoning budget trying to infer whether the
`-co`-suffixed models genuinely co-reside. The live matrix settles it:

```yaml
matrix:
  evict_costs:
    q122: 50   # 122B slow cold start
    g4v: 50    # 200B+ slow cold start
    mx8..mx12: 60  # 230B slowest
    n2..n3: 40     # 120B slow
    q27: 10    # moderate
    q35: 5     # fast
    g8: 1      # fastest (CPU, instant load)
    q9: 1
  sets:
    colocated: "(g26 | g31 | q122 | g4v | gf4 | gr30 | n2 | n3 | mx8 | mx9 | mx12 | q35 | q35m | q27 | q27m) & (g8 & q9)"
```

The expression is `(GPU-primary alternatives) & (CPU agents)`. Every GPU model is
in an `|` group — **one at a time**. Only the CPU agents (`--n-gpu-layers 0`) are
`&`-joined and therefore genuinely concurrent. The arithmetic agrees: an 18 GB
resident model leaves ≈2.5 GB, which cannot hold a second GPU model of any
useful size.

So co-residency on this rig is not a property of a model, and not a naming
convention. It is a property of **whether the model contends for VRAM at all.**

### 5. Static load estimates had rotted, and the model list had drifted

The operator's benchmark document (validated 2026-05-20) listed seven models no
longer served — `granite-4.1-8b`, `qwen3.5-9b`, `glm-4.6v`, `glm-4.7-flash`,
`granite-4.1-30b`, `nemotron-udq2/3`, `minimax-80/96/128k` — including **both
CPU agents the matrix depends on** and the only vision model. The live
`/v1/models` had gained `lfm2-5-350m-{cpu,gpu}` and a family of `-mtp` variants
the document never mentioned.

The harness planned its entire routing strategy against the stale document
before probing the live endpoint.

### 6. An agent cannot benchmark the rig it is running on

The harness could not measure auxiliary warm latency, because every measurement
turn it took *was* a generation on the main model, which evicted the model under
test. It said so explicitly, then reasoned in circles for several thousand
tokens before conceding the measurement was unobtainable from inside.

This is a direct argument for Anemoi's shape: residency truth requires an
observer that is not itself consuming the resource.

## What This Means for Anemoi

Mapped against the current implementation.

### Gap 1 — Displacement cost is not scored

`score_candidate` (`crates/anemoi-policy/src/lib.rs`) scores a candidate in
isolation: `quality` + `residency` reuse bonus + `load_penalty` +
`latency_budget` + `context_window.fit` + pressure + `continuity`.

Every term is about **what this request pays**. None is about **what this request
costs the incumbent.** Selecting a cold 2B model while a hot 27B is resident
scores well locally (small `load_penalty`, low pressure) and is exactly the
choice the field evidence shows to be wrong: it displaces the interactive model
and the next interactive turn pays 157 s.

`ResidencyState::reuse_bonus` rewards *reusing* what is hot. Nothing penalizes
*displacing* what is hot.

### Gap 2 — `evict_costs` is parsed and then dropped

`LlamaSwapMatrixConfig` (`crates/anemoi-runtime/src/lib.rs`) parses
`evict_costs` and retains it on the adapter. But `colocation_constraints()`
lowers only `loadouts` into `ColocationConstraints`, which is the sole colocation
channel into `RuntimeSnapshot`. The weights stop at the adapter boundary and
never reach policy.

llama-swap's own solver picks "the valid set containing X with the fewest
evictions, weighted by `evict_costs`". Anemoi currently governs a runtime whose
eviction economics it cannot see.

Related: the doc comment on `evict_costs` calls the values "cold-load cost
estimates in milliseconds". The live config uses ordinal weights — `1`, `5`,
`10`, `40`, `50`, `60`, with comments like `# fastest (CPU, instant load)`.
Reading them as milliseconds would be wrong by four orders of magnitude.

### Gap 3 — No representation of a non-contending model

Anemoi has `ResidencyState::WarmCpu`, an observed *state*. It has no
`ModelProfile` notion of a model that **never competes for VRAM** — the
`--n-gpu-layers 0` class that the matrix `&`-joins and that is the only thing on
this rig that is genuinely concurrent with the resident GPU model.

Without it, policy cannot distinguish "small, so cheap to load" (wrong: the 2B
cost 84 s) from "CPU-resident, so free to call" (right, and the actual design
intent of the operator's matrix).

`ColocationConstraints::can_colocate` answers *feasibility* — may these two be
resident together. The field evidence shows feasibility is necessary but not
sufficient: the pair also has to not thrash, which is a function of VRAM
headroom and call frequency, neither of which the loadout expression carries.

### Gap 4 — `cold_load_estimate_ms` is hand-authored and rots

`config/anemoi.llama-swap.example.yaml` declares `cold_load_estimate_ms: 18000`
for a 9B and `45000` for a 35B. Measured on the live rig: 116 s and 140–164 s —
2.6–9× the configured estimates. The `load_penalty` term is
`-(load_estimate_ms / 1000)`, so a stale estimate under-penalizes cold loads by
exactly that factor.

Anemoi already observes load transitions through the reconciliation loop and the
llama-swap `/api/events` SSE stream (prompt 29). Nothing feeds observed load
duration back into the estimate.

### Gap 5 — Call frequency is not a request attribute

`InferenceRequest` carries `domain`, `mode`, token estimates, `latency_budget_ms`,
`quality_floor`, `escalation_intent`. `ExecutionMode` distinguishes
`Interactive` / `Batch` / `Background`.

The 14 auxiliary slots in the harness are all `Background`, and they differ from
each other by two orders of magnitude in call rate — title generation fires on
nearly every turn, context compaction fires rarely. The evidence says that
difference, not their shared `Background` mode, decides whether they may be
routed off the incumbent.

`ExecutionMode::Background` is currently too coarse to express the distinction
the hardware enforces.

### Confirmed by evidence (no change needed)

- **The residency-truth contract (prompt 18) is right.** Seven models the
  operator's document claimed were gone from `/v1/models`, and models were served
  that the document never listed. Anemoi's split between `configured_models` and
  observed `residents` is exactly the discipline that would have caught this.
- **Out-of-band observation is right.** The harness demonstrated, at length, that
  an agent cannot measure the residency of the runtime it is generating on.

## Open Questions

- `Needs validation` — Whether a `--n-gpu-layers 0` model on this rig is
  genuinely concurrent under load. The matrix declares it; the run could not
  isolate it, and `lfm2-5-350m-cpu` measured 142 s warm, which is not consistent
  with a 350M model running on 56 threads. Either it was not actually CPU-hosted,
  or it was evicted between calls by main-model activity.
- `Needs validation` — Whether the `-co` profiles differ from their base
  profiles by quantization, context length, or `--cache-ram` KV placement. The
  `8.9 s` first call to `qwen3.6-35b-a3b-mtp-co` is unexplained by cold-load
  arithmetic for a 35B model and suggests warm page cache rather than true
  co-residency.
- `Unknown` — The live matrix on the current server. The block quoted above is
  from the operator's 2026-05-20 document, and the model list around it has
  demonstrably drifted since. The harness could not read the live config.
- `Unknown` — Whether displacement cost should be scored symmetrically (penalize
  evicting anything hot) or asymmetrically (penalize evicting a `keep_hot` /
  `pinned` group harder). Prompt 25 established eviction and pinning policy;
  the interaction is undesigned.

## Derived Work

Build prompt `32-displacement-aware-routing.md` covers gaps 1 and 2 —
displacement cost as a scored term, fed by the runtime's declared eviction
weights. It is scoped to one reviewable change.

Gaps 3 (a non-contending CPU-resident model class), 4 (observed load feedback
replacing static `cold_load_estimate_ms`), and 5 (call frequency as a request
attribute finer than `ExecutionMode::Background`) are recorded here and not yet
scoped into prompts.
