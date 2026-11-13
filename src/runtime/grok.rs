use std::future::Future;

use rig_core::{
    completion::CompletionRequest,
    error::ProviderError,
    message::StopReason,
    providers::{openai::OpenAI, xai},
};

use crate::core::{ErrorCategory, RequestError};

pub trait Model: Send + Sync {
    fn name(&self) -> Option<&str> {
        None
    }

    fn complete(&self, prompt: &str) -> impl Future<Output = Result<String, ErrorCategory>> + Send;

    fn complete_detailed(&self, prompt: &str) -> impl Future<Output = Result<String, RequestError>> + Send {
        async move { self.complete(prompt).await.map_err(|category| RequestError::new(category, "model", "Model returned an error without additional details")) }
    }

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
        self.complete_detailed(prompt).await.map_err(|error| error.category)
    }

    async fn complete_detailed(&self, prompt: &str) -> Result<String, RequestError> {
        let response = self
            .client
            .completion(&self.model)
            .call(CompletionRequest::new(prompt))
            .await
            .map_err(|error| provider_diagnostic(&error))?;
        if response
            .raw
            .get("status")
            .and_then(|status| status.as_str())
            != Some("completed")
        {
            return Err(RequestError::new(ErrorCategory::InvalidResponse, "response_validation", format!("Unexpected response status: {:?}", response.raw["status"].as_str())));
        }
        if response.stop() != StopReason::Stop {
            return Err(RequestError::new(ErrorCategory::InvalidResponse, "response_validation", format!("Unexpected stop reason: {:?}", response.stop())));
        }
        // Rig 0.46 keeps these client-executed Responses items opaque.
        let client_action = response.raw["output"].as_array().is_some_and(|output| {
            output.iter().any(|item| {
                matches!(
                    item["type"].as_str(),
                    Some(
                        "computer_call"
                            | "local_shell_call"
                            | "shell_call"
                            | "apply_patch_call"
                            | "mcp_approval_request"
                    )
                )
            })
        });
        let refusal = response.raw["output"].as_array().is_some_and(|output| {
            output.iter().any(|item| {
                item["type"] == "message"
                    && item["content"]
                        .as_array()
                        .is_some_and(|content| content.iter().any(|part| part["type"] == "refusal"))
            })
        });
        if response.tool_calls().next().is_some() || client_action || refusal {
            return Err(RequestError::new(ErrorCategory::InvalidResponse, "response_validation", format!("Rejected response: tool_calls={}, client_action={client_action}, refusal={refusal}", response.tool_calls().count())));
        }
        response_text(response.text()).map_err(|category| RequestError::new(category, "response_text", "Response contains no text"))
    }
}

fn provider_diagnostic(error: &ProviderError) -> RequestError {
    let reason = match error {
        ProviderError::Http(_) => "Transport failure",
        ProviderError::Json(_) => "Provider envelope JSON could not be decoded",
        ProviderError::Response(_) => "Provider response does not match the SDK response contract",
        ProviderError::InvalidAuthentication(_) => "Authentication rejected",
        _ => "Provider request failed",
    };
    let mut diagnostic = RequestError::new(classify_error(error), "provider", reason);
    diagnostic.http_status = error.provider_response_status().map(|status| status.as_u16());
    diagnostic
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

    fn message(text: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}]
        })
    }

    fn completed(output: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "id": "resp_1", "object": "response", "created_at": 1700000000,
            "status": "completed", "model": "grok-test", "error": null,
            "incomplete_details": null, "instructions": null, "max_output_tokens": null,
            "output": output, "tools": []
        })
    }

    async fn send(http: &RecordingHttpClient) -> Result<String, ErrorCategory> {
        let grok = Grok {
            client: OpenAIConfig::with_key(&xai::DIALECT, "test-key").connect(http.clone()),
            model: "grok-test".into(),
        };
        grok.complete("hello").await
    }

    #[tokio::test]
    async fn rejects_opaque_client_actions_but_accepts_served_items() {
        for (kind, expected) in [
            ("computer_call", Err(ErrorCategory::InvalidResponse)),
            ("local_shell_call", Err(ErrorCategory::InvalidResponse)),
            ("shell_call", Err(ErrorCategory::InvalidResponse)),
            ("apply_patch_call", Err(ErrorCategory::InvalidResponse)),
            ("mcp_approval_request", Err(ErrorCategory::InvalidResponse)),
            ("reasoning", Ok("[]".to_owned())),
            ("mcp_call", Ok("[]".to_owned())),
        ] {
            let response = completed(vec![
                message("[]"),
                serde_json::json!({
                    "type": kind, "id": "item_1", "summary": []
                }),
            ]);
            let http = RecordingHttpClient::new(response.to_string());
            assert_eq!(send(&http).await, expected, "{kind}");
            assert_eq!(http.requests().len(), 1, "{kind}");
        }
    }

    #[tokio::test]
    async fn rejects_tool_calls_with_or_without_text() {
        let tool = serde_json::json!({
            "type": "function_call", "id": "fc_1", "call_id": "call_1",
            "name": "lookup", "arguments": "{}", "status": "completed"
        });
        for output in [vec![tool.clone()], vec![message("[]"), tool]] {
            let http = RecordingHttpClient::new(completed(output).to_string());
            assert_eq!(send(&http).await, Err(ErrorCategory::InvalidResponse));
            assert_eq!(http.requests().len(), 1);
        }
    }

    #[tokio::test]
    async fn rejects_refusal_even_when_json_is_valid() {
        let mut text = message("[]");
        text["content"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "type": "refusal", "refusal": "Cannot comply"
            }));
        let http = RecordingHttpClient::new(completed(vec![text]).to_string());
        assert_eq!(send(&http).await, Err(ErrorCategory::InvalidResponse));
        assert_eq!(http.requests().len(), 1);
    }

    #[tokio::test]
    async fn rejects_incomplete_response_even_when_json_is_valid() {
        let mut response = completed(vec![message("[]")]);
        for (status, details, expected) in [
            ("completed", serde_json::Value::Null, Ok("[]".to_owned())),
            (
                "incomplete",
                serde_json::json!({"reason": "max_output_tokens"}),
                Err(ErrorCategory::InvalidResponse),
            ),
        ] {
            response["status"] = status.into();
            response["incomplete_details"] = details;
            let http = RecordingHttpClient::new(response.to_string());
            assert_eq!(send(&http).await, expected);
        }
    }

    #[tokio::test]
    async fn sends_one_xai_responses_request_and_handles_success_and_rate_limit() {
        let success = RecordingHttpClient::new(completed(vec![message("answer")]).to_string());
        let limited = RecordingHttpClient::with_error(StatusCode::TOO_MANY_REQUESTS, "secret body");

        for (http, expected) in [
            (success, Ok("answer".to_owned())),
            (limited, Err(ErrorCategory::RateLimited)),
        ] {
            assert_eq!(send(&http).await, expected);
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
