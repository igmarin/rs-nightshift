//! Model catalogs for doctor: Ollama `/api/tags` and OpenAI-compatible `/models`.

use crate::adapters::ollama::validate_ollama_url;
#[cfg(test)]
use crate::error::{ArtifactError, ConfigError, GitError};
use crate::error::{Error, ProviderError};
use async_trait::async_trait;

/// Lists models known to an Ollama server.
#[async_trait]
pub trait ModelCatalog: Send + Sync {
    /// Installed model tags (for example `llama3.1:8b`).
    async fn list_models(&self) -> Result<Vec<String>, Error>;
}

/// Ollama `/api/tags` client.
pub struct HttpModelCatalog {
    client: reqwest::Client,
    base_url: String,
}

impl HttpModelCatalog {
    /// Build a catalog for `base_url` (no trailing path).
    pub fn new(base_url: impl Into<String>) -> Result<Self, Error> {
        let base_url = validate_ollama_url(&base_url.into())?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .no_proxy()
            .build()
            .map_err(|error| Error::from(ProviderError::Ollama(error.to_string())))?;
        Ok(Self { client, base_url })
    }
}

#[derive(serde::Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(serde::Deserialize)]
struct TagModel {
    name: String,
}

#[async_trait]
impl ModelCatalog for HttpModelCatalog {
    async fn list_models(&self) -> Result<Vec<String>, Error> {
        let url = format!("{}/api/tags", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| Error::from(ProviderError::Ollama(error.to_string())))?;
        if !response.status().is_success() {
            return Err(Error::from(ProviderError::Ollama(format!(
                "status {}",
                response.status()
            ))));
        }
        let body: TagsResponse = response
            .json()
            .await
            .map_err(|error| Error::from(ProviderError::Ollama(error.to_string())))?;
        Ok(body.models.into_iter().map(|model| model.name).collect())
    }
}

/// Lists models served by an OpenAI-compatible server (rapid-mlx, llama.cpp,
/// LM Studio, vLLM, hosted APIs) at a given base URL.
#[async_trait]
pub trait ProviderCatalog: Send + Sync {
    /// Model ids from `GET {base_url}/models`; `api_key` is sent as a bearer
    /// token when present.
    async fn list_models(
        &self,
        base_url: &str,
        api_key: Option<&str>,
    ) -> Result<Vec<String>, Error>;
}

/// HTTP [`ProviderCatalog`] with a short timeout, so doctor never hangs.
pub struct HttpProviderCatalog {
    client: reqwest::Client,
}

impl HttpProviderCatalog {
    /// Build the catalog client.
    pub fn new() -> Result<Self, Error> {
        // No redirects, as in the chat client: the key must reach only base_url.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| Error::from(ProviderError::Ollama(error.to_string())))?;
        Ok(Self { client })
    }
}

#[derive(serde::Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(serde::Deserialize)]
struct ModelEntry {
    id: String,
}

#[async_trait]
impl ProviderCatalog for HttpProviderCatalog {
    async fn list_models(
        &self,
        base_url: &str,
        api_key: Option<&str>,
    ) -> Result<Vec<String>, Error> {
        let base_url = crate::adapters::providers::validate_chat_base_url(base_url)?;
        let mut request = self.client.get(format!("{base_url}/models"));
        if let Some(key) = api_key {
            request = request.bearer_auth(key);
        }
        let http = |error: reqwest::Error| {
            // without_url: never echo a URL (or anything in it) into the report.
            Error::from(ProviderError::Ollama(error.without_url().to_string()))
        };
        let response = request.send().await.map_err(http)?;
        if !response.status().is_success() {
            return Err(Error::from(ProviderError::Ollama(format!(
                "status {}",
                response.status()
            ))));
        }
        let body: ModelsResponse = response.json().await.map_err(http)?;
        Ok(body.data.into_iter().map(|model| model.id).collect())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct FakeCatalog {
        pub result: Result<Vec<String>, Error>,
    }

    #[async_trait]
    impl ModelCatalog for FakeCatalog {
        async fn list_models(&self) -> Result<Vec<String>, Error> {
            match &self.result {
                Ok(models) => Ok(models.clone()),
                Err(Error::Provider(ProviderError::Ollama(msg))) => {
                    Err(Error::from(ProviderError::Ollama(msg.clone())))
                }
                Err(Error::Provider(ProviderError::ModelNotFound { model })) => {
                    Err(Error::from(ProviderError::ModelNotFound {
                        model: model.clone(),
                    }))
                }
                Err(Error::Provider(ProviderError::Timeout)) => {
                    Err(Error::from(ProviderError::Timeout))
                }
                Err(Error::Provider(ProviderError::CutOff { text })) => {
                    Err(Error::from(ProviderError::CutOff { text: text.clone() }))
                }
                Err(Error::Artifact(ArtifactError::Artifact(msg))) => {
                    Err(Error::from(ArtifactError::artifact(msg.clone())))
                }
                Err(Error::Artifact(ArtifactError::InvalidArtifact { artifact, reason })) => {
                    Err(Error::from(ArtifactError::InvalidArtifact {
                        artifact,
                        reason: reason.clone(),
                    }))
                }
                Err(Error::Context(msg)) => Err(Error::Context(msg.clone())),
                Err(Error::Git(GitError(msg))) => Err(Error::from(GitError::new(msg.clone()))),
                Err(Error::Io(e)) => Err(Error::Io(std::io::Error::new(e.kind(), e.to_string()))),
                Err(Error::Config(ConfigError { path, message })) => {
                    Err(Error::from(ConfigError {
                        path: path.clone(),
                        message: message.clone(),
                    }))
                }
                Err(Error::RoleGraph(msg)) => Err(Error::RoleGraph(msg.clone())),
                Err(Error::Provider(ProviderError::InvalidOllamaUrl { url })) => {
                    Err(Error::from(ProviderError::InvalidOllamaUrl {
                        url: url.clone(),
                    }))
                }
            }
        }
    }

    /// The same canned result serves the OpenAI-compatible catalog.
    #[async_trait]
    impl ProviderCatalog for FakeCatalog {
        async fn list_models(
            &self,
            _base_url: &str,
            _api_key: Option<&str>,
        ) -> Result<Vec<String>, Error> {
            ModelCatalog::list_models(self).await
        }
    }

    #[tokio::test]
    async fn http_provider_catalog_lists_ids_with_bearer() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v1/models"))
            .and(wiremock::matchers::header("Authorization", "Bearer sk-x"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_raw(
                r#"{"object":"list","data":[{"id":"qwen3.6-35b-4bit","object":"model"}]}"#,
                "application/json",
            ))
            .mount(&server)
            .await;

        let catalog = HttpProviderCatalog::new().expect("catalog");
        let models =
            ProviderCatalog::list_models(&catalog, &format!("{}/v1/", server.uri()), Some("sk-x"))
                .await
                .expect("models");
        assert_eq!(models, vec!["qwen3.6-35b-4bit".to_string()]);
    }

    #[tokio::test]
    async fn http_provider_catalog_sends_no_auth_without_key() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v1/models"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_raw(r#"{"data":[]}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let catalog = HttpProviderCatalog::new().expect("catalog");
        ProviderCatalog::list_models(&catalog, &format!("{}/v1", server.uri()), None)
            .await
            .expect("models");
        let requests = server.received_requests().await.expect("requests");
        assert!(requests[0].headers.get("authorization").is_none());
    }

    #[tokio::test]
    async fn http_provider_catalog_rejects_query_in_base_url() {
        let catalog = HttpProviderCatalog::new().expect("catalog");
        let err = ProviderCatalog::list_models(&catalog, "http://127.0.0.1:8000/v1?tenant=x", None)
            .await
            .expect_err("query must be rejected before any request");
        assert!(err.to_string().contains("query string"), "{err}");
    }

    #[tokio::test]
    async fn http_provider_catalog_maps_error_status() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let catalog = HttpProviderCatalog::new().expect("catalog");
        let err = ProviderCatalog::list_models(&catalog, &server.uri(), None)
            .await
            .expect_err("503 must fail");
        assert!(err.to_string().contains("503"), "{err}");
    }

    #[tokio::test]
    async fn http_catalog_lists_tags() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/tags"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_raw(r#"{"models":[{"name":"llama3.1:8b"}]}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let catalog = HttpModelCatalog::new(server.uri()).expect("catalog");
        let models = catalog.list_models().await.expect("tags");
        assert_eq!(models, vec!["llama3.1:8b".to_string()]);
    }

    #[tokio::test]
    async fn http_catalog_maps_invalid_json() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/tags"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw("not-json", "application/json"),
            )
            .mount(&server)
            .await;

        let catalog = HttpModelCatalog::new(server.uri()).expect("catalog");
        let err = catalog.list_models().await.expect_err("json");
        assert!(matches!(err, Error::Provider(ProviderError::Ollama(_))));
    }

    #[tokio::test]
    async fn http_catalog_maps_http_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/tags"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let catalog = HttpModelCatalog::new(server.uri()).expect("catalog");
        let err = catalog.list_models().await.expect_err("status");
        match err {
            Error::Provider(ProviderError::Ollama(msg)) => assert!(msg.contains("503"), "{msg}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_catalog_maps_connection_error() {
        let catalog = HttpModelCatalog::new("http://127.0.0.1:9").expect("catalog");
        let err = catalog.list_models().await.expect_err("connect");
        assert!(matches!(err, Error::Provider(ProviderError::Ollama(_))));
    }
}
