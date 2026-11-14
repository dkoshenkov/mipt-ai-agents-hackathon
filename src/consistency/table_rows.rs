use super::{
    claims::{Claim, Constraint, Grounded, Object, Operator},
    markdown::MarkdownIndex,
    numeric::{parse_decimal, parse_unit},
    table_measure::work_measure,
    table_scope::{characteristic_owner, text, work_owner},
};

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

// Only an explicit measurement can establish a nominal. The row number and the work
// quantity have their own columns and never participate in this lookup.
fn characteristic(index: &MarkdownIndex, value_id: usize) -> Option<Claim> {
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
    if index.blocks[value_id]
        .table_context
        .iter()
        .filter_map(|id| text(index, *id))
        .any(|cell| cell.text.to_lowercase().starts_with("примечан"))
    {
        return None;
    }
    let owner = owner.or_else(|| characteristic_owner(index, value_id))?;
    Some(nominal(statement, owner, property?, value, unit, None))
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
