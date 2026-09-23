use crate::profile_utils::extract_parameter_class;

#[test]
fn extract_parameter_class_parses_common_model_ids() {
    let cases = [
        ("qwen3.5-9b-mtp", "9b"),
        ("qwen3.6-35b-a3b-mtp", "35b"),
        ("qwen3.5-122b-a10b-mtp", "122b"),
        ("qwen3.5-2b-mtp", "2b"),
        ("qwen3.5-4b-mtp", "4b"),
        ("qwen3.6-27b-mtp", "27b"),
        ("gemma-4-26b-a4b-it-mtp", "26b"),
        ("gemma-4-31b-it", "31b"),
        ("gemma-4-e2b-it", "2b"),
        ("gemma-4-e4b-it", "4b"),
        ("granite-4.1-8b-gpu", "8b"),
    ];
    for (id, expected) in cases {
        assert_eq!(extract_parameter_class(id), expected, "failed for {}", id);
    }
}

#[test]
fn extract_parameter_class_returns_unknown_for_non_parametric_ids() {
    let cases = ["minimax-256k", "nemotron-udiq4-256k", "minimax-256k-iq3s"];
    for id in cases {
        assert_eq!(
            extract_parameter_class(id),
            "unknown",
            "expected unknown for {}",
            id
        );
    }
}
