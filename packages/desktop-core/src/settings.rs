//! Stream settings: the persisted encoder and API endpoint choices.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSettings {
    pub fps: f64,
    pub bitrate_limit: f64,
    pub video_codec: String,
    pub resolution: String,
    pub api_endpoint: String,
    pub api_endpoint_is_custom: bool,
    pub pending_api_endpoint: Option<String>,
    pub auto_bitrate: bool,
    pub motion_mode: String,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            fps: 60.0,
            bitrate_limit: 20_000_000.0,
            video_codec: "vp8".into(),
            resolution: "1080p".into(),
            api_endpoint: "http://localhost:3001".into(),
            api_endpoint_is_custom: false,
            pending_api_endpoint: None,
            auto_bitrate: true,
            motion_mode: "auto".into(),
        }
    }
}

pub const VALID_CODECS: [&str; 5] = ["vp8", "h264", "h265", "vp9", "av1"];
pub const VALID_RESOLUTIONS: [&str; 5] = ["480p", "720p", "1080p", "1440p", "2160p"];
pub const VALID_MOTION_MODES: [&str; 4] = ["auto", "static", "mixed", "dynamic"];
pub const FPS_OPTIONS: [f64; 4] = [15.0, 24.0, 30.0, 60.0];

#[must_use]
pub fn default_stream_settings() -> StreamSettings {
    StreamSettings::default()
}

/// Reads a settings object leniently: each invalid field falls back to its default.
#[must_use]
pub fn sanitize_stream_settings(raw: &Value) -> StreamSettings {
    let defaults = StreamSettings::default();
    let Some(o) = raw.as_object() else {
        return defaults;
    };

    let num = |value: Option<&Value>, min: f64, max: f64, fallback: f64| {
        value
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= min && *v <= max)
            .unwrap_or(fallback)
    };
    let pick =
        |key: &str, valid: &[&str], fallback: String| match o.get(key).and_then(Value::as_str) {
            Some(v) if valid.contains(&v) => v.to_string(),
            _ => fallback,
        };

    let api_endpoint = match o.get("apiEndpoint").and_then(Value::as_str) {
        Some(v) if !v.trim().is_empty() => v.to_string(),
        _ => defaults.api_endpoint.clone(),
    };
    let api_endpoint_is_custom = o
        .get("apiEndpointIsCustom")
        .and_then(Value::as_bool)
        .unwrap_or(api_endpoint != defaults.api_endpoint);
    let pending_api_endpoint = o
        .get("pendingApiEndpoint")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);

    StreamSettings {
        fps: num(o.get("fps"), 1.0, 60.0, defaults.fps),
        bitrate_limit: num(
            o.get("bitrateLimit"),
            100_000.0,
            200_000_000.0,
            defaults.bitrate_limit,
        ),
        video_codec: pick("videoCodec", &VALID_CODECS, defaults.video_codec),
        resolution: pick("resolution", &VALID_RESOLUTIONS, defaults.resolution),
        api_endpoint,
        api_endpoint_is_custom,
        pending_api_endpoint,
        auto_bitrate: o
            .get("autoBitrate")
            .and_then(Value::as_bool)
            .unwrap_or(defaults.auto_bitrate),
        motion_mode: pick("motionMode", &VALID_MOTION_MODES, defaults.motion_mode),
    }
}

/// Pixel dimensions of a resolution preset.
#[must_use]
pub fn resolution_dims(preset: &str) -> (u32, u32) {
    match preset {
        "480p" => (854, 480),
        "1080p" => (1920, 1080),
        "1440p" => (2560, 1440),
        "2160p" => (3840, 2160),
        _ => (1280, 720),
    }
}

#[must_use]
pub fn resolution_label(preset: &str) -> &'static str {
    match preset {
        "480p" => "480p (854×480)",
        "720p" => "720p (1280×720)",
        "1440p" => "1440p (2560×1440)",
        "2160p" => "4K (3840×2160)",
        _ => "1080p (1920×1080)",
    }
}

#[must_use]
pub fn motion_label(mode: &str) -> &'static str {
    match mode {
        "static" => "Static (documents, slides)",
        "mixed" => "Mixed (browsing, demos)",
        "dynamic" => "Dynamic (gaming, video)",
        _ => "Auto-detect",
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "exact conformance values")]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_match_the_shared_table() {
        let defaults = default_stream_settings();
        assert_eq!(defaults.fps, 60.0);
        assert_eq!(defaults.bitrate_limit, 20_000_000.0);
        assert_eq!(defaults.video_codec, "vp8");
        assert_eq!(defaults.resolution, "1080p");
        assert_eq!(defaults.api_endpoint, "http://localhost:3001");
        assert!(!defaults.api_endpoint_is_custom);
        assert_eq!(defaults.pending_api_endpoint, None);
        assert!(defaults.auto_bitrate);
        assert_eq!(defaults.motion_mode, "auto");
    }

    #[test]
    fn non_objects_and_missing_fields_use_defaults() {
        for raw in [json!(null), json!(42), json!("x"), json!([1, 2]), json!({})] {
            assert_eq!(sanitize_stream_settings(&raw), default_stream_settings());
        }
        let partial = sanitize_stream_settings(&json!({ "fps": 30.0 }));
        assert_eq!(partial.fps, 30.0);
        assert_eq!(partial.resolution, "1080p");
    }

    #[test]
    fn numbers_are_range_checked() {
        let fps = |v: f64| sanitize_stream_settings(&json!({ "fps": v })).fps;
        assert_eq!(fps(0.0), 60.0);
        assert_eq!(fps(1.0), 1.0);
        assert_eq!(fps(59.5), 59.5);
        assert_eq!(fps(240.0), 60.0);
        assert_eq!(fps(f64::NAN), 60.0);
        let bitrate =
            |v: f64| sanitize_stream_settings(&json!({ "bitrateLimit": v })).bitrate_limit;
        assert_eq!(bitrate(100_000.0), 100_000.0);
        assert_eq!(bitrate(99_999.0), 20_000_000.0);
        assert_eq!(bitrate(200_000_001.0), 20_000_000.0);
        assert_eq!(
            sanitize_stream_settings(&json!({ "fps": "60", "bitrateLimit": "1000000" })),
            default_stream_settings()
        );
    }

    #[test]
    fn enumerations_are_whitelisted() {
        for valid in VALID_CODECS {
            assert_eq!(
                sanitize_stream_settings(&json!({ "videoCodec": valid })).video_codec,
                valid
            );
        }
        assert_eq!(
            sanitize_stream_settings(&json!({ "videoCodec": "VP8" })).video_codec,
            "vp8"
        );
        assert_eq!(
            sanitize_stream_settings(&json!({ "resolution": "4k" })).resolution,
            "1080p"
        );
        assert_eq!(
            sanitize_stream_settings(&json!({ "motionMode": "gaming" })).motion_mode,
            "auto"
        );
        assert!(sanitize_stream_settings(&json!({ "autoBitrate": "false" })).auto_bitrate);
        assert!(!sanitize_stream_settings(&json!({ "autoBitrate": false })).auto_bitrate);
    }

    #[test]
    fn endpoint_state_is_validated() {
        assert_eq!(
            sanitize_stream_settings(&json!({ "apiEndpoint": "   " })).api_endpoint,
            "http://localhost:3001"
        );
        let invalid = sanitize_stream_settings(&json!({
            "apiEndpointIsCustom": "true",
            "pendingApiEndpoint": " "
        }));
        assert!(!invalid.api_endpoint_is_custom);
        assert_eq!(invalid.pending_api_endpoint, None);
        let legacy =
            sanitize_stream_settings(&json!({ "apiEndpoint": "https://legacy.example.com" }));
        assert!(legacy.api_endpoint_is_custom);
    }

    #[test]
    fn serializes_with_the_existing_file_keys() {
        let value = serde_json::to_value(default_stream_settings()).unwrap_or(Value::Null);
        for key in [
            "fps",
            "bitrateLimit",
            "videoCodec",
            "resolution",
            "apiEndpoint",
            "apiEndpointIsCustom",
            "pendingApiEndpoint",
            "autoBitrate",
            "motionMode",
        ] {
            assert!(value.get(key).is_some(), "{key}");
        }
    }
}
