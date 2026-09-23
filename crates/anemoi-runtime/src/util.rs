/// Converts bytes to megabytes (integer division).
pub(crate) fn bytes_to_mb(bytes: u64) -> u64 {
    bytes / 1024 / 1024
}

/// Normalizes a runtime-reported model identifier to a bare model id: strips any
/// directory prefix (handling both `/` and `\` separators) and a trailing
/// `.gguf`/`.bin` extension.
///
/// Returns [`RuntimeError::Url`] for an id that normalizes to empty (`""`,
/// `"models/"`, `"weights\\"`, …). A runtime that reports a model with no usable
/// id is malformed; callers skip such entries so an empty `ModelId` never
/// propagates into scheduling, logging, and telemetry.
pub(crate) fn normalize_model_id(raw: &str) -> Result<anemoi_core::ModelId, crate::RuntimeError> {
    let normalized = raw.replace('\\', "/");
    let leaf = normalized.rsplit('/').next().unwrap_or(&normalized);
    let leaf = leaf
        .strip_suffix(".gguf")
        .or_else(|| leaf.strip_suffix(".bin"))
        .unwrap_or(leaf);
    if leaf.is_empty() {
        return Err(crate::RuntimeError::Url(format!(
            "empty model id (raw: {raw:?})"
        )));
    }
    Ok(anemoi_core::ModelId(leaf.to_string()))
}
