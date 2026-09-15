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
/// act on. Likewise, a per-GPU total whose cross-device sum overflows `u64`
/// maps to `None` (unknown) rather than panicking, wrapping, or saturating.
pub(crate) fn parse_llama_swap_memory_metrics(text: &str) -> RuntimeMemorySnapshot {
    let mut ram_total_bytes: Option<u64> = None;
    let mut ram_used_bytes: Option<u64> = None;
    let mut vram_total_bytes: Option<u64> = None;
    let mut vram_used_bytes: Option<u64> = None;
    // Once a per-device sum overflows, the field reports unknown forever:
    // the true total exceeds `u64::MAX`, so no later sample can repair it.
    let mut vram_total_overflowed = false;
    let mut vram_used_overflowed = false;

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
            // A sum that overflows `u64` leaves the field `None` (unknown)
            // instead of panicking in debug or wrapping in release.
            "llamaswap_gpu_memory_total_bytes" => {
                add_gpu_sample(
                    &mut vram_total_bytes,
                    &mut vram_total_overflowed,
                    value_bytes,
                );
            }
            "llamaswap_gpu_memory_used_bytes" => {
                add_gpu_sample(&mut vram_used_bytes, &mut vram_used_overflowed, value_bytes);
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

/// Accumulates one per-device GPU sample into the cross-device running
/// total. The first sample starts the total; later samples add to it with
/// checked arithmetic. If any addition would overflow `u64`, the field
/// becomes `None` (unknown) and stays `None`: the true total exceeds
/// `u64::MAX`, so no later sample can bring it back in range, and a debug
/// panic or a release wrap would be a confidently-wrong number the scheduler
/// would act on.
fn add_gpu_sample(total: &mut Option<u64>, overflowed: &mut bool, value: u64) {
    if *overflowed {
        return;
    }
    match *total {
        None => *total = Some(value),
        Some(acc) => match acc.checked_add(value) {
            Some(sum) => *total = Some(sum),
            None => {
                *total = None;
                *overflowed = true;
            }
        },
    }
}

/// `2^64` — the first `f64` that cannot fit in a `u64`. `f64 as u64`
/// saturates every finite value at or above this to `u64::MAX` rather than
/// failing, so samples at or above it must be rejected before the cast.
const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;

/// Extracts the metric name and byte value from one Prometheus exposition line,
/// returning `None` when the line is not a readable `<name>{labels} value`
/// sample (the caller skips such lines).
///
/// The value is parsed as `f64` so scientific-notation samples
/// (e.g. `2.0699938816e+10`) survive — a `u64`-only parse would silently drop
/// them. It is then converted to whole bytes. Rejected (the line is skipped,
/// the metric stays `None`) are: non-finite values, negative values,
/// fractional values (the cast would truncate rather than represent them
/// exactly), and any value at or above 2^64 (`f64 as u64` saturates those to
/// `u64::MAX` — fabricated capacity is worse than a missing one).
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
    // Reject anything the cast to `u64` would misrepresent: non-finite or
    // fractional values (the cast truncates rather than represents exactly),
    // and values outside [0, 2^64) (at or above 2^64 the cast saturates to
    // `u64::MAX` instead of failing).
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..TWO_POW_64).contains(&value) {
        return None;
    }
    // In range: the cast to `u64` is exact (integer-valued, < 2^64), so no
    // truncation or saturation can occur here.
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

    /// A finite value at or above 2^64 must be rejected, not saturated:
    /// `f64 as u64` turns `1e30` into `u64::MAX` (17592186044415 MB of
    /// fabricated capacity). The line is now skipped, exactly like a
    /// negative or non-finite one, so the metric stays `None` (unknown).
    #[test]
    fn value_at_or_above_2_pow_64_yields_none_not_u64_max() {
        let text = "llamaswap_memory_total_bytes 1e30
llamaswap_memory_used_bytes 18446744073709551616
llamaswap_gpu_memory_total_bytes{id=\"0\"} 1.8446744073709552e+19";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.ram_total_mb, None,
            "1e30 (>= 2^64) is unknown, not saturated"
        );
        assert_eq!(
            snap.ram_used_mb, None,
            "exact 2^64 is unknown, not saturated"
        );
        assert_eq!(
            snap.vram_total_mb, None,
            "scientific-notation 2^64 is unknown, not saturated"
        );

        // Explicitly not the saturated u64::MAX rendered in megabytes.
        let saturated_mb = u64::MAX / 1024 / 1024;
        assert_ne!(snap.ram_total_mb, Some(saturated_mb));
        assert_ne!(snap.ram_used_mb, Some(saturated_mb));
        assert_ne!(snap.vram_total_mb, Some(saturated_mb));
    }

    /// Two GPU samples whose cross-device sum exceeds u64::MAX leave the
    /// field `None` (unknown) rather than panicking (debug) or wrapping
    /// (release). A later sample cannot undo the overflow, and the
    /// independent used-bytes counter still sums normally.
    ///
    /// Values chosen: `18446744073709549568` is the largest `f64` strictly
    /// below 2^64 (f64 spacing there is 2^11), and `2048` (= 2^11) is the
    /// smallest value that pushes their sum past `u64::MAX`. Each sample is
    /// individually in range and exactly representable, so only the *sum*
    /// overflows.
    #[test]
    fn gpu_sum_overflow_yields_none_rather_than_panic_or_wrap() {
        let text = "llamaswap_gpu_memory_total_bytes{id=\"0\"} 18446744073709549568
llamaswap_gpu_memory_total_bytes{id=\"1\"} 2048
llamaswap_gpu_memory_total_bytes{id=\"2\"} 1048576
llamaswap_gpu_memory_used_bytes{id=\"0\"} 1048576
llamaswap_gpu_memory_used_bytes{id=\"1\"} 1048576";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.vram_total_mb, None,
            "sum past u64::MAX is unknown: no panic, no wrap, no sticky repair"
        );
        assert_eq!(
            snap.vram_used_mb,
            Some(2),
            "independent counter still sums across devices"
        );
    }
}
