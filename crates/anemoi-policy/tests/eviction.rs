//! Eviction ranking behavior (issue #85): unknown idle time must rank last.

use anemoi_core::{ModelId, ResidencyState, RuntimeId};
use anemoi_policy::{plan_evictions, EvictionCandidateResident, EvictionPlan, EvictionRequest};

fn resident(model: &str, idle_secs: Option<u64>) -> EvictionCandidateResident {
    EvictionCandidateResident {
        model_id: ModelId(model.to_string()),
        runtime_id: RuntimeId("rt".to_string()),
        state: ResidencyState::HotGpu,
        keep_hot: false,
        pinned: false,
        idle_secs,
    }
}

fn candidate_order(plan: &EvictionPlan) -> Vec<String> {
    plan.candidates
        .iter()
        .map(|c| c.model_id.to_string())
        .collect()
}

#[test]
fn eviction_unknown_idle_ranks_last() {
    // "aardvark" has unknown idle and sorts alphabetically before "zebra",
    // which has a known idle of 0. Despite the alphabetical edge, the unknown
    // resident must rank last (it is the worst eviction candidate, not the best).
    let residents = [resident("aardvark", None), resident("zebra", Some(0))];
    let plan = plan_evictions(&EvictionRequest {
        residents: &residents,
        force: false,
    });

    assert_eq!(
        candidate_order(&plan),
        vec!["zebra".to_string(), "aardvark".to_string()]
    );
}

#[test]
fn eviction_orders_known_idle_descending_then_unknown() {
    // Most-idle known resident first, then descending by idle time, with all
    // unknown-idle residents last (ordered among themselves by model id).
    let residents = [
        resident("alpha", None),
        resident("bravo", Some(5)),
        resident("charlie", Some(120)),
        resident("delta", None),
    ];
    let plan = plan_evictions(&EvictionRequest {
        residents: &residents,
        force: false,
    });

    assert_eq!(
        candidate_order(&plan),
        vec![
            "charlie".to_string(), // 120s idle, best candidate
            "bravo".to_string(),   // 5s idle
            "alpha".to_string(),   // unknown idle, model-id tiebreak
            "delta".to_string(),   // unknown idle
        ]
    );
}
