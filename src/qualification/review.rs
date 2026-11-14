use super::{
    Work, assessment,
    extract::sort_records,
    materials, prompts,
    sources::{HttpTransport, SourceSession},
    types::*,
};
use crate::runtime::{Runtime, StageKind, grok::Model, search::SearchModel};
use std::collections::{HashMap, HashSet};

pub(crate) async fn review<W: SearchModel, R: Model, H: HttpTransport>(
    report: &mut QualificationReport,
    worker: &Work<'_, W>,
    reviewer: &Runtime<R>,
    sources: &mut SourceSession<'_, H>,
) {
    let original_ids: Vec<_> = report
        .coverage
        .blocks
        .iter()
        .map(|b| b.block_id.clone())
        .collect();
    let payload = serde_json::json!({"document_id":report.document_id,"original_blocks":original_ids,
        "blocks":report.blocks,"materials":report.materials,"records":report.requirements,
        "clarifications":report.clarifications,"source_origins":report.external_sources,"coverage":report.coverage});
    let review_work = Work {
        runtime: reviewer,
        run: worker.run,
        options: worker.options,
        reserve_review: false,
    };
    let response: ReviewResponse = match review_work
        .call(
            prompts::REVIEW,
            &payload,
            &report.visibility(),
            StageKind::Review,
        )
        .await
    {
        Ok(result) => result,
        Err(reason) => {
            report.coverage.review = ReviewStatus::Failed {
                failure: Failure::new("final_review", reason.clone()),
            };
            report.gap(original_ids, "final_review", reason);
            return;
        }
    };
    report.coverage.review = ReviewStatus::Completed;
    let mut note_counts = HashMap::new();
    let mut target_counts = HashMap::new();
    for note in &response.notes {
        *note_counts.entry(note.id.clone()).or_insert(0) += 1;
        for id in targets(&note.change) {
            *target_counts.entry(id).or_insert(0) += 1;
        }
    }
    let work = Work {
        runtime: worker.runtime,
        run: worker.run,
        options: worker.options,
        reserve_review: false,
    };
    let original_requirements = report.requirements.clone();
    let closed_questions = report.closed_questions.len();
    let first_note = report.notes.len();
    for note in response.notes {
        let original_ids = targets(&note.change);
        let evidence = materials::resolve_all(&report.blocks, &note.evidence);
        let valid = !note.id.trim().is_empty()
            && !note.reason.trim().is_empty()
            && !note.evidence.is_empty()
            && evidence.is_ok()
            && note_counts[&note.id] == 1
            && original_ids.iter().all(|id| target_counts[id] == 1);
        let result = if valid {
            apply(report, &note, &work, sources).await
        } else {
            Err(FailureReason::InvalidResponse)
        };
        match result {
            Ok(resulting_id) => report.notes.push(NoteOutcome {
                note_id: note.id,
                applied: true,
                reason: note.reason,
                original_ids,
                resulting_id,
                evidence: evidence.unwrap(),
            }),
            Err(reason) => {
                let ids = note.evidence.iter().map(|q| q.block_id.clone()).collect();
                report.gap(ids, "review_note", reason);
                report.notes.push(NoteOutcome {
                    note_id: note.id,
                    applied: false,
                    reason: note.reason,
                    original_ids,
                    resulting_id: None,
                    evidence: evidence.unwrap_or_default(),
                });
            }
        }
    }
    sort_records(report);
    if !reconcile(report, response.blocks) {
        // Notes and their block mappings form one change; reject it without losing prior records.
        report.requirements = original_requirements;
        report.closed_questions.truncate(closed_questions);
        for note in &mut report.notes[first_note..] {
            note.applied = false;
            note.resulting_id = None;
        }
        for block in &mut report.coverage.blocks {
            block.reviewed = false;
        }
    }
}

fn targets(change: &ReviewChange) -> Vec<String> {
    match change {
        ReviewChange::Add { .. } => vec![],
        ReviewChange::Reassess { requirement_id } | ReviewChange::Remove { requirement_id } => {
            vec![requirement_id.clone()]
        }
        ReviewChange::Merge {
            requirement_ids, ..
        } => requirement_ids.clone(),
    }
}

async fn apply<W: SearchModel, H: HttpTransport>(
    report: &mut QualificationReport,
    note: &ReviewNote,
    work: &Work<'_, W>,
    sources: &mut SourceSession<'_, H>,
) -> Result<Option<String>, FailureReason> {
    let evidence = materials::resolve_all(&report.blocks, &note.evidence)?;
    let ids = targets(&note.change);
    if ids.iter().collect::<HashSet<_>>().len() != ids.len() {
        return Err(FailureReason::InvalidResponse);
    }
    for id in &ids {
        let record = report
            .requirements
            .iter()
            .find(|r| &r.requirement.id == id)
            .ok_or(FailureReason::InvalidResponse)?;
        if !evidence
            .iter()
            .any(|e| record.requirement.evidence.iter().any(|s| overlaps(e, s)))
        {
            return Err(FailureReason::InvalidResponse);
        }
    }
    match &note.change {
        ReviewChange::Add { requirement } => {
            let id = report.next_id("requirement");
            let req = materials::requirement(report, id.clone(), requirement.clone())?;
            if !req
                .evidence
                .iter()
                .any(|s| evidence.iter().any(|e| overlaps(e, s)))
                || report.requirements.iter().any(|r| {
                    r.requirement.evidence == req.evidence
                        && r.requirement.constraint == req.constraint
                        && r.requirement.property == req.property
                })
            {
                return Err(FailureReason::InvalidResponse);
            }
            let mut record = fresh(req);
            assessment::assess(report, &mut record, work, sources).await;
            report.requirements.push(record);
            Ok(Some(id))
        }
        ReviewChange::Reassess { requirement_id } => {
            let index = report
                .requirements
                .iter()
                .position(|r| r.requirement.id == *requirement_id)
                .ok_or(FailureReason::InvalidResponse)?;
            let mut record = report.requirements.remove(index);
            assessment::assess(report, &mut record, work, sources).await;
            report.requirements.insert(index, record);
            Ok(Some(requirement_id.clone()))
        }
        ReviewChange::Remove { requirement_id } => {
            let index = report
                .requirements
                .iter()
                .position(|r| r.requirement.id == *requirement_id)
                .ok_or(FailureReason::InvalidResponse)?;
            let record = report.requirements.remove(index);
            close_question(report, &record);
            Ok(None)
        }
        ReviewChange::Merge {
            requirement_ids,
            requirement,
        } => {
            if requirement_ids.len() < 2 {
                return Err(FailureReason::InvalidResponse);
            }
            let mut indices = requirement_ids
                .iter()
                .map(|id| {
                    report
                        .requirements
                        .iter()
                        .position(|r| r.requirement.id == *id)
                        .ok_or(FailureReason::InvalidResponse)
                })
                .collect::<Result<Vec<_>, _>>()?;
            indices.sort();
            let first = &report.requirements[indices[0]].requirement;
            let id = first.id.clone();
            let req = materials::requirement(report, id.clone(), requirement.clone())?;
            if req.position != first.position
                || req.property != first.property
                || req.component != first.component
                || req.scope != first.scope
            {
                return Err(FailureReason::InvalidResponse);
            }
            let originals: Vec<_> = indices
                .iter()
                .map(|i| &report.requirements[*i].requirement)
                .collect();
            if originals.iter().any(|r| {
                r.position != first.position
                    || r.property != first.property
                    || r.component != first.component
                    || r.scope != first.scope
            }) || !covers(&req, &originals)
            {
                return Err(FailureReason::InvalidResponse);
            }
            let mut record = fresh(req);
            for index in indices.into_iter().rev() {
                let old = report.requirements.remove(index);
                close_question(report, &old);
                record.facts.extend(old.facts);
                record.read_evidence.extend(old.read_evidence);
            }
            assessment::assess(report, &mut record, work, sources).await;
            report.requirements.push(record);
            Ok(Some(id))
        }
    }
}

fn close_question(report: &mut QualificationReport, record: &RequirementRecord) {
    if let RequirementOutcome::Pending {
        question: Some(q), ..
    } = &record.outcome
    {
        report.closed_questions.push(q.clone());
    }
}
fn fresh(requirement: Requirement) -> RequirementRecord {
    RequirementRecord {
        requirement,
        outcome: RequirementOutcome::Stopped {
            failure: Failure::new("assessment", FailureReason::BudgetExhausted),
        },
        facts: vec![],
        read_evidence: vec![],
    }
}
fn overlaps(a: &SourceRef, b: &SourceRef) -> bool {
    a.block_id == b.block_id && a.version == b.version && a.start < b.end && b.start < a.end
}

fn covers(new: &Requirement, old: &[&Requirement]) -> bool {
    if old.iter().any(|r| {
        r.evidence.iter().any(|s| {
            !new.evidence
                .iter()
                .any(|n| n.block_id == s.block_id && n.start <= s.start && n.end >= s.end)
        })
    }) {
        return false;
    }
    if old.iter().all(|r| r.constraint == new.constraint) {
        return true;
    }
    match &new.constraint {
        Constraint::Alternatives { values } => {
            let mut original_values = HashSet::new();
            for r in old {
                match &r.constraint {
                    Constraint::Qualitative { value } => {
                        original_values.insert(value);
                    }
                    Constraint::Alternatives { values } => original_values.extend(values),
                    _ => return false,
                }
            }
            values.iter().collect::<HashSet<_>>() == original_values
        }
        Constraint::Numeric {
            operator: Operator::Range,
            value,
            upper: Some(upper),
            unit,
        } => {
            old.iter().any(|r| {
                matches!(
                    r.constraint,
                    Constraint::Numeric {
                        operator: Operator::AtLeast,
                        ..
                    }
                )
            }) && old.iter().any(|r| {
                matches!(
                    r.constraint,
                    Constraint::Numeric {
                        operator: Operator::AtMost,
                        ..
                    }
                )
            }) && old.iter().all(|r| match &r.constraint {
                Constraint::Numeric {
                    operator: Operator::AtLeast,
                    value: v,
                    unit: u,
                    ..
                } => v == value && u == unit,
                Constraint::Numeric {
                    operator: Operator::AtMost,
                    value: v,
                    unit: u,
                    ..
                } => v == upper && u == unit,
                _ => false,
            })
        }
        _ => false,
    }
}

fn reconcile(report: &mut QualificationReport, results: Vec<BlockReconciliation>) -> bool {
    let gaps = report.coverage.gaps.len();
    let mut mapped = HashSet::new();
    let coverage_ids: Vec<_> = report
        .coverage
        .blocks
        .iter()
        .map(|b| b.block_id.clone())
        .collect();
    if results.iter().any(|b| !coverage_ids.contains(&b.block_id)) {
        report.gap(vec![], "reconcile", FailureReason::InvalidResponse);
    }
    for block_id in coverage_ids {
        let rows: Vec<_> = results.iter().filter(|r| r.block_id == block_id).collect();
        let mut valid = rows.len() == 1;
        if let Some(row) = rows.first() {
            if row.conditions.is_empty() {
                valid &= row
                    .no_requirements_reason
                    .as_ref()
                    .is_some_and(|s| !s.trim().is_empty());
            } else {
                valid &= row.no_requirements_reason.is_none();
            }
            for condition in &row.conditions {
                let source = materials::resolve(&report.blocks, &condition.condition);
                let id = match (&condition.requirement_id, &condition.note_id) {
                    (Some(id), None) => Some(id.clone()),
                    (None, Some(note)) => report
                        .notes
                        .iter()
                        .find(|n| n.note_id == *note && n.applied)
                        .and_then(|n| n.resulting_id.clone()),
                    _ => None,
                };
                let found = id
                    .as_ref()
                    .and_then(|id| report.requirements.iter().find(|r| r.requirement.id == *id));
                let mapping_valid = match (&source, found) {
                    (Ok(source), Some(record)) => {
                        source.block_id == block_id
                            && record.requirement.evidence.iter().any(|s| {
                                s.block_id == source.block_id
                                    && s.start <= source.start
                                    && s.end >= source.end
                            })
                    }
                    _ => false,
                };
                if !mapping_valid || !id.is_some_and(|id| mapped.insert(id)) {
                    valid = false;
                }
            }
        }
        if valid {
            if let Some(block) = report
                .coverage
                .blocks
                .iter_mut()
                .find(|b| b.block_id == block_id)
            {
                block.reviewed = true;
            }
        } else {
            report.gap(vec![block_id], "reconcile", FailureReason::InvalidResponse);
        }
    }
    let missing: Vec<_> = report
        .requirements
        .iter()
        .filter(|r| !mapped.contains(&r.requirement.id))
        .flat_map(|r| r.requirement.evidence.iter().map(|s| s.block_id.clone()))
        .collect();
    if !missing.is_empty() {
        report.gap(missing, "reconcile", FailureReason::InvalidResponse);
    }
    report.coverage.gaps.len() == gaps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_minima_cannot_invent_an_upper_bound() {
        let mut minimum = Requirement {
            id: "one".into(),
            position: "1".into(),
            component: None,
            property: "radius".into(),
            scope: "measurement".into(),
            constraint: Constraint::Numeric {
                operator: Operator::AtLeast,
                value: "7".into(),
                upper: None,
                unit: "mm".into(),
            },
            evidence: vec![],
            reference: None,
        };
        let mut range = minimum.clone();
        range.constraint = Constraint::Numeric {
            operator: Operator::Range,
            value: "7".into(),
            upper: Some("25".into()),
            unit: "mm".into(),
        };
        assert!(!covers(&range, &[&minimum, &minimum]));
        let mut maximum = minimum.clone();
        maximum.constraint = Constraint::Numeric {
            operator: Operator::AtMost,
            value: "25".into(),
            upper: None,
            unit: "mm".into(),
        };
        assert!(covers(&range, &[&minimum, &maximum]));
        minimum.constraint = Constraint::Numeric {
            operator: Operator::AtLeast,
            value: "8".into(),
            upper: None,
            unit: "mm".into(),
        };
        assert!(!covers(&range, &[&minimum, &maximum]));
    }
}
