use super::claims::Grounded;
use super::markdown::{BlockKind, MarkdownIndex};
use super::numeric::{parse_decimal, parse_unit};

// Restrict inflection to common endings of a single Russian noun.
fn stem(word: &str) -> &str {
    for ending in [
        "ью", "ями", "ами", "ов", "ев", "ом", "ем", "ах", "ях", "а", "я", "у", "ю", "ы", "и", "е",
        "ь",
    ] {
        if let Some(root) = word.strip_suffix(ending) {
            return root;
        }
    }
    word
}

fn same_property(name: &str, word: &str) -> bool {
    let name = name.to_lowercase();
    let word = word.to_lowercase();
    let (a, b) = (stem(&name), stem(&word));
    if a == b {
        return true;
    }
    let (a, b): (Vec<_>, Vec<_>) = (a.chars().collect(), b.chars().collect());
    if a.len().min(b.len()) < 10
        || a[..3] != b[..3]
        || a[a.len() - 3..] != b[b.len() - 3..]
        || a.len().abs_diff(b.len()) > 1
    {
        return false;
    }
    // One substitution, insertion, or deletion, with a shared prefix and suffix.
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    prefix + suffix >= a.len().max(b.len()) - 1
}

fn grounded(index: &MarkdownIndex, id: usize, text: &str) -> Option<Grounded> {
    Some(Grounded {
        text: text.into(),
        evidence: index.resolve(id, text)?,
    })
}

pub(super) fn work_measure(
    index: &MarkdownIndex,
    id: usize,
    property: &Grounded,
    unit: &Grounded,
) -> Option<(Grounded, Grounded, Grounded)> {
    let block = index.blocks.get(id)?;
    if !matches!(block.kind, BlockKind::TableCell | BlockKind::HtmlTableCell)
        || block.table.is_none()
    {
        return None;
    }
    let wanted_unit = parse_unit(&unit.text)?;
    let name = property.text.trim();
    if name.is_empty() || !name.chars().all(char::is_alphabetic) {
        return None;
    }
    // Conditions and alternatives can be separated from the value by punctuation or markup.
    for range in &block.text_ranges {
        if index
            .source
            .get(range.clone())?
            .to_lowercase()
            .split(|c: char| !c.is_alphabetic())
            .any(|w| matches!(w, "если" | "при" | "или" | "либо"))
        {
            return None;
        }
    }
    let mut result = None;
    for range in &block.text_ranges {
        let text = index.source.get(range.clone())?;
        let mut words = text.char_indices().peekable();
        while let Some((start, ch)) = words.next() {
            if !ch.is_alphabetic() {
                continue;
            }
            let mut end = start + ch.len_utf8();
            while let Some(&(pos, c)) = words.peek() {
                if !c.is_alphabetic() {
                    break;
                }
                end = pos + c.len_utf8();
                words.next();
            }
            if !same_property(name, &text[start..end]) {
                continue;
            }
            // A bound or negation immediately before the property changes its meaning.
            let before = text[..start]
                .rsplit([',', ':', ';', '.', '\n'])
                .next()?
                .to_lowercase();
            if before.split(|c: char| !c.is_alphabetic()).any(|w| {
                matches!(
                    w,
                    "не" | "без" | "если" | "или" | "около" | "примерно" | "приблизительно"
                )
            }) {
                return None;
            }
            let tail = &text[end..];
            let value_start = tail.len() - tail.trim_start().len();
            let number = &tail[value_start..];
            let mut value_end = 0;
            for (i, c) in number.char_indices() {
                if c.is_ascii_digit() || c == ',' || c == '.' {
                    value_end = i + c.len_utf8();
                } else {
                    break;
                }
            }
            // A terminal punctuation mark is not part of a decimal.
            let value = number.get(..value_end)?.trim_end_matches([',', '.']);
            parse_decimal(value)?;
            let after_value = &number[value.len()..];
            let after_value = after_value.trim_start();
            let unit_len = after_value
                .char_indices()
                .take_while(|(_, c)| c.is_alphabetic())
                .last()
                .map(|(i, c)| i + c.len_utf8())?;
            let found_unit = &after_value[..unit_len];
            if parse_unit(found_unit)?.dimension != wanted_unit.dimension {
                return None;
            }
            let after = &after_value[unit_len..];
            let clause = after.split(['.', '\n']).next()?.to_lowercase();
            if clause.contains(['±', '-', '–', '—', '/', '<', '>', '=', '+', '~'])
                || clause.split(|c: char| !c.is_alphabetic()).any(|w| {
                    matches!(
                        w,
                        "или"
                            | "либо"
                            | "не"
                            | "без"
                            | "возможно"
                            | "предположительно"
                            | "до"
                            | "от"
                            | "если"
                            | "при"
                            | "около"
                            | "примерно"
                            | "приблизительно"
                    )
                })
            {
                return None;
            }
            let statement_end = text.len() - after.len();
            let statement = &text[start..statement_end];
            let candidate = (
                grounded(index, id, statement)?,
                grounded(index, id, value)?,
                grounded(index, id, found_unit)?,
            );
            if result.replace(candidate).is_some() {
                return None;
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_only_unambiguous_literals() {
        for (description, valid) in [
            ("Лифт грузовой грузоодъемностью500кг", true),
            (
                "скорость до 0,5 м/с: грузоподъемностью 500 кг на 4 остановки",
                true,
            ),
            ("грузоподъемность 500 кг, на 4 остановки", true),
            ("Лифт грузовой при аварии, грузоподъемность 500 кг", false),
            (
                "Лифт грузовой **при аварии**, грузоподъемность 500 кг",
                false,
            ),
            ("грузоподъемность 500 кг **или** 600 кг", false),
            ("немощность 500 кг", false),
            ("грузоподъемность не менее 500 кг", false),
            ("грузоподъемность 500-600 кг", false),
            ("грузоподъемность 500 кг или 600 кг", false),
            ("грузоподъемность 500 кг, или 0,6 т", false),
            ("грузоподъемность 500 кг и 600 кг", false),
            ("грузоподъемность 500 кг, грузоподъемность 600 кг", false),
            ("грузоподъемность. Масса 500 кг", false),
        ] {
            let source = format!("| Работы | Количество |\n| --- | --- |\n| {description} | 1 |\n");
            let index = MarkdownIndex::new(&source).unwrap();
            let block = index
                .blocks
                .iter()
                .find(|b| index.resolve(b.id, "500").is_some())
                .unwrap();
            let dummy = |s: &str| Grounded {
                text: s.into(),
                evidence: index.resolve(block.id, "500").unwrap(),
            };
            assert_eq!(
                work_measure(&index, block.id, &dummy("грузоподъемность"), &dummy("кг")).is_some(),
                valid,
                "{description}"
            );
        }
    }
}
