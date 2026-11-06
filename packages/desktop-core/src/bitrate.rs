//! Bitrate recommendations, ported from the renderer's `utils/bitrate.ts`.

fn av1_ceiling(resolution: &str) -> f64 {
    match resolution {
        "480p" => 2_000_000.0,
        "720p" => 4_000_000.0,
        "1440p" => 12_000_000.0,
        "2160p" => 20_000_000.0,
        _ => 8_000_000.0,
    }
}

fn codec_scale(codec: &str) -> f64 {
    match codec {
        "av1" => 1.0,
        "h264" => 1.5,
        "h265" | "vp9" => 1.4,
        _ => 1.7,
    }
}

fn motion_factor(tier: &str) -> f64 {
    match tier {
        "dynamic" => 1.5,
        "mixed" => 1.25,
        _ => 1.0,
    }
}

fn snap(value: f64) -> f64 {
    (value / 500_000.0).round() * 500_000.0
}

#[must_use]
pub fn recommend_bitrate_cap(codec: &str, resolution: &str, fps: f64, motion_tier: &str) -> f64 {
    let fps_scale = 0.6 + 0.4 * (fps.clamp(1.0, 60.0) / 60.0);
    let raw = av1_ceiling(resolution) * fps_scale * codec_scale(codec) * motion_factor(motion_tier);
    snap(raw).max(1_000_000.0)
}

#[must_use]
pub fn recommended_bitrate_range(
    codec: &str,
    resolution: &str,
    fps: f64,
    motion_tier: &str,
) -> (f64, f64) {
    let maximum = recommend_bitrate_cap(codec, resolution, fps, motion_tier);
    (snap(maximum * 2.0 / 3.0).max(1_000_000.0), maximum)
}

#[must_use]
pub fn manual_bitrate_options(codec: &str) -> &'static [f64] {
    if codec == "av1" {
        &[1e6, 2e6, 4e6, 6e6, 8e6, 12e6, 16e6, 20e6]
    } else {
        &[1e6, 2e6, 4e6, 6e6, 10e6, 20e6, 30e6, 50e6]
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "values are snapped to exact multiples")]
mod tests {
    use super::*;

    #[test]
    fn matches_the_renderer_recommendations() {
        assert_eq!(recommend_bitrate_cap("av1", "1080p", 60.0, "static"), 8e6);
        assert_eq!(recommend_bitrate_cap("av1", "1440p", 60.0, "static"), 12e6);
        assert_eq!(recommend_bitrate_cap("av1", "1080p", 60.0, "dynamic"), 12e6);
        assert_eq!(recommend_bitrate_cap("av1", "1440p", 60.0, "dynamic"), 18e6);
        assert!(
            recommend_bitrate_cap("h264", "1080p", 60.0, "static")
                > recommend_bitrate_cap("av1", "1080p", 60.0, "static")
        );
        assert_eq!(
            recommend_bitrate_cap("h265", "1080p", 60.0, "static"),
            recommend_bitrate_cap("vp9", "1080p", 60.0, "static")
        );
        assert!(
            recommend_bitrate_cap("av1", "1080p", 30.0, "static")
                < recommend_bitrate_cap("av1", "1080p", 60.0, "static")
        );
        assert!(
            manual_bitrate_options("av1")
                .contains(&recommend_bitrate_cap("av1", "1080p", 60.0, "static"))
        );
        assert_eq!(
            recommended_bitrate_range("h264", "1080p", 60.0, "static"),
            (8e6, 12e6)
        );
        assert!(manual_bitrate_options("av1").iter().all(|v| *v <= 20e6));
    }
}
