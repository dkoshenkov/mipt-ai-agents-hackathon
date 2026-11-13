use std::{
    future::Future,
    sync::{Arc, Mutex},
};

use rig_core::{
    completion::{CompletionRequest, Usage},
    error::ProviderError,
    message::StopReason,
    providers::{openai::OpenAI, xai},
};

use crate::core::ErrorCategory;

pub trait Model: Send + Sync {
    fn name(&self) -> Option<&str> {
        None
    }

    fn complete(&self, prompt: &str) -> impl Future<Output = Result<String, ErrorCategory>> + Send;

    fn complete_structured(
        &self,
        prompt: &str,
        _schema: &schemars::Schema,
    ) -> impl Future<Output = Result<String, ErrorCategory>> + Send {
        self.complete(prompt)
    }
}

pub struct Grok {
    client: OpenAI,
    model: String,
    usage: Option<Arc<Mutex<Vec<Usage>>>>,
}

impl Grok {
    /// Reads XAI_API_KEY from the environment or `.env` in the working directory.
    pub fn from_env(model: String) -> Result<Self, &'static str> {
        let key = match std::env::var("XAI_API_KEY") {
            Ok(key) => Some(key),
            Err(std::env::VarError::NotPresent) => std::fs::read_to_string(".env")
                .ok()
                .and_then(|content| dotenv_key(&content)),
            Err(std::env::VarError::NotUnicode(_)) => None,
        };
        let key = key
            .filter(|key| !key.trim().is_empty())
            .ok_or("Set a non-empty XAI_API_KEY in the environment or .env")?;
        Ok(Self::new(key, model))
    }

    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: xai::new(api_key),
            model,
            usage: None,
        }
    }

    /// Record usage from received responses when explicitly requested.
    pub fn with_usage_recording(mut self, usage: Arc<Mutex<Vec<Usage>>>) -> Self {
        self.usage = Some(usage);
        self
    }
}

fn dotenv_key(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "XAI_API_KEY").then(|| value.trim().to_owned())
    })
}

impl Model for Grok {
    fn name(&self) -> Option<&str> {
        Some(&self.model)
    }

    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        self.send(prompt, None).await
    }

    async fn complete_structured(
        &self,
        prompt: &str,
        schema: &schemars::Schema,
    ) -> Result<String, ErrorCategory> {
        self.send(prompt, Some(schema)).await
    }
}

impl Grok {
    async fn send(
        &self,
        prompt: &str,
        schema: Option<&schemars::Schema>,
    ) -> Result<String, ErrorCategory> {
        let mut request = CompletionRequest::new(prompt).preamble(
            "Return exactly the JSON value requested, without Markdown fences or commentary.",
        );
        if let Some(schema) = schema {
            request = request.output_schema(schema.clone());
        }
        let response = self
            .client
            .completion(&self.model)
            .call(request)
            .await
            .map_err(|error| classify_error(&error))?;
        if let Some(usage) = &self.usage {
            usage.lock().unwrap().push(response.usage);
        }
        if response
            .raw
            .get("status")
            .and_then(|status| status.as_str())
            != Some("completed")
        {
            return Err(ErrorCategory::InvalidResponse);
        }
        if response.stop() != StopReason::Stop {
            return Err(ErrorCategory::InvalidResponse);
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
            return Err(ErrorCategory::InvalidResponse);
        }
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

    #[test]
    fn reads_only_the_xai_key_from_dotenv() {
        assert_eq!(
            dotenv_key("OTHER=ignored\nXAI_API_KEY= test-key\n"),
            Some("test-key".into())
        );
        assert_eq!(dotenv_key("OTHER=ignored"), None);
    }

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
            usage: None,
        };
        grok.complete("hello").await
    }

    #[tokio::test]
    async fn sends_structured_schema_in_responses_wire_format() {
        let http = RecordingHttpClient::new(completed(vec![message("{}")]).to_string());
        let grok = Grok {
            client: OpenAIConfig::with_key(&xai::DIALECT, "test-key").connect(http.clone()),
            model: "grok-test".into(),
            usage: None,
        };
        let schema: schemars::Schema = serde_json::from_value(serde_json::json!({
            "title": "test", "type": "object", "properties": {},
            "required": [], "additionalProperties": false
        }))
        .unwrap();
        assert_eq!(
            grok.complete_structured("hello", &schema).await,
            Ok("{}".into())
        );
        let requests = http.requests();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["strict"], true);
        assert_eq!(
            body["text"]["format"]["schema"]["additionalProperties"],
            false
        );
    }

    #[tokio::test]
    async fn records_usage_even_when_response_is_rejected() {
        let mut response = completed(vec![message("answer")]);
        response["status"] = "incomplete".into();
        response["usage"] = serde_json::json!({
            "input_tokens": 120, "output_tokens": 30, "total_tokens": 150,
            "input_tokens_details": {"cached_tokens": 20}
        });
        let http = RecordingHttpClient::new(response.to_string());
        let usage = Arc::new(Mutex::new(Vec::new()));
        let grok = Grok {
            client: OpenAIConfig::with_key(&xai::DIALECT, "test-key").connect(http),
            model: "grok-test".into(),
            usage: None,
        }
        .with_usage_recording(usage.clone());
        assert_eq!(
            grok.complete("hello").await,
            Err(ErrorCategory::InvalidResponse)
        );
        let recorded = usage.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].input_tokens, Some(120));
        assert_eq!(recorded[0].output_tokens, Some(30));
        assert_eq!(recorded[0].cached_input_tokens, Some(20));
        assert!(recorded[0].cost.is_none());
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
            assert!(body.to_string().contains("without Markdown fences"));
            let user = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["role"] == "user")
                .unwrap();
            assert_eq!(user["content"][0]["text"], "hello");
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
