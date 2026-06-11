(function(){let e=document.createElement(`link`).relList;if(e&&e.supports&&e.supports(`modulepreload`))return;for(let e of document.querySelectorAll(`link[rel="modulepreload"]`))n(e);new MutationObserver(e=>{for(let t of e)if(t.type===`childList`)for(let e of t.addedNodes)e.tagName===`LINK`&&e.rel===`modulepreload`&&n(e)}).observe(document,{childList:!0,subtree:!0});function t(e){let t={};return e.integrity&&(t.integrity=e.integrity),e.referrerPolicy&&(t.referrerPolicy=e.referrerPolicy),e.crossOrigin===`use-credentials`?t.credentials=`include`:e.crossOrigin===`anonymous`?t.credentials=`omit`:t.credentials=`same-origin`,t}function n(e){if(e.ep)return;e.ep=!0;let n=t(e);fetch(e.href,n)}})();var e=document.querySelector(`#anemoi-dashboard`);if(!e)throw Error(`missing dashboard root`);var t=e,n=null,r=``,i=new URLSearchParams(window.location.search).get(`fixture`)===`1`;async function a(e){let t=await fetch(e,{headers:{accept:`application/json`}});if(!t.ok)throw Error(`${e} returned ${t.status}`);return await t.json()}async function o(){if(i)return D();let[e,t,n,r,o,s]=await Promise.all([a(`/telemetry/summary`),a(`/telemetry/runtime-snapshots?limit=25`),a(`/telemetry/decisions?limit=25`),a(`/telemetry/staging-events?limit=25`),a(`/telemetry/resident-events?limit=25`),a(`/telemetry/action-plans?limit=25`)]);return{summary:e,runtimes:t,decisions:n,staging:r,residentEvents:o,actionPlans:s}}async function s(e){n=i?O(e):await a(`/telemetry/decision/${e}`),c(await o())}function c(e){r=new Date().toLocaleTimeString(),t.innerHTML=`
    <main class="shell">
      <header class="topbar">
        <div>
          <h1>Anemoi</h1>
          <p>Telemetry dashboard</p>
        </div>
        <div class="topbar-actions">
          ${v(e.summary.live_execution_enabled?`live execute on`:`dry-run gate`,e.summary.live_execution_enabled?`danger`:`neutral`)}
          ${v(e.summary.cache_populated?`cache fresh`:`cache unknown`,e.summary.cache_populated?`ok`:`warn`)}
          <span class="timestamp">Updated ${T(r)}</span>
        </div>
      </header>

      <section class="metrics" aria-label="Overview">
        ${g(`Runtimes`,e.summary.runtime_count)}
        ${g(`Residents`,e.summary.resident_count)}
        ${g(`Decisions`,e.summary.recent_decision_count)}
        ${g(`Staging`,e.summary.staging.total)}
        ${g(`Active`,e.summary.active_request_count)}
      </section>

      <div class="layout">
        <section class="panel runtimes">
          <div class="panel-header">
            <h2>Runtimes</h2>
            <span>${e.runtimes.count} current / ${e.runtimes.history_count} events</span>
          </div>
          ${l(e.runtimes.items)}
        </section>

        <section class="panel residents">
          <div class="panel-header">
            <h2>Residents</h2>
            <span>${e.summary.resident_count} observed</span>
          </div>
          ${u(e.runtimes.items)}
        </section>

        <section class="panel decisions">
          <div class="panel-header">
            <h2>Decisions</h2>
            <span>${e.decisions.count} recent</span>
          </div>
          ${d(e.decisions.items)}
        </section>

        <section class="panel detail">
          <div class="panel-header">
            <h2>Decision Detail</h2>
            <span>${n?T(n.id.slice(0,8)):`none selected`}</span>
          </div>
          ${f(n)}
        </section>

        <section class="panel staging">
          <div class="panel-header">
            <h2>Staging</h2>
            <span>${b(e.summary.staging)}</span>
          </div>
          ${p(e.staging.items)}
        </section>

        <section class="panel events">
          <div class="panel-header">
            <h2>Events</h2>
            <span>${e.residentEvents.count} resident / ${e.actionPlans.count} plans</span>
          </div>
          ${m(e.residentEvents.items,e.actionPlans.items)}
        </section>
      </div>
    </main>
  `,t.querySelectorAll(`[data-decision-id]`).forEach(e=>{e.addEventListener(`click`,()=>{let t=e.dataset.decisionId;t&&s(t)})})}function l(e){return e.length===0?y(`No reconciled runtime snapshots yet.`):h([`Runtime`,`Availability`,`Freshness`,`Residents`,`Active`,`Last inspection`],e.map(e=>[T(e.runtime_id),v(e.availability,e.availability===`available`?`ok`:`danger`),v(e.freshness,e.freshness===`fresh`?`ok`:`warn`),String(e.resident_count),String(e.active_request_count),w(e.last_inspected)]))}function u(e){let t=e.flatMap(e=>e.snapshot.residents.map(t=>[T(e.runtime_id),T(t.model_id),v(t.state,S(t.state)),C(t),String(e.active_request_count)]));return t.length===0?y(`No resident models reported by runtime inspection.`):h([`Runtime`,`Model`,`State`,`Memory`,`Active`],t)}function d(e){return e.length===0?y(`No decisions recorded yet.`):h([`When`,`Action`,`Selected`,`Runtime`,`Score`,`Explanation`],e.map(e=>[w(e.created_at),v(e.action,x(e.action)),_(e),T(e.selected_runtime??`none`),String(e.score.total),T(e.explanation_summary)]))}function f(e){if(!e)return y(`Select a decision to inspect reasons and rejected options.`);let t=e.explanation.reasons.map(e=>`<li><b>${T(e.code)}</b> ${T(e.detail)} <span>${e.impact}</span></li>`).join(``),n=e.explanation.rejected_options.map(e=>`<li>${T(e.model_id??`unknown`)} on ${T(e.runtime_id??`unknown`)}: ${T(e.reason)}</li>`).join(``);return`
    <div class="detail-grid">
      <div><span>Action</span>${v(e.action,x(e.action))}</div>
      <div><span>Model</span><strong>${T(e.selected_model??`none`)}</strong></div>
      <div><span>Runtime</span><strong>${T(e.selected_runtime??`none`)}</strong></div>
      <div><span>Score</span><strong>${e.score.total}</strong></div>
    </div>
    <p class="summary-line">${T(e.explanation.summary)}</p>
    <h3>Reasons</h3>
    <ul class="event-list">${t||`<li>No reasons recorded.</li>`}</ul>
    <h3>Rejected</h3>
    <ul class="event-list">${n||`<li>No rejected options recorded.</li>`}</ul>
  `}function p(e){return e.length===0?y(`No staging intents queued.`):h([`State`,`Background`,`Runtime`,`Attempts`,`Reason`],e.map(e=>[v(e.state,S(e.state)),T(e.background_model),T(e.target_runtime),String(e.attempt_count),T(e.last_skip_reason??e.last_error??e.reason)]))}function m(e,t){let n=e.slice(0,5).map(e=>`<li>${w(e.observed_at)} ${T(e.model_id)} ${T(e.from_state)} -> ${T(e.to_state)} on ${T(e.runtime_id)}</li>`),r=t.slice(0,5).map(e=>`<li>${w(e.recorded_at)} ${T(e.decision_id.slice(0,8))} ${e.plan.dry_run?`dry-run`:`live`} ${e.plan.actions.length} actions</li>`);return n.length===0&&r.length===0?y(`No durable event history available.`):`
    <h3>Resident transitions</h3>
    <ul class="event-list">${n.join(``)||`<li>No resident transitions.</li>`}</ul>
    <h3>Action plans</h3>
    <ul class="event-list">${r.join(``)||`<li>No action plans recorded.</li>`}</ul>
  `}function h(e,t){return`
    <div class="table-wrap">
      <table>
        <thead><tr>${e.map(e=>`<th>${T(e)}</th>`).join(``)}</tr></thead>
        <tbody>${t.map(e=>`<tr>${e.map(e=>`<td>${e}</td>`).join(``)}</tr>`).join(``)}</tbody>
      </table>
    </div>
  `}function g(e,t){return`<div class="metric"><span>${T(e)}</span><strong>${t}</strong></div>`}function _(e){let t=e.selected_model??e.background_model??`none`;return`<button class="link-button" data-decision-id="${T(e.id)}">${T(t)}</button>`}function v(e,t){return`<span class="chip chip-${t}">${T(E(e))}</span>`}function y(e){return`<p class="empty">${T(e)}</p>`}function b(e){return`${e.pending} pending / ${e.blocked} blocked / ${e.failed} failed`}function x(e){return e===`reuse_hot`||e===`promote_warm`?`ok`:e===`stage_background`||e===`cold_load`?`warn`:e===`deny`||e===`defer`?`danger`:`neutral`}function S(e){return[`hot_gpu`,`serving`,`completed`,`fresh`,`available`].includes(e)?`ok`:[`loading`,`warm_cpu`,`partial`,`pending`,`blocked`,`stale`].includes(e)?`warn`:[`failed`,`evicting`,`unavailable`].includes(e)?`danger`:`neutral`}function C(e){return T([e.vram_mb?`${e.vram_mb} VRAM`:``,e.ram_mb?`${e.ram_mb} RAM`:``,e.kv_cache_mb?`${e.kv_cache_mb} KV`:``].filter(Boolean).join(` / `)||`unknown`)}function w(e){let t=new Date(e);return Number.isNaN(t.getTime())?T(e):T(t.toLocaleTimeString())}function T(e){return e.replaceAll(`&`,`&amp;`).replaceAll(`<`,`&lt;`).replaceAll(`>`,`&gt;`).replaceAll(`"`,`&quot;`).replaceAll(`'`,`&#039;`)}function E(e){return e.replaceAll(`_`,` `)}function D(){let e=new Date().toISOString();return{summary:{cache_populated:!0,runtime_count:1,resident_count:1,unavailable_runtime_count:0,stale_runtime_count:0,active_request_count:0,staging:{total:1,blocked:1,pending:0,failed:0,completed:0},recent_decision_count:1,policy_warnings:[],live_execution_enabled:!1},runtimes:{items:[{runtime_id:`llama_swap`,availability:`available`,freshness:`fresh`,last_inspected:e,last_error:null,resident_count:1,active_request_count:0,configured_model_count:3,snapshot:{runtime_id:`llama_swap`,available:!0,residents:[{model_id:`qwen9b`,state:`hot_gpu`,vram_mb:9e3,ram_mb:12e3}],configured_models:[`qwen9b`,`qwen35_a3b`,`granite8b`],active_requests:[]}}],history:[{event_id:`snapshot-1`,runtime_id:`llama_swap`,observed_at:e}],cache_populated:!0,count:1,history_count:1,limit:25},decisions:{items:[{id:`11111111-1111-4111-8111-111111111111`,request_id:`req-fixture`,action:`stage_background`,selected_model:`qwen9b`,selected_runtime:`llama_swap`,selected_group:`small_swarm`,background_model:`qwen35_a3b`,score:{total:78,contributions:[{label:`hot resident`,value:35}]},explanation_summary:`Selected hot worker now and staged the larger model.`,reason_count:2,rejected_option_count:1,created_at:e}],count:1,limit:25},staging:{items:[{id:`22222222-2222-4222-8222-222222222222`,decision_id:`11111111-1111-4111-8111-111111111111`,foreground_model:`qwen9b`,background_model:`qwen35_a3b`,target_runtime:`llama_swap`,reason:`Background staging for quality upgrade.`,created_at:e,state:`blocked`,last_error:`blocked: target runtime is not mock and ANEMOI_ENABLE_LIVE_EXECUTE is not set`,attempt_count:0,last_skip_reason:null}],history:[],count:1,history_count:0,limit:25},residentEvents:{items:[{model_id:`qwen9b`,runtime_id:`llama_swap`,from_state:`cold`,to_state:`hot_gpu`,observed_at:e,evidence_source:`llama_swap reconciliation round 1`,note:`first observation; prior state inferred as cold`}],count:1,limit:25},actionPlans:{items:[{decision_id:`11111111-1111-4111-8111-111111111111`,recorded_at:e,plan:{dry_run:!0,actions:[{kind:`load`,runtime_id:`llama_swap`,model_id:`qwen35_a3b`,reason:`Background staging load`}]}}],count:1,limit:25}}}function O(e){return{id:e,action:`stage_background`,selected_model:`qwen9b`,selected_runtime:`llama_swap`,score:{total:78,contributions:[{label:`hot resident`,value:35}]},explanation:{summary:`Selected hot worker now and staged the larger model.`,reasons:[{code:`continuity`,detail:`Small worker is already hot inside the latency budget.`,impact:35},{code:`quality_stage`,detail:`Large model should be staged for the next turn.`,impact:22}],rejected_options:[{model_id:`qwen35_a3b`,runtime_id:`llama_swap`,reason:`Cold load exceeds the interactive latency budget.`}]},created_at:new Date().toISOString()}}async function k(){try{c(await o())}catch(e){t.innerHTML=`<main class="shell"><section class="panel error"><h1>Anemoi</h1><p>${T(String(e))}</p></section></main>`}}k(),window.setInterval(()=>{k()},5e3);