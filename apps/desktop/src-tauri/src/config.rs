//! `slopcast.config.json` parsing — a serde mirror of
//! `packages/shared-types/src/config.ts` (`loadConfig`): same defaults, same
//! env-override precedence, same upward file search, same production assert.
#![allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command arguments (State and owned payloads) must be taken by value for the #[tauri::command] macro"
)]

use std::path::PathBuf;

/// Fully resolved app configuration (internal state; the command surface only
/// exposes `apiEndpoint` + `livekitUrl`, matching the Electron handler).
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub server_port: u16,
    pub web_port: u16,
    pub api_endpoint: String,
    pub website_url: String,
    pub livekit_url: String,
    pub livekit_api_key: String,
    pub livekit_api_secret: String,
}

/// Per-field optional mirror of `AppConfig` for the JSON file and env vars.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartialConfig {
    server_port: Option<u16>,
    web_port: Option<u16>,
    api_endpoint: Option<String>,
    website_url: Option<String>,
    livekit_url: Option<String>,
    livekit_api_key: Option<String>,
    livekit_api_secret: Option<String>,
}

impl PartialConfig {
    fn from_env() -> Self {
        Self {
            server_port: env_u16("SERVER_PORT").or_else(|| env_u16("PORT")),
            web_port: env_u16("WEB_PORT"),
            api_endpoint: env_string("API_ENDPOINT"),
            website_url: env_string("WEBSITE_URL"),
            livekit_url: env_string("LIVEKIT_URL"),
            livekit_api_key: env_string("LIVEKIT_API_KEY"),
            livekit_api_secret: env_string("LIVEKIT_API_SECRET"),
        }
    }
}

fn env_u16(name: &str) -> Option<u16> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Walks the current directory upward looking for `slopcast.config.json`,
/// mirroring `findConfigFile` in `config.ts`.
fn find_config_file() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join("slopcast.config.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

impl AppConfig {
    /// Loads the config with the TS precedence: env wins over file, file wins
    /// over defaults; `apiEndpoint`/`websiteUrl` fall back to the chosen
    /// ports.
    ///
    /// # Errors
    ///
    /// Returns an error when the production assert fails (dev `LiveKit`
    /// credentials in a production build).
    pub fn load() -> Result<Self, String> {
        let env = PartialConfig::from_env();
        let defaults = Self::defaults(&env);

        let file = find_config_file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<PartialConfig>(&text).ok());

        let pick = |env: Option<u16>, file: Option<u16>, fallback: u16| {
            env.filter(|v| *v != 0)
                .or_else(|| file.filter(|v| *v != 0))
                .unwrap_or(fallback)
        };
        let pick_str = |env: Option<String>, file: Option<String>, fallback: String| {
            env.or(file).filter(|v| !v.is_empty()).unwrap_or(fallback)
        };

        let server_port = pick(
            env.server_port,
            file.as_ref().and_then(|f| f.server_port),
            defaults.server_port,
        );
        let web_port = pick(
            env.web_port,
            file.as_ref().and_then(|f| f.web_port),
            defaults.web_port,
        );
        let api_endpoint = pick_str(
            env.api_endpoint,
            file.as_ref().and_then(|f| f.api_endpoint.clone()),
            format!("http://localhost:{server_port}"),
        );
        let website_url = pick_str(
            env.website_url,
            file.as_ref().and_then(|f| f.website_url.clone()),
            format!("http://localhost:{web_port}"),
        );
        let livekit_url = pick_str(
            env.livekit_url,
            file.as_ref().and_then(|f| f.livekit_url.clone()),
            defaults.livekit_url,
        );
        let livekit_api_key = pick_str(
            env.livekit_api_key,
            file.as_ref().and_then(|f| f.livekit_api_key.clone()),
            defaults.livekit_api_key,
        );
        let livekit_api_secret = pick_str(
            env.livekit_api_secret,
            file.as_ref().and_then(|f| f.livekit_api_secret.clone()),
            defaults.livekit_api_secret,
        );

        let config = Self {
            server_port,
            web_port,
            api_endpoint,
            website_url,
            livekit_url,
            livekit_api_key,
            livekit_api_secret,
        };
        assert_production_config(&config)?;
        Ok(config)
    }

    fn defaults(env: &PartialConfig) -> Self {
        Self {
            server_port: env.server_port.unwrap_or(3001),
            web_port: env.web_port.unwrap_or(3000),
            api_endpoint: env
                .api_endpoint
                .clone()
                .unwrap_or_else(|| format!("http://localhost:{}", env.server_port.unwrap_or(3001))),
            website_url: env
                .website_url
                .clone()
                .unwrap_or_else(|| format!("http://localhost:{}", env.web_port.unwrap_or(3000))),
            livekit_url: env
                .livekit_url
                .clone()
                .unwrap_or_else(|| "ws://localhost:7880".into()),
            livekit_api_key: env
                .livekit_api_key
                .clone()
                .unwrap_or_else(|| "devkey".into()),
            livekit_api_secret: env
                .livekit_api_secret
                .clone()
                .unwrap_or_else(|| "secret".into()),
        }
    }
}

/// Mirrors `assertProductionConfig` in `config.ts`: production builds must
/// not ship the dev `LiveKit` credentials.
fn assert_production_config(config: &AppConfig) -> Result<(), String> {
    let production = std::env::var("NODE_ENV").is_ok_and(|v| v == "production")
        && !std::env::var("ALLOW_DEV_KEYS").is_ok_and(|v| v == "true");
    if production && (config.livekit_api_key == "devkey" || config.livekit_api_secret == "secret") {
        return Err(
            "Production environment requires custom LIVEKIT_API_KEY and LIVEKIT_API_SECRET".into(),
        );
    }
    Ok(())
}

/// Managed state holding the config loaded once at startup (mirrors the
/// Electron main process loading `loadConfig()` at module scope).
pub struct AppConfigState(pub AppConfig);

impl AppConfigState {
    /// Loads the app config once at startup.
    ///
    /// # Errors
    ///
    /// Returns an error when the production assert fails (dev `LiveKit`
    /// credentials in a production build).
    pub fn load() -> Result<Self, String> {
        AppConfig::load().map(Self)
    }
}

/// The subset the renderer consumes, identical to the Electron
/// `get-app-config` handler.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicAppConfig {
    pub api_endpoint: String,
    pub livekit_url: String,
}

/// Returns the app config subset (`apiEndpoint`, `livekitUrl`).
#[must_use]
#[tauri::command]
pub fn get_app_config(state: tauri::State<'_, AppConfigState>) -> PublicAppConfig {
    PublicAppConfig {
        api_endpoint: state.0.api_endpoint.clone(),
        livekit_url: state.0.livekit_url.clone(),
    }
}
