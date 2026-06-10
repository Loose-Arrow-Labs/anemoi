# Anemoi Inference Gateway Guide

The Anemoi Inference Gateway (added in issue #34) provides an OpenAI-compatible endpoint that intelligently selects which model to use for each request.

## What is the Inference Gateway?

Instead of:
```json
{
  "model": "qwen3.6-35b-a3b-mtp",
  "messages": [...]
}
```

You can send:
```json
{
  "model": "coding",
  "messages": [...]
}
```

And Anemoi will:
1. **Decide** which model to use (qwen3.6-35b, qwen3.5-9b, gemma-4, etc.)
2. **Forward** your request to that model
3. **Stream** the response back with decision telemetry

---

## Available Endpoints

### List Models

**Endpoint**: `GET /v1/models`

**Request**:
```bash
curl http://anemoi.home.arpa/v1/models
```

**Response**:
```json
{
  "object": "list",
  "data": [
    {
      "id": "coding",
      "object": "model",
      "owned_by": "anemoi",
      "anemoi_domain": true
    }
  ]
}
```

The `anemoi_domain` flag indicates this is a governance domain (not a raw model).

### Chat Completions

**Endpoint**: `POST /v1/chat/completions`

**Request**:
```bash
curl -X POST http://anemoi.home.arpa/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coding",
    "messages": [
      {"role": "user", "content": "What is 2+2?"}
    ],
    "max_tokens": 100,
    "stream": false
  }'
```

**Response** (non-streaming):
```json
{
  "id": "chatcmpl-ABCD1234...",
  "object": "chat.completion",
  "created": 1780090362,
  "model": "qwen3.6-35b-a3b-mtp",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "2 + 2 = 4"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 17,
    "completion_tokens": 11,
    "total_tokens": 28
  }
}
```

**Response Headers**:
```
X-Anemoi-Decision-Id: d-abc123xyz
X-Anemoi-Selected-Model: qwen3.6-35b-a3b-mtp
X-Anemoi-Action: forward-to-runtime
```

**Streaming** (SSE format):
```bash
curl -X POST http://anemoi.home.arpa/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coding",
    "messages": [{"role": "user", "content": "hi"}],
    "stream": true
  }'
```

Response streams as SSE:
```
data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}
data: {"choices":[{"index":0,"delta":{"content":"!"},"finish_reason":null}]}
data: [DONE]
```

---

## How It Works: The Decision Engine

### Step 1: Request Arrives

```json
{
  "model": "coding",
  "messages": [{"role": "user", "content": "..."}],
  "max_tokens": 100
}
```

### Step 2: Anemoi Inspects Runtime

Anemoi checks the current state:
```
Runtime State:
  qwen3.6-35b-a3b-mtp: hot_gpu (GPU loaded, 65% VRAM)
  qwen3.5-9b: hot_gpu (GPU loaded, 30% VRAM)  
  gemma-4-26b-a4b-it: loading (being loaded now)
  
Active Requests:
  qwen3.6-35b: 2 requests
  qwen3.5-9b: 0 requests
```

### Step 3: Score Candidates

For each available model:
```
Score = policy_weight * (
  resource_pressure +
  latency_cost +
  capability_match
)

qwen3.6-35b-a3b-mtp:
  - VRAM pressure: 65% (acceptable)
  - Active requests: 2 (busy but manageable)
  - Capability: Excellent for code (+10 points)
  - Score: 85

qwen3.5-9b:
  - VRAM pressure: 30% (plenty available)
  - Active requests: 0 (idle)
  - Capability: Good for code (+5 points)  
  - Score: 78

gemma-4-26b-a4b-it:
  - VRAM pressure: Still loading...
  - Score: 0 (not available yet)
```

### Step 4: Select Winner

qwen3.6-35b-a3b-mtp wins with score 85.

### Step 5: Forward Request

```
Original:  {"model": "coding", ...}
Rewritten: {"model": "qwen3.6-35b-a3b-mtp", ...}
Auth: Inject llama-swap API key
Forward to: http://llama-swap.home.arpa:8000/v1/chat/completions
```

### Step 6: Stream Response

Response streams back with telemetry headers:
```
X-Anemoi-Decision-Id: d-abc123xyz  (for audit trail)
X-Anemoi-Selected-Model: qwen3.6-35b-a3b-mtp  (what was selected)
X-Anemoi-Action: forward-to-runtime  (what anemoi did)
```

### Step 7: Record Decision

Anemoi logs to SQLite:
```
Decision ID: d-abc123xyz
Domain: coding
Selected Model: qwen3.6-35b-a3b-mtp
Selected At: 2026-05-30T14:23:45.123Z
Latency: 47ms
Runtime: remote
Status: success
```

---

## Request Parameters

The gateway supports all standard OpenAI parameters:

| Parameter | Type | Default | Notes |
|-----------|------|---------|-------|
| `model` | string | required | Governance domain (e.g., "coding") |
| `messages` | array | required | Chat messages |
| `temperature` | float | 0.7 | Randomness (0.0-2.0) |
| `top_p` | float | 0.9 | Nucleus sampling |
| `max_tokens` | integer | unlimited | Output length limit |
| `stream` | boolean | false | Server-sent events format |
| `presence_penalty` | float | 0 | Penalize repeated tokens |
| `frequency_penalty` | float | 0 | Penalize frequent tokens |

**Note**: Not all parameters are supported by all underlying runtimes. Unsupported parameters are silently ignored.

### Anemoi Selection Metadata

Clients can include a private `anemoi` object to describe scheduling needs. The
gateway uses this metadata for the Anemoi decision and strips it before
forwarding the request to the selected runtime.

```json
{
  "model": "coding",
  "messages": [{"role": "user", "content": "Plan this migration"}],
  "max_tokens": 1024,
  "anemoi": {
    "quality_floor": { "minimum_parameter_class": "32b" },
    "latency_budget_ms": 2000,
    "escalation_intent": {
      "task_type": "planning",
      "context": "small model handled setup; escalate for architecture work"
    }
  }
}
```

`quality_floor.minimum_parameter_class` is a hard floor for normal selection.
For example, a `32b` request cannot be silently satisfied by a `9b` model. If a
qualifying larger model exists but would cold-load beyond the latency budget,
continuity policy may select a hot smaller worker immediately while staging the
qualifying model in the background. If no qualifying model is configured for the
domain, Anemoi denies the request instead of pretending the smaller model met
the floor.

Live background staging still respects the daemon safety gate:
`ANEMOI_ENABLE_LIVE_EXECUTE=1` is required before Anemoi asks a runtime to load
models.

---

## Response Telemetry

Every response includes headers with decision info:

### X-Anemoi-Decision-Id
Unique identifier for this decision. Use to query the event store:

```bash
sqlite3 anemoi-events.db \
  "SELECT * FROM decisions WHERE id = 'd-abc123xyz';"
```

### X-Anemoi-Selected-Model
The actual model that was selected and ran your request.

Examples:
- `qwen3.6-35b-a3b-mtp`
- `qwen3.5-9b`
- `gemma-4-26b-a4b-it`

### X-Anemoi-Action
What anemoi did with your request:

| Action | Meaning |
|--------|---------|
| `forward-to-runtime` | Request forwarded to actual runtime (live) |
| `mock-forward` | Request simulated (testing mode) |
| `decision-only` | Decision made but not executed |

---

## Error Handling

### Invalid Domain

**Request**:
```json
{"model": "unknown-domain", "messages": [...]}
```

**Response** (400 Bad Request):
```json
{
  "error": {
    "message": "Unknown domain: unknown-domain. Available: coding",
    "type": "invalid_request_error",
    "param": "model",
    "code": "invalid_value"
  }
}
```

### No Available Models

**Request**: When all models are unavailable/loading

**Response** (503 Service Unavailable):
```json
{
  "error": {
    "message": "No models available for domain: coding. All are loading or unavailable.",
    "type": "server_error",
    "code": "no_models_available"
  }
}
```

### Runtime Failure

**Request**: When the selected runtime fails

**Response** (502 Bad Gateway):
```json
{
  "error": {
    "message": "Runtime error: Failed to reach llama-swap",
    "type": "server_error",
    "code": "runtime_unavailable"
  },
  "anemoi_decision_id": "d-abc123xyz"
}
```

---

## Examples

### Example 1: Simple Code Generation

```bash
curl -X POST http://anemoi.home.arpa/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coding",
    "messages": [
      {
        "role": "user",
        "content": "Write a Python function to calculate factorial"
      }
    ],
    "max_tokens": 200
  }' | python -m json.tool
```

**What Happens**:
1. Anemoi checks: "Is this a code task? Yes → coding domain"
2. Inspects runtime: "Both qwen3.6 and qwen3.5 are available"
3. Scores: qwen3.6 is better for code (more capable)
4. Forwards request to qwen3.6-35b-a3b-mtp
5. Streams back: Python code for factorial function
6. Headers show: Selected qwen3.6-35b-a3b-mtp

### Example 2: Streaming Response

```bash
curl -X POST http://anemoi.home.arpa/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coding",
    "messages": [{"role": "user", "content": "count to 5"}],
    "stream": true
  }' -N | while read -r line; do
    echo "$line"
  done
```

**Output** (SSE stream):
```
data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"1"}}]}
data: {"choices":[{"index":0,"delta":{"content":"\n2"}}]}
data: {"choices":[{"index":0,"delta":{"content":"\n3"}}]}
...
data: [DONE]
```

### Example 3: Check Decision in Database

```bash
# Get decision ID from response headers
DECISION_ID="d-abc123xyz"

# Query what was recorded
sqlite3 anemoi-events.db \
  "SELECT domain, model, latency_ms, created_at FROM decisions WHERE id = '$DECISION_ID';"

# Output:
# coding|qwen3.6-35b-a3b-mtp|47|2026-05-30 14:23:45
```

---

## Comparison: Before vs After

### Before Anemoi (Direct Model Selection)

```
User: Select "qwen3.6-35b-a3b-mtp"
      ↓
Request: model=qwen3.6-35b-a3b-mtp
      ↓
If model is busy or out of memory: ERROR
      ↓
User must manually select a different model
```

**Problems**:
- ❌ Deterministic (same model every time)
- ❌ May fail if that model is busy
- ❌ Requires manual model management
- ❌ No audit trail of what was used

### After Anemoi (Governance Domain)

```
User: Select "Anemoi Governed Coding"
      ↓
Request: model=coding (governance domain)
      ↓
Anemoi Decision Engine:
  Check available models
  Score each one
  Select best fit
      ↓
Forward to selected model
      ↓
Response headers show which model was used
      ↓
Decision logged to SQLite
```

**Benefits**:
- ✅ Automatic (no manual model selection)
- ✅ Adaptive (picks best available model)
- ✅ Transparent (headers show what was selected)
- ✅ Auditable (all decisions logged)
- ✅ Intelligent (resource-aware scoring)

---

## Integration with Pi and OpenCode

### Pi Integration

1. **Install**: Copy `models.json` to `C:\Users\Alex Lucero\.pi\agent\`
2. **Restart**: Close and reopen Pi
3. **Select**: Choose "Anemoi Governed Coding" from dropdown
4. **Use**: Send requests as normal
5. **Check**: Look at `X-Anemoi-Selected-Model` header

### OpenCode Integration

Same steps as Pi:
1. **Install**: Copy `models.json` to `.opencode\`
2. **Restart**: Close and reopen OpenCode
3. **Select**: Choose "Anemoi Governed Coding" from dropdown
4. **Use**: Send requests as normal

---

## Performance Notes

### Decision Latency

Typical decision time: **30-100ms**

Breaking down:
- Runtime inspection: 20-80ms
- Scoring: 5-10ms
- Model selection: <1ms
- Total: 30-100ms

### Network Overhead

If reverse proxy adds latency:
- Consider direct localhost access
- Or use HTTP/2 for connection reuse

### Throughput

Single daemon can handle:
- **Mock mode**: 100+ requests/second
- **Live mode**: 50-100 requests/second (limited by runtime)

---

## Troubleshooting

### Request keeps selecting same model

**Problem**: Anemoi always picks the same model even if it's busy

**Solution**:
1. Check scoring weights in config
2. Verify load detection is working
3. Review decision explanation: `explain <decision-id>`

### Very slow decisions

**Problem**: Decision latency > 500ms

**Solution**:
1. Check if runtime is slow to inspect
2. Reduce `inspect_timeout_ms` in config
3. Consider enabling mock mode for testing

### Different model selected than expected

**Problem**: You expected one model but got another

**Solution**:
1. Use `explain <decision-id>` to see the scoring
2. Check runtime state: `cli residents`
3. Adjust policy weights if needed

---

## Monitoring

### Log Decisions

```bash
# Watch decisions in real-time
tail -f /var/log/anemoi.log | grep "Decision made"

# Count per model
tail -100 /var/log/anemoi.log | grep "Decision made" | awk -F'model:' '{print $2}' | sort | uniq -c
```

### Query Event Store

```bash
# Recent decisions
sqlite3 anemoi-events.db "SELECT id, model, latency_ms FROM decisions ORDER BY created_at DESC LIMIT 20;"

# Slowest decisions
sqlite3 anemoi-events.db "SELECT id, model, latency_ms FROM decisions ORDER BY latency_ms DESC LIMIT 10;"

# Most popular models
sqlite3 anemoi-events.db "SELECT model, COUNT(*) as count FROM decisions GROUP BY model ORDER BY count DESC;"
```

---

## Advanced: Custom Domains

Create new governance domains in `config/anemoi.yaml`:

```yaml
domains:
  coding:
    roster:
      - group: large_models
        models: ["qwen3.6-35b-a3b-mtp"]
      - group: small_models
        models: ["qwen3.5-9b"]
  
  chat:  # New domain
    roster:
      - group: conversational
        models: ["qwen3.6-35b-a3b-mtp", "gemma-4-26b-a4b-it"]
  
  summarize:  # Another new domain
    roster:
      - group: extractive
        models: ["qwen3.5-9b"]
```

Then use in requests:

```json
{"model": "chat", "messages": [...]}
{"model": "summarize", "messages": [...]}
```

Anemoi will independently score candidates for each domain.

---

## Next Steps

- [Deployment Guide](DEPLOYMENT.md) - Set up Anemoi in production
- [Getting Started](GETTING_STARTED.md) - Complete walkthrough
- [README](../README.md) - Full feature overview
