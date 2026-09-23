use anemoi_core::ModelId;
use anemoi_core::ModelProfile;
use anemoi_core::RuntimeId;

/// Build a synthetic `ModelProfile` from a model ID reported by a live runtime.
///
/// Family is the leading alphabetic prefix of the ID (e.g. `qwen`, `gemma`,
/// `minimax`).  Parameter class is extracted from the first `NNb` token found
/// in the ID (e.g. `9b`, `35b`, `122b`); models whose IDs carry no such token
/// (e.g. `minimax-256k`, `nemotron-udiq4-256k`) get `"unknown"` and will score
/// low on quality but are still selectable when hot.
pub(crate) fn synthesize_profile(model_id: &ModelId, runtime_id: &RuntimeId) -> ModelProfile {
    let family: String = model_id
        .0
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect();

    let parameter_class = extract_parameter_class(&model_id.0);

    ModelProfile {
        id: model_id.clone(),
        family: if family.is_empty() {
            "unknown".to_string()
        } else {
            family
        },
        parameter_class,
        context_window: None,
        vram_required_mb: None,
        ram_required_mb: None,
        cold_load_estimate_ms: None,
        supports_streaming: Some(true),
        supported_runtimes: vec![runtime_id.clone()],
    }
}

/// Scan `id` for the first `NNb` token (one or more ASCII digits immediately
/// followed by the letter `b`) and return it, e.g. `"35b"`.  Returns
/// `"unknown"` when no such token is found.
pub(crate) fn extract_parameter_class(id: &str) -> String {
    let bytes = id.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'b' || bytes[i] == b'B') {
                return format!("{}b", &id[start..i]);
            }
        } else {
            i += 1;
        }
    }
    "unknown".to_string()
}
