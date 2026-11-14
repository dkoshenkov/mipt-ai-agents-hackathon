use std::ops::Range;

use serde::Serialize;

use super::{
    ConsistencyReport,
    claims::{Claim, Constraint, Grounded},
    investigation::{Investigation, Verdict},
    markdown::{Evidence, MarkdownIndex},
    priority::{Selection, Target},
    standard_usage::{self, UsageReview},
    standards::{self, Existence, ReferenceReview},
};

#[derive(Clone, Debug, Serialize)]
pub struct Patch {
    pub target: Target,
    pub change: Evidence,
    pub range: Range<usize>,
    pub expected: String,
    pub replacement: String,
    pub basis: Vec<Grounded>,
    pub user_provenance: Option<String>,
    pub source_constraint: Option<Constraint>,
    pub official_basis: Option<standard_usage::EditorialCorrection>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Applied {
    pub source: String,
    pub version: String,
    pub patches: Vec<Patch>,
}

#[derive(Clone, Debug, Serialize)]
pub enum Verification {
    Verified,
    Rejected { reasons: Vec<String> },
    Incomplete { reasons: Vec<String> },
}

fn valid(index: &MarkdownIndex, g: &Grounded) -> bool {
    g.evidence.version == index.version
        && index.resolve(g.evidence.block_id, &g.text).as_ref() == Some(&g.evidence)
}

fn valid_scoped(index: &MarkdownIndex, g: &Grounded, statement: &Grounded) -> bool {
    index
        .resolve_scoped(
            g.evidence.block_id,
            &g.text,
            &statement.text,
            &statement.evidence,
        )
        .as_ref()
        == Some(&g.evidence)
}

/// Builds a replacement only for a uniquely bound, selected requirement.
pub fn prepare(
    index: &MarkdownIndex,
    selection: &Selection,
    claims: &[Claim],
) -> Result<Option<Patch>, String> {
    let Selection::Selected {
        target,
        change,
        source,
        basis,
        user_provenance,
    } = selection
    else {
        return Ok(None);
    };
    let matches: Vec<_> = claims
        .iter()
        .filter(|c| c.complete && c.statement.evidence == *change)
        .collect();
    if target.version != index.version
        || matches.len() != 1
        || !target.sides.iter().any(|e| {
            e == change || (e.range.start >= change.range.start && e.range.end <= change.range.end)
        })
        || !valid(index, &matches[0].statement)
        || !valid(index, &source.statement)
        || basis.iter().any(|g| !valid(index, g))
    {
        return Err("stale or ambiguous selection".into());
    }
    let claim = matches[0];
    let sources: Vec<_> = claims
        .iter()
        .filter(|c| {
            c.complete
                && c.statement == source.statement
                && c.constraint == source.constraint
                && c.conditions == source.conditions
        })
        .collect();
    if sources.len() != 1
        || claim.object.key != sources[0].object.key
        || claim.property.text != sources[0].property.text
        || claim.conditions.iter().map(|g| &g.text).collect::<Vec<_>>()
            != source
                .conditions
                .iter()
                .map(|g| &g.text)
                .collect::<Vec<_>>()
    {
        return Err("source requirement is unbound or has different scope".into());
    }
    let (old_parts, new_parts): (Vec<&Grounded>, Vec<&Grounded>) =
        match (&claim.constraint, &source.constraint) {
            (
                Constraint::Numeric {
                    operator: ao,
                    operator_evidence: aop,
                    value: av,
                    unit: au,
                },
                Constraint::Numeric {
                    operator: bo,
                    operator_evidence: bop,
                    value: bv,
                    unit: bu,
                },
            ) => {
                if !valid_scoped(index, aop, &claim.statement)
                    || !valid_scoped(index, bop, &source.statement)
                    || au
                        .as_ref()
                        .is_none_or(|u| !valid_scoped(index, u, &claim.statement))
                    || bu
                        .as_ref()
                        .is_none_or(|u| !valid_scoped(index, u, &source.statement))
                {
                    return Err("ungrounded operator or unit".into());
                }
                if ao == bo && au.as_ref().map(|u| &u.text) == bu.as_ref().map(|u| &u.text) {
                    (vec![av], vec![bv])
                } else {
                    let (Some(au), Some(bu)) = (au, bu) else {
                        return Err("missing unit".into());
                    };
                    (vec![aop, av, au], vec![bop, bv, bu])
                }
            }
            (Constraint::Semantic { text: a }, Constraint::Semantic { text: b }) => {
                (vec![a], vec![b])
            }
            _ => return Err("incompatible constraints".into()),
        };
    let span = |parts: &[&Grounded], statement: &Grounded| -> Result<Range<usize>, String> {
        if parts.iter().any(|p| {
            !valid_scoped(index, p, statement)
                || p.evidence.range.start < statement.evidence.range.start
                || p.evidence.range.end > statement.evidence.range.end
        }) {
            return Err("constraint outside statement or ungrounded".into());
        }
        Ok(parts.iter().map(|p| p.evidence.range.start).min().unwrap()
            ..parts.iter().map(|p| p.evidence.range.end).max().unwrap())
    };
    let range = span(&old_parts, &claim.statement)?;
    let source_range = span(&new_parts, &source.statement)?;
    // Replacing a whole expression must not erase another grounded part of the requirement.
    if claim
        .object
        .evidence
        .iter()
        .chain(&claim.conditions)
        .chain(&claim.stage)
        .chain(&claim.mandatory)
        .chain(std::iter::once(&claim.property))
        .any(|g| g.evidence.range.start < range.end && range.start < g.evidence.range.end)
    {
        return Err("replacement overlaps requirement context".into());
    }
    Ok(Some(Patch {
        target: target.clone(),
        change: change.clone(),
        expected: index.source[range.clone()].into(),
        replacement: index.source[source_range].into(),
        range,
        basis: basis.clone(),
        user_provenance: user_provenance.clone(),
        source_constraint: Some(source.constraint.clone()),
        official_basis: None,
    }))
}

/// Prepare a single addressed designation edit only from a checked official card.
pub fn prepare_standard(
    index: &MarkdownIndex,
    reference: &ReferenceReview,
    usage: &UsageReview,
) -> Result<Option<Patch>, String> {
    let Some(basis) = &usage.editorial_correction else {
        return Ok(None);
    };
    let mention = &reference.mention;
    if let (Some(clause), Some(action)) = (&basis.clause, &basis.action) {
        let evidence = [&basis.title, &basis.statement, clause, action];
        if mention.evidence != usage.mention
            || mention.evidence.version != index.version
            || index.source.get(mention.evidence.range.clone()) != Some(mention.original.as_str())
            || !matches!(usage.role, standard_usage::Role::Direct)
            || !matches!(usage.outcome, standard_usage::Outcome::InsufficientEvidence)
            || mention.key.year.is_none()
            || reference.cards.len() != 1
            || !standards::matches_reference(&reference.cards[0], &mention.key)
            || reference.cards[0].designation != basis.card.designation
            || reference.cards[0].url != basis.card.url
            || reference.cards[0].title != basis.card.title
            || !basis.card.url.starts_with("https://protect.gost.ru/")
            || !matches!(reference.date_status.as_slice(), [standards::DateStatus::SnapshotStatus(status)] if status == "Действует")
            || usage.official_url.as_deref() != Some(basis.card.url.as_str())
            || !usage.title_alternatives.is_empty()
            || evidence.iter().any(|e| {
                e.version != index.version
                    || e.block_id != mention.evidence.block_id
                    || index.source.get(e.range.clone()).is_none()
                    || e.range.start < basis.statement.range.start
                    || e.range.end > basis.statement.range.end
            })
            || basis.statement.range.start > mention.evidence.range.start
            || basis.statement.range.end < mention.evidence.range.end
            || [clause, action].iter().any(|e| {
                e.range.start < basis.title.range.end && basis.title.range.start < e.range.end
            })
            || !basis.card.clauses.iter().any(|c| {
                index.source.get(clause.range.clone()) == Some(c.number.as_str())
                    && usage.official_excerpt.as_deref() == Some(c.text.as_str())
                    && reference.cards[0]
                        .clauses
                        .iter()
                        .any(|original| original.number == c.number && original.text == c.text)
                    && index
                        .source
                        .get(action.range.clone())
                        .is_some_and(|a| !a.trim().is_empty() && c.text.contains(a))
            })
        {
            return Err("stale or unsupported standard title correction".into());
        }
        let old = &index.source[basis.title.range.clone()];
        let new = &basis.card.title;
        let prefix = old
            .char_indices()
            .zip(new.char_indices())
            .take_while(|(a, b)| a.1 == b.1)
            .map(|(a, _)| a.0 + a.1.len_utf8())
            .last()
            .unwrap_or(0);
        let suffix: usize = old[prefix..]
            .chars()
            .rev()
            .zip(new[prefix..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        let mut start = prefix;
        let mut old_end = old.len() - suffix;
        let mut new_end = new.len() - suffix;
        if old[start..old_end].chars().count() > 1
            || new[start..new_end].chars().count() > 1
            || old == new
        {
            return Err("title correction is not a single-character typo".into());
        }
        if start == old_end {
            if start > 0 {
                start = old[..start].char_indices().next_back().unwrap().0;
            } else if let Some(c) = old[old_end..].chars().next() {
                old_end += c.len_utf8();
                new_end += c.len_utf8();
            } else {
                return Err("empty title".into());
            }
        }
        let range = basis.title.range.start + start..basis.title.range.start + old_end;
        return Ok(Some(Patch {
            target: Target {
                version: index.version.clone(),
                sides: vec![mention.evidence.clone()],
            },
            change: basis.title.clone(),
            expected: index.source[range.clone()].into(),
            range,
            replacement: new[start..new_end].into(),
            basis: vec![],
            user_provenance: None,
            source_constraint: None,
            official_basis: Some(basis.clone()),
        }));
    }
    let (Some(method), Some(temperature)) = (&basis.method, &basis.temperature) else {
        return Err("missing standard correction evidence".into());
    };
    if mention.evidence != usage.mention
        || mention.evidence.version != index.version
        || index.source.get(mention.evidence.range.clone()) != Some(mention.original.as_str())
        || !matches!(usage.role, standard_usage::Role::Direct)
        || !matches!(usage.outcome, standard_usage::Outcome::InsufficientEvidence)
        || !reference.cards.is_empty()
        || !standards::one_component_variant(&mention.key, &basis.card.designation)
        || usage.official_url.as_deref() != Some(basis.card.url.as_str())
        || usage.official_excerpt.as_deref() != Some(basis.card.scope.as_str())
        || [&basis.title, &basis.statement, method, temperature]
            .iter()
            .any(|e| {
                e.version != index.version
                    || e.block_id != mention.evidence.block_id
                    || index.source.get(e.range.clone()).is_none()
            })
        || index
            .source
            .get(basis.title.range.clone())
            .is_none_or(|s| !s.eq_ignore_ascii_case(&basis.card.title))
        || basis.statement.range.start > mention.evidence.range.start
        || basis.statement.range.end < mention.evidence.range.end
        || [&basis.title, method, temperature].iter().any(|e| {
            e.range.start < basis.statement.range.start || e.range.end > basis.statement.range.end
        })
    {
        return Err("stale or unsupported standard correction".into());
    }
    let old = mention.original.as_str();
    let new = basis.card.designation.as_str();
    let prefix = old
        .char_indices()
        .zip(new.char_indices())
        .take_while(|(a, b)| a.1 == b.1)
        .map(|(a, _)| a.0 + a.1.len_utf8())
        .last()
        .unwrap_or(0);
    let old_tail = &old[prefix..];
    let new_tail = &new[prefix..];
    let suffix: usize = old_tail
        .chars()
        .rev()
        .zip(new_tail.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    let mut start = prefix;
    let mut old_end = old.len() - suffix;
    let mut new_end = new.len() - suffix;
    // Include one adjacent character when the typo requires insertion.
    if start == old_end {
        if start > 0 {
            let previous = old[..start].char_indices().next_back().unwrap().0;
            start = previous;
        } else if let Some(c) = old[old_end..].chars().next() {
            old_end += c.len_utf8();
            new_end += c.len_utf8();
        } else {
            return Ok(None);
        }
    }
    let range = mention.evidence.range.start + start..mention.evidence.range.start + old_end;
    Ok(Some(Patch {
        target: Target {
            version: index.version.clone(),
            sides: vec![mention.evidence.clone()],
        },
        change: mention.evidence.clone(),
        expected: index.source[range.clone()].into(),
        range,
        replacement: new[start..new_end].into(),
        basis: vec![],
        user_provenance: None,
        source_constraint: None,
        official_basis: Some(basis.clone()),
    }))
}

/// All preconditions are checked before any bytes are changed.
pub fn apply(index: &MarkdownIndex, patches: &[Patch]) -> Result<Applied, String> {
    let mut ordered = patches.to_vec();
    ordered.sort_by_key(|p| (p.range.start, p.range.end));
    for (i, p) in ordered.iter().enumerate() {
        if p.target.version != index.version
            || p.change.version != index.version
            || p.range.start >= p.range.end
            || p.range.start < p.change.range.start
            || p.range.end > p.change.range.end
            || index.source.get(p.range.clone()) != Some(p.expected.as_str())
            || (i > 0 && ordered[i - 1].range.end > p.range.start)
        {
            return Err("stale, invalid, or overlapping patch".into());
        }
    }
    let mut source = index.source.clone();
    for p in ordered.iter().rev() {
        source.replace_range(p.range.clone(), &p.replacement);
    }
    let version = MarkdownIndex::new(&source)
        .map_err(|_| "empty copy")?
        .version;
    Ok(Applied {
        source,
        version,
        patches: ordered,
    })
}

/// Map a full original range to the corresponding range in the copy.
fn mapped(range: &Range<usize>, patches: &[Patch]) -> Option<Range<usize>> {
    let mut start = range.start as isize;
    let mut end = range.end as isize;
    for p in patches {
        let delta = p.replacement.len() as isize - (p.range.end - p.range.start) as isize;
        if p.range.end <= range.start {
            start += delta;
            end += delta;
        } else if p.range.start >= range.end {
            break;
        } else if range.start <= p.range.start && p.range.end <= range.end {
            end += delta;
        } else {
            return None;
        }
    }
    Some(usize::try_from(start).ok()?..usize::try_from(end).ok()?)
}

fn text(g: &Option<Grounded>) -> Option<&str> {
    g.as_ref().map(|g| g.text.as_str())
}
fn same_context(a: &Claim, b: &Claim) -> bool {
    a.complete
        && b.complete
        && a.object.key == b.object.key
        && a.property.text == b.property.text
        && text(&a.stage) == text(&b.stage)
        && text(&a.mandatory) == text(&b.mandatory)
        && a.conditions.iter().map(|g| &g.text).collect::<Vec<_>>()
            == b.conditions.iter().map(|g| &g.text).collect::<Vec<_>>()
}
fn same_constraint(a: &Constraint, b: &Constraint) -> bool {
    match (a, b) {
        (
            Constraint::Numeric {
                operator: ao,
                value: av,
                unit: au,
                ..
            },
            Constraint::Numeric {
                operator: bo,
                value: bv,
                unit: bu,
                ..
            },
        ) => ao == bo && av.text == bv.text && text(au) == text(bu),
        (Constraint::Semantic { text: a }, Constraint::Semantic { text: b }) => a.text == b.text,
        _ => false,
    }
}
fn involved(inv: &Investigation, claim: &Claim) -> bool {
    inv.candidate.evidence.iter().any(|e| {
        e.range.start >= claim.statement.evidence.range.start
            && e.range.end <= claim.statement.evidence.range.end
    })
}
fn confirmed(inv: &Investigation) -> bool {
    matches!(inv.outcome, Ok(Verdict::Confirmed { .. }))
}

/// Compare by mapped requirement addresses, never by the number of findings.
pub fn verify(
    before: &ConsistencyReport,
    after: &ConsistencyReport,
    applied: &Applied,
) -> Verification {
    let mut rejected = Vec::new();
    let mut incomplete = Vec::new();
    if before.version
        != applied
            .patches
            .first()
            .map_or(before.version.as_str(), |p| p.target.version.as_str())
        || after.version != applied.version
    {
        return Verification::Rejected {
            reasons: vec!["report version mismatch".into()],
        };
    }
    let coverage_incomplete = !after.calculate_standard_coverage().complete
        || !after.semantic_gaps.is_empty()
        || after.investigations.iter().any(|i| {
            !matches!(
                i.outcome,
                Ok(Verdict::Confirmed { .. } | Verdict::Dismissed { .. })
            )
        });
    let mut pairs = Vec::new();
    for old in &before.claims {
        let Some(range) = mapped(&old.statement.evidence.range, &applied.patches) else {
            rejected.push("overlapping requirement address".into());
            continue;
        };
        let found: Vec<_> = after
            .claims
            .iter()
            .filter(|c| c.statement.evidence.range == range)
            .collect();
        if found.len() != 1 {
            let message = format!(
                "requirement mapping missing or ambiguous at {}",
                old.statement.evidence.range.start
            );
            if found.is_empty() && !coverage_incomplete {
                rejected.push(message);
            } else {
                incomplete.push(message);
            }
            continue;
        }
        let new = found[0];
        if !same_context(old, new) {
            rejected.push(format!(
                "requirement context changed at {}",
                old.statement.evidence.range.start
            ));
        }
        let expected = applied
            .patches
            .iter()
            .find(|p| p.change == old.statement.evidence)
            .and_then(|p| p.source_constraint.as_ref())
            .unwrap_or(&old.constraint);
        if !same_constraint(expected, &new.constraint) {
            rejected.push(format!(
                "requirement changed at {}",
                old.statement.evidence.range.start
            ));
        }
        pairs.push((old, new));
    }
    for patch in &applied.patches {
        if patch.official_basis.is_some() {
            continue;
        }
        let old_target: Vec<_> = before
            .investigations
            .iter()
            .filter(|i| {
                confirmed(i)
                    && i.candidate.evidence.len() == patch.target.sides.len()
                    && patch
                        .target
                        .sides
                        .iter()
                        .all(|e| i.candidate.evidence.contains(e))
            })
            .collect();
        if old_target.len() != 1 {
            incomplete.push("target could not be matched".into());
            continue;
        }
        let sides: Vec<_> = pairs
            .iter()
            .filter(|(old, _)| involved(old_target[0], old))
            .map(|(_, new)| *new)
            .collect();
        if sides.len() < 2 {
            incomplete.push(format!(
                "target sides could not be mapped: {}",
                old_target[0].candidate.id
            ));
            continue;
        }
        for inv in after.investigations.iter().filter(|i| confirmed(i)) {
            if sides.iter().all(|c| involved(inv, c)) {
                rejected.push(format!("target conflict remains: {}", inv.candidate.id));
            }
        }
    }
    for inv in after.investigations.iter().filter(|i| confirmed(i)) {
        let new_sides: Vec<_> = pairs
            .iter()
            .filter(|(_, new)| involved(inv, new))
            .map(|(old, _)| *old)
            .collect();
        let existed = new_sides.len() >= 2
            && before
                .investigations
                .iter()
                .filter(|i| confirmed(i))
                .any(|old| {
                    new_sides.iter().all(|c| involved(old, c))
                        && pairs.iter().filter(|(c, _)| involved(old, c)).count() == new_sides.len()
                });
        if !existed {
            rejected.push(format!("new conflict: {}", inv.candidate.id));
        }
    }
    // Every standard occurrence must map exactly once, including unchanged occurrences.
    let mut matched = Vec::new();
    for (i, old) in before.standard_references.iter().enumerate() {
        let Some(range) = mapped(&old.mention.evidence.range, &applied.patches) else {
            rejected.push("overlapping standard address".into());
            continue;
        };
        let found: Vec<_> = after
            .standard_references
            .iter()
            .enumerate()
            .filter(|(_, r)| r.mention.evidence.range == range)
            .collect();
        if found.len() != 1 {
            incomplete.push(format!(
                "standard mapping missing or ambiguous at {}",
                old.mention.evidence.range.start
            ));
            continue;
        }
        let (j, new) = found[0];
        matched.push(j);
        let (Some(old_usage), Some(new_usage)) =
            (before.standard_usage.get(i), after.standard_usage.get(j))
        else {
            incomplete.push("standard usage missing".into());
            continue;
        };
        if old_usage.mention != old.mention.evidence || new_usage.mention != new.mention.evidence {
            incomplete.push("standard usage address mismatch".into());
            continue;
        }
        if let Some(patch) = applied
            .patches
            .iter()
            .find(|p| p.official_basis.is_some() && p.target.sides.contains(&old.mention.evidence))
        {
            let basis = patch.official_basis.as_ref().unwrap();
            if matches!(
                new_usage.outcome,
                standard_usage::Outcome::TechnicalFailure
                    | standard_usage::Outcome::InsufficientEvidence
            ) || matches!(new.existence, Existence::PartialSearch)
            {
                incomplete.push("corrected standard could not be rechecked".into());
            } else if !matches!(new.existence, Existence::Exact)
                || new.cards.len() != 1
                || new.cards[0].designation != basis.card.designation
                || new.cards[0].url != basis.card.url
                || !matches!(new.date_status.as_slice(), [standards::DateStatus::SnapshotStatus(status)] if status == "Действует")
                || !matches!(new_usage.role, standard_usage::Role::Direct)
                || !matches!(new_usage.outcome, standard_usage::Outcome::Supported)
                || if let Some(clause) = &basis.clause {
                    let title = mapped(&basis.title.range, &applied.patches);
                    let clause_range = mapped(&clause.range, &applied.patches);
                    title.and_then(|r| applied.source.get(r)) != Some(basis.card.title.as_str())
                        || !clause_range.is_some_and(|r| {
                            basis.card.clauses.iter().any(|c| {
                                applied.source.get(r.clone()) == Some(c.number.as_str())
                                    && new_usage.official_excerpt.as_deref()
                                        == Some(c.text.as_str())
                            })
                        })
                        || !new_usage.evidence.iter().any(|e| {
                            Some(e.range.clone()) == mapped(&basis.title.range, &applied.patches)
                        })
                } else {
                    new_usage.official_excerpt.as_deref() != Some(basis.card.scope.as_str())
                }
            {
                rejected.push("corrected standard status or applicability changed".into());
            }
        } else if serde_json::to_value((
            &old.existence,
            &old.date_status,
            &old_usage.role,
            &old_usage.outcome,
        ))
        .ok()
            != serde_json::to_value((
                &new.existence,
                &new.date_status,
                &new_usage.role,
                &new_usage.outcome,
            ))
            .ok()
        {
            if matches!(
                new_usage.outcome,
                standard_usage::Outcome::TechnicalFailure
                    | standard_usage::Outcome::InsufficientEvidence
            ) {
                incomplete.push("unchanged standard could not be rechecked".into());
            } else {
                rejected.push("unchanged standard status or applicability changed".into());
            }
        }
    }
    if matched.len() != after.standard_references.len() {
        rejected.push("new or unmapped standard occurrence".into());
    }
    if coverage_incomplete {
        incomplete.push("copy review has incomplete coverage".into());
    }
    if !rejected.is_empty() {
        Verification::Rejected { reasons: rejected }
    } else if !incomplete.is_empty() {
        Verification::Incomplete {
            reasons: incomplete,
        }
    } else {
        Verification::Verified
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consistency::{
        claims::{Object, Operator},
        priority::OptionValue,
    };

    fn fixture() -> (MarkdownIndex, Vec<Claim>, Selection) {
        let index = MarkdownIndex::new("lift mass exactly 300 kg must\n\nlift mass exactly 500 kg must\n\nlift mass exactly 300 kg must").unwrap();
        let g = |id, s: &str| Grounded {
            text: s.into(),
            evidence: index.resolve(id, s).unwrap(),
        };
        let claim = |id, number| Claim {
            complete: true,
            statement: g(id, &format!("lift mass exactly {number} kg must")),
            object: Object {
                key: Some("lift".into()),
                evidence: vec![g(id, "lift")],
                alternatives: vec![],
            },
            property: g(id, "mass"),
            constraint: Constraint::Numeric {
                operator: Operator::Exact,
                operator_evidence: g(id, "exactly"),
                value: g(id, number),
                unit: Some(g(id, "kg")),
            },
            stage: None,
            conditions: vec![],
            mandatory: Some(g(id, "must")),
        };
        let claims = vec![claim(0, "300"), claim(1, "500"), claim(2, "300")];
        let selection = Selection::Selected {
            target: Target {
                version: index.version.clone(),
                sides: claims[..2]
                    .iter()
                    .map(|c| c.statement.evidence.clone())
                    .collect(),
            },
            change: claims[0].statement.evidence.clone(),
            source: Box::new(OptionValue {
                statement: claims[1].statement.clone(),
                constraint: claims[1].constraint.clone(),
                conditions: vec![],
            }),
            basis: vec![],
            user_provenance: Some("customer".into()),
        };
        (index, claims, selection)
    }

    #[test]
    fn addressed_copy_and_atomic_preconditions() {
        let (index, claims, selection) = fixture();
        let patch = prepare(&index, &selection, &claims).unwrap().unwrap();
        assert_eq!((&*patch.expected, &*patch.replacement), ("300", "500"));
        let copy = apply(&index, std::slice::from_ref(&patch)).unwrap();
        assert_eq!(
            copy.source,
            "lift mass exactly 500 kg must\n\nlift mass exactly 500 kg must\n\nlift mass exactly 300 kg must"
        );
        assert!(index.source.starts_with("lift mass exactly 300"));
        let mut stale = patch.clone();
        stale.expected = "wrong".into();
        assert!(apply(&index, &[patch.clone(), stale]).is_err());
        assert!(apply(&index, &[patch.clone(), patch.clone()]).is_err());
        let mut stale = patch;
        stale.target.version = "old".into();
        assert!(apply(&index, &[stale]).is_err());
    }

    #[test]
    fn maps_changed_and_following_offsets() {
        let (index, claims, selection) = fixture();
        let mut patch = prepare(&index, &selection, &claims).unwrap().unwrap();
        patch.replacement = "5000".into();
        let applied = apply(&index, &[patch.clone()]).unwrap();
        let mapped_first = mapped(&claims[0].statement.evidence.range, &applied.patches).unwrap();
        let mapped_last = mapped(&claims[2].statement.evidence.range, &applied.patches).unwrap();
        assert_eq!(
            &applied.source[mapped_first],
            "lift mass exactly 5000 kg must"
        );
        assert_eq!(
            &applied.source[mapped_last],
            "lift mass exactly 300 kg must"
        );
        assert_eq!(
            mapped(&patch.range, &applied.patches).unwrap().end,
            patch.range.end + 1
        );
    }
    fn report(
        index: &MarkdownIndex,
        claims: Vec<Claim>,
        conflicts: Vec<(usize, usize)>,
    ) -> ConsistencyReport {
        use crate::consistency::investigation::Candidate;
        let investigations = conflicts
            .into_iter()
            .enumerate()
            .map(|(id, (a, b))| Investigation {
                candidate: Candidate {
                    id: id.to_string(),
                    category: "numeric".into(),
                    object: "lift".into(),
                    property: "mass".into(),
                    evidence: vec![
                        claims[a].statement.evidence.clone(),
                        claims[b].statement.evidence.clone(),
                    ],
                    context: vec![],
                    known_gaps: vec![],
                },
                critic_calls: 1,
                read_decisions: vec![],
                additional_evidence: vec![],
                cited_evidence: vec![],
                outcome: Ok(Verdict::Confirmed {
                    explanation: "conflict".into(),
                }),
            })
            .collect();
        ConsistencyReport {
            run_id: "test".into(),
            version: index.version.clone(),
            assessment_date: "2026-01-01".into(),
            assumed_assessment_date: false,
            parsing_gaps: vec![],
            oversized_blocks: vec![],
            packets: vec![],
            claims,
            priority_mentions: vec![],
            initial_candidates: vec![],
            investigations,
            selections: vec![],
            response_target_matches: None,
            standard_usage: vec![],
            standard_references: vec![],
            missing_standard_materials: vec![],
            recovered_standard_mentions: vec![],
            standard_coverage: super::super::StandardCoverage::default(),
            standard_material_questions: vec![],
            object_questions: vec![],
            semantic_gaps: vec![],
            model_attempts: 0,
            patches: vec![],
            patch_verification: None,
            copy_version: None,
            catalog_trace: vec![],
            catalog_requests: 0,
        }
    }
    fn rebound(index: &MarkdownIndex, claims: &[Claim], first_number: &str) -> Vec<Claim> {
        let mut result = claims.to_vec();
        for (id, claim) in result.iter_mut().enumerate() {
            let number = if id == 0 {
                first_number
            } else if id == 1 {
                "500"
            } else {
                "300"
            };
            let g = |s: &str| Grounded {
                text: s.into(),
                evidence: index.resolve(id, s).unwrap(),
            };
            claim.statement = g(&format!("lift mass exactly {number} kg must"));
            claim.object.evidence = vec![g("lift")];
            claim.property = g("mass");
            claim.constraint = Constraint::Numeric {
                operator: Operator::Exact,
                operator_evidence: g("exactly"),
                value: g(number),
                unit: Some(g("kg")),
            };
            claim.mandatory = Some(g("must"));
        }
        result
    }
    #[test]
    fn verification_distinguishes_harm_and_incomplete_coverage() {
        let (index, claims, selection) = fixture();
        let patch = prepare(&index, &selection, &claims).unwrap().unwrap();
        let before = report(&index, claims.clone(), vec![(0, 1)]);
        let applied = apply(&index, std::slice::from_ref(&patch)).unwrap();
        let copy_index = MarkdownIndex::new(&applied.source).unwrap();
        let new_claims = rebound(&copy_index, &claims, "500");
        let mut after = report(&copy_index, new_claims, vec![]);
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Verified
        ));
        after.claims[2].mandatory = None;
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Rejected { .. }
        ));
        after.claims[2].mandatory = Some(
            rebound(&copy_index, &claims, "500")[2]
                .mandatory
                .clone()
                .unwrap(),
        );
        after
            .semantic_gaps
            .push(("lift".into(), "budget exhausted".into()));
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Incomplete { .. }
        ));
        after.semantic_gaps.clear();
        after.investigations =
            report(&copy_index, after.claims.clone(), vec![(1, 2)]).investigations;
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Rejected { .. }
        ));

        let mut decimal = patch;
        decimal.replacement = "300,0".into();
        let applied = apply(&index, &[decimal]).unwrap();
        let copy_index = MarkdownIndex::new(&applied.source).unwrap();
        let after = report(
            &copy_index,
            rebound(&copy_index, &claims, "300,0"),
            vec![(0, 1)],
        );
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Rejected { .. }
        ));
    }
    #[tokio::test]
    async fn standard_patch_rechecks_address_status_and_coverage() {
        use crate::{
            core::{ErrorCategory, Visibility},
            runtime::{ReviewRun, Runtime, grok::Model},
        };
        use std::time::Duration;
        struct Mock(String);
        impl Model for Mock {
            async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
                Ok(self.0.clone())
            }
        }
        let catalog: standards::Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let title = &catalog.cards[0].title;
        let source = format!("{title}: метод Роквелла при 20 °C по КОСТ 9013-59");
        let index = MarkdownIndex::new(&source).unwrap();
        let mut before = report(&index, vec![], vec![]);
        before.standard_references = catalog.review(&standards::scan(&index), "2026-11-13");
        let answer = |statement: &str| {
            let q = |s: &str| serde_json::json!({"block_id": 0, "quote":s});
            serde_json::json!({"role":"direct", "title":q(title), "statement":q(statement), "method":q("Роквелла"),
                "temperature":q("20 °C"), "caveats":[], "attributed_threshold":null,
                "official_excerpt":catalog.cards[0].scope, "outcome":"supported"}).to_string()
        };
        let rt = Runtime::new(Mock(answer(&source)), Duration::from_secs(1));
        let usage = standard_usage::evaluate(
            &index,
            &before.standard_references[0],
            &catalog,
            &rt,
            &ReviewRun::new(1),
            Visibility::Public,
        )
        .await;
        let patch = prepare_standard(&index, &before.standard_references[0], &usage)
            .unwrap()
            .unwrap();
        before.standard_usage.push(usage);
        let applied = apply(&index, &[patch]).unwrap();
        let copy = MarkdownIndex::new(&applied.source).unwrap();
        let mut after = report(&copy, vec![], vec![]);
        after.standard_references = catalog.review(&standards::scan(&copy), "2026-11-13");
        let rt = Runtime::new(Mock(answer(&applied.source)), Duration::from_secs(1));
        after.standard_usage.push(
            standard_usage::evaluate(
                &copy,
                &after.standard_references[0],
                &catalog,
                &rt,
                &ReviewRun::new(1),
                Visibility::Public,
            )
            .await,
        );
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Verified
        ));
        after.standard_usage[0].outcome = standard_usage::Outcome::OutsideScope;
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Rejected { .. }
        ));
        after.standard_usage[0].outcome = standard_usage::Outcome::TechnicalFailure;
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Incomplete { .. }
        ));
        after.standard_usage[0].outcome = standard_usage::Outcome::Supported;
        after
            .catalog_trace
            .push(super::super::catalog_http::Lookup {
                key: after.standard_references[0].mention.key.clone(),
                catalog: catalog.clone(),
                outcome: super::super::catalog_http::Outcome::NetworkError,
                requests: vec![],
            });
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Incomplete { .. }
        ));
        after.catalog_trace.clear();
        after
            .standard_references
            .push(after.standard_references[0].clone());
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Incomplete { .. } | Verification::Rejected { .. }
        ));
    }
    #[tokio::test]
    async fn title_typo_requires_independent_clause_and_is_rechecked_on_copy() {
        use crate::{
            core::{ErrorCategory, Visibility},
            runtime::{ReviewRun, Runtime, grok::Model},
        };
        use std::time::Duration;
        struct Mock(String);
        impl Model for Mock {
            async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
                Ok(self.0.clone())
            }
        }
        let mut catalog: standards::Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        catalog.cards[0].clauses.push(standards::OfficialClause {
            number: "3.1".into(),
            text: "измерение твердости проводят на образце.".into(),
        });
        let title = catalog.cards[0].title.clone();
        let typo = format!("{}я", &title[..title.char_indices().next_back().unwrap().0]);
        let line =
            format!("ГОСТ 9013-59 «{typo}»: пункт 3.1, измерение твердости проводят на образце.");
        // Same title in another block must not change.
        let source = format!("{line}\n\n{typo}");
        let index = MarkdownIndex::new(&source).unwrap();
        let mut before = report(&index, vec![], vec![]);
        before.standard_references = catalog.review(&standards::scan(&index), "2026-11-13");
        let answer = |statement: &str, name: &str, clause: &str| {
            let q = |s: &str| serde_json::json!({"block_id":0,"quote":s});
            serde_json::json!({"role":"direct", "title":q(name), "statement":q(statement), "method":null, "temperature":null,
                "caveats":[], "attributed_threshold":null, "official_excerpt":clause,
                "clause_number":"3.1", "clause_reference":q("3.1"), "action":q("измерение твердости"), "outcome":"supported"}).to_string()
        };
        let clause = catalog.cards[0].clauses[0].text.clone();
        let rt = Runtime::new(Mock(answer(&line, &typo, &clause)), Duration::from_secs(1));
        let usage = standard_usage::evaluate(
            &index,
            &before.standard_references[0],
            &catalog,
            &rt,
            &ReviewRun::new(1),
            Visibility::Public,
        )
        .await;
        let patch = prepare_standard(&index, &before.standard_references[0], &usage)
            .unwrap()
            .unwrap();
        before.standard_usage.push(usage);
        let applied = apply(&index, &[patch]).unwrap();
        assert_eq!(
            applied.source,
            format!(
                "ГОСТ 9013-59 «{title}»: пункт 3.1, измерение твердости проводят на образце.\n\n{typo}"
            )
        );
        let copy = MarkdownIndex::new(&applied.source).unwrap();
        let mut after = report(&copy, vec![], vec![]);
        after.standard_references = catalog.review(&standards::scan(&copy), "2026-11-13");
        let corrected_line = applied.source.lines().next().unwrap();
        let rt = Runtime::new(
            Mock(answer(corrected_line, &title, &clause)),
            Duration::from_secs(1),
        );
        after.standard_usage.push(
            standard_usage::evaluate(
                &copy,
                &after.standard_references[0],
                &catalog,
                &rt,
                &ReviewRun::new(1),
                Visibility::Public,
            )
            .await,
        );
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Verified
        ));
        after.standard_usage[0].outcome = standard_usage::Outcome::TechnicalFailure;
        assert!(matches!(
            verify(&before, &after, &applied),
            Verification::Incomplete { .. }
        ));
        let mut no_clause = catalog.clone();
        no_clause.cards[0].clauses.clear();
        let rt = Runtime::new(Mock(answer(&line, &typo, &clause)), Duration::from_secs(1));
        let no_clause_refs = no_clause.review(&standards::scan(&index), "2026-11-13");
        let usage = standard_usage::evaluate(
            &index,
            &no_clause_refs[0],
            &no_clause,
            &rt,
            &ReviewRun::new(1),
            Visibility::Public,
        )
        .await;
        assert!(usage.editorial_correction.is_none());
        let mut other = catalog.cards[0].clone();
        other.designation = "ГОСТ 1234-59".into();
        other.full_designation = other.designation.clone();
        other.title = typo.clone();
        catalog.cards.push(other);
        let rt = Runtime::new(Mock(answer(&line, &typo, &clause)), Duration::from_secs(1));
        let usage = standard_usage::evaluate(
            &index,
            &before.standard_references[0],
            &catalog,
            &rt,
            &ReviewRun::new(1),
            Visibility::Public,
        )
        .await;
        assert!(usage.editorial_correction.is_none());
        assert!(usage.action.unwrap().contains("Уточнить"));
    }
}
