//! The DPS chart: damage per time bucket, smoothed and turned into path commands in the chart's pixels.

/// DPS is averaged over this many seconds, so single big hits do not make spikes.
const SMOOTHING_SECONDS: f64 = 5.0;

/// The DPS of `series` (damage per `bucket_seconds`) at each point, averaged over the last `SMOOTHING_SECONDS`.
pub fn smoothed_dps(series: &[i64], bucket_seconds: f64) -> Vec<f64> {
    let window = ((SMOOTHING_SECONDS / bucket_seconds.max(1e-3)).ceil() as usize).max(1);
    let mut sum = 0i64;
    let mut out = Vec::with_capacity(series.len());
    for (i, &v) in series.iter().enumerate() {
        sum += v;
        if i >= window {
            sum -= series[i - window];
        }
        let span = (i + 1).min(window) as f64 * bucket_seconds;
        out.push(sum as f64 / span);
    }
    out
}

/// A polyline through `values` in a `width` x `height` box, the highest value `top` at the top edge.
pub fn path_commands(values: &[f64], top: f64, width: f32, height: f32) -> String {
    if values.is_empty() || top <= 0.0 || width <= 0.0 || height <= 0.0 {
        return String::new();
    }
    let step = if values.len() > 1 { width as f64 / (values.len() - 1) as f64 } else { 0.0 };
    let mut out = String::with_capacity(values.len() * 16);
    for (i, v) in values.iter().enumerate() {
        let x = i as f64 * step;
        let y = height as f64 * (1.0 - (v / top).clamp(0.0, 1.0));
        out.push_str(&format!("{}{x:.1} {y:.1} ", if i == 0 { "M" } else { "L" }));
    }
    if values.len() == 1 {
        out.push_str(&format!("L{width:.1} {:.1}", height as f64 * (1.0 - (values[0] / top).clamp(0.0, 1.0))));
    }
    out
}

/// A round number at or above `max` for the top of the axis: 1, 2 or 5 times a power of ten.
pub fn nice_top(max: f64) -> f64 {
    if max <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(max.log10().floor());
    [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * magnitude).find(|v| *v >= max).unwrap_or(10.0 * magnitude)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dps_is_averaged_over_the_window() {
        // one hit of 1000 every second: 1000 dps throughout
        let dps = smoothed_dps(&[1000; 10], 1.0);
        assert!(dps.iter().all(|d| (*d - 1000.0).abs() < 1e-9));
        // a single hit of 5000 spreads over five seconds
        let dps = smoothed_dps(&[5000, 0, 0, 0, 0, 0, 0], 1.0);
        assert_eq!(dps[4], 1000.0);
        assert_eq!(dps[5], 0.0);
    }

    #[test]
    fn paths_span_the_box() {
        assert_eq!(path_commands(&[0.0, 50.0, 100.0], 100.0, 200.0, 100.0), "M0.0 100.0 L100.0 50.0 L200.0 0.0 ");
        assert_eq!(path_commands(&[], 100.0, 200.0, 100.0), "");
    }

    #[test]
    fn axis_tops_are_round() {
        assert_eq!(nice_top(730.0), 1000.0);
        assert_eq!(nice_top(1500.0), 2000.0);
        assert_eq!(nice_top(4200.0), 5000.0);
        assert_eq!(nice_top(0.0), 1.0);
    }
}
