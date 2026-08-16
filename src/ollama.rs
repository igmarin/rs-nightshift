//! Sequential Ollama HTTP client.

use crate::error::Error;
use crate::generate::ROLE_TEMPERATURE;
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::Mutex;

/// Default generate timeout.
pub const DEFAULT_GENERATE_TIMEOUT: Duration = Duration::from_secs(600);

/// Validate an Ollama HTTP(S) origin and return its normalized form.
pub fn validate_ollama_url(value: &str) -> Result<String, Error> {
    let value = value.trim();
    let mut parsed = reqwest::Url::parse(value).map_err(|_| Error::InvalidOllamaUrl {
        url: redact_ollama_url(value),
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !matches!(parsed.path(), "" | "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(Error::InvalidOllamaUrl {
            url: redact_ollama_url(value),
        });
    }
    parsed.set_path("");
    Ok(parsed.to_string().trim_end_matches('/').to_owned())
}

/// Redact userinfo credentials from an Ollama URL before reporting it.
///
/// Both the username and password are stripped so that neither credential
/// form (username-only tokens or username/password pairs) leaks into
/// operator-facing logs or error messages. Only the scheme, host, and port
/// are preserved.
#[must_use]
pub fn redact_ollama_url(value: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(value) else {
        return redact_unparsed_url(value);
    };
    // Clear the entire userinfo so username-only and username-password
    // credentials are both removed before the URL is reported.
    if parsed.set_username("").is_err() || parsed.set_password(None).is_err() {
        return redact_unparsed_url(value);
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string().trim_end_matches('/').to_owned()
}

fn redact_unparsed_url(value: &str) -> String {
    let end = value.find(['?', '#']).unwrap_or(value.len());
    let value = &value[..end];
    let Some(authority_start) = value.find("://").map(|index| index + 3) else {
        return value.to_owned();
    };
    let authority_end = value[authority_start..]
        .find('/')
        .map(|index| authority_start + index)
        .unwrap_or(value.len());
    let authority = &value[authority_start..authority_end];
    let Some(userinfo_end) = authority.rfind('@') else {
        return value.to_owned();
    };
    let userinfo_end = authority_start + userinfo_end;
    format!(
        "{}[REDACTED]@{}",
        &value[..authority_start],
        &value[userinfo_end + 1..]
    )
}

/// HTTP client that talks to one Ollama origin, one generate at a time.
pub struct OllamaClient {
    client: reqwest::Client,
    base_url: String,
    redacted_base_url: String,
    generate_lock: Mutex<()>,
}

#[derive(serde::Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
    keep_alive: i32,
    options: GenerateOptions,
}

#[derive(serde::Serialize)]
struct GenerateOptions {
    temperature: f32,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

impl OllamaClient {
    /// Client for `base_url` with a 10 minute generate timeout.
    pub fn new(base_url: impl Into<String>) -> Result<Self, Error> {
        Self::with_timeout(base_url, DEFAULT_GENERATE_TIMEOUT)
    }

    /// Client with an explicit request timeout (used in tests).
    pub fn with_timeout(base_url: impl Into<String>, timeout: Duration) -> Result<Self, Error> {
        let base_url = validate_ollama_url(&base_url.into())?;
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .no_proxy()
            .build()
            .map_err(|error| Error::Ollama(error.to_string()))?;
        Ok(Self {
            client,
            redacted_base_url: redact_ollama_url(&base_url),
            base_url,
            generate_lock: Mutex::new(()),
        })
    }

    /// Redacted origin used for operator-facing run logs.
    #[must_use]
    pub fn redacted_origin(&self) -> &str {
        &self.redacted_base_url
    }

    /// Complete `prompt` with `model`. Unloads the model after the call (`keep_alive: 0`).
    pub async fn generate(&self, model: &str, prompt: &str) -> Result<String, Error> {
        self.generate_with(model, prompt, ROLE_TEMPERATURE).await
    }

    /// Complete `prompt` with an explicit sampling temperature.
    pub async fn generate_with(
        &self,
        model: &str,
        prompt: &str,
        temperature: f32,
    ) -> Result<String, Error> {
        let _guard = self.generate_lock.lock().await;
        let url = format!("{}/api/generate", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(url)
            .json(&GenerateRequest {
                model,
                prompt,
                stream: false,
                keep_alive: 0,
                options: GenerateOptions { temperature },
            })
            .send()
            .await
            .map_err(map_reqwest)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(Error::ModelNotFound {
                model: model.to_string(),
            });
        }
        if !response.status().is_success() {
            return Err(Error::Ollama(format!("status {}", response.status())));
        }
        let body: GenerateResponse = response.json().await.map_err(map_reqwest)?;
        Ok(body.response)
    }
}

fn map_reqwest(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Ollama(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    struct CountingDelay {
        in_flight: Arc<AtomicU32>,
        max: Arc<AtomicU32>,
    }

    impl Respond for CountingDelay {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(current, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(80));
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            ResponseTemplate::new(200)
                .set_body_raw(r#"{"response":"ok","done":true}"#, "application/json")
        }
    }

    async fn capture_generate_body(server: &MockServer) -> Value {
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1);
        serde_json::from_slice(&requests[0].body).expect("json body")
    }

    #[tokio::test]
    async fn generate_sends_keep_alive_zero_and_no_stream() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(r#"{"response":"hello","done":true}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri()).expect("client");
        let text = client
            .generate("qwen2.5-coder:7b", "say hi")
            .await
            .expect("generate");
        assert_eq!(text, "hello");

        let body = capture_generate_body(&server).await;
        assert_eq!(body["model"], "qwen2.5-coder:7b");
        assert_eq!(body["prompt"], "say hi");
        assert_eq!(body["stream"], false);
        assert_eq!(body["keep_alive"], 0);
        assert_eq!(body["options"]["temperature"], 0.2);
    }

    #[tokio::test]
    async fn generate_with_sends_requested_temperature() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(r#"{"response":"ok","done":true}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri()).expect("client");
        client
            .generate_with("gemma2:9b", "draft", 0.5)
            .await
            .expect("generate");
        let body = capture_generate_body(&server).await;
        assert_eq!(body["options"]["temperature"], 0.5);
        assert_eq!(body["keep_alive"], 0);
    }

    #[tokio::test]
    async fn generate_maps_404_to_model_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_raw(r#"{"error":"model 'nope' not found"}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri()).expect("client");
        let err = client
            .generate("nope", "x")
            .await
            .expect_err("missing model");
        match err {
            Error::ModelNotFound { model } => assert_eq!(model, "nope"),
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn generate_maps_non_404_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri()).expect("client");
        let err = client.generate("m", "p").await.expect_err("status");
        match err {
            Error::Ollama(msg) => assert!(msg.contains("500"), "{msg}"),
            other => panic!("expected Ollama status error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn generate_maps_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(200))
                    .set_body_raw(r#"{"response":"late","done":true}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let client =
            OllamaClient::with_timeout(server.uri(), Duration::from_millis(40)).expect("client");
        let err = client.generate("m", "p").await.expect_err("timeout");
        assert!(
            matches!(err, Error::Timeout),
            "expected Timeout, got {err:?}"
        );
    }

    #[tokio::test]
    async fn generate_is_serialized() {
        let server = MockServer::start().await;
        let in_flight = Arc::new(AtomicU32::new(0));
        let max = Arc::new(AtomicU32::new(0));
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(CountingDelay {
                in_flight: Arc::clone(&in_flight),
                max: Arc::clone(&max),
            })
            .expect(2)
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri()).expect("client");
        let first = client.generate("m", "one");
        let second = client.generate("m", "two");
        let (a, b) = tokio::join!(first, second);
        a.expect("first");
        b.expect("second");
        assert_eq!(
            max.load(Ordering::SeqCst),
            1,
            "two generates must not overlap"
        );
    }

    #[test]
    fn rejects_invalid_ollama_url() {
        let error = match OllamaClient::new("not a URL") {
            Ok(_) => panic!("invalid URL must fail"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(text.contains("not a URL"), "{text}");
        assert!(text.contains("http://"), "{text}");
    }

    #[test]
    fn rejects_non_origin_ollama_urls() {
        for value in [
            "http://example.test/path",
            "http://example.test?token=secret",
            "http://example.test#fragment",
        ] {
            assert!(
                validate_ollama_url(value).is_err(),
                "expected origin rejection for {value}"
            );
        }
    }

    #[test]
    fn normalizes_trailing_slash() {
        assert_eq!(
            validate_ollama_url("http://example.test:11434/").expect("valid origin"),
            "http://example.test:11434"
        );
    }

    #[test]
    fn redacts_ollama_userinfo_username_and_password() {
        let redacted = redact_ollama_url("http://user:secret@example.test:11434");
        assert!(!redacted.contains("user"), "{redacted}");
        assert!(!redacted.contains("secret"), "{redacted}");
        assert!(!redacted.contains('@'), "{redacted}");
        assert_eq!(redacted, "http://example.test:11434");
    }

    #[test]
    fn redacts_ollama_userinfo_username_only() {
        let redacted = redact_ollama_url("http://api-token@example.test:11434");
        assert!(!redacted.contains("api-token"), "{redacted}");
        assert!(!redacted.contains('@'), "{redacted}");
        assert_eq!(redacted, "http://example.test:11434");
    }

    #[test]
    fn redacts_ollama_userinfo_preserves_scheme_host_port() {
        let redacted = redact_ollama_url("https://token:pass@host.local:8080");
        assert_eq!(redacted, "https://host.local:8080");
    }

    #[test]
    fn redacts_ollama_userinfo_no_credentials() {
        assert_eq!(
            redact_ollama_url("http://example.test:11434"),
            "http://example.test:11434"
        );
    }

    #[test]
    fn redacts_malformed_credentials() {
        let error = match OllamaClient::new("http://user:secret@[invalid") {
            Ok(_) => panic!("invalid URL must fail"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(!text.contains("secret"), "{text}");
        assert!(text.contains("[REDACTED]"), "{text}");
    }
}
