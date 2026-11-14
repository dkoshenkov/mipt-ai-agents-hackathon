use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    claims::{Claim, Constraint, Quote},
    investigation::Candidate,
    markdown::{Evidence, MarkdownIndex},
};
use crate::{
    core::Visibility,
    runtime::{ReviewRun, Runtime, StageKind, StageRequest, StepError, grok::Model},
};

#[derive(Debug, Default, Serialize)]
pub struct SemanticOutcome {
    pub candidates: Vec<Candidate>,
    pub issues: Vec<String>,
}

#[derive(Deserialize)]
struct Response {
    candidates: Vec<Proposal>,
}

#[derive(Deserialize)]
struct Proposal {
    category: String,
    claims: Vec<usize>,
    relation: Vec<Quote>,
}

/// Ask about one explicitly scoped set of grounded claims. A failure affects this set only.
pub async fn generate<M: Model>(
    runtime: &Runtime<M>,
    run: &ReviewRun,
    index: &MarkdownIndex,
    visibility: Visibility,
    claims: &[Claim],
    max_bytes: usize,
) -> Result<SemanticOutcome, StepError> {
    let mut ids = Vec::new();
    for claim in claims {
        let Some(block) = index.blocks.get(claim.statement.evidence.block_id) else {
            return Err(StepError::Preparation);
        };
        ids.push(block.id);
        ids.extend(&block.headings);
        ids.extend(&block.table_context);
    }
    ids.sort_unstable();
    ids.dedup();
    let source_text: Vec<_> = ids
        .iter()
        .map(|&id| (id, &index.source[index.blocks[id].range.clone()]))
        .collect();
    if claims
        .iter()
        .any(|c| c.statement.evidence.version != index.version)
        || serde_json::to_vec(&(claims, &source_text))
            .map_err(|_| StepError::Preparation)?
            .len()
            > max_bytes
    {
        return Err(StepError::Preparation);
    }
    let sources = [visibility];
    let request = StageRequest::new(StageKind::Comparison, "Find potential semantic conflicts in these grounded claims. Return JSON {candidates:[{category,claims,relation}]}, where category is semantic or naming, claims are at least two distinct zero-based indices, and relation contains exact {block_id,quote} citations showing how these claims refer to the same position or obligation. Include incompatible actions, exclusions, mandatory components and names; do not treat compatible refinements, abbreviations, permitted equivalents, or different units of counting as conflicts. Do not guess relations or infer missing facts. A later reviewer will check each hypothesis.")
        .field("claims", &sources, claims)
        .field("sources", &sources, &source_text);
    let response: Response = runtime.step(run, &request).await?;
    let mut result = SemanticOutcome::default();
    for proposal in response.candidates {
        let mut ids = proposal.claims;
        ids.sort_unstable();
        ids.dedup();
        let selected: Option<Vec<_>> = ids
            .iter()
            .map(|i| claims.get(*i).filter(|c| c.complete))
            .collect();
        let relation: Option<Vec<_>> = proposal
            .relation
            .iter()
            .map(|q| {
                // Relations must come from an address already supplied in the selected claims.
                let selected = selected.as_ref()?;
                let evidence = index.resolve(q.block_id, &q.quote)?;
                selected
                    .iter()
                    .any(|c| {
                        claim_evidence(c)
                            .iter()
                            .any(|outer| contains(outer, &evidence))
                    })
                    .then_some(evidence)
            })
            .collect();
        let Some(selected) = selected.filter(|s| s.len() >= 2) else {
            result.issues.push(
                "semantic proposal references missing, incomplete, or fewer than two claims".into(),
            );
            continue;
        };
        let Some(relation) = relation.filter(|r| !r.is_empty()) else {
            result
                .issues
                .push("semantic proposal has an invalid or absent relation".into());
            continue;
        };
        if selected.iter().any(|c| {
            !relation
                .iter()
                .any(|e| claim_evidence(c).iter().any(|outer| contains(outer, e)))
        }) {
            result
                .issues
                .push("semantic relation must cite each claim".into());
            continue;
        }
        if proposal.category != "semantic" && proposal.category != "naming" {
            result.issues.push("unknown semantic category".into());
            continue;
        }
        let mut evidence: Vec<_> = selected
            .iter()
            .map(|c| c.statement.evidence.clone())
            .collect();
        if evidence
            .iter()
            .enumerate()
            .any(|(i, e)| evidence[..i].contains(e))
        {
            result
                .issues
                .push("semantic proposal repeats a statement".into());
            continue;
        }
        let mut context: Vec<_> = selected
            .iter()
            .flat_map(|c| claim_evidence(c))
            .chain(relation)
            .collect();
        let order = |e: &Evidence| (e.range.start, e.range.end, e.block_id);
        evidence.sort_by_key(order);
        evidence.dedup();
        context.sort_by_key(order);
        context.dedup();
        context.retain(|e| !evidence.contains(e));
        let id = serde_json::to_vec(&(&proposal.category, &evidence))
            .map_err(|_| StepError::Preparation)?;
        let candidate = Candidate {
            id: format!("{:x}", Sha256::digest(id)),
            category: proposal.category,
            object: selected[0].object.key.clone().unwrap_or_default(),
            property: selected[0].property.text.clone(),
            evidence,
            context,
            known_gaps: vec![],
        };
        if !result.candidates.iter().any(|c| c.id == candidate.id) {
            result.candidates.push(candidate);
        }
    }
    Ok(result)
}

fn contains(outer: &Evidence, inner: &Evidence) -> bool {
    outer.block_id == inner.block_id
        && outer.range.start <= inner.range.start
        && inner.range.end <= outer.range.end
}

pub(super) fn claim_evidence(c: &Claim) -> Vec<Evidence> {
    let mut evidence = vec![c.statement.evidence.clone(), c.property.evidence.clone()];
    evidence.extend(
        c.object
            .evidence
            .iter()
            .chain(&c.object.alternatives)
            .chain(&c.conditions)
            .chain(&c.stage)
            .chain(&c.mandatory)
            .map(|g| g.evidence.clone()),
    );
    match &c.constraint {
        Constraint::Semantic { text } => evidence.push(text.evidence.clone()),
        Constraint::Numeric {
            operator_evidence,
            value,
            unit,
            ..
        } => {
            evidence.extend([operator_evidence.evidence.clone(), value.evidence.clone()]);
            evidence.extend(unit.iter().map(|g| g.evidence.clone()));
        }
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        consistency::claims::{
            ClaimBatch, ClaimProposal, ConstraintProposal, ObjectProposal, validate_batch,
        },
        core::ErrorCategory,
    };
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    struct Mock(Arc<Mutex<Vec<String>>>);
    impl Model for Mock {
        async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
            self.0.lock().unwrap().push(prompt.into());
            Ok(r#"{"candidates":[{"category":"semantic","claims":[0,1],"relation":[{"block_id":0,"quote":"монтаж обязателен"},{"block_id":1,"quote":"монтаж исключён"}]},{"category":"semantic","claims":[0,9],"relation":[]},{"category":"semantic","claims":[0,1],"relation":[{"block_id":0,"quote":"нет"}]}]}"#.into())
        }
    }
    fn q(block_id: usize, quote: &str) -> Quote {
        Quote {
            block_id,
            quote: quote.into(),
        }
    }
    fn claim(id: usize, text: &str, action: &str) -> ClaimProposal {
        ClaimProposal {
            statement: q(id, text),
            object: ObjectProposal {
                key: "прибор".into(),
                evidence: vec![q(id, "прибор")],
                alternatives: vec![],
            },
            property: q(id, "монтаж"),
            constraint: ConstraintProposal::Semantic {
                text: q(id, action),
            },
            stage: None,
            conditions: vec![],
            mandatory: None,
        }
    }
    #[tokio::test]
    async fn scoped_semantic_call_cites_every_claim_and_rejects_orphans() {
        let index =
            MarkdownIndex::new("прибор монтаж обязателен\n\nприбор монтаж исключён").unwrap();
        let outcome = validate_batch(
            &index,
            &[0, 1],
            ClaimBatch {
                claims: vec![
                    claim(0, "прибор монтаж обязателен", "обязателен"),
                    claim(1, "прибор монтаж исключён", "исключён"),
                ],
                priority_rules: vec![],
            },
        );
        let prompts = Arc::new(Mutex::new(vec![]));
        let runtime = Runtime::new(Mock(prompts.clone()), Duration::from_secs(1));
        let result = generate(
            &runtime,
            &ReviewRun::new(1),
            &index,
            Visibility::Public,
            &outcome.claims,
            10000,
        )
        .await
        .unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].evidence.len(), 2);
        assert_eq!(result.issues.len(), 2);
        assert!(prompts.lock().unwrap()[0].contains("исключён"));
        assert!(prompts.lock().unwrap()[0].contains("\"name\":\"sources\""));
    }
}
