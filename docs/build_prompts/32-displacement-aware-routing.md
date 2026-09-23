# Prompt 32: Displacement-Aware Routing

## Goal

Make the cost a candidate imposes on the **incumbent resident** an explicit,
scored, explained term, so Anemoi stops preferring a cold small model that
displaces a hot large one on single-resident rigs.

## Motivation

`docs/live_validation/hermes-routing-field-evidence.md` records a live
single-GPU llama-swap rig where a cold 2B model cost **84 s** for a completion
the resident 27B answered in **8.4 s**, and where a background task routed off
the incumbent made every subsequent call — auxiliary *and* interactive — pay a
full reload. Residency, not parameter count, ordered every result.

`score_candidate` currently scores a candidate only by what *that request* pays:
`quality`, `residency` reuse bonus, `load_penalty`, `latency_budget`,
`context_window.fit`, pressure, `continuity`. No term represents what the
request costs whatever is already hot.

Separately, `LlamaSwapMatrixConfig::evict_costs` is parsed and retained on the
adapter but never lowered into `ColocationConstraints`, so llama-swap's own
eviction weights — the runtime's stated opinion about which models are expensive
to displace — never reach policy.

## Issue

`Unknown` — no tracking issue filed yet.

## Scope

Allowed:

- add a `displacement` score contribution for candidates whose selection would
  evict a currently-resident model
- carry the runtime's declared per-model eviction weights through
  `ColocationConstraints` into the observed `RuntimeSnapshot`
- weight displacement harder for `keep_hot` and `pinned` groups, consistent with
  the prompt 25 eviction and pinning policy
- name the displaced model and the displacement impact in the explanation
- correct the `evict_costs` doc comment: live llama-swap configs use ordinal
  weights (`1`, `5`, `10`, `40`, `50`, `60`), not milliseconds

Not allowed:

- re-implement llama-swap's matrix solver inside Anemoi
- infer displacement from model naming conventions (`-co` suffixes are not
  evidence — see the field evidence document)
- make displacement scoring probabilistic
- mutate runtime state to measure displacement

Deferred to later prompts:

- observed cold-load feedback replacing static `cold_load_estimate_ms` (gap 4)
- a non-contending / CPU-resident model class (gap 3)
- call-frequency as a request attribute finer than `ExecutionMode::Background`
  (gap 5)

## Required Tests

Add failing tests first:

- `displacement_penalizes_candidate_that_evicts_hot_resident`
- `displacement_is_zero_when_candidate_is_the_hot_resident`
- `displacement_is_zero_when_runtime_reports_no_residents`
- `displacement_uses_runtime_declared_eviction_weight_when_present`
- `displacement_falls_back_to_default_weight_without_runtime_weights`
- `displacement_penalty_is_higher_for_keep_hot_group_resident`
- `displacement_explanation_names_the_displaced_model`
- `cold_small_model_loses_to_hot_large_model_under_displacement_scoring`
- `colocation_constraints_carry_runtime_eviction_weights`
- `colocatable_candidate_receives_no_displacement_penalty`

## Acceptance Criteria

- A candidate that would evict a resident model receives a negative
  `displacement` contribution recorded in `DecisionScore::contributions`.
- A candidate that *is* the resident model, or that the runtime's colocation
  constraints permit alongside the resident, receives no displacement penalty.
- Eviction weights declared by the runtime reach policy through
  `ColocationConstraints`; absent weights fall back to a documented default
  rather than to zero.
- Displacing a `keep_hot` or `pinned` group's resident costs strictly more than
  displacing an unpinned one.
- The explanation for a displacing selection names the model being displaced.
- On the field-evidence shape — hot large resident, cold small alternative, no
  quality floor — the hot resident wins.

## Validation

```powershell
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p anemoi-guard -- crates
```
