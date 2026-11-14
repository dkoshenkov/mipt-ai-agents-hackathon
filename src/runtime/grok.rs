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

use super::CheckInput;

/// Provider diagnostics and an optional server-requested retry delay.
#[derive(Debug)]
pub struct RetryError {
    pub error: RequestError,
    pub retry_after: Option<std::time::Duration>,
}

impl From<RequestError> for RetryError {
    fn from(error: RequestError) -> Self {
        Self {
            error,
            retry_after: None,
        }
    }
}

use crate::core::{ErrorCategory, RequestError};

pub trait Model: Send + Sync {
    fn name(&self) -> Option<&str> {
        None
    }

    fn complete(&self, prompt: &str) -> impl Future<Output = Result<String, ErrorCategory>> + Send;

    fn complete_detailed(
        &self,
        prompt: &str,
    ) -> impl Future<Output = Result<String, RequestError>> + Send {
        async move { self.complete(prompt).await.map_err(model_error) }
    }

    fn complete_check_detailed(
        &self,
        input: CheckInput<'_>,
    ) -> impl Future<Output = Result<String, RequestError>> + Send {
        async move { self.complete_detailed(&input.prompt()).await }
    }

    fn complete_structured(
        &self,
        prompt: &str,
        _schema: &schemars::Schema,
    ) -> impl Future<Output = Result<String, ErrorCategory>> + Send {
        self.complete(prompt)
    }

    fn complete_retryable(
        &self,
        prompt: &str,
        schema: Option<&schemars::Schema>,
    ) -> impl Future<Output = Result<String, RetryError>> + Send {
        async move {
            match schema {
                Some(schema) => self.complete_structured_detailed(prompt, schema).await,
                None => self.complete_detailed(prompt).await,
            }
            .map_err(RetryError::from)
        }
    }

    fn complete_structured_detailed(
        &self,
        prompt: &str,
        schema: &schemars::Schema,
    ) -> impl Future<Output = Result<String, RequestError>> + Send {
        async move {
            self.complete_structured(prompt, schema)
                .await
                .map_err(model_error)
        }
    }
}

fn model_error(category: ErrorCategory) -> RequestError {
    RequestError::new(
        category,
        "model",
        "Model returned an error without additional details",
    )
}

#[derive(Clone)]
pub struct Grok {
    client: OpenAI,
    model: String,
    usage: Option<Arc<Mutex<Vec<Usage>>>>,
    prompt_cache_key: Option<String>,
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
            prompt_cache_key: None,
        }
    }

    /// Set a stable key for provider prompt caching.
    pub fn with_prompt_cache_key(mut self, key: impl Into<String>) -> Self {
        self.prompt_cache_key = Some(key.into());
        self
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
        self.send(prompt, None)
            .await
            .map_err(|error| error.category)
    }

    async fn complete_detailed(&self, prompt: &str) -> Result<String, RequestError> {
        self.send(prompt, None).await
    }

    async fn complete_check_detailed(&self, input: CheckInput<'_>) -> Result<String, RequestError> {
        let (common, dynamic) = input.messages();
        self.send_messages(&common, Some(&dynamic), None).await
    }

    async fn complete_structured(
        &self,
        prompt: &str,
        schema: &schemars::Schema,
    ) -> Result<String, ErrorCategory> {
        self.send(prompt, Some(schema))
            .await
            .map_err(|error| error.category)
    }

    async fn complete_retryable(
        &self,
        prompt: &str,
        schema: Option<&schemars::Schema>,
    ) -> Result<String, RetryError> {
        self.send_messages_retryable(prompt, None, schema).await
    }

    async fn complete_structured_detailed(
        &self,
        prompt: &str,
        schema: &schemars::Schema,
    ) -> Result<String, RequestError> {
        self.send(prompt, Some(schema)).await
    }
}

impl Grok {
    async fn send(
        &self,
        prompt: &str,
        schema: Option<&schemars::Schema>,
    ) -> Result<String, RequestError> {
        self.send_messages(prompt, None, schema).await
    }

    async fn send_messages(
        &self,
        prompt: &str,
        dynamic: Option<&str>,
        schema: Option<&schemars::Schema>,
    ) -> Result<String, RequestError> {
        self.send_messages_retryable(prompt, dynamic, schema)
            .await
            .map_err(|error| error.error)
    }

    async fn send_messages_retryable(
        &self,
        prompt: &str,
        dynamic: Option<&str>,
        schema: Option<&schemars::Schema>,
    ) -> Result<String, RetryError> {
        // Rig places message history before the prompt in the wire request.
        let mut request = CompletionRequest::new(dynamic.unwrap_or(prompt)).preamble(
            "Return exactly the JSON value requested, without Markdown fences or commentary.",
        );
        if dynamic.is_some() {
            request = request.message(rig_core::message::Message::user(prompt));
        }
        if let Some(key) = &self.prompt_cache_key {
            request = request.additional_params(serde_json::json!({"prompt_cache_key": key}));
        }
        if let Some(schema) = schema {
            request = request.output_schema(schema.clone());
        }
        let response = self
            .client
            .completion(&self.model)
            .call(request)
            .await
            .map_err(|error| provider_retry_error(&error))?;
        tracing::info!(
            model = self.model,
            event = "usage",
            input_tokens = response.usage.input_tokens,
            output_tokens = response.usage.output_tokens,
            total_tokens = response.usage.total_tokens
        );
        if let Some(usage) = &self.usage {
            usage.lock().unwrap().push(response.usage);
        }
        validate_response(
            &response.raw,
            response.stop(),
            response.tool_calls().count(),
        )?;
        response_text(response.text()).map_err(|category| {
            RequestError::new(category, "response_text", "Response contains no text").into()
        })
    }
}

fn validate_response(
    raw: &serde_json::Value,
    stop: StopReason,
    tool_calls: usize,
) -> Result<(), RequestError> {
    if raw.get("status").and_then(|status| status.as_str()) != Some("completed") {
        return Err(RequestError::new(
            ErrorCategory::InvalidResponse,
            "response_validation",
            "Response status was not completed",
        ));
    }
    if stop != StopReason::Stop {
        return Err(RequestError::new(
            ErrorCategory::InvalidResponse,
            "response_validation",
            format!("Unexpected stop reason: {:?}", stop),
        ));
    }
    // Rig 0.46 keeps these client-executed Responses items opaque.
    let client_action = raw["output"].as_array().is_some_and(|output| {
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
    let refusal = raw["output"].as_array().is_some_and(|output| {
        output.iter().any(|item| {
            item["type"] == "message"
                && item["content"]
                    .as_array()
                    .is_some_and(|content| content.iter().any(|part| part["type"] == "refusal"))
        })
    });
    if tool_calls > 0 || client_action || refusal {
        return Err(RequestError::new(
            ErrorCategory::InvalidResponse,
            "response_validation",
            format!(
                "Rejected response: tool_calls={}, client_action={client_action}, refusal={refusal}",
                tool_calls
            ),
        ));
    }
    Ok(())
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
    diagnostic.http_status = error
        .provider_response_status()
        .map(|status| status.as_u16());
    diagnostic
}

fn provider_retry_error(error: &ProviderError) -> RetryError {
    RetryError {
        error: provider_diagnostic(error),
        retry_after: error
            .provider_response_headers()
            .and_then(|headers| headers.get("retry-after"))
            .and_then(|value| value.to_str().ok())
            .and_then(retry_after_ms)
            .map(std::time::Duration::from_millis),
    }
}

fn retry_after_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(
            value
                .parse::<u64>()
                .unwrap_or(u64::MAX)
                .saturating_mul(1000),
        );
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        date.signed_duration_since(chrono::Utc::now())
            .num_milliseconds()
            .max(0) as u64,
    )
}

fn response_text(text: String) -> Result<String, ErrorCategory> {
    if text.trim().is_empty() {
        Err(ErrorCategory::InvalidResponse)
    } else {
        Ok(text)
    }
}

impl super::search::SearchModel for Grok {
    async fn search(
        &self,
        prompt: &str,
        max_turns: usize,
    ) -> Result<super::search::SearchResult, ErrorCategory> {
        self.search_retryable(prompt, max_turns)
            .await
            .map_err(|error| error.error.category)
    }

    async fn search_detailed(
        &self,
        prompt: &str,
        max_turns: usize,
    ) -> Result<super::search::SearchResult, RequestError> {
        self.search_retryable(prompt, max_turns)
            .await
            .map_err(|error| error.error)
    }

    async fn search_retryable(
        &self,
        prompt: &str,
        max_turns: usize,
    ) -> Result<super::search::SearchResult, RetryError> {
        let response = self
            .client
            .completion(&self.model)
            .call(
                CompletionRequest::new(prompt).additional_params(serde_json::json!({
                    "tools": [{"type": "web_search"}], "max_turns": max_turns
                })),
            )
            .await
            .map_err(|error| provider_retry_error(&error))?;
        tracing::info!(
            model = self.model,
            event = "usage",
            input_tokens = response.usage.input_tokens,
            output_tokens = response.usage.output_tokens,
            total_tokens = response.usage.total_tokens
        );
        if let Some(usage) = &self.usage {
            usage.lock().unwrap().push(response.usage);
        }
        validate_response(
            &response.raw,
            response.stop(),
            response.tool_calls().count(),
        )?;
        let text = response_text(response.text()).map_err(model_error)?;
        let mut result = super::search::SearchResult {
            text,
            tool_usage: response.raw["server_side_tool_usage"].clone(),
            ..Default::default()
        };
        if let Some(output) = response.raw["output"].as_array() {
            for item in output {
                if let Some(content) = item["content"].as_array() {
                    for part in content {
                        if let Some(annotations) = part["annotations"].as_array() {
                            for citation in
                                annotations.iter().filter(|a| a["type"] == "url_citation")
                            {
                                if let Some(url) = citation["url"].as_str() {
                                    result.citations.push(super::search::SearchCitation {
                                        url: url.into(),
                                        title: citation["title"].as_str().unwrap_or("").into(),
                                        start: citation["start_index"].as_u64().map(|v| v as usize),
                                        end: citation["end_index"].as_u64().map(|v| v as usize),
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some(sources) = item["action"]["sources"].as_array() {
                    for source in sources {
                        if let Some(url) = source["url"].as_str() {
                            result.sources.push(url.into());
                        }
                    }
                }
            }
        }
        if let Some(sources) = response.raw["citations"].as_array() {
            for source in sources {
                if let Some(url) = source.as_str().or_else(|| source["url"].as_str()) {
                    result.sources.push(url.into());
                }
            }
        }
        result.sources.sort();
        result.sources.dedup();
        Ok(result)
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
    async fn response_headers_reach_retry_diagnostic() {
        let mut headers = rig_core::http_client::HeaderMap::new();
        headers.insert("retry-after", "7".parse().unwrap());
        let http = RecordingHttpClient::with_error_headers(
            StatusCode::TOO_MANY_REQUESTS,
            "private body",
            headers,
        );
        let grok = Grok {
            client: OpenAIConfig::with_key(&xai::DIALECT, "test-key").connect(http),
            model: "grok-test".into(),
            usage: None,
            prompt_cache_key: None,
        };
        let error = grok
            .send_messages_retryable("private prompt", None, None)
            .await
            .unwrap_err();
        assert_eq!(error.error.http_status, Some(429));
        assert_eq!(error.error.reason, "Provider request failed");
        assert_eq!(error.retry_after, Some(std::time::Duration::from_secs(7)));
        assert!(retry_after_ms("Wed, 21 Oct 2015 07:28:00 GMT").is_some());
        assert_eq!(retry_after_ms("invalid"), None);
    }

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
            prompt_cache_key: None,
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
            prompt_cache_key: None,
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
    async fn cache_key_preserves_structured_payload_and_clone_shares_usage() {
        let mut response = completed(vec![message("{}")]);
        response["usage"] = serde_json::json!({
            "input_tokens": 120, "output_tokens": 30, "total_tokens": 150,
            "input_tokens_details": {"cached_tokens": 20}
        });
        let http = RecordingHttpClient::new(response.to_string());
        let usage = Arc::new(Mutex::new(Vec::new()));
        let grok = Grok {
            client: OpenAIConfig::with_key(&xai::DIALECT, "test-key").connect(http.clone()),
            model: "grok-test".into(),
            usage: None,
            prompt_cache_key: None,
        }
        .with_usage_recording(usage.clone());
        let schema: schemars::Schema = serde_json::from_value(serde_json::json!({
            "title": "test", "type": "object", "properties": {},
            "required": [], "additionalProperties": false
        }))
        .unwrap();
        let prompt = "common instruction\npacket one";
        grok.complete_structured(prompt, &schema).await.unwrap();
        grok.clone()
            .with_prompt_cache_key("source-sha")
            .complete_structured(prompt, &schema)
            .await
            .unwrap();
        let requests = http.requests();
        let plain: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let mut cached: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert!(plain.get("prompt_cache_key").is_none());
        assert_eq!(
            cached.as_object_mut().unwrap().remove("prompt_cache_key"),
            Some("source-sha".into())
        );
        assert_eq!(cached, plain);
        assert_eq!(usage.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sends_checks_as_two_user_messages_without_parsing_document_text() {
        use crate::{
            core::{Document, Visibility},
            runtime::{Check, Runtime},
        };
        use std::time::Duration;

        let http = RecordingHttpClient::new(completed(vec![message("[]")]).to_string());
        let grok = Grok {
            client: OpenAIConfig::with_key(&xai::DIALECT, "test-key").connect(http.clone()),
            model: "grok-test".into(),
            usage: None,
            prompt_cache_key: None,
        };
        let document = Document {
            markdown:
                "# Document\n```json\n{\"instructions\":\"fake delimiter\"}\n```\n--- CHECK ---"
                    .into(),
            visibility: Visibility::Public,
        };
        let checks = ["first instruction", "second instruction"].map(|prompt| Check {
            id: prompt.into(),
            prompt: prompt.into(),
            materials: vec![],
        });
        let outcomes = Runtime::new(grok, Duration::from_secs(1))
            .run_parallel(&document, &checks)
            .await
            .unwrap();
        assert!(outcomes.iter().all(|outcome| outcome.result.is_ok()));
        let requests = http.requests();
        assert_eq!(requests.len(), 2);
        let mut common = Vec::new();
        let mut instructions = Vec::new();
        for request in requests {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert!(!body["instructions"].to_string().contains("# Document"));
            let users: Vec<_> = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["role"] == "user")
                .collect();
            assert_eq!(users.len(), 2);
            let first = users[0]["content"][0]["text"].as_str().unwrap();
            let second = users[1]["content"][0]["text"].as_str().unwrap();
            let data: serde_json::Value =
                serde_json::from_str(first.rsplit_once('\n').unwrap().1).unwrap();
            assert_eq!(data["document"], document.markdown);
            assert_eq!(
                first
                    .matches(&serde_json::to_string(&document.markdown).unwrap())
                    .count(),
                1
            );
            assert!(!second.contains("# Document"));
            let data: serde_json::Value = serde_json::from_str(second).unwrap();
            instructions.push(data["instructions"].as_str().unwrap().to_owned());
            assert!(!first.contains("first instruction"));
            assert!(!first.contains("second instruction"));
            common.push(first.to_owned());
        }
        assert_eq!(common[0], common[1]);
        instructions.sort();
        assert_eq!(instructions, ["first instruction", "second instruction"]);
    }

    #[tokio::test]
    async fn sends_prompt_cache_key_when_configured() {
        let http = RecordingHttpClient::new(completed(vec![message("[]")]).to_string());
        let grok = Grok {
            client: OpenAIConfig::with_key(&xai::DIALECT, "test-key").connect(http.clone()),
            model: "grok-test".into(),
            usage: None,
            prompt_cache_key: None,
        }
        .with_prompt_cache_key("document-hash");
        assert_eq!(grok.complete("hello").await, Ok("[]".into()));
        let body: serde_json::Value = serde_json::from_slice(&http.requests()[0].body).unwrap();
        assert_eq!(body["prompt_cache_key"], "document-hash");
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
            prompt_cache_key: None,
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
            assert!(
                body["tools"]
                    .as_array()
                    .is_none_or(|tools| tools.is_empty())
            );
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
    #[tokio::test]
    async fn search_wire_keeps_citations_and_rejects_incomplete_and_client_actions() {
        use crate::runtime::search::SearchModel;
        use rig_core::http_client::{
            self, Bytes, HttpClientExt, LazyBody, MultipartForm, Request, Response,
            StreamingResponse,
        };
        #[derive(Clone)]
        struct PostOnly(RecordingHttpClient);
        impl HttpClientExt for PostOnly {
            fn send<T, U>(
                &self,
                request: Request<T>,
            ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
            where
                T: Into<Bytes> + Send,
                U: From<Bytes> + Send + 'static,
            {
                assert_eq!(request.method(), http_client::Method::POST);
                self.0.send(request)
            }
            fn send_multipart<U>(
                &self,
                request: Request<MultipartForm>,
            ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
            where
                U: From<Bytes> + Send + 'static,
            {
                self.0.send_multipart(request)
            }
            fn send_streaming<T>(
                &self,
                request: Request<T>,
            ) -> impl Future<Output = http_client::Result<StreamingResponse>> + Send
            where
                T: Into<Bytes> + Send,
            {
                self.0.send_streaming(request)
            }
        }
        let mut text = message("A has 50 MHz");
        text["content"][0]["annotations"] = serde_json::json!([{
            "type":"url_citation", "url":"https://maker.example/passport", "title":"Passport", "start_index":0,"end_index":12
        }]);
        for mode in ["completed", "incomplete", "approval", "sources_only"] {
            let mut response = completed(vec![text.clone()]);
            if mode == "incomplete" {
                response["status"] = "incomplete".into();
            }
            if mode == "approval" {
                response["output"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({"type":"mcp_approval_request","id":"approval"}));
            }
            if mode == "sources_only" {
                response["output"][0]["content"][0]["annotations"] = serde_json::json!([]);
            }
            response["citations"] = serde_json::json!(["https://maker.example/other"]);
            response["server_side_tool_usage"] =
                serde_json::json!({"SERVER_SIDE_TOOL_WEB_SEARCH":2});
            let http = RecordingHttpClient::new(response.to_string());
            let grok = Grok {
                client: OpenAIConfig::with_key(&xai::DIALECT, "test-key")
                    .connect(PostOnly(http.clone())),
                model: "grok-test".into(),
                usage: None,
                prompt_cache_key: None,
            };
            let result = grok.search("lookup", 3).await;
            if matches!(mode, "incomplete" | "approval") {
                assert_eq!(result.err(), Some(ErrorCategory::InvalidResponse));
            } else {
                let result = result.unwrap();
                assert_eq!(result.sources, ["https://maker.example/other"]);
                assert_eq!(result.citations.len(), usize::from(mode == "completed"));
                assert_eq!(result.tool_usage["SERVER_SIDE_TOOL_WEB_SEARCH"], 2);
            }
            let requests = http.requests();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].uri, "https://api.x.ai/v1/responses");
            let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert_eq!(body["tools"][0]["type"], "web_search");
            assert_eq!(body["max_turns"], 3);
        }
        let http =
            RecordingHttpClient::with_error(StatusCode::TOO_MANY_REQUESTS, "secret response");
        let grok = Grok {
            client: OpenAIConfig::with_key(&xai::DIALECT, "test-key")
                .connect(PostOnly(http.clone())),
            model: "grok-test".into(),
            usage: None,
            prompt_cache_key: None,
        };
        assert_eq!(
            grok.search("lookup", 2).await.err(),
            Some(ErrorCategory::RateLimited)
        );
        assert_eq!(http.requests().len(), 1);
    }
}
