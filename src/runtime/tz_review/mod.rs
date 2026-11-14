//! One read-only 223-FZ workflow branch. All external calls use the shared typed-stage budget.
mod premise;
pub mod reader;
mod source;
pub use source::Location;

use super::{ReviewRun, Runtime, StageKind, StageRequest, grok::Model};
use crate::core::{Document, Visibility};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::json;
use source::Source;
use std::collections::BTreeSet;

const CATALOG: &str = include_str!("../../../223fz_27_agents_ru_unified.json");
const PLAN: &str = include_str!("plan.txt");
const CHECK: &str = include_str!("check.txt");
const CRITIC: &str = include_str!("critic.txt");

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
    pub line: usize,
    pub quote: String,
}

#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Regime {
    Applicable,
    Uncertain,
    Inapplicable,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub id: String,
    pub scope: String,
    pub reason: String,
    pub lines: Vec<usize>,
    #[serde(default)]
    pub context: Vec<usize>,
    #[serde(default)]
    pub materials: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
struct Candidate {
    lines: Vec<usize>,
    scope: String,
    claim: String,
    basis: String,
    #[serde(default)]
    wording: Option<String>,
}

#[derive(schemars::JsonSchema, Serialize, Deserialize)]
struct Plan {
    regime: Regime,
    reason: String,
    line: Option<usize>,
    assignments: Vec<Assignment>,
    candidates: Vec<Candidate>,
}
#[derive(schemars::JsonSchema, Serialize, Deserialize)]
struct Claims {
    #[serde(default)]
    reviewed: Vec<usize>,
    candidates: Vec<Candidate>,
}
#[derive(schemars::JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decisions {
    decisions: Vec<Decision>,
}
#[derive(schemars::JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    index: usize,
    observation: Observation,
    #[serde(default)]
    evidence: Vec<Anchor>,
    #[serde(default)]
    lines: Vec<usize>,
    #[serde(default)]
    context: Vec<usize>,
    #[serde(default)]
    correction_reason: String,
    explanation: String,
    basis: String,
    missing_facts: Vec<String>,
    #[serde(default)]
    accept_wording: bool,
    #[serde(default)]
    procedure_source: Option<Anchor>,
    #[serde(default)]
    procedure_material: Option<String>,
    #[serde(default)]
    wording_reason: String,
}
#[derive(schemars::JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Observation {
    NoRiskIndicator,
    CustomerRegime,
    ProcurementPolicy,
    CompetitionRestriction,
    ParticipantAdmission,
    DescriptionDefect,
    CommercialIdentifier,
}

impl Observation {
    fn check(self) -> Option<&'static str> {
        match self {
            Self::NoRiskIndicator => None,
            Self::CustomerRegime => Some("A01"),
            Self::ProcurementPolicy => Some("A02"),
            Self::CompetitionRestriction => Some("A03"),
            Self::ParticipantAdmission => Some("A04"),
            Self::DescriptionDefect => Some("A05"),
            Self::CommercialIdentifier => Some("A06"),
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Rule {
    id: String,
    name: String,
    prompt: String,
}
#[derive(Deserialize)]
struct Catalog {
    agents: Vec<Rule>,
    revision_notice: String,
}

#[derive(Debug, Serialize)]
pub struct Finding {
    pub check: String,
    pub category: String,
    pub scope: String,
    pub status: &'static str,
    pub source: Location,
    pub additional_sources: Vec<Location>,
    pub context: Vec<Location>,
    pub fact: String,
    pub original_lines: Vec<usize>,
    pub procedure_source: Option<Location>,
    pub procedure_material: Option<String>,
    pub explanation: String,
    pub basis: String,
    pub missing: Option<String>,
    pub wording: Option<String>,
    pub wording_reason: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Outcome {
    pub check: String,
    pub stage: &'static str,
    pub state: &'static str,
    pub detail: String,
}
#[derive(Debug, Serialize)]
pub struct Position {
    pub id: usize,
    pub state: &'static str,
}
#[derive(Debug, Serialize)]
pub struct DecisionTrace {
    pub index: usize,
    pub check: String,
    pub scope: String,
    pub claim: String,
    pub original_lines: Vec<usize>,
    pub reviewed_lines: Vec<usize>,
    pub context: Vec<usize>,
    pub correction_reason: String,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub revision: String,
    pub catalog_revision: String,
    pub regime: Option<Regime>,
    pub reason: String,
    pub applicability_source: Option<Location>,
    pub plan: Vec<Assignment>,
    pub positions: Vec<Position>,
    pub decisions: Vec<DecisionTrace>,
    pub findings: Vec<Finding>,
    pub coverage: Vec<Outcome>,
    pub attempts: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stage_log: Vec<serde_json::Value>,
}
impl Report {
    fn outcome(
        &mut self,
        id: &str,
        stage: &'static str,
        state: &'static str,
        detail: impl Into<String>,
    ) {
        self.coverage.push(Outcome {
            check: id.into(),
            stage,
            state,
            detail: detail.into(),
        });
    }
}

pub struct Material<'a> {
    pub name: &'a str,
    pub document: &'a Document,
}

pub async fn review<M: Model>(
    runtime: &Runtime<M>,
    doc: &Document,
    materials: &[Material<'_>],
    budget: usize,
) -> Result<Report, &'static str> {
    review_with_mode(runtime, doc, materials, budget, false).await
}

/// A full diagnostic run exercises each MVP rule and the critic within one budget.
pub async fn review_with_mode<M: Model>(
    runtime: &Runtime<M>,
    doc: &Document,
    materials: &[Material<'_>],
    budget: usize,
    full: bool,
) -> Result<Report, &'static str> {
    if doc.markdown.trim().is_empty() {
        return Err("Пустой входной Markdown");
    }
    let source = Source::new(&doc.markdown);
    let run = ReviewRun::new(budget);
    let visibility = [doc.visibility];
    let public = [Visibility::Public];
    let catalog: Catalog = serde_json::from_str(CATALOG).expect("embedded catalog");
    let rules: Vec<_> = catalog
        .agents
        .into_iter()
        .filter(|r| matches!(r.id.as_str(), "A01" | "A02" | "A03" | "A04" | "A05" | "A06"))
        .collect();
    let mut report = Report {
        revision: source.revision.clone(),
        catalog_revision: catalog.revision_notice,
        regime: None,
        reason: String::new(),
        applicability_source: None,
        plan: vec![],
        positions: vec![],
        decisions: vec![],
        findings: vec![],
        coverage: vec![],
        attempts: 0,
        stage_log: Vec::new(),
    };
    let mut names = BTreeSet::new();
    if materials
        .iter()
        .any(|m| m.name.trim().is_empty() || !names.insert(m.name))
    {
        return Err("Имена приложений должны быть уникальными и непустыми");
    }
    // Restricted attachments are omitted entirely, including their names.
    let allowed: Vec<_> = materials
        .iter()
        .filter(|m| m.document.visibility == Visibility::Public)
        .collect();
    for _ in materials
        .iter()
        .filter(|m| m.document.visibility != Visibility::Public)
    {
        report.outcome(
            "",
            "material",
            "restricted",
            "Закрытое приложение исключено до отправки",
        );
    }
    let attachments: Vec<_> = allowed
        .iter()
        .map(|m| json!({"name":m.name,"text":m.document.markdown}))
        .collect();
    let routing: Vec<_> = rules[1..]
        .iter()
        .map(|r| json!({"id":r.id,"name":r.name,"when":r.prompt.lines().next()}))
        .collect();
    let request = StageRequest::new(StageKind::Claims, PLAN)
        .field("numbered_markdown", &visibility, &source.numbered)
        .field("public_materials", &public, &attachments)
        .field("applicability_rule", &public, &rules[0])
        .field("available_checks", &public, &routing)
        .schema(schemars::schema_for!(Plan));
    let mut plan: Plan = match runtime.step(&run, &request).await {
        Ok(p) => p,
        Err(e) => {
            report.outcome("A01", "plan", "error", format!("{e:?}"));
            report.attempts = run.attempts();
            return Ok(report);
        }
    };
    if full {
        report.stage_log.push(json!({"stage":"A01","result":plan}));
    }
    report.applicability_source = plan.line.and_then(|n| source.line(n)).filter(|loc| {
        let quote = loc.quote.trim();
        !(quote.starts_with("<a id=\"_Toc") && quote.ends_with("</a>"))
    });
    if report.applicability_source.is_none()
        && (plan.regime != Regime::Uncertain || plan.line.is_some())
    {
        report.outcome(
            "A01",
            "plan",
            "rejected",
            format!(
                "Решение {:?}, адрес {:?} не подтверждён; дальнейшие проверки условны",
                plan.regime, plan.line
            ),
        );
        plan.reason = "Применимость не установлена: строка решения A01 не подтверждена.".into();
        plan.regime = Regime::Uncertain;
    }
    report.regime = Some(plan.regime);
    report.reason = plan.reason;
    report.outcome("A01", "plan", "completed", format!("{:?}", plan.regime));
    let mut batches = Vec::new();
    if plan.regime != Regime::Inapplicable {
        batches.push((&rules[0], plan.candidates, attachments.clone()));
        for mut assignment in plan.assignments {
            if assignment.id == "A02"
                && assignment.materials.is_empty()
                && !doc.markdown.lines().any(|l| {
                    l.trim_start_matches('#')
                        .trim()
                        .to_lowercase()
                        .starts_with("положение о закупке")
                })
            {
                report.outcome("A02", "plan", "skipped", "Нет текста применимого положения");
                continue;
            }
            if !rules.iter().skip(1).any(|r| r.id == assignment.id)
                || assignment.scope.trim().is_empty()
                || assignment.reason.trim().is_empty()
            {
                report.outcome(
                    &assignment.id,
                    "plan",
                    "rejected",
                    "Неверный ID или пустая область/основание",
                );
                continue;
            }
            if !assignment
                .materials
                .iter()
                .all(|name| allowed.iter().any(|m| m.name == name))
            {
                report.outcome(
                    &assignment.id,
                    "plan",
                    "skipped",
                    "Требуемое приложение недоступно",
                );
                continue;
            }
            let is_blank = |number: &mut usize| {
                source
                    .line_text(*number)
                    .is_some_and(|row| row.trim().is_empty())
            };
            let blank_lines: Vec<_> = assignment.lines.extract_if(.., is_blank).collect();
            let blank_context: Vec<_> = assignment.context.extract_if(.., is_blank).collect();
            if !blank_lines.is_empty() || !blank_context.is_empty() {
                report.outcome(
                    &assignment.id,
                    "plan",
                    "filtered",
                    format!(
                        "{} · пропущены пустые строки: основные {blank_lines:?}, контекст {blank_context:?}",
                        assignment.scope
                    ),
                );
            }
            let invalid: Vec<_> = assignment
                .lines
                .iter()
                .chain(&assignment.context)
                .filter(|n| source.line_text(**n).is_none())
                .collect();
            if !invalid.is_empty() {
                report.outcome(
                    &assignment.id,
                    "plan",
                    "rejected",
                    format!(
                        "{} · строки {:?}, контекст {:?}, ошибочные адреса {:?}",
                        assignment.scope, assignment.lines, assignment.context, invalid
                    ),
                );
                continue;
            }
            if assignment.lines.is_empty() {
                report.outcome(
                    &assignment.id,
                    "plan",
                    "rejected",
                    format!(
                        "{} · нет непустых основных строк · контекст {:?}",
                        assignment.scope, assignment.context
                    ),
                );
                continue;
            }
            if !report.plan.iter().any(|a| {
                a.id == assignment.id
                    && a.scope == assignment.scope
                    && a.lines == assignment.lines
                    && a.context == assignment.context
                    && a.materials == assignment.materials
            }) {
                report.plan.push(assignment);
            }
        }
    }
    if plan.regime != Regime::Inapplicable {
        for line in source.brand_lines() {
            if report
                .plan
                .iter()
                .any(|a| a.id == "A06" && a.lines.contains(&line))
            {
                continue;
            }
            report.plan.push(Assignment {
                id: "A06".into(),
                scope: format!("Позиция со ссылкой на модель, L{line}"),
                reason: "Явное поле модели, производителя или референса".into(),
                lines: vec![line],
                context: vec![],
                materials: allowed.iter().map(|m| m.name.to_owned()).collect(),
            });
        }
    }
    for rule in &rules[1..] {
        if (!full || plan.regime == Regime::Inapplicable)
            && !report.plan.iter().any(|a| a.id == rule.id)
        {
            report.outcome(&rule.id, "check", "skipped", "Вне выбранного плана");
        }
    }
    // Keep every scoped assignment in the plan, dispatch each rule once with all its scopes.
    let jobs = rules[1..].iter().filter_map(|rule| {
        let assignments: Vec<_> = report
            .plan
            .iter()
            .enumerate()
            .filter(|(_, a)| a.id == rule.id)
            .collect();
        if assignments.is_empty() && (!full || plan.regime == Regime::Inapplicable) {
            return None;
        }
        let selected: Vec<_> = attachments
            .iter()
            .filter(|m| {
                assignments.iter().any(|(_, a)| {
                    a.materials
                        .iter()
                        .any(|name| m["name"].as_str() == Some(name))
                })
            })
            .cloned()
            .collect();
        let run = &run;
        let numbered = &source.numbered;
        let reason = &report.reason;
        Some(async move {
            let ids: Vec<_> = assignments.iter().map(|(index, _)| *index).collect();
            let scope = assignments
                .iter()
                .map(|(_, a)| a.scope.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ");
            let request = StageRequest::new(StageKind::Claims, CHECK)
                .field("numbered_markdown", &visibility, numbered)
                .field("rule", &public, rule)
                .field("regime", &visibility, &plan.regime)
                .field("regime_reason", &visibility, reason)
                .field("assignments", &visibility, &assignments)
                .field("full_diagnostic", &public, &full)
                .field("public_materials", &public, &selected)
                .schema(schemars::schema_for!(Claims));
            let result = runtime.step::<Claims>(run, &request).await;
            drop(request);
            (rule, scope, result, selected, ids)
        })
    });
    for (rule, scope, result, selected, ids) in join_all(jobs).await {
        if full {
            report
                .stage_log
                .push(json!({"stage":rule.id,"assignments":ids,"result":result}));
        }
        for id in ids {
            let state = match &result {
                Ok(items) if items.reviewed.iter().filter(|n| **n == id).count() == 1 => "reviewed",
                Ok(_) => "unacknowledged",
                Err(
                    super::StepError::BudgetExhausted
                    | super::StepError::RestrictedContext
                    | super::StepError::Preparation,
                ) => "not_dispatched",
                Err(_) => "failed",
            };
            if state == "unacknowledged" {
                report.outcome(
                    &rule.id,
                    "position",
                    "error",
                    format!("Нет однозначного результата для назначения {id}"),
                );
            }
            report.positions.push(Position { id, state });
        }
        match result {
            Ok(items) => {
                report.outcome(
                    &rule.id,
                    "check",
                    "completed",
                    format!("{scope} · кандидатов {}", items.candidates.len()),
                );
                batches.push((rule, items.candidates, selected));
            }
            Err(e) => report.outcome(&rule.id, "check", "error", format!("{scope}: {e:?}")),
        }
    }
    let mut candidates = Vec::new();
    for (rule, items, selected) in batches {
        for item in items {
            candidates.push((rule, item, selected.clone()));
        }
    }
    if !candidates.is_empty() || full {
        let input: Vec<_> = candidates
            .iter()
            .enumerate()
            .map(|(index, (rule, candidate, materials))| {
                let names: Vec<_> = materials.iter().map(|m| &m["name"]).collect();
                json!({"index":index,"rule":rule.id,"candidate":candidate,"materials":names})
            })
            .collect();
        let used_rules: Vec<_> = rules
            .iter()
            .filter(|r| candidates.iter().any(|(rule, _, _)| r.id == rule.id))
            .collect();
        let used_materials: Vec<_> = attachments
            .iter()
            .filter(|m| candidates.iter().any(|(_, _, ms)| ms.contains(m)))
            .collect();
        let request = StageRequest::new(StageKind::Review, CRITIC)
            .field("numbered_markdown", &visibility, &source.numbered)
            .field("regime", &visibility, &report.regime)
            .field("regime_reason", &visibility, &report.reason)
            .field("catalog_revision", &public, &report.catalog_revision)
            .field("rules", &public, &used_rules)
            .field("public_materials", &public, &used_materials)
            .field("candidates", &visibility, &input)
            .schema(schemars::schema_for!(Decisions));
        let result = runtime.step::<Decisions>(&run, &request).await;
        drop(request);
        if full {
            report
                .stage_log
                .push(json!({"stage":"critic","inputs":input,"result":result}));
        }
        for (index, (rule, candidate, materials)) in candidates.into_iter().enumerate() {
            let id = &rule.id;
            let decisions = match &result {
                Ok(r) => &r.decisions,
                Err(e) => {
                    report.outcome(id, "critic", "error", format!("#{index}: {e:?}"));
                    continue;
                }
            };
            let mut matches = decisions.iter().filter(|d| d.index == index);
            let Some(d) = matches.next() else {
                report.outcome(id, "critic", "error", format!("Нет решения #{index}"));
                continue;
            };
            if matches.next().is_some() || d.explanation.trim().is_empty() {
                report.outcome(
                    id,
                    "critic",
                    "error",
                    format!("Некорректное решение #{index}"),
                );
                continue;
            }
            report.decisions.push(DecisionTrace {
                index,
                check: id.clone(),
                scope: candidate.scope.clone(),
                claim: candidate.claim.clone(),
                original_lines: candidate.lines.clone(),
                reviewed_lines: d.lines.clone(),
                context: d.context.clone(),
                correction_reason: d.correction_reason.clone(),
            });
            let item_number = premise::position_number(&candidate.scope)
                .or_else(|| candidate.lines.first().and_then(|n| source.item_number(*n)));
            let lot = candidate.lines.iter().find_map(|&n| source.lot(n));
            let foreign = |n: usize| {
                if source.lot(n).zip(lot).is_some_and(|(a, b)| a != b) {
                    return true;
                }

                source
                    .item_number(n)
                    .zip(item_number)
                    .is_some_and(|(actual, expected)| actual != expected)
            };
            if d.observation.check().is_some_and(|check| check != id)
                || d.missing_facts.iter().any(|fact| fact.trim().is_empty())
            {
                report.outcome(
                    id,
                    "critic",
                    "error",
                    format!("Некорректное наблюдение для {id} #{index}"),
                );
                continue;
            }
            if d.observation == Observation::NoRiskIndicator {
                if d.context
                    .iter()
                    .chain(&d.lines)
                    .copied()
                    .chain(d.evidence.iter().map(|a| a.line))
                    .any(foreign)
                {
                    report.outcome(
                        id,
                        "critic",
                        "error",
                        format!("Снятие основано на чужой позиции #{index}"),
                    );
                    continue;
                }
                report.outcome(
                    id,
                    "critic",
                    "dismissed",
                    format!("#{index}: {}", d.explanation),
                );
                continue;
            }
            let changed = d.lines.iter().any(|line| !candidate.lines.contains(line));
            let mut evidence_lines: Vec<_> = d.evidence.iter().map(|a| a.line).collect();
            let mut evidence = d
                .evidence
                .iter()
                .map(|anchor| source.exact(anchor))
                .collect::<Option<Vec<_>>>();
            if id == "A06" && matches!(evidence.as_deref(), None | Some([])) {
                let shared: Vec<_> = d
                    .lines
                    .iter()
                    .filter(|n| candidate.lines.contains(n))
                    .copied()
                    .collect();
                if let Some(field) = premise::named_field(&source, &shared, &candidate.claim) {
                    evidence_lines = vec![field.line];
                    if let Some(trace) = report.decisions.last_mut() {
                        trace.reviewed_lines.clone_from(&evidence_lines);
                        trace.correction_reason =
                            "Проверенное значение именованного поля из строки исходной позиции"
                                .into();
                    }
                    evidence = Some(vec![field]);
                }
            }
            let Some(mut evidence) = evidence else {
                report.outcome(
                    id,
                    "critic",
                    "error",
                    format!("Нет точного доказательства по указанным адресам #{index}"),
                );
                continue;
            };
            // Bind an address correction to the same assignment. Overlap of a
            // rule ID or a reason written by the critic is not a position ID.
            let owners: Vec<_> = report
                .plan
                .iter()
                .filter(|a| {
                    a.id == *id
                        && a.scope == candidate.scope
                        && a.lines.iter().any(|n| candidate.lines.contains(n))
                })
                .collect();
            let in_position = |line: usize| {
                if foreign(line) {
                    return false;
                }
                if let [owner] = owners.as_slice() {
                    owner.lines.contains(&line)
                } else {
                    candidate.lines.contains(&line)
                }
            };
            if evidence.first().is_some_and(|e| !in_position(e.line)) {
                report.outcome(
                    id,
                    "critic",
                    "error",
                    format!("Основная ссылка вне исходной позиции #{index}"),
                );
                continue;
            }
            let owned_context: BTreeSet<_> = owners
                .iter()
                .flat_map(|a| a.context.iter().copied())
                .collect();
            let context_lines: BTreeSet<_> = owned_context
                .iter()
                .copied()
                .chain(d.context.iter().copied())
                .collect();
            let context: Vec<_> = context_lines
                .into_iter()
                .filter_map(|n| source.line(n))
                .filter(|loc| {
                    !foreign(loc.line)
                        && (in_position(loc.line)
                            || premise::global_condition(&loc.quote)
                            || (owned_context.contains(&loc.line)
                                && !premise::item_row(&loc.quote)))
                })
                .collect();
            if evidence.is_empty()
                || evidence_lines.iter().collect::<BTreeSet<_>>()
                    != report
                        .decisions
                        .last()
                        .expect("current decision")
                        .reviewed_lines
                        .iter()
                        .collect::<BTreeSet<_>>()
                || d.basis.trim().is_empty()
                || (changed && d.correction_reason.trim().is_empty())
            {
                report.outcome(
                    id,
                    "critic",
                    "error",
                    format!("Неполные доказательства #{index}"),
                );
                continue;
            }
            evidence.retain(|e| in_position(e.line) && !source.historical_at(e.line));
            if evidence.is_empty() {
                report.outcome(
                    id,
                    "critic",
                    "dismissed",
                    format!("#{index}: опоры относятся к архивному разделу"),
                );
                continue;
            }
            let grounded = match premise::evaluate(id, &source, &evidence, &context, item_number) {
                premise::Premise::Dismiss(reason) => {
                    report.outcome(id, "critic", "dismissed", format!("#{index}: {reason}"));
                    continue;
                }
                premise::Premise::Supported {
                    source: verified,
                    explanation,
                } => {
                    evidence = vec![verified];
                    Some(explanation)
                }
                premise::Premise::A05Evidence(field, requirement, explanation) => {
                    evidence = vec![field, requirement];
                    Some(explanation)
                }
                premise::Premise::Other => None,
            };
            let fact = evidence
                .iter()
                .map(|e| e.quote.as_str())
                .collect::<Vec<_>>()
                .join(" … ");
            let location = evidence.remove(0);
            // A03–A06 depend on a competitive procedure. A customer's name
            // or a model number cannot establish that precondition on its own.
            let needs_procedure = matches!(id.as_str(), "A03" | "A04" | "A05" | "A06");
            let declared = d.procedure_source.as_ref().and_then(|a| {
                let material_source;
                let origin = if let Some(name) = &d.procedure_material {
                    let m = materials
                        .iter()
                        .find(|m| m["name"].as_str() == Some(name))?;
                    material_source = Source::new(m["text"].as_str()?);
                    &material_source
                } else {
                    &source
                };
                let location = origin.exact(a)?;
                let whole = origin.line(location.line)?;
                if origin.historical_at(whole.line) {
                    return None;
                }
                Some((premise::procedure(&whole.quote), location))
            });
            if needs_procedure
                && (premise::single_supplier(&doc.markdown)
                    || materials
                        .iter()
                        .any(|m| m["text"].as_str().is_some_and(premise::single_supplier))
                    || declared
                        .as_ref()
                        .is_some_and(|(p, _)| *p == premise::Procedure::Single))
            {
                report.outcome(
                    id,
                    "critic",
                    "dismissed",
                    format!("#{index}: указан неконкурентный способ"),
                );
                continue;
            }
            let procedure = declared
                .filter(|(p, _)| *p == premise::Procedure::Competitive)
                .map(|(_, loc)| loc);
            let mut missing = d.missing_facts.iter().map(|s| s.trim()).collect::<Vec<_>>();
            if plan.regime != Regime::Applicable {
                missing.push("Применимость 223-ФЗ к закупке.");
            }
            if needs_procedure && procedure.is_none() {
                missing.push("Конкурентный способ этой закупки.");
            }
            let confirmed = missing.is_empty();
            let explanation = grounded.unwrap_or(&d.explanation).to_owned();
            if report.findings.iter().any(|f| {
                f.check == *id && f.source.start == location.start && f.explanation == explanation
            }) {
                report.outcome(id, "critic", "dismissed", format!("Дубликат #{index}"));
                continue;
            }
            let wording = candidate.wording.filter(|w| {
                confirmed
                    && (id != "A03"
                        || candidate.lines == [location.line]
                            && candidate.claim.trim() == location.quote.trim())
                    && d.accept_wording
                    && !w.trim().is_empty()
                    && !d.wording_reason.trim().is_empty()
            });
            report.findings.push(Finding {
                check: id.clone(),
                category: rule.name.clone(),
                scope: candidate.scope,
                status: if confirmed {
                    "подтверждено"
                } else {
                    "требует проверки"
                },
                source: location,
                additional_sources: evidence,
                context,
                original_lines: candidate.lines,
                fact,
                procedure_material: procedure.as_ref().and(d.procedure_material.clone()),
                procedure_source: procedure,
                explanation,
                basis: match id.as_str() {
                    "A03" | "A06" => rule
                        .prompt
                        .lines()
                        .find(|line| line.starts_with("Норма:"))
                        .unwrap_or(&d.basis)
                        .into(),
                    _ => d.basis.clone(),
                },
                missing: (!confirmed).then(|| missing.join(" ")),
                wording_reason: wording.as_ref().map(|_| d.wording_reason.clone()),
                wording,
            });
            report.outcome(id, "critic", "published", format!("#{index}"));
        }
    }
    report.attempts = run.attempts();
    Ok(report)
}
