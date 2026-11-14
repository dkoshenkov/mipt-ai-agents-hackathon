use super::{
    claims::{Claim, Constraint, Grounded, Object, Operator},
    investigation::{Candidate, SourceComparison},
    markdown::{BlockKind, MarkdownIndex},
    numeric::{parse_decimal, parse_unit},
    table_measure::work_measure,
    table_scope::{characteristic_owner, text, work_owner},
};
use sha2::{Digest, Sha256};

fn subquote(index: &MarkdownIndex, cell: &Grounded, literal: &str) -> Option<Grounded> {
    Some(Grounded {
        text: literal.into(),
        evidence: index.resolve_scoped(
            cell.evidence.block_id,
            literal,
            &cell.text,
            &cell.evidence,
        )?,
    })
}

fn column(index: &MarkdownIndex, id: usize) -> Option<String> {
    let headers = index.table_column_ids(id);
    let [header] = headers.as_slice() else {
        return None;
    };
    Some(text(index, *header)?.text.to_lowercase())
}

fn qualified(label: &str) -> bool {
    let label = label.to_lowercase();
    label.contains(['?', ';', '±', '<', '>', '~'])
        || label.split(|c: char| !c.is_alphabetic()).any(|w| {
            matches!(
                w,
                "при" | "если" | "или" | "либо" | "до" | "от" | "не" | "диапазон"
            ) || [
                "ориентиров",
                "приблизитель",
                "рекоменду",
                "опциональ",
                "необязат",
                "минималь",
                "максималь",
                "примерн",
            ]
            .iter()
            .any(|prefix| w.starts_with(prefix))
        })
}

fn plain_requirement_row(index: &MarkdownIndex, value_id: usize) -> Option<()> {
    let position = index.blocks[value_id].table.as_ref()?;
    if position.rows.len() != 1
        || position.columns.len() != 1
        || position.section.is_some()
        || position
            .title
            .and_then(|id| text(index, id))
            .is_some_and(|title| qualified(&title.text))
        || index.blocks[value_id]
            .headings
            .iter()
            .filter_map(|id| text(index, *id))
            .any(|heading| qualified(&heading.text))
    {
        return None;
    }
    if index.blocks[value_id]
        .table_context
        .iter()
        .filter_map(|id| text(index, *id))
        .any(|cell| cell.text.to_lowercase().starts_with("примечан"))
    {
        return None;
    }
    Some(())
}

// Only an explicit measurement can establish a nominal. The row number and the work
// quantity have their own columns and never participate in this lookup.
fn characteristic(index: &MarkdownIndex, value_id: usize) -> Option<Claim> {
    plain_requirement_row(index, value_id)?;
    let statement = text(index, value_id)?;
    let raw = statement.text.as_str();
    let split = raw.find(char::is_alphabetic).unwrap_or(raw.len());
    let number = raw[..split].trim();
    parse_decimal(number)?;
    let value = subquote(index, &statement, number)?;
    let mut units = Vec::new();
    let suffix = raw[split..].trim();
    if !suffix.is_empty() {
        units.push(subquote(index, &statement, suffix)?);
    }
    let mut owner = None;
    let mut property = None;
    for id in index.same_row_ids(value_id) {
        if id == value_id {
            continue;
        }
        let header = column(index, id)?;
        match header.as_str() {
            "№ п/п" | "№" | "номер" => (),
            "объект" | "оборудование" => {
                let cell = text(index, id)?;
                if owner.is_some()
                    || qualified(&cell.text)
                    || !cell.text.chars().any(char::is_alphabetic)
                {
                    return None;
                }
                owner = Some(Object {
                    key: Some(cell.text.to_lowercase()),
                    evidence: vec![cell],
                    alternatives: vec![],
                });
            }
            "единица измерения" | "ед. изм." | "ед. изм" => {
                units.push(text(index, id)?)
            }
            "параметр"
            | "свойство"
            | "характеристика"
            | "наименование параметра"
            | "наименование характеристики"
            | "наименование оборудования" => {
                let cell = text(index, id)?;
                if property.is_some() || qualified(&cell.text) {
                    return None;
                }
                let label = cell.text.as_str();
                let parts = label
                    .rsplit_once(',')
                    .or_else(|| label.strip_suffix(')')?.rsplit_once('('));
                let name = if let Some((name, unit)) = parts {
                    units.push(subquote(index, &cell, unit.trim())?);
                    name.trim()
                } else {
                    label
                };
                if name.is_empty()
                    || !name
                        .chars()
                        .all(|c| c.is_alphabetic() || c.is_whitespace() || c == '-')
                {
                    return None;
                }
                property = Some(subquote(index, &cell, name)?);
            }
            // An unmodelled field can qualify the value (for example, a condition or a tolerance).
            _ => return None,
        }
    }
    let unit = units.pop()?;
    let parsed = parse_unit(&unit.text)?;
    if units
        .iter()
        .any(|u| parse_unit(&u.text).as_ref() != Some(&parsed))
    {
        return None;
    }
    let owner = owner.or_else(|| characteristic_owner(index, value_id))?;
    Some(nominal(statement, owner, property?, value, unit, None))
}

// Match a single explicit type in a supply table with an explicit description of the
// new object. A reference to the equipment being removed cannot establish its successor's type.
pub(super) fn equipment_type(index: &MarkdownIndex) -> Vec<(Candidate, SourceComparison)> {
    let mut comparisons = Vec::new();
    for block in &index.blocks {
        let Some(property) = block.table.as_ref().and_then(|_| text(index, block.id)) else {
            continue;
        };
        let label = property.text.to_lowercase();
        let Some(owner_name) = label
            .strip_prefix("тип ")
            .or_else(|| label.strip_prefix("вид "))
        else {
            continue;
        };
        let Some(object) = characteristic_owner(index, block.id) else {
            continue;
        };
        if object.key.as_deref() != Some(owner_name)
            || !matches!(
                column(index, block.id).as_deref(),
                Some(
                    "параметр"
                        | "наименование оборудования"
                        | "характеристика"
                        | "наименование характеристики"
                )
            )
        {
            continue;
        }
        let mut values = index.same_row_ids(block.id).into_iter().filter(|&id| {
            matches!(
                column(index, id).as_deref(),
                Some("значение" | "технические характеристики")
            )
        });
        let Some(value_id) = values.next() else {
            continue;
        };
        if values.next().is_some() || plain_requirement_row(index, value_id).is_none() {
            continue;
        }
        let Some(value) = text(index, value_id) else {
            continue;
        };
        if !value.text.chars().all(char::is_alphabetic) {
            continue;
        }
        let descriptions: Vec<_> = index
            .blocks
            .iter()
            .filter(|b| b.kind == BlockKind::Paragraph && b.id < block.id)
            .filter_map(|b| {
                let source = text(index, b.id)?;
                let words: Vec<_> = source.text.split_whitespace().collect();
                let [state, kind, noun, verb, ..] = words.as_slice() else {
                    return None;
                };
                let subject = noun.to_lowercase();
                let same_noun = subject == owner_name
                    || ["а", "я"]
                        .iter()
                        .any(|ending| owner_name.strip_suffix(ending) == Some(&subject));
                if !matches!(state.to_lowercase().as_str(), "новый" | "новая" | "новое")
                    || !matches!(
                        verb.to_lowercase().as_str(),
                        "устанавливается" | "поставляется"
                    )
                    || !kind.chars().all(char::is_alphabetic)
                    || !same_noun
                    || qualified(&source.text)
                {
                    return None;
                }
                let kind = kind.to_string();
                Some((source, kind, subject))
            })
            .collect();
        let [(description, kind, subject)] = descriptions.as_slice() else {
            continue;
        };
        if kind.to_lowercase() == value.text.to_lowercase() {
            continue;
        }
        let sides = [description.evidence.clone(), value.evidence];
        let id = serde_json::to_vec(&("equipment_type", &sides)).expect("serializable evidence");
        let candidate = Candidate {
            id: format!("{:x}", Sha256::digest(id)),
            category: "naming".into(),
            object: subject.clone(),
            property: property.text.clone(),
            evidence: sides.to_vec(),
            context: object
                .evidence
                .into_iter()
                .map(|g| g.evidence)
                .chain([property.evidence])
                .collect(),
            known_gaps: vec![],
        };
        let question = format!(
            "{} — «{}» или «{}»? Уточните, какой тип требуется к поставке.",
            property.text, kind, value.text
        );
        comparisons.push((candidate, SourceComparison { sides, question }));
    }
    comparisons
}

pub(super) fn nominal_comparison(
    candidate: &Candidate,
    claims: &[Claim],
) -> Option<SourceComparison> {
    let mut seen = Vec::<(&Claim, &Grounded, &Grounded)>::new();
    for claim in claims {
        if !claim.complete
            || claim.object.key.as_deref() != Some(&candidate.object)
            || claim.property.text.to_lowercase() != candidate.property
            || !candidate.evidence.contains(&claim.statement.evidence)
        {
            continue;
        }
        let Constraint::Numeric {
            operator: Operator::Exact | Operator::TableNominal,
            value,
            unit: Some(unit),
            ..
        } = &claim.constraint
        else {
            continue;
        };
        for &(other, prior, prior_unit) in &seen {
            if claim.statement.evidence == other.statement.evidence
                || matches!((&other.stage, &claim.stage), (Some(a), Some(b)) if a.text.to_lowercase() != b.text.to_lowercase())
                || super::numeric::hypotheses(&[other, claim]).is_empty()
            {
                continue;
            }
            return Some(SourceComparison {
                sides: [
                    other.statement.evidence.clone(),
                    claim.statement.evidence.clone(),
                ],
                question: format!(
                    "Какое значение требуется для «{}» — {} {} или {} {}?",
                    claim.property.text, prior.text, prior_unit.text, value.text, unit.text
                ),
            });
        }
        seen.push((claim, value, unit));
    }
    None
}

fn nominal(
    statement: Grounded,
    object: Object,
    property: Grounded,
    value: Grounded,
    unit: Grounded,
    stage: Option<Grounded>,
) -> Claim {
    Claim {
        complete: true,
        statement,
        object,
        property,
        stage,
        constraint: Constraint::Numeric {
            operator: Operator::TableNominal,
            operator_evidence: value.clone(),
            value,
            unit: Some(unit),
        },
        conditions: Vec::new(),
        mandatory: None,
    }
}

pub(super) fn claims(index: &MarkdownIndex) -> Vec<Claim> {
    let mut characteristics = Vec::new();
    let mut descriptions = Vec::new();
    for block in &index.blocks {
        let Some(position) = &block.table else {
            continue;
        };
        if position.rows.start == 0 {
            continue;
        }
        match column(index, block.id).as_deref() {
            Some("значение" | "технические характеристики") => {
                if let Some(claim) = characteristic(index, block.id) {
                    characteristics.push(claim);
                }
            }
            Some("наименование работ" | "работы") => {
                descriptions.push(block.id)
            }
            _ => (),
        }
    }
    let mut found = Vec::new();
    for id in descriptions {
        for characteristic in &characteristics {
            let Constraint::Numeric {
                unit: Some(unit), ..
            } = &characteristic.constraint
            else {
                continue;
            };
            let Some((statement, value, unit)) =
                work_measure(index, id, &characteristic.property, unit)
            else {
                continue;
            };
            let Some((object, stage)) = work_owner(index, id, &characteristic.object) else {
                continue;
            };
            found.push(nominal(
                statement,
                object,
                characteristic.property.clone(),
                value,
                unit,
                Some(stage),
            ));
        }
    }
    found.extend(characteristics);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separate_unit_is_usable_only_when_the_row_is_unqualified_and_consistent() {
        let base = "# Комплектация\n\n| Объект | Параметр | Значение | Ед. изм. |\n|---|---|---|---|\n| Насос А | Масса | 0,5 | т |\n";
        let index = MarkdownIndex::new(base).unwrap();
        let facts = claims(&index);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].object.key.as_deref(), Some("насос а"));
        for source in [
            base.replace("0,5 | т", "0,5 кг | т"),
            base.replace("0,5 | т", "до 0,5 | т"),
            base.replace("Комплектация", "Комплектация при аварии"),
            format!("{base}| Примечание: допускается замена | | | |\n"),
        ] {
            assert!(
                claims(&MarkdownIndex::new(&source).unwrap()).is_empty(),
                "{source}"
            );
        }
    }
}
