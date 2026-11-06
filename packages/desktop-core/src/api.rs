//! The Slopcast server's presenter API: rooms, spectators, heartbeats and health.

use std::time::Duration;

use serde_json::Value;

const DESKTOP_ORIGIN: &str = "desktop";
const ENDPOINT_TIMEOUT: Duration = Duration::from_secs(5);

/// A room created on the server. The token and close key stay in memory only.
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedRoom {
    pub api_endpoint: String,
    pub close_key: String,
    pub code: String,
    pub identity: String,
    pub livekit_url: String,
    pub share_url: String,
    pub token: String,
}

impl std::fmt::Debug for PreparedRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedRoom")
            .field("api_endpoint", &self.api_endpoint)
            .field("code", &self.code)
            .field("identity", &self.identity)
            .field("livekit_url", &self.livekit_url)
            .field("share_url", &self.share_url)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatResult {
    Ok {
        token: String,
    },
    /// The server no longer knows the room (closed elsewhere or reaped).
    Closed,
    /// Network trouble, rate limiting, a restarting server: keep streaming and retry.
    Retry {
        reason: String,
    },
    /// A server without the heartbeat route.
    Unsupported,
}

fn read_str<'a>(body: Option<&'a Value>, key: &str) -> Option<&'a str> {
    body?.as_object()?.get(key)?.as_str()
}

/// Classifies a `POST /presenter-token` response.
#[must_use]
pub fn heartbeat_result(status: u16, body: Option<&Value>) -> HeartbeatResult {
    if status == 200 {
        return match read_str(body, "token") {
            Some(token) if !token.is_empty() => HeartbeatResult::Ok {
                token: token.into(),
            },
            _ => HeartbeatResult::Retry {
                reason: "heartbeat response is missing a token".into(),
            },
        };
    }
    // Only the route's own JSON 404 means "room gone"; a bare 404 is an older server.
    let has_error_body = read_str(body, "error").is_some();
    match status {
        404 if has_error_body => HeartbeatResult::Closed,
        404 => HeartbeatResult::Unsupported,
        403 if has_error_body => HeartbeatResult::Closed,
        _ => HeartbeatResult::Retry {
            reason: format!("server returned {status}"),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointErrorKind {
    Invalid,
    Timeout,
    Unreachable,
    Unexpected,
    Unhealthy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointFailure {
    pub kind: EndpointErrorKind,
    pub message: String,
}

fn failure(kind: EndpointErrorKind, message: &str) -> EndpointFailure {
    EndpointFailure {
        kind,
        message: message.into(),
    }
}

/// Normalizes a base URL: http(s), no credentials, query or fragment, no trailing slash.
///
/// # Errors
/// Returns an `Invalid` failure describing the problem.
pub fn normalize_api_endpoint(input: &str) -> Result<String, EndpointFailure> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(failure(
            EndpointErrorKind::Invalid,
            "Enter an API endpoint.",
        ));
    }
    let Ok(mut parsed) = url::Url::parse(trimmed) else {
        return Err(failure(
            EndpointErrorKind::Invalid,
            "Enter a valid URL, such as https://share.example.com.",
        ));
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(failure(
            EndpointErrorKind::Invalid,
            "Use an http:// or https:// endpoint.",
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(failure(
            EndpointErrorKind::Invalid,
            "Use a base URL without credentials, a query, or a fragment.",
        ));
    }
    let path = parsed.path().trim_end_matches('/').to_string();
    parsed.set_path(&path);
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

/// Classifies a `/api/health` response.
///
/// # Errors
/// Returns the reason the response is not a healthy Slopcast API.
pub fn health_result(status: u16, body: Option<&Value>) -> Result<(), EndpointFailure> {
    if status == 503 {
        return Err(failure(
            EndpointErrorKind::Unhealthy,
            "The API is online, but its LiveKit service is unavailable.",
        ));
    }
    if status != 200 {
        return Err(EndpointFailure {
            kind: EndpointErrorKind::Unexpected,
            message: format!("The endpoint returned HTTP {status} instead of a healthy response."),
        });
    }
    match body {
        None => Err(failure(
            EndpointErrorKind::Unexpected,
            "The endpoint returned an unreadable health response.",
        )),
        Some(body) if read_str(Some(body), "status") == Some("ok") => Ok(()),
        Some(_) => Err(failure(
            EndpointErrorKind::Unexpected,
            "The endpoint did not identify itself as a healthy Slopcast API.",
        )),
    }
}

#[derive(Clone)]
pub struct RoomApi {
    client: reqwest::Client,
}

impl Default for RoomApi {
    fn default() -> Self {
        Self::new()
    }
}

async fn json_body(response: reqwest::Response) -> Option<Value> {
    response.json::<Value>().await.ok()
}

fn error_message(body: Option<&Value>, fallback: String) -> String {
    read_str(body, "error")
        .filter(|message| !message.is_empty())
        .map_or(fallback, str::to_string)
}

impl RoomApi {
    #[must_use]
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("Slopcast-Desktop/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|error| {
                log::error!("Cannot configure the HTTP client, using defaults: {error}");
                reqwest::Client::new()
            });
        Self { client }
    }

    /// # Errors
    /// Returns the server's error message, or a description of the transport failure.
    pub async fn create_room(&self, api_endpoint: &str) -> Result<PreparedRoom, String> {
        let response = self
            .client
            .post(format!("{api_endpoint}/api/rooms"))
            .header("Content-Type", "application/json")
            .header("X-Client-Origin", DESKTOP_ORIGIN)
            .send()
            .await
            .map_err(|error| format!("The API could not be reached: {error}"))?;
        let status = response.status();
        let body = json_body(response).await;
        if !status.is_success() {
            return Err(error_message(
                body.as_ref(),
                format!("Server returned {}", status.as_u16()),
            ));
        }
        let body = body.ok_or("Room response was not a JSON object")?;
        let field = |key: &str| {
            read_str(Some(&body), key)
                .map(str::to_string)
                .ok_or_else(|| format!("Room response is missing {key}"))
        };
        Ok(PreparedRoom {
            api_endpoint: api_endpoint.into(),
            close_key: field("closeKey")?,
            code: field("code")?,
            identity: field("identity")?,
            livekit_url: field("livekitUrl")?,
            share_url: field("shareUrl")?,
            token: field("token")?,
        })
    }

    /// Deletes the room; a room the server no longer knows counts as deleted.
    ///
    /// # Errors
    /// Returns the server's error message or a reachability message.
    pub async fn delete_room(&self, room: &PreparedRoom) -> Result<(), String> {
        let response = self
            .client
            .delete(format!("{}/api/rooms/{}", room.api_endpoint, room.code))
            .header("X-Room-Close-Key", &room.close_key)
            .send()
            .await
            .map_err(|_| "The API could not be reached. Check the endpoint and try again.")?;
        let status = response.status();
        if status.is_success() || status.as_u16() == 404 {
            return Ok(());
        }
        let body = json_body(response).await;
        Err(error_message(
            body.as_ref(),
            format!("Server returned {}", status.as_u16()),
        ))
    }

    pub async fn spectator_count(&self, room: &PreparedRoom) -> Option<u32> {
        let response = self
            .client
            .get(format!(
                "{}/api/rooms/{}/spectators",
                room.api_endpoint, room.code
            ))
            .timeout(ENDPOINT_TIMEOUT)
            .send()
            .await
            .inspect_err(|error| log::warn!("Transient spectator count failure: {error}"))
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let count = json_body(response).await?.get("count")?.as_f64()?;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a finite, clamped spectator count"
        )]
        (count.is_finite()).then(|| count.clamp(0.0, f64::from(u32::MAX)) as u32)
    }

    pub async fn heartbeat(&self, room: &PreparedRoom, timeout: Duration) -> HeartbeatResult {
        let request = self
            .client
            .post(format!(
                "{}/api/rooms/{}/presenter-token",
                room.api_endpoint, room.code
            ))
            .header("X-Room-Close-Key", &room.close_key)
            .timeout(timeout)
            .send();
        match request.await {
            Ok(response) => {
                let status = response.status().as_u16();
                let body = json_body(response).await;
                heartbeat_result(status, body.as_ref())
            }
            Err(error) => HeartbeatResult::Retry {
                reason: error.to_string(),
            },
        }
    }

    /// Checks that `input` is a reachable, healthy Slopcast API and returns it normalized.
    ///
    /// # Errors
    /// Returns a classified failure with a user-facing message.
    pub async fn check_endpoint(&self, input: &str) -> Result<String, EndpointFailure> {
        let endpoint = normalize_api_endpoint(input)?;
        let response = self
            .client
            .get(format!("{endpoint}/api/health"))
            .timeout(ENDPOINT_TIMEOUT)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    failure(
                        EndpointErrorKind::Timeout,
                        "The endpoint did not respond within 5 seconds.",
                    )
                } else {
                    failure(
                        EndpointErrorKind::Unreachable,
                        "The server could not be reached. Check the URL and your network.",
                    )
                }
            })?;
        let status = response.status().as_u16();
        let body = if status == 200 {
            json_body(response).await
        } else {
            None
        };
        health_result(status, body.as_ref())?;
        Ok(endpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn heartbeat_classification_matches_the_renderer() {
        assert_eq!(
            heartbeat_result(200, Some(&json!({ "identity": "p", "token": "jwt" }))),
            HeartbeatResult::Ok {
                token: "jwt".into()
            }
        );
        assert!(matches!(
            heartbeat_result(200, Some(&json!({}))),
            HeartbeatResult::Retry { .. }
        ));
        assert_eq!(
            heartbeat_result(404, Some(&json!({ "error": "Room not found or closed" }))),
            HeartbeatResult::Closed
        );
        assert_eq!(
            heartbeat_result(403, Some(&json!({ "error": "Invalid room close key" }))),
            HeartbeatResult::Closed
        );
        for (status, body) in [
            (503, Some(json!({ "error": "unavailable" }))),
            (429, Some(json!({ "error": "Too many" }))),
            (502, None),
            (403, None),
        ] {
            assert!(matches!(
                heartbeat_result(status, body.as_ref()),
                HeartbeatResult::Retry { .. }
            ));
        }
        assert_eq!(heartbeat_result(404, None), HeartbeatResult::Unsupported);
    }

    #[test]
    fn endpoint_normalization() {
        assert_eq!(
            normalize_api_endpoint(" https://example.com/slopcast/// "),
            Ok("https://example.com/slopcast".into())
        );
        assert_eq!(
            normalize_api_endpoint("http://localhost:3001/"),
            Ok("http://localhost:3001".into())
        );
        for endpoint in [
            "",
            "not-a-url",
            "ftp://example.com",
            "https://user:secret@example.com",
            "https://example.com?mode=test",
            "https://example.com#health",
        ] {
            let result = normalize_api_endpoint(endpoint);
            assert_eq!(
                result.map_err(|f| f.kind),
                Err(EndpointErrorKind::Invalid),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn health_contract() {
        assert!(health_result(200, Some(&json!({ "status": "ok" }))).is_ok());
        assert_eq!(
            health_result(503, None).map_err(|f| f.kind),
            Err(EndpointErrorKind::Unhealthy)
        );
        assert_eq!(
            health_result(200, Some(&json!({ "status": "different-service" }))).map_err(|f| f.kind),
            Err(EndpointErrorKind::Unexpected)
        );
        assert_eq!(
            health_result(500, None).map_err(|f| f.kind),
            Err(EndpointErrorKind::Unexpected)
        );
    }

    #[test]
    fn debug_output_omits_credentials() {
        let room = PreparedRoom {
            api_endpoint: "http://x".into(),
            close_key: "close-secret".into(),
            code: "abc-123-def".into(),
            identity: "presenter".into(),
            livekit_url: "ws://x".into(),
            share_url: "http://x/r".into(),
            token: "jwt-secret".into(),
        };
        let printed = format!("{room:?}");
        assert!(!printed.contains("secret"));
    }

    #[tokio::test]
    async fn unreachable_endpoints_are_classified() {
        let api = RoomApi::new();
        let result = api.check_endpoint("http://127.0.0.1:9").await;
        assert!(matches!(
            result.map_err(|f| f.kind),
            Err(EndpointErrorKind::Unreachable | EndpointErrorKind::Timeout)
        ));
    }
}
