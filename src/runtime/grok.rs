use std::future::Future;

use rig_core::{
    completion::CompletionRequest,
    error::ProviderError,
    providers::{openai::OpenAI, xai},
};

use crate::core::ErrorCategory;

pub trait Model: Send + Sync {
    fn name(&self) -> Option<&str> {
        None
    }

    fn complete(&self, prompt: &str) -> impl Future<Output = Result<String, ErrorCategory>> + Send;
}

pub struct Grok {
    client: OpenAI,
    model: String,
}

impl Grok {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: xai::new(api_key),
            model,
        }
    }
}

impl Model for Grok {
    fn name(&self) -> Option<&str> {
        Some(&self.model)
    }

    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let response = self
            .client
            .completion(&self.model)
            .call(CompletionRequest::new(prompt))
            .await
            .map_err(|error| classify_error(&error))?;
        response_text(response.text())
    }
}

fn response_text(text: String) -> Result<String, ErrorCategory> {
    if text.trim().is_empty() {
        Err(ErrorCategory::InvalidResponse)
    } else {
        Ok(text)
    }
}

fn classify_error(error: &ProviderError) -> ErrorCategory {
    if error
        .provider_response_status()
        .is_some_and(|status| status.as_u16() == 429)
    {
        return ErrorCategory::RateLimited;
    }
    match error {
        ProviderError::Response(_) | ProviderError::Json(_) => ErrorCategory::InvalidResponse,
        _ => ErrorCategory::Provider,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::{
        http_client::StatusCode, providers::openai::OpenAIConfig, test_utils::RecordingHttpClient,
    };

    #[tokio::test]
    async fn sends_one_xai_responses_request_and_handles_success_and_rate_limit() {
        let success = RecordingHttpClient::new(
            r#"{
            "id":"resp_1", "object":"response", "created_at":1700000000,
            "status":"completed", "model":"grok-test", "error":null,
            "incomplete_details":null, "instructions":null, "max_output_tokens":null,
            "output":[{"type":"message", "id":"msg_1", "role":"assistant",
                "status":"completed", "content":[{"type":"output_text", "text":"answer", "annotations":[]}]}],
            "tools":[]
        }"#,
        );
        let limited = RecordingHttpClient::with_error(StatusCode::TOO_MANY_REQUESTS, "secret body");

        for (http, expected) in [
            (success, Ok("answer".to_owned())),
            (limited, Err(ErrorCategory::RateLimited)),
        ] {
            let grok = Grok {
                client: OpenAIConfig::with_key(&xai::DIALECT, "test-key").connect(http.clone()),
                model: "grok-test".into(),
            };
            assert_eq!(grok.complete("hello").await, expected);
            let requests = http.requests();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].uri, "https://api.x.ai/v1/responses");
            assert_eq!(requests[0].headers["authorization"], "Bearer test-key");
            let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert_eq!(body["model"], "grok-test");
            assert_eq!(body["input"][0]["role"], "user");
            assert_eq!(body["input"][0]["content"][0]["text"], "hello");
        }
    }

    #[test]
    fn checks_response_and_classifies_errors_without_exposing_bodies() {
        assert_eq!(response_text("answer".into()), Ok("answer".into()));
        assert_eq!(
            response_text("  ".into()),
            Err(ErrorCategory::InvalidResponse)
        );
        let error = ProviderError::from_http_response("429".parse().unwrap(), "secret body");
        assert_eq!(classify_error(&error), ErrorCategory::RateLimited);
        assert_eq!(
            classify_error(&ProviderError::Response("secret body".into())),
            ErrorCategory::InvalidResponse
        );
    }
}
