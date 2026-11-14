use super::{
    Work, assessment, materials,
    sources::{HttpTransport, SourceSession},
    types::*,
};
use crate::{
    core::Visibility,
    runtime::{ReviewRun, Runtime, search::SearchModel},
};

pub async fn resume_equipment_adequacy<M: SearchModel, H: HttpTransport>(
    input: &QualificationInput,
    mut report: QualificationReport,
    answer: Clarification,
    options: &QualificationOptions,
    worker: &Runtime<M>,
    sources: &H,
) -> ResumeOutcome {
    let rejection = validate(input, &report, &answer, options).err();
    if let Some(reason) = rejection {
        return ResumeOutcome::Rejected { reason, report };
    }
    let index = report
        .requirements
        .iter()
        .position(|r| r.requirement.id == answer.requirement_id)
        .unwrap();
    let mut record = report.requirements.remove(index);
    report.clarifications.push(answer);
    let run = ReviewRun::new(options.model_attempts);
    let work = Work {
        runtime: worker,
        run: &run,
        options,
        reserve_review: false,
    };
    let mut session = SourceSession {
        transport: sources,
        options,
        requests: 0,
    };
    assessment::assess(&mut report, &mut record, &work, &mut session).await;
    report.requirements.insert(index, record);
    report.model_attempts += run.attempts();
    report.http_requests += session.requests;
    ResumeOutcome::Updated { report }
}

fn validate(
    input: &QualificationInput,
    report: &QualificationReport,
    answer: &Clarification,
    options: &QualificationOptions,
) -> Result<(), FailureReason> {
    materials::validate_input(input, options).map_err(|_| FailureReason::InvalidInput)?;
    let actual = materials::manifest(input);
    for expected in &report.materials {
        let found = actual
            .iter()
            .find(|m| m.id == expected.id)
            .ok_or_else(|| FailureReason::MissingMaterial(expected.id.clone()))?;
        if found.version != expected.version || found.format != expected.format {
            return Err(FailureReason::VersionMismatch(expected.id.clone()));
        }
        if found.visibility != expected.visibility {
            return Err(FailureReason::VisibilityMismatch(expected.id.clone()));
        }
    }
    if actual.len() != report.materials.len() || input.document.id != report.document_id {
        return Err(FailureReason::InvalidInput);
    }
    if actual
        .first()
        .is_none_or(|m| m.version != answer.document_version)
    {
        return Err(FailureReason::VersionMismatch(report.document_id.clone()));
    }
    if answer.text.trim().is_empty() {
        return Err(FailureReason::InvalidQuestion);
    }
    let matches = report
        .questions()
        .filter(|(r, q)| r.id == answer.requirement_id && q.id == answer.question_id)
        .count();
    if matches != 1
        || report
            .clarifications
            .iter()
            .any(|a| a.question_id == answer.question_id)
    {
        return Err(FailureReason::InvalidQuestion);
    }
    if answer.visibility != Visibility::Public
        || report.visibility().iter().any(|v| *v != Visibility::Public)
    {
        return Err(FailureReason::RestrictedContext);
    }
    Ok(())
}
