//! Per-condition equipment adequacy review with grounded evidence and resumable questions.
mod assessment;
mod extract;
pub mod materials;
mod prompts;
mod resume;
mod review;
pub mod sources;
#[cfg(test)]
mod tests;
mod types;

pub use resume::resume_equipment_adequacy;
pub use types::*;

use crate::runtime::{
    ReviewRun, Runtime, StageKind, StageRequest, grok::Model, search::SearchModel,
};
use serde::{Serialize, de::DeserializeOwned};
use sources::{HttpTransport, SourceSession};

pub(crate) struct Work<'a, M> {
    pub runtime: &'a Runtime<M>,
    pub run: &'a ReviewRun,
    pub options: &'a QualificationOptions,
    pub reserve_review: bool,
}
impl<M: Model> Work<'_, M> {
    fn available(&self) -> Result<(), FailureReason> {
        if self.run.remaining() <= usize::from(self.reserve_review) {
            Err(FailureReason::BudgetExhausted)
        } else {
            Ok(())
        }
    }
    pub(crate) async fn call<T: DeserializeOwned + schemars::JsonSchema>(
        &self,
        instruction: &'static str,
        value: &(impl Serialize + Sync),
        visibility: &[crate::core::Visibility],
        kind: StageKind,
    ) -> Result<T, FailureReason> {
        let schema = schemars::schema_for!(T);
        if serde_json::to_vec(value)
            .map_err(|_| FailureReason::Preparation)?
            .len()
            .saturating_add(schema.as_value().to_string().len())
            > self.options.max_context_bytes
        {
            return Err(FailureReason::ContextLimit);
        }
        self.available()?;
        let request = StageRequest::new(kind, instruction)
            .field("input", visibility, value)
            .field(
                "expected_response",
                &[crate::core::Visibility::Public],
                &schema,
            );
        self.runtime
            .step(self.run, &request)
            .await
            .map_err(Into::into)
    }
}

pub async fn review_equipment_adequacy<W: SearchModel, R: Model, H: HttpTransport>(
    input: &QualificationInput,
    options: &QualificationOptions,
    worker: &Runtime<W>,
    reviewer: &Runtime<R>,
    sources: &H,
) -> Result<QualificationReport, QualificationInputError> {
    materials::validate_input(input, options)?;
    let mut report = materials::prepare(input, options).await;
    let run = ReviewRun::new(options.model_attempts);
    let mut session = SourceSession {
        transport: sources,
        options,
        requests: 0,
    };
    let work = Work {
        runtime: worker,
        run: &run,
        options,
        reserve_review: true,
    };
    extract::extract(&mut report, &work).await;
    for i in 0..report.requirements.len() {
        let mut record = report.requirements.remove(i);
        assessment::assess(&mut report, &mut record, &work, &mut session).await;
        report.requirements.insert(i, record);
    }
    review::review(&mut report, &work, reviewer, &mut session).await;
    report.model_attempts = run.attempts();
    report.http_requests = session.requests;
    Ok(report)
}
