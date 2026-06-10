# Anemoi Deployment Guide

This guide covers deploying Anemoi to production environments.

## Pre-Deployment Checklist

- [ ] All tests passing: `cargo test --workspace`
- [ ] Linting passes: `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Code formatted: `cargo fmt --check`
- [ ] Configuration reviewed: `config/anemoi.yaml`
- [ ] Runtimes accessible: llama-swap, Ollama, etc.
- [ ] Reverse proxy configured: Traefik or similar
- [ ] SQLite database initialized
- [ ] Monitoring set up: logs, metrics, database queries
- [ ] Backup plan for rollback ready

---

## Step 1: Build Release Binary

```powershell
# Clean build
cargo clean

# Build with optimizations
cargo build -p anemoi-daemon --release

# Result: target/release/anemoi-daemon.exe
```

**Size**: ~50-100 MB depending on target

## Step 2: Prepare Configuration

Copy and customize `config/anemoi.example.yaml`:

```yaml
# config/anemoi.yaml (production)

telemetry:
  decision_log:
    database_url: "sqlite:///var/lib/anemoi/events.db"
  retention_days: 90

runtimes:
  remote:
    adapter: llama-swap
    base_url: "http://llama-swap.production:8000"
    health_timeout_ms: 5000
    inspect_timeout_ms: 10000

domains:
  coding:
    roster:
      - group: large_cpu
        models:
          - "qwen3.6-35b-a3b-mtp"
      - group: small_gpu
        models:
          - "qwen3.5-9b"
```

**Key sections**:
- `telemetry.database_url`: Where to store decisions (must be writable)
- `runtimes`: Connection info for each runtime
- `domains`: Governance domains and model groups
- `residency_groups`: Keep-hot policies

## Step 3: Configure Reverse Proxy (Traefik Example)

Use the checked-in example at
`deploy/traefik/anemoi.home.arpa.yml`, or create an equivalent dynamic
configuration:

```yaml
http:
  services:
    anemoi:
      loadBalancer:
        servers:
          - url: "http://anemoi:7070"

  routers:
    anemoi-http:
      rule: "Host(`anemoi.home.arpa`)"
      service: anemoi
      entryPoints: [web]
      middlewares: ["anemoi-local-only"]
    
    anemoi-https:
      rule: "Host(`anemoi.home.arpa`)"
      service: anemoi
      entryPoints: [websecure]
      middlewares: ["anemoi-local-only"]
      tls: {}

middlewares:
  anemoi-local-only:
    ipAllowList:
      sourceRange:
        - "127.0.0.1/32"
        - "10.0.0.0/8"
        - "172.16.0.0/12"
        - "192.168.0.0/16"
```

**Key points**:
- Route via hostname: `anemoi.home.arpa`
- Route to the daemon on port `7070`
- Keep the route LAN-only unless you add authentication and TLS explicitly
- If Traefik runs on the host instead of the compose network, point the service
  at `http://127.0.0.1:7070` or another host-reachable address

## Step 4: Set Up Database

```bash
# Create directory
mkdir -p /var/lib/anemoi

# Initialize SQLite (daemon will create schema)
touch /var/lib/anemoi/events.db
chmod 666 /var/lib/anemoi/events.db

# Verify
sqlite3 /var/lib/anemoi/events.db ".tables"
```

## Step 5: Start the Daemon

### Option A: Direct Execution

```powershell
./target/release/anemoi-daemon
```

Output:
```
[INFO] Anemoi daemon starting on 127.0.0.1:7070
[INFO] Loaded configuration from config/anemoi.yaml
[INFO] Connected to runtime: remote (llama-swap)
[INFO] Reconciliation loop started
[INFO] Background staging worker started
[INFO] Ready to accept requests
```

### Option B: Systemd Service (Linux)

Create `/etc/systemd/system/anemoi.service`:

```ini
[Unit]
Description=Anemoi Inference Governance Daemon
After=network.target

[Service]
Type=simple
User=anemoi
WorkingDirectory=/opt/anemoi
ExecStart=/opt/anemoi/target/release/anemoi-daemon
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

Then:
```bash
sudo systemctl enable anemoi
sudo systemctl start anemoi
sudo systemctl status anemoi
```

### Option C: Docker Container

Use the checked-in Docker files:

```powershell
docker compose -f deploy/docker/docker-compose.yml up --build
```

The compose file runs the Rust daemon with:

```text
ANEMOI_BIND=0.0.0.0:7070
ANEMOI_CONFIG=/app/config/anemoi.yaml
host 7070 -> container 7070
```

For a direct `docker run` equivalent:

```bash
docker build -f deploy/docker/Dockerfile -t anemoi:latest .
docker run -d \
  --name anemoi \
  -p 7070:7070 \
  -e ANEMOI_BIND=0.0.0.0:7070 \
  -e ANEMOI_CONFIG=/app/config/anemoi.yaml \
  -v /etc/anemoi/anemoi.yaml:/app/config/anemoi.yaml:ro \
  -v /var/lib/anemoi:/var/lib/anemoi \
  anemoi:latest
```

Loopback development (`127.0.0.1:7070`) is for same-machine use. Container,
LAN, and DNS deployments must bind `0.0.0.0:7070` inside the container so the
published port and reverse proxy can reach the daemon.

## Step 6: Verify Deployment

```bash
# Health check
curl http://anemoi.home.arpa/health
curl https://anemoi.home.arpa/health

# Status
curl http://anemoi.home.arpa/status
curl https://anemoi.home.arpa/status

# Residents
curl http://anemoi.home.arpa/residents

# Telemetry summary
curl http://anemoi.home.arpa/telemetry/summary

# Dashboard
open http://anemoi.home.arpa/dashboard/

# Models list (inference gateway)
curl http://anemoi.home.arpa/v1/models
curl https://anemoi.home.arpa/v1/models

# Test inference request
curl -X POST http://anemoi.home.arpa/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"coding","messages":[{"role":"user","content":"test"}],"max_tokens":10}'
```

After the telemetry dashboard issue lands on the deployed version, the same DNS
route should also serve:

```bash
curl http://anemoi.home.arpa/telemetry/summary
curl http://anemoi.home.arpa/dashboard
```

All enabled routes should respond with 200 OK.

## Step 7: Set Up Monitoring

### Log Aggregation

```bash
# View daemon logs
tail -f /var/log/anemoi.log

# Search for errors
grep ERROR /var/log/anemoi.log | tail -20

# Count decisions per hour
grep "Decision made" /var/log/anemoi.log | cut -d' ' -f1,2 | sort | uniq -c
```

### Database Monitoring

```bash
# Decision rate (decisions per minute)
sqlite3 /var/lib/anemoi/events.db \
  "SELECT COUNT(*) as decisions_per_minute FROM decisions WHERE created_at > datetime('now', '-1 minute');"

# Latency percentiles
sqlite3 /var/lib/anemoi/events.db \
  "SELECT 
     MIN(latency_ms) as min,
     MAX(latency_ms) as max,
     AVG(latency_ms) as avg
   FROM decisions WHERE created_at > datetime('now', '-1 hour');"

# Model usage distribution
sqlite3 /var/lib/anemoi/events.db \
  "SELECT model, COUNT(*) as count FROM decisions GROUP BY model ORDER BY count DESC LIMIT 10;"
```

### Alerting Rules

Set up alerts for:
- **Daemon down**: No decisions in last 5 minutes
- **High latency**: Average decision latency > 500ms
- **Database full**: SQLite file size > 10GB
- **Runtime unreachable**: Failed runtime inspections > 10%

## Step 8: Configure Backups

### Database Backup

```bash
# Daily backup to S3
aws s3 cp /var/lib/anemoi/events.db s3://backups/anemoi/events-$(date +%Y%m%d).db

# Keep 30 days of history
aws s3 ls s3://backups/anemoi/ --recursive | grep -v "$(date -d '30 days ago' +%Y%m%d)" | awk '{print $4}' | xargs -I {} aws s3 rm s3://{}
```

### Configuration Backup

```bash
git commit -am "Production config snapshot"
git push backup main
```

---

## Performance Tuning

### Decision Latency

If decisions are slow (>500ms):

1. **Check reconciliation cache TTL**:
   ```yaml
   runtime_reconciliation:
     cache_ttl_seconds: 10  # How long before re-inspecting
   ```

2. **Enable mock mode for testing**:
   ```yaml
   execution:
     mock_forwarding_enabled: true
   ```

3. **Reduce inspection timeouts**:
   ```yaml
   runtimes:
     remote:
       inspect_timeout_ms: 5000  # Default 10000
   ```

### Memory Usage

If memory is high:

1. **Limit decision history**:
   ```yaml
   telemetry:
     memory_log_capacity: 1000  # Default 10000
   ```

2. **Archive old decisions**:
   ```bash
   sqlite3 /var/lib/anemoi/events.db \
     "DELETE FROM decisions WHERE created_at < datetime('now', '-30 days');"
   ```

### Throughput

For high request volume:

1. **Increase staging worker parallelism**:
   ```yaml
   background_staging:
     max_concurrent_loads: 3  # Default 1
   ```

2. **Tune candidate scoring cache**:
   ```yaml
   policy:
     candidate_cache_ttl_seconds: 5
   ```

---

## Troubleshooting Deployment

### Daemon won't start

```bash
# Check permissions
ls -la /var/lib/anemoi/

# Check logs for errors
journalctl -u anemoi -n 50 -e

# Verify config syntax
cargo run -p anemoi-cli -- status  # Will fail if config is invalid
```

### Runtime not found

```bash
# Verify runtime is accessible
curl http://llama-swap.production:8000/health

# Check configuration
grep "adapter:" config/anemoi.yaml
```

### Database locked

```bash
# Check if daemon is running
ps aux | grep anemoi-daemon

# If stuck, restart daemon
systemctl restart anemoi

# Verify database
sqlite3 /var/lib/anemoi/events.db "SELECT COUNT(*) FROM decisions;"
```

### High latency decisions

```bash
# Check reconciliation cache staleness
sqlite3 /var/lib/anemoi/events.db \
  "SELECT inspection_latency_ms FROM decisions WHERE created_at > datetime('now', '-1 hour') ORDER BY inspection_latency_ms DESC LIMIT 5;"

# If consistently slow, runtime may be overloaded
# Reduce staging load or increase latency budgets
```

---

## Rollback Plan

If deployment has issues:

1. **Keep previous binary**:
   ```bash
   cp target/release/anemoi-daemon target/release/anemoi-daemon.bak
   ```

2. **Stop current daemon**:
   ```bash
   systemctl stop anemoi
   ```

3. **Restore previous version**:
   ```bash
   cp target/release/anemoi-daemon.bak target/release/anemoi-daemon
   systemctl start anemoi
   ```

4. **Verify it works**:
   ```bash
   curl http://anemoi.home.arpa/health
   ```

5. **Investigate what went wrong**:
   ```bash
   diff config/anemoi.yaml config/anemoi.yaml.bak
   ```

---

## Production Checklist

- [ ] Daemon is running and responding to health checks
- [ ] All configured runtimes are accessible
- [ ] Database is initialized and writable
- [ ] Reverse proxy is routing requests correctly
- [ ] Monitoring is collecting logs and metrics
- [ ] Backups are running automatically
- [ ] Alerting is configured and tested
- [ ] Rollback procedure has been tested
- [ ] Team knows how to monitor and troubleshoot
- [ ] Documentation updated with production URLs

---

## Support

- **Logs**: Check daemon output for errors
- **Database**: Query `anemoi-events.db` to analyze decisions
- **CLI**: Use `cargo run -p anemoi-cli -- explain <id>` to understand decisions
- **Source**: `crates/anemoi-daemon` for implementation details
