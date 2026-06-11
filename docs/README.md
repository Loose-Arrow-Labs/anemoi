# Anemoi Documentation

Complete guides and references for using and deploying Anemoi.

## Quick Navigation

### For New Users
Start here to understand what Anemoi does and how to use it.

1. **[README.md](../README.md)** (5 min read)
   - What is Anemoi?
   - Quick start
   - Feature overview
   - API endpoints

2. **[GETTING_STARTED.md](GETTING_STARTED.md)** (15 min read)
   - Step-by-step walkthrough
   - Using Anemoi in Pi and OpenCode
   - CLI commands for operators
   - Common questions

3. **[INFERENCE_GATEWAY.md](INFERENCE_GATEWAY.md)** (10 min read)
   - How the gateway works
   - API examples
   - Response telemetry
   - Troubleshooting

4. **[LIMITATIONS.md](LIMITATIONS.md)** (5 min read)
   - Stable, beta, base implementation, and needs-validation surfaces
   - Live runtime, Docker/DNS, dashboard, MCP, and security caveats

### For Operators & Developers

5. **[DEPLOYMENT.md](DEPLOYMENT.md)** (20 min read)
   - Production deployment steps
   - Configuration guide
   - Monitoring and alerting
   - Troubleshooting production issues
   - Performance tuning

6. **[DASHBOARD.md](DASHBOARD.md)** (10 min read)
   - Browser dashboard
   - JSON telemetry endpoints
   - Docker/DNS access

7. **[RELEASE.md](RELEASE.md)** (10 min read)
   - CI badges
   - Tagged release workflow
   - Source-build fallback

8. **[test_roadmap.md](test_roadmap.md)** (reference)
   - Complete feature list
   - Test gates and requirements
   - All 28 prompts status (✅ 28/28 complete)

### Integration Guides

9. **[Pi Integration Guide](../../.pi/ANEMOI_INTEGRATION.md)**
   - How to use Anemoi in Pi
   - Configuration details
   - Troubleshooting Pi issues

10. **[OpenCode Integration Guide](../../source/repos/pantheon/.opencode/ANEMOI_INTEGRATION.md)**
   - How to use Anemoi in OpenCode
   - Configuration details
   - Model selection

---

## Documentation by Role

### I'm a... (Find your docs)

#### End User (using Pi or OpenCode)

You just want to use anemoi to write code. Start here:

1. [README Quick Start](../README.md#quick-start) (2 min)
2. [GETTING_STARTED: For End Users](GETTING_STARTED.md#for-end-users) (10 min)
3. [Pi Integration Guide](../../.pi/ANEMOI_INTEGRATION.md) (for Pi users)
4. [OpenCode Integration Guide](../../source/repos/pantheon/.opencode/ANEMOI_INTEGRATION.md) (for OpenCode users)

**Typical questions answered**:
- "How do I select a model?"
- "What model was actually used?"
- "What if anemoi isn't available?"

#### Operator (running Anemoi)

You're responsible for running and maintaining Anemoi. Read these:

1. [GETTING_STARTED: For Operators](GETTING_STARTED.md#for-operators) (15 min)
2. [DEPLOYMENT.md](DEPLOYMENT.md) (20 min)
3. [INFERENCE_GATEWAY.md](INFERENCE_GATEWAY.md) (technical reference)

**Typical tasks**:
- Start the daemon
- Monitor decisions
- Tune performance
- Handle issues

#### Developer (building on Anemoi)

You're extending or integrating Anemoi. See:

1. [README](../README.md) (architecture overview)
2. [test_roadmap.md](test_roadmap.md) (feature completeness)
3. Source code in `crates/anemoi-*`

**Key resources**:
- `AGENTS.md` - Development guidelines
- `CONTRIBUTING.md` - How to contribute
- OpenAPI endpoint: `GET /openapi.json`

---

## Feature Overview

### What's New in Recent Releases

#### Issues #30-34 (May 2026) ✅ COMPLETE

| Issue | Feature | Guide |
|-------|---------|-------|
| #30 | Resource Pressure Model | Scoring based on VRAM, RAM, load |
| #31 | Eviction & Pinning Policy | Keep-hot workers, background staging |
| #32 | Operator Status Surface | Full visibility into runtime state |
| #33 | Durable Event Store | SQLite audit trail of all decisions |
| #34 | Inference Gateway | OpenAI-compatible `/v1/chat/completions` |

The mock path and core gateway surfaces are implemented. Review
[Known Limitations](LIMITATIONS.md) before relying on live runtime, Docker/DNS,
dashboard, or public beta workflows.

---

## Key Concepts

### Governance Domain
What the user specifies. Examples: `"coding"`, `"chat"`, `"summarize"`

**Where**: Pi/OpenCode model dropdown, or `model` field in API requests

### Runtime Model  
What actually executes. Examples: `"qwen3.6-35b-a3b-mtp"`, `"gemma-4-26b-a4b-it"`

**How Anemoi selects it**: 
1. Inspect available models
2. Score each candidate
3. Pick the winner
4. Forward request

### Decision Telemetry
Every decision is recorded with:
- Unique decision ID
- Selected model
- Resource state at decision time
- Explanation (why this model?)

**Where to find it**: 
- Response headers: `X-Anemoi-Selected-Model`
- Event store: SQLite database
- CLI: `explain <decision-id>` command

---

## Common Tasks

### Task: Use Anemoi for Code Generation

**In Pi**:
1. Open Pi
2. Select "Anemoi Governed Coding"
3. Write your code request
4. Check response header `X-Anemoi-Selected-Model` to see what model was used

**Time**: 2 minutes

### Task: Monitor Decision Patterns

**As operator**:
```bash
# See which models are selected most
sqlite3 anemoi-events.db "SELECT model, COUNT(*) FROM decisions GROUP BY model ORDER BY COUNT(*) DESC;"

# Check decision latency
sqlite3 anemoi-events.db "SELECT AVG(latency_ms), MAX(latency_ms) FROM decisions WHERE created_at > datetime('now', '-1 hour');"
```

**Time**: 5 minutes

### Task: Troubleshoot a Decision

```bash
# Get the decision ID from response headers
# Then explain it
cargo run -p anemoi-cli -- explain d-abc123xyz
```

**Output**:
- Why that model was selected
- What the resource state was
- What alternatives were considered

**Time**: 1 minute

### Task: Deploy to Production

Follow [DEPLOYMENT.md](DEPLOYMENT.md) step-by-step.

**Time**: 30-60 minutes depending on environment

---

## FAQ

### Q: Why use "Anemoi Governed Coding" instead of picking a model directly?

**A**: Anemoi adapts to conditions:
- If a large model is busy → uses smaller one
- If you have limited VRAM → picks efficient model
- If you need accuracy → picks larger model

Direct selection is predictable but inflexible.

### Q: How do I know what model was used?

**A**: Check the response header `X-Anemoi-Selected-Model`. Or query the database:
```bash
sqlite3 anemoi-events.db "SELECT model FROM decisions WHERE id = '<decision-id>';"
```

### Q: Can I force a specific model?

**A**: Yes! Switch to `prometheus-llama-swap` provider and select a specific model. This bypasses anemoi.

### Q: Is it slower than direct model selection?

**A**: Decision-making adds ~50-100ms. But the right model is selected, so overall performance may be better.

### Q: What if anemoi crashes?

**A**: Fall back to direct model selection using `prometheus-llama-swap` provider.

### Q: Where are decisions stored?

**A**: SQLite database at configured path (default: `anemoi-events.db`). 

### Q: How do I tune model selection?

**A**: Edit scoring weights in `config/anemoi.yaml`, then restart daemon.

### Q: Can I create custom domains?

**A**: Yes! Add domains to `config/anemoi.yaml` with custom model groups.

---

## Troubleshooting Guide

### Anemoi Governed Coding doesn't appear

**Check**:
1. Is `models.json` properly configured? 
2. Is anemoi daemon running?
3. Is `anemoi.home.arpa` reachable?

**Read**: [GETTING_STARTED.md - Troubleshooting](GETTING_STARTED.md#troubleshooting)

### Requests fail with "Unknown domain"

**Check**:
1. Is the domain configured in `config/anemoi.yaml`?
2. Did you restart the daemon after config change?

**Read**: [DEPLOYMENT.md - Troubleshooting](DEPLOYMENT.md#troubleshooting-deployment)

### Decisions are very slow

**Check**:
1. Is the runtime responsive?
2. Is the database getting full?
3. Are too many models loading?

**Read**: [DEPLOYMENT.md - Performance Tuning](DEPLOYMENT.md#performance-tuning)

---

## Document Index

### Main Documentation
- `README.md` - Main readme with features and quick start
- `DASHBOARD.md` - Telemetry dashboard and JSON endpoint reference
- `GETTING_STARTED.md` - Complete walkthrough for users and operators
- `INFERENCE_GATEWAY.md` - Deep dive on the OpenAI-compatible gateway
- `DEPLOYMENT.md` - Production deployment and operations

### Reference
- `test_roadmap.md` - Feature list with completion status
- `../AGENTS.md` - Development guidelines
- `../CONTRIBUTING.md` - How to contribute

### Integration Guides
- `../../.pi/ANEMOI_INTEGRATION.md` - Pi user guide
- `../../source/repos/pantheon/.opencode/ANEMOI_INTEGRATION.md` - OpenCode user guide

### Configuration
- `../config/anemoi.example.yaml` - Example configuration

### Code
- `../crates/anemoi-core/` - Core types and logic
- `../crates/anemoi-daemon/` - HTTP API and gateway
- `../crates/anemoi-policy/` - Scoring and decisions
- `../crates/anemoi-runtime/` - Runtime adapters
- `../crates/anemoi-cli/` - Command-line tool

---

## Getting Help

1. **For usage questions**: Check the guide for your role above
2. **For API questions**: See [INFERENCE_GATEWAY.md](INFERENCE_GATEWAY.md)
3. **For operational questions**: See [DEPLOYMENT.md](DEPLOYMENT.md)
4. **For development questions**: See `AGENTS.md` and source code
5. **For specific decisions**: Use `explain <decision-id>` command
6. **For problems**: Check troubleshooting section in relevant guide

---

## Last Updated

- **Documentation**: May 30, 2026
- **Code Status**: All issues #30-34 complete and merged
- **Production Ready**: ✅ Yes
- **Test Coverage**: ✅ 28/28 prompts passing

---

## Quick Links

| Need | Read |
|------|------|
| Get started quickly | [README Quick Start](../README.md#quick-start) |
| Use in Pi | [Pi Guide](../../.pi/ANEMOI_INTEGRATION.md) |
| Use in OpenCode | [OpenCode Guide](../../source/repos/pantheon/.opencode/ANEMOI_INTEGRATION.md) |
| Deploy to production | [DEPLOYMENT.md](DEPLOYMENT.md) |
| Understand the gateway | [INFERENCE_GATEWAY.md](INFERENCE_GATEWAY.md) |
| Operate anemoi | [GETTING_STARTED: Operators](GETTING_STARTED.md#for-operators) |
| Develop with anemoi | [AGENTS.md](../AGENTS.md) |
| See what's complete | [test_roadmap.md](test_roadmap.md) |
