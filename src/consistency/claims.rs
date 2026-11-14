use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::markdown::{Evidence, MarkdownIndex};

/// A model-supplied quotation. The address is assigned only after local validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Quote {
    pub block_id: usize,
    pub quote: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObjectProposal {
    /// Exact object phrase from evidence; prefer the phrase in the statement in its original form.
    pub key: String,
    pub evidence: Vec<Quote>,
    /// Genuinely possible antecedents only; do not guess when a reference is ambiguous.
    pub alternatives: Vec<Quote>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstraintProposal {
    Numeric {
        operator: Quote,
        value: Quote,
        unit: Option<Quote>,
    },
    Semantic {
        text: Quote,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimProposal {
    pub statement: Quote,
    pub object: ObjectProposal,
    /// Exact phrase naming the property, distinct from the object and its value.
    pub property: Quote,
    pub constraint: ConstraintProposal,
    /// Only an explicit restriction of applicability to a stage.
    pub stage: Option<Quote>,
    /// Explicit applicability conditions, excluding general priority rules.
    pub conditions: Vec<Quote>,
    pub mandatory: Option<Quote>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimBatch {
    pub claims: Vec<ClaimProposal>,
    pub priority_rules: Vec<Quote>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Grounded {
    pub text: String,
    pub evidence: Evidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Operator {
    /// Exact nominal established by locally verified table structure, not text extraction.
    TableNominal,
    Exact,
    AtLeast,
    MoreThan,
    AtMost,
    LessThan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Constraint {
    Numeric {
        operator: Operator,
        operator_evidence: Grounded,
        value: Grounded,
        unit: Option<Grounded>,
    },
    Semantic {
        text: Grounded,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Object {
    /// None when a reference has multiple possible antecedents or no verified definition.
    pub key: Option<String>,
    pub evidence: Vec<Grounded>,
    pub alternatives: Vec<Grounded>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Claim {
    /// False when any proposed field or object binding could not be verified.
    pub complete: bool,
    pub statement: Grounded,
    pub object: Object,
    pub property: Grounded,
    pub constraint: Constraint,
    pub stage: Option<Grounded>,
    pub conditions: Vec<Grounded>,
    pub mandatory: Option<Grounded>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BindingIssue {
    pub claim_index: Option<usize>,
    pub field: String,
    pub quote: Quote,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BatchOutcome {
    pub claims: Vec<Claim>,
    pub priority_rules: Vec<Grounded>,
    pub issues: Vec<BindingIssue>,
}

/// Resolve only quotations from the packet actually supplied to the model.
pub fn validate_batch(
    index: &MarkdownIndex,
    trusted_block_ids: &[usize],
    batch: ClaimBatch,
) -> BatchOutcome {
    validate_batch_linked(index, trusted_block_ids, batch, &[])
}

fn validate_batch_linked(
    index: &MarkdownIndex,
    trusted_block_ids: &[usize],
    batch: ClaimBatch,
    linked: &[usize],
) -> BatchOutcome {
    let trusted: BTreeSet<_> = trusted_block_ids.iter().copied().collect();
    let mut outcome = BatchOutcome::default();
    for (i, proposal) in batch.claims.into_iter().enumerate() {
        let issue_start = outcome.issues.len();
        let mut delayed_issues = Vec::new();
        let mut rows = None::<(usize, std::ops::Range<usize>)>;
        let mut bind =
            |field: &str, quote: Quote, statement: Option<&Grounded>| -> Option<Grounded> {
                let evidence = trusted
                    .contains(&quote.block_id)
                    .then(|| match statement {
                        Some(s) => {
                            index.resolve_scoped(quote.block_id, &quote.quote, &s.text, &s.evidence)
                        }
                        None => index.resolve(quote.block_id, &quote.quote),
                    })
                    .flatten()
                    .filter(|e| {
                        statement.is_none_or(|s| {
                            e.block_id != s.evidence.block_id
                                || (s.evidence.range.start <= e.range.start
                                    && e.range.end <= s.evidence.range.end)
                        })
                    })
                    .filter(|e| {
                        if field == "statement" {
                            rows = index.blocks[e.block_id]
                                .table
                                .as_ref()
                                .map(|t| (t.id, t.rows.clone()));
                            return true;
                        }
                        let Some(s) = statement else { return true };
                        let Some((table, range)) = &mut rows else {
                            return true;
                        };
                        if !matches!(field, "property" | "operator" | "value" | "unit") {
                            return true;
                        }
                        if matches!(field, "property" | "unit")
                            && index
                                .table_column_ids(s.evidence.block_id)
                                .contains(&e.block_id)
                        {
                            return true;
                        }
                        let Some(cell) = &index.blocks[e.block_id].table else {
                            return false;
                        };
                        if cell.id != *table
                            || cell.rows.start >= range.end
                            || range.start >= cell.rows.end
                        {
                            return false;
                        }
                        // All fields must share one physical row, including when the statement spans rows.
                        *range = range.start.max(cell.rows.start)..range.end.min(cell.rows.end);
                        true
                    });
                match evidence {
                    Some(evidence) => Some(Grounded {
                        text: quote.quote,
                        evidence,
                    }),
                    None => {
                        outcome.issues.push(BindingIssue {
                            claim_index: Some(i),
                            field: field.into(),
                            quote,
                            reason: "quote is missing, ambiguous, or outside the supplied packet"
                                .into(),
                        });
                        None
                    }
                }
            };
        let statement = bind("statement", proposal.statement, None);
        let property = bind("property", proposal.property, statement.as_ref());
        let evidence: Vec<_> = proposal
            .object
            .evidence
            .into_iter()
            .filter_map(|q| bind("object", q, statement.as_ref()))
            .collect();
        let alternatives: Vec<_> = proposal
            .object
            .alternatives
            .into_iter()
            .filter_map(|q| bind("object.alternatives", q, statement.as_ref()))
            .collect();
        let key = proposal.object.key.trim().to_lowercase();
        // A normalized label must occur in a cited definition; an arbitrary model label is not evidence.
        let object_key = (!key.is_empty()
            && alternatives.is_empty()
            && statement.as_ref().is_some_and(|s| {
                evidence
                    .iter()
                    .filter(|e| e.text.trim().to_lowercase() == key)
                    .any(|e| {
                        e.evidence.block_id == s.evidence.block_id
                            || (linked.contains(&e.evidence.block_id)
                                && index.resolve(e.evidence.block_id, &s.text).is_some()
                                && index.resolve(e.evidence.block_id, &s.text).is_some_and(
                                    |quote| {
                                        e.evidence.range.end <= quote.range.start
                                            || e.evidence.range.start >= quote.range.end
                                    },
                                ))
                            || index.blocks[s.evidence.block_id]
                                .headings
                                .contains(&e.evidence.block_id)
                            || index.blocks[s.evidence.block_id]
                                .table_context
                                .contains(&e.evidence.block_id)
                            || (e.evidence.block_id.checked_add(1) == Some(s.evidence.block_id)
                                && matches!(
                                    index.blocks[e.evidence.block_id].kind,
                                    super::markdown::BlockKind::Paragraph
                                )
                                && matches!(
                                    index.blocks[s.evidence.block_id].kind,
                                    super::markdown::BlockKind::Paragraph
                                )
                                && evidence.iter().any(|reference| {
                                    let text = reference.text.trim().to_lowercase();
                                    reference.evidence.block_id == s.evidence.block_id
                                        && s.evidence.range.start <= reference.evidence.range.start
                                        && reference.evidence.range.end <= s.evidence.range.end
                                        && ((matches!(
                                            text.as_str(),
                                            "он" | "она"
                                                | "оно"
                                                | "его"
                                                | "её"
                                                | "ее"
                                                | "ему"
                                                | "ней"
                                                | "него"
                                                | "этот"
                                                | "эта"
                                                | "это"
                                                | "данный"
                                                | "данная"
                                                | "данное"
                                        ) && index.source
                                            [index.blocks[e.evidence.block_id].range.clone()]
                                        .trim()
                                        .trim_end_matches(['.', '!', '?'])
                                        .trim()
                                        .eq_ignore_ascii_case(&e.text))
                                            || (key.contains(&text)
                                                && text.chars().any(|c| c.is_alphabetic())
                                                && text.chars().any(|c| c.is_ascii_digit())))
                                }))
                    })
            }))
        .then_some(key);
        if object_key.is_none() {
            delayed_issues.push(BindingIssue {
                claim_index: Some(i),
                field: "object.key".into(),
                quote: Quote {
                    block_id: statement
                        .as_ref()
                        .map_or(usize::MAX, |s| s.evidence.block_id),
                    quote: proposal.object.key,
                },
                reason: "object is ambiguous or its key is not supported by a cited definition"
                    .into(),
            });
        }
        let constraint = match proposal.constraint {
            ConstraintProposal::Numeric {
                operator,
                value,
                unit,
            } => {
                let op = bind("operator", operator, statement.as_ref());
                let value = bind("value", value, statement.as_ref());
                let unit = unit.map(|q| bind("unit", q, statement.as_ref()));
                match (op, value, unit) {
                    (Some(op), Some(value), unit) if unit.as_ref().is_none_or(Option::is_some) => {
                        match (
                            parse_operator(&op, &value, statement.as_ref()),
                            super::numeric::parse_decimal(&value.text),
                        ) {
                            (Some(operator), Some(_)) => Some(Constraint::Numeric {
                                operator,
                                operator_evidence: op,
                                value,
                                unit: unit.flatten(),
                            }),
                            _ => {
                                delayed_issues.push(BindingIssue {
                                    claim_index: Some(i),
                                    field: "constraint".into(),
                                    quote: Quote {
                                        block_id: op.evidence.block_id,
                                        quote: op.text,
                                    },
                                    reason: "unsupported operator or invalid number".into(),
                                });
                                None
                            }
                        }
                    }
                    _ => None,
                }
            }
            ConstraintProposal::Semantic { text } => {
                bind("semantic", text, statement.as_ref()).map(|text| Constraint::Semantic { text })
            }
        };
        let stage = proposal
            .stage
            .and_then(|q| bind("stage", q, statement.as_ref()));
        let conditions = proposal
            .conditions
            .into_iter()
            .filter_map(|q| bind("conditions", q, statement.as_ref()))
            .collect();
        let mandatory = proposal
            .mandatory
            .and_then(|q| bind("mandatory", q, statement.as_ref()));
        outcome.issues.extend(delayed_issues);
        if let (Some(statement), Some(property), Some(constraint)) =
            (statement, property, constraint)
        {
            outcome.claims.push(Claim {
                complete: outcome.issues.len() == issue_start && object_key.is_some(),
                statement,
                object: Object {
                    key: object_key,
                    evidence,
                    alternatives,
                },
                property,
                constraint,
                stage,
                conditions,
                mandatory,
            });
        }
    }
    for quote in batch.priority_rules {
        match trusted
            .contains(&quote.block_id)
            .then(|| index.resolve(quote.block_id, &quote.quote))
            .flatten()
        {
            Some(evidence) => outcome.priority_rules.push(Grounded {
                text: quote.quote,
                evidence,
            }),
            None => outcome.issues.push(BindingIssue {
                claim_index: None,
                field: "priority_rule".into(),
                quote,
                reason: "quote is missing, ambiguous, or outside the supplied packet".into(),
            }),
        }
    }
    outcome
}

fn parse_operator(
    op: &Grounded,
    value: &Grounded,
    statement: Option<&Grounded>,
) -> Option<Operator> {
    let text = op.text.trim().to_lowercase();
    match text.as_str() {
        "=" | "ровно" | "точно" | "exactly" => Some(Operator::Exact),
        ">=" | "≥" | "не менее" | "не ранее" | "минимум" | "at least" => {
            Some(Operator::AtLeast)
        }
        ">" | "более" | "greater than" => Some(Operator::MoreThan),
        "<=" | "≤" | "не более" | "не позднее" | "максимум" | "at most" => {
            Some(Operator::AtMost)
        }
        "<" | "менее" | "less than" => Some(Operator::LessThan),
        "от" | "до" => {
            let statement = statement?;
            let contains = |g: &Grounded| {
                g.evidence.block_id == statement.evidence.block_id
                    && statement.evidence.range.start <= g.evidence.range.start
                    && g.evidence.range.end <= statement.evidence.range.end
            };
            if !contains(op) || !contains(value) {
                return None;
            }
            let words: Vec<_> = statement.text.split_whitespace().collect();
            // ponytail: only the explicit four-token range is supported; broaden on real inputs.
            words.windows(4).find_map(|range| {
                if range[0].eq_ignore_ascii_case("от")
                    && range[2].eq_ignore_ascii_case("до")
                    && super::numeric::parse_decimal(range[1]).is_some()
                    && super::numeric::parse_decimal(range[3].trim_end_matches(['.', ',', ';']))
                        .is_some()
                {
                    let starts_at = |word: &str, grounded: &Grounded| {
                        statement.evidence.range.start
                            + (word.as_ptr() as usize - statement.text.as_ptr() as usize)
                            == grounded.evidence.range.start
                    };
                    match (text.as_str(), value.text.as_str()) {
                        ("от", v)
                            if v == range[1]
                                && starts_at(range[0], op)
                                && starts_at(range[1], value) =>
                        {
                            Some(Operator::AtLeast)
                        }
                        ("до", v)
                            if v == range[3].trim_end_matches(['.', ',', ';'])
                                && starts_at(range[2], op)
                                && starts_at(range[3], value) =>
                        {
                            Some(Operator::AtMost)
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            })
        }
        _ => None,
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClaimRegistry {
    claims: Vec<Claim>,
    by_property: BTreeMap<(String, String), Vec<usize>>,
}

impl ClaimRegistry {
    /// Replace only a uniquely identified existing claim with an independently grounded refinement.
    pub fn apply_refinements(&mut self, refinements: Vec<ClaimRefinement>) -> Vec<BindingIssue> {
        let snapshot = self.claims.clone();
        let mut issues = Vec::new();
        let mut grouped: BTreeMap<usize, Vec<Claim>> = BTreeMap::new();
        for refinement in refinements {
            let matches: Vec<_> = snapshot
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c.statement.evidence == refinement.target.statement.evidence
                        && c.property.evidence == refinement.target.property.evidence
                        && c.constraint == refinement.target.constraint
                })
                .map(|(i, _)| i)
                .collect();
            if matches.len() == 1
                && (snapshot[matches[0]] == refinement.target
                    || snapshot[matches[0]] == refinement.replacement)
                && refinement.replacement.complete
                && refinement.replacement.statement.evidence == refinement.target.statement.evidence
            {
                grouped
                    .entry(matches[0])
                    .or_default()
                    .push(refinement.replacement);
            } else {
                issues.push(BindingIssue {
                    claim_index: None,
                    field: "refinement".into(),
                    quote: Quote { block_id: refinement.target.statement.evidence.block_id, quote: refinement.target.statement.text },
                    reason: "refinement must identify one existing claim and preserve its anchored statement".into(),
                });
            }
        }
        for (id, mut replacements) in grouped {
            replacements.sort_by_key(|c| serde_json::to_string(c).expect("claim serializes"));
            replacements.dedup();
            let mut merged = replacements[0].clone();
            // Distinct condition sets may contradict each other; only a proposed superset
            // demonstrates that all conditions were accepted together.
            let compatible = replacements.iter().any(|superset| {
                replacements.iter().all(|candidate| {
                    candidate
                        .conditions
                        .iter()
                        .all(|c| superset.conditions.contains(c))
                })
            }) && replacements.iter().all(|candidate| {
                let mut without_conditions = candidate.clone();
                without_conditions.conditions = merged.conditions.clone();
                without_conditions == merged
                    && snapshot[id]
                        .conditions
                        .iter()
                        .all(|c| candidate.conditions.contains(c))
            });
            if replacements.len() == 1 || compatible {
                for candidate in replacements.into_iter().skip(1) {
                    merge_grounded(&mut merged.conditions, candidate.conditions);
                }
                self.claims[id] = merged;
            } else {
                issues.push(BindingIssue {
                    claim_index: Some(id),
                    field: "refinement".into(),
                    quote: Quote {
                        block_id: snapshot[id].statement.evidence.block_id,
                        quote: snapshot[id].statement.text.clone(),
                    },
                    reason: "conflicting refinements for the same claim".into(),
                });
            }
        }
        self.index_groups();
        issues
    }
    pub fn insert_batch(&mut self, claims: impl IntoIterator<Item = Claim>) {
        self.claims.extend(claims);
        self.claims.sort_by_key(|c| {
            (
                claim_key(c),
                serde_json::to_string(c).expect("claim serializes"),
            )
        });
        let mut merged: Vec<Claim> = Vec::new();
        for claim in self.claims.drain(..) {
            if let Some(previous) = merged
                .last_mut()
                .filter(|p| claim_key(p) == claim_key(&claim))
            {
                if previous.object.key != claim.object.key
                    || previous.stage != claim.stage
                    || previous.conditions != claim.conditions
                    || previous.mandatory != claim.mandatory
                {
                    previous.complete = false;
                }
                previous.complete &= claim.complete;
                merge_grounded(&mut previous.object.evidence, claim.object.evidence);
                merge_grounded(&mut previous.object.alternatives, claim.object.alternatives);
                merge_grounded(&mut previous.conditions, claim.conditions);
            } else {
                merged.push(claim);
            }
        }
        self.claims = merged;
        self.index_groups();
    }
    pub fn claims(&self) -> &[Claim] {
        &self.claims
    }
    pub fn group(&self, object: &str, property: &str) -> Vec<&Claim> {
        self.by_property
            .get(&(object.to_lowercase(), property.to_lowercase()))
            .into_iter()
            .flatten()
            .map(|id| &self.claims[*id])
            .collect()
    }
    pub fn groups(&self) -> BTreeMap<(String, String), Vec<&Claim>> {
        self.by_property
            .iter()
            .map(|(key, ids)| {
                (
                    key.clone(),
                    ids.iter().map(|id| &self.claims[*id]).collect(),
                )
            })
            .collect()
    }
    fn index_groups(&mut self) {
        self.by_property.clear();
        for (id, claim) in self.claims.iter().enumerate() {
            if let Some(object) = &claim.object.key {
                self.by_property
                    .entry((object.clone(), claim.property.text.to_lowercase()))
                    .or_default()
                    .push(id);
            }
        }
    }
}

fn merge_grounded(into: &mut Vec<Grounded>, values: Vec<Grounded>) {
    into.extend(values);
    into.sort_by_key(|g| {
        (
            g.evidence.version.clone(),
            g.evidence.range.start,
            g.evidence.range.end,
            g.text.clone(),
        )
    });
    into.dedup();
}

fn claim_key(c: &Claim) -> (String, usize, usize, String, String) {
    let constraint = match &c.constraint {
        Constraint::Numeric {
            operator,
            value,
            unit,
            ..
        } => format!(
            "{operator:?}:{}:{}",
            value.text,
            unit.as_ref().map_or("", |v| &v.text)
        ),
        Constraint::Semantic { text } => text.text.clone(),
    };
    (
        c.statement.evidence.version.clone(),
        c.statement.evidence.range.start,
        c.statement.evidence.range.end,
        c.property.text.to_lowercase(),
        constraint,
    )
}

pub(super) fn strict_schema<T: JsonSchema>() -> schemars::Schema {
    fn strict(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                object.remove("$schema");
                object.remove("format");
                object.remove("minimum");
                if let Some(value) = object.remove("const") {
                    object.insert("enum".into(), serde_json::json!([value]));
                }
                if let Some(properties) = object.get("properties").and_then(|v| v.as_object()) {
                    let required = properties
                        .keys()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect();
                    object.insert("required".into(), serde_json::Value::Array(required));
                    object.insert("additionalProperties".into(), false.into());
                }
                if let Some(one_of) = object.remove("oneOf") {
                    object.insert("anyOf".into(), one_of);
                }
                for child in object.values_mut() {
                    strict(child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    strict(item);
                }
            }
            _ => {}
        }
    }
    let mut schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    strict(&mut schema);
    serde_json::from_value(schema).unwrap()
}

const EXTRACTION_INSTRUCTION: &str = r#"Extract requirements from subject blocks only. Return JSON with claims and priority_rules arrays. Each quote is {"block_id": number, "quote": "exact text from one transmitted block"}; never invent or paraphrase quotes. Claim: {"statement": quote, "object": {"key": "full exact text of one object definition quote", "evidence": [quote], "alternatives": [quote]}, "property": quote, "constraint": {"kind":"numeric", "operator":quote, "value":quote, "unit":quote or null} or {"kind":"semantic", "text":quote}, "stage":quote or null, "conditions":[quote], "mandatory":quote or null}. Object key must exactly match an object phrase in cited evidence, preserving its original case and grammatical form. Prefer the object phrase explicitly named within the statement; use a heading only when the object is not named in the statement. Property must quote the phrase naming the characteristic, distinct from the object and its value; do not guess an unstated property. Alternatives must be empty unless there are genuinely different possible antecedents; headings are not alternatives. Do not guess an ambiguous object or property. Conditions contain only explicit applicability conditions, not general priority rules. Quote priority rules separately. Set stage only when applicability is explicitly restricted to that stage, not merely because a heading names a stage. Cite object definitions and references, units and conditions separately when present. Mandatory must quote the shortest verbatim explicit modal, such as "должна"; never discard an explicit modal. For an object defined in the preceding paragraph, cite both the exact definition and the numbered reference or pronoun in the statement; do not bind an ambiguous reference to a guessed object. For table cells, use table.rows and table.columns to identify the same physical row and its column header. Keep work quantity separate from an equipment nominal; cite units from the same row or the addressed column header. Numeric operator must be an explicit operator quote; value must contain only the number. For an explicit range written от N до M return two numeric claims with the full range in each statement and separate operator and value quotes. A reference with multiple possible objects has alternatives and no confirmed key. Priority rules must be quoted. Context blocks may support fields but the statement must be from a subject. Omit unsupported claims; do not infer facts from the absence of text."#;

/// Run one extraction stage; a control pass uses the same schema and independent input.
pub async fn extract<M: crate::runtime::grok::Model>(
    runtime: &crate::runtime::Runtime<M>,
    run: &crate::runtime::ReviewRun,
    index: &MarkdownIndex,
    packet: &super::batches::Packet<'_>,
    visibility: crate::core::Visibility,
    control: bool,
) -> Result<BatchOutcome, crate::runtime::StepError> {
    if packet.version != index.version {
        return Err(crate::runtime::StepError::Preparation);
    }
    let sources = [visibility];
    let request = crate::runtime::StageRequest::new(
        if control {
            crate::runtime::StageKind::Control
        } else {
            crate::runtime::StageKind::Claims
        },
        EXTRACTION_INSTRUCTION,
    )
    .schema(strict_schema::<ClaimBatch>())
    .field("packet", &sources, packet)
    .field("control_pass", &sources, &control);
    let mut batch: ClaimBatch = runtime.step(run, &request).await?;
    let mut rejected = Vec::new();
    batch.claims.retain(|claim| {
        if packet.subjects.contains(&claim.statement.block_id) {
            true
        } else {
            rejected.push(BindingIssue {
                claim_index: None,
                field: "statement".into(),
                quote: claim.statement.clone(),
                reason: "statement is not in a subject block".into(),
            });
            false
        }
    });
    let trusted: Vec<_> = packet.blocks.iter().map(|block| block.id).collect();
    let mut outcome = validate_batch(index, &trusted, batch);
    outcome.issues.extend(rejected);
    Ok(outcome)
}

#[derive(Clone, Debug, Serialize)]
pub struct ClaimRefinement {
    pub target: Claim,
    pub replacement: Claim,
}

#[derive(Debug, Default, Serialize)]
pub struct ControlOutcome {
    pub batch: BatchOutcome,
    pub refinements: Vec<ClaimRefinement>,
    pub standard_mentions: Vec<Grounded>,
}

#[derive(Deserialize, JsonSchema)]
struct ControlResponse {
    claims: Vec<ClaimProposal>,
    priority_rules: Vec<Quote>,
    refinements: Vec<ControlRefinement>,
    standard_mentions: Vec<Quote>,
}

#[derive(Deserialize, JsonSchema)]
struct ControlRefinement {
    claim_index: usize,
    replacement: ClaimProposal,
}

/// Read subjects independently and accept replacements only for claims in this packet.
pub async fn extract_control<M: crate::runtime::grok::Model>(
    runtime: &crate::runtime::Runtime<M>,
    run: &crate::runtime::ReviewRun,
    index: &MarkdownIndex,
    packet: &super::batches::Packet<'_>,
    visibility: crate::core::Visibility,
    existing: &[Claim],
    max_bytes: usize,
) -> Result<ControlOutcome, crate::runtime::StepError> {
    use crate::runtime::{StageKind, StageRequest, StepError};
    if packet.version != index.version {
        return Err(StepError::Preparation);
    }
    let linked = |claim: &Claim| -> Vec<usize> {
        packet
            .subjects
            .iter()
            .copied()
            .filter(|&id| {
                id > claim.statement.evidence.block_id
                    && index.resolve(id, &claim.statement.text).is_some()
            })
            .collect()
    };
    let scoped: Vec<_> = existing
        .iter()
        .filter(|c| {
            packet.subjects.contains(&c.statement.evidence.block_id) || !linked(c).is_empty()
        })
        .collect();
    let mut ids = packet.subjects.clone();
    ids.extend(
        scoped
            .iter()
            .filter(|c| !linked(c).is_empty())
            .map(|c| c.statement.evidence.block_id),
    );
    let mut augmented = index.packet_for(&ids);
    augmented.subjects = packet.subjects.clone();
    if serde_json::to_vec(&(&augmented, &scoped))
        .map_err(|_| StepError::Preparation)?
        .len()
        > max_bytes
    {
        return Err(StepError::Preparation);
    }
    let sources = [visibility];
    let request = StageRequest::new(StageKind::Control, concat!(
        "Independently read every subject block, including text before headings. ",
        "Return JSON with claims, priority_rules, refinements and standard_mentions arrays. ",
        "Claims use the extraction schema: statement, object {key,evidence,alternatives}, property, ",
        "constraint {kind:numeric,operator,value,unit} or {kind:semantic,text}, stage, conditions, mandatory. ",
        "Object key must exactly match an object phrase in cited evidence, preserving its original case and grammatical form. Prefer the object phrase explicitly named within the statement; use a heading only when the object is not named in the statement. Property must quote the phrase naming the characteristic, distinct from the object and its value; do not guess an unstated property. Alternatives must be empty unless there are genuinely different possible antecedents; headings are not alternatives. Do not guess an ambiguous object or property. Conditions contain only explicit applicability conditions, not general priority rules. Quote priority rules separately. Set stage only when applicability is explicitly restricted to that stage, not merely because a heading names a stage. Mandatory must quote the shortest verbatim explicit modal, such as должна; never discard an explicit modal. ",
        "Quotes are {block_id,quote}, exact text in the packet. For table cells use table.rows and table.columns to identify the same physical row and its column header. Keep work quantity separate from an equipment nominal; cite units from the same row or the addressed column header. For an explicit range от N до M return two numeric claims, each with the full range in its statement and separate operator and value quotes. ",
        "Each refinement is {claim_index,replacement}, where claim_index indexes existing_claims and replacement is a complete claim with the same statement. ",
        "Refine only when the text supports a correction to object, condition, or eligibility. ",
        "A subject may quote an earlier statement verbatim and specify a correction: cite the earlier statement and quote the corrected object or condition in that subject. ",
        "List omitted standard mentions as quotes. Do not invent facts or use context blocks as subjects."
    )).schema(strict_schema::<ControlResponse>()).field("packet", &sources, &augmented).field("existing_claims", &sources, &scoped);
    let response: ControlResponse = runtime.step(run, &request).await?;
    let trusted: Vec<_> = augmented.blocks.iter().map(|b| b.id).collect();
    let mut result = ControlOutcome::default();
    let mut claims = Vec::new();
    for claim in response.claims {
        if packet.subjects.contains(&claim.statement.block_id) {
            claims.push(claim);
        } else {
            result.batch.issues.push(BindingIssue {
                claim_index: None,
                field: "statement".into(),
                quote: claim.statement,
                reason: "statement is not in a subject block".into(),
            });
        }
    }
    let batch = validate_batch(
        index,
        &trusted,
        ClaimBatch {
            claims,
            priority_rules: response.priority_rules,
        },
    );
    result.batch.claims = batch.claims;
    result.batch.priority_rules = batch.priority_rules;
    result.batch.issues.extend(batch.issues);
    for refinement in response.refinements {
        let Some(target) = scoped.get(refinement.claim_index) else {
            result.batch.issues.push(BindingIssue {
                claim_index: None,
                field: "refinement".into(),
                quote: refinement.replacement.statement,
                reason: "unknown claim index".into(),
            });
            continue;
        };
        let links = linked(target);
        let mut validated = validate_batch_linked(
            index,
            &trusted,
            ClaimBatch {
                claims: vec![refinement.replacement],
                priority_rules: vec![],
            },
            &links,
        );
        result.batch.issues.append(&mut validated.issues);
        if let Some(replacement) = validated.claims.pop() {
            let local = packet
                .subjects
                .contains(&target.statement.evidence.block_id);
            let addressed = |g: &Grounded| {
                links.iter().any(|&id| {
                    g.evidence.block_id == id
                        && index.resolve(id, &target.statement.text).is_some_and(|q| {
                            g.evidence.range.end <= q.range.start
                                || g.evidence.range.start >= q.range.end
                        })
                })
            };
            let remote_supported = !links.is_empty()
                && replacement.property == target.property
                && replacement.constraint == target.constraint
                && replacement.stage == target.stage
                && replacement.mandatory == target.mandatory
                && (replacement.object == target.object
                    || replacement.object.key.as_ref().is_some_and(|key| {
                        replacement
                            .object
                            .evidence
                            .iter()
                            .any(|e| addressed(e) && e.text.trim().to_lowercase() == *key)
                    }))
                && target
                    .conditions
                    .iter()
                    .all(|c| replacement.conditions.contains(c))
                && replacement
                    .conditions
                    .iter()
                    .filter(|c| !target.conditions.contains(c))
                    .all(&addressed)
                && (replacement.object != target.object
                    || replacement.conditions != target.conditions);
            if replacement.complete
                && replacement.statement.evidence == target.statement.evidence
                && ((local && links.is_empty()) || remote_supported)
            {
                result.refinements.push(ClaimRefinement {
                    target: (*target).clone(),
                    replacement,
                });
            } else {
                result.batch.issues.push(BindingIssue {
                    claim_index: None,
                    field: "refinement".into(),
                    quote: Quote {
                        block_id: replacement.statement.evidence.block_id,
                        quote: replacement.statement.text,
                    },
                    reason: "refinement is incomplete or changes the statement".into(),
                });
            }
        }
    }
    for quote in response.standard_mentions {
        match packet
            .subjects
            .contains(&quote.block_id)
            .then(|| index.resolve(quote.block_id, &quote.quote))
            .flatten()
        {
            Some(evidence) => result.standard_mentions.push(Grounded {
                text: quote.quote,
                evidence,
            }),
            None => result.batch.issues.push(BindingIssue {
                claim_index: None,
                field: "standard_mention".into(),
                quote,
                reason: "mention is missing, ambiguous, or outside subject blocks".into(),
            }),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    #[test]
    fn extraction_schemas_require_strict_quote_objects_and_refinements() {
        fn check(value: &serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(properties) = object.get("properties").and_then(|v| v.as_object()) {
                        assert_eq!(
                            object.get("additionalProperties"),
                            Some(&serde_json::json!(false))
                        );
                        let required = object["required"].as_array().unwrap();
                        assert_eq!(required.len(), properties.len());
                        assert!(
                            properties
                                .keys()
                                .all(|key| required.contains(&serde_json::json!(key)))
                        );
                    }
                    for child in object.values() {
                        check(child);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        check(item);
                    }
                }
                _ => {}
            }
        }
        for schema in [
            super::strict_schema::<super::ClaimBatch>(),
            super::strict_schema::<super::ControlResponse>(),
        ] {
            let schema = serde_json::to_value(schema).unwrap();
            check(&schema);
            assert_eq!(
                schema["$defs"]["Quote"]["properties"]["block_id"]["type"],
                "integer"
            );
            assert_eq!(
                schema["$defs"]["Quote"]["properties"]["quote"]["type"],
                "string"
            );
            let claim = &schema["$defs"]["ClaimProposal"]["properties"];
            assert_eq!(claim["stage"]["anyOf"][1]["type"], "null");
            assert_eq!(claim["mandatory"]["anyOf"][1]["type"], "null");
            assert_eq!(
                schema["$defs"]["ConstraintProposal"]["anyOf"][0]["properties"]["unit"]["anyOf"][1]
                    ["type"],
                "null"
            );
            if schema["title"] == "ControlResponse" {
                assert_eq!(
                    schema["$defs"]["ControlRefinement"]["properties"]["replacement"]["$ref"],
                    "#/$defs/ClaimProposal"
                );
            }
        }
    }

    use super::*;

    struct ControlModel(String);
    impl crate::runtime::grok::Model for ControlModel {
        async fn complete(&self, _: &str) -> Result<String, crate::core::ErrorCategory> {
            Ok(self.0.clone())
        }
    }

    fn q(block_id: usize, quote: &str) -> Quote {
        Quote {
            block_id,
            quote: quote.into(),
        }
    }
    fn proposal(block: usize, number: &str) -> ClaimProposal {
        ClaimProposal {
            statement: q(
                block,
                &format!("лифт масса не менее {number} кг обязателен"),
            ),
            object: ObjectProposal {
                key: "лифт".into(),
                evidence: vec![q(block, "лифт")],
                alternatives: vec![],
            },
            property: q(block, "масса"),
            constraint: ConstraintProposal::Numeric {
                operator: q(block, "не менее"),
                value: q(block, number),
                unit: Some(q(block, "кг")),
            },
            stage: None,
            conditions: vec![],
            mandatory: Some(q(block, "обязателен")),
        }
    }
    #[test]
    fn ambiguous_fields_resolve_only_within_unique_statement() {
        let index =
            MarkdownIndex::new("лифт проверен. лифт масса не менее 300 кг обязателен\n\nлифт лифт")
                .unwrap();
        let batch = |claim| {
            validate_batch(
                &index,
                &[0, 1],
                ClaimBatch {
                    claims: vec![claim],
                    priority_rules: vec![],
                },
            )
        };
        let claim = proposal(0, "300");
        let result = batch(claim.clone());
        assert!(result.issues.is_empty());
        let grounded = &result.claims[0].object.evidence[0].evidence.range;
        assert_eq!(&index.source[grounded.clone()], "лифт");
        assert!(grounded.start >= result.claims[0].statement.evidence.range.start);
        assert!(grounded.end <= result.claims[0].statement.evidence.range.end);

        let mut ambiguous = claim.clone();
        ambiguous.statement = q(0, "лифт");
        assert!(batch(ambiguous).claims.is_empty());

        let mut ambiguous = claim.clone();
        ambiguous.statement = q(0, "лифт проверен. лифт масса не менее 300 кг обязателен");
        let result = batch(ambiguous);
        assert!(result.issues.iter().any(|issue| issue.field == "object"));
        assert!(!result.claims[0].complete);

        let mut other_block = claim;
        other_block.conditions.push(q(1, "лифт"));
        let result = batch(other_block);
        assert!(
            result
                .issues
                .iter()
                .any(|issue| issue.field == "conditions")
        );
        assert!(!result.claims[0].complete);
    }

    #[test]
    fn fields_cannot_be_taken_from_another_sentence_but_table_cells_work() {
        let index =
            MarkdownIndex::new("лифт масса. Кран должен иметь массу не менее 300 кг").unwrap();
        let mut claim = proposal(0, "300");
        claim.statement = q(0, "лифт масса");
        claim.mandatory = Some(q(0, "должен"));
        let result = validate_batch(
            &index,
            &[0],
            ClaimBatch {
                claims: vec![claim],
                priority_rules: vec![],
            },
        );
        assert!(result.claims.is_empty());
        for field in ["operator", "value", "mandatory"] {
            assert!(result.issues.iter().any(|issue| issue.field == field));
        }

        let table = MarkdownIndex::new(
            "| Объект | Параметр | Значение |\n|---|---|---|\n| лифт | масса | не менее 300 кг |",
        )
        .unwrap();
        let id = |text: &str| {
            table
                .blocks
                .iter()
                .find(|b| table.source[b.range.clone()].trim() == text)
                .unwrap()
                .id
        };
        let statement = id("не менее 300 кг");
        let claim = ClaimProposal {
            statement: q(statement, "не менее 300 кг"),
            object: ObjectProposal {
                key: "лифт".into(),
                evidence: vec![q(id("лифт"), "лифт")],
                alternatives: vec![],
            },
            property: q(id("масса"), "масса"),
            constraint: ConstraintProposal::Numeric {
                operator: q(statement, "не менее"),
                value: q(statement, "300"),
                unit: Some(q(statement, "кг")),
            },
            stage: None,
            conditions: vec![],
            mandatory: None,
        };
        let trusted: Vec<_> = table.blocks.iter().map(|b| b.id).collect();
        let result = validate_batch(
            &table,
            &trusted,
            ClaimBatch {
                claims: vec![claim],
                priority_rules: vec![],
            },
        );
        assert!(result.issues.is_empty(), "{:?}", result.issues);
        assert!(result.claims[0].complete);
    }

    #[test]
    fn table_fields_must_belong_to_the_row_or_addressed_column_header() {
        let index = MarkdownIndex::new(
            "<table><tr><th>Объект</th><th>Масса кг</th><th>Объем л</th></tr>\
             <tr><td rowspan='2'>лифт</td><td>не менее 300</td><td>10</td></tr>\
             <tr><td>не более 500</td><td>20</td></tr></table>",
        )
        .unwrap();
        let id = |text: &str| {
            index
                .blocks
                .iter()
                .find(|b| index.source[b.range.clone()].trim() == text)
                .unwrap()
                .id
        };
        let statement = id("не более 500");
        let proposal = ClaimProposal {
            statement: q(statement, "не более 500"),
            object: ObjectProposal {
                key: "лифт".into(),
                evidence: vec![q(id("лифт"), "лифт")],
                alternatives: vec![],
            },
            property: q(id("Масса кг"), "Масса"),
            constraint: ConstraintProposal::Numeric {
                operator: q(statement, "не более"),
                value: q(statement, "500"),
                unit: Some(q(id("Масса кг"), "кг")),
            },
            stage: None,
            conditions: vec![],
            mandatory: None,
        };
        let trusted: Vec<_> = index.blocks.iter().map(|b| b.id).collect();
        let validate = |claim| {
            validate_batch(
                &index,
                &trusted,
                ClaimBatch {
                    claims: vec![claim],
                    priority_rules: vec![],
                },
            )
        };
        let valid = validate(proposal.clone());
        assert!(valid.issues.is_empty(), "{:?}", valid.issues);
        assert!(valid.claims[0].complete);

        for field in ["property", "operator", "value", "unit"] {
            let mut bad = proposal.clone();
            match field {
                "property" => bad.property = q(id("Объем л"), "Объем"),
                "operator" => {
                    if let ConstraintProposal::Numeric { operator, .. } = &mut bad.constraint {
                        *operator = q(id("не менее 300"), "не менее");
                    }
                }
                "value" => {
                    if let ConstraintProposal::Numeric { value, .. } = &mut bad.constraint {
                        *value = q(id("не менее 300"), "300");
                    }
                }
                "unit" => {
                    if let ConstraintProposal::Numeric { unit, .. } = &mut bad.constraint {
                        *unit = Some(q(id("Объем л"), "л"));
                    }
                }
                _ => unreachable!(),
            }
            let result = validate(bad);
            assert!(
                result.issues.iter().any(|issue| issue.field == field),
                "{field}: {:?}",
                result.issues
            );
            assert!(result.claims.is_empty());
        }
    }

    #[test]
    fn competing_refinements_are_order_independent_and_preserve_conditions() {
        let index =
            MarkdownIndex::new("лифт масса не менее 300 кг обязателен летом зимой").unwrap();
        let mut original = proposal(0, "300");
        original.statement = q(0, "лифт масса не менее 300 кг обязателен летом зимой");
        let target = validate_batch(
            &index,
            &[0],
            ClaimBatch {
                claims: vec![original],
                priority_rules: vec![],
            },
        )
        .claims
        .remove(0);
        let grounded = |text: &str| Grounded {
            text: text.into(),
            evidence: index.resolve(0, text).unwrap(),
        };
        let mut a = target.clone();
        a.conditions.push(grounded("летом"));
        let mut b = a.clone();
        b.conditions.push(grounded("зимой"));
        let refinement = |replacement| ClaimRefinement {
            target: target.clone(),
            replacement,
        };
        let run = |values| {
            let mut registry = ClaimRegistry::default();
            registry.insert_batch([target.clone()]);
            let issues = registry.apply_refinements(values);
            (registry, issues)
        };
        let (first, issues) = run(vec![
            refinement(a.clone()),
            refinement(b.clone()),
            refinement(a.clone()),
        ]);
        let (second, other_issues) = run(vec![refinement(b.clone()), refinement(a.clone())]);
        assert!(issues.is_empty() && other_issues.is_empty());
        assert_eq!(first.claims(), second.claims());
        assert_eq!(first.claims()[0].conditions.len(), 2);
        assert!(first.claims()[0].conditions.contains(&grounded("летом")));
        assert!(first.claims()[0].conditions.contains(&grounded("зимой")));

        let mut uncertain = target.clone();
        uncertain.conditions.push(grounded("зимой"));
        let (first, issues) = run(vec![refinement(a.clone()), refinement(uncertain.clone())]);
        let (second, other_issues) = run(vec![refinement(uncertain), refinement(a.clone())]);
        assert_eq!(issues, other_issues);
        assert_eq!(issues.len(), 1);
        assert_eq!(first.claims(), second.claims());
        assert_eq!(first.claims()[0], target);

        b.object.key = Some("кран".into());
        let (first, issues) = run(vec![refinement(a.clone()), refinement(b.clone())]);
        let (second, other_issues) = run(vec![refinement(b), refinement(a)]);
        assert_eq!(issues, other_issues);
        assert_eq!(issues.len(), 1);
        assert_eq!(first.claims(), second.claims());
        assert_eq!(first.claims()[0], target);
    }

    #[test]
    fn standalone_from_and_to_are_not_assumed_inclusive() {
        let index = MarkdownIndex::new("лифт масса до 500 кг").unwrap();
        let mut claim = proposal(0, "500");
        claim.statement = q(0, "лифт масса до 500 кг");
        claim.mandatory = None;
        if let ConstraintProposal::Numeric { operator, .. } = &mut claim.constraint {
            *operator = q(0, "до");
        }
        let result = validate_batch(
            &index,
            &[0],
            ClaimBatch {
                claims: vec![claim],
                priority_rules: vec![],
            },
        );
        assert!(result.claims.is_empty());
        assert!(
            result
                .issues
                .iter()
                .any(|issue| issue.field == "constraint")
        );
    }
    #[test]
    fn evidence_trust_identity_and_registry_are_stable() {
        let index = MarkdownIndex::new("лифт масса не менее 300 кг обязателен\n\nлифт масса не менее 500 кг обязателен\n\nлифт масса не менее 300 кг обязателен").unwrap();
        let claims = [proposal(0, "300"), proposal(1, "500"), proposal(2, "300")];
        let batch = |claims| {
            validate_batch(
                &index,
                &[0, 1, 2],
                ClaimBatch {
                    claims,
                    priority_rules: vec![],
                },
            )
        };
        let first = batch(claims.to_vec());
        assert!(first.issues.is_empty());
        assert!(first.claims.iter().all(|c| c.complete));
        let Constraint::Numeric {
            operator,
            value,
            unit,
            ..
        } = &first.claims[0].constraint
        else {
            panic!()
        };
        assert_eq!(*operator, Operator::AtLeast);
        assert_eq!(
            super::super::numeric::parse_decimal(&value.text)
                .unwrap()
                .coefficient,
            300
        );
        assert_ne!(value.evidence.range, unit.as_ref().unwrap().evidence.range);
        let mut a = ClaimRegistry::default();
        a.insert_batch(first.claims.clone());
        a.insert_batch(first.claims.clone());
        let mut b = ClaimRegistry::default();
        b.insert_batch(first.claims.into_iter().rev());
        assert_eq!(a.claims(), b.claims());
        assert_eq!(a.group("лифт", "масса").len(), 3);
        let mut invalid = claims[0].clone();
        invalid.conditions.push(q(2, "несуществующее условие"));
        invalid.object.key = "ифт".into();
        let bad = batch(vec![invalid]);
        assert!(!bad.issues.is_empty());
        assert!(!bad.claims[0].complete);
        assert_eq!(bad.claims[0].object.key, None);
        let bad = validate_batch(
            &index,
            &[0],
            ClaimBatch {
                claims: vec![claims[1].clone()],
                priority_rules: vec![],
            },
        );
        assert!(bad.claims.is_empty());
        assert!(!bad.issues.is_empty());
        assert!(serde_json::from_str::<ClaimBatch>("{}").is_err());
    }

    #[test]
    fn orphaned_objects_and_explicit_refinements() {
        let index = MarkdownIndex::new(
            "лифт масса не менее 300 кг обязателен\n\nлифт масса не менее 500 кг обязателен",
        )
        .unwrap();
        let mut orphan = proposal(1, "500");
        orphan.object.evidence = vec![q(0, "лифт")];
        let invalid = validate_batch(
            &index,
            &[0, 1],
            ClaimBatch {
                claims: vec![orphan],
                priority_rules: vec![],
            },
        );
        assert!(!invalid.claims[0].complete);
        let batch = validate_batch(
            &index,
            &[0, 1],
            ClaimBatch {
                claims: vec![proposal(0, "300"), proposal(1, "500")],
                priority_rules: vec![],
            },
        );
        let mut registry = ClaimRegistry::default();
        registry.insert_batch(batch.claims.clone());
        let target = registry.claims()[0].clone();
        let mut replacement = target.clone();
        replacement
            .conditions
            .push(replacement.mandatory.clone().unwrap());
        assert!(
            registry
                .apply_refinements(vec![ClaimRefinement {
                    target: target.clone(),
                    replacement: replacement.clone()
                }])
                .is_empty()
        );
        assert_eq!(registry.claims()[0], replacement);
        assert_eq!(registry.claims().len(), 2);
        let mut wrong = replacement.clone();
        wrong.statement = registry.claims()[1].statement.clone();
        assert_eq!(
            registry
                .apply_refinements(vec![ClaimRefinement {
                    target: replacement.clone(),
                    replacement: wrong
                }])
                .len(),
            1
        );
        assert_eq!(registry.claims()[0], replacement);
    }

    #[tokio::test]
    async fn control_recovers_new_claim_and_standard_from_empty_registry() {
        let index =
            MarkdownIndex::new("лифт масса не менее 300 кг обязателен\n\nГОСТ 123").unwrap();
        let packets = index.packets(10000);
        let response = serde_json::json!({"claims": [proposal(0, "300")], "priority_rules": [], "refinements": [], "standard_mentions": [q(1, "ГОСТ 123")]});
        let runtime = crate::runtime::Runtime::new(
            ControlModel(response.to_string()),
            std::time::Duration::from_secs(1),
        );
        let outcome = extract_control(
            &runtime,
            &crate::runtime::ReviewRun::new(1),
            &index,
            &packets.packets[0],
            crate::core::Visibility::Public,
            &[],
            20000,
        )
        .await
        .unwrap();
        assert_eq!(outcome.batch.claims.len(), 1);
        assert_eq!(outcome.standard_mentions[0].text, "ГОСТ 123");
        assert!(outcome.batch.issues.is_empty());
    }
    #[tokio::test]
    async fn remote_refinement_preserves_prior_conditions_and_requires_local_evidence() {
        let index = MarkdownIndex::new("лифт масса не менее 300 кг обязателен летом\n\nПримечание: «лифт масса не менее 300 кг обязателен летом» — кран, только зимой").unwrap();
        let mut original = proposal(0, "300");
        original.statement = q(0, "лифт масса не менее 300 кг обязателен летом");
        original.conditions.push(q(0, "летом"));
        let target = validate_batch(
            &index,
            &[0],
            ClaimBatch {
                claims: vec![original.clone()],
                priority_rules: vec![],
            },
        )
        .claims
        .remove(0);
        let packet = index.packet_for(&[1]);
        for (conditions, accepted) in [
            (vec![q(1, "зимой")], false),
            (vec![q(0, "летом"), q(1, "зимой")], true),
        ] {
            let mut replacement = original.clone();
            replacement.object = ObjectProposal {
                key: "кран".into(),
                evidence: vec![q(1, "кран")],
                alternatives: vec![],
            };
            replacement.conditions = conditions;
            let response = serde_json::json!({"claims":[], "priority_rules":[], "refinements":[{"claim_index":0,"replacement":replacement}], "standard_mentions":[]});
            let runtime = crate::runtime::Runtime::new(
                ControlModel(response.to_string()),
                std::time::Duration::from_secs(1),
            );
            let result = extract_control(
                &runtime,
                &crate::runtime::ReviewRun::new(1),
                &index,
                &packet,
                crate::core::Visibility::Public,
                std::slice::from_ref(&target),
                10000,
            )
            .await
            .unwrap();
            assert_eq!(!result.refinements.is_empty(), accepted);
        }
    }
}
