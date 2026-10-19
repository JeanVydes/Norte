//! Number formatting shared by the reports.

use std::time::Duration;

/// Formats a duration: milliseconds under a second, seconds above it.
#[must_use]
pub fn duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    if seconds < 1.0 {
        format!("{:.0} ms", seconds * 1000.0)
    } else {
        format!("{seconds:.2} s")
    }
}

/// Formats money with six decimals, the way a provider reports it.
#[must_use]
pub fn money(value: f64) -> String {
    format!("${value:.6}")
}

/// Formats an optional money value, or an em dash when it was not reported.
#[must_use]
pub fn money_opt(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), money)
}

/// Formats an optional token count, or an em dash when it was not reported.
#[must_use]
pub fn tokens(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| value.to_string())
}

/// Formats `part / whole` as a percentage, or an em dash when `whole` is zero.
#[must_use]
pub fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        "—".to_owned()
    } else {
        format!("{:.1}%", 100.0 * as_f64(part) / as_f64(whole))
    }
}

/// `u64 as f64` loses precision as far as the lint policy is concerned, so the
/// conversion is explicit. A token count never nears the ceiling.
#[must_use]
pub fn as_f64(value: u64) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

/// A one-line sparkline of `values`, scaled between their minimum and maximum.
#[must_use]
pub fn sparkline(values: &[u64]) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let Some(min) = values.iter().min() else {
        return String::new();
    };
    let max = values.iter().max().copied().unwrap_or(*min);
    let span = max.saturating_sub(*min).max(1);
    values
        .iter()
        .map(|value| {
            let scaled = (value - min) * 7 / span;
            let index = usize::try_from(scaled).unwrap_or(7).min(7);
            BLOCKS[index]
        })
        .collect()
}
