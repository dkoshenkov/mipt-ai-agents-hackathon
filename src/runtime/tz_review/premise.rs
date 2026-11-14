//! Check the document premise before publishing a legal interpretation.
use super::{Location, source::Source};

pub(super) enum Premise {
    Supported {
        source: Location,
        explanation: &'static str,
    },
    A05Evidence(Location, Location, &'static str),
    Dismiss(&'static str),
    Other,
}

fn commercial_value(row: &str, header: &str, quote: &str) -> bool {
    // Bind the identifier to a labelled field of this item. A heading elsewhere
    // in the table does not turn the ordinary product name into a brand.
    let cells = row.split('|').map(str::trim).collect::<Vec<_>>();
    let labels = [
        "производитель",
        "изготовитель",
        "модель",
        "референс",
        "товарный знак",
        "торговая марка",
        "бренд",
    ];
    let matches_value = |value: &str| {
        let value = value.trim();
        !value.is_empty()
            && !["не указан", "любой", "по выбору", "не установлен"]
                .iter()
                .any(|w| value.starts_with(w))
            && (quote == value
                || quote.contains(value)
                || value
                    .split_once(" или эквивалент")
                    .is_some_and(|(name, _)| name.trim() == quote))
    };
    let columns = header.split('|').map(str::trim);
    if cells
        .iter()
        .zip(columns)
        .any(|(value, column)| labels.contains(&column) && matches_value(value))
    {
        return true;
    }
    for (i, cell) in cells.iter().enumerate() {
        if labels.contains(cell) && cells.get(i + 1).is_some_and(|v| matches_value(v)) {
            return true;
        }
        let normalized = cell.replace("<br>", "\n");
        for label in labels {
            if let Some((_, rest)) = normalized.split_once(&format!("{label}:")) {
                let value = rest.trim().lines().next().unwrap_or_default();
                if matches_value(value) {
                    return true;
                }
            }
        }
    }
    row.contains(&format!("{quote}®")) || row.contains(&format!("{quote}™"))
}

/// Recover a mistyped A06 quotation only from an explicit field on the
/// candidate's own line and only if that identifier is named in its claim.
pub(super) fn named_field(source: &Source<'_>, lines: &[usize], claim: &str) -> Option<Location> {
    let claim = claim.to_lowercase();
    for &number in lines {
        let Some(row) = source.line(number) else {
            continue;
        };
        let parts: Vec<_> = row
            .quote
            .split('|')
            .flat_map(|c| c.split("<br>"))
            .map(str::trim)
            .collect();
        for (i, part) in parts.iter().enumerate() {
            let (key, tail) = part.split_once(':').unwrap_or((part, ""));
            if ![
                "производитель",
                "изготовитель",
                "модель",
                "товарный знак",
                "бренд",
                "референс",
            ]
            .contains(&key.to_lowercase().as_str())
            {
                continue;
            }
            let value = if tail.trim().is_empty() {
                parts.get(i + 1).copied().unwrap_or("")
            } else {
                tail.trim()
            };
            if value.len() < 2 || !claim.contains(&value.to_lowercase()) {
                continue;
            }
            if let Some(loc) = source.exact(&super::Anchor {
                line: number,
                quote: value.into(),
            }) {
                return Some(loc);
            }
        }
    }
    None
}

fn inactive(text: &str) -> bool {
    let withdrawn = text.split(|c: char| !c.is_alphabetic()).any(|w| {
        matches!(
            w,
            "исключен"
                | "исключён"
                | "исключены"
                | "исключена"
                | "исключено"
                | "отменен"
                | "отменён"
                | "отменены"
                | "отменена"
                | "отменено"
        )
    });
    withdrawn
        || [
            "утратил",
            "не применяется",
            "не действует",
            "пример:",
            "образец:",
        ]
        .iter()
        .any(|word| text.contains(word))
}

fn ban(text: &str) -> bool {
    if inactive(text) {
        return false;
    }
    text.split([',', ';', '.', '|']).any(clause_bans)
}

fn clause_bans(text: &str) -> bool {
    let words: Vec<_> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let equivalent = |w: &str| w.starts_with("эквивалент") || w.starts_with("аналог");
    // Require the prohibition and its grammatical object to be adjacent,
    // optionally through “поставка/применение”. Ignore a negated predicate.
    for (i, &word) in words.iter().enumerate() {
        let previous = &words[..i];
        let negated = previous.last() == Some(&"не");
        let prohibited = (word.starts_with("запрещ") && !negated)
            || ((word.starts_with("допуска") || word.starts_with("разреш")) && negated);
        if !prohibited {
            continue;
        }
        let subject = if negated {
            &previous[..i - 1]
        } else {
            previous
        };
        let next = &words[i + 1..];
        let supply = |w: &str| {
            matches!(
                w,
                "поставка"
                    | "поставку"
                    | "поставки"
                    | "применение"
                    | "использование"
                    | "предложение"
            )
        };
        if subject.last().is_some_and(|w| equivalent(w))
            || next.first().is_some_and(|w| equivalent(w))
            || (next.first().is_some_and(|w| supply(w))
                && next.get(1).is_some_and(|w| equivalent(w)))
        {
            return true;
        }
    }
    false
}

fn direct_restriction(source: &Source<'_>, e: &Location) -> bool {
    let Some(row) = source.line(e.line) else {
        return false;
    };
    let q = e.quote.to_lowercase();
    row.quote
        .to_lowercase()
        .split(['.', ';', '|', '!', '?'])
        .any(|clause| {
            if inactive(clause) || !clause.split_whitespace().any(|w| q.contains(w)) {
                return false;
            }
            q.split(['.', ';', '|', '!', '?'])
                .map(str::trim)
                .filter(|s| !s.is_empty() && clause.contains(s))
                .any(|part| {
                    (ban(part) && ban(clause) && part == clause.trim())
                        || ([
                            "исключительно производства ",
                            "только производства ",
                            "только от производителя ",
                        ]
                        .iter()
                        .any(|w| part.contains(w))
                            && part == clause.trim()
                            && !clause.split_whitespace().any(|w| w == "не"))
                })
        })
}

pub(super) fn item_row(text: &str) -> bool {
    let first = text
        .trim()
        .trim_start_matches('|')
        .trim_start()
        .to_lowercase();
    first.starts_with("позиция ") || first.chars().next().is_some_and(|c| c.is_ascii_digit())
}

pub(super) fn position_number(text: &str) -> Option<u32> {
    let lower = text.trim().to_lowercase();
    let value = lower
        .strip_prefix("позиция ")?
        .trim_start_matches('№')
        .trim_start();
    let digits = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    value[..digits].parse().ok()
}

pub(super) fn historical(heading: &str) -> bool {
    let lower = heading.to_lowercase();
    inactive(&lower)
        || ["архив", "прошл", "предыдущ", "пример", "образец"]
            .iter()
            .any(|s| lower.contains(s))
}

pub(super) fn single_supplier(text: &str) -> bool {
    let source = Source::new(text);
    text.lines()
        .enumerate()
        .any(|(i, row)| procedure(row) == Procedure::Single && !source.historical_at(i + 1))
}

pub(super) fn global_condition(text: &str) -> bool {
    let lower = text.to_lowercase();
    !item_row(text)
        && [
            "для всех позиций",
            "ко всем позициям",
            "указанные в тз товарные знаки",
        ]
        .iter()
        .any(|w| lower.contains(w))
}

fn allows_equivalent(text: &str) -> bool {
    let lower = text.to_lowercase();
    if inactive(&lower) || ban(&lower) {
        return false;
    }
    [
        "или эквивалент",
        "или аналог",
        "допускаются эквивалент",
        "допускаются аналог",
        "разрешены аналоги",
        "допускается поставка эквивалент",
        "эквиваленты допускаются",
        "эквиваленты разрешены",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

/// None means the scope cannot safely support an adverse conclusion.
fn permission_for(text: &str, item: Option<u32>) -> Option<bool> {
    if !allows_equivalent(text) {
        return Some(false);
    }
    let lower = text.to_lowercase();
    let Some(tail) = ["кроме ", "за исключением ", "исключая "]
        .iter()
        .find_map(|prefix| lower.split_once(prefix).map(|(_, s)| s))
    else {
        return Some(true);
    };
    let tail = ["позиций ", "позиции ", "позицию "]
        .iter()
        .find_map(|s| tail.strip_prefix(s))?;
    let item = item?;
    let mut excluded = false;
    let mut tail = tail.trim_start();
    loop {
        tail = tail.trim_start_matches('№').trim_start();
        let length = tail.bytes().take_while(u8::is_ascii_digit).count();
        let first: u32 = tail[..length].parse().ok()?;
        tail = tail[length..].trim_start();
        let mut last = first;
        if let Some(rest) = tail.strip_prefix(['-', '–', '—']) {
            let rest = rest.trim_start();
            let length = rest.bytes().take_while(u8::is_ascii_digit).count();
            last = rest[..length].parse().ok()?;
            if last < first {
                return None;
            }
            tail = rest[length..].trim_start();
        }
        excluded |= (first..=last).contains(&item);
        if let Some(rest) = tail.strip_prefix("и ") {
            tail = rest;
            continue;
        }
        if let Some(rest) = tail.strip_prefix(',') {
            let rest = rest.trim_start();
            if rest.starts_with(|c: char| c.is_ascii_digit() || c == '№') {
                tail = rest;
                continue;
            }
            if rest.starts_with("допуска") || rest.starts_with("разреш") || rest.is_empty()
            {
                break;
            }
            return None;
        }
        if tail.is_empty()
            || tail == "."
            || tail.starts_with("допуска")
            || tail.starts_with("разреш")
        {
            break;
        }
        return None;
    }
    Some(!excluded)
}

#[derive(PartialEq)]
pub(super) enum Procedure {
    Competitive,
    Single,
    Unknown,
}

/// Recognise a declaration of the method for this purchase (not a reference to
/// another procedure, a negated statement or a list of possible methods).
pub(super) fn procedure(text: &str) -> Procedure {
    let lower = text.to_lowercase();
    let plain = lower.trim_matches(|c: char| c.is_whitespace() || "#|.".contains(c));
    let declaration = plain.strip_prefix("извещение. ").unwrap_or(plain);
    let method = [
        "способ закупки:",
        "способ определения поставщика:",
        "способ:",
        "форма закупки:",
        "вид закупки:",
    ]
    .iter()
    .find_map(|prefix| declaration.strip_prefix(prefix))
    .unwrap_or(declaration)
    .trim();
    match method {
        "конкурс"
        | "открытый конкурс"
        | "аукцион"
        | "электронный аукцион"
        | "запрос котировок"
        | "запрос предложений"
        | "конкурентная закупка" => Procedure::Competitive,
        "закупка у единственного поставщика"
        | "у единственного поставщика"
        | "неконкурентная закупка" => Procedure::Single,
        _ => Procedure::Unknown,
    }
}

fn mandatory_field(sentence: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    let sentence = [
        "в описании предмета закупки ",
        "в описании товара ",
        "в техническом задании ",
        "в тз ",
    ]
    .iter()
    .find_map(|prefix| sentence.strip_prefix(prefix))
    .unwrap_or(sentence);
    let label = [
        "обязательно указать ",
        "необходимо указать ",
        "требуется указать ",
        "заказчик обязан указать ",
        "подлежит обязательному указанию ",
    ]
    .iter()
    .find_map(|prefix| sentence.strip_prefix(prefix))
    .or_else(|| sentence.strip_suffix(" обязательно для заполнения"))
    .or_else(|| sentence.strip_suffix(" обязателен для заполнения"));
    let Some(label) = label else { return false };
    let label = label
        .strip_prefix("поле ")
        .or_else(|| label.strip_prefix("параметр "))
        .unwrap_or(label);
    label.trim_matches(['«', '»', '"']) == name
}

fn empty_field_name(row: &Location) -> Option<&str> {
    if let Some((name, value)) = row.quote.trim().trim_end_matches('.').split_once(':')
        && matches!(value.trim(), "не указана" | "не указан" | "не указано")
        && let Some(name) = name
            .strip_prefix("Обязательная характеристика ")
            .or_else(|| name.strip_prefix("Параметр "))
    {
        return Some(name.trim_matches(['«', '»']));
    }
    let cells = row.quote.trim().strip_prefix('|')?.strip_suffix('|')?;
    let cells: Vec<_> = cells.split('|').map(str::trim).collect();
    let header: Vec<_> = row
        .table_header
        .trim()
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_lowercase())
        .collect();
    let name_col = header.iter().position(|c| {
        matches!(
            c.as_str(),
            "параметр"
                | "характеристика"
                | "наименование характеристики"
                | "наименование параметра"
        )
    });
    let value_col = header.iter().position(|c| {
        matches!(
            c.as_str(),
            "значение" | "значение характеристики" | "требуемое значение"
        )
    });
    let (name, value) = match (name_col, value_col) {
        (Some(n), Some(v)) if header.len() == cells.len() => (cells[n], cells[v]),
        (None, None) => match cells.as_slice() {
            [name, value] => (*name, *value),
            [number, name, value] if number.parse::<usize>().is_ok() => (*name, *value),
            _ => return None,
        },
        _ => return None,
    };
    (!name.is_empty() && value.is_empty()).then_some(name)
}

/// A05 may concern a missing characteristic only when the source establishes
/// that its value is required in the specification for this very item.
fn required_empty_field(
    source: &Source<'_>,
    evidence: &[Location],
) -> Option<(Location, Location)> {
    for field in evidence {
        let row = source.line(field.line)?;
        if source.historical_at(field.line) {
            continue;
        }
        let Some(name) = empty_field_name(&row) else {
            continue;
        };
        if !field.quote.contains(name) {
            continue;
        }
        for requirement in evidence {
            if requirement.line == field.line || source.historical_at(requirement.line) {
                continue;
            }
            let full = source.line(requirement.line)?.quote;
            let sentence = full.trim().trim_end_matches('.').to_lowercase();
            let applicable = mandatory_field(&sentence, name);
            if applicable
                && requirement
                    .quote
                    .trim()
                    .trim_end_matches('.')
                    .to_lowercase()
                    == sentence
                && source.item_number(requirement.line) == source.item_number(field.line)
                && source.same_section_or_parent(requirement.line, field.line)
            {
                return Some((row, requirement.clone()));
            }
        }
    }
    None
}

/// Preserve the critic's standard-based A05 when the source itself ties
/// an applicable clause to an explicit departure. Both quotations go to the reader.
fn standard_departure(source: &Source<'_>, evidence: &[Location]) -> Option<(Location, Location)> {
    for norm in evidence {
        let line = source.line(norm.line)?.quote.to_lowercase();
        let rule = norm.quote.to_lowercase();
        if source.historical_at(norm.line)
            || inactive(&line)
            || [
                "не обязател",
                "не подлежит",
                "не должен",
                "необходимость применения не",
            ]
            .iter()
            .any(|s| line.contains(s))
            || !["обязател", "должен соответствовать", "подлежит применению"]
                .iter()
                .any(|s| rule.contains(s))
            || !["регламент", "стандарт", "гост"]
                .iter()
                .any(|s| rule.contains(s))
        {
            continue;
        }
        let Some(clause) = clause_reference(&rule) else {
            continue;
        };
        for fact in evidence {
            if fact.line == norm.line
                || source.historical_at(fact.line)
                || source
                    .item_number(norm.line)
                    .zip(source.item_number(fact.line))
                    .is_some_and(|(a, b)| a != b)
                || !source.same_section_or_parent(norm.line, fact.line)
            {
                continue;
            }
            let text = fact.quote.to_lowercase();
            let whole = source.line(fact.line)?.quote.to_lowercase();
            if inactive(&whole)
                || whole.contains("не отказывается")
                || whole.contains("не отступает")
            {
                continue;
            }
            // A repeated clause number alone does not identify a different standard.
            if standard_id(&text).is_some_and(|other| standard_id(&rule) != Some(other)) {
                continue;
            }
            if clause_reference(&text) == Some(clause)
                && [
                    "отказывается применять",
                    "отступает от",
                    "допускает отступление от",
                ]
                .iter()
                .any(|s| text.contains(s))
                && [
                    "обоснование отсутствует",
                    "обоснование отступления отсутствует",
                    "без обоснования",
                ]
                .iter()
                .any(|s| text.contains(s))
            {
                return Some((fact.clone(), norm.clone()));
            }
        }
    }
    None
}

fn standard_id(text: &str) -> Option<&str> {
    let mut words = text.split_whitespace();
    while let Some(word) = words.next() {
        if word.starts_with("стандарт") || word.starts_with("регламент") || word == "гост"
        {
            return words
                .next()
                .map(|id| id.trim_matches(|c: char| !c.is_alphanumeric() && c != '-'));
        }
    }
    None
}

fn clause_reference(text: &str) -> Option<&str> {
    let words: Vec<_> = text.split_whitespace().collect();
    words.windows(2).find_map(|pair| {
        let id = pair[1].trim_end_matches(['.', ',', ';', ':']);
        (matches!(pair[0], "пункт" | "пункта" | "пункту" | "п.")
            && !id.is_empty()
            && id
                .split('.')
                .all(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())))
        .then_some(id)
    })
}

pub(super) fn evaluate(
    id: &str,
    source: &Source<'_>,
    evidence: &[Location],
    context: &[Location],
    item: Option<u32>,
) -> Premise {
    let (anchor, explanation) = match id {
        "A06" => {
            let anchor = evidence.iter().find(|e| {
                let Some(row) = source.line(e.line) else {
                    return false;
                };
                let q = e.quote.trim().to_lowercase();
                q.chars().any(char::is_alphanumeric)
                    && commercial_value(
                        &row.quote.to_lowercase(),
                        &row.table_header.to_lowercase(),
                        &q,
                    )
            });
            let Some(anchor) = anchor else {
                return Premise::Dismiss(
                    "Цитаты не устанавливают коммерческий идентификатор позиции",
                );
            };
            let row = source.line(anchor.line).expect("verified anchor");
            let item = item.or_else(|| source.item_number(anchor.line));
            let applicable = context.iter().filter(|c| !source.historical_at(c.line));
            let local_ban = row.quote.to_lowercase().split(['.', ';', '|']).any(ban)
                || applicable.clone().any(|c| {
                    item.is_some()
                        && source.item_number(c.line) == item
                        && ban(&c.quote.to_lowercase())
                });
            if !local_ban {
                for text in std::iter::once(&row.quote).chain(applicable.map(|c| &c.quote)) {
                    match permission_for(text, item) {
                        Some(true) => {
                            return Premise::Dismiss("Для позиции указано разрешение эквивалента");
                        }
                        None => {
                            return Premise::Dismiss(
                                "Область исключения неоднозначна, отсутствие разрешения не установлено",
                            );
                        }
                        Some(false) => {}
                    }
                }
            }
            (
                anchor,
                "В описании позиции указан коммерческий идентификатор. Оценка зависит от режима процедуры, условий об эквивалентности и применимости исключений.",
            )
        }
        "A04" => {
            let admission = evidence.iter().any(|e| {
                let quote = e.quote.to_lowercase();
                [
                    "участник",
                    "претендент",
                    "к участию",
                    "отклонение заявки",
                    "отклонения заявки",
                    "заявка отклоняется",
                    "допуск к закупке",
                ]
                .iter()
                .any(|term| quote.contains(term))
            });
            if !admission {
                return Premise::Dismiss(
                    "В цитатах отсутствует условие допуска участника к закупке",
                );
            }
            return Premise::Other;
        }
        "A05" => {
            return match required_empty_field(source, evidence) {
                Some((field, requirement)) => Premise::A05Evidence(
                    field,
                    requirement,
                    "В описании прямо требуется указать характеристику, но её значение в ТЗ не указано.",
                ),
                None if let Some((fact, norm)) = standard_departure(source, evidence) => {
                    Premise::A05Evidence(
                        fact,
                        norm,
                        "В описании указан обязательный пункт и прямо заявлено отступление от него без обоснования. Следует проверить применимость нормы и материалы закупки.",
                    )
                }
                None => Premise::Dismiss(
                    "Цитаты не устанавливают обязанность указать конкретную недостающую характеристику или иное доказанное основание A05",
                ),
            };
        }
        "A03" => {
            let Some(anchor) = evidence.iter().find(|e| direct_restriction(source, e)) else {
                return Premise::Dismiss(
                    "В доказательствах нет прямого условия, ограничивающего выбор; параметры сами по себе его не устанавливают",
                );
            };
            (
                anchor,
                "В документе прямо сформулировано ограничивающее условие, приведённое в цитате. Его обоснованность зависит от условий конкретной процедуры.",
            )
        }
        _ => return Premise::Other,
    };
    Premise::Supported {
        source: anchor.clone(),
        explanation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recover_only_an_identifier_named_in_the_same_candidate() {
        let source = Source::new(
            "| 1 | Насос Alpha |\n| 1 | Производитель:<br>Alpha |\n| 2 | Производитель:<br>Beta |",
        );
        let found = named_field(&source, &[1, 2], "Насос ALPHA указан как единственный").unwrap();
        assert_eq!((found.line, found.quote.as_str()), (2, "Alpha"));
        assert!(named_field(&source, &[1, 3], "Насос ALPHA").is_none());
        assert!(named_field(&source, &[1], "Насос Alpha").is_none());
    }

    #[test]
    fn name_and_specification_are_insufficient_but_label_and_ban_are_evidence() {
        let source = Source::new(
            "| 1 | Реагент AB-12 | 50 мг |\n| 2 | Производитель:<br>Завод X |\nЭквиваленты не допускаются.\n| Модель | X7 или эквивалент |",
        );
        for (id, n, text, supported) in [
            ("A03", 1, "50 мг", false),
            ("A06", 1, "Реагент AB-12", false),
            ("A03", 3, "Эквиваленты не допускаются", true),
            ("A06", 2, "Завод X", true),
            ("A06", 4, "X7", false),
        ] {
            let location = source
                .exact(&super::super::Anchor {
                    line: n,
                    quote: text.into(),
                })
                .unwrap();
            assert_eq!(
                matches!(
                    evaluate(id, &source, &[location], &[], None),
                    Premise::Supported { .. }
                ),
                supported
            );
        }
    }
}
