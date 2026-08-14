//! Sequential Ollama HTTP client.

use crate::error::Error;
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::Mutex;

/// Default generate timeout.
pub const DEFAULT_GENERATE_TIMEOUT: Duration = Duration::from_secs(600);

/// HTTP client that talks to one Ollama origin, one generate at a time.
pub struct OllamaClient {
    client: reqwest::Client,
    base_url: String,
    generate_lock: Mutex<()>,
}

#[derive(serde::Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
    keep_alive: i32,
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
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .no_proxy()
            .build()
            .map_err(|error| Error::Ollama(error.to_string()))?;
        Ok(Self {
            client,
            base_url: base_url.into(),
            generate_lock: Mutex::new(()),
        })
    }

    /// Complete `prompt` with `model`. Unloads the model after the call (`keep_alive: 0`).
    pub async fn generate(&self, model: &str, prompt: &str) -> Result<String, Error> {
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
        let body: GenerateResponse = response
            .json()
            .await
            .map_err(|error| Error::Ollama(error.to_string()))?;
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
}
