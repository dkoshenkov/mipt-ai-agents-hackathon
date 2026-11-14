pub mod batches;
pub mod catalog_http;
pub mod claims;
mod html;
pub mod investigation;
pub mod markdown;
pub mod numeric;
pub mod patch;
pub mod priority;
pub mod relations;
pub mod semantic;
pub mod standard_usage;
pub mod standards;
mod table_measure;
mod table_rows;
mod table_scope;

use std::{collections::BTreeSet, ops::Range};

use serde::Serialize;

use crate::{
    core::Document,
    runtime::{ReviewRun, Runtime, grok::Model},
};
use claims::{BindingIssue, Claim, ClaimRegistry, Constraint, Grounded};
use investigation::{Candidate, Investigation, investigate_bounded};
use markdown::{EmptyDocument, Evidence, MarkdownIndex};

#[derive(Debug, Clone, Copy)]
pub struct ReviewOptions<'a> {
    pub model_attempts: usize,
    pub packet_bytes: usize,
    pub read_bytes: usize,
    pub allow_read: bool,
    /// ISO calendar date for historical assessment. Defaults to the UTC date of the run.
    pub assessment_date: Option<&'a str>,
    pub user_response: Option<&'a priority::UserResponse>,
}

impl Default for ReviewOptions<'_> {
    fn default() -> Self {
        Self {
            model_attempts: 64,
            packet_bytes: 32_768,
            read_bytes: 8192,
            allow_read: true,
            assessment_date: None,
            user_response: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PacketCoverage {
    pub control_pass: bool,
    pub subjects: Vec<usize>,
    pub stop: Option<String>,
    pub binding_issues: Vec<BindingIssue>,
}

#[derive(Debug, Serialize)]
pub struct ObjectQuestion {
    pub reference: Grounded,
    pub alternatives: Vec<Grounded>,
    pub question: String,
}

/// Indices refer to the corresponding arrays in this report.
#[derive(Debug, Default, Serialize)]
pub struct StandardCoverage {
    pub complete: bool,
    pub uncertain_usage: Vec<usize>,
    pub failed_usage: Vec<usize>,
    pub partial_references: Vec<usize>,
    pub failed_catalog: Vec<usize>,
    pub missing_materials: Vec<usize>,
    pub unread_packets: Vec<usize>,
    pub parsing_gaps: Vec<usize>,
    pub oversized_blocks: Vec<usize>,
}

#[derive(Debug, Serialize)]
pub struct ConsistencyReport {
    pub run_id: String,
    pub version: String,
    pub assessment_date: String,
    pub assumed_assessment_date: bool,
    pub parsing_gaps: Vec<Range<usize>>,
    pub oversized_blocks: Vec<usize>,
    pub packets: Vec<PacketCoverage>,
    pub claims: Vec<Claim>,
    pub priority_mentions: Vec<Grounded>,
    pub initial_candidates: Vec<Candidate>,
    pub investigations: Vec<Investigation>,
    pub selections: Vec<(String, priority::Selection)>,
    pub response_target_matches: Option<usize>,
    pub standard_references: Vec<standards::ReferenceReview>,
    pub standard_usage: Vec<standard_usage::UsageReview>,
    pub missing_standard_materials: Vec<Grounded>,
    pub recovered_standard_mentions: Vec<Grounded>,
    pub standard_coverage: StandardCoverage,
    pub standard_material_questions: Vec<ObjectQuestion>,
    pub object_questions: Vec<ObjectQuestion>,
    pub semantic_gaps: Vec<(String, String)>,
    pub model_attempts: usize,
    pub patches: Vec<patch::Patch>,
    pub patch_verification: Option<patch::Verification>,
    pub copy_version: Option<String>,
    pub catalog_trace: Vec<catalog_http::Lookup>,
    pub catalog_requests: usize,
}

impl ConsistencyReport {
    fn calculate_standard_coverage(&self) -> StandardCoverage {
        let indices = |flags: Vec<bool>| {
            flags
                .into_iter()
                .enumerate()
                .filter_map(|(i, yes)| yes.then_some(i))
                .collect::<Vec<_>>()
        };
        let mut coverage = StandardCoverage {
            uncertain_usage: indices(
                self.standard_usage
                    .iter()
                    .map(|u| matches!(u.outcome, standard_usage::Outcome::InsufficientEvidence))
                    .collect(),
            ),
            failed_usage: indices(
                self.standard_usage
                    .iter()
                    .map(|u| matches!(u.outcome, standard_usage::Outcome::TechnicalFailure))
                    .collect(),
            ),
            partial_references: indices(
                self.standard_references
                    .iter()
                    .map(|r| matches!(r.existence, standards::Existence::PartialSearch))
                    .collect(),
            ),
            failed_catalog: indices(
                self.catalog_trace
                    .iter()
                    .map(|l| {
                        !matches!(
                            l.outcome,
                            catalog_http::Outcome::Found | catalog_http::Outcome::NotFound
                        )
                    })
                    .collect(),
            ),
            missing_materials: (0..self.missing_standard_materials.len()).collect(),
            unread_packets: indices(
                self.packets
                    .iter()
                    .map(|p| p.stop.is_some() || !p.binding_issues.is_empty())
                    .collect(),
            ),
            parsing_gaps: (0..self.parsing_gaps.len()).collect(),
            oversized_blocks: (0..self.oversized_blocks.len()).collect(),
            complete: false,
        };
        coverage.complete = coverage.uncertain_usage.is_empty()
            && coverage.failed_usage.is_empty()
            && coverage.partial_references.is_empty()
            && coverage.failed_catalog.is_empty()
            && coverage.missing_materials.is_empty()
            && coverage.unread_packets.is_empty()
            && coverage.parsing_gaps.is_empty()
            && coverage.oversized_blocks.is_empty();
        coverage
    }
}

pub async fn review_markdown<M: Model>(
    document: &Document,
    options: ReviewOptions<'_>,
    runtime: &Runtime<M>,
    catalog: &standards::Catalog,
) -> Result<ConsistencyReport, EmptyDocument> {
    review_with_catalog::<M, catalog_http::Http>(document, options, runtime, catalog, None).await
}

/// Run the same workflow with a caller-owned, bounded official catalogue session.
pub async fn review_with_catalog<M: Model, T: catalog_http::Transport>(
    document: &Document,
    options: ReviewOptions<'_>,
    runtime: &Runtime<M>,
    catalog: &standards::Catalog,
    mut live: Option<&mut catalog_http::CatalogHttp<T>>,
) -> Result<ConsistencyReport, EmptyDocument> {
    let run = ReviewRun::new(options.model_attempts);
    let index = MarkdownIndex::new(&document.markdown)?;
    let mut report = analyze(
        document,
        &index,
        options,
        runtime,
        catalog,
        &run,
        live.as_deref_mut(),
    )
    .await;
    if report
        .response_target_matches
        .is_some_and(|matches| matches != 1)
    {
        report.patch_verification = Some(patch::Verification::Incomplete {
            reasons: vec!["user response target is missing or ambiguous".into()],
        });
        return Ok(report);
    }
    let mut prepared = Vec::new();
    for (_, selection) in &report.selections {
        match patch::prepare(&index, selection, &report.claims) {
            Ok(Some(p)) => prepared.push(p),
            Ok(None) => (),
            Err(reason) => {
                report.patch_verification = Some(patch::Verification::Incomplete {
                    reasons: vec![reason],
                });
                return Ok(report);
            }
        }
    }
    for (reference, usage) in report
        .standard_references
        .iter()
        .zip(&report.standard_usage)
    {
        match patch::prepare_standard(&index, reference, usage) {
            Ok(Some(p)) => prepared.push(p),
            Ok(None) => (),
            Err(reason) => {
                report.patch_verification = Some(patch::Verification::Incomplete {
                    reasons: vec![reason],
                });
                return Ok(report);
            }
        }
    }
    if !prepared.is_empty() {
        report.patches = prepared;
        match patch::apply(&index, &report.patches) {
            Ok(applied) => {
                let copy = Document {
                    markdown: applied.source.clone(),
                    visibility: options
                        .user_response
                        .filter(|_| report.patches.iter().any(|p| p.user_provenance.is_some()))
                        .filter(|r| r.visibility != crate::core::Visibility::Public)
                        .map_or(document.visibility, |r| r.visibility),
                };
                let copy_index = MarkdownIndex::new(&copy.markdown)?;
                let copy_options = ReviewOptions {
                    user_response: None,
                    assessment_date: Some(&report.assessment_date),
                    ..options
                };
                let after = analyze(
                    &copy,
                    &copy_index,
                    copy_options,
                    runtime,
                    catalog,
                    &run,
                    live,
                )
                .await;
                report.patch_verification = Some(patch::verify(&report, &after, &applied));
                report.catalog_requests = after.catalog_requests;
                report.catalog_trace.extend(after.catalog_trace);
                report.copy_version = Some(after.version);
            }
            Err(reason) => {
                report.patch_verification = Some(patch::Verification::Rejected {
                    reasons: vec![reason],
                })
            }
        }
    }
    report.model_attempts = run.attempts();
    Ok(report)
}

async fn analyze<M: Model, T: catalog_http::Transport>(
    document: &Document,
    index: &MarkdownIndex,
    options: ReviewOptions<'_>,
    runtime: &Runtime<M>,
    catalog: &standards::Catalog,
    run: &ReviewRun,
    mut live: Option<&mut catalog_http::CatalogHttp<T>>,
) -> ConsistencyReport {
    let packets = index.packets(options.packet_bytes);
    let mut extracted = ClaimRegistry::default();
    let table_claims = table_rows::claims(index);
    let mut seen = BTreeSet::new();
    let assessment_date = options
        .assessment_date
        .map(str::to_owned)
        .unwrap_or_else(|| chrono::Utc::now().date_naive().to_string());
    let mut report = ConsistencyReport {
        run_id: run.id().to_owned(),
        assessment_date: assessment_date.clone(),
        assumed_assessment_date: options.assessment_date.is_none(),
        version: index.version.clone(),
        parsing_gaps: index.gaps.clone(),
        oversized_blocks: packets.oversized,
        packets: Vec::new(),
        claims: Vec::new(),
        priority_mentions: Vec::new(),
        initial_candidates: Vec::new(),
        investigations: Vec::new(),
        selections: Vec::new(),
        response_target_matches: None,
        standard_usage: Vec::new(),
        missing_standard_materials: standards::missing_materials(index),
        recovered_standard_mentions: Vec::new(),
        standard_coverage: StandardCoverage::default(),
        standard_material_questions: standards::missing_materials(index)
            .into_iter()
            .map(|reference| ObjectQuestion {
                reference,
                alternatives: vec![],
                question: "Предоставьте указанное приложение для проверки стандартов.".into(),
            })
            .collect(),
        object_questions: Vec::new(),
        semantic_gaps: Vec::new(),
        standard_references: Vec::new(),
        model_attempts: 0,
        patches: Vec::new(),
        patch_verification: None,
        copy_version: None,
        catalog_trace: Vec::new(),
        catalog_requests: 0,
    };
    let mut semantic_cache = std::collections::BTreeMap::new();
    let mut final_candidates = Vec::new();
    for control_pass in [false, true] {
        let initial_registry = extracted.claims().to_vec();
        let mut refinements = Vec::new();
        let mut queue: std::collections::VecDeque<_> =
            packets.packets.iter().map(|p| p.subjects.clone()).collect();
        while let Some(subjects) = queue.pop_front() {
            let packet = index.packet_for(&subjects);
            let mut coverage = PacketCoverage {
                subjects: packet.subjects.clone(),
                control_pass,
                stop: None,
                binding_issues: Vec::new(),
            };
            let response = if control_pass {
                match claims::extract_control(
                    runtime,
                    run,
                    index,
                    &packet,
                    document.visibility,
                    &initial_registry,
                    options.packet_bytes,
                )
                .await
                {
                    Ok(outcome) => {
                        refinements.extend(outcome.refinements);
                        report
                            .recovered_standard_mentions
                            .extend(outcome.standard_mentions);
                        Ok(outcome.batch)
                    }
                    Err(crate::runtime::StepError::Preparation) if subjects.len() > 1 => {
                        let (left, right) = subjects.split_at(subjects.len() / 2);
                        queue.push_front(right.to_vec());
                        queue.push_front(left.to_vec());
                        continue;
                    }
                    Err(error) => Err(error),
                }
            } else {
                claims::extract(runtime, run, index, &packet, document.visibility, false).await
            };
            match response {
                Ok(batch) => {
                    seen.extend(packet.subjects.iter().copied());
                    extracted.insert_batch(batch.claims);
                    for rule in batch.priority_rules {
                        if !report.priority_mentions.contains(&rule) {
                            report.priority_mentions.push(rule);
                        }
                    }
                    coverage.binding_issues = batch.issues;
                }
                Err(reason) => coverage.stop = Some(format!("{reason:?}")),
            }
            report.packets.push(coverage);
        }
        let issues = extracted.apply_refinements(refinements);
        for issue in issues {
            if let Some(entry) = report
                .packets
                .iter_mut()
                .rev()
                .find(|p| p.subjects.contains(&issue.quote.block_id))
            {
                entry.binding_issues.push(issue);
            }
        }
        // The control packet indexes the model's own extraction. Merge locally verified
        // table facts for comparisons after that pass has resolved those indices.
        let mut registry = extracted.clone();
        registry.insert_batch(
            table_claims
                .iter()
                .filter(|claim| {
                    let Constraint::Numeric {
                        value,
                        unit: Some(unit),
                        ..
                    } = &claim.constraint
                    else {
                        return false;
                    };
                    [&claim.statement, &claim.property, value, unit]
                        .into_iter()
                        .chain(&claim.object.evidence)
                        .chain(&claim.stage)
                        .all(|source| seen.contains(&source.evidence.block_id))
                })
                .cloned(),
        );
        let mut candidates = numeric_candidates(&registry);
        let mut objects = std::collections::BTreeMap::<_, Vec<_>>::new();
        for claim in registry.claims().iter().filter(|c| c.complete) {
            if let Some(object) = &claim.object.key {
                objects.entry(object).or_default().push(claim.clone());
            }
        }
        for (object, claims) in objects {
            if claims.len() < 2 {
                continue;
            }
            let key = serde_json::to_vec(&claims).expect("serializable claims");
            if !semantic_cache.contains_key(&key) {
                let mut parsed = Vec::new();
                if claims
                    .iter()
                    .any(|c| matches!(c.constraint, Constraint::Semantic { .. }))
                {
                    match semantic::generate(
                        runtime,
                        run,
                        index,
                        document.visibility,
                        &claims,
                        options.packet_bytes,
                    )
                    .await
                    {
                        Ok(out) => {
                            parsed.extend(out.candidates);
                            report
                                .semantic_gaps
                                .extend(out.issues.into_iter().map(|msg| (object.clone(), msg)));
                        }
                        Err(e) => report
                            .semantic_gaps
                            .push((object.clone(), format!("semantic comparison: {e:?}"))),
                    }
                }
                if relations::relevant(&claims) {
                    match relations::generate(
                        runtime,
                        run,
                        index,
                        document.visibility,
                        &claims,
                        options.packet_bytes,
                    )
                    .await
                    {
                        Ok(out) => {
                            parsed.extend(out.candidates);
                            report
                                .semantic_gaps
                                .extend(out.issues.into_iter().map(|msg| (object.clone(), msg)));
                        }
                        Err(e) => report
                            .semantic_gaps
                            .push((object.clone(), format!("related requirements: {e:?}"))),
                    }
                }
                semantic_cache.insert(key.clone(), parsed);
            }
            candidates.extend(semantic_cache[&key].iter().cloned());
        }
        candidates.sort_by_key(|c| c.evidence[0].range.start);
        if control_pass {
            final_candidates = candidates;
            report.claims = registry.claims().to_vec();
        } else {
            report.initial_candidates = candidates;
        }
    }
    let mut scan = standards::scan(index);
    for recovered in &report.recovered_standard_mentions {
        if let Some(mention) = standards::recovered(index, recovered)
            && !scan.mentions.iter().any(|m| m.evidence == mention.evidence)
        {
            scan.mentions.push(mention);
        }
    }
    scan.mentions.sort_by_key(|m| m.evidence.range.start);
    report.standard_references = catalog.review(&scan, &assessment_date);
    if let Some(client) = live.as_mut() {
        for row in &mut report.standard_references {
            let lookup = client.lookup(document.visibility, &row.mention.key).await;
            if !lookup.catalog.cards.is_empty() {
                let one = standards::Scan {
                    mentions: vec![row.mention.clone()],
                    unread_ranges: vec![],
                };
                if let Some(updated) = lookup.catalog.review(&one, &assessment_date).pop() {
                    *row = updated;
                }
            } else if matches!(lookup.outcome, catalog_http::Outcome::NotFound)
                && row.cards.is_empty()
            {
                row.existence = standards::Existence::NotFoundInOfficialSearch;
            }
            report.catalog_trace.push(lookup);
        }
        report.catalog_requests = client.total_requests();
    }
    for reference in &report.standard_references {
        report.standard_usage.push(
            standard_usage::evaluate(index, reference, catalog, runtime, run, document.visibility)
                .await,
        );
    }
    let failed: BTreeSet<_> = report
        .packets
        .iter()
        .filter(|p| p.stop.is_some() || !p.binding_issues.is_empty())
        .flat_map(|p| p.subjects.iter().copied())
        .chain(report.oversized_blocks.iter().copied())
        .collect();
    let allowed: Vec<_> = index
        .blocks
        .iter()
        .filter(|b| index.has_readable_text(b.id))
        .map(|b| (b.id, document.visibility))
        .collect();
    for mut candidate in final_candidates {
        for e in candidate.evidence.iter().chain(&candidate.context) {
            let block = &index.blocks[e.block_id];
            candidate.known_gaps.extend(
                block
                    .table_context
                    .iter()
                    .chain(&block.headings)
                    .chain([&block.id])
                    .copied()
                    .filter(|id| failed.contains(id)),
            );
        }
        // A failed later block that quotes this requirement may qualify it.
        for claim in report
            .claims
            .iter()
            .filter(|c| candidate.evidence.contains(&c.statement.evidence))
        {
            candidate
                .known_gaps
                .extend(failed.iter().copied().filter(|&id| {
                    id > claim.statement.evidence.block_id
                        && index.resolve(id, &claim.statement.text).is_some()
                }));
        }
        candidate.known_gaps.sort_unstable();
        candidate.known_gaps.dedup();
        // Empty cells locate a row but contain no evidence for a critic to read.
        candidate
            .known_gaps
            .retain(|id| index.has_readable_text(*id));
        report.investigations.push(
            investigate_bounded(
                runtime,
                run,
                index,
                document.visibility,
                candidate,
                if options.allow_read { &allowed } else { &[] },
                investigation::InvestigationLimits {
                    read_bytes: options.read_bytes,
                    context_bytes: Some(options.packet_bytes),
                },
            )
            .await,
        );
    }
    for claim in &report.claims {
        if claim.object.key.is_none() && claim.object.alternatives.len() >= 2 {
            let options: Vec<_> = claim
                .object
                .alternatives
                .iter()
                .map(|item| item.text.as_str())
                .collect();
            report.object_questions.push(ObjectQuestion {
                reference: claim.statement.clone(),
                alternatives: claim.object.alternatives.clone(),
                question: format!(
                    "К какому объекту относится требование «{}»: {}?",
                    claim.statement.text,
                    options.join(" или ")
                ),
            });
        }
    }
    let applicable = |inv: &Investigation| {
        options
            .user_response
            .is_some_and(|r| r.target == priority::target(index, inv))
    };
    let matches = report
        .investigations
        .iter()
        .filter(|inv| applicable(inv))
        .count();
    report.response_target_matches = options.user_response.map(|_| matches);
    let incomplete_choice = !failed.is_empty() || !report.parsing_gaps.is_empty();
    for inv in &report.investigations {
        let response = options
            .user_response
            .filter(|_| applicable(inv) && matches == 1);
        let choice = priority::select(
            runtime,
            run,
            inv,
            response,
            priority::ChoiceContext {
                index,
                visibility: document.visibility,
                claims: &report.claims,
                mentions: &report.priority_mentions,
                assessment_date: Some(&assessment_date),
                selection_read_error: incomplete_choice.then_some("incomplete source coverage"),
            },
        )
        .await;
        report.selections.push((inv.candidate.id.clone(), choice));
    }
    report.model_attempts = run.attempts();
    report.standard_coverage = report.calculate_standard_coverage();
    report
}

fn numeric_candidates(registry: &ClaimRegistry) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for ((object, property), group) in registry.groups() {
        let mut by_conditions = std::collections::BTreeMap::new();
        for claim in group.into_iter().filter(|c| c.complete) {
            let conditions: BTreeSet<_> = claim
                .conditions
                .iter()
                .map(|c| c.text.trim().to_lowercase())
                .collect();
            by_conditions
                .entry(conditions)
                .or_insert_with(Vec::new)
                .push(claim);
        }
        for group in by_conditions.values() {
            for hypothesis in numeric::hypotheses(group) {
                let mut evidence = Vec::new();
                let mut context = Vec::new();
                for i in hypothesis.claims {
                    let claim = group[i];
                    evidence.push(claim.statement.evidence.clone());
                    context.push(claim.property.evidence.clone());
                    context.extend(
                        claim
                            .object
                            .evidence
                            .iter()
                            .chain(&claim.conditions)
                            .chain(&claim.stage)
                            .chain(&claim.mandatory)
                            .map(|g| g.evidence.clone()),
                    );
                    if let Constraint::Numeric {
                        operator_evidence,
                        value,
                        unit,
                        ..
                    } = &claim.constraint
                    {
                        evidence
                            .extend([operator_evidence.evidence.clone(), value.evidence.clone()]);
                        evidence.extend(unit.iter().map(|u| u.evidence.clone()));
                    }
                }
                let order = |e: &Evidence| (e.range.start, e.range.end, e.block_id);
                evidence.sort_by_key(order);
                evidence.dedup();
                context.sort_by_key(order);
                context.dedup();
                context.retain(|e| !evidence.contains(e));
                let id = serde_json::to_string(&(&object, &property, &evidence))
                    .expect("serializable evidence");
                use sha2::{Digest, Sha256};
                candidates.push(Candidate {
                    id: format!("{:x}", Sha256::digest(id)),
                    category: "numeric".into(),
                    object: object.clone(),
                    property: property.clone(),
                    evidence,
                    context,
                    known_gaps: vec![],
                });
            }
        }
    }
    candidates.sort_by_key(|candidate| candidate.evidence[0].range.start);
    candidates
}
