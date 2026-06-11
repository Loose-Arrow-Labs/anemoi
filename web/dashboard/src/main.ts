import "./styles.css";

type StagingSummary = {
  total: number;
  blocked: number;
  pending: number;
  failed: number;
  completed: number;
};

type TelemetrySummary = {
  cache_populated: boolean;
  runtime_count: number;
  resident_count: number;
  unavailable_runtime_count: number;
  stale_runtime_count: number;
  active_request_count: number;
  staging: StagingSummary;
  recent_decision_count: number;
  policy_warnings: string[];
  live_execution_enabled: boolean;
};

type RuntimeSnapshot = {
  runtime_id: string;
  available: boolean;
  residents: Resident[];
  configured_models: string[];
  active_requests: Array<{ model_id?: string }>;
};

type Resident = {
  model_id: string;
  state: string;
  vram_mb?: number | null;
  ram_mb?: number | null;
  kv_cache_mb?: number | null;
};

type RuntimeItem = {
  runtime_id: string;
  availability: string;
  freshness: string;
  last_inspected: string;
  last_error?: string | null;
  resident_count: number;
  active_request_count: number;
  configured_model_count: number;
  snapshot: RuntimeSnapshot;
};

type RuntimeSnapshotResponse = {
  items: RuntimeItem[];
  history: Array<{ event_id: string; runtime_id: string; observed_at: string }>;
  cache_populated: boolean;
  count: number;
  history_count: number;
  limit: number;
};

type DecisionSummary = {
  id: string;
  request_id: string;
  action: string;
  selected_model?: string | null;
  selected_runtime?: string | null;
  selected_group?: string | null;
  background_model?: string | null;
  score: { total: number; contributions: Array<{ label: string; value: number }> };
  explanation_summary: string;
  reason_count: number;
  rejected_option_count: number;
  created_at: string;
};

type DecisionDetail = {
  id: string;
  action: string;
  selected_model?: string | null;
  selected_runtime?: string | null;
  score: { total: number; contributions: Array<{ label: string; value: number }> };
  explanation: {
    summary: string;
    reasons: Array<{ code: string; detail: string; impact: number }>;
    rejected_options: Array<{ model_id?: string | null; runtime_id?: string | null; reason: string }>;
  };
  created_at: string;
};

type ListResponse<T> = {
  items: T[];
  count: number;
  limit: number;
};

type StagingIntent = {
  id: string;
  decision_id: string;
  foreground_model?: string | null;
  background_model: string;
  target_runtime: string;
  reason: string;
  created_at: string;
  state: string;
  last_error?: string | null;
  attempt_count: number;
  last_skip_reason?: string | null;
};

type StagingResponse = {
  items: StagingIntent[];
  history: Array<{
    intent_id: string;
    decision_id: string;
    background_model: string;
    target_runtime: string;
    reason: string;
    state: string;
    recorded_at: string;
  }>;
  count: number;
  history_count: number;
  limit: number;
};

type ResidentEvent = {
  model_id: string;
  runtime_id: string;
  from_state: string;
  to_state: string;
  observed_at: string;
  evidence_source: string;
  note?: string | null;
};

type ActionPlanEvent = {
  decision_id: string;
  recorded_at: string;
  plan: { dry_run: boolean; actions: Array<{ kind: string; runtime_id: string; model_id?: string | null; reason: string }> };
};

type DashboardData = {
  summary: TelemetrySummary;
  runtimes: RuntimeSnapshotResponse;
  decisions: ListResponse<DecisionSummary>;
  staging: StagingResponse;
  residentEvents: ListResponse<ResidentEvent>;
  actionPlans: ListResponse<ActionPlanEvent>;
};

const rootElement = document.querySelector<HTMLDivElement>("#anemoi-dashboard");
if (!rootElement) {
  throw new Error("missing dashboard root");
}
const root: HTMLDivElement = rootElement;

let selectedDecision: DecisionDetail | null = null;
let lastUpdated = "";
const useFixture = new URLSearchParams(window.location.search).get("fixture") === "1";

async function fetchJson<T>(path: string): Promise<T> {
  const response = await fetch(path, { headers: { accept: "application/json" } });
  if (!response.ok) {
    throw new Error(`${path} returned ${response.status}`);
  }
  return (await response.json()) as T;
}

async function loadDashboardData(): Promise<DashboardData> {
  if (useFixture) {
    return fixtureData();
  }

  const [summary, runtimes, decisions, staging, residentEvents, actionPlans] = await Promise.all([
    fetchJson<TelemetrySummary>("/telemetry/summary"),
    fetchJson<RuntimeSnapshotResponse>("/telemetry/runtime-snapshots?limit=25"),
    fetchJson<ListResponse<DecisionSummary>>("/telemetry/decisions?limit=25"),
    fetchJson<StagingResponse>("/telemetry/staging-events?limit=25"),
    fetchJson<ListResponse<ResidentEvent>>("/telemetry/resident-events?limit=25"),
    fetchJson<ListResponse<ActionPlanEvent>>("/telemetry/action-plans?limit=25")
  ]);

  return { summary, runtimes, decisions, staging, residentEvents, actionPlans };
}

async function loadDecisionDetail(id: string): Promise<void> {
  selectedDecision = useFixture ? fixtureDecisionDetail(id) : await fetchJson<DecisionDetail>(`/telemetry/decision/${id}`);
  const data = await loadDashboardData();
  render(data);
}

function render(data: DashboardData): void {
  lastUpdated = new Date().toLocaleTimeString();
  root.innerHTML = `
    <main class="shell">
      <header class="topbar">
        <div>
          <h1>Anemoi</h1>
          <p>Telemetry dashboard</p>
        </div>
        <div class="topbar-actions">
          ${chip(data.summary.live_execution_enabled ? "live execute on" : "dry-run gate", data.summary.live_execution_enabled ? "danger" : "neutral")}
          ${chip(data.summary.cache_populated ? "cache fresh" : "cache unknown", data.summary.cache_populated ? "ok" : "warn")}
          <span class="timestamp">Updated ${escapeHtml(lastUpdated)}</span>
        </div>
      </header>

      <section class="metrics" aria-label="Overview">
        ${metric("Runtimes", data.summary.runtime_count)}
        ${metric("Residents", data.summary.resident_count)}
        ${metric("Decisions", data.summary.recent_decision_count)}
        ${metric("Staging", data.summary.staging.total)}
        ${metric("Active", data.summary.active_request_count)}
      </section>

      <div class="layout">
        <section class="panel runtimes">
          <div class="panel-header">
            <h2>Runtimes</h2>
            <span>${data.runtimes.count} current / ${data.runtimes.history_count} events</span>
          </div>
          ${renderRuntimes(data.runtimes.items)}
        </section>

        <section class="panel residents">
          <div class="panel-header">
            <h2>Residents</h2>
            <span>${data.summary.resident_count} observed</span>
          </div>
          ${renderResidents(data.runtimes.items)}
        </section>

        <section class="panel decisions">
          <div class="panel-header">
            <h2>Decisions</h2>
            <span>${data.decisions.count} recent</span>
          </div>
          ${renderDecisions(data.decisions.items)}
        </section>

        <section class="panel detail">
          <div class="panel-header">
            <h2>Decision Detail</h2>
            <span>${selectedDecision ? escapeHtml(selectedDecision.id.slice(0, 8)) : "none selected"}</span>
          </div>
          ${renderDecisionDetail(selectedDecision)}
        </section>

        <section class="panel staging">
          <div class="panel-header">
            <h2>Staging</h2>
            <span>${stageSummary(data.summary.staging)}</span>
          </div>
          ${renderStaging(data.staging.items)}
        </section>

        <section class="panel events">
          <div class="panel-header">
            <h2>Events</h2>
            <span>${data.residentEvents.count} resident / ${data.actionPlans.count} plans</span>
          </div>
          ${renderEvents(data.residentEvents.items, data.actionPlans.items)}
        </section>
      </div>
    </main>
  `;

  root.querySelectorAll<HTMLButtonElement>("[data-decision-id]").forEach((button) => {
    button.addEventListener("click", () => {
      const id = button.dataset.decisionId;
      if (id) {
        void loadDecisionDetail(id);
      }
    });
  });
}

function renderRuntimes(items: RuntimeItem[]): string {
  if (items.length === 0) {
    return emptyState("No reconciled runtime snapshots yet.");
  }

  return table(
    ["Runtime", "Availability", "Freshness", "Residents", "Active", "Last inspection"],
    items.map((item) => [
      escapeHtml(item.runtime_id),
      chip(item.availability, item.availability === "available" ? "ok" : "danger"),
      chip(item.freshness, item.freshness === "fresh" ? "ok" : "warn"),
      String(item.resident_count),
      String(item.active_request_count),
      formatDate(item.last_inspected)
    ])
  );
}

function renderResidents(runtimes: RuntimeItem[]): string {
  const rows = runtimes.flatMap((runtime) =>
    runtime.snapshot.residents.map((resident) => [
      escapeHtml(runtime.runtime_id),
      escapeHtml(resident.model_id),
      chip(resident.state, stateTone(resident.state)),
      memoryCell(resident),
      String(runtime.active_request_count)
    ])
  );

  if (rows.length === 0) {
    return emptyState("No resident models reported by runtime inspection.");
  }

  return table(["Runtime", "Model", "State", "Memory", "Active"], rows);
}

function renderDecisions(items: DecisionSummary[]): string {
  if (items.length === 0) {
    return emptyState("No decisions recorded yet.");
  }

  return table(
    ["When", "Action", "Selected", "Runtime", "Score", "Explanation"],
    items.map((item) => [
      formatDate(item.created_at),
      chip(item.action, actionTone(item.action)),
      decisionButton(item),
      escapeHtml(item.selected_runtime ?? "none"),
      String(item.score.total),
      escapeHtml(item.explanation_summary)
    ])
  );
}

function renderDecisionDetail(detail: DecisionDetail | null): string {
  if (!detail) {
    return emptyState("Select a decision to inspect reasons and rejected options.");
  }

  const reasons = detail.explanation.reasons
    .map((reason) => `<li><b>${escapeHtml(reason.code)}</b> ${escapeHtml(reason.detail)} <span>${reason.impact}</span></li>`)
    .join("");
  const rejected = detail.explanation.rejected_options
    .map((option) => `<li>${escapeHtml(option.model_id ?? "unknown")} on ${escapeHtml(option.runtime_id ?? "unknown")}: ${escapeHtml(option.reason)}</li>`)
    .join("");

  return `
    <div class="detail-grid">
      <div><span>Action</span>${chip(detail.action, actionTone(detail.action))}</div>
      <div><span>Model</span><strong>${escapeHtml(detail.selected_model ?? "none")}</strong></div>
      <div><span>Runtime</span><strong>${escapeHtml(detail.selected_runtime ?? "none")}</strong></div>
      <div><span>Score</span><strong>${detail.score.total}</strong></div>
    </div>
    <p class="summary-line">${escapeHtml(detail.explanation.summary)}</p>
    <h3>Reasons</h3>
    <ul class="event-list">${reasons || "<li>No reasons recorded.</li>"}</ul>
    <h3>Rejected</h3>
    <ul class="event-list">${rejected || "<li>No rejected options recorded.</li>"}</ul>
  `;
}

function renderStaging(items: StagingIntent[]): string {
  if (items.length === 0) {
    return emptyState("No staging intents queued.");
  }

  return table(
    ["State", "Background", "Runtime", "Attempts", "Reason"],
    items.map((item) => [
      chip(item.state, stateTone(item.state)),
      escapeHtml(item.background_model),
      escapeHtml(item.target_runtime),
      String(item.attempt_count),
      escapeHtml(item.last_skip_reason ?? item.last_error ?? item.reason)
    ])
  );
}

function renderEvents(residentEvents: ResidentEvent[], actionPlans: ActionPlanEvent[]): string {
  const residentRows = residentEvents.slice(0, 5).map((event) =>
    `<li>${formatDate(event.observed_at)} ${escapeHtml(event.model_id)} ${escapeHtml(event.from_state)} -> ${escapeHtml(event.to_state)} on ${escapeHtml(event.runtime_id)}</li>`
  );
  const actionRows = actionPlans.slice(0, 5).map((event) =>
    `<li>${formatDate(event.recorded_at)} ${escapeHtml(event.decision_id.slice(0, 8))} ${event.plan.dry_run ? "dry-run" : "live"} ${event.plan.actions.length} actions</li>`
  );

  if (residentRows.length === 0 && actionRows.length === 0) {
    return emptyState("No durable event history available.");
  }

  return `
    <h3>Resident transitions</h3>
    <ul class="event-list">${residentRows.join("") || "<li>No resident transitions.</li>"}</ul>
    <h3>Action plans</h3>
    <ul class="event-list">${actionRows.join("") || "<li>No action plans recorded.</li>"}</ul>
  `;
}

function table(headers: string[], rows: string[][]): string {
  return `
    <div class="table-wrap">
      <table>
        <thead><tr>${headers.map((header) => `<th>${escapeHtml(header)}</th>`).join("")}</tr></thead>
        <tbody>${rows.map((row) => `<tr>${row.map((cell) => `<td>${cell}</td>`).join("")}</tr>`).join("")}</tbody>
      </table>
    </div>
  `;
}

function metric(label: string, value: number): string {
  return `<div class="metric"><span>${escapeHtml(label)}</span><strong>${value}</strong></div>`;
}

function decisionButton(item: DecisionSummary): string {
  const model = item.selected_model ?? item.background_model ?? "none";
  return `<button class="link-button" data-decision-id="${escapeHtml(item.id)}">${escapeHtml(model)}</button>`;
}

function chip(label: string, tone: "ok" | "warn" | "danger" | "neutral"): string {
  return `<span class="chip chip-${tone}">${escapeHtml(formatChipLabel(label))}</span>`;
}

function emptyState(message: string): string {
  return `<p class="empty">${escapeHtml(message)}</p>`;
}

function stageSummary(summary: StagingSummary): string {
  return `${summary.pending} pending / ${summary.blocked} blocked / ${summary.failed} failed`;
}

function actionTone(action: string): "ok" | "warn" | "danger" | "neutral" {
  if (action === "reuse_hot" || action === "promote_warm") return "ok";
  if (action === "stage_background" || action === "cold_load") return "warn";
  if (action === "deny" || action === "defer") return "danger";
  return "neutral";
}

function stateTone(state: string): "ok" | "warn" | "danger" | "neutral" {
  if (["hot_gpu", "serving", "completed", "fresh", "available"].includes(state)) return "ok";
  if (["loading", "warm_cpu", "partial", "pending", "blocked", "stale"].includes(state)) return "warn";
  if (["failed", "evicting", "unavailable"].includes(state)) return "danger";
  return "neutral";
}

function memoryCell(resident: Resident): string {
  const values = [
    resident.vram_mb ? `${resident.vram_mb} VRAM` : "",
    resident.ram_mb ? `${resident.ram_mb} RAM` : "",
    resident.kv_cache_mb ? `${resident.kv_cache_mb} KV` : ""
  ].filter(Boolean);
  return escapeHtml(values.join(" / ") || "unknown");
}

function formatDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return escapeHtml(value);
  }
  return escapeHtml(date.toLocaleTimeString());
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function formatChipLabel(label: string): string {
  return label.replaceAll("_", " ");
}

function fixtureData(): DashboardData {
  const now = new Date().toISOString();
  return {
    summary: {
      cache_populated: true,
      runtime_count: 1,
      resident_count: 1,
      unavailable_runtime_count: 0,
      stale_runtime_count: 0,
      active_request_count: 0,
      staging: { total: 1, blocked: 1, pending: 0, failed: 0, completed: 0 },
      recent_decision_count: 1,
      policy_warnings: [],
      live_execution_enabled: false
    },
    runtimes: {
      items: [
        {
          runtime_id: "llama_swap",
          availability: "available",
          freshness: "fresh",
          last_inspected: now,
          last_error: null,
          resident_count: 1,
          active_request_count: 0,
          configured_model_count: 3,
          snapshot: {
            runtime_id: "llama_swap",
            available: true,
            residents: [{ model_id: "qwen9b", state: "hot_gpu", vram_mb: 9000, ram_mb: 12000 }],
            configured_models: ["qwen9b", "qwen35_a3b", "granite8b"],
            active_requests: []
          }
        }
      ],
      history: [{ event_id: "snapshot-1", runtime_id: "llama_swap", observed_at: now }],
      cache_populated: true,
      count: 1,
      history_count: 1,
      limit: 25
    },
    decisions: {
      items: [
        {
          id: "11111111-1111-4111-8111-111111111111",
          request_id: "req-fixture",
          action: "stage_background",
          selected_model: "qwen9b",
          selected_runtime: "llama_swap",
          selected_group: "small_swarm",
          background_model: "qwen35_a3b",
          score: { total: 78, contributions: [{ label: "hot resident", value: 35 }] },
          explanation_summary: "Selected hot worker now and staged the larger model.",
          reason_count: 2,
          rejected_option_count: 1,
          created_at: now
        }
      ],
      count: 1,
      limit: 25
    },
    staging: {
      items: [
        {
          id: "22222222-2222-4222-8222-222222222222",
          decision_id: "11111111-1111-4111-8111-111111111111",
          foreground_model: "qwen9b",
          background_model: "qwen35_a3b",
          target_runtime: "llama_swap",
          reason: "Background staging for quality upgrade.",
          created_at: now,
          state: "blocked",
          last_error: "blocked: target runtime is not mock and ANEMOI_ENABLE_LIVE_EXECUTE is not set",
          attempt_count: 0,
          last_skip_reason: null
        }
      ],
      history: [],
      count: 1,
      history_count: 0,
      limit: 25
    },
    residentEvents: {
      items: [
        {
          model_id: "qwen9b",
          runtime_id: "llama_swap",
          from_state: "cold",
          to_state: "hot_gpu",
          observed_at: now,
          evidence_source: "llama_swap reconciliation round 1",
          note: "first observation; prior state inferred as cold"
        }
      ],
      count: 1,
      limit: 25
    },
    actionPlans: {
      items: [
        {
          decision_id: "11111111-1111-4111-8111-111111111111",
          recorded_at: now,
          plan: { dry_run: true, actions: [{ kind: "load", runtime_id: "llama_swap", model_id: "qwen35_a3b", reason: "Background staging load" }] }
        }
      ],
      count: 1,
      limit: 25
    }
  };
}

function fixtureDecisionDetail(id: string): DecisionDetail {
  const now = new Date().toISOString();
  return {
    id,
    action: "stage_background",
    selected_model: "qwen9b",
    selected_runtime: "llama_swap",
    score: { total: 78, contributions: [{ label: "hot resident", value: 35 }] },
    explanation: {
      summary: "Selected hot worker now and staged the larger model.",
      reasons: [
        { code: "continuity", detail: "Small worker is already hot inside the latency budget.", impact: 35 },
        { code: "quality_stage", detail: "Large model should be staged for the next turn.", impact: 22 }
      ],
      rejected_options: [
        { model_id: "qwen35_a3b", runtime_id: "llama_swap", reason: "Cold load exceeds the interactive latency budget." }
      ]
    },
    created_at: now
  };
}

async function refresh(): Promise<void> {
  try {
    const data = await loadDashboardData();
    render(data);
  } catch (error) {
    root.innerHTML = `<main class="shell"><section class="panel error"><h1>Anemoi</h1><p>${escapeHtml(String(error))}</p></section></main>`;
  }
}

void refresh();
window.setInterval(() => {
  void refresh();
}, 5000);
