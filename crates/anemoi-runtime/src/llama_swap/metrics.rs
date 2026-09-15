//! Parsing of llama-swap's Prometheus `/metrics` endpoint into the memory
//! numbers the scheduling policy reasons about. Kept separate from
//! [`super::adapter::LlamaSwapAdapter`] so the byte-to-megabyte conversion,
//! per-GPU summation, and comment handling are unit-testable without a network.

use std::collections::{hash_map::Entry, BTreeSet, HashMap};

use anemoi_core::RuntimeMemorySnapshot;

use crate::util::bytes_to_mb;

/// The four metric names this snapshot consumes. Each maps to exactly one
/// field of [`RuntimeMemorySnapshot`].
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Metric {
    RamTotal,
    RamUsed,
    VramTotal,
    VramUsed,
}

/// One sample's identity: which metric, and — for the per-device GPU
/// samples — which device. Host-RAM samples are unlabeled and carry no
/// device.
type SampleKey = (Metric, Option<String>);

/// Every sample filed by pass one, keyed by [`SampleKey`]. The value is the
/// sample's byte count, or `None` when the same key was filed more than once
/// (a duplicate): the stored value no longer matters, because pass two
/// blanks the whole pair a duplicated key belongs to.
type SampleMap = HashMap<SampleKey, Option<u64>>;

/// Parses llama-swap's Prometheus `/metrics` text into a
/// [`RuntimeMemorySnapshot`] expressed in megabytes.
///
/// Recognized samples (all byte counts, possibly in scientific notation):
/// - `llamaswap_memory_total_bytes`     -> `ram_total_mb`
/// - `llamaswap_memory_used_bytes`      -> `ram_used_mb`
/// - `llamaswap_gpu_memory_total_bytes` -> `vram_total_mb`
/// - `llamaswap_gpu_memory_used_bytes`  -> `vram_used_mb`
///
/// The parse runs in two passes. **Pass one, collect:** every line is parsed
/// into a sample and filed in a map keyed by `(metric, device id)`. Nothing
/// is accumulated. A line that fails to parse, or a GPU sample whose `id`
/// label is absent or unparseable, never enters the map. If the same key is
/// filed twice, the key is recorded as duplicated. **Pass two, decide:** with
/// the whole scrape in hand, each pair is validated and, only if valid,
/// aggregated:
///
/// - The host-RAM samples are unlabeled, so there is a single total and a
///   single used. The RAM pair is known only when both are present and
///   neither was duplicated.
/// - The GPU samples are per-device. The VRAM pair is known only when the set
///   of device ids with a valid total **exactly equals** the set with a valid
///   used, and no key on either side was duplicated. A device that reported
///   one half but not the other would otherwise let the aggregate check pass
///   with the full capacity of every card against the usage of a subset — a
///   VRAM-pressure reading far lower than reality. A duplicate sample for a
///   metric and device id is the same trap in disguise: sets deduplicate
///   while sums do not, so two totals for `id="0"` plus one used sample
///   matches device sets yet doubles the total.
///
/// Per-device values are summed with checked arithmetic; an overflow of
/// `u64` leaves that field `None` (unknown) rather than panicking, wrapping,
/// or saturating. A total/used pair is **all-or-nothing**: if validation
/// fails, *both* fields of that pair read `None` (unknown) — never a total
/// with no used, which would be mistaken for "0 MB used", a lie the
/// scheduler would act on. The two pairs (VRAM, host RAM) are independent: a
/// broken VRAM pair never discards a valid RAM pair, and vice versa. Lines
/// beginning with `#` (HELP/TYPE) and any unrelated metric are ignored.
pub(crate) fn parse_llama_swap_memory_metrics(text: &str) -> RuntimeMemorySnapshot {
    // Pass one: file every parseable sample in a map keyed by
    // (metric, device id), marking any key that appears more than once.
    let samples = collect_samples(text);
    // Pass two: validate each pair against the complete scrape, then
    // aggregate. The pairs are decided independently: a broken VRAM pair
    // never discards a valid RAM pair, and vice versa.
    let (vram_total_mb, vram_used_mb) = vram_pair(&samples);
    let (ram_total_mb, ram_used_mb) = ram_pair(&samples);

    RuntimeMemorySnapshot {
        vram_total_mb,
        vram_used_mb,
        ram_total_mb,
        ram_used_mb,
    }
}

/// Pass one: walks the exposition text and files each recognized sample in a
/// map keyed by `(metric, device id)`. Host-RAM samples are unlabeled and key
/// on `device id = None`; GPU samples key on their `id` label. A sample that
/// fails to parse, or a GPU sample with no usable `id` label, is skipped
/// rather than filed — the sample joins no device bucket and cannot corrupt
/// another device's figures. A key filed twice is marked as duplicated by
/// replacing its value with `None`: pass two treats a marked key as "this
/// pair is unknown", so the value never reaches a sum or a reported field.
fn collect_samples(text: &str) -> SampleMap {
    let mut samples: SampleMap = HashMap::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value_bytes, gpu_device_id)) = parse_sample_line(line) else {
            continue;
        };
        let key = match name {
            "llamaswap_memory_total_bytes" => (Metric::RamTotal, None),
            "llamaswap_memory_used_bytes" => (Metric::RamUsed, None),
            // GPU samples are per-device. A sample whose `id` label is absent
            // or unparseable joins no device: it is skipped entirely rather
            // than being folded into another device's bucket.
            "llamaswap_gpu_memory_total_bytes" => match gpu_device_id {
                Some(device_id) => (Metric::VramTotal, Some(device_id.to_owned())),
                None => continue,
            },
            "llamaswap_gpu_memory_used_bytes" => match gpu_device_id {
                Some(device_id) => (Metric::VramUsed, Some(device_id.to_owned())),
                None => continue,
            },
            _ => continue,
        };
        // Filing the same key twice is a duplicate: mark it. Pass two
        // treats a marked key as "this pair is unknown"; the `None` value
        // never reaches a sum or a reported field.
        match samples.entry(key) {
            Entry::Vacant(vacant) => {
                vacant.insert(Some(value_bytes));
            }
            Entry::Occupied(mut occupied) => {
                occupied.insert(None);
            }
        }
    }

    samples
}

/// Pass two, VRAM: validate and aggregate the per-device GPU samples.
///
/// Returns `(None, None)` unless both halves are sound: the set of device
/// ids with a valid total exactly equals the set with a valid used, and no
/// key on either side was duplicated. Only then are the per-device values
/// summed with checked arithmetic — an overflow of `u64` again yields
/// `None` (unknown).
fn vram_pair(samples: &SampleMap) -> (Option<u64>, Option<u64>) {
    // The device ids that reported a sample of the named half, derived from
    // the collected map now that the whole scrape is in hand.
    let devices_reporting = |metric: Metric| -> BTreeSet<String> {
        samples
            .iter()
            .filter_map(|((sample_metric, device), _)| {
                if *sample_metric == metric {
                    device.clone()
                } else {
                    None
                }
            })
            .collect()
    };
    let total_devices = devices_reporting(Metric::VramTotal);
    let used_devices = devices_reporting(Metric::VramUsed);

    // Differing device sets mean some device reported one half but not the
    // other: a full total against a partial used reads far lower pressure
    // than reality, so the whole pair is unknown.
    if total_devices != used_devices {
        return (None, None);
    }
    // A duplicated key (stored as `None`) means a device reported the same
    // half twice: sets deduplicate but sums do not, so the second sample
    // would inflate the total (or used) against the other half. Either half
    // duplicated blanks the pair.
    for ((sample_metric, device), value) in samples.iter() {
        if value.is_none()
            && device.is_some()
            && (*sample_metric == Metric::VramTotal || *sample_metric == Metric::VramUsed)
        {
            return (None, None);
        }
    }

    // Both halves are sound: sum each across devices.
    match (
        sum_metric_values(samples, Metric::VramTotal),
        sum_metric_values(samples, Metric::VramUsed),
    ) {
        (Some(total), Some(used)) => (Some(bytes_to_mb(total)), Some(bytes_to_mb(used))),
        _ => (None, None),
    }
}

/// Sums every sample filed under `metric` with checked arithmetic. The first
/// sample starts the sum; any addition that would overflow `u64` leaves the
/// result `None` (unknown) instead of panicking in debug or wrapping in
/// release — a wrap would be a confidently-wrong number the scheduler would
/// act on. With no samples the sum is `None`, so an empty scrape still reads
/// unknown, never a fabricated zero.
fn sum_metric_values(samples: &SampleMap, metric: Metric) -> Option<u64> {
    let mut total: Option<u64> = None;
    for ((sample_metric, _), value) in samples.iter() {
        if *sample_metric != metric {
            continue;
        }
        // A duplicated key stores `None` rather than a byte count: the sum
        // cannot be computed, so report unknown.
        let bytes = (*value)?;
        total = match total {
            None => Some(bytes),
            Some(acc) => acc.checked_add(bytes),
        };
        total?;
    }
    total
}

/// Pass two, host RAM: unlabeled, so there is exactly one total sample and
/// one used sample. The pair is known only when both are present and neither
/// key was duplicated; otherwise both fields read `None` (unknown). No
/// summation is involved — a duplicate here is a second, contradictory
/// reading of the same gauge, and keeping either half of a contradicted pair
/// is guessing.
fn ram_pair(samples: &SampleMap) -> (Option<u64>, Option<u64>) {
    // A `None` value marks a duplicated key: a second, contradictory reading
    // of the same unlabeled gauge. Neither half of a contradicted pair is
    // trusted, so both fields read `None` (unknown).
    let total = samples.get(&(Metric::RamTotal, None));
    let used = samples.get(&(Metric::RamUsed, None));
    if total.is_some_and(|t| t.is_none()) || used.is_some_and(|u| u.is_none()) {
        return (None, None);
    }
    let total = total.copied().flatten();
    let used = used.copied().flatten();
    match (total, used) {
        (Some(total), Some(used)) => (Some(bytes_to_mb(total)), Some(bytes_to_mb(used))),
        _ => (None, None),
    }
}

/// `2^64` — the first `f64` that cannot fit in a `u64`. `f64 as u64`
/// saturates every finite value at or above this to `u64::MAX` rather than
/// failing, so samples at or above it must be rejected before the cast.
const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;

/// Extracts the metric name, byte value, and — for labeled samples — the
/// `id` label, from one Prometheus exposition line, returning `None` when the
/// line is not a readable `<name>{labels} value` sample (the caller skips
/// such lines). The third element is `Some` only when the label block
/// contains a parseable, non-empty `id` label; samples without one carry
/// `None`, and it is the caller's responsibility to keep unlabeled host-RAM
/// samples while rejecting GPU samples.
///
/// The value is parsed as `f64` so scientific-notation samples
/// (e.g. `2.0699938816e+10`) survive — a `u64`-only parse would silently drop
/// them. It is then converted to whole bytes. Rejected (the line is skipped,
/// the metric stays `None`) are: non-finite values, negative values,
/// fractional values (the cast would truncate rather than represent them
/// exactly), and any value at or above 2^64 (`f64 as u64` saturates those to
/// `u64::MAX` — fabricated capacity is worse than a missing one).
fn parse_sample_line(line: &str) -> Option<(&str, u64, Option<&str>)> {
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
    // Locate the label block, if present. Label values may contain spaces and
    // braces, so the closing `}` is found while respecting double quotes; an
    // unterminated block rejects the line.
    let (after_labels, label_block) = match rest.strip_prefix('{') {
        Some(tail) => {
            let end = find_label_block_end(tail)?;
            (tail.get(end + 1..)?.trim_start(), Some(&tail[..end]))
        }
        None => (rest, None),
    };
    // The device identity: the value of the `id` label, when present and
    // non-empty. No parseable id means the sample cannot be attributed to a
    // device.
    let device_id = label_block.and_then(|block| extract_label_value(block, "id"));
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
    Some((name, value as u64, device_id))
}

/// Returns the text of the double-quoted value of the named label within a
/// Prometheus label block (the text between the outer `{` and `}`), or `None`
/// when the block has no such label or its value is empty. Labels are
/// `key="value"` pairs separated by `,`; a value may itself contain spaces
/// and `}`. No unescaping is attempted: a literal quote inside a value would
/// have to appear as `\"` in exposition form, and treating that backslash-
/// quoted quote as a terminator leaves the label unparseable — the
/// "reject, do not guess" outcome, never a misattribution to another device.
fn extract_label_value<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    for part in block.split(',') {
        let part = part.trim();
        let Some((label_key, label_value)) = part.split_once('=') else {
            continue;
        };
        if label_key.trim() != key {
            continue;
        }
        let quoted = label_value.trim();
        let inner = quoted.strip_prefix('"')?.strip_suffix('"')?;
        if inner.is_empty() {
            return None;
        }
        return Some(inner);
    }
    None
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

        // A lone total (no matching used) is an incomplete pair: the whole
        // pair reads `None` — never a bare `Some(total)` with no used, which
        // pressure.rs would read as 0% used, and never a leaked `Some(0)`.
        let snap = parse_llama_swap_memory_metrics("llamaswap_memory_total_bytes 104857600\n");
        assert_eq!(
            snap.ram_total_mb, None,
            "lone total: incomplete pair is unknown"
        );
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
llamaswap_memory_total_bytes 209715200
llamaswap_memory_used_bytes 104857600";

        let snap = parse_llama_swap_memory_metrics(text);

        // The complete RAM pair parses through the comments and unrelated
        // lines; nothing spurious is invented.
        assert_eq!(
            snap.ram_used_mb,
            Some(100),
            "unrelated lines do not disturb a complete pair"
        );
        assert_eq!(
            snap.ram_total_mb,
            Some(200),
            "unrelated lines do not disturb a complete pair"
        );
        assert_eq!(snap.vram_total_mb, None);
        assert_eq!(snap.vram_used_mb, None);
    }

    /// A malformed value is skipped rather than panicking; because the
    /// malformed used leaves its pair incomplete, the whole RAM pair reads
    /// `None`. A well-formed complete pair on other lines is still parsed.
    #[test]
    fn malformed_value_is_skipped_not_panicked() {
        let text = "llamaswap_memory_used_bytes not_a_number
llamaswap_memory_total_bytes 104857600
llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_used_bytes{id=\"0\"} 1073741824";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.ram_used_mb, None,
            "malformed sample is skipped, leaving the pair incomplete"
        );
        assert_eq!(
            snap.ram_total_mb, None,
            "incomplete RAM pair is unknown, not a bare total"
        );
        assert_eq!(
            snap.vram_total_mb,
            Some(2048),
            "a well-formed complete pair on other lines still parses"
        );
        assert_eq!(
            snap.vram_used_mb,
            Some(1024),
            "a well-formed complete pair on other lines still parses"
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
    /// total field `None` (unknown) rather than panicking (debug) or
    /// wrapping (release). A later sample cannot undo the overflow. Because
    /// a total/used pair is all-or-nothing, the overflowed total also blanks
    /// the independently-summed used counter for that same pair.
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
            snap.vram_used_mb, None,
            "all-or-nothing pair: the overflowed total also blanks the used counter"
        );
    }

    /// Defect 1: a present total with an absent used must blank the whole
    /// pair, not leave a bare total that pressure.rs would read as 0% used.
    #[test]
    fn total_present_used_missing_blanks_whole_pair() {
        let snap = parse_llama_swap_memory_metrics(
            "llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648",
        );

        assert_eq!(
            snap.vram_total_mb, None,
            "a total with no used must not stand alone"
        );
        assert_eq!(
            snap.vram_used_mb, None,
            "the pair is unknown, not a fabricated 0% used"
        );
    }

    /// Defect 1: a malformed used is rejected; the pair is then incomplete,
    /// so both VRAM fields read `None`.
    #[test]
    fn total_present_used_malformed_blanks_whole_pair() {
        let snap = parse_llama_swap_memory_metrics(
            "llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_used_bytes{id=\"0\"} not_a_number",
        );

        assert_eq!(
            snap.vram_total_mb, None,
            "malformed used leaves the pair incomplete"
        );
        assert_eq!(snap.vram_used_mb, None, "malformed used is rejected");
    }

    /// Defect 1: the two pairs are independent. A broken VRAM pair must not
    /// discard a valid RAM pair, and vice versa.
    #[test]
    fn broken_vram_pair_leaves_valid_ram_pair_intact() {
        let text = "llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_memory_total_bytes 1073741824
llamaswap_memory_used_bytes 536870912";

        let snap = parse_llama_swap_memory_metrics(text);

        // VRAM pair: total present, used absent -> both None.
        assert_eq!(snap.vram_total_mb, None, "broken VRAM pair: total");
        assert_eq!(snap.vram_used_mb, None, "broken VRAM pair: used");
        // RAM pair: complete -> both parse, independent of the broken VRAM pair.
        assert_eq!(
            snap.ram_total_mb,
            Some(1024),
            "valid RAM pair survives a broken VRAM pair"
        );
        assert_eq!(
            snap.ram_used_mb,
            Some(512),
            "valid RAM pair survives a broken VRAM pair"
        );
    }

    /// Defect 1: fully valid input still parses to the same values as before.
    #[test]
    fn fully_valid_input_parses_to_same_values() {
        let text = "llamaswap_memory_total_bytes 101195972608
llamaswap_memory_used_bytes 59613642752
llamaswap_gpu_memory_used_bytes{id=\"0\"} 2.0699938816e+10
llamaswap_gpu_memory_total_bytes{id=\"0\"} 2.14695936e+10";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(snap.ram_total_mb, Some(96508), "ram_total_mb");
        assert_eq!(snap.ram_used_mb, Some(56852), "ram_used_mb");
        assert_eq!(snap.vram_total_mb, Some(20475), "vram_total_mb");
        assert_eq!(snap.vram_used_mb, Some(19741), "vram_used_mb");
    }

    /// Per-device pairing: two devices, both halves present for each. The
    /// total and used sets are identical ({"0","1"}), so both sums pass and
    /// aggregate across devices exactly as before this fix.
    #[test]
    fn two_devices_both_halves_present_sum_across_devices() {
        let text = "llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_total_bytes{id=\"1\"} 2147483648
llamaswap_gpu_memory_used_bytes{id=\"0\"} 1073741824
llamaswap_gpu_memory_used_bytes{id=\"1\"} 1073741824";

        let snap = parse_llama_swap_memory_metrics(text);

        // 2 GiB + 2 GiB = 4096 MB total; 1 GiB + 1 GiB = 2048 MB used.
        assert_eq!(
            snap.vram_total_mb,
            Some(4096),
            "both devices complete: totals sum across devices"
        );
        assert_eq!(
            snap.vram_used_mb,
            Some(2048),
            "both devices complete: used sums across devices"
        );
    }

    /// The defect: two devices, device 1's used sample missing. Previously the
    /// aggregate check saw total = 4 GiB (both devices) and used = 1 GiB
    /// (device 0 only), both `Some`, and reported the full capacity of both
    /// cards against the usage of one — 25% pressure where reality is 50% on
    /// the only card whose usage is even known. Now the total set {"0","1"}
    /// differs from the used set {"0"}, so the whole VRAM pair reads `None`:
    /// unknown, never a full total against a partial used.
    #[test]
    fn device_missing_used_blanks_the_pair_not_partial_used() {
        let text = "llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_total_bytes{id=\"1\"} 2147483648
llamaswap_gpu_memory_used_bytes{id=\"0\"} 1073741824";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.vram_total_mb, None,
            "device 1 reported a total but no used: the total and used device sets \
             differ, so the whole pair is unknown — never 4096 MB total against \
             1024 MB used, which reads 25% pressure instead of 50%"
        );
        assert_eq!(
            snap.vram_used_mb, None,
            "the partial used (device 0 only) must not stand against the full total"
        );
    }

    /// Same as the missing case, but device 1's used sample is malformed
    /// (`not_a_number`): the line is skipped, so again only device 0 is in the
    /// used set while both devices are in the total set, and the pair blanks.
    #[test]
    fn device_malformed_used_blanks_the_pair() {
        let text = "llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_total_bytes{id=\"1\"} 2147483648
llamaswap_gpu_memory_used_bytes{id=\"0\"} 1073741824
llamaswap_gpu_memory_used_bytes{id=\"1\"} not_a_number";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.vram_total_mb, None,
            "malformed device-1 used is rejected, so the total and used device sets \
             differ and the pair is unknown — never the full total against one \
             device's used"
        );
        assert_eq!(
            snap.vram_used_mb, None,
            "a partial used set must not stand against the full total set"
        );
    }

    /// A GPU sample whose `id` label is absent joins no device bucket: it is
    /// skipped entirely, so it cannot corrupt the other device's figures.
    /// Here device 0 reports both halves and the unlabeled total is discarded
    /// — device 0's numbers come through unchanged.
    #[test]
    fn gpu_sample_without_id_label_joins_no_device() {
        let text = "llamaswap_gpu_memory_total_bytes 999999999
llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_used_bytes{id=\"0\"} 1073741824";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.vram_total_mb,
            Some(2048),
            "unlabeled GPU total joins no device bucket; device 0's total stands alone"
        );
        assert_eq!(
            snap.vram_used_mb,
            Some(1024),
            "device 0's used is untouched by the unlabeled sample"
        );

        // Same, with the unlabeled sample on the used side.
        let text = "llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_used_bytes 999999999
llamaswap_gpu_memory_used_bytes{id=\"0\"} 1073741824";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.vram_total_mb,
            Some(2048),
            "unlabeled GPU used joins no device bucket; device 0's total stands alone"
        );
        assert_eq!(
            snap.vram_used_mb,
            Some(1024),
            "device 0's used is untouched by the unlabeled sample"
        );
    }

    /// Single device with both halves present: unchanged from today's
    /// behavior — the pair parses to exactly the reported values.
    #[test]
    fn single_device_both_halves_unchanged() {
        let text = "llamaswap_gpu_memory_used_bytes{id=\"0\"} 2.0699938816e+10
llamaswap_gpu_memory_total_bytes{id=\"0\"} 2.14695936e+10";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.vram_used_mb,
            Some(19741),
            "single complete device: used unchanged from pre-fix behavior"
        );
        assert_eq!(
            snap.vram_total_mb,
            Some(20475),
            "single complete device: total unchanged from pre-fix behavior"
        );
    }

    /// The pairs are independent at every level: a VRAM pair broken by a
    /// missing device half must not discard a complete host-RAM pair.
    #[test]
    fn incomplete_gpu_pair_leaves_host_ram_pair_intact() {
        let text = "llamaswap_memory_total_bytes 1073741824
llamaswap_memory_used_bytes 536870912
llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_total_bytes{id=\"1\"} 2147483648
llamaswap_gpu_memory_used_bytes{id=\"0\"} 1073741824";

        let snap = parse_llama_swap_memory_metrics(text);

        // VRAM: total set {0,1} vs used set {0} -> pair blanked.
        assert_eq!(
            snap.vram_total_mb, None,
            "device-mismatched VRAM pair: total"
        );
        assert_eq!(snap.vram_used_mb, None, "device-mismatched VRAM pair: used");
        // RAM: unlabeled, complete -> parses independently of the broken VRAM.
        assert_eq!(
            snap.ram_total_mb,
            Some(1024),
            "complete host-RAM pair survives an incomplete GPU pair"
        );
        assert_eq!(
            snap.ram_used_mb,
            Some(512),
            "complete host-RAM pair survives an incomplete GPU pair"
        );
    }
    /// The defect this rewrite fixes: two totals for `id="0"` and one used
    /// for `id="0"`. Sets deduplicate while sums do not, so a set-equality
    /// check alone sees {0} == {0} and a running sum sees 4 GiB against 1
    /// GiB — a doubled total that reads 25% pressure instead of 50%. The
    /// duplicated key blanks the whole pair: unknown, never the doubled
    /// total against a single used.
    #[test]
    fn duplicate_gpu_total_blanks_the_pair_not_a_doubled_total() {
        let text = "llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_used_bytes{id=\"0\"} 1073741824";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.vram_total_mb, None,
            "two totals for id=0: the duplicated key blanks the pair"
        );
        assert_eq!(
            snap.vram_used_mb, None,
            "the single used must not stand against a doubled total"
        );
        // Specifically not the doubled total (4096 MB) against the single
        // used (1024 MB), which would read 25% pressure instead of 50%.
        assert_ne!(snap.vram_total_mb, Some(4096));
        assert_ne!(snap.vram_used_mb, Some(1024));
    }

    /// A duplicated host-RAM total is a second, contradictory reading of the
    /// same unlabeled gauge: the pair is unknown, never the last-written
    /// value standing against a used that was never contradicted.
    #[test]
    fn duplicate_host_ram_total_blanks_the_pair() {
        let text = "llamaswap_memory_total_bytes 1073741824
llamaswap_memory_total_bytes 2147483648
llamaswap_memory_used_bytes 536870912";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.ram_total_mb, None,
            "duplicated total: neither reading is trusted"
        );
        assert_eq!(
            snap.ram_used_mb, None,
            "all-or-nothing: a contradicted pair blanks both fields"
        );
    }

    /// The pairs stay independent even under duplication: a VRAM pair blanked
    /// by a duplicated total must not discard a complete host-RAM pair.
    #[test]
    fn duplicate_vram_sample_leaves_valid_ram_pair_intact() {
        let text = "llamaswap_memory_total_bytes 1073741824
llamaswap_memory_used_bytes 536870912
llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_total_bytes{id=\"0\"} 2147483648
llamaswap_gpu_memory_used_bytes{id=\"0\"} 1073741824";

        let snap = parse_llama_swap_memory_metrics(text);

        assert_eq!(
            snap.vram_total_mb, None,
            "duplicated VRAM total: pair blanked"
        );
        assert_eq!(
            snap.vram_used_mb, None,
            "duplicated VRAM total: pair blanked"
        );
        assert_eq!(
            snap.ram_total_mb,
            Some(1024),
            "complete host-RAM pair survives a duplicated VRAM sample"
        );
        assert_eq!(
            snap.ram_used_mb,
            Some(512),
            "complete host-RAM pair survives a duplicated VRAM sample"
        );
    }
}
