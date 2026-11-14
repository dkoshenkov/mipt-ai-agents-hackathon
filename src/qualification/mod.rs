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
    pub remaining_requirements: usize,
}
impl<M: Model> Work<'_, M> {
    fn reserved(&self, report: &QualificationReport) -> usize {
        self.remaining_requirements
            .saturating_add(if self.reserve_review {
                review::packet_count(report, self.options)
            } else {
                0
            })
    }
    fn available(&self, reserved: usize, first: bool) -> Result<(), FailureReason> {
        let reserved = if first {
            reserved.saturating_sub(self.remaining_requirements)
        } else {
            reserved
        };
        if self.run.remaining() <= reserved {
            Err(FailureReason::BudgetExhausted)
        } else {
            Ok(())
        }
    }
    pub(crate) async fn call_checked<T: DeserializeOwned + schemars::JsonSchema, U>(
        &self,
        report: &QualificationReport,
        first: bool,
        instruction: &'static str,
        value: &(impl Serialize + Sync),
        kind: StageKind,
        mut validate: impl FnMut(T) -> Result<U, &'static str>,
    ) -> Result<U, FailureReason> {
        let schema = schemars::schema_for!(T);
        if serde_json::to_vec(value)
            .map_err(|_| FailureReason::Preparation)?
            .len()
            .saturating_add(schema.as_value().to_string().len())
            > self.options.max_context_bytes
        {
            return Err(FailureReason::ContextLimit);
        }
        let reserved = self.reserved(report);
        self.available(reserved, first)?;
        tracing::info!(
            run_id = self.run.id(),
            event = "budget",
            remaining = self.run.remaining(),
            reserved,
            remaining_requirements = self.remaining_requirements
        );
        let visibility = report.visibility();
        let request = StageRequest::new(kind, instruction)
            .field("input", &visibility, value)
            .field(
                "expected_response",
                &[crate::core::Visibility::Public],
                &schema,
            );
        self.run
            .retry_invalid(
                reserved,
                |reason| {
                    if reason == Some(assessment::INDEPENDENT_BASIS_FEEDBACK) {
                        1
                    } else {
                        3
                    }
                },
                |feedback| {
                    self.runtime
                        .step_once_with_reserve(self.run, &request, feedback, reserved)
                },
                |result| {
                    let value =
                        result.map_err(|e| (FailureReason::from(e), e.invalid_feedback()))?;
                    validate(value).map_err(|detail| (FailureReason::InvalidResponse, Some(detail)))
                },
            )
            .await
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
        remaining_requirements: 0,
    };
    extract::extract(&mut report, &work).await;
    for i in 0..report.requirements.len() {
        let mut record = report.requirements[i].clone();
        let assessment_work = Work {
            remaining_requirements: report.requirements.len() - i - 1,
            ..work
        };
        tracing::info!(run_id = run.id(), event = "requirement", requirement = i + 1, total = report.requirements.len(), requirement_id = %record.requirement.id, remaining = run.remaining());
        assessment::assess(&mut report, &mut record, &assessment_work, &mut session).await;
        report.requirements[i] = record;
    }
    review::review(&mut report, &work, reviewer, &mut session).await;
    report.model_attempts = run.attempts();
    report.http_requests = session.requests;
    Ok(report)
}
