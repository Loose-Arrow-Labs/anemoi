# Prompt 31 — Concurrent residency transition coordination

Source issue: **#79 — Handle concurrent residency transition collisions across
Anemoi instances.**

## Scope

Add a control-plane policy concept that arbitrates collisions when concurrent
requests — possibly from multiple Anemoi instances or multiple request streams —
want incompatible residency states for the same runtime. This is
scheduling/control-plane policy, **not** model execution: the coordinator decides
and explains, it never loads a model or mutates a runtime (that stays behind the
runtime-adapter boundary, per `AGENTS.md` §2/§4/§9).

Lives in `anemoi-policy` (`transition` module). Uses the established residency
vocabulary and reuses `anemoi-core`'s `DecisionReason` / `RejectedOption` for
explanations — no new residency state names are invented.

## Behavior

A `TransitionCoordinator` tracks at most one `ActiveTransition` per runtime. Each
active transition records `owner_instance`, a monotonic `fencing_token`, a
`lease_expires_at_ms`, and an active `serving_leases` count. `request_transition`
is pure given an explicit `now_ms` (no wall-clock) and returns a
`TransitionDecision { path, reasons, rejected }`:

- **Same model in flight** → `Joined` (reuse, no duplicate load).
- **No active transition** → `Started` (interactive) or `Staged` (background); the
  requester becomes the fenced owner.
- **Conflicting model, interactive request with a compatible hot worker** →
  `ServedHot` (continuity fallback; the in-flight load is left undisturbed).
- **Conflicting model, background stage** → `Rejected` (a background stage must
  not displace an active transition lease).
- **Conflicting model, otherwise** → deterministic winner on policy inputs
  (priority, then interactive over background, then a stable instance-id tiebreak);
  the winner `TookOver`, the loser is `Rejected`. A transition with serving leases
  is protected from preemption.
- **Expired owner lease** → `TookOver` with a new (advanced) fencing token.
- **Runtime unavailable** → `RuntimeUnavailable`.

Multi-instance ownership is **code-enforced**: `complete(runtime, token)` accepts
only the current fencing token, so a stale owner whose lease was taken over cannot
clobber the new owner's transition.

## Required Tests

These exact names must appear in `cargo test --workspace` output
(`anemoi-policy`, module `transition`):

- `same_model_request_joins_existing_transition`
- `conflicting_models_pick_deterministic_winner`
- `interactive_collision_falls_back_to_hot_worker`
- `background_stage_allowed_when_no_active_transition`
- `background_stage_blocked_by_active_conflicting_transition`
- `expired_owner_lease_is_taken_over_with_new_fencing_token`
- `stale_owner_cannot_complete_after_takeover`
- `serving_lease_protects_active_transition_from_preemption`
- `unavailable_runtime_request_is_rejected_deterministically`
- `every_transition_decision_carries_explanation`

## Out of scope (follow-ups)

- Wiring the coordinator into the daemon's `/decide` and reconciliation paths so
  live concurrent request streams are arbitrated through it end-to-end.
- Cross-process shared lease storage. The lease/fencing **mechanism** is enforced
  in code here and validated with simulated instance ids; durable cross-process
  sharing of the lease table is a separate integration.
- Model execution, provider gateway behavior, runtime-specific live mutation.
