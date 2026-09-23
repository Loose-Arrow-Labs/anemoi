# llama-swap Live Path — Validation Report

**Status:** validated (read-only inspection + decision + live-gate behavior)
**Date:** 2026-06-09
**Runtime:** llama-swap, real GPU host, reached over HTTP with bearer-token auth

This document records one real, end-to-end llama-swap live path for Anemoi:
load the sanitized config, connect to a live llama-swap, inspect health and
configured models, consume `/api/events` residency evidence, make and explain
read-only decisions for both a static-roster and a live-roster domain, and prove
the controlled-execution gate behaves correctly. All evidence below is captured
from a real run and sanitized (private hostnames, container IPs, deploy paths,
and the auth token are redacted; model ids and response structure are real).

Related contracts:

- [`residency-truth-contract.md`](residency-truth-contract.md) — why configured
  models are not residency evidence.
- [`controlled-execution-gate.md`](controlled-execution-gate.md) — the
  `ANEMOI_ENABLE_LIVE_EXECUTE` boundary.
- [`safety-plan.md`](safety-plan.md) — read-only vs controlled phases.

---

## 1. Environment

The daemon is configured entirely from the environment; no secrets are
committed. The sanitized config is [`config/anemoi.prometheus.yaml`](../../config/anemoi.prometheus.yaml).

| Variable | Value (sanitized) | Purpose |
|---|---|---|
| `ANEMOI_CONFIG` | `/config/anemoi.prometheus.yaml` | Sanitized config; declares `coding` (static roster) and `coding-full` (live roster). |
| `ANEMOI_BIND` | `0.0.0.0:7070` | Daemon listen address. |
| `ANEMOI_LLAMA_SWAP_BASE_URL` | `http://<llama-swap-host>:8085` | Live llama-swap base URL (deploy uses `host.docker.internal:8085`; the docker-bridge address `172.17.0.1:8085` is equivalent from the host). |
| `ANEMOI_LLAMA_SWAP_AUTH_TOKEN` | `<redacted>` | Bearer token. llama-swap **requires** auth (see §6). |
| `ANEMOI_ENABLE_LIVE_EXECUTE` | `1` on the production daemon; **unset** for the gate-disabled check in §5 | Live runtime mutation / non-mock forwarding opt-in. |

The config path is mounted **read-only** into the daemon container.

---

## 2. Daemon startup

The production daemon runs as a container from the project image (`anemoi:local`,
entrypoint `/usr/local/bin/anemoi-daemon`), config mounted read-only:

```bash
docker run -d --name anemoi \
  -v <deploy-dir>/config:/config:ro \
  -e ANEMOI_CONFIG=/config/anemoi.prometheus.yaml \
  -e ANEMOI_BIND=0.0.0.0:7070 \
  -e ANEMOI_LLAMA_SWAP_BASE_URL=http://<llama-swap-host>:8085 \
  -e ANEMOI_LLAMA_SWAP_AUTH_TOKEN=<redacted> \
  -e ANEMOI_ENABLE_LIVE_EXECUTE=1 \
  anemoi:local
```

Equivalent local run from a checkout:

```bash
ANEMOI_CONFIG=config/anemoi.prometheus.yaml \
ANEMOI_BIND=0.0.0.0:7070 \
ANEMOI_LLAMA_SWAP_BASE_URL=http://<llama-swap-host>:8085 \
ANEMOI_LLAMA_SWAP_AUTH_TOKEN=<redacted> \
cargo run -p anemoi-daemon
```

All probes below were run against the live daemon over its bound port
(`<anemoi-host>:7070`). Substitute your own reachable address.

---

## 3. Read-only inspection

Read-only `GET`s require no `ANEMOI_ENABLE_LIVE_EXECUTE`.

### `GET /health`

```console
$ curl -s <anemoi>:7070/health
{"ok":true}            # http 200
```

### `GET /status`

```console
$ curl -s <anemoi>:7070/status
```

```json
{
  "cache_populated": true,
  "runtimes": [
    {
      "runtime_id": "llama_swap",
      "adapter": "llama_swap",
      "availability": "available",
      "freshness": "fresh",
      "last_error": null,
      "active_request_count": 0,
      "residents": [
        {"model_id": "qwen3.6-35b-a3b-mtp", "state": "hot_gpu", "idle_secs": null}
      ]
    }
  ],
  "residency_groups": [
    {"group_id": "fast_coding", "keep_hot": false, "member_count": 2, "hot_resident_count": 0, "health": "healthy"}
  ],
  "active_request_count": 0,
  "staging": {"total": 0, "blocked": 0, "pending": 0, "failed": 0, "completed": 0},
  "recent_decision_count": 0,
  "policy_warnings": [],
  "live_execution_enabled": true
}
```

### `GET /residents`

The runtime reports **26 configured models** but only **one hot resident**
(`qwen3.6-35b-a3b-mtp`), proving Anemoi does not infer residency from
configuration — residency comes only from the push-updated `/api/events` stream.

```json
[
  {
    "runtime_id": "llama_swap",
    "available": true,
    "residents": [
      {"model_id": "qwen3.6-35b-a3b-mtp", "state": "hot_gpu",
       "vram_mb": null, "ram_mb": null, "kv_cache_mb": null, "loaded_since": null}
    ],
    "configured_models": [
      "gemma-4-26b-a4b-it-mtp", "gemma-4-26b-a4b-it-mtp-co", "gemma-4-31b-it",
      "gemma-4-31b-it-co", "gemma-4-e2b-it", "gemma-4-e2b-it-co", "gemma-4-e4b-it",
      "lfm2-5-350m-cpu", "lfm2-5-350m-gpu", "minimax-256k", "minimax-256k-co",
      "minimax-256k-iq3s", "minimax-256k-iq3s-co", "nemotron-udiq4-256k",
      "nemotron-udiq4-256k-co", "qwen3.5-122b-a10b-mtp", "qwen3.5-122b-a10b-mtp-co",
      "qwen3.5-2b-mtp", "qwen3.5-2b-mtp-co", "qwen3.5-4b-mtp", "qwen3.5-9b-mtp",
      "qwen3.6-27b-mtp", "qwen3.6-27b-mtp-co", "qwen3.6-35b-a3b-mtp",
      "qwen3.6-35b-a3b-mtp-co"
    ],
    "memory": {"vram_total_mb": null, "vram_used_mb": null, "ram_total_mb": null, "ram_used_mb": null},
    "active_requests": []
  }
]
```

> Note: `memory` is all `null` — llama-swap does not report VRAM/RAM totals over
> its API, so pressure scoring treats capacity as unknown rather than zero (see
> the `pressure.vram`/`pressure.ram` reasons in §4). This is a known limitation,
> not a fault.

### `GET /v1/models`

Anemoi advertises **domains** (not raw models) as the OpenAI-compatible model
list, so callers select a routing domain:

```json
{"object":"list","data":[
  {"id":"coding","object":"model","owned_by":"anemoi","anemoi_domain":true},
  {"id":"coding-full","object":"model","owned_by":"anemoi","anemoi_domain":true}
]}
```

---

## 4. Decision check — `POST /decide`

`/decide` is read-only: it computes and explains a decision without loading,
unloading, or forwarding anything. Both domains were exercised.

### `coding` (static roster — fast models only)

```console
$ curl -s -X POST <anemoi>:7070/decide -H 'Content-Type: application/json' \
    -d '{"domain":"coding","mode":"interactive","prompt_tokens_estimate":1000,
         "max_output_tokens":500,"latency_budget_ms":2000,"quality_floor":null}'
```

Result: `cold_load` of `qwen3.5-4b-mtp` via `llama_swap` (group `fast_coding`),
with a full scored explanation and empty `rejected_options`:

```json
{
  "action": "cold_load",
  "selected_model": "qwen3.5-4b-mtp",
  "selected_runtime": "llama_swap",
  "selected_group": "fast_coding",
  "background_model": null,
  "score": {"total": -22, "contributions": [
    {"label":"quality","value":4}, {"label":"residency","value":0},
    {"label":"load_penalty","value":-20}, {"label":"latency_budget","value":-36},
    {"label":"context_window.fit","value":10}, {"label":"pressure.vram","value":0},
    {"label":"pressure.ram","value":0}, {"label":"pressure.active_requests","value":0},
    {"label":"continuity","value":20}, {"label":"streaming_capability","value":0}
  ]},
  "explanation": {
    "summary": "Selected qwen3.5-4b-mtp via llama_swap with action ColdLoad.",
    "reasons": [
      {"code":"context_window.fit","detail":"request requires 1500 token(s) and qwen3.5-4b-mtp provides a 262144 token context window","impact":10},
      {"code":"pressure.vram","detail":"vram capacity is unknown; treating residency as unproven and assigning no pressure credit","impact":0},
      {"code":"continuity","detail":"qwen3.5-4b-mtp belongs to a continuity-friendly residency group","impact":20}
    ],
    "rejected_options": []
  }
}
```

The hot `qwen3.6-35b-a3b-mtp` is **not** a `coding` candidate — that domain's
roster is intentionally constrained to fast interactive models, so the bigger
hot model is out of scope here (it belongs to `coding-full`).

### `coding-full` (live roster — full runtime catalog)

```console
$ curl -s -X POST <anemoi>:7070/decide -H 'Content-Type: application/json' \
    -d '{"domain":"coding-full","mode":"interactive","prompt_tokens_estimate":1000,
         "max_output_tokens":500,"latency_budget_ms":60000,"quality_floor":null}'
```

Result: `reuse_hot` of the already-hot `qwen3.6-35b-a3b-mtp` (group `live`),
score `125` (quality 35 + residency 60 + latency 10 + continuity 20):

```json
{
  "action": "reuse_hot",
  "selected_model": "qwen3.6-35b-a3b-mtp",
  "selected_runtime": "llama_swap",
  "selected_group": "live",
  "score": {"total": 125, "contributions": [
    {"label":"quality","value":35}, {"label":"residency","value":60},
    {"label":"latency_budget","value":10}, {"label":"continuity","value":20}
  ]}
}
```

This exercises the live-roster path: candidates are synthesized from the
runtime's `configured_models` snapshot, and the hot model wins on its residency
bonus — exactly the reuse-what-is-hot behavior we want.

---

## 5. Controlled-execution gate — `POST /v1/chat/completions`

Non-mock forwarding requires `ANEMOI_ENABLE_LIVE_EXECUTE=1`.

The **production** daemon runs with the gate **open** (`live_execution_enabled:
true`) as a deliberate operator opt-in for the daily driver, so sending a chat
completion there would forward a **real** inference request. To validate the
safety boundary without putting traffic through the open production gate, the
**disabled**-gate behavior was captured on a throwaway instance of the same
image and config with `ANEMOI_ENABLE_LIVE_EXECUTE` **unset**, then removed.

```console
# throwaway instance, gate unset
$ curl -s <gatecheck>:7070/status | grep -o '"live_execution_enabled":[a-z]*'
"live_execution_enabled":false

$ curl -s -i -X POST <gatecheck>:7070/v1/chat/completions \
    -H 'Content-Type: application/json' \
    -d '{"model":"coding","messages":[{"role":"user","content":"what is 2+2?"}],"max_tokens":16}'
HTTP/1.1 403 Forbidden
x-anemoi-decision-id: <uuid>
content-type: application/json

{"error":{
  "decision_id":"<uuid>",
  "message":"forwarding to a non-mock runtime requires ANEMOI_ENABLE_LIVE_EXECUTE=1",
  "type":"anemoi_gateway_error"
}}
```

The request is rejected **before any forward** — no inference reaches llama-swap.
A decision is still made and recorded, and its id is returned both in the body
and the `x-anemoi-decision-id` header so the caller can query `/explain/<id>`.

> The throwaway instance also cannot mutate the live runtime: with the gate
> unset, the daemon refuses load/unload/forward against a non-mock adapter, so
> the gate-disabled check is itself read-only with respect to llama-swap.

---

## 6. Expected headers and auth

**llama-swap auth.** llama-swap requires a bearer token; Anemoi sends
`Authorization: Bearer $ANEMOI_LLAMA_SWAP_AUTH_TOKEN`. Verified directly:

```console
$ curl -s -o /dev/null -w '%{http_code}\n' <llama-swap>:8085/v1/models
401
$ curl -s -o /dev/null -w '%{http_code}\n' -H 'Authorization: Bearer <redacted>' <llama-swap>:8085/v1/models
200
```

**Anemoi gateway response headers** (`/v1/chat/completions`):

| Header | When | Meaning |
|---|---|---|
| `x-anemoi-decision-id` | on success, and on gateway errors that carry a decision (incl. the 403 above) | The decision id; query `/explain/<id>` for the full explanation. |
| `x-anemoi-selected-model` | on a successful forward | The model the request was routed to. |

---

## 7. Telemetry / dashboard (#130)

Decisions are recorded in the decision log/event store and surfaced by the
telemetry endpoints and the operator dashboard added in #130. `/status` exposes
live counters used by the dashboard — `recent_decision_count`, per-runtime
`availability`/`freshness`/`residents`, `residency_groups` health, `staging`
totals, and `live_execution_enabled`. A `/decide` or gated `/v1/chat/completions`
call produces a decision id (see §5) that resolves via `/explain/<id>` and
appears in the dashboard's decision view.

---

## 8. Known limitations / Needs validation

- **Memory pressure is blind.** llama-swap reports no VRAM/RAM totals, so
  `memory` is all `null` and pressure scoring treats capacity as unknown. VRAM
  headroom does not influence decisions on this runtime.
- **Live forward not exercised end-to-end here.** The successful-forward path
  (gate open → real completion, `x-anemoi-selected-model` header) was **not**
  driven in this report to avoid sending traffic through the production gate.
  *Needs validation:* one gated live completion with sanitized response headers,
  run deliberately against a non-production instance.
- **Colocation not yet consulted (#112).** llama-swap's `-co` colocation matrix
  is parsed but, on `main`, not yet consulted by `decide()` when planning
  co-resident loadouts. Wiring is tracked in #112; this report does not depend
  on it.
- **Residency `loaded_since`/sizing is null.** The event stream reports state
  but not load timestamps or per-model memory, so idle-time and sizing-based
  policy inputs are unavailable on this runtime.

---

## 9. Rollback / stop conditions

- Read-only probes (`/health`, `/status`, `/residents`, `/v1/models`, `/decide`)
  make no runtime changes and need no rollback.
- The gate-disabled check runs a throwaway container that cannot mutate the
  runtime; remove it with `docker rm -f <name>` (done — confirmed absent).
- **Stop immediately** if: llama-swap returns sustained non-200 on `/health`;
  `/status` shows a runtime stuck `unavailable`/`stale` with a `last_error`;
  residents report a model `hot_gpu` that the event stream did not actually
  observe (residency-truth violation); or any probe would require enabling live
  execute without explicit operator approval.
- To disable live forwarding on the production daemon, unset
  `ANEMOI_ENABLE_LIVE_EXECUTE` and restart — the gate then rejects all non-mock
  forwards (§5).
