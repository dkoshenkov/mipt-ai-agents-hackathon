use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::core::{ErrorCategory, Visibility};

use super::{ReviewRun, Runtime, grok::Model};

/// A controlled label for a model stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    Claims,
    Review,
    Control,
    Comparison,
    Search,
}

/// An application-defined instruction for a typed model stage.
pub struct StageRequest<'a> {
    pub kind: StageKind,
    pub instruction: &'static str,
    fields: Vec<StageField<'a>>,
    schema: Option<schemars::Schema>,
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
            schema: None,
        }
    }

    pub fn schema(mut self, schema: schemars::Schema) -> Self {
        self.schema = Some(schema);
        self
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

    pub(super) fn prepare(&self, feedback: Option<&'static str>) -> Result<String, StepError> {
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
        if let Some(reason) = feedback {
            fields.push(json!({"name":"retry_feedback","value":reason}));
        }
        // Document-wide checks share the numbered source across different instructions.
        // Packet-based checks share the instruction across different source fragments.
        #[derive(Serialize)]
        #[serde(untagged)]
        enum Prompt<'a> {
            DocumentFirst {
                data: Vec<Value>,
                instruction: &'a str,
            },
            InstructionFirst {
                instruction: &'a str,
                data: Vec<Value>,
            },
        }
        let instruction = self.instruction;
        let prompt = if self
            .fields
            .first()
            .is_some_and(|field| field.name == "numbered_markdown")
        {
            Prompt::DocumentFirst {
                data: fields,
                instruction,
            }
        } else {
            Prompt::InstructionFirst {
                instruction,
                data: fields,
            }
        };
        serde_json::to_string(&prompt).map_err(|_| StepError::Preparation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StepError {
    Preparation,
    RestrictedContext,
    BudgetExhausted,
    Model(ErrorCategory),
}

impl StepError {
    pub(crate) fn invalid_feedback(&self) -> Option<&'static str> {
        matches!(self, Self::Model(ErrorCategory::InvalidResponse))
            .then_some("The previous response was invalid. Return a complete JSON value matching the requested schema and field types.")
    }
}

impl<M: Model> Runtime<M> {
    /// Send one typed request after validating its data sources and reserving a run attempt.
    /// A decoded result is still subject to the calling scenario's domain checks.
    pub async fn step<T: DeserializeOwned>(
        &self,
        run: &ReviewRun,
        request: &StageRequest<'_>,
    ) -> Result<T, StepError> {
        run.retry_invalid(
            0,
            |_| 3,
            |feedback| self.step_once(run, request, feedback),
            |result| result.map_err(|e| (e, e.invalid_feedback())),
        )
        .await
    }

    pub(crate) async fn step_once<T: DeserializeOwned>(
        &self,
        run: &ReviewRun,
        request: &StageRequest<'_>,
        feedback: Option<&'static str>,
    ) -> Result<T, StepError> {
        self.step_once_with_reserve(run, request, feedback, 0).await
    }

    pub(crate) async fn step_once_with_reserve<T: DeserializeOwned>(
        &self,
        run: &ReviewRun,
        request: &StageRequest<'_>,
        feedback: Option<&'static str>,
        reserved: usize,
    ) -> Result<T, StepError> {
        let model = match request.kind {
            StageKind::Claims | StageKind::Search => &self.model,
            StageKind::Review | StageKind::Control | StageKind::Comparison => self.reviewer(),
        };
        let stage = super::logging::TypedStage::start(&run.id, request.kind);
        let prompt = match request.prepare(feedback) {
            Ok(prompt) => prompt,
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
                model.name(),
                |attempt| {
                    last_attempt = Some(attempt);
                    stage.dispatch(attempt);
                    self.decode_request_retryable(model, &prompt, request.schema.as_ref(), None)
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
        stage.finish(
            result.as_ref().map(|_| ()).map_err(|error| *error),
            last_attempt,
        );
        result
    }
}
