//! A server-side search is one model dispatch, sharing the typed-stage budget.
use std::future::Future;

use serde::{Deserialize, Serialize};

use super::{
    ReviewRun, Runtime, StageKind, StageRequest, StepError, grok::Model, logging::TypedStage,
};
use crate::core::{ErrorCategory, RequestError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchCitation {
    pub url: String,
    pub title: String,
    /// Character offsets in the answer, not byte offsets in a source document.
    pub start: Option<usize>,
    pub end: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchResult {
    pub text: String,
    pub citations: Vec<SearchCitation>,
    /// Unassociated URLs are candidates only.
    pub sources: Vec<String>,
    pub tool_usage: serde_json::Value,
}

pub trait SearchModel: Model {
    fn search(
        &self,
        prompt: &str,
        max_turns: usize,
    ) -> impl Future<Output = Result<SearchResult, ErrorCategory>> + Send;

    fn search_detailed(
        &self,
        prompt: &str,
        max_turns: usize,
    ) -> impl Future<Output = Result<SearchResult, RequestError>> + Send {
        async move {
            self.search(prompt, max_turns).await.map_err(|category| {
                RequestError::new(category, "search", "Search model returned an error")
            })
        }
    }

    fn search_retryable(
        &self,
        prompt: &str,
        max_turns: usize,
    ) -> impl Future<Output = Result<SearchResult, super::grok::RetryError>> + Send {
        async move {
            self.search_detailed(prompt, max_turns)
                .await
                .map_err(Into::into)
        }
    }
}

impl<M: SearchModel> Runtime<M> {
    pub async fn search(
        &self,
        run: &ReviewRun,
        request: &StageRequest<'_>,
        max_turns: usize,
    ) -> Result<SearchResult, StepError> {
        self.search_with_reserve(run, request, max_turns, 0).await
    }

    pub(crate) async fn search_with_reserve(
        &self,
        run: &ReviewRun,
        request: &StageRequest<'_>,
        max_turns: usize,
        reserved: usize,
    ) -> Result<SearchResult, StepError> {
        run.retry_invalid(
            reserved,
            |_| 3,
            |feedback| self.search_once(run, request, max_turns, feedback, reserved),
            |result| result.map_err(|e| (e, e.invalid_feedback())),
        )
        .await
    }

    async fn search_once(
        &self,
        run: &ReviewRun,
        request: &StageRequest<'_>,
        max_turns: usize,
        feedback: Option<&'static str>,
        reserved: usize,
    ) -> Result<SearchResult, StepError> {
        let stage = TypedStage::start(&run.id, StageKind::Search);
        let prompt = match request.prepare(feedback) {
            Ok(prompt) if max_turns > 0 => prompt,
            Ok(_) => {
                stage.finish(Err(StepError::Preparation), None);
                return Err(StepError::Preparation);
            }
            Err(error) => {
                stage.finish(Err(error), None);
                return Err(error);
            }
        };
        if run.remaining() == 0 {
            stage.finish(Err(StepError::BudgetExhausted), None);
            return Err(StepError::BudgetExhausted);
        }
        let mut last_attempt = None;
        let result = run
            .retry_transient(
                reserved,
                feedback.is_some(),
                self.timeout,
                self.model.name(),
                |attempt| {
                    last_attempt = Some(attempt);
                    stage.dispatch(attempt);
                    async {
                        match tokio::time::timeout(
                            self.timeout,
                            self.model.search_retryable(&prompt, max_turns),
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => Err(RequestError::new(
                                ErrorCategory::TimedOut,
                                "timeout",
                                "Search timed out",
                            )
                            .into()),
                        }
                    }
                },
            )
            .await
            .map_err(|error| {
                if error.error.stage == "budget" {
                    StepError::BudgetExhausted
                } else {
                    StepError::Model(error.error.category)
                }
            });
        stage.finish(result.as_ref().map(|_| ()).map_err(|e| *e), last_attempt);
        result
    }
}
