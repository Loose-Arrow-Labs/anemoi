//! Concurrent residency transition coordination (issue #79).
//!
//! When Anemoi runs across multiple instances — or coordinates multiple request
//! streams — concurrent requests can ask for incompatible residency states for
//! the same runtime: one wants to keep a model hot, another wants a cold-load,
//! a third wants to switch the runtime to a different model. This module makes
//! those collisions explicit instead of allowing duplicate loads, unexplained
//! waits, or contradictory control decisions.
//!
//! It is **scheduling/control-plane policy only**. It decides whether a request
//! is served now, joined to existing work, degraded to a hot worker, staged,
//! rejected, or taken over — and always explains why. It never executes a load
//! or mutates a runtime; that stays behind the runtime-adapter boundary.
//!
//! Multi-instance ownership is protected by a code-enforced lease: each active
//! transition records its `owner_instance`, a monotonic `fencing_token`, and a
//! `lease_expires_at_ms`. Another instance may only take over an expired lease,
//! and a take-over always advances the fencing token so a stale owner that
//! resumes is fenced off (its old token can no longer complete the transition).

use anemoi_core::{DecisionReason, ModelId, RejectedOption, ResidencyGroupId, RuntimeId};
use std::cmp::Ordering;
use std::collections::HashMap;

/// A request to bring a runtime's residency to `desired_model`.
#[derive(Debug, Clone)]
pub struct TransitionRequest {
    pub runtime_id: RuntimeId,
    pub residency_group_id: ResidencyGroupId,
    pub desired_model: ModelId,
    /// The Anemoi instance making the request (multi-instance ownership).
    pub owner_instance: String,
    /// Higher priority wins a conflict.
    pub priority: u32,
    pub latency_budget_ms: Option<u64>,
    /// Interactive requests outrank background ones at equal priority.
    pub interactive: bool,
    /// True when this is a background pre-stage rather than an immediate need.
    pub background: bool,
    /// Whether the target runtime is currently available (from inspection).
    pub runtime_available: bool,
    /// A hot, policy-compatible worker the caller may fall back to instead of
    /// waiting on a conflicting cold-load. `Some` only when continuity policy has
    /// already decided a hot fallback is acceptable (an interactive request with
    /// a tight latency budget and a compatible hot worker already resident).
    pub hot_compatible_model: Option<ModelId>,
}

/// An in-flight transition that owns a runtime under a fenced lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTransition {
    pub runtime_id: RuntimeId,
    pub residency_group_id: ResidencyGroupId,
    pub desired_model: ModelId,
    pub owner_instance: String,
    /// Monotonic per-coordinator revision. A take-over always advances it.
    pub fencing_token: u64,
    pub lease_expires_at_ms: u64,
    /// Active serving leases held against this transition's model. A transition
    /// with serving leases is protected from preemption.
    pub serving_leases: u32,
    priority: u32,
    interactive: bool,
}

/// The arbitration outcome for a transition request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionPath {
    /// Reused an existing transition already loading the same model — no
    /// duplicate work started.
    Joined { fencing_token: u64 },
    /// Started a new transition; the requester is now the owner.
    Started { fencing_token: u64 },
    /// Scheduled a background stage; the requester owns the new transition.
    Staged { fencing_token: u64 },
    /// Took over an expired (or preempted) owner's transition, fenced with a new
    /// token.
    TookOver {
        fencing_token: u64,
        previous_owner: String,
    },
    /// Continuity fallback: served an already-hot compatible worker now instead
    /// of waiting on the conflicting in-flight cold-load. The active transition
    /// is left undisturbed.
    ServedHot { model: ModelId },
    /// Lost a conflict to another owner; no work started.
    Rejected { winner_instance: String },
    /// The runtime is unavailable; the request cannot proceed now.
    RuntimeUnavailable,
}

/// A transition decision: the chosen path plus a structured explanation listing
/// the reasons and the rejected/conflicting options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionDecision {
    pub path: TransitionPath,
    pub reasons: Vec<DecisionReason>,
    pub rejected: Vec<RejectedOption>,
}

impl TransitionDecision {
    /// True when at least one collision/transition reason is recorded. Every
    /// decision this coordinator returns carries a reason — see the
    /// `every_transition_decision_carries_explanation` test.
    pub fn is_explained(&self) -> bool {
        !self.reasons.is_empty()
    }
}

fn reason(code: &str, detail: String) -> DecisionReason {
    DecisionReason {
        code: code.to_string(),
        detail,
        impact: 0,
    }
}

/// Coordinates residency transitions for a set of runtimes. One transition may
/// own a given runtime at a time; collisions are arbitrated by policy inputs.
#[derive(Debug, Clone)]
pub struct TransitionCoordinator {
    lease_ttl_ms: u64,
    active: HashMap<String, ActiveTransition>,
    next_fencing_token: u64,
}

impl TransitionCoordinator {
    pub fn new(lease_ttl_ms: u64) -> Self {
        Self {
            lease_ttl_ms,
            active: HashMap::new(),
            next_fencing_token: 1,
        }
    }

    /// The active transition for a runtime, if any (regardless of lease expiry).
    pub fn active_transition(&self, runtime_id: &RuntimeId) -> Option<&ActiveTransition> {
        self.active.get(&runtime_id.0)
    }

    fn issue_token(&mut self) -> u64 {
        let token = self.next_fencing_token;
        self.next_fencing_token += 1;
        token
    }

    fn install(&mut self, req: &TransitionRequest, now_ms: u64) -> u64 {
        let fencing_token = self.issue_token();
        self.active.insert(
            req.runtime_id.0.clone(),
            ActiveTransition {
                runtime_id: req.runtime_id.clone(),
                residency_group_id: req.residency_group_id.clone(),
                desired_model: req.desired_model.clone(),
                owner_instance: req.owner_instance.clone(),
                fencing_token,
                lease_expires_at_ms: now_ms + self.lease_ttl_ms,
                serving_leases: 0,
                priority: req.priority,
                interactive: req.interactive,
            },
        );
        fencing_token
    }

    /// Arbitrate a transition request against the current active transition for
    /// its runtime. Pure given `now_ms`; mutates only the coordinator's lease
    /// table. Never performs a load.
    pub fn request_transition(
        &mut self,
        req: TransitionRequest,
        now_ms: u64,
    ) -> TransitionDecision {
        if !req.runtime_available {
            return TransitionDecision {
                path: TransitionPath::RuntimeUnavailable,
                reasons: vec![reason(
                    "transition.runtime_unavailable",
                    format!(
                        "runtime {} is unavailable; {} cannot transition to {} now",
                        req.runtime_id, req.owner_instance, req.desired_model
                    ),
                )],
                rejected: vec![RejectedOption {
                    model_id: Some(req.desired_model.clone()),
                    runtime_id: Some(req.runtime_id.clone()),
                    reason: "target runtime is unavailable".to_string(),
                }],
            };
        }

        let active = self.active.get(&req.runtime_id.0).cloned();
        match active {
            None => self.start_fresh(req, now_ms),
            Some(active) if now_ms >= active.lease_expires_at_ms => {
                self.take_over(req, now_ms, &active, "owner lease expired")
            }
            Some(active) if active.desired_model == req.desired_model => self.join(req, &active),
            Some(active) => self.resolve_conflict(req, now_ms, &active),
        }
    }

    fn start_fresh(&mut self, req: TransitionRequest, now_ms: u64) -> TransitionDecision {
        let background = req.background;
        let token = self.install(&req, now_ms);
        let (path, code, detail) = if background {
            (
                TransitionPath::Staged {
                    fencing_token: token,
                },
                "transition.staged",
                format!(
                    "{} staged background load of {} on {} (no transition in flight); fencing token {}",
                    req.owner_instance, req.desired_model, req.runtime_id, token
                ),
            )
        } else {
            (
                TransitionPath::Started {
                    fencing_token: token,
                },
                "transition.started",
                format!(
                    "{} started transition of {} to {}; fencing token {}",
                    req.owner_instance, req.runtime_id, req.desired_model, token
                ),
            )
        };
        TransitionDecision {
            path,
            reasons: vec![reason(code, detail)],
            rejected: Vec::new(),
        }
    }

    fn join(&self, req: TransitionRequest, active: &ActiveTransition) -> TransitionDecision {
        TransitionDecision {
            path: TransitionPath::Joined {
                fencing_token: active.fencing_token,
            },
            reasons: vec![reason(
                "transition.joined",
                format!(
                    "{} joined the in-flight transition of {} to {} owned by {} (fencing token {}); no duplicate load started",
                    req.owner_instance,
                    req.runtime_id,
                    req.desired_model,
                    active.owner_instance,
                    active.fencing_token
                ),
            )],
            rejected: Vec::new(),
        }
    }

    fn take_over(
        &mut self,
        req: TransitionRequest,
        now_ms: u64,
        previous: &ActiveTransition,
        why: &str,
    ) -> TransitionDecision {
        let previous_owner = previous.owner_instance.clone();
        let token = self.install(&req, now_ms);
        TransitionDecision {
            path: TransitionPath::TookOver {
                fencing_token: token,
                previous_owner: previous_owner.clone(),
            },
            reasons: vec![reason(
                "transition.took_over",
                format!(
                    "{} took over {} from {} ({}); previous fencing token {} fenced off, new token {}",
                    req.owner_instance,
                    req.runtime_id,
                    previous_owner,
                    why,
                    previous.fencing_token,
                    token
                ),
            )],
            rejected: vec![RejectedOption {
                model_id: Some(previous.desired_model.clone()),
                runtime_id: Some(req.runtime_id.clone()),
                reason: format!("previous owner {previous_owner} {why}"),
            }],
        }
    }

    fn resolve_conflict(
        &mut self,
        req: TransitionRequest,
        now_ms: u64,
        active: &ActiveTransition,
    ) -> TransitionDecision {
        // Continuity fallback: an interactive request with a compatible hot
        // worker available need not wait on the conflicting cold-load. Serve the
        // hot worker now and leave the active transition undisturbed.
        if req.interactive {
            if let Some(hot) = &req.hot_compatible_model {
                return TransitionDecision {
                    path: TransitionPath::ServedHot { model: hot.clone() },
                    reasons: vec![reason(
                        "transition.served_hot",
                        format!(
                            "{} served hot worker {} now instead of waiting on {}'s in-flight load of {} on {} (continuity over a blank wait)",
                            req.owner_instance, hot, active.owner_instance, active.desired_model, req.runtime_id
                        ),
                    )],
                    rejected: vec![RejectedOption {
                        model_id: Some(req.desired_model.clone()),
                        runtime_id: Some(req.runtime_id.clone()),
                        reason: format!(
                            "would collide with {}'s in-flight transition to {}; served hot {} instead",
                            active.owner_instance, active.desired_model, hot
                        ),
                    }],
                };
            }
        }

        // Background staging must not violate an active transition lease.
        if req.background {
            return TransitionDecision {
                path: TransitionPath::Rejected {
                    winner_instance: active.owner_instance.clone(),
                },
                reasons: vec![reason(
                    "transition.stage_blocked",
                    format!(
                        "background stage of {} on {} blocked: {} holds an in-flight transition to {} (fencing token {})",
                        req.desired_model, req.runtime_id, active.owner_instance, active.desired_model, active.fencing_token
                    ),
                )],
                rejected: vec![RejectedOption {
                    model_id: Some(req.desired_model.clone()),
                    runtime_id: Some(req.runtime_id.clone()),
                    reason: format!(
                        "active transition to {} owned by {} must not be displaced by a background stage",
                        active.desired_model, active.owner_instance
                    ),
                }],
            };
        }

        // Deterministic winner for a genuine conflict.
        if incoming_wins(&req, active) {
            self.take_over(req, now_ms, active, "won the conflict on policy inputs")
        } else {
            let serving_note = if active.serving_leases > 0 {
                format!(
                    " ({} active serving lease(s) protect it from preemption)",
                    active.serving_leases
                )
            } else {
                String::new()
            };
            TransitionDecision {
                path: TransitionPath::Rejected {
                    winner_instance: active.owner_instance.clone(),
                },
                reasons: vec![reason(
                    "transition.rejected",
                    format!(
                        "{}'s request for {} on {} lost to {}'s in-flight transition to {}{}",
                        req.owner_instance,
                        req.desired_model,
                        req.runtime_id,
                        active.owner_instance,
                        active.desired_model,
                        serving_note
                    ),
                )],
                rejected: vec![RejectedOption {
                    model_id: Some(req.desired_model.clone()),
                    runtime_id: Some(req.runtime_id.clone()),
                    reason: format!(
                        "conflicting transition to {} owned by {} won on policy inputs",
                        active.desired_model, active.owner_instance
                    ),
                }],
            }
        }
    }

    /// Records an active serving lease against a runtime's transition, protecting
    /// it from preemption while requests are being served. Returns false when
    /// there is no active transition to lease against.
    pub fn acquire_serving_lease(&mut self, runtime_id: &RuntimeId) -> bool {
        match self.active.get_mut(&runtime_id.0) {
            Some(active) => {
                active.serving_leases += 1;
                true
            }
            None => false,
        }
    }

    /// Releases a previously acquired serving lease.
    pub fn release_serving_lease(&mut self, runtime_id: &RuntimeId) {
        if let Some(active) = self.active.get_mut(&runtime_id.0) {
            active.serving_leases = active.serving_leases.saturating_sub(1);
        }
    }

    /// Completes (and clears) the active transition for a runtime — but only for
    /// the current fencing token. A stale owner whose lease was taken over holds
    /// an older token and is fenced off: its completion is rejected, so it cannot
    /// clobber the new owner's transition. Returns true when the completion was
    /// accepted.
    pub fn complete(&mut self, runtime_id: &RuntimeId, fencing_token: u64) -> bool {
        match self.active.get(&runtime_id.0) {
            Some(active) if active.fencing_token == fencing_token => {
                self.active.remove(&runtime_id.0);
                true
            }
            _ => false,
        }
    }
}

/// Deterministic conflict winner: does `incoming` preempt the `active` owner?
///
/// Serving leases protect the active transition absolutely. Otherwise the same
/// owner always wins its own runtime; across instances, higher priority wins,
/// then interactive over background, then the lexicographically smaller instance
/// id (an arbitrary but stable tiebreak so the outcome never depends on timing
/// or map iteration order).
fn incoming_wins(incoming: &TransitionRequest, active: &ActiveTransition) -> bool {
    if active.serving_leases > 0 {
        return false;
    }
    if incoming.owner_instance == active.owner_instance {
        return true;
    }
    match incoming.priority.cmp(&active.priority) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => match (incoming.interactive, active.interactive) {
            (true, false) => true,
            (false, true) => false,
            _ => incoming.owner_instance < active.owner_instance,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(instance: &str, model: &str) -> TransitionRequest {
        TransitionRequest {
            runtime_id: RuntimeId("rt".to_string()),
            residency_group_id: ResidencyGroupId("grp".to_string()),
            desired_model: ModelId(model.to_string()),
            owner_instance: instance.to_string(),
            priority: 5,
            latency_budget_ms: Some(1500),
            interactive: true,
            background: false,
            runtime_available: true,
            hot_compatible_model: None,
        }
    }

    #[test]
    fn same_model_request_joins_existing_transition() {
        let mut coord = TransitionCoordinator::new(10_000);
        let started = coord.request_transition(req("inst-a", "qwen9b"), 0);
        let first_token = match started.path {
            TransitionPath::Started { fencing_token } => fencing_token,
            other => panic!("expected Started, got {other:?}"),
        };

        // A second instance asking for the same model joins instead of starting
        // duplicate work, and reuses the same fencing token.
        let joined = coord.request_transition(req("inst-b", "qwen9b"), 100);
        assert_eq!(
            joined.path,
            TransitionPath::Joined {
                fencing_token: first_token
            }
        );
        // Still exactly one active transition, owned by the original instance.
        assert_eq!(
            coord
                .active_transition(&RuntimeId("rt".to_string()))
                .unwrap()
                .owner_instance,
            "inst-a"
        );
    }

    #[test]
    fn conflicting_models_pick_deterministic_winner() {
        // Lower-priority incoming loses to the higher-priority active owner.
        let mut coord = TransitionCoordinator::new(10_000);
        let mut high = req("inst-a", "qwen9b");
        high.priority = 9;
        coord.request_transition(high, 0);

        let mut low = req("inst-b", "gemma-4-e4b-it");
        low.priority = 1;
        let decision = coord.request_transition(low, 100);
        assert_eq!(
            decision.path,
            TransitionPath::Rejected {
                winner_instance: "inst-a".to_string()
            }
        );
        // The active model/profile is unchanged.
        assert_eq!(
            coord
                .active_transition(&RuntimeId("rt".to_string()))
                .unwrap()
                .desired_model,
            ModelId("qwen9b".to_string())
        );

        // The reverse order yields the same winner: arbitration depends on policy
        // inputs, not arrival order.
        let mut coord2 = TransitionCoordinator::new(10_000);
        let mut low2 = req("inst-b", "gemma-4-e4b-it");
        low2.priority = 1;
        coord2.request_transition(low2, 0);
        let mut high2 = req("inst-a", "qwen9b");
        high2.priority = 9;
        let decision2 = coord2.request_transition(high2, 100);
        match &decision2.path {
            TransitionPath::TookOver { previous_owner, .. } => {
                assert_eq!(previous_owner.as_str(), "inst-b");
            }
            other => {
                panic!("higher-priority request must take over regardless of order, got {other:?}")
            }
        }
        assert_eq!(
            coord2
                .active_transition(&RuntimeId("rt".to_string()))
                .unwrap()
                .desired_model,
            ModelId("qwen9b".to_string())
        );
    }

    #[test]
    fn interactive_collision_falls_back_to_hot_worker() {
        let mut coord = TransitionCoordinator::new(10_000);
        // inst-a is cold-loading a large model.
        coord.request_transition(req("inst-a", "qwen3.5-122b-a10b-mtp"), 0);

        // inst-b wants a different model but is interactive with a hot compatible
        // worker available — continuity serves the hot worker now.
        let mut interactive = req("inst-b", "gemma-4-e4b-it");
        interactive.hot_compatible_model = Some(ModelId("qwen9b".to_string()));
        let decision = coord.request_transition(interactive, 50);
        assert_eq!(
            decision.path,
            TransitionPath::ServedHot {
                model: ModelId("qwen9b".to_string())
            }
        );
        // The in-flight cold-load is left undisturbed.
        assert_eq!(
            coord
                .active_transition(&RuntimeId("rt".to_string()))
                .unwrap()
                .owner_instance,
            "inst-a"
        );
        assert!(!decision.rejected.is_empty());
    }

    #[test]
    fn background_stage_allowed_when_no_active_transition() {
        let mut coord = TransitionCoordinator::new(10_000);
        let mut bg = req("inst-a", "qwen3.6-27b-mtp");
        bg.background = true;
        bg.interactive = false;
        let decision = coord.request_transition(bg, 0);
        assert!(
            matches!(decision.path, TransitionPath::Staged { .. }),
            "background stage with no conflict should be scheduled, got {:?}",
            decision.path
        );
    }

    #[test]
    fn background_stage_blocked_by_active_conflicting_transition() {
        let mut coord = TransitionCoordinator::new(10_000);
        coord.request_transition(req("inst-a", "qwen9b"), 0);

        let mut bg = req("inst-b", "gemma-4-e4b-it");
        bg.background = true;
        bg.interactive = false;
        let decision = coord.request_transition(bg, 100);
        assert_eq!(
            decision.path,
            TransitionPath::Rejected {
                winner_instance: "inst-a".to_string()
            }
        );
        // The active owner's transition is preserved (lease not violated).
        assert_eq!(
            coord
                .active_transition(&RuntimeId("rt".to_string()))
                .unwrap()
                .desired_model,
            ModelId("qwen9b".to_string())
        );
    }

    #[test]
    fn expired_owner_lease_is_taken_over_with_new_fencing_token() {
        let mut coord = TransitionCoordinator::new(1_000);
        let started = coord.request_transition(req("inst-a", "qwen9b"), 0);
        let first_token = match started.path {
            TransitionPath::Started { fencing_token } => fencing_token,
            other => panic!("expected Started, got {other:?}"),
        };

        // After the lease TTL, inst-b takes over; the fencing token advances.
        let decision = coord.request_transition(req("inst-b", "qwen9b"), 1_500);
        match decision.path {
            TransitionPath::TookOver {
                fencing_token,
                previous_owner,
            } => {
                assert_eq!(previous_owner, "inst-a");
                assert!(
                    fencing_token > first_token,
                    "take-over must advance the fencing token ({fencing_token} > {first_token})"
                );
            }
            other => panic!("expected TookOver, got {other:?}"),
        }
        assert_eq!(
            coord
                .active_transition(&RuntimeId("rt".to_string()))
                .unwrap()
                .owner_instance,
            "inst-b"
        );
    }

    #[test]
    fn stale_owner_cannot_complete_after_takeover() {
        let mut coord = TransitionCoordinator::new(1_000);
        let started = coord.request_transition(req("inst-a", "qwen9b"), 0);
        let stale_token = match started.path {
            TransitionPath::Started { fencing_token } => fencing_token,
            other => panic!("expected Started, got {other:?}"),
        };

        let taken = coord.request_transition(req("inst-b", "qwen9b"), 2_000);
        let new_token = match taken.path {
            TransitionPath::TookOver { fencing_token, .. } => fencing_token,
            other => panic!("expected TookOver, got {other:?}"),
        };

        // The fenced-off original owner cannot complete with its stale token.
        let rt = RuntimeId("rt".to_string());
        assert!(
            !coord.complete(&rt, stale_token),
            "a stale fencing token must not complete the taken-over transition"
        );
        assert!(
            coord.active_transition(&rt).is_some(),
            "the new owner's transition must survive the stale completion attempt"
        );
        // The current owner can complete with the new token.
        assert!(coord.complete(&rt, new_token));
        assert!(coord.active_transition(&rt).is_none());
    }

    #[test]
    fn serving_lease_protects_active_transition_from_preemption() {
        let mut coord = TransitionCoordinator::new(10_000);
        // inst-a holds a low-priority transition but is actively serving.
        let mut low = req("inst-a", "qwen9b");
        low.priority = 1;
        coord.request_transition(low, 0);
        assert!(coord.acquire_serving_lease(&RuntimeId("rt".to_string())));

        // A higher-priority interactive request without a hot fallback would
        // normally win, but the serving lease protects the active transition.
        let mut high = req("inst-b", "gemma-4-e4b-it");
        high.priority = 9;
        let decision = coord.request_transition(high, 100);
        assert_eq!(
            decision.path,
            TransitionPath::Rejected {
                winner_instance: "inst-a".to_string()
            }
        );
    }

    #[test]
    fn unavailable_runtime_request_is_rejected_deterministically() {
        let mut coord = TransitionCoordinator::new(10_000);
        let mut down = req("inst-a", "qwen9b");
        down.runtime_available = false;
        let decision = coord.request_transition(down, 0);
        assert_eq!(decision.path, TransitionPath::RuntimeUnavailable);
        assert!(
            coord
                .active_transition(&RuntimeId("rt".to_string()))
                .is_none(),
            "an unavailable runtime starts no transition"
        );
        assert!(!decision.rejected.is_empty());
    }

    #[test]
    fn every_transition_decision_carries_explanation() {
        let mut coord = TransitionCoordinator::new(1_000);
        // Drive each path and assert all carry a reason and a recognizable code.
        let started = coord.request_transition(req("inst-a", "qwen9b"), 0);
        let joined = coord.request_transition(req("inst-b", "qwen9b"), 10);

        let mut hot = req("inst-c", "gemma-4-e4b-it");
        hot.hot_compatible_model = Some(ModelId("qwen9b".to_string()));
        let served_hot = coord.request_transition(hot, 20);

        let mut bg = req("inst-d", "gemma-4-e4b-it");
        bg.background = true;
        bg.interactive = false;
        let blocked = coord.request_transition(bg, 30);

        let took_over = coord.request_transition(req("inst-e", "minimax-256k"), 5_000);

        let mut down = req("inst-f", "qwen9b");
        down.runtime_available = false;
        let unavailable = coord.request_transition(down, 5_100);

        for decision in [
            &started,
            &joined,
            &served_hot,
            &blocked,
            &took_over,
            &unavailable,
        ] {
            assert!(
                decision.is_explained(),
                "every decision must carry a reason: {:?}",
                decision.path
            );
            assert!(
                decision
                    .reasons
                    .iter()
                    .all(|r| r.code.starts_with("transition.")),
                "reason codes should be namespaced: {:?}",
                decision.reasons
            );
        }

        // Spot-check the paths actually exercised the distinct branches.
        assert!(matches!(started.path, TransitionPath::Started { .. }));
        assert!(matches!(joined.path, TransitionPath::Joined { .. }));
        assert!(matches!(served_hot.path, TransitionPath::ServedHot { .. }));
        assert!(matches!(blocked.path, TransitionPath::Rejected { .. }));
        assert!(matches!(took_over.path, TransitionPath::TookOver { .. }));
        assert_eq!(unavailable.path, TransitionPath::RuntimeUnavailable);
    }
}
