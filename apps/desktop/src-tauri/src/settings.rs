#![allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command arguments (State and owned payloads) must be taken by value for the #[tauri::command] macro"
)]

use std::path::PathBuf;

use crate::AppHandle;
use tauri::Manager;

const STREAM_SETTINGS_FILE: &str = "stream-settings.json";
const ONBOARDING_FILE: &str = "onboarding.json";

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

#[must_use]
pub fn default_stream_settings() -> StreamSettings {
    StreamSettings {
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

const VALID_CODECS: [&str; 5] = ["vp8", "h264", "h265", "vp9", "av1"];
const VALID_RESOLUTIONS: [&str; 5] = ["480p", "720p", "1080p", "1440p", "2160p"];
const VALID_MOTION_MODES: [&str; 4] = ["auto", "static", "mixed", "dynamic"];

#[must_use]
pub fn sanitize_stream_settings(raw: &serde_json::Value) -> StreamSettings {
    let defaults = default_stream_settings();
    let Some(o) = raw.as_object() else {
        return defaults;
    };

    let num = |value: Option<&serde_json::Value>, min: f64, max: f64, fallback: f64| {
        value
            .and_then(serde_json::Value::as_f64)
            .filter(|v| v.is_finite() && *v >= min && *v <= max)
            .unwrap_or(fallback)
    };

    let codec = match o.get("videoCodec").and_then(serde_json::Value::as_str) {
        Some(v) if VALID_CODECS.contains(&v) => v.to_string(),
        _ => defaults.video_codec,
    };
    let resolution = match o.get("resolution").and_then(serde_json::Value::as_str) {
        Some(v) if VALID_RESOLUTIONS.contains(&v) => v.to_string(),
        _ => defaults.resolution,
    };
    let api_endpoint = match o.get("apiEndpoint").and_then(serde_json::Value::as_str) {
        Some(v) if !v.trim().is_empty() => v.to_string(),
        _ => defaults.api_endpoint.clone(),
    };
    let api_endpoint_is_custom = o
        .get("apiEndpointIsCustom")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(api_endpoint != defaults.api_endpoint);
    let pending_api_endpoint = o
        .get("pendingApiEndpoint")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let motion_mode = match o.get("motionMode").and_then(serde_json::Value::as_str) {
        Some(v) if VALID_MOTION_MODES.contains(&v) => v.to_string(),
        _ => defaults.motion_mode,
    };
    let auto_bitrate = match o.get("autoBitrate").and_then(serde_json::Value::as_bool) {
        Some(v) => v,
        _ => defaults.auto_bitrate,
    };

    StreamSettings {
        fps: num(o.get("fps"), 1.0, 60.0, defaults.fps),
        bitrate_limit: num(
            o.get("bitrateLimit"),
            100_000.0,
            200_000_000.0,
            defaults.bitrate_limit,
        ),
        video_codec: codec,
        resolution,
        api_endpoint,
        api_endpoint_is_custom,
        pending_api_endpoint,
        auto_bitrate,
        motion_mode,
    }
}

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("failed to resolve app config dir: {e}"))
}

#[tauri::command]
/// # Errors
/// Returns an error if the settings directory cannot be read.
pub fn get_stream_settings(app: AppHandle) -> Result<StreamSettings, String> {
    let path = config_dir(&app)?.join(STREAM_SETTINGS_FILE);
    let parsed = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => value,
            Err(e) => {
                eprintln!("Failed to parse {STREAM_SETTINGS_FILE}, using defaults: {e}");
                serde_json::Value::Null
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Value::Null,
        Err(e) => {
            eprintln!("Failed to read {STREAM_SETTINGS_FILE}, using defaults: {e}");
            serde_json::Value::Null
        }
    };
    Ok(sanitize_stream_settings(&parsed))
}

#[must_use]
#[tauri::command]
pub fn save_stream_settings(app: AppHandle, settings: serde_json::Value) -> bool {
    let sanitized = sanitize_stream_settings(&settings);
    let Ok(path) = config_dir(&app) else {
        return false;
    };
    let path = path.join(STREAM_SETTINGS_FILE);
    let Ok(json) = serde_json::to_string_pretty(&sanitized) else {
        return false;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, format!("{json}\n")) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("Failed to write {STREAM_SETTINGS_FILE}: {e}");
            false
        }
    }
}

#[must_use]
#[tauri::command]
pub fn get_onboarding_completed(app: AppHandle) -> bool {
    let Ok(dir) = config_dir(&app) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(dir.join(ONBOARDING_FILE)) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("completed").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

#[must_use]
#[tauri::command]
pub fn set_onboarding_completed(app: AppHandle) -> bool {
    let Ok(dir) = config_dir(&app) else {
        return false;
    };
    let path = dir.join(ONBOARDING_FILE);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, r#"{"completed":true}"#) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("Failed to write {ONBOARDING_FILE}: {e}");
            false
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "exact conformance values, see above")]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_match_ts_table() {
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
    fn sanitize_rejects_non_object() {
        for raw in [json!(null), json!(42), json!("x"), json!([1, 2])] {
            assert_eq!(sanitize_stream_settings(&raw), default_stream_settings());
        }
    }

    #[test]
    fn sanitize_fills_missing_fields_with_defaults() {
        let empty = sanitize_stream_settings(&json!({}));
        assert_eq!(empty, default_stream_settings());

        let partial = sanitize_stream_settings(&json!({ "fps": 30.0 }));
        let mut expected = default_stream_settings();
        expected.fps = 30.0;
        assert_eq!(partial, expected);
    }

    #[test]
    fn sanitize_clamps_fps_like_ts() {
        let sanitize_fps = |fps: f64| sanitize_stream_settings(&json!({ "fps": fps })).fps;
        assert_eq!(sanitize_fps(0.0), 60.0);
        assert_eq!(sanitize_fps(1.0), 1.0);
        assert_eq!(sanitize_fps(60.0), 60.0);
        assert_eq!(sanitize_fps(240.0), 60.0);
        assert_eq!(sanitize_fps(241.0), 60.0);
        assert_eq!(sanitize_fps(59.5), 59.5);
        assert_eq!(sanitize_fps(f64::NAN), 60.0);
    }

    #[test]
    fn sanitize_clamps_bitrate_like_ts() {
        let sanitize_bitrate =
            |b: f64| sanitize_stream_settings(&json!({ "bitrateLimit": b })).bitrate_limit;
        assert_eq!(sanitize_bitrate(100_000.0), 100_000.0);
        assert_eq!(sanitize_bitrate(99_999.0), 20_000_000.0);
        assert_eq!(sanitize_bitrate(200_000_000.0), 200_000_000.0);
        assert_eq!(sanitize_bitrate(200_000_001.0), 20_000_000.0);
    }

    #[test]
    fn sanitize_validates_codec_whitelist() {
        let sanitize_codec =
            |c: &str| sanitize_stream_settings(&json!({ "videoCodec": c })).video_codec;
        for valid in VALID_CODECS {
            assert_eq!(sanitize_codec(valid), valid);
        }
        assert_eq!(sanitize_codec("theora"), "vp8");
        assert_eq!(sanitize_codec(""), "vp8");
        assert_eq!(sanitize_codec("VP8"), "vp8");
    }

    #[test]
    fn sanitize_validates_resolution_whitelist() {
        let sanitize_res =
            |r: &str| sanitize_stream_settings(&json!({ "resolution": r })).resolution;
        for valid in VALID_RESOLUTIONS {
            assert_eq!(sanitize_res(valid), valid);
        }
        assert_eq!(sanitize_res("4k"), "1080p");
        assert_eq!(sanitize_res(""), "1080p");
    }

    #[test]
    fn sanitize_validates_endpoint() {
        let sanitize_endpoint = |e: serde_json::Value| {
            sanitize_stream_settings(&json!({ "apiEndpoint": e })).api_endpoint
        };
        assert_eq!(
            sanitize_endpoint(json!("https://example.com")),
            "https://example.com"
        );
        assert_eq!(sanitize_endpoint(json!("")), "http://localhost:3001");
        assert_eq!(sanitize_endpoint(json!("   ")), "http://localhost:3001");
        assert_eq!(sanitize_endpoint(json!(42)), "http://localhost:3001");
    }

    #[test]
    fn sanitize_validates_endpoint_state() {
        let sanitized = sanitize_stream_settings(&json!({
            "apiEndpointIsCustom": true,
            "pendingApiEndpoint": "https://pending.example.com"
        }));
        assert!(sanitized.api_endpoint_is_custom);
        assert_eq!(
            sanitized.pending_api_endpoint,
            Some("https://pending.example.com".into())
        );

        let invalid = sanitize_stream_settings(&json!({
            "apiEndpointIsCustom": "true",
            "pendingApiEndpoint": " "
        }));
        assert!(!invalid.api_endpoint_is_custom);
        assert_eq!(invalid.pending_api_endpoint, None);

        let legacy = sanitize_stream_settings(&json!({
            "apiEndpoint": "https://legacy.example.com"
        }));
        assert!(legacy.api_endpoint_is_custom);
    }

    #[test]
    fn sanitize_validates_auto_bitrate() {
        let sanitize_auto = |v: serde_json::Value| {
            sanitize_stream_settings(&json!({ "autoBitrate": v })).auto_bitrate
        };
        assert!(!sanitize_auto(json!(false)));
        assert!(sanitize_auto(json!(true)));
        assert!(sanitize_auto(json!(null)));
        assert!(sanitize_auto(json!("false")));
    }

    #[test]
    fn sanitize_validates_motion_mode() {
        let sanitize_motion =
            |v: &str| sanitize_stream_settings(&json!({ "motionMode": v })).motion_mode;
        for valid in VALID_MOTION_MODES {
            assert_eq!(sanitize_motion(valid), valid);
        }
        assert_eq!(sanitize_motion("gaming"), "auto");
        assert_eq!(sanitize_motion(""), "auto");
    }

    #[test]
    fn sanitize_ignores_string_numbers() {
        let sanitized =
            sanitize_stream_settings(&json!({ "fps": "60", "bitrateLimit": "1000000" }));
        assert_eq!(sanitized, default_stream_settings());
    }

    #[test]
    fn sanitize_serializes_camel_case_for_the_file() {
        let settings = sanitize_stream_settings(&json!({ "fps": 30.0 }));
        let json = serde_json::to_value(&settings).unwrap_or_else(|e| panic!("serialize: {e}"));
        let obj = json.as_object().unwrap_or_else(|| panic!("not an object"));
        assert!(obj.contains_key("fps"));
        assert!(obj.contains_key("bitrateLimit"));
        assert!(obj.contains_key("videoCodec"));
        assert!(obj.contains_key("resolution"));
        assert!(obj.contains_key("apiEndpoint"));
        assert!(obj.contains_key("apiEndpointIsCustom"));
        assert!(obj.contains_key("pendingApiEndpoint"));
    }
}
