use super::{Work, materials, prompts, types::*};
use crate::runtime::{StageKind, grok::Model};

pub(crate) async fn extract<M: Model>(report: &mut QualificationReport, work: &Work<'_, M>) {
    let overhead = serde_json::json!({"packet": [], "specification": []})
        .to_string()
        .len()
        + schemars::schema_for!(Extraction)
            .as_value()
            .to_string()
            .len();
    let packet_bytes = work
        .options
        .extraction_batch_bytes
        .min(work.options.max_context_bytes.saturating_sub(overhead) / 2);
    let mut packets: Vec<Vec<Block>> = vec![];
    let mut packet = vec![];
    let mut size = 0;
    for block in report.blocks.clone() {
        let length = serde_json::to_vec(&block).unwrap().len() + 1;
        if length > packet_bytes {
            report.gap(vec![block.id], "extract", FailureReason::ContextLimit);
            continue;
        }
        if size + length > packet_bytes && !packet.is_empty() {
            packets.push(std::mem::take(&mut packet));
            size = 0;
        }
        size += length;
        packet.push(block);
    }
    if !packet.is_empty() {
        packets.push(packet);
    }
    let specification_packets: Vec<Vec<Block>> = packets
        .iter()
        .map(|packet| {
            packet
                .iter()
                .filter(|b| b.material_id == report.document_id)
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|packet| !packet.is_empty())
        .collect();
    let no_specification = vec![vec![]];
    for blocks in packets {
        let ids: Vec<_> = blocks.iter().map(|b| b.id.clone()).collect();
        // Check attachment obligations against every specification packet without sending the whole document.
        let contexts = if blocks.iter().any(|b| b.material_id != report.document_id)
            && !specification_packets.is_empty()
        {
            &specification_packets
        } else {
            &no_specification
        };
        for document in contexts {
            let input = serde_json::json!({"packet": blocks, "specification": document});
            let mut accepted = Vec::new();
            let mut unresolved = Vec::new();
            let mut rejected = Vec::new();
            let mut received = false;
            let extraction_work = Work {
                runtime: work.runtime,
                run: work.run,
                options: work.options,
                reserve_review: work.reserve_review,
                remaining_requirements: report.requirements.len(),
            };
            let result = extraction_work.call_checked(report, true, prompts::EXTRACT, &input, StageKind::Claims, |extraction: Extraction| {
                received = true;
                rejected.clear();
                for quote in extraction.unresolved {
                    if materials::resolve(&report.blocks, &quote).is_ok() {
                        if !unresolved.contains(&quote.block_id) {
                            unresolved.push(quote.block_id);
                        }
                    } else {
                        rejected.push(("obligation_scope", vec![quote.block_id]));
                    }
                }
                for draft in extraction.requirements {
                    if draft.evidence.iter().any(|q| ids.contains(&q.block_id))
                        && let Ok(requirement) = materials::requirement(report, String::new(), draft)
                    {
                        if !accepted.contains(&requirement) {
                            accepted.push(requirement);
                        }
                    } else {
                        rejected.push(("extract", ids.clone()));
                    }
                }
                if rejected.is_empty() { Ok(()) } else { Err("Correct the rejected requirements and unresolved quotes. Cite exact text and block IDs within this packet and give the complete extraction.") }
            }).await;
            if received {
                for coverage in &mut report.coverage.blocks {
                    if ids.contains(&coverage.block_id) {
                        coverage.extracted = true;
                    }
                }
            }
            for id in unresolved {
                report.gap(vec![id], "obligation_scope", FailureReason::Unreadable);
            }
            for mut requirement in accepted {
                if report.requirements.iter().any(|r| {
                    r.requirement.evidence == requirement.evidence
                        && r.requirement.property == requirement.property
                        && r.requirement.constraint == requirement.constraint
                        && r.requirement.position == requirement.position
                        && r.requirement.scope == requirement.scope
                }) {
                    continue;
                }
                requirement.id = report.next_id("requirement");
                report.requirements.push(RequirementRecord {
                    requirement,
                    facts: vec![],
                    read_evidence: vec![],
                    outcome: RequirementOutcome::Stopped {
                        failure: Failure::new("assessment", FailureReason::BudgetExhausted),
                    },
                });
            }
            if let Err(reason) = result {
                for (stage, blocks) in &rejected {
                    report.gap(blocks.clone(), stage, FailureReason::InvalidResponse);
                }
                if rejected.is_empty() || reason != FailureReason::InvalidResponse {
                    report.gap(ids.clone(), "extract", reason);
                }
            }
        }
    }
    sort_records(report);
}

pub(crate) fn sort_records(report: &mut QualificationReport) {
    let ranks: std::collections::HashMap<_, _> = report
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id.clone(), i))
        .collect();
    report.requirements.sort_by_key(|r| {
        r.requirement
            .evidence
            .iter()
            .map(|s| {
                (
                    ranks.get(&s.block_id).copied().unwrap_or(usize::MAX),
                    s.start,
                )
            })
            .min()
    });
}
