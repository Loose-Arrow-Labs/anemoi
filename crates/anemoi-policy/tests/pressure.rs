//! Active-request pressure behavior (issue #80): the penalty must keep growing
//! with load instead of saturating at a floor.

use anemoi_core::RuntimeMemorySnapshot;
use anemoi_policy::{PressureInputs, PressureModel};

fn active_request_penalty(count: usize) -> i32 {
    // Empty memory snapshot => VRAM/RAM pressure is Unknown (no memory penalty),
    // so the resulting penalty is purely the active-request contribution.
    let memory = RuntimeMemorySnapshot::default();
    PressureModel::default()
        .assess(&PressureInputs {
            memory: &memory,
            vram_required_mb: None,
            ram_required_mb: None,
            is_cold_load: false,
            active_request_count: count,
        })
        .penalty
}

#[test]
fn active_request_penalty_grows_with_count_beyond_floor() {
    // Previously `(each * count).max(floor)` capped the penalty at -20, so 5, 50,
    // and 100 active requests all scored identically. The penalty is now linear
    // in the request count (each = -5) and keeps growing past the old floor.
    let p5 = active_request_penalty(5);
    let p50 = active_request_penalty(50);
    let p100 = active_request_penalty(100);

    assert_eq!(p5, -25);
    assert_eq!(p50, -250);
    assert_eq!(p100, -500);

    assert!(
        p50 < p5,
        "50 requests ({p50}) must be penalized more than 5 ({p5})"
    );
    assert!(
        p100 < p50,
        "100 requests ({p100}) must be penalized more than 50 ({p50})"
    );
}
