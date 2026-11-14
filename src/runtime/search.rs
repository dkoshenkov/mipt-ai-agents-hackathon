//! A server-side search is one model dispatch, sharing the typed-stage budget.
use std::future::Future;

use serde::{Deserialize, Serialize};

use super::{
    ReviewRun, Runtime, StageKind, StageRequest, StepError, grok::Model, logging::TypedStage,
};
use crate::core::ErrorCategory;

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
}

impl<M: SearchModel> Runtime<M> {
    pub async fn search(
        &self,
        run: &ReviewRun,
        request: &StageRequest<'_>,
        max_turns: usize,
    ) -> Result<SearchResult, StepError> {
        let stage = TypedStage::start(&run.id, StageKind::Search);
        let prompt = match request.prepare() {
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
        let Some(attempt) = run.reserve() else {
            stage.finish(Err(StepError::BudgetExhausted), None);
            return Err(StepError::BudgetExhausted);
        };
        stage.dispatch(attempt);
        let result =
            match tokio::time::timeout(self.timeout, self.model.search(&prompt, max_turns)).await {
                Ok(result) => result.map_err(StepError::Model),
                Err(_) => Err(StepError::Model(ErrorCategory::TimedOut)),
            };
        stage.finish(result.as_ref().map(|_| ()).map_err(|e| *e), Some(attempt));
        result
    }
}
