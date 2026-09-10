//! Parsing of llama-swap's Prometheus `/metrics` endpoint into the memory
//! numbers the scheduling policy reasons about. Kept separate from
//! [`super::adapter::LlamaSwapAdapter`] so the byte-to-megabyte conversion,
//! per-GPU summation, and comment handling are unit-testable without a network.

use anemoi_core::RuntimeMemorySnapshot;

use crate::util::bytes_to_mb;

/// Parses llama-swap's Prometheus `/metrics` text into a
/// [`RuntimeMemorySnapshot`] expressed in megabytes.
///
/// Recognized samples (all byte counts, possibly in scientific notation):
/// - `llamaswap_memory_total_bytes`     -> `ram_total_mb`
/// - `llamaswap_memory_used_bytes`      -> `ram_used_mb`
/// - `llamaswap_gpu_memory_total_bytes` -> `vram_total_mb`
/// - `llamaswap_gpu_memory_used_bytes`  -> `vram_used_mb`
///
/// The host-RAM samples are unlabeled single values. The GPU samples carry a
/// per-device label block (`{id="0",...}`); when more than one device reports,
/// their values are **summed across every `id=`** rather than assuming
/// `id="0"`. Lines beginning with `#` (HELP/TYPE) and any unrelated metric are
/// ignored. A metric that is absent maps to `None` — never `Some(0)` — so a
/// missing sample can't be mistaken for "0 MB used", a lie the scheduler would
/// act on.
pub(crate) fn parse_llama_swap_memory_metrics(text: &str) -> RuntimeMemorySnapshot {
    let mut ram_total_bytes: Option<u64> = None;
    let mut ram_used_bytes: Option<u64> = None;
    let mut vram_total_bytes: Option<u64> = None;
    let mut vram_used_bytes: Option<u64> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value_bytes)) = parse_sample_line(line) else {
            continue;
        };
        match name {
            "llamaswap_memory_total_bytes" => ram_total_bytes = Some(value_bytes),
            "llamaswap_memory_used_bytes" => ram_used_bytes = Some(value_bytes),
            // GPU samples are per-device; sum across every reported device.
            "llamaswap_gpu_memory_total_bytes" => {
                vram_total_bytes = Some(vram_total_bytes.unwrap_or(0) + value_bytes);
            }
            "llamaswap_gpu_memory_used_bytes" => {
                vram_used_bytes = Some(vram_used_bytes.unwrap_or(0) + value_bytes);
            }
            _ => {}
        }
    }

    RuntimeMemorySnapshot {
        vram_total_mb: vram_total_bytes.map(bytes_to_mb),
        vram_used_mb: vram_used_bytes.map(bytes_to_mb),
        ram_total_mb: ram_total_bytes.map(bytes_to_mb),
        ram_used_mb: ram_used_bytes.map(bytes_to_mb),
    }
}

/// Extracts the metric name and byte value from one Prometheus exposition line,
/// returning `None` when the line is not a readable `<name>{labels} value`
/// sample (the caller skips such lines).
///
/// The value is parsed as `f64` so scientific-notation samples
/// (e.g. `2.0699938816e+10`) survive — a `u64`-only parse would silently drop
/// them. It is then converted to whole bytes; non-finite and negative values
/// are rejected so a malformed sample can't become a bogus `Some(0)` or a
/// saturated `u64::MAX`.
fn parse_sample_line(line: &str) -> Option<(&str, u64)> {
    // A Prometheus metric name runs from the start of the line to the first `{`
    // or whitespace and contains neither.
    let name_end = line
        .find(|c: char| c == '{' || c.is_whitespace())
        .unwrap_or(line.len());
    let name = &line[..name_end];
    if name.is_empty() {
        return None;
    }
    let rest = line.get(name_end..)?.trim_start();
    // Drop the label block, if present. Label values may contain spaces and
    // braces, so the closing `}` is located while respecting double quotes.
    let after_labels = match rest.strip_prefix('{') {
        Some(tail) => tail.get(find_label_block_end(tail)? + 1..)?.trim_start(),
        None => rest,
    };
    // The value is the first token after the label block; a trailing timestamp
    // (when present) is the next token and is ignored.
    let value = after_labels
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some((name, value as u64))
}

/// Byte index of the closing `}` of a Prometheus label block, respecting
/// double-quoted label values (which may themselves contain spaces or `}`).
/// Returns `None` when the block is unterminated.
fn find_label_block_end(tail: &str) -> Option<usize> {
    let mut in_quotes = false;
    for (index, byte) in tail.bytes().enumerate() {
        match byte {
            b'"' => in_quotes = !in_quotes,
            b'}' if !in_quotes => return Some(index),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real four-line sample as captured from a llama-swap instance: the
    /// two host-RAM samples plus one per-GPU used/total pair in scientific
    /// notation. Expected MB values are `bytes / 1024 / 1024` (integer
    /// division, as in `bytes_to_mb`):
    /// - 101195972608 -> 96508
    /// - 59613642752  -> 56852
    /// - 20699938816  -> 19741
    /// - 21469593600  -> 20475
    #[test]
    fn real_four_line_sample_parses_to_expected_mb() {
        let text = "llamaswap_memory_total_bytes 101195972608
llamaswap_memory_used_bytes 59613642752
llamaswap_gpu_memory_used_bytes{id=\"0\",name=\"NVIDIA RTX 4000 Ada Generation\",uuid=\"GPU-bae34a68\"} 2.0699938816e+10
llamaswap_gpu_memory_total_bytes{id=\"0\",name=\"NVIDIA RTX 4000 Ada Generation\",uuid=\"GPU-bae34a68\"} 2.14695936e+10";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(snap.ram_total_mb, Some(96508), "ram_total_mb");
        assert_eq!(snap.ram_used_mb, Some(56852), "ram_used_mb");
        assert_eq!(snap.vram_total_mb, Some(20475), "vram_total_mb");
        assert_eq!(snap.vram_used_mb, Some(19741), "vram_used_mb");
    }

    /// The GPU samples in the real sample use scientific notation. If the
    /// parser dropped non-integer values, these would be `None` and the
    /// scheduler would see "no VRAM data" instead of ~20 GB on the card.
    #[test]
    fn scientific_notation_gpu_samples_are_not_dropped() {
        let text = "llamaswap_gpu_memory_used_bytes{id=\"0\"} 2.0699938816e+10
llamaswap_gpu_memory_total_bytes{id=\"0\"} 2.14695936e+10";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(snap.vram_used_mb, Some(19741), "vram_used_mb");
        assert_eq!(snap.vram_total_mb, Some(20475), "vram_total_mb");
    }

    /// Two GPUs (`id=\"0\"` and `id=\"1\"`) must sum across devices, not have
    /// the later device overwrite the earlier one.
    #[test]
    fn two_gpu_devices_sum_rather_than_overwrite() {
        let text = "llamaswap_gpu_memory_used_bytes{id=\"0\"} 1073741824
llamaswap_gpu_memory_used_bytes{id=\"1\"} 1073741824
llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_total_bytes{id=\"1\"} 2147483648";

        let snap = parse_llama_swap_memory_metrics(text);

        // 1 GiB + 1 GiB = 2048 MB used; 2 GiB + 2 GiB = 4096 MB total.
        assert_eq!(snap.vram_used_mb, Some(2048), "vram_used_mb");
        assert_eq!(snap.vram_total_mb, Some(4096), "vram_total_mb");
    }

    /// A metric that is absent maps to `None`, never `Some(0)`. A zero would
    /// read to the scheduler as "0 MB used", which is a lie it would act on.
    #[test]
    fn absent_metrics_yield_none_never_some_zero() {
        let snap = parse_llama_swap_memory_metrics("");

        assert_eq!(snap.vram_total_mb, None, "empty input: vram_total_mb");
        assert_eq!(snap.vram_used_mb, None, "empty input: vram_used_mb");
        assert_eq!(snap.ram_total_mb, None, "empty input: ram_total_mb");
        assert_eq!(snap.ram_used_mb, None, "empty input: ram_used_mb");

        // Present metric must not leak `Some(0)` into absent fields.
        let snap = parse_llama_swap_memory_metrics("llamaswap_memory_total_bytes 104857600\n");
        assert_eq!(snap.ram_total_mb, Some(100), "only ram_total present");
        assert_eq!(snap.ram_used_mb, None, "ram_used absent");
        assert_eq!(snap.vram_total_mb, None, "vram_total absent");
        assert_eq!(snap.vram_used_mb, None, "vram_used absent");
    }

    /// `#` HELP/TYPE comment lines and unrelated metric lines are ignored.
    #[test]
    fn comments_and_unrelated_metrics_are_ignored() {
        let text = "# HELP llamaswap_memory_total_bytes Total system memory in bytes.
# TYPE llamaswap_memory_total_bytes gauge
some_other_metric 42
llamaswap_unrelated_metric 999
llamaswap_memory_used_bytes 104857600";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.ram_used_mb,
            Some(100),
            "unrelated lines do not disturb parsing"
        );
        assert_eq!(snap.ram_total_mb, None);
        assert_eq!(snap.vram_total_mb, None);
        assert_eq!(snap.vram_used_mb, None);
    }

    /// A malformed value is skipped rather than panicking; a well-formed
    /// sample on another line is still parsed.
    #[test]
    fn malformed_value_is_skipped_not_panicked() {
        let text = "llamaswap_memory_used_bytes not_a_number
llamaswap_memory_total_bytes 104857600";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(snap.ram_used_mb, None, "malformed sample is skipped");
        assert_eq!(
            snap.ram_total_mb,
            Some(100),
            "well-formed sample still parsed"
        );
    }
}
