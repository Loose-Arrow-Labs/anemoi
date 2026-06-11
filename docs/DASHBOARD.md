# Telemetry Dashboard

Anemoi serves a static Vite + TypeScript dashboard from the Rust daemon. No Node
process is required in production.

## URLs

Local daemon:

```text
http://127.0.0.1:7070/dashboard/
```

Docker or DNS deployment:

```text
https://anemoi.home.arpa/dashboard/
```

The dashboard reads the same read-only JSON telemetry endpoints that operator
tools can use directly.

## JSON Endpoints

```text
GET /telemetry/summary
GET /telemetry/decisions?limit=50
GET /telemetry/decision/:id
GET /telemetry/resident-events?model_id=...
GET /telemetry/staging-events?limit=50
GET /telemetry/action-plans?decision_id=...
GET /telemetry/runtime-snapshots?runtime_id=...
```

The endpoints expose current in-memory daemon state without requiring SQLite.
When `ANEMOI_DATABASE_URL=sqlite://...` is configured, resident transitions,
runtime snapshots, staging events, and action plans are read from durable event
history where available.

The telemetry endpoints are read-only. Runtime mutation remains behind existing
execution paths and the `ANEMOI_ENABLE_LIVE_EXECUTE=1` gate.

## Build

```powershell
cd web/dashboard
npm install
npm test
npm run build
```

Then start the daemon:

```powershell
$env:ANEMOI_CONFIG = "config/anemoi.example.yaml"
$env:ANEMOI_BIND = "127.0.0.1:7070"
cargo run -p anemoi-daemon
```

Open `http://127.0.0.1:7070/dashboard/`.

For frontend-only fixture data, open:

```text
http://127.0.0.1:7070/dashboard/?fixture=1
```

## Docker/DNS Shape

The Docker image copies the built dashboard assets into
`/app/web/dashboard/dist` and sets:

```text
ANEMOI_DASHBOARD_DIST=/app/web/dashboard/dist
ANEMOI_BIND=0.0.0.0:7070
```

`deploy/docker/docker-compose.yml` maps host `7070` to container `7070`.
A local reverse proxy can then route:

```text
anemoi.home.arpa -> anemoi:7070
```

Required paths through that route:

```text
/dashboard/
/telemetry/*
/v1/*
```

Treat the dashboard as a local operator surface. Put it behind LAN-only DNS,
TLS, and/or reverse-proxy access controls before exposing it beyond a trusted
machine or network.
