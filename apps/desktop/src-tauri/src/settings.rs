//! Persistence for `stream-settings.json` and `onboarding.json` in the app
//! config dir (`~/.config/slopcast`).
//!
//! TS↔Rust sync rule: `StreamSettings`, `default_stream_settings` and
//! `sanitize_stream_settings` mirror `DEFAULT_STREAM_SETTINGS` and
//! `sanitizeStreamSettings` in `packages/shared-types/src/index.ts`
//! field-for-field (same defaults, same clamps, same whitelists). Update both
//! files together; the `defaults_match_ts_table` test below enforces the
//! default values.
#![allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command arguments (State and owned payloads) must be taken by value for the #[tauri::command] macro"
)]

use std::path::PathBuf;

use crate::AppHandle;
use tauri::Manager;

const STREAM_SETTINGS_FILE: &str = "stream-settings.json";
const ONBOARDING_FILE: &str = "onboarding.json";

/// User-configurable encoder parameters, persisted to `stream-settings.json`.
/// Numeric fields are `f64` (like the TS `number`) so the sanitizer is a
/// faithful port — `bitrateLimit`/`fps` stay exact up to 2^53.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSettings {
    pub fps: f64,
    pub bitrate_limit: f64,
    pub video_codec: String,
    pub resolution: String,
    pub api_endpoint: String,
}

/// Defensive copy on every call (mirrors the TS spread of the shared default).
///
/// TS↔Rust sync rule: values must match `DEFAULT_STREAM_SETTINGS` in
/// `packages/shared-types/src/index.ts` (vp8 because the bundled libwebrtc's
/// VA-API H264 path collapses to ~1-3 fps on Linux; see the TS comment).
#[must_use]
pub fn default_stream_settings() -> StreamSettings {
    StreamSettings {
        fps: 60.0,
        bitrate_limit: 20_000_000.0,
        video_codec: "vp8".into(),
        resolution: "1080p".into(),
        api_endpoint: "http://localhost:3001".into(),
    }
}

const VALID_CODECS: [&str; 4] = ["vp8", "h264", "vp9", "av1"];
const VALID_RESOLUTIONS: [&str; 5] = ["480p", "720p", "1080p", "1440p", "2160p"];

/// Field-for-field port of `sanitizeStreamSettings` in shared-types: numbers
/// must be finite and within `[min, max]`, codecs/resolutions must be
/// whitelisted, `apiEndpoint` must be a non-blank string; anything else falls
/// back to the default.
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
        _ => defaults.api_endpoint,
    };

    StreamSettings {
        // fps is capped at 60: the capture pacer (`PREVIEW_MAX_FPS`) and the
        // preview emitter both clamp to 60 regardless, so higher values
        // silently ran the stream at 60 fps with a 120 fps SDP claim (and a
        // GOP key-int-max that assumed the configured framerate).
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
    }
}

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("failed to resolve app config dir: {e}"))
}

/// Reads `stream-settings.json`, falling back to defaults on any read/parse
/// failure (mirrors `loadStreamSettings`).
///
/// # Errors
///
/// Returns an error if the app config dir cannot be resolved.
#[tauri::command]
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

/// Sanitizes and persists `stream-settings.json` (2-space indented, trailing
/// newline).
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

/// Mirrors `isOnboardingCompleted`: the file must parse and carry
/// `completed === true`.
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

/// Mirrors `setOnboardingCompleted`: writes `{ "completed": true }`.
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

// The conformance values are exactly representable in f64 (60.0,
// 20_000_000.0, 59.5, …), so strict equality is the point of the tests.
#[cfg(test)]
#[allow(clippy::float_cmp, reason = "exact conformance values, see above")]
mod tests {
    use super::*;
    use serde_json::json;

    // TS↔Rust sync rule: these values must match DEFAULT_STREAM_SETTINGS in
    // packages/shared-types/src/index.ts (fps 60, bitrateLimit 20_000_000,
    // videoCodec 'vp8', resolution '1080p', apiEndpoint 'http://localhost:3001').
    #[test]
    fn defaults_match_ts_table() {
        let defaults = default_stream_settings();
        assert_eq!(defaults.fps, 60.0);
        assert_eq!(defaults.bitrate_limit, 20_000_000.0);
        assert_eq!(defaults.video_codec, "vp8");
        assert_eq!(defaults.resolution, "1080p");
        assert_eq!(defaults.api_endpoint, "http://localhost:3001");
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
        assert_eq!(sanitize_fps(0.0), 60.0); // below min
        assert_eq!(sanitize_fps(1.0), 1.0); // min edge
        // Max edge: fps is capped at 60 (the capture pacer and preview
        // emitter clamp there regardless — see the sanitizer comment).
        assert_eq!(sanitize_fps(60.0), 60.0);
        assert_eq!(sanitize_fps(240.0), 60.0); // above max
        assert_eq!(sanitize_fps(241.0), 60.0); // above max
        assert_eq!(sanitize_fps(59.5), 59.5); // fractional kept (TS number)
        assert_eq!(sanitize_fps(f64::NAN), 60.0); // non-finite
    }

    #[test]
    fn sanitize_clamps_bitrate_like_ts() {
        let sanitize_bitrate =
            |b: f64| sanitize_stream_settings(&json!({ "bitrateLimit": b })).bitrate_limit;
        assert_eq!(sanitize_bitrate(100_000.0), 100_000.0); // min edge
        assert_eq!(sanitize_bitrate(99_999.0), 20_000_000.0); // below min
        assert_eq!(sanitize_bitrate(200_000_000.0), 200_000_000.0); // max edge
        assert_eq!(sanitize_bitrate(200_000_001.0), 20_000_000.0); // above max
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
    fn sanitize_ignores_string_numbers() {
        // TS `typeof v === 'number'` rejects string-typed numbers.
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
    }
}
