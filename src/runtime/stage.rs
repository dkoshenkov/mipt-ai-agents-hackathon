use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::core::{ErrorCategory, Visibility};

use super::{ReviewRun, Runtime, grok::Model};

/// A controlled label for a model stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    Claims,
    Review,
}

/// An application-defined instruction for a typed model stage.
pub struct StageRequest<'a> {
    pub kind: StageKind,
    pub instruction: &'static str,
    fields: Vec<StageField<'a>>,
}

struct StageField<'a> {
    name: &'static str,
    sources: &'a [Visibility],
    serialize: Box<dyn Fn() -> Result<Value, serde_json::Error> + Send + Sync + 'a>,
}

impl<'a> StageRequest<'a> {
    pub fn new(kind: StageKind, instruction: &'static str) -> Self {
        Self {
            kind,
            instruction,
            fields: Vec::new(),
        }
    }

    /// Add data and every source from which it was derived.
    pub fn field<T: Serialize + Sync + ?Sized>(
        mut self,
        name: &'static str,
        sources: &'a [Visibility],
        value: &'a T,
    ) -> Self {
        self.fields.push(StageField {
            name,
            sources,
            serialize: Box::new(move || serde_json::to_value(value)),
        });
        self
    }

    fn prepare(&self) -> Result<String, StepError> {
        if self.fields.iter().any(|field| {
            field.sources.is_empty()
                || field
                    .sources
                    .iter()
                    .any(|source| *source != Visibility::Public)
        }) {
            return Err(StepError::RestrictedContext);
        }

        let mut fields = Vec::new();
        for field in &self.fields {
            let value = (field.serialize)().map_err(|_| StepError::Preparation)?;
            fields.push(json!({"name": field.name, "value": value}));
        }
        Ok(json!({"instruction": self.instruction, "data": fields}).to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepError {
    Preparation,
    RestrictedContext,
    BudgetExhausted,
    Model(ErrorCategory),
}

impl<M: Model> Runtime<M> {
    /// Send one typed request after validating its data sources and reserving a run attempt.
    /// A decoded result is still subject to the calling scenario's domain checks.
    pub async fn step<T: DeserializeOwned>(
        &self,
        run: &ReviewRun,
        request: &StageRequest<'_>,
    ) -> Result<T, StepError> {
        let stage = super::logging::TypedStage::start(&run.id, request.kind);
        let prompt = match request.prepare() {
            Ok(prompt) => prompt,
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
        let result = self.request_json(&prompt).await.map_err(StepError::Model);
        stage.finish(
            result.as_ref().map(|_| ()).map_err(|error| *error),
            Some(attempt),
        );
        result
    }
}
