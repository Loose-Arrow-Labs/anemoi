# Governance & Roster Management

Operators curate Anemoi governance — which runtime models belong to which
domains, rosters, and residency groups — from the dashboard or the control-plane
API, without hand-editing YAML for every change. This document defines the
override semantics, persistence model, safety boundary, and API.

## Model

```
effective config  =  baseline YAML  +  operator overrides
```

- **Baseline** is the hand-authored `ANEMOI_CONFIG` YAML. It stays the
  reviewable source of truth.
- **Overrides** are Anemoi-owned edits (new/changed rosters, domain→roster
  assignments, removed rosters, per-model profile overrides).
- The **effective config** is the merge of the two, and it is what the real
  scheduler consumes. A roster edit takes effect on the **very next `/decide`** —
  the daemon rebuilds the effective config and its scheduler atomically on each
  edit.

This is the "hybrid" persistence model: YAML baseline + an Anemoi-owned overrides
file, effective = baseline + overrides. Anemoi never rewrites the baseline YAML.

## Runtime catalogs are read-only discovery input

llama-swap (and, later, ComfyUI / Pipecat) catalogs are **discovery input only**:

- Anemoi reads each runtime's configured/catalog model surface to populate the
  Model Catalog. **Configured/catalog membership is not residency evidence** — a
  model's `resident_state` is set only when it is *separately* observed loaded
  (via the `/api/events` stream); see
  [`live_validation/residency-truth-contract.md`](live_validation/residency-truth-contract.md).
- **Anemoi never writes a runtime's own config.** Governance edits change
  Anemoi-owned overrides only.

## Metadata precedence and provenance

For a model's effective profile, an operator override wins over baseline config,
which wins over values inferred from the model id:

```
operator_override  >  config  >  inferred
```

Every effective value carries a provenance label, surfaced in the catalog:

| `metadata_source` | Meaning |
|---|---|
| `operator_override` | Set explicitly by an operator; takes precedence. |
| `config` | Declared in the baseline Anemoi YAML. |
| `inferred` | Derived by Anemoi from the model id (family prefix, `NNb` size token). |
| `runtime` | Reported directly by the runtime catalog. |

## Persistence

Overrides persist to an Anemoi-owned JSON file named by `ANEMOI_GOVERNANCE_OVERRIDES`:

```powershell
$env:ANEMOI_GOVERNANCE_OVERRIDES = "C:\anemoi\governance-overrides.json"
```

- Writes are **atomic** (temp file + rename) so a crash mid-write never leaves a
  truncated file.
- On start-up the daemon loads the file if present; a malformed file is a hard
  start-up error (it is never silently ignored).
- When the variable is unset, edits are **in-memory only** and do not survive a
  restart.
- The file is plain JSON — diff it for an audit trail of who changed what.

## Safety boundary

- Dashboard/API governance edits are **policy/config edits only**. They
  **never** load, unload, stage, or otherwise mutate a runtime. Adding a model to
  a roster does not load it; removing it does not unload it.
- A domain that would be left with no rosters (and no `live_roster`) is rejected.
- Deleting a roster still referenced by a domain is rejected (409 Conflict).
- Empty rosters and domains that cannot escalate above 9B are surfaced as
  warnings (see Validation).
- Runtime auth tokens are never exposed through the governance surface
  (`/policy/effective-config` redacts them).

## Validation

`GET /policy/validate` returns advisory warnings over the effective config:

| Code | Meaning |
|---|---|
| `roster.empty` | A residency group has no models. |
| `model.missing_profile` | A roster model has no profile; policy will reject it. |
| `domain.no_roster` | A static-roster domain has no rosters. |
| `domain.unknown_roster` | A domain references an undefined residency group. |
| `domain.no_escalation` | A static-roster domain's largest model is ≤ 9B — it cannot escalate. This is the exact failure mode where `coding` was stuck on 9B. |

## API

All mutation endpoints update Anemoi-owned overrides only; none mutate a runtime.

| Method & path | Purpose |
|---|---|
| `GET /catalog/models` | All models discovered from runtime catalogs, labeled configured vs resident, with provenance and roster membership. |
| `PATCH /catalog/models/:id` | Set (or clear, when empty) an operator profile override for a model. |
| `GET /rosters` | List residency groups (rosters) with flags, models, and which domains use them. |
| `POST /rosters` | Create a roster. |
| `GET /rosters/:id` | One roster. |
| `PATCH /rosters/:id` | Update roster metadata, flags, or models. |
| `DELETE /rosters/:id` | Delete a roster (rejected while a domain references it). |
| `POST /rosters/:id/models` | Add a model to a roster (no runtime load). |
| `DELETE /rosters/:id/models/:model_id` | Remove a model from a roster (no runtime unload). |
| `GET /domains` | List domains with their rosters and any `live_roster`. |
| `PATCH /domains/:id/rosters` | Set a domain's roster assignment. |
| `GET /policy/effective-config` | The merged effective config the scheduler consumes (auth tokens redacted). |
| `GET /policy/validate` | Advisory governance warnings. |

## Dashboard

The dashboard's **Governance** tab provides a Model Catalog, roster editors
(create/delete, flags, add/remove models), domain→roster assignment, and a
validation panel. See [`DASHBOARD.md`](DASHBOARD.md).

## Future providers

The catalog is sourced from runtime snapshots, so any runtime adapter that
reports `configured_models` participates automatically. ComfyUI and Pipecat are
explicit future providers; they are not required for the initial llama-swap
implementation.
