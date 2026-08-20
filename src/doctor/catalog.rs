//! Ollama model catalog trait and HTTP client.

use crate::adapters::ollama::validate_ollama_url;
use crate::error::Error;
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
            .map_err(|error| Error::Ollama(error.to_string()))?;
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
            .map_err(|error| Error::Ollama(error.to_string()))?;
        if !response.status().is_success() {
            return Err(Error::Ollama(format!("status {}", response.status())));
        }
        let body: TagsResponse = response
            .json()
            .await
            .map_err(|error| Error::Ollama(error.to_string()))?;
        Ok(body.models.into_iter().map(|model| model.name).collect())
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
                Err(Error::Ollama(msg)) => Err(Error::Ollama(msg.clone())),
                Err(Error::ModelNotFound { model }) => Err(Error::ModelNotFound {
                    model: model.clone(),
                }),
                Err(Error::Timeout) => Err(Error::Timeout),
                Err(Error::Artifact(msg)) => Err(Error::Artifact(msg.clone())),
                Err(Error::InvalidArtifact { artifact, reason }) => Err(Error::InvalidArtifact {
                    artifact,
                    reason: reason.clone(),
                }),
                Err(Error::Context(msg)) => Err(Error::Context(msg.clone())),
                Err(Error::Git(msg)) => Err(Error::Git(msg.clone())),
                Err(Error::Io(e)) => Err(Error::Io(std::io::Error::new(e.kind(), e.to_string()))),
                Err(Error::Config { path, message }) => Err(Error::Config {
                    path: path.clone(),
                    message: message.clone(),
                }),
                Err(Error::RoleGraph(msg)) => Err(Error::RoleGraph(msg.clone())),
                Err(Error::InvalidOllamaUrl { url }) => {
                    Err(Error::InvalidOllamaUrl { url: url.clone() })
                }
            }
        }
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
        assert!(matches!(err, Error::Ollama(_)));
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
            Error::Ollama(msg) => assert!(msg.contains("503"), "{msg}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_catalog_maps_connection_error() {
        let catalog = HttpModelCatalog::new("http://127.0.0.1:9").expect("catalog");
        let err = catalog.list_models().await.expect_err("connect");
        assert!(matches!(err, Error::Ollama(_)));
    }
}
