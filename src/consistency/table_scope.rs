use super::claims::{Grounded, Object};
use super::markdown::MarkdownIndex;

pub(super) fn text(index: &MarkdownIndex, id: usize) -> Option<Grounded> {
    let block = index.blocks.get(id)?;
    let mut parts = block
        .text_ranges
        .iter()
        .filter_map(|r| index.source.get(r.clone()))
        .map(|s| s.trim().trim_start_matches("&#9;").trim())
        .filter(|s| !s.is_empty());
    let value = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Grounded {
        text: value.into(),
        evidence: index.resolve(id, value)?,
    })
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphabetic())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

// A single inflected noun only; this deliberately does not resolve compound names.
fn stem(s: &str) -> &str {
    s.strip_suffix(['а', 'я', 'ы', 'и']).unwrap_or(s)
}

fn has_word(s: &str, name: &str) -> bool {
    words(s).iter().any(|w| stem(w) == name)
}

fn unique(index: &MarkdownIndex, pred: impl Fn(&Grounded) -> bool) -> Option<Grounded> {
    let mut matches = index
        .blocks
        .iter()
        .filter_map(|b| text(index, b.id))
        .filter(pred);
    let value = matches.next()?;
    matches.next().is_none().then_some(value)
}

pub(super) fn characteristic_owner(index: &MarkdownIndex, cell_id: usize) -> Option<Object> {
    let table = index.blocks.get(cell_id)?.table.as_ref()?;
    let caption = text(index, table.title?)?;
    if caption.text.contains('№') || caption.text.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let parts = words(&caption.text);
    let mut parts = parts.iter().map(String::as_str);
    match parts.next()? {
        "технические" if parts.next() == Some("характеристики") => (),
        "характеристики" => (),
        _ => return None,
    }
    let tail: Vec<_> = parts.collect();
    let name = match tail.as_slice() {
        ["поставляемого", "для", "монтажа", name] | [name] => *name,
        _ => return None,
    };
    let key = name.to_owned();
    let name_stem = stem(&key);
    if name_stem.chars().count() < 3 {
        return None;
    }
    // Multiple characteristic tables for the same object do not identify a unique position.
    let captions = index
        .blocks
        .iter()
        .filter_map(|b| b.table.as_ref()?.title)
        .collect::<std::collections::BTreeSet<_>>();
    if captions
        .iter()
        .filter_map(|id| text(index, *id))
        .filter(|c| {
            words(&c.text).iter().any(|w| w == "характеристики") && has_word(&c.text, name_stem)
        })
        .count()
        != 1
    {
        return None;
    }
    Some(Object {
        key: Some(key),
        evidence: vec![caption],
        alternatives: vec![],
    })
}

pub(super) fn work_owner(
    index: &MarkdownIndex,
    cell_id: usize,
    supplied: &Object,
) -> Option<(Object, Grounded)> {
    let table = index.blocks.get(cell_id)?.table.as_ref()?;
    let caption = text(index, table.title?)?;
    let key = supplied.key.as_deref()?;
    let name = stem(key);
    let characteristic_cell = index.blocks.iter().find(|b| {
        b.table
            .as_ref()
            .is_some_and(|t| t.title == supplied.evidence.first().map(|e| e.evidence.block_id))
    })?;
    if characteristic_owner(index, characteristic_cell.id).as_ref() != Some(supplied) {
        return None;
    }
    if !has_word(&caption.text, name)
        || !words(&caption.text).iter().any(|w| w.starts_with("замен"))
    {
        return None;
    }
    let section_id = table.section?;
    let section = text(index, section_id)?;
    let stage = words(&section.text);
    let stage = stage.strip_prefix(&["раздел".to_owned()]).unwrap_or(&stage);
    let (old, state) = match stage.first().map(String::as_str) {
        Some("демонтажные") => (true, "старого"),
        Some("монтажные" | "пусконаладочные") => (false, "нового"),
        _ => return None,
    };
    if !matches!(stage, [_, works] if works == "работы")
        && !matches!(stage, [_, works, qualifier, subject] if works == "работы" && qualifier == state && stem(subject) == name)
    {
        return None;
    }
    let action_name = if old {
        "демонтаж"
    } else {
        "монтаж"
    };
    let action = unique(index, |g| {
        let w = words(&g.text);
        w.iter().any(|v| v == action_name)
            && w.windows(2)
                .any(|pair| pair[0] == state && stem(&pair[1]) == name)
    })?;
    let supply = if old {
        None
    } else {
        Some(unique(index, |g| {
            let w = words(&g.text);
            w.iter().any(|v| v.starts_with("поставк"))
                && w.windows(2)
                    .any(|p| p[0] == "нового" && stem(&p[1]) == name)
        })?)
    };
    let mut positions = index
        .blocks
        .iter()
        .filter(|b| {
            b.table.as_ref().is_some_and(|t| {
                t.id == table.id && t.section == Some(section_id) && t.columns == table.columns
            })
        })
        .filter_map(|b| text(index, b.id))
        .filter(|g| words(&g.text).first().is_some_and(|w| stem(w) == name));
    let position = positions.next()?;
    if position.evidence.block_id != cell_id || positions.next().is_some() {
        return None;
    }
    let lower = position.text.to_lowercase();
    let position_words = words(&lower);
    if position_words.iter().skip(1).any(|w| {
        stem(w) == name
            || ["стар", "нов", "запасн", "друг"]
                .iter()
                .any(|prefix| w.starts_with(prefix))
    }) {
        return None;
    }
    if lower.contains('№')
        || lower.contains("модел")
        || lower.contains("марка")
        || lower.contains("тип ")
    {
        return None;
    }
    // A number immediately following the object or an adjective may be an equipment identifier.
    let ws: Vec<_> = lower.split_whitespace().collect();
    if ws
        .get(1)
        .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        || ws
            .get(2)
            .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_digit()))
    {
        return None;
    }
    let mut object = if old {
        Object {
            key: Some(format!("старого {key}")),
            evidence: vec![],
            alternatives: vec![],
        }
    } else {
        supplied.clone()
    };
    let end = position
        .text
        .find(|c: char| !c.is_alphabetic())
        .unwrap_or(position.text.len());
    let label = &position.text[..end];
    let position_name = Grounded {
        text: label.into(),
        evidence: index.resolve(cell_id, label)?,
    };
    object
        .evidence
        .extend([caption, action, position_name, section.clone()]);
    object.evidence.extend(supply);
    Some((object, section))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binds_named_equipment_and_rejects_ambiguity() {
        for (genitive, nominative) in [
            ("лифта", "Лифт"),
            ("насоса", "Насос"),
            ("вентилятора", "Вентилятор"),
        ] {
            let source = format!(
                "поставку нового {genitive}\n\nмонтаж нового {genitive}\n\nТехнические характеристики поставляемого для монтажа {genitive}:\n\n| Свойство | Значение |\n| --- | --- |\n| Масса | 300 |\n\nРаботы по замене {genitive}:\n\n| Работы | Количество |\n| --- | --- |\n| Монтажные работы | |\n| {nominative} грузовой, масса 500 кг | 1 |\n"
            );
            for (suffix, valid) in [
                ("", true),
                (" № 2", false),
                ("\n| {nominative} другой | 1 |", false),
            ] {
                let modified = if suffix.starts_with('\n') {
                    format!(
                        "{source}{}",
                        suffix
                            .trim_start_matches('\n')
                            .replace("{nominative}", nominative)
                    )
                } else {
                    source.replace("грузовой, масса", &format!("грузовой{suffix}, масса"))
                };
                let index = MarkdownIndex::new(&modified).unwrap();
                let find = |q: &str| {
                    index
                        .blocks
                        .iter()
                        .find(|b| index.resolve(b.id, q).is_some())
                        .unwrap()
                        .id
                };
                let object = characteristic_owner(&index, find("300")).unwrap();
                assert_eq!(work_owner(&index, find("500 кг"), &object).is_some(), valid);
            }
            for section in [
                format!("Монтажные работы старого {genitive}"),
                format!("Монтажные работы другого {genitive}"),
                "Монтажные и демонтажные работы".to_owned(),
            ] {
                let modified = source.replace("Монтажные работы", &section);
                let index = MarkdownIndex::new(&modified).unwrap();
                let find = |q: &str| {
                    index
                        .blocks
                        .iter()
                        .find(|b| index.resolve(b.id, q).is_some())
                        .unwrap()
                        .id
                };
                let object = characteristic_owner(&index, find("300")).unwrap();
                assert!(
                    work_owner(&index, find("500 кг"), &object).is_none(),
                    "{section}"
                );
            }
            for qualifier in ["старого", "запасного", "другого"] {
                let modified = source.replace(
                    "грузовой, масса",
                    &format!("грузовой, монтаж {qualifier} {genitive}, масса"),
                );
                let index = MarkdownIndex::new(&modified).unwrap();
                let find = |q: &str| {
                    index
                        .blocks
                        .iter()
                        .find(|b| index.resolve(b.id, q).is_some())
                        .unwrap()
                        .id
                };
                let object = characteristic_owner(&index, find("300")).unwrap();
                assert!(
                    work_owner(&index, find("500 кг"), &object).is_none(),
                    "{qualifier}"
                );
            }
        }
    }
}
