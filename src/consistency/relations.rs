//! Grounded cross-property hypotheses; the model proposes relations, code checks them.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    claims::{Claim, Constraint, Operator, Quote},
    investigation::Candidate,
    markdown::{Evidence, MarkdownIndex},
    numeric,
};
use crate::{
    core::Visibility,
    runtime::{ReviewRun, Runtime, StageKind, StageRequest, StepError, grok::Model},
};

#[derive(Debug, Default, Serialize)]
pub struct RelationsOutcome {
    pub candidates: Vec<Candidate>,
    pub issues: Vec<String>,
}

#[derive(Deserialize)]
struct Response {
    relations: Vec<Relation>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Relation {
    Choice {
        choice: usize,
        options: Vec<Quote>,
        exclusions: Vec<Exclusion>,
        relation: Vec<Quote>,
    },
    Deadline {
        first: usize,
        second: usize,
        origin: Vec<Quote>,
        order: Quote,
    },
    Drawing {
        claims: Vec<usize>,
        requirement: Quote,
    },
}

#[derive(Deserialize)]
struct Exclusion {
    claim: usize,
    option: usize,
    evidence: Quote,
}

/// A clue warrants a scoped relation proposal; numbers alone never do.
pub fn relevant(claims: &[Claim]) -> bool {
    claims.len() >= 2
        && claims.iter().any(|c| {
            let text = c.statement.text.to_lowercase();
            [
                " или ",
                " либо ",
                "после",
                "чертеж",
                "чертёж",
                " or ",
                "after",
                "drawing",
            ]
            .iter()
            .any(|word| text.contains(word))
        })
}

/// Ask about one object, including the source blocks and their heading/table context.
pub async fn generate<M: Model>(
    runtime: &Runtime<M>,
    run: &ReviewRun,
    index: &MarkdownIndex,
    visibility: Visibility,
    claims: &[Claim],
    max_bytes: usize,
) -> Result<RelationsOutcome, StepError> {
    if !relevant(claims) {
        return Ok(RelationsOutcome::default());
    }
    if claims
        .iter()
        .any(|c| !c.complete || c.statement.evidence.version != index.version)
    {
        return Err(StepError::Preparation);
    }
    let mut ids = Vec::new();
    for c in claims {
        let Some(block) = index.blocks.get(c.statement.evidence.block_id) else {
            return Err(StepError::Preparation);
        };
        ids.push(block.id);
        ids.extend(&block.headings);
        ids.extend(&block.table_context);
    }
    ids.sort_unstable();
    ids.dedup();
    let sources: Vec<_> = ids
        .iter()
        .filter_map(|&id| {
            index
                .blocks
                .get(id)
                .map(|b| (id, &index.source[b.range.clone()]))
        })
        .collect();
    if serde_json::to_vec(&(claims, &sources))
        .map_err(|_| StepError::Preparation)?
        .len()
        > max_bytes
    {
        return Err(StepError::Preparation);
    }
    let visibilities = [visibility];
    let request = StageRequest::new(StageKind::Comparison, "Propose only explicitly documented relationships in JSON {relations:[...]}. Use zero-based claim indices and exact source {block_id,quote} quotations. choice: {kind:'choice',choice,options:[quotes],exclusions:[{claim,option,evidence:quote}],relation:[quotes]}; options are all distinct verbatim alternatives from the choice, each exclusion explicitly excludes that exact option. deadline: {kind:'deadline',first,second,origin:[quotes],order:quote}; first and second are numeric deadlines from the same explicit origin, order names both events and states the second event is strictly after the first. drawing: {kind:'drawing',claims:[indices],requirement:quote}; only where a drawing is explicitly required to reconcile the claims and is missing from supplied material. Do not infer links from proximity or values. Omit unsupported relationships.")
        .field("claims", &visibilities, claims).field("sources", &visibilities, &sources);
    let response: Response = runtime.step(run, &request).await?;
    Ok(evaluate(index, claims, &ids, response))
}

fn evaluate(
    index: &MarkdownIndex,
    claims: &[Claim],
    allowed: &[usize],
    response: Response,
) -> RelationsOutcome {
    let mut out = RelationsOutcome::default();
    for relation in response.relations {
        let proposal = match relation {
            Relation::Choice {
                choice,
                options,
                exclusions,
                relation,
            } => {
                let valid = claims.get(choice).filter(|c| c.complete);
                let option_evidence: Option<Vec<_>> =
                    options.iter().map(|q| quote(index, allowed, q)).collect();
                let links: Option<Vec<_>> =
                    relation.iter().map(|q| quote(index, allowed, q)).collect();
                let mut sides = vec![choice];
                let mut excluded = vec![false; options.len()];
                let mut extra = Vec::new();
                let mut ok = valid.is_some()
                    && options.len() >= 2
                    && option_evidence.is_some()
                    && links.as_ref().is_some_and(|v| !v.is_empty());
                if let Some(c) = valid {
                    ok &= option_evidence
                        .as_ref()
                        .is_some_and(|v| v.iter().all(|e| contains(&c.statement.evidence, e)));
                    ok &= links
                        .as_ref()
                        .is_some_and(|v| v.iter().any(|e| contains(&c.statement.evidence, e)));
                    let text = c.statement.text.to_lowercase();
                    let separators = text.matches(" или ").count()
                        + text.matches(" либо ").count()
                        + text.matches(" or ").count();
                    ok &= !text.contains(',') && separators == options.len().saturating_sub(1);
                    ok &= options.iter().enumerate().all(|(i, q)| {
                        !options[..i]
                            .iter()
                            .any(|p| p.quote.trim().eq_ignore_ascii_case(q.quote.trim()))
                    });
                }
                for e in exclusions {
                    let bound = quote(index, allowed, &e.evidence);
                    let c = claims.get(e.claim).filter(|c| c.complete);
                    if let (Some(c), Some(ev), Some(opt)) = (c, bound, options.get(e.option)) {
                        if e.claim == choice
                            || valid.is_none_or(|v| !same_conditions(c, v))
                            || !contains(&c.statement.evidence, &ev)
                            || !["исключ", "запрещ", "не допуска", "exclude", "prohibit"]
                                .iter()
                                .any(|s| e.evidence.quote.to_lowercase().contains(s))
                            || !e
                                .evidence
                                .quote
                                .to_lowercase()
                                .contains(&opt.quote.to_lowercase())
                        {
                            ok = false;
                            continue;
                        }
                        excluded[e.option] = true;
                        sides.push(e.claim);
                        extra.push(ev);
                    } else {
                        ok = false;
                    }
                }
                if ok && excluded.iter().all(|x| *x) {
                    extra.extend(links.unwrap());
                    Some(("choice", sides, extra))
                } else if ok {
                    continue;
                } else {
                    None
                }
            }
            Relation::Deadline {
                first,
                second,
                origin,
                order,
            } => {
                let (Some(a), Some(b)) = (claims.get(first), claims.get(second)) else {
                    out.issues.push("invalid deadline claims".into());
                    continue;
                };
                let origins: Option<Vec<_>> =
                    origin.iter().map(|q| quote(index, allowed, q)).collect();
                let Some(ord) = quote(index, allowed, &order) else {
                    out.issues.push("unbound sequence requirement".into());
                    continue;
                };
                if first == second
                    || !a.complete
                    || !b.complete
                    || !same_conditions(a, b)
                    || !matches!(
                        &a.constraint,
                        Constraint::Numeric {
                            operator: Operator::Exact,
                            ..
                        }
                    )
                    || !matches!(
                        &b.constraint,
                        Constraint::Numeric {
                            operator: Operator::AtMost | Operator::LessThan,
                            ..
                        }
                    )
                    || a.mandatory.is_none()
                    || b.mandatory.is_none()
                    || origins.as_ref().is_none_or(|v| {
                        v.len() < 2
                            || !v.iter().any(|e| contains(&a.statement.evidence, e))
                            || !v.iter().any(|e| contains(&b.statement.evidence, e))
                    })
                    || origin.len() != 2
                    || !origin[0]
                        .quote
                        .trim()
                        .eq_ignore_ascii_case(origin[1].quote.trim())
                    || !ordered_events(&order.quote, &a.property.text, &b.property.text)
                {
                    None
                } else if let (Some(mut x), Some(y)) = (day_constraint(a), day_constraint(b)) {
                    x.operator = numeric::Operator::GreaterThan;
                    match numeric::overlaps(&x, &y) {
                        Some(false) => {
                            let mut e = origins.unwrap();
                            e.push(ord);
                            Some(("deadline", vec![first, second], e))
                        }
                        Some(true) => continue,
                        None => None,
                    }
                } else {
                    None
                }
            }
            Relation::Drawing {
                claims: sides,
                requirement,
            } => {
                let q = quote(index, allowed, &requirement);
                if sides.len() < 2
                    || sides
                        .iter()
                        .any(|&i| claims.get(i).is_none_or(|c| !c.complete))
                    || !["чертеж", "чертёж", "drawing"]
                        .iter()
                        .any(|s| requirement.quote.to_lowercase().contains(s))
                    || !["соглас", "reconcile", "coordinate"]
                        .iter()
                        .any(|s| requirement.quote.to_lowercase().contains(s))
                {
                    None
                } else {
                    q.map(|requirement| ("drawing", sides, vec![requirement]))
                }
            }
        };
        let Some((category, mut sides, extra)) = proposal else {
            out.issues.push("invalid or unsupported relation".into());
            continue;
        };
        sides.sort_unstable();
        sides.dedup();
        if sides.len() < 2 {
            out.issues.push("relation needs distinct claims".into());
            continue;
        }
        let mut evidence: Vec<Evidence> = sides
            .iter()
            .map(|&i| claims[i].statement.evidence.clone())
            .chain(extra)
            .collect();
        evidence.sort_by_key(|e| (e.range.start, e.range.end));
        evidence.dedup();
        let mut context: Vec<_> = sides
            .iter()
            .flat_map(|&i| super::semantic::claim_evidence(&claims[i]))
            .filter(|address| !evidence.contains(address))
            .collect();
        context.sort_by_key(|e| (e.range.start, e.range.end));
        context.dedup();
        let id = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(category, &evidence)).expect("serializable evidence")
            )
        );
        let c = Candidate {
            id,
            category: category.into(),
            object: claims[sides[0]].object.key.clone().unwrap_or_default(),
            property: claims[sides[0]].property.text.clone(),
            evidence,
            context,
            known_gaps: vec![],
        };
        if !out.candidates.iter().any(|old| old.id == c.id) {
            out.candidates.push(c);
        }
    }
    out
}

fn quote(index: &MarkdownIndex, allowed: &[usize], q: &Quote) -> Option<Evidence> {
    allowed
        .contains(&q.block_id)
        .then(|| index.resolve(q.block_id, &q.quote))
        .flatten()
}
fn contains(outer: &Evidence, inner: &Evidence) -> bool {
    outer.block_id == inner.block_id
        && outer.range.start <= inner.range.start
        && inner.range.end <= outer.range.end
}

fn names_event(text: &str, name: &str) -> bool {
    let name = name.trim().to_lowercase();
    let stem: String = name.chars().take(5).collect();
    stem.chars().count() >= 4 && text.to_lowercase().contains(&stem)
}

fn ordered_events(text: &str, first: &str, second: &str) -> bool {
    let text = text.to_lowercase();
    ["после", "after"].iter().any(|separator| {
        text.split_once(separator)
            .is_some_and(|(before, after)| names_event(before, second) && names_event(after, first))
    })
}

fn same_conditions(a: &Claim, b: &Claim) -> bool {
    let normalized = |c: &Claim| {
        let mut values: Vec<_> = c
            .conditions
            .iter()
            .map(|g| g.text.trim().to_lowercase())
            .collect();
        values.sort();
        values
    };
    normalized(a) == normalized(b)
}

fn day_constraint(c: &Claim) -> Option<numeric::Constraint> {
    let Constraint::Numeric {
        operator,
        value,
        unit: Some(unit),
        ..
    } = &c.constraint
    else {
        return None;
    };
    if !matches!(
        unit.text.trim().to_lowercase().as_str(),
        "дней" | "дня" | "день" | "суток" | "сутки" | "сут" | "day" | "days"
    ) {
        return None;
    }
    let operator = match operator {
        Operator::Exact => numeric::Operator::Equal,
        Operator::AtMost => numeric::Operator::AtMost,
        Operator::LessThan => numeric::Operator::LessThan,
        _ => return None,
    };
    let mut parts = value.text.trim().split(['.', ',']);
    let whole = parts.next()?;
    let fraction = parts.next().unwrap_or("");
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || parts.next().is_some()
    {
        return None;
    }
    let coefficient = format!("{whole}{fraction}").parse().ok()?;
    Some(numeric::Constraint {
        value: numeric::Decimal {
            coefficient,
            scale: u32::try_from(fraction.len()).ok()?,
        },
        operator,
        unit: numeric::Unit {
            dimension: "days".into(),
            multiplier: numeric::Decimal {
                coefficient: 1,
                scale: 0,
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consistency::claims::{Grounded, Object};

    fn q(block_id: usize, quote: &str) -> Quote {
        Quote {
            block_id,
            quote: quote.into(),
        }
    }
    fn claim(index: &MarkdownIndex, block: usize, text: &str, number: Option<&str>) -> Claim {
        let g = |text: &str| Grounded {
            text: text.into(),
            evidence: index.resolve(block, text).unwrap(),
        };
        Claim {
            complete: true,
            statement: g(text),
            object: Object {
                key: Some("изделие".into()),
                evidence: vec![],
                alternatives: vec![],
            },
            property: if number.is_some() {
                g(if block == 0 {
                    "поставка"
                } else {
                    "монтаж"
                })
            } else {
                g("изделие")
            },
            constraint: match number {
                Some(n) => Constraint::Numeric {
                    operator: if block == 0 {
                        Operator::Exact
                    } else {
                        Operator::AtMost
                    },
                    operator_evidence: g("изделие"),
                    value: g(n),
                    unit: Some(g("дней")),
                },
                None => Constraint::Semantic { text: g(text) },
            },
            stage: None,
            conditions: vec![],
            mandatory: Some(g("изделие")),
        }
    }
    #[test]
    fn choice_requires_all_grounded_exclusions() {
        let texts = [
            "изделие вариант Альфа или Бета",
            "изделие исключить Альфа",
            "изделие исключить Бета",
        ];
        let index = MarkdownIndex::new(&texts.join("\n\n")).unwrap();
        let claims: Vec<_> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| claim(&index, i, t, None))
            .collect();
        let proposal = |both| Relation::Choice {
            choice: 0,
            options: vec![q(0, "Альфа"), q(0, "Бета")],
            exclusions: if both {
                vec![
                    Exclusion {
                        claim: 1,
                        option: 0,
                        evidence: q(1, "исключить Альфа"),
                    },
                    Exclusion {
                        claim: 2,
                        option: 1,
                        evidence: q(2, "исключить Бета"),
                    },
                ]
            } else {
                vec![Exclusion {
                    claim: 1,
                    option: 0,
                    evidence: q(1, "исключить Альфа"),
                }]
            },
            relation: vec![q(0, "или")],
        };
        assert!(
            evaluate(
                &index,
                &claims,
                &[0, 1, 2],
                Response {
                    relations: vec![proposal(false)]
                }
            )
            .candidates
            .is_empty()
        );
        let result = evaluate(
            &index,
            &claims,
            &[0, 1, 2],
            Response {
                relations: vec![proposal(true)],
            },
        );
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(
            result.candidates[0]
                .evidence
                .iter()
                .filter(|e| e.range == claims[e.block_id].statement.evidence.range)
                .count(),
            3
        );
        let invalid = Relation::Choice {
            choice: 99,
            options: vec![q(0, "Альфа"), q(0, "Бета")],
            exclusions: vec![Exclusion {
                claim: 1,
                option: 0,
                evidence: q(1, "исключить Альфа"),
            }],
            relation: vec![q(0, "или")],
        };
        assert!(
            evaluate(
                &index,
                &claims,
                &[0, 1, 2],
                Response {
                    relations: vec![invalid]
                }
            )
            .candidates
            .is_empty()
        );
        let false_exclusion = Relation::Choice {
            choice: 0,
            options: vec![q(0, "Альфа"), q(0, "Бета")],
            exclusions: vec![
                Exclusion {
                    claim: 1,
                    option: 0,
                    evidence: q(1, "Альфа"),
                },
                Exclusion {
                    claim: 2,
                    option: 1,
                    evidence: q(2, "исключить Бета"),
                },
            ],
            relation: vec![q(0, "или")],
        };
        assert!(
            evaluate(
                &index,
                &claims,
                &[0, 1, 2],
                Response {
                    relations: vec![false_exclusion]
                }
            )
            .candidates
            .is_empty()
        );
    }
    #[test]
    fn deadlines_need_explicit_relationship_and_check_bounds() {
        for (deadline, expected) in [("40", 1), ("60", 0)] {
            let first = "изделие поставка от договора ровно 50 дней";
            let second = format!("изделие монтаж от договора не позднее {deadline} дней");
            let order = "изделие монтаж после поставки";
            let index = MarkdownIndex::new(&format!("{first}\n\n{second}\n\n{order}")).unwrap();
            let claims = vec![
                claim(&index, 0, first, Some("50")),
                claim(&index, 1, &second, Some(deadline)),
                claim(&index, 2, order, None),
            ];
            let response = Response {
                relations: vec![Relation::Deadline {
                    first: 0,
                    second: 1,
                    origin: vec![q(0, "от договора"), q(1, "от договора")],
                    order: q(2, "монтаж после поставки"),
                }],
            };
            assert_eq!(
                evaluate(&index, &claims, &[0, 1, 2], response)
                    .candidates
                    .len(),
                expected
            );
            assert!(!relevant(&claims[..2]));
            let fabricated = Response {
                relations: vec![Relation::Deadline {
                    first: 0,
                    second: 1,
                    origin: vec![q(0, "от договора"), q(1, "дней")],
                    order: q(2, "монтаж после поставки"),
                }],
            };
            assert!(
                evaluate(&index, &claims, &[0, 1, 2], fabricated)
                    .candidates
                    .is_empty()
            );
        }
    }

    #[test]
    fn dimensions_only_require_a_drawing_when_the_document_says_so() {
        let shaft = "изделие ширина шахты 2000 мм";
        let cabin = "изделие ширина кабины 1400 мм";
        for (condition, expected) in [
            ("изделие указано в ведомости", false),
            ("изделие: согласовать обе ширины по чертежу", true),
        ] {
            let index = MarkdownIndex::new(&[shaft, cabin, condition].join("\n\n")).unwrap();
            let claims = [shaft, cabin, condition]
                .iter()
                .enumerate()
                .map(|(id, text)| claim(&index, id, text, None))
                .collect::<Vec<_>>();
            let output = evaluate(
                &index,
                &claims,
                &[0, 1, 2],
                Response {
                    relations: vec![Relation::Drawing {
                        claims: vec![0, 1],
                        requirement: q(2, condition),
                    }],
                },
            );
            assert_eq!(relevant(&claims), expected);
            assert_eq!(output.candidates.len(), usize::from(expected));
            if let Some(candidate) = output.candidates.first() {
                assert_eq!(candidate.category, "drawing");
                assert!(candidate.evidence.contains(&claims[2].statement.evidence));
            }
        }
    }

    #[test]
    fn omitted_choice_and_irrelevant_drawing_are_rejected() {
        let texts = [
            "изделие Альфа или Бета или Гамма",
            "изделие исключить Альфа",
            "изделие исключить Бета",
        ];
        let index = MarkdownIndex::new(&texts.join("\n\n")).unwrap();
        let claims: Vec<_> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| claim(&index, i, t, None))
            .collect();
        let omitted = Relation::Choice {
            choice: 0,
            options: vec![q(0, "Альфа"), q(0, "Бета")],
            exclusions: vec![
                Exclusion {
                    claim: 1,
                    option: 0,
                    evidence: q(1, "исключить Альфа"),
                },
                Exclusion {
                    claim: 2,
                    option: 1,
                    evidence: q(2, "исключить Бета"),
                },
            ],
            relation: vec![q(0, "или Бета")],
        };
        let drawing = Relation::Drawing {
            claims: vec![0, 1],
            requirement: q(0, "изделие"),
        };
        assert!(
            evaluate(
                &index,
                &claims,
                &[0, 1, 2],
                Response {
                    relations: vec![omitted, drawing]
                }
            )
            .candidates
            .is_empty()
        );
    }
}
