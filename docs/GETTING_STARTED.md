# Anemoi Getting Started Guide

Welcome to Anemoi! This guide walks you through using Anemoi for the first time, whether you're an end user or an operator.

Before relying on a live runtime or LAN/DNS deployment, review
[Known Limitations](LIMITATIONS.md).

## Table of Contents

1. [For End Users (Pi / OpenCode)](#for-end-users)
2. [For Operators](#for-operators)
3. [Architecture Overview](#architecture-overview)
4. [Common Tasks](#common-tasks)
5. [Troubleshooting](#troubleshooting)
6. [Known Limitations](LIMITATIONS.md)

---

## For End Users

### What is Anemoi Governed Coding?

Instead of picking a specific model (like "Qwen 3.6 35B"), you select **"Anemoi Governed Coding (dynamic model selection)"**. 

When you send a request:
1. **Anemoi decides** which model is best right now (based on resource availability, load, etc.)
2. **The right model is selected** (e.g., Qwen 3.6 if VRAM is available, or a faster model if it's busy)
3. **Your request runs** on the selected model
4. **You see what model was used** in the response headers

### Step-by-Step: Using Anemoi in Pi

#### 1. Open Pi

Launch Pi on your machine.

#### 2. Select the Model

In the model dropdown, look for:
- **"Anemoi Governed Coding (dynamic model selection)"**

Click it to select it.

#### 3. Send Your Request

Type your prompt and send it as normal.

Examples:
```
"Write a Python function to sort an array"
"Explain how recursion works"
"Debug this code: [paste code]"
```

#### 4. Check What Model Was Used

After you get the response, look for the response header:
- **`X-Anemoi-Selected-Model`**: Shows which model anemoi actually selected

This might be:
- `qwen3.6-35b-a3b-mtp` (large model for complex tasks)
- `qwen3.5-9b` (smaller model if the large one is busy)
- Or another model

### Step-by-Step: Using Anemoi in OpenCode

The process is identical to Pi:

1. **Select Model**: "Anemoi Governed Coding (dynamic model selection)"
2. **Send Request**: Your code or question
3. **Check Headers**: Look at `X-Anemoi-Selected-Model` to see what was used

### Why Not Pick a Model Directly?

**Direct model selection** (picking "Qwen 3.6 35B" specifically):
- ✅ Stable and predictable
- ✅ Good for benchmarking
- ❌ May be slow if that model is busy
- ❌ May fail if the model runs out of memory

**Anemoi Governed** (dynamic selection):
- ✅ Fast - picks the best available model right now
- ✅ Adapts - uses smaller models when larger ones are busy
- ✅ Transparent - you see what was selected in the header
- ✅ Auditable - every decision is logged
- ❌ Non-deterministic (different models for same request if conditions change)

### Response Headers Explained

When you use Anemoi, the response includes these headers:

| Header | Meaning | Example |
|--------|---------|---------|
| `X-Anemoi-Decision-Id` | Unique ID for this decision | `decision-abc123xyz` |
| `X-Anemoi-Selected-Model` | Model that was actually used | `qwen3.6-35b-a3b-mtp` |
| `X-Anemoi-Action` | What anemoi did | `forward-to-runtime` |

### Fallback to Direct Models

If Anemoi is unavailable:
1. Switch to `prometheus-llama-swap` provider
2. Select any specific model you want
3. Your request bypasses anemoi entirely

---

## For Operators

### What is Anemoi?

Anemoi is a **decision engine** for model selection. It:

- **Observes** runtime state (VRAM, load, model residency)
- **Scores** candidate models (resource pressure, cost, latency)
- **Decides** which model to use
- **Logs** every decision (SQLite)
- **Executes** by forwarding requests to the actual runtime

### Initial Setup

#### 1. Build from Source

```powershell
git clone <repo>
cd anemoi
cargo build -p anemoi-daemon --release
```

#### 2. Review Configuration

Default config: `config/anemoi.example.yaml`

Key sections:
- **domains**: Governance domains (e.g., "coding", "chat")
- **runtimes**: Where models actually run (e.g., llama-swap, Ollama)
- **models**: Available models and their profiles
- **residency_groups**: Groupings for scheduling decisions

#### 3. Start the Daemon

```powershell
cargo run -p anemoi-daemon
```

The daemon starts on `localhost:7070` by default.

#### 4. Verify Health

```bash
curl http://localhost:7070/health
# Expected response: {"status":"ok"}
```

### Common Operator Commands

#### Check Overall Status

```powershell
cargo run -p anemoi-cli -- status
```

**Output shows**:
```
Runtime Summary:
  Total runtimes: 1 (remote)
  Total models: 20
  Total residents: 3 (models currently loaded)

Policy Summary:
  Latency budget: 30000ms (30 seconds)
  Background staging: enabled
```

#### View Currently Loaded Models

```powershell
cargo run -p anemoi-cli -- residents
```

**Output shows**:
```
remote (available):
  qwen3.6-35b-a3b-mtp: hot_gpu (in use, on GPU)
  qwen3.5-9b: hot_gpu (in use, on GPU)
  gemma-4-26b-a4b-it: loading (being loaded now)
```

#### Make a Dry-Run Decision

```powershell
cargo run -p anemoi-cli -- decide --domain coding --latency-budget-ms 1500
```

**Output shows**:
```
Decision: Select qwen3.6-35b-a3b-mtp
Reason: Sufficient VRAM available, lowest latency cost for code task
Staging: qwen3.5-9b would be staged if time permits
```

#### Explain a Past Decision

```powershell
cargo run -p anemoi-cli -- explain <decision-id>
```

**Output shows**:
```
Decision ID: d-abc123
Domain: coding
Selected Model: qwen3.6-35b-a3b-mtp
Selected At: 2026-05-30T14:23:45Z

Evidence:
- VRAM pressure: 65% (acceptable)
- Load on target: 2 active requests
- Context size: 2048 tokens (fits in KV cache)

Explanation:
Large model selected (qwen3.6) because:
1. VRAM available
2. Code task benefits from larger model
3. Latency budget of 1500ms allows loading

Alternatives considered:
- qwen3.5-9b: Too small for complex code
- gemma-4-31b: Busy with 5 active requests
```

### Database Queries

Anemoi records decisions in SQLite. Query them directly:

```bash
# List recent decisions
sqlite3 anemoi-events.db "SELECT id, domain, model, created_at FROM decisions ORDER BY created_at DESC LIMIT 20;"

# Count decisions per model
sqlite3 anemoi-events.db "SELECT model, COUNT(*) as count FROM decisions GROUP BY model ORDER BY count DESC;"

# Find slow decisions
sqlite3 anemoi-events.db "SELECT id, model, latency_ms FROM decisions WHERE latency_ms > 1000;"

# View decision explanation
sqlite3 anemoi-events.db "SELECT explanation FROM decision_explanations WHERE decision_id = '<id>';"
```

### Monitoring

#### Log File

The daemon outputs logs to stdout:

```
[2026-05-30T14:20:00Z INFO] Daemon starting on 127.0.0.1:7070
[2026-05-30T14:20:01Z INFO] Reconciliation loop started
[2026-05-30T14:20:02Z INFO] Background staging worker started
[2026-05-30T14:23:45Z DEBUG] Decision made for domain:coding -> model:qwen3.6-35b-a3b-mtp
```

#### Performance

To understand decision latency:

```bash
sqlite3 anemoi-events.db "SELECT AVG(latency_ms), MAX(latency_ms), MIN(latency_ms) FROM decisions;"
```

Typical latencies:
- **Mock mode**: <10ms (no network)
- **Live mode**: 50-200ms (remote runtime inspection)

---

## Architecture Overview

### High-Level Flow

```
User Request with model="coding"
        ↓
Anemoi Decision Engine
  ├─ Read reconciliation cache (runtime state)
  ├─ Generate candidates (available models)
  ├─ Score each candidate (pressure, cost, latency)
  └─ Select winner (highest score)
        ↓
Forward to Runtime with selected model
  ├─ Rewrite model field
  ├─ Inject authentication
  └─ Record decision
        ↓
Runtime Executes (llama.cpp / Ollama / vLLM)
        ↓
Response with Telemetry Headers
```

### Key Concepts

#### Governance Domain
What the user specifies. Examples: `"coding"`, `"chat"`, `"summarize"`

#### Runtime Model
What actually runs. Examples: `"qwen3.6-35b-a3b-mtp"`, `"gemma-4-26b-a4b-it"`

#### Residency State
Where a model is. Options:
- **cold**: Not loaded
- **loading**: Being loaded now
- **hot_gpu**: On GPU, ready immediately
- **serving**: Currently running a request

#### Pressure
Resource utilization when scoring:
- **VRAM Pressure**: How full is GPU memory?
- **RAM Pressure**: How full is system memory?
- **Load Pressure**: How many requests on each model?
- **KV Cache Pressure**: How much context is available?

---

## Common Tasks

### Task: Monitor Model Usage Over Time

```bash
# Every 5 seconds, show which models are being selected
watch -n 5 "sqlite3 anemoi-events.db \"SELECT model, COUNT(*) FROM decisions WHERE created_at > datetime('now', '-5 minutes') GROUP BY model ORDER BY COUNT(*) DESC;\""
```

### Task: Find Why a Specific Decision Was Made

```bash
# Get the decision ID from logs
DECISION_ID="d-abc123"

# Show the explanation
cargo run -p anemoi-cli -- explain $DECISION_ID
```

### Task: Tune Resource Pressure Weights

Edit `config/anemoi.yaml`:

```yaml
policy:
  pressure_weights:
    vram: 1.0      # Higher = penalize VRAM pressure more
    ram: 0.8       # Lower = RAM pressure matters less
    load: 1.2      # Higher = load balancing matters more
    latency_cost: 0.5
```

Then restart the daemon for changes to take effect.

### Task: Pre-load a Large Model

Add to residency group in config:

```yaml
residency_groups:
  large_models:
    keep_hot: true
    models:
      - "nemotron-udiq4-256k"
```

Anemoi will keep it loaded and ready.

---

## Troubleshooting

### "Connection refused" or "Cannot reach anemoi.home.arpa"

**Problem**: Anemoi daemon isn't running or isn't accessible

**Solution**:
```powershell
# Check if daemon is running
Get-Process anemoi-daemon

# If not, start it
cargo run -p anemoi-daemon

# Check if it's listening
curl http://localhost:7070/health
```

### "Unknown domain: coding"

**Problem**: The domain isn't configured

**Solution**:
1. Check `config/anemoi.yaml` has domain `"coding"` defined
2. Restart the daemon after config changes
3. Verify with `cargo run -p anemoi-cli -- status`

### Response took too long (latency budget exceeded)

**Problem**: Decision making was slow

**Solution**:
```powershell
# Check decision latency
cargo run -p anemoi-cli -- explain <decision-id>

# If reconciliation cache is stale, trigger refresh
# (Usually automatic every 5 seconds)
```

### SQLite database is locked

**Problem**: Cannot query the events database

**Solution**:
```bash
# Check if daemon is using it
lsof anemoi-events.db

# Try again in a few seconds (temporary lock)
# Or use SQLite WAL mode for concurrent access
```

---

## Next Steps

1. **Deploy to production**: Configure `config/anemoi.yaml` for your runtimes
2. **Set up monitoring**: Query SQLite regularly to track decisions
3. **Tune policy**: Adjust scoring weights for your workload patterns
4. **Integrate tools**: Connect more tools via the OpenAI-compatible gateway
5. **Analyze patterns**: Use telemetry to optimize model staging

---

## Additional Resources

- **Full README**: `README.md` in repository root
- **Test Roadmap**: `docs/test_roadmap.md` (feature list with test gates)
- **Configuration Example**: `config/anemoi.example.yaml`
- **API Documentation**: `GET /openapi.json` endpoint
- **Source Code**: `crates/anemoi-*` directories

---

## Getting Help

- **For usage questions**: Read the relevant section above
- **For specific errors**: Check the Troubleshooting section
- **For code questions**: See `CONTRIBUTING.md`
- **For decision explanations**: Use `explain` command
- **For design decisions**: See `AGENTS.md`
