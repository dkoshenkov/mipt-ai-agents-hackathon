use super::{
    Work, materials, prompts,
    sources::{HttpTransport, SourceSession},
    types::*,
};
use crate::runtime::{StageKind, StageRequest, search::SearchModel};
use std::collections::HashSet;

pub(super) const INDEPENDENT_BASIS_FEEDBACK: &str = "Cite an independent operational basis. If a customer fact is missing, return ask with a concrete question.";

pub(crate) async fn assess<M: SearchModel, H: HttpTransport>(
    report: &mut QualificationReport,
    record: &mut RequirementRecord,
    work: &Work<'_, M>,
    sources: &mut SourceSession<'_, H>,
) {
    let mut read = HashSet::new();
    let mut first = true;
    let outcome = loop {
        update_reservation_record(report, record, work);
        let answers: Vec<_> = report
            .clarifications
            .iter()
            .filter(|c| c.requirement_id == record.requirement.id)
            .collect();
        let data = serde_json::json!({"current_requirement":record.requirement,"blocks":report.blocks,
            "confirmed_facts":record.facts,"source_origins":report.external_sources,
            "read_evidence":record.read_evidence,"answers":answers});
        let (decision, sources_for_decision) = match work
            .call_checked(
                report, first, prompts::ASSESS,
                &data,
                StageKind::Claims,
                |decision| {
                    let evidence = validate_decision(report, record, &decision, !answers.is_empty()).inspect_err(|detail| {
                        tracing::warn!(run_id = work.run.id(), requirement_id = %record.requirement.id,
                            stage = "assessment", event = "decision_rejected", detail);
                    })?;
                    Ok((decision, evidence))
                },
            )
            .await
        {
            Ok(value) => value,
            Err(reason) => break stopped(report, &record.requirement, "assessment", reason),
        };
        first = false;
        match decision {
            Decision::Assess {
                assessment,
                operation,
                reason,
                allowed_case,
                alternative,
                margin,
                ..
            } => {
                break RequirementOutcome::Assessed {
                    assessment,
                    operation,
                    reason,
                    evidence: sources_for_decision,
                    allowed_case,
                    alternative,
                    margin,
                };
            }
            Decision::Ask { fact, question, .. } => {
                break RequirementOutcome::Pending {
                    missing_fact: fact.clone(),
                    question: Some(Question {
                        id: report.next_id("question"),
                        fact,
                        text: question,
                    }),
                };
            }
            Decision::Read { block_ids, .. } => {
                if block_ids.iter().all(|id| read.contains(id)) {
                    break RequirementOutcome::Pending {
                        missing_fact: "No new evidence in repeated read".into(),
                        question: None,
                    };
                }
                for id in block_ids {
                    if read.insert(id.clone()) {
                        let block = report.blocks.iter().find(|b| b.id == id).unwrap();
                        record.read_evidence.push(
                            materials::resolve(
                                &report.blocks,
                                &Quote {
                                    block_id: id,
                                    quote: block.text.clone(),
                                },
                            )
                            .unwrap(),
                        );
                    }
                }
            }
            Decision::TechnicalFact { target, query, .. } => {
                if record.facts.iter().any(|f| f.target == target) {
                    break RequirementOutcome::Pending {
                        missing_fact: format!(
                            "No new fact for {} / {}",
                            target.model, target.parameter
                        ),
                        question: None,
                    };
                }
                match find_fact(report, record, &target, &query, work, sources).await {
                    Ok(Some(fact)) => {
                        let key = fact_key(&fact, &report.blocks);
                        if record
                            .facts
                            .iter()
                            .any(|f| fact_key(f, &report.blocks) == key)
                        {
                            break RequirementOutcome::Pending {
                                missing_fact: "Repeated technical fact".into(),
                                question: None,
                            };
                        }
                        record.facts.push(fact);
                    }
                    Ok(None) => {
                        break RequirementOutcome::Pending {
                            missing_fact: format!(
                                "Primary source did not confirm {} / {}",
                                target.model, target.parameter
                            ),
                            question: None,
                        };
                    }
                    Err(reason) => break stopped(report, &record.requirement, "source", reason),
                }
            }
        }
    };
    if let RequirementOutcome::Pending {
        question: Some(q), ..
    } = &record.outcome
    {
        let answered = report.clarifications.iter().any(|a| a.question_id == q.id);
        if matches!(outcome, RequirementOutcome::Stopped { .. }) && !answered {
            return;
        }
        report.closed_questions.push(q.clone());
    }
    record.outcome = outcome;
}

fn update_reservation_record<M>(
    report: &mut QualificationReport,
    record: &RequirementRecord,
    work: &Work<'_, M>,
) {
    if work.reserve_review
        && let Some(current) = report
            .requirements
            .iter_mut()
            .find(|r| r.requirement.id == record.requirement.id)
    {
        *current = record.clone();
    }
}

fn validate_decision(
    report: &QualificationReport,
    record: &RequirementRecord,
    decision: &Decision,
    has_answers: bool,
) -> Result<Vec<SourceRef>, &'static str> {
    if decision.requirement_id() != record.requirement.id {
        return Err("Copy current_requirement.id exactly into requirement_id.");
    }
    match decision {
        Decision::Assess {
            assessment,
            operation,
            reason,
            evidence,
            allowed_case,
            alternative,
            margin,
            ..
        } => {
            let refs = materials::resolve_all(&report.blocks, evidence).map_err(
                |_| "Use a unique, exact quote from the named block for every evidence item.",
            )?;
            if operation.trim().is_empty() || reason.trim().is_empty() {
                return Err("Provide the operation and a reason for the assessment.");
            }
            if refs.is_empty()
                || (refs.iter().all(|s| record.requirement.evidence.contains(s))
                    && record.facts.is_empty()
                    && !has_answers)
            {
                return Err(INDEPENDENT_BASIS_FEEDBACK);
            }
            if *assessment == Assessment::Insufficient
                && allowed_case.as_ref().is_none_or(|s| s.trim().is_empty())
            {
                return Err("Provide the permitted configuration in allowed_case.");
            }
            if *assessment == Assessment::PossiblyExcessive
                && (alternative.as_ref().is_none_or(|s| s.trim().is_empty())
                    || margin.as_ref().is_none_or(|s| s.trim().is_empty()))
            {
                return Err("Provide both a supported alternative and its margin.");
            }
            return Ok(refs);
        }
        Decision::Ask { fact, question, .. }
            if fact.trim().is_empty() || question.trim().is_empty() =>
        {
            return Err("Give the missing fact and a specific question.");
        }
        Decision::Read { block_ids, .. }
            if block_ids.is_empty()
                || block_ids
                    .iter()
                    .any(|id| !report.blocks.iter().any(|b| b.id == *id)) =>
        {
            return Err("List at least one existing block ID for a read.");
        }
        Decision::TechnicalFact { target, query, .. }
            if target.model.trim().is_empty()
                || target.parameter.trim().is_empty()
                || query.trim().is_empty() =>
        {
            return Err("Specify the model, parameter and search query.");
        }
        _ => {}
    }
    Ok(vec![])
}

fn stopped(
    report: &mut QualificationReport,
    requirement: &Requirement,
    stage: &str,
    reason: FailureReason,
) -> RequirementOutcome {
    report.gap(
        requirement
            .evidence
            .iter()
            .map(|s| s.block_id.clone())
            .collect(),
        stage,
        reason.clone(),
    );
    RequirementOutcome::Stopped {
        failure: Failure::new(stage, reason),
    }
}

async fn find_fact<M: SearchModel, H: HttpTransport>(
    report: &mut QualificationReport,
    record: &mut RequirementRecord,
    target: &FactTarget,
    query: &str,
    work: &Work<'_, M>,
    sources: &mut SourceSession<'_, H>,
) -> Result<Option<ConfirmedFact>, FailureReason> {
    let attached: Vec<_> = report
        .blocks
        .iter()
        .filter(|b| b.material_id != report.document_id)
        .cloned()
        .collect();
    if !attached.is_empty()
        && let Some(fact) = verify(&attached, target, report, work).await?
    {
        return Ok(Some(fact));
    }
    let reserved = work.reserved(report);
    work.available(reserved, false)?;
    let data = serde_json::json!({"target":target,"query":query});
    if data.to_string().len() > work.options.max_context_bytes {
        return Err(FailureReason::ContextLimit);
    }
    let visibility = report.visibility();
    let request =
        StageRequest::new(StageKind::Search, prompts::SEARCH).field("query", &visibility, &data);
    let search = work
        .runtime
        .search_with_reserve(work.run, &request, work.options.max_search_turns, reserved)
        .await
        .map_err(FailureReason::from)?;
    report.searches.push(SearchTrace {
        requirement_id: record.requirement.id.clone(),
        target: target.clone(),
        result: search.clone(),
    });
    let mut urls: Vec<_> = search
        .citations
        .iter()
        .map(|c| c.url.clone())
        .chain(search.sources)
        .collect();
    let mut visited = HashSet::new();
    urls.retain(|u| visited.insert(u.clone()));
    let mut failure = None;
    for url in urls {
        let (material, provenance) = match sources.read(&url, &visibility).await {
            Ok(result) => result,
            Err(reason) => {
                failure = Some(reason);
                if sources.requests >= work.options.http_requests {
                    break;
                }
                continue;
            }
        };
        let (text, uncertain) = match materials::decode(&material, work.options).await {
            Ok(value) => value,
            Err(reason) => {
                failure = Some(reason);
                continue;
            }
        };
        let blocks = materials::index_text(&material.id, &provenance.version, &text);
        if uncertain || blocks.is_empty() {
            failure = Some(FailureReason::Unreadable);
            continue;
        }
        // Keep the locally read evidence even if verification's next model call fails.
        for block in &blocks {
            record.read_evidence.push(materials::resolve(
                &blocks,
                &Quote {
                    block_id: block.id.clone(),
                    quote: block.text.clone(),
                },
            )?);
        }
        report.blocks.retain(|b| b.material_id != material.id);
        report.blocks.extend(blocks.clone());
        report.external_sources.push(provenance);
        update_reservation_record(report, record, work);
        if let Some(fact) = verify(&blocks, target, report, work).await? {
            return Ok(Some(fact));
        }
    }
    match failure {
        Some(reason) => Err(reason),
        None => Ok(None),
    }
}

async fn verify<M: SearchModel>(
    blocks: &[Block],
    target: &FactTarget,
    report: &QualificationReport,
    work: &Work<'_, M>,
) -> Result<Option<ConfirmedFact>, FailureReason> {
    let origins: Vec<_> = report
        .external_sources
        .iter()
        .filter(|source| blocks.iter().any(|b| b.material_id == source.material_id))
        .collect();
    let attachments: Vec<_> = report
        .materials
        .iter()
        .filter(|material| blocks.iter().any(|b| b.material_id == material.id))
        .collect();
    let value = serde_json::json!({"target":target,"source_blocks":blocks,"source_origins":origins,"attachments":attachments});
    work.call_checked(report, false, prompts::VERIFY, &value, StageKind::Claims, |verification: FactVerification| {
        let Some(proposal) = verification.fact else {return Ok(None)};
        if !verification.primary_source || verification.reason.trim().is_empty() {return Ok(None);}
        validate_fact(blocks, target, proposal).map(Some).map_err(|_| "Cite the exact model, parameter, value, unit and conditions from the same applicable source and table column.")
    }).await
}

pub(crate) fn validate_fact(
    blocks: &[Block],
    target: &FactTarget,
    fact: FactProposal,
) -> Result<ConfirmedFact, FailureReason> {
    let model = materials::resolve(blocks, &fact.model)?;
    let parameter = materials::resolve(blocks, &fact.parameter)?;
    let value = materials::resolve(blocks, &fact.value)?;
    let unit = fact
        .unit
        .as_ref()
        .map(|q| materials::resolve(blocks, q))
        .transpose()?;
    let conditions = materials::resolve_all(blocks, &fact.conditions)?;
    if normalized(&model.quote) != normalized(&target.model)
        || !normalized(&parameter.quote).contains(&normalized(&target.parameter))
        || (target.numeric
            && (materials::number(&value.quote).is_none()
                || unit.as_ref().is_none_or(|s| s.quote.trim().is_empty())))
        || conditions.len() != target.conditions.len()
        || target.conditions.iter().any(|c| {
            !conditions
                .iter()
                .any(|s| normalized(&s.quote).contains(&normalized(c)))
        })
    {
        return Err(FailureReason::InvalidResponse);
    }
    if !materials::same_table_column(blocks, &model, &value)
        && !(model.block_id == value.block_id
            && materials::same_table_column(blocks, &parameter, &value))
    {
        return Err(FailureReason::InvalidResponse);
    }
    if parameter.material_id != value.material_id
        || model.material_id != value.material_id
        || (parameter.block_id != value.block_id
            && !materials::same_table_column(blocks, &parameter, &value))
        || unit.as_ref().is_some_and(|s| {
            let value_block = blocks.iter().find(|b| b.id == value.block_id).unwrap();
            let unit_block = blocks.iter().find(|b| b.id == s.block_id).unwrap();
            let same_column = materials::same_table_column(blocks, s, &value);
            let row_unit = s.block_id == value.block_id
                && (same_column
                    || (parameter.block_id == value.block_id
                        && materials::same_table_column(blocks, s, &parameter)));
            let header_unit = value_block.table_headers.contains(&unit_block.text) && same_column;
            s.material_id != value.material_id || !(row_unit || header_unit)
        })
        || conditions
            .iter()
            .any(|s| s.material_id != value.material_id)
    {
        return Err(FailureReason::InvalidResponse);
    }
    let mut evidence = vec![model, parameter, value.clone()];
    evidence.extend(unit.iter().cloned());
    evidence.extend(conditions);
    Ok(ConfirmedFact {
        target: target.clone(),
        value: value.quote,
        unit: unit.map(|s| s.quote),
        evidence,
    })
}

fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn fact_key(fact: &ConfirmedFact, blocks: &[Block]) -> String {
    // Canonical source cells make paraphrased search requests the same fact.
    let field = |source: &SourceRef| {
        blocks
            .iter()
            .find(|b| b.id == source.block_id)
            .and_then(|block| {
                materials::cells(&block.text)
                    .into_iter()
                    .find(|(start, end)| {
                        source.start >= block.start + start && source.end <= block.start + end
                    })
                    .map(|(start, end)| normalized(&block.text[start..end]))
            })
            .unwrap_or_else(|| normalized(&source.quote))
    };
    let mut conditions = fact
        .evidence
        .iter()
        .skip(3 + usize::from(fact.unit.is_some()))
        .map(&field)
        .collect::<Vec<_>>();
    conditions.sort();
    let value = materials::number(&fact.value)
        .map(|s| {
            if s.contains('.') {
                s.trim_end_matches('0').trim_end_matches('.').to_owned()
            } else {
                s
            }
        })
        .unwrap_or_else(|| normalized(&fact.value));
    format!(
        "{}|{}|{}|{}|{:?}",
        field(&fact.evidence[0]),
        field(&fact.evidence[1]),
        value,
        normalized(fact.unit.as_deref().unwrap_or("")),
        conditions
    )
}
