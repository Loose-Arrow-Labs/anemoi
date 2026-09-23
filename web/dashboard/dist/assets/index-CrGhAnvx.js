(function(){let e=document.createElement(`link`).relList;if(e&&e.supports&&e.supports(`modulepreload`))return;for(let e of document.querySelectorAll(`link[rel="modulepreload"]`))n(e);new MutationObserver(e=>{for(let t of e)if(t.type===`childList`)for(let e of t.addedNodes)e.tagName===`LINK`&&e.rel===`modulepreload`&&n(e)}).observe(document,{childList:!0,subtree:!0});function t(e){let t={};return e.integrity&&(t.integrity=e.integrity),e.referrerPolicy&&(t.referrerPolicy=e.referrerPolicy),e.crossOrigin===`use-credentials`?t.credentials=`include`:e.crossOrigin===`anonymous`?t.credentials=`omit`:t.credentials=`same-origin`,t}function n(e){if(e.ep)return;e.ep=!0;let n=t(e);fetch(e.href,n)}})();var e=document.querySelector(`#anemoi-dashboard`);if(!e)throw Error(`missing dashboard root`);var t=e,n=null,r=``,i=new URLSearchParams(window.location.search).get(`fixture`)===`1`,a=`telemetry`,o=null,s=``;async function c(e){let t=await fetch(e,{headers:{accept:`application/json`}});if(!t.ok)throw Error(`${e} returned ${t.status}`);return await t.json()}async function l(){if(i)return A();let[e,t,n,r,a,o]=await Promise.all([c(`/telemetry/summary`),c(`/telemetry/runtime-snapshots?limit=25`),c(`/telemetry/decisions?limit=25`),c(`/telemetry/staging-events?limit=25`),c(`/telemetry/resident-events?limit=25`),c(`/telemetry/action-plans?limit=25`)]);return{summary:e,runtimes:t,decisions:n,staging:r,residentEvents:a,actionPlans:o}}async function u(e){n=i?j(e):await c(`/telemetry/decision/${e}`),d(await l())}function d(e){r=new Date().toLocaleTimeString(),t.innerHTML=`
    <main class="shell">
      ${M(`
          ${x(e.summary.live_execution_enabled?`live execute on`:`dry-run gate`,e.summary.live_execution_enabled?`danger`:`neutral`)}
          ${x(e.summary.cache_populated?`cache fresh`:`cache unknown`,e.summary.cache_populated?`ok`:`warn`)}
          <span class="timestamp">Updated ${O(r)}</span>
      `)}

      <section class="metrics" aria-label="Overview">
        ${y(`Runtimes`,e.summary.runtime_count)}
        ${y(`Residents`,e.summary.resident_count)}
        ${y(`Decisions`,e.summary.recent_decision_count)}
        ${y(`Staging`,e.summary.staging.total)}
        ${y(`Active`,e.summary.active_request_count)}
      </section>

      <div class="layout">
        <section class="panel runtimes">
          <div class="panel-header">
            <h2>Runtimes</h2>
            <span>${e.runtimes.count} current / ${e.runtimes.history_count} events</span>
          </div>
          ${f(e.runtimes.items)}
        </section>

        <section class="panel residents">
          <div class="panel-header">
            <h2>Residents</h2>
            <span>${e.summary.resident_count} observed</span>
          </div>
          ${p(e.runtimes.items)}
        </section>

        <section class="panel decisions">
          <div class="panel-header">
            <h2>Decisions</h2>
            <span>${e.decisions.count} recent</span>
          </div>
          ${m(e.decisions.items)}
        </section>

        <section class="panel detail">
          <div class="panel-header">
            <h2>Decision Detail</h2>
            <span>${n?O(n.id.slice(0,8)):`none selected`}</span>
          </div>
          ${h(n)}
        </section>

        <section class="panel staging">
          <div class="panel-header">
            <h2>Staging</h2>
            <span>${C(e.summary.staging)}</span>
          </div>
          ${g(e.staging.items)}
        </section>

        <section class="panel events">
          <div class="panel-header">
            <h2>Events</h2>
            <span>${e.residentEvents.count} resident / ${e.actionPlans.count} plans</span>
          </div>
          ${_(e.residentEvents.items,e.actionPlans.items)}
        </section>
      </div>
    </main>
  `,t.querySelectorAll(`[data-decision-id]`).forEach(e=>{e.addEventListener(`click`,()=>{let t=e.dataset.decisionId;t&&u(t)})}),N()}function f(e){return e.length===0?S(`No reconciled runtime snapshots yet.`):v([`Runtime`,`Availability`,`Freshness`,`Residents`,`Active`,`Last inspection`],e.map(e=>[O(e.runtime_id),x(e.availability,e.availability===`available`?`ok`:`danger`),x(e.freshness,e.freshness===`fresh`?`ok`:`warn`),String(e.resident_count),String(e.active_request_count),D(e.last_inspected)]))}function p(e){let t=e.flatMap(e=>e.snapshot.residents.map(t=>[O(e.runtime_id),O(t.model_id),x(t.state,T(t.state)),E(t),String(e.active_request_count)]));return t.length===0?S(`No resident models reported by runtime inspection.`):v([`Runtime`,`Model`,`State`,`Memory`,`Active`],t)}function m(e){return e.length===0?S(`No decisions recorded yet.`):v([`When`,`Action`,`Selected`,`Runtime`,`Score`,`Explanation`],e.map(e=>[D(e.created_at),x(e.action,w(e.action)),b(e),O(e.selected_runtime??`none`),String(e.score.total),O(e.explanation_summary)]))}function h(e){if(!e)return S(`Select a decision to inspect reasons and rejected options.`);let t=e.explanation.reasons.map(e=>`<li><b>${O(e.code)}</b> ${O(e.detail)} <span>${e.impact}</span></li>`).join(``),n=e.explanation.rejected_options.map(e=>`<li>${O(e.model_id??`unknown`)} on ${O(e.runtime_id??`unknown`)}: ${O(e.reason)}</li>`).join(``);return`
    <div class="detail-grid">
      <div><span>Action</span>${x(e.action,w(e.action))}</div>
      <div><span>Model</span><strong>${O(e.selected_model??`none`)}</strong></div>
      <div><span>Runtime</span><strong>${O(e.selected_runtime??`none`)}</strong></div>
      <div><span>Score</span><strong>${e.score.total}</strong></div>
    </div>
    <p class="summary-line">${O(e.explanation.summary)}</p>
    <h3>Reasons</h3>
    <ul class="event-list">${t||`<li>No reasons recorded.</li>`}</ul>
    <h3>Rejected</h3>
    <ul class="event-list">${n||`<li>No rejected options recorded.</li>`}</ul>
  `}function g(e){return e.length===0?S(`No staging intents queued.`):v([`State`,`Background`,`Runtime`,`Attempts`,`Reason`],e.map(e=>[x(e.state,T(e.state)),O(e.background_model),O(e.target_runtime),String(e.attempt_count),O(e.last_skip_reason??e.last_error??e.reason)]))}function _(e,t){let n=e.slice(0,5).map(e=>`<li>${D(e.observed_at)} ${O(e.model_id)} ${O(e.from_state)} -> ${O(e.to_state)} on ${O(e.runtime_id)}</li>`),r=t.slice(0,5).map(e=>`<li>${D(e.recorded_at)} ${O(e.decision_id.slice(0,8))} ${e.plan.dry_run?`dry-run`:`live`} ${e.plan.actions.length} actions</li>`);return n.length===0&&r.length===0?S(`No durable event history available.`):`
    <h3>Resident transitions</h3>
    <ul class="event-list">${n.join(``)||`<li>No resident transitions.</li>`}</ul>
    <h3>Action plans</h3>
    <ul class="event-list">${r.join(``)||`<li>No action plans recorded.</li>`}</ul>
  `}function v(e,t){return`
    <div class="table-wrap">
      <table>
        <thead><tr>${e.map(e=>`<th>${O(e)}</th>`).join(``)}</tr></thead>
        <tbody>${t.map(e=>`<tr>${e.map(e=>`<td>${e}</td>`).join(``)}</tr>`).join(``)}</tbody>
      </table>
    </div>
  `}function y(e,t){return`<div class="metric"><span>${O(e)}</span><strong>${t}</strong></div>`}function b(e){let t=e.selected_model??e.background_model??`none`;return`<button class="link-button" data-decision-id="${O(e.id)}">${O(t)}</button>`}function x(e,t){return`<span class="chip chip-${t}">${O(k(e))}</span>`}function S(e){return`<p class="empty">${O(e)}</p>`}function C(e){return`${e.pending} pending / ${e.blocked} blocked / ${e.failed} failed`}function w(e){return e===`reuse_hot`||e===`promote_warm`?`ok`:e===`stage_background`||e===`cold_load`?`warn`:e===`deny`||e===`defer`?`danger`:`neutral`}function T(e){return[`hot_gpu`,`serving`,`completed`,`fresh`,`available`].includes(e)?`ok`:[`loading`,`warm_cpu`,`partial`,`pending`,`blocked`,`stale`].includes(e)?`warn`:[`failed`,`evicting`,`unavailable`].includes(e)?`danger`:`neutral`}function E(e){return O([e.vram_mb?`${e.vram_mb} VRAM`:``,e.ram_mb?`${e.ram_mb} RAM`:``,e.kv_cache_mb?`${e.kv_cache_mb} KV`:``].filter(Boolean).join(` / `)||`unknown`)}function D(e){let t=new Date(e);return Number.isNaN(t.getTime())?O(e):O(t.toLocaleTimeString())}function O(e){return e.replaceAll(`&`,`&amp;`).replaceAll(`<`,`&lt;`).replaceAll(`>`,`&gt;`).replaceAll(`"`,`&quot;`).replaceAll(`'`,`&#039;`)}function k(e){return e.replaceAll(`_`,` `)}function A(){let e=new Date().toISOString();return{summary:{cache_populated:!0,runtime_count:1,resident_count:1,unavailable_runtime_count:0,stale_runtime_count:0,active_request_count:0,staging:{total:1,blocked:1,pending:0,failed:0,completed:0},recent_decision_count:1,policy_warnings:[],live_execution_enabled:!1},runtimes:{items:[{runtime_id:`llama_swap`,availability:`available`,freshness:`fresh`,last_inspected:e,last_error:null,resident_count:1,active_request_count:0,configured_model_count:3,snapshot:{runtime_id:`llama_swap`,available:!0,residents:[{model_id:`qwen9b`,state:`hot_gpu`,vram_mb:9e3,ram_mb:12e3}],configured_models:[`qwen9b`,`qwen35_a3b`,`granite8b`],active_requests:[]}}],history:[{event_id:`snapshot-1`,runtime_id:`llama_swap`,observed_at:e}],cache_populated:!0,count:1,history_count:1,limit:25},decisions:{items:[{id:`11111111-1111-4111-8111-111111111111`,request_id:`req-fixture`,action:`stage_background`,selected_model:`qwen9b`,selected_runtime:`llama_swap`,selected_group:`small_swarm`,background_model:`qwen35_a3b`,score:{total:78,contributions:[{label:`hot resident`,value:35}]},explanation_summary:`Selected hot worker now and staged the larger model.`,reason_count:2,rejected_option_count:1,created_at:e}],count:1,limit:25},staging:{items:[{id:`22222222-2222-4222-8222-222222222222`,decision_id:`11111111-1111-4111-8111-111111111111`,foreground_model:`qwen9b`,background_model:`qwen35_a3b`,target_runtime:`llama_swap`,reason:`Background staging for quality upgrade.`,created_at:e,state:`blocked`,last_error:`blocked: target runtime is not mock and ANEMOI_ENABLE_LIVE_EXECUTE is not set`,attempt_count:0,last_skip_reason:null}],history:[],count:1,history_count:0,limit:25},residentEvents:{items:[{model_id:`qwen9b`,runtime_id:`llama_swap`,from_state:`cold`,to_state:`hot_gpu`,observed_at:e,evidence_source:`llama_swap reconciliation round 1`,note:`first observation; prior state inferred as cold`}],count:1,limit:25},actionPlans:{items:[{decision_id:`11111111-1111-4111-8111-111111111111`,recorded_at:e,plan:{dry_run:!0,actions:[{kind:`load`,runtime_id:`llama_swap`,model_id:`qwen35_a3b`,reason:`Background staging load`}]}}],count:1,limit:25}}}function j(e){return{id:e,action:`stage_background`,selected_model:`qwen9b`,selected_runtime:`llama_swap`,score:{total:78,contributions:[{label:`hot resident`,value:35}]},explanation:{summary:`Selected hot worker now and staged the larger model.`,reasons:[{code:`continuity`,detail:`Small worker is already hot inside the latency budget.`,impact:35},{code:`quality_stage`,detail:`Large model should be staged for the next turn.`,impact:22}],rejected_options:[{model_id:`qwen35_a3b`,runtime_id:`llama_swap`,reason:`Cold load exceeds the interactive latency budget.`}]},created_at:new Date().toISOString()}}function M(e){return`
      <header class="topbar">
        <div>
          <h1>Anemoi</h1>
          <nav class="tabs">
            <button class="tab ${a===`telemetry`?`tab-active`:``}" data-view="telemetry">Telemetry</button>
            <button class="tab ${a===`governance`?`tab-active`:``}" data-view="governance">Governance</button>
          </nav>
        </div>
        <div class="topbar-actions">${e}</div>
      </header>`}function N(){t.querySelectorAll(`[data-view]`).forEach(e=>{e.addEventListener(`click`,()=>{let t=e.dataset.view;t&&t!==a&&(a=t,s=``,W())})})}async function P(){if(i)return U();let[e,t,n,r]=await Promise.all([c(`/catalog/models`),c(`/rosters`),c(`/domains`),c(`/policy/validate`)]);return{catalog:e,rosters:t,domains:n,validation:r}}async function F(e,t,n){if(i){s=`Fixture view is read-only; run the daemon to edit governance.`,o&&I(o);return}try{let r=await fetch(t,{method:e,headers:{"content-type":`application/json`,accept:`application/json`},body:n===void 0?void 0:JSON.stringify(n)});if(r.ok)s=`${e} ${t} → ok`;else{let n=await r.json().catch(()=>({}));s=`${e} ${t} → ${r.status}: ${n.error??`error`}`}}catch(n){s=`${e} ${t} failed: ${String(n)}`}await W()}function I(e){r=new Date().toLocaleTimeString(),t.innerHTML=`
    <main class="shell">
      ${M(`
        ${x(e.validation.ok?`policy ok`:`${e.validation.count} warning${e.validation.count===1?``:`s`}`,e.validation.ok?`ok`:`warn`)}
        <span class="timestamp">Updated ${O(r)}</span>
      `)}

      ${s?`<p class="banner">${O(s)}</p>`:``}

      <section class="metrics" aria-label="Governance overview">
        ${y(`Catalog models`,e.catalog.count)}
        ${y(`Rosters`,e.rosters.count)}
        ${y(`Domains`,e.domains.count)}
        ${y(`Warnings`,e.validation.count)}
      </section>

      <div class="layout">
        <section class="panel validation">
          <div class="panel-header"><h2>Validation</h2><span>${e.validation.count} warnings</span></div>
          ${L(e.validation.warnings)}
        </section>

        <section class="panel domains">
          <div class="panel-header"><h2>Domains</h2><span>${e.domains.count}</span></div>
          ${B(e.domains.domains,e.rosters.rosters)}
        </section>

        <section class="panel rosters">
          <div class="panel-header"><h2>Rosters</h2><span>${e.rosters.count}</span></div>
          ${z(e.rosters.rosters,e.catalog.models)}
        </section>

        <section class="panel catalog">
          <div class="panel-header"><h2>Model Catalog</h2><span>${e.catalog.count}</span></div>
          ${R(e.catalog.models)}
        </section>
      </div>
      <p class="empty catalog-note">${O(e.catalog.note)}</p>
    </main>
  `,N(),V()}function L(e){return e.length===0?S(`No governance warnings. Every domain can produce candidates.`):`<ul class="event-list">${e.map(e=>`<li><b>${O(k(e.code))}</b> <span>${O(e.target)}</span> ${O(e.detail)}</li>`).join(``)}</ul>`}function R(e){return e.length===0?S(`No models discovered from runtime catalogs yet.`):v([`Model`,`Runtime`,`Source`,`Residency`,`Class`,`Metadata`,`In rosters`],e.map(e=>[O(e.model_id),O(e.runtime_id),O(e.source_adapter),e.resident_state?x(e.resident_state,T(e.resident_state)):x(`configured only`,`neutral`),O(e.parameter_class),x(e.metadata_source,e.metadata_source===`operator_override`?`ok`:`neutral`),e.in_rosters.length?O(e.in_rosters.join(`, `)):`—`]))}function z(e,t){let n=t.map(e=>`<option value="${O(e.model_id)}">${O(e.model_id)}</option>`).join(``);return`
    <div class="roster-create">
      <input type="text" data-role="new-roster-id" placeholder="new-roster-id"/>
      <button class="link-button" data-action="create-roster">create roster</button>
    </div>
    ${e.map(e=>`
      <div class="roster-card">
        <div class="roster-head">
          <strong>${O(e.id)}</strong>
          <button class="link-button danger-link" data-action="delete-roster" data-roster="${O(e.id)}">delete</button>
        </div>
        <div class="roster-flags">
          <label><input type="checkbox" data-action="flag" data-flag="keep_hot" data-roster="${O(e.id)}" ${e.keep_hot?`checked`:``}/> keep hot</label>
          <label><input type="checkbox" data-action="flag" data-flag="allow_background_load" data-roster="${O(e.id)}" ${e.allow_background_load?`checked`:``}/> bg load</label>
          <label><input type="checkbox" data-action="flag" data-flag="pinned" data-roster="${O(e.id)}" ${e.pinned?`checked`:``}/> pinned</label>
        </div>
        <div class="roster-models">
          ${e.models.length?e.models.map(t=>`<span class="chip chip-neutral">${O(t)}<button class="chip-x" data-action="remove-model" data-roster="${O(e.id)}" data-model="${O(t)}" title="remove">×</button></span>`).join(``):`<span class="empty">empty roster</span>`}
        </div>
        <div class="roster-add">
          <select data-role="add-model-select" data-roster="${O(e.id)}"><option value="">add model…</option>${n}</select>
          <button class="link-button" data-action="add-model" data-roster="${O(e.id)}">add</button>
        </div>
        <div class="roster-used">used by: ${e.used_by_domains.length?O(e.used_by_domains.join(`, `)):`no domains`}</div>
      </div>`).join(``)||S(`No rosters yet.`)}
  `}function B(e,t){return e.length===0?S(`No domains configured.`):e.map(e=>{if(e.live_roster)return`<div class="domain-card"><strong>${O(e.id)}</strong> ${x(`live: ${e.live_roster}`,`neutral`)}</div>`;let n=t.map(t=>`<label><input type="checkbox" data-role="domain-roster" data-domain="${O(e.id)}" value="${O(t.id)}" ${e.rosters.includes(t.id)?`checked`:``}/> ${O(t.id)}</label>`).join(``);return`
      <div class="domain-card">
        <strong>${O(e.id)}</strong>
        <div class="domain-rosters">${n||`<span class="empty">no rosters defined</span>`}</div>
        <button class="link-button" data-action="assign-domain" data-domain="${O(e.id)}">apply rosters</button>
      </div>`}).join(``)}function V(){let e=e=>Array.from(t.querySelectorAll(e));e(`[data-action="create-roster"]`).forEach(e=>e.addEventListener(`click`,()=>{let e=t.querySelector(`[data-role="new-roster-id"]`)?.value.trim();e&&F(`POST`,`/rosters`,{id:e})})),e(`[data-action="delete-roster"]`).forEach(e=>e.addEventListener(`click`,()=>{let t=e.dataset.roster;t&&window.confirm(`Delete roster ${t}?`)&&F(`DELETE`,`/rosters/${encodeURIComponent(t)}`)})),e(`[data-action="add-model"]`).forEach(e=>e.addEventListener(`click`,()=>{let n=e.dataset.roster??``,r=t.querySelector(`[data-role="add-model-select"][data-roster="${H(n)}"]`)?.value;n&&r&&F(`POST`,`/rosters/${encodeURIComponent(n)}/models`,{model_id:r})})),e(`[data-action="remove-model"]`).forEach(e=>e.addEventListener(`click`,()=>{let t=e.dataset.roster??``,n=e.dataset.model??``;t&&n&&F(`DELETE`,`/rosters/${encodeURIComponent(t)}/models/${encodeURIComponent(n)}`)})),e(`[data-action="flag"]`).forEach(e=>e.addEventListener(`change`,()=>{let t=e.dataset.roster??``,n=e.dataset.flag??``;t&&n&&F(`PATCH`,`/rosters/${encodeURIComponent(t)}`,{[n]:e.checked})})),e(`[data-action="assign-domain"]`).forEach(t=>t.addEventListener(`click`,()=>{let n=t.dataset.domain??``,r=e(`[data-role="domain-roster"][data-domain="${H(n)}"]`).filter(e=>e.checked).map(e=>e.value);n&&F(`PATCH`,`/domains/${encodeURIComponent(n)}/rosters`,{rosters:r})}))}function H(e){return e.replace(/["\\]/g,`\\$&`)}function U(){return{catalog:{models:[{model_id:`qwen9b`,runtime_id:`llama_swap`,source_adapter:`llama_swap`,configured:!0,resident_state:`hot_gpu`,family:`qwen`,parameter_class:`9b`,context_window:32768,supports_streaming:!0,metadata_source:`config`,in_rosters:[`fast`]},{model_id:`qwen35b`,runtime_id:`llama_swap`,source_adapter:`llama_swap`,configured:!0,resident_state:null,family:`qwen`,parameter_class:`35b`,context_window:32768,supports_streaming:!0,metadata_source:`config`,in_rosters:[`big`]},{model_id:`gemma-4-31b-it`,runtime_id:`llama_swap`,source_adapter:`llama_swap`,configured:!0,resident_state:null,family:`gemma`,parameter_class:`27b`,context_window:262144,supports_streaming:!0,metadata_source:`operator_override`,in_rosters:[]}],count:3,note:`Catalog/configured models are discovered from runtime catalogs and are NOT residency evidence. resident_state is set only when a model is separately observed loaded.`},rosters:{rosters:[{id:`fast`,purpose:[`coding`,`interactive`],models:[`qwen9b`],keep_hot:!0,allow_background_load:!0,pinned:!1,model_count:1,used_by_domains:[`coding`]},{id:`big`,purpose:[`escalation`],models:[`qwen35b`],keep_hot:!1,allow_background_load:!0,pinned:!1,model_count:1,used_by_domains:[]}],count:2},domains:{domains:[{id:`coding`,rosters:[`fast`],live_roster:null},{id:`coding-full`,rosters:[],live_roster:`llama_swap`}],count:2},validation:{ok:!1,warnings:[{code:`domain.no_escalation`,target:`coding`,detail:`domain coding cannot escalate beyond 9B: its largest roster model is 9B; add a larger model to enable escalation`}],count:1}}}async function W(){try{a===`governance`?(o=await P(),I(o)):d(await l())}catch(e){t.innerHTML=`<main class="shell">${M(``)}<section class="panel error"><p>${O(String(e))}</p></section></main>`,N()}}W(),window.setInterval(()=>{a===`telemetry`&&W()},5e3);