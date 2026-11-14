//! Per-address standard applicability. Model interpretations are grounded locally.
use super::{
    claims::{strict_schema, Quote},
    markdown::{BlockKind, Evidence, MarkdownIndex},
    standards::{self, Catalog, ReferenceReview},
};
use crate::{
    core::Visibility,
    runtime::{ReviewRun, Runtime, StageKind, StageRequest, grok::Model},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Bibliography,
    Direct,
    Indirect,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Supported,
    OutsideScope,
    InsufficientEvidence,
    #[schemars(skip)]
    TechnicalFailure,
}
#[derive(Debug, Clone, Serialize)]
pub struct UsageReview {
    pub mention: Evidence,
    pub role: Role,
    pub outcome: Outcome,
    pub evidence: Vec<Evidence>,
    pub official_url: Option<String>,
    pub official_field: Option<String>,
    pub official_excerpt: Option<String>,
    pub snapshot_date: String,
    pub assessment_date: String,
    pub title_alternatives: Vec<String>,
    pub action: Option<String>,
    pub limitation: Option<String>,
    pub editorial_correction: Option<EditorialCorrection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditorialCorrection {
    pub card: standards::Card,
    pub title: Evidence,
    pub statement: Evidence,
    pub method: Option<Evidence>,
    pub temperature: Option<Evidence>,
    pub clause: Option<Evidence>,
    pub action: Option<Evidence>,
}
#[derive(Deserialize, JsonSchema)]
struct Proposal {
    role: Role,
    title: Option<Quote>,
    statement: Option<Quote>,
    method: Option<Quote>,
    temperature: Option<Quote>,
    #[serde(default)]
    caveats: Vec<Quote>,
    attributed_threshold: Option<Quote>,
    // Exact quotation from the selected card's scope, never another card or a model summary.
    official_excerpt: Option<String>,
    #[serde(default)]
    clause_source: Option<String>,
    #[serde(default)]
    clause_number: Option<String>,
    #[serde(default)]
    clause_reference: Option<Quote>,
    #[serde(default)]
    action: Option<Quote>,
    outcome: Outcome,
}

fn resolve_statement(index: &MarkdownIndex, q: &Quote) -> Option<Evidence> {
    if let Some(evidence) = index.resolve(q.block_id, &q.quote) {
        return Some(evidence);
    }
    let block = index.blocks.get(q.block_id)?;
    if block.kind != BlockKind::HtmlTableCell || q.quote.is_empty() {
        return None;
    }
    let text = index.source.get(block.range.clone())?;
    let offset = text.find(&q.quote)?;
    let next = offset + text[offset..].chars().next()?.len_utf8();
    if text[next..].contains(&q.quote) {
        return None;
    }
    let range = block.range.start + offset..block.range.start + offset + q.quote.len();
    let first = block
        .text_ranges
        .iter()
        .position(|r| r.contains(&range.start))?;
    let last = block
        .text_ranges
        .iter()
        .position(|r| r.start < range.end && range.end <= r.end)?;
    if first >= last
        || block.text_ranges[first..=last]
            .windows(2)
            .any(|pair| index.source.get(pair[0].end..pair[1].start) != Some("<br>"))
    {
        return None;
    }
    Some(Evidence {
        version: index.version.clone(),
        block_id: q.block_id,
        range,
    })
}

/// One call per occurrence; inaccessible or unsupported evidence remains explicitly incomplete.
pub async fn evaluate<M: Model>(
    index: &MarkdownIndex,
    reference: &ReferenceReview,
    catalog: &Catalog,
    runtime: &Runtime<M>,
    run: &ReviewRun,
    visibility: Visibility,
) -> UsageReview {
    let mut result = UsageReview {
        mention: reference.mention.evidence.clone(), role: Role::Unknown,
        outcome: Outcome::InsufficientEvidence, evidence: vec![], official_url: None,
        official_field: None, official_excerpt: None, snapshot_date: reference.snapshot_date.clone(),
        assessment_date: reference.assessment_date.clone(), title_alternatives: vec![], action: None,
        limitation: Some(if reference.cards.is_empty() {
            "Карточка стандарта недоступна для проверки этого упоминания"
        } else {
            "Карточка найдена, но доказательств её применимости к этому требованию недостаточно"
        }.into()),
        editorial_correction: None,
    };
    let mention = &reference.mention;
    if mention.evidence.version != index.version
        || index.source.get(mention.evidence.range.clone()) != Some(mention.original.as_str())
        || !index
            .blocks
            .get(mention.evidence.block_id)
            .is_some_and(|b| {
                b.text_ranges.iter().any(|r| {
                    r.start <= mention.evidence.range.start && mention.evidence.range.end <= r.end
                })
            })
    {
        return result;
    }
    let packet = index.packet_for(&[mention.evidence.block_id]);
    let block = &index.blocks[mention.evidence.block_id];
    let source_text = (block.kind == BlockKind::HtmlTableCell)
        .then(|| (block.id, &index.source[block.range.clone()]));
    let sources = [visibility];
    let public = [Visibility::Public];
    let official_cards: Vec<_> = reference
        .cards
        .iter()
        .filter(|c| {
            c.url.starts_with("https://protect.gost.ru/")
                && standards::matches_reference(c, &mention.key)
        })
        .collect();
    // Candidate cards are local and public; the original document stays under its visibility.
    let local_index = standards::CatalogIndex::new(catalog);
    let candidate_cards: Vec<_> = local_index
        .variants(&mention.key)
        .into_iter()
        .filter(|c| c.url.starts_with("https://protect.gost.ru/"))
        .collect();
    let referring_cards: Vec<_> = catalog
        .cards
        .iter()
        .filter(|c| {
            c.url.starts_with("https://protect.gost.ru/")
                && c.clauses
                    .iter()
                    .any(|clause| clause.text.contains(&mention.original))
        })
        .collect();
    let request = StageRequest::new(StageKind::Review, "For this single numbered occurrence, identify its role (bibliography, direct, indirect, unknown), explicit official title only (not paraphrase), statement, method, temperature and all nearby caveats including independent customer thresholds. Return JSON {role,title,statement,method,temperature,caveats,attributed_threshold,official_excerpt,outcome,clause_source,clause_number,clause_reference,action}. For an explicit title typo, clause_reference quotes the independent clause number in the statement, clause_number is that exact number, and official_excerpt is the complete numbered clause of the exact card; action quotes the same specific action in both statement and clause. Individual quote fields are exact {block_id,quote} from provided blocks, or null. The title field quotes a standard title explicitly written in the document itself. If the document only gives a designation, return null for title, even when the official card has a title. For an HTML mention cell, source_text pairs its block ID with its literal source; if statement spans <br>, include the exact <br> in the quote, never replace it with a space. caveats is always an array of exact {block_id,quote} from provided blocks: [] when there are no caveats, and an array with one element when there is one caveat; never null or a single object. For indirect use, clause_source is the exact designation of a referring card, clause_number is its numbered clause, official_excerpt is the entire clause, and action is an exact quote occurring in both the statement and clause. For other direct uses, official_excerpt is the complete card scope. For support or exclusion of an action or method without temperature, action quotes the precise action or method attested by both the statement and scope. An exclusion must quote an explicit exclusion in that scope. outcome is supported, outside_scope or insufficient_evidence. Bibliographic citations alone never establish applicability; do not treat customer thresholds as attributed to the standard. attributed_threshold is a quote for a numerical requirement explicitly attributed to the standard other than the stated temperature, otherwise null. The statement must contain this occurrence at the specified byte range; method and temperature must be within that same statement. Do not infer exclusions from silence. Evaluate object, stage, method and numerical statement, not just similar words.")
        .schema(strict_schema::<Proposal>())
        .field("packet", &sources, &packet)
        .field("source_text", &sources, &source_text)
        .field("mention", &sources, &mention)
        .field("cards", &public, &official_cards)
        .field("candidate_cards", &public, &candidate_cards);
    let request = request.field("referring_cards", &public, &referring_cards);
    let proposal: Proposal = match runtime.step(run, &request).await {
        Ok(p) => p,
        Err(e) => {
            result.outcome = Outcome::TechnicalFailure;
            result.limitation = Some(format!("Проверка употребления не завершена: {e:?}"));
            return result;
        }
    };
    let statement_bound = proposal
        .statement
        .as_ref()
        .and_then(|q| {
            if !packet.blocks.iter().any(|b| b.id == q.block_id) {
                return None;
            }
            if q.block_id == mention.evidence.block_id {
                resolve_statement(index, q)
            } else {
                index.resolve(q.block_id, &q.quote)
            }
        });
    let bind = |q: &Quote| -> Option<Evidence> {
        if !packet.blocks.iter().any(|b| b.id == q.block_id) {
            return None;
        }
        if let Some(statement) = statement_bound
            .as_ref()
            .filter(|e| e.block_id == q.block_id)
        {
            let text = index.source.get(statement.range.clone())?;
            if !q.quote.is_empty()
                && let Some(offset) = text.find(&q.quote)
                && !text[offset + text[offset..].chars().next()?.len_utf8()..].contains(&q.quote)
            {
                let start = statement.range.start + offset;
                let range = start..start + q.quote.len();
                if index.blocks[q.block_id].text_ranges.iter().any(|r| r.start <= range.start && range.end <= r.end) {
                    return Some(Evidence {
                        version: index.version.clone(),
                        block_id: q.block_id,
                        range,
                    });
                }
            }
        }
        index.resolve(q.block_id, &q.quote)
    };
    let Some(mut evidence) = proposal
        .method.iter()
        .chain(proposal.temperature.iter())
        .chain(proposal.title.iter())
        .chain(proposal.caveats.iter())
        .chain(proposal.attributed_threshold.iter())
        .chain(proposal.action.iter())
        .chain(proposal.clause_reference.iter())
        .map(bind)
        .collect::<Option<Vec<_>>>()
    else {
        result.limitation = Some("Цитата модели не найдена в переданном контексте".into());
        return result;
    };
    if proposal.statement.is_some() {
        let Some(statement) = statement_bound.clone() else {
            result.limitation = Some("Цитата модели не найдена в переданном контексте".into());
            return result;
        };
        evidence.insert(0, statement);
    }
    result.evidence = evidence;
    result.role = proposal.role;
    if let Some(title) = &proposal.title
        && statement_bound.as_ref().is_some_and(|statement| {
            statement.block_id == mention.evidence.block_id
                && statement.range.start <= mention.evidence.range.start
                && statement.range.end >= mention.evidence.range.end
                && bind(title).is_some_and(|e| {
                    e.block_id == statement.block_id
                        && e.range.start >= statement.range.start
                        && e.range.end <= statement.range.end
                })
        })
    {
        let candidates: Vec<_> = local_index
            .title_matches(&title.quote)
            .into_iter()
            .filter(|c| c.url.starts_with("https://protect.gost.ru/"))
            .collect();
        result.title_alternatives = candidates.iter().map(|c| c.designation.clone()).collect();
        if let Some(card) = reference.cards.first() {
            if !card.title.eq_ignore_ascii_case(title.quote.trim()) {
                result.action = Some(if candidates.is_empty() {
                    "Заглавие отличается от карточки; поиск по частичному снимку не установил другое обозначение".into()
                } else {
                    format!(
                        "Уточнить намерение: {} или {}",
                        card.designation,
                        result.title_alternatives.join(", ")
                    )
                });
            }
        } else if candidates.len() == 1
            && standards::one_component_variant(&mention.key, &candidates[0].designation)
        {
            result.action = Some(format!(
                "Возможная замена обозначения: {}",
                candidates[0].designation
            ));
        }
    }
    let title_evidence = proposal.title.as_ref().and_then(bind);
    if matches!(result.role, Role::Indirect) {
        let Some(statement) = statement_bound.as_ref() else {
            return result;
        };
        let Some(action) = proposal.action.as_ref().and_then(bind) else {
            return result;
        };
        if statement.block_id != mention.evidence.block_id
            || statement.range.start > mention.evidence.range.start
            || statement.range.end < mention.evidence.range.end
            || action.block_id != statement.block_id
            || action.range.start < statement.range.start
            || action.range.end > statement.range.end
            || reference.cards.len() != 1
            || mention.key.year.is_none()
            || !standards::matches_reference(&reference.cards[0], &mention.key)
            || proposal.attributed_threshold.is_some()
        {
            return result;
        }
        let Some(source) = proposal.clause_source.as_deref() else {
            return result;
        };
        let source_index = match MarkdownIndex::new(source) {
            Ok(i) => i,
            Err(_) => return result,
        };
        let source_scan = standards::scan(&source_index);
        if source_scan.mentions.len() != 1
            || source_scan.mentions[0].original != source
            || source_scan.mentions[0].key.year.is_none()
        {
            return result;
        }
        let source_reviews = catalog.review(&source_scan, &reference.assessment_date);
        let source_review = &source_reviews[0];
        if source_review.cards.len() != 1
            || !matches!(source_review.date_status.as_slice(), [standards::DateStatus::SnapshotStatus(s)] if s == "Действует")
        {
            return result;
        }
        let card = &source_review.cards[0];
        if !card.url.starts_with("https://protect.gost.ru/") {
            return result;
        }
        let Some(clause) = card.clauses.iter().find(|clause| {
            Some(clause.number.as_str()) == proposal.clause_number.as_deref()
                && Some(clause.text.as_str()) == proposal.official_excerpt.as_deref()
                && MarkdownIndex::new(&clause.text).ok().is_some_and(|index| {
                    standards::scan(&index)
                        .mentions
                        .iter()
                        .any(|m| m.key == mention.key)
                })
                && clause.text.contains(&index.source[action.range.clone()])
        }) else {
            return result;
        };
        if matches!(proposal.outcome, Outcome::Supported) {
            result.outcome = Outcome::Supported;
            result.official_url = Some(card.url.clone());
            result.official_field = Some(format!("Пункт {}", clause.number));
            result.official_excerpt = Some(clause.text.clone());
            result.limitation = None;
        }
        return result;
    }
    let candidate = if reference.cards.is_empty() && mention.key.year.is_some() {
        proposal.title.as_ref().and_then(|title| {
            let (title_bound, statement) = (title_evidence.as_ref()?, statement_bound.as_ref()?);
            if title_bound.block_id != statement.block_id || statement.block_id != mention.evidence.block_id
                || title_bound.range.start < statement.range.start || title_bound.range.end > statement.range.end
                || statement.range.start > mention.evidence.range.start || statement.range.end < mention.evidence.range.end {
                return None;
            }
            let matches: Vec<_> = local_index.title_matches(&title.quote).into_iter()
                .filter(|c| c.url.starts_with("https://protect.gost.ru/")).collect();
            let card = (matches.len() == 1).then(|| matches[0])?;
            if !card.url.starts_with("https://protect.gost.ru/")
                || !standards::one_component_variant(&mention.key, &card.designation)
            { return None; }
            let candidate_index = MarkdownIndex::new(&card.designation).ok()?;
            let key = standards::scan(&candidate_index).mentions.into_iter().next()?.key;
            // Changing a dated edition is a separate decision, not a typo correction.
            if key.year != mention.key.year { return None; }
            let mut corrected = mention.clone();
            corrected.key = key;
            let checked = catalog.review(&standards::Scan { mentions: vec![corrected.clone()], unread_ranges: vec![] }, &reference.assessment_date);
            if !matches!(checked[0].date_status.as_slice(), [standards::DateStatus::SnapshotStatus(status)] if status == "Действует") { return None; }
            Some((card, corrected))
        })
    } else {
        None
    };
    let exact_card = reference.cards.first().filter(|c| {
        reference.cards.len() == 1
            && mention.key.year.is_some()
            && c.url.starts_with("https://protect.gost.ru/")
            && standards::matches_reference(c, &mention.key)
    });
    let Some(card) = exact_card.or_else(|| candidate.as_ref().map(|(card, _)| *card)) else {
        return result;
    };
    result.official_url = Some(card.url.clone());
    // An independently cited numbered clause can establish the title of this exact edition.
    if exact_card.is_some()
        && matches!(result.role, Role::Direct)
        && matches!(proposal.outcome, Outcome::Supported)
        && matches!(reference.date_status.as_slice(), [standards::DateStatus::SnapshotStatus(status)] if status == "Действует")
        && let (Some(title), Some(statement), Some(clause_ref), Some(action), Some(number)) = (
            title_evidence.as_ref(),
            statement_bound.as_ref(),
            proposal.clause_reference.as_ref().and_then(bind),
            proposal.action.as_ref().and_then(bind),
            proposal.clause_number.as_deref(),
        )
        && [title, statement, &clause_ref, &action]
            .iter()
            .all(|e| e.block_id == mention.evidence.block_id)
        && statement.range.start <= mention.evidence.range.start
        && statement.range.end >= mention.evidence.range.end
        && [title, &clause_ref, &action]
            .iter()
            .all(|e| e.range.start >= statement.range.start && e.range.end <= statement.range.end)
        && [&clause_ref, &action]
            .iter()
            .all(|e| e.range.end <= title.range.start || e.range.start >= title.range.end)
        && index.source.get(clause_ref.range.clone()) == Some(number)
        && !index.source[statement.range.clone()]
            .to_lowercase()
            .contains(" или ")
        && let Some(clause) = card.clauses.iter().find(|c| {
            c.number == number
                && Some(c.text.as_str()) == proposal.official_excerpt.as_deref()
                && !index.source[action.range.clone()].trim().is_empty()
                && c.text.contains(&index.source[action.range.clone()])
        })
    {
        let actual = &index.source[title.range.clone()];
        if actual.eq_ignore_ascii_case(&card.title) {
            result.outcome = Outcome::Supported;
            result.action = None;
            result.official_field = Some(format!("Пункт {}", clause.number));
            result.official_excerpt = Some(clause.text.clone());
            result.limitation = None;
            return result;
        }
        // Restrict automatic edits to one changed character, not a different title or paraphrase.
        let prefix = actual
            .chars()
            .zip(card.title.chars())
            .take_while(|(a, b)| a == b)
            .count();
        let a: Vec<_> = actual.chars().collect();
        let b: Vec<_> = card.title.chars().collect();
        let suffix = a[prefix..]
            .iter()
            .rev()
            .zip(b[prefix..].iter().rev())
            .take_while(|(x, y)| x == y)
            .count();
        if a.len() - prefix - suffix <= 1
            && b.len() - prefix - suffix <= 1
            && result.title_alternatives.is_empty()
        {
            result.official_field = Some(format!("Пункт {}", clause.number));
            result.official_excerpt = Some(clause.text.clone());
            result.action = None;
            result.editorial_correction = Some(EditorialCorrection {
                card: card.clone(),
                title: title.clone(),
                statement: statement.clone(),
                method: None,
                temperature: None,
                clause: Some(clause_ref),
                action: Some(action),
            });
            return result;
        }
    }
    let Some(excerpt) = proposal
        .official_excerpt
        .filter(|s| !s.trim().is_empty() && card.scope.contains(s))
    else {
        return result;
    };
    result.official_field = Some("Область применения".into());
    result.official_excerpt = Some(excerpt);
    if result.official_excerpt.as_deref() != Some(card.scope.as_str()) {
        return result;
    }
    if !matches!(result.role, Role::Direct) {
        return result;
    }
    let Some(statement) = statement_bound.as_ref() else {
        return result;
    };
    if statement.block_id != mention.evidence.block_id
        || statement.range.start > mention.evidence.range.start
        || statement.range.end < mention.evidence.range.end
    {
        return result;
    }
    if proposal
        .method
        .iter()
        .chain(proposal.temperature.iter())
        .chain(proposal.attributed_threshold.iter())
        .chain(proposal.action.iter())
        .any(|q| {
            bind(q).is_none_or(|e| {
                e.block_id != statement.block_id
                    || e.range.start < statement.range.start
                    || e.range.end > statement.range.end
            })
        })
    {
        return result;
    }
    // The snapshot contains only a scope, not the numbered clause needed to support a separate threshold.
    if proposal.attributed_threshold.is_some() {
        return result;
    }
    // Temperature is compared locally: exact quotation, same occurrence, method and units required.
    if let (Some(method), Some(temp)) = (&proposal.method, &proposal.temperature) {
        if result.official_excerpt.as_deref() != Some(card.scope.as_str()) {
            return result;
        }
        if let (Some(m), Some(t)) = (bind(method), bind(temp)) {
            let checked_mention = candidate
                .as_ref()
                .map_or(mention, |(_, corrected)| corrected);
            let comparison = standards::compare_temperature(index, checked_mention, card, m, t);
            match comparison.outcome {
                standards::Applicability::SupportedWithinComparedConditions
                    if matches!(proposal.outcome, Outcome::Supported) =>
                {
                    result.outcome = Outcome::Supported
                }
                standards::Applicability::OutsideTemperatureRange => {
                    result.outcome = Outcome::OutsideScope
                }
                _ => (),
            }
        }
    } else if let Some(action) = &proposal.action {
        let action = action.quote.trim().to_lowercase();
        let scope = card.scope.to_lowercase();
        if !action.is_empty() && scope.contains(&action) {
            let excluded = ["не распространяется на ", "не применяется к ", "исключает "]
                .iter()
                .any(|prefix| scope.contains(&format!("{prefix}{action}")));
            if excluded && matches!(proposal.outcome, Outcome::OutsideScope) {
                result.outcome = Outcome::OutsideScope;
            } else if !excluded
                && matches!(proposal.outcome, Outcome::Supported)
                && [
                    "распространяется на ",
                    "устанавливает метод ",
                    "применяется к ",
                ]
                .iter()
                .any(|prefix| {
                    scope.starts_with(&format!("{prefix}{action}"))
                        || scope.contains(&format!(" стандарт {prefix}{action}"))
                })
            {
                result.outcome = Outcome::Supported;
            }
        }
    }
    if candidate.is_some()
        && matches!(result.outcome, Outcome::Supported)
        && let (Some(title), Some(statement), Some(method), Some(temperature)) = (
            title_evidence,
            statement_bound.as_ref(),
            proposal.method.as_ref().and_then(bind),
            proposal.temperature.as_ref().and_then(bind),
        )
        && title.block_id == statement.block_id
        && title.range.start >= statement.range.start
        && title.range.end <= statement.range.end
    {
        result.editorial_correction = Some(EditorialCorrection {
            card: card.clone(),
            title,
            statement: statement.clone(),
            method: Some(method),
            temperature: Some(temperature),
            clause: None,
            action: None,
        });
        // The corrected candidate is supported; the original designation remains unresolved.
        result.outcome = Outcome::InsufficientEvidence;
    }
    if matches!(result.outcome, Outcome::Supported | Outcome::OutsideScope) {
        result.limitation = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ErrorCategory;
    use std::time::Duration;

    struct Mock(String);
    impl Model for Mock {
        async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
            Ok(self.0.clone())
        }
    }
    #[tokio::test]
    async fn converted_docx_statement_preserves_literal_break() {
        let source = "<table><tr><td>Формат бумаги: А4 <br>Марка бумаги: С<br>Количество листов в пачке: 500 (±2%)<br>Цвет: белый<br>Белизна CIE: 146 ± 3%<br>Масса листа бумаги площадью 1 м2: 80 ± 3 г/м2<br>Толщина: 104 ± 2 мкм<br>Соответствие ГОСТ Р 57641-2017</td></tr></table>";
        let index = MarkdownIndex::new(source).unwrap();
        let catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let rows = catalog.review(&standards::scan(&index), "2026-11-13");
        let row = rows
            .iter()
            .find(|r| r.mention.original == "ГОСТ Р 57641-2017")
            .unwrap();
        let id = row.mention.evidence.block_id;
        let full = "Толщина: 104 ± 2 мкм<br>Соответствие ГОСТ Р 57641-2017";
        let short = "Соответствие ГОСТ Р 57641-2017";
        let normalized = full.replace("<br>", " ");
        let packet = index.packet_for(&[id]);
        assert!(packet.blocks.iter().any(|b| b.id == id));
        let serialized = serde_json::to_value(&packet).unwrap();
        let transmitted = serialized["blocks"].as_array().unwrap().iter().find(|b| b["id"] == id).unwrap();
        assert!(transmitted["text"].as_array().unwrap().iter().all(|s| !s.as_str().unwrap().contains("<br>")));
        for (statement, caveat, valid) in [
            (full, None, true), (short, None, true), (normalized.as_str(), None, false),
            (full, Some("<br>"), false), (full, Some("br"), false),
            (full, Some("Толщина: 104 ± 2 мкм"), true),
            (full, Some("Соответствие ГОСТ Р 57641-2017"), true),
        ] {
            let answer = serde_json::json!({"role":"direct", "title":null,
                "statement":{"block_id":id,"quote":statement}, "method":null, "temperature":null,
                "caveats":caveat.map(|s| vec![serde_json::json!({"block_id":id,"quote":s})]).unwrap_or_default(), "attributed_threshold":null, "official_excerpt":null,
                "outcome":"insufficient_evidence"})
            .to_string();
            let runtime = Runtime::new(Mock(answer), Duration::from_secs(1));
            let result = evaluate(
                &index,
                row,
                &catalog,
                &runtime,
                &ReviewRun::new(1),
                Visibility::Public,
            )
            .await;
            assert_eq!(matches!(result.role, Role::Direct), valid);
            if valid {
                assert_eq!(&index.source[result.evidence[0].range.clone()], statement);
            }
        }
    }
    #[tokio::test]
    async fn direct_verdict_uses_the_bound_multiline_statement() {
        let mut catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        )).unwrap();
        // A synthetic scope keeps this test independent of the real catalogue.
        catalog.cards[0].scope = "Настоящий стандарт распространяется на испытания.".into();
        let statement = "испытания<br>по ГОСТ 9013-59";
        let source = format!("<table><tr><td>{statement}</td></tr></table>");
        let index = MarkdownIndex::new(&source).unwrap();
        let row = catalog.review(&standards::scan(&index), "2026-11-13").remove(0);
        let quote = |text: &str| serde_json::json!({"block_id": row.mention.evidence.block_id, "quote": text});
        let answer = serde_json::json!({
            "role": "direct", "outcome": "supported", "statement": quote(statement),
            "action": quote("испытания"), "caveats": [], "official_excerpt": catalog.cards[0].scope
        });
        let runtime = Runtime::new(Mock(answer.to_string()), Duration::from_secs(1));
        let result = evaluate(&index, &row, &catalog, &runtime, &ReviewRun::new(1), Visibility::Public).await;
        assert!(matches!(result.outcome, Outcome::Supported));
        assert!(result.limitation.is_none());
    }

    #[test]
    fn statement_break_binding_excludes_markup_and_ambiguous_matches() {
        for (source, quote) in [
            ("<table><tr><td>а<br>а<br>а</td></tr></table>", "а<br>а"),
            ("<table><tr><td>а<br>б а<br>б</td></tr></table>", "а<br>б"),
            ("<table><tr><td>а<br>б</td></tr></table>", "<br>б"),
            ("<table><tr><td>а<br>б</td></tr></table>", "а<br>"),
            ("<table><tr><td>а<b>б</b>в</td></tr></table>", "а<b>б</b>в"),
        ] {
            let index = MarkdownIndex::new(source).unwrap();
            let id = index.blocks.iter().find(|b| b.kind == BlockKind::HtmlTableCell).unwrap().id;
            assert!(resolve_statement(&index, &Quote { block_id: id, quote: quote.into() }).is_none());
        }
    }
    #[tokio::test]
    async fn indirect_use_needs_a_recorded_referring_clause() {
        let mut catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        // This card and its clause are artificial and exist only for this test.
        let mut source = catalog.cards[0].clone();
        source.designation = "ГОСТ 9014-59".into();
        source.full_designation = source.designation.clone();
        source.clauses.push(standards::OfficialClause {
            number: "3.1".into(),
            text: "Измерение проводить по ГОСТ 9013-59.".into(),
        });
        catalog.cards.push(source);
        let text = "Измерение проводить по ГОСТ 9013-59.";
        let index = MarkdownIndex::new(text).unwrap();
        let rows = catalog.review(&standards::scan(&index), "2026-11-13");
        let q = |s: &str| serde_json::json!({"block_id":0,"quote":s});
        let answer = serde_json::json!({"role":"indirect", "title":null, "statement":q(text), "method":null, "temperature":null, "caveats":[], "attributed_threshold":null,
            "official_excerpt":text, "clause_source":"ГОСТ 9014-59", "clause_number":"3.1", "action":q("Измерение проводить"), "outcome":"supported"}).to_string();
        let rt = Runtime::new(Mock(answer), Duration::from_secs(1));
        let result = evaluate(
            &index,
            &rows[0],
            &catalog,
            &rt,
            &ReviewRun::new(1),
            Visibility::Public,
        )
        .await;
        assert!(matches!(result.outcome, Outcome::Supported));
        assert_eq!(result.official_field.as_deref(), Some("Пункт 3.1"));
        catalog.cards[1].clauses.clear();
        let result = evaluate(
            &index,
            &rows[0],
            &catalog,
            &rt,
            &ReviewRun::new(1),
            Visibility::Public,
        )
        .await;
        assert!(matches!(result.outcome, Outcome::InsufficientEvidence));
    }
    #[tokio::test]
    async fn different_title_never_authorizes_substitution_of_existing_number() {
        let mut catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        // A fabricated second card tests the decision independently of real catalog facts.
        let mut other = catalog.cards[0].clone();
        other.designation = "ГОСТ 1234-59".into();
        other.full_designation = other.designation.clone();
        other.title = "Иной официальный метод".into();
        catalog.cards.push(other);
        let text = "Иной официальный метод: метод Роквелла при 20 °C по ГОСТ 9013-59";
        let index = MarkdownIndex::new(text).unwrap();
        let rows = catalog.review(&standards::scan(&index), "2026-11-13");
        let q = |s: &str| serde_json::json!({"block_id":0,"quote":s});
        let answer = serde_json::json!({"role":"direct", "title":q("Иной официальный метод"), "statement":q(text), "method":q("Роквелла"), "temperature":q("20 °C"), "caveats":[], "attributed_threshold":null, "official_excerpt":catalog.cards[0].scope, "outcome":"supported"}).to_string();
        let rt = Runtime::new(Mock(answer), Duration::from_secs(1));
        let result = evaluate(
            &index,
            &rows[0],
            &catalog,
            &rt,
            &ReviewRun::new(1),
            Visibility::Public,
        )
        .await;
        assert_eq!(result.title_alternatives, ["ГОСТ 1234-59"]);
        assert!(result.editorial_correction.is_none());
        assert!(result.action.as_deref().unwrap().contains("Уточнить"));
        catalog.cards.pop();
        let result = evaluate(
            &index,
            &rows[0],
            &catalog,
            &rt,
            &ReviewRun::new(1),
            Visibility::Public,
        )
        .await;
        assert!(result.title_alternatives.is_empty());
        assert!(result.editorial_correction.is_none());
        assert!(
            result
                .action
                .as_deref()
                .unwrap()
                .contains("частичному снимку")
        );
    }
    fn answer(statement: &str, temperature: &str, scope: &str, threshold: Option<&str>) -> String {
        let q = |s: &str| serde_json::json!({"block_id":0,"quote":s});
        serde_json::json!({"role":"direct", "title":null, "statement":q(statement),
            "method":q("Роквелла"), "temperature":q(temperature), "caveats":[],
            "attributed_threshold":threshold.map(q), "official_excerpt":scope, "outcome":"supported"}).to_string()
    }
    #[tokio::test]
    async fn same_block_uses_are_separately_bound_and_threshold_needs_clause() {
        let source =
            "Метод Роквелла при 20 °C по ГОСТ 9013-59; метод Роквелла при 50 °C по ГОСТ 9013-59.";
        let index = MarkdownIndex::new(source).unwrap();
        let catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let rows = catalog.review(&standards::scan(&index), "2026-11-13");
        let scope = &catalog.cards[0].scope;
        let first = "Метод Роквелла при 20 °C по ГОСТ 9013-59";
        let second = "метод Роквелла при 50 °C по ГОСТ 9013-59";
        let run = ReviewRun::new(3);
        let rt = Runtime::new(
            Mock(answer(first, "20 °C", scope, None)),
            Duration::from_secs(1),
        );
        let a = evaluate(&index, &rows[0], &catalog, &rt, &run, Visibility::Public).await;
        assert!(matches!(a.outcome, Outcome::Supported));
        let rt = Runtime::new(
            Mock(answer(second, "50 °C", scope, None)),
            Duration::from_secs(1),
        );
        let b = evaluate(&index, &rows[1], &catalog, &rt, &run, Visibility::Public).await;
        assert!(matches!(b.outcome, Outcome::OutsideScope));
        let rt = Runtime::new(
            Mock(answer(first, "20 °C", scope, Some("20 °C"))),
            Duration::from_secs(1),
        );
        let c = evaluate(&index, &rows[0], &catalog, &rt, &run, Visibility::Public).await;
        assert!(matches!(c.outcome, Outcome::InsufficientEvidence));
        let d = evaluate(&index, &rows[1], &catalog, &rt, &run, Visibility::Private).await;
        assert!(matches!(d.outcome, Outcome::TechnicalFailure));
    }
    #[tokio::test]
    async fn typo_needs_exact_title_scope_and_same_edition() {
        let catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let title = &catalog.cards[0].title;
        let source = format!("{}: метод Роквелла при 20 °C по КОСТ 9013-59", title);
        let index = MarkdownIndex::new(&source).unwrap();
        let rows = catalog.review(&standards::scan(&index), "2026-11-13");
        let q = |s: &str| serde_json::json!({"block_id":0,"quote":s});
        let answer = serde_json::json!({"role":"direct", "title":q(title), "statement":q(&source),
            "method":q("Роквелла"), "temperature":q("20 °C"), "caveats":[],
            "attributed_threshold":null, "official_excerpt":catalog.cards[0].scope, "outcome":"supported"}).to_string();
        let rt = Runtime::new(Mock(answer), Duration::from_secs(1));
        let usage = evaluate(
            &index,
            &rows[0],
            &catalog,
            &rt,
            &ReviewRun::new(1),
            Visibility::Public,
        )
        .await;
        assert!(matches!(usage.outcome, Outcome::InsufficientEvidence));
        let patch = super::super::patch::prepare_standard(&index, &rows[0], &usage)
            .unwrap()
            .unwrap();
        assert_eq!((&*patch.expected, &*patch.replacement), ("К", "Г"));
        let applied = super::super::patch::apply(&index, &[patch]).unwrap();
        assert!(applied.source.ends_with("ГОСТ 9013-59"));
        let mut ambiguous = rows[0].clone();
        ambiguous.cards.push(catalog.cards[0].clone());
        assert!(super::super::patch::prepare_standard(&index, &ambiguous, &usage).is_err());
        let mut wrong_year = rows[0].clone();
        wrong_year.mention.key.year = Some("69".into());
        let usage = evaluate(
            &index,
            &wrong_year,
            &catalog,
            &rt,
            &ReviewRun::new(1),
            Visibility::Public,
        )
        .await;
        assert!(usage.editorial_correction.is_none());
    }
}
