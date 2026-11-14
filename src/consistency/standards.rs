//! Local, dated standard-card lookup. A snapshot is never a complete search.
use super::markdown::{Evidence, MarkdownIndex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key {
    pub family: String,
    pub number: String,
    pub year: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mention {
    pub original: String,
    pub key: Key,
    pub evidence: Evidence,
    /// The addressable block is retained without inferring that a list is a requirement.
    pub context: Evidence,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scan {
    pub mentions: Vec<Mention>,
    pub unread_ranges: Vec<std::ops::Range<usize>>,
}

/// Find explicit numbered designations in visible Markdown text, preserving every address.
pub fn scan(index: &MarkdownIndex) -> Scan {
    let mut mentions = Vec::new();
    for block in &index.blocks {
        for range in &block.text_ranges {
            let text = &index.source[range.clone()];
            let mut consumed = 0;
            for (offset, _) in text.char_indices() {
                if offset < consumed {
                    continue;
                }
                if offset > 0
                    && text[..offset]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_alphanumeric)
                {
                    continue;
                }
                if let Some((key, end)) = parse_prefix(&text[offset..]) {
                    let start = range.start + offset;
                    consumed = offset + end;
                    let end = start + end;
                    mentions.push(Mention {
                        original: index.source[start..end].to_owned(),
                        key,
                        evidence: Evidence {
                            version: index.version.clone(),
                            block_id: block.id,
                            range: start..end,
                        },
                        context: Evidence {
                            version: index.version.clone(),
                            block_id: block.id,
                            range: block.range.clone(),
                        },
                    });
                }
            }
        }
    }
    Scan {
        mentions,
        unread_ranges: index.gaps.clone(),
    }
}

/// Explicit references to missing appendices that define standards or normative documents.
pub fn missing_materials(index: &MarkdownIndex) -> Vec<super::claims::Grounded> {
    fn appendix_id(text: &str) -> Option<String> {
        let mut words = text.split(|c: char| !c.is_alphanumeric());
        while let Some(word) = words.next() {
            if word.to_lowercase().starts_with("приложен") {
                let id = words.find(|s| !s.is_empty())?;
                if (id.chars().count() == 1 && id.chars().all(char::is_alphabetic))
                    || (id.len() <= 3 && id.bytes().all(|b| b.is_ascii_digit()))
                {
                    return Some(id.to_uppercase());
                }
            }
        }
        None
    }
    let supplied: std::collections::HashSet<_> = index
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, super::markdown::BlockKind::Heading))
        .filter_map(|b| appendix_id(&index.source[b.range.clone()]))
        .collect();
    let mut missing = Vec::new();
    for block in &index.blocks {
        if matches!(block.kind, super::markdown::BlockKind::Heading) {
            continue;
        }
        for range in &block.text_ranges {
            let text = &index.source[range.clone()];
            let lower = text.to_lowercase();
            if !(lower.contains("стандарт") || lower.contains("норматив") || lower.contains("гост"))
            {
                continue;
            }
            if appendix_id(text).is_some_and(|id| !supplied.contains(&id)) {
                missing.push(super::claims::Grounded {
                    text: text.to_owned(),
                    evidence: Evidence {
                        version: index.version.clone(),
                        block_id: block.id,
                        range: range.clone(),
                    },
                });
            }
        }
    }
    missing
}

/// Rebind a control-pass designation before adding it to the local scan.
pub(super) fn recovered(index: &MarkdownIndex, found: &super::claims::Grounded) -> Option<Mention> {
    let (key, end) = parse_prefix(&found.text)?;
    if end != found.text.len()
        || index.resolve(found.evidence.block_id, &found.text)? != found.evidence
    {
        return None;
    }
    let block = index.blocks.get(found.evidence.block_id)?;
    Some(Mention {
        original: found.text.clone(),
        key,
        evidence: found.evidence.clone(),
        context: Evidence {
            range: block.range.clone(),
            ..found.evidence.clone()
        },
    })
}

pub(super) fn parse_prefix(text: &str) -> Option<(Key, usize)> {
    let (first, mut pos) = word(text, 0)?;
    if !matches!(
        first.as_str(),
        "ГОСТ" | "КОСТ" | "ИСО" | "МЭК" | "ISO" | "IEC"
    ) {
        return None;
    }
    let mut family = first;
    loop {
        let space = skip_space(text, pos);
        let next = if text[space..].starts_with('/') {
            skip_space(text, space + 1)
        } else {
            space
        };
        if next == pos {
            return None;
        }
        let (token, end) = word(text, next)?;
        if matches!(token.as_str(), "Р" | "ИСО" | "МЭК" | "ISO" | "IEC") {
            family.push(' ');
            family.push_str(&token);
            pos = end;
            continue;
        }
        let bytes = text.as_bytes();
        if !bytes[next].is_ascii_digit() {
            return None;
        }
        let mut end = next;
        while end < bytes.len()
            && (bytes[end].is_ascii_digit()
                || (bytes[end] == b'.' && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)))
        {
            end += 1;
        }
        let mut number = text[next..end].to_owned();
        let mut year = None;
        while let Some(dash) = text[end..]
            .chars()
            .next()
            .filter(|c| matches!(c, '-' | '–' | '—'))
        {
            let start = end + dash.len_utf8();
            let mut finish = start;
            while finish < bytes.len() && bytes[finish].is_ascii_digit() {
                finish += 1;
            }
            if finish == start {
                break;
            }
            let digits = &text[start..finish];
            let more = text[finish..]
                .chars()
                .next()
                .is_some_and(|c| matches!(c, '-' | '–' | '—'));
            if !more && matches!(digits.len(), 2 | 4) {
                year = Some(digits.to_owned());
            } else {
                number.push('-');
                number.push_str(digits);
            }
            end = finish;
        }
        return Some((
            Key {
                family,
                number,
                year,
            },
            end,
        ));
    }
}

fn skip_space(s: &str, p: usize) -> usize {
    p + s[p..]
        .chars()
        .take_while(|c| c.is_whitespace())
        .map(char::len_utf8)
        .sum::<usize>()
}
fn word(s: &str, p: usize) -> Option<(String, usize)> {
    let n = s[p..]
        .chars()
        .take_while(|c| c.is_alphanumeric())
        .map(char::len_utf8)
        .sum::<usize>();
    (n > 0).then(|| (s[p..p + n].to_uppercase(), p + n))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    Standard,
    Correction,
    Reference,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemperatureScope {
    pub method: String,
    pub min_c: f64,
    pub max_c: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Card {
    pub kind: CardKind,
    pub designation: String,
    pub full_designation: String,
    pub title: String,
    pub url: String,
    pub scope: String,
    pub temperature_scope: Option<TemperatureScope>,
    pub effective_from: Option<String>,
    pub replaces: Option<String>,
    pub replacement: Option<String>,
    pub status_at_retrieval: String,
    pub rf_note: Option<String>,
    #[serde(default)]
    pub clauses: Vec<OfficialClause>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OfficialClause {
    pub number: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub retrieved_at: String,
    pub cards: Vec<Card>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Existence {
    Exact,
    Undated,
    PartialSearch,
    NotFoundInOfficialSearch,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DateStatus {
    SnapshotStatus(String),
    BeforeEffective,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReferenceReview {
    pub mention: Mention,
    pub existence: Existence,
    pub cards: Vec<Card>,
    /// Similar local cards are candidates only; they do not establish a replacement.
    #[serde(default)]
    pub variants: Vec<Variant>,
    #[serde(default)]
    pub confirmed_family_variants: Vec<Card>,
    pub date_status: Vec<DateStatus>,
    pub snapshot_date: String,
    pub assessment_date: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Variant {
    pub card: Card,
    pub component: VariantComponent,
    pub original: String,
    pub alternative: String,
}
/// A family variant must have the same dated number and its title in the source context.
pub(super) fn confirmed_family_variant(key: &Key, card: &Card, context: &str) -> bool {
    let Some((other, end)) = parse_prefix(&card.designation) else {
        return false;
    };
    let normalize = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let context = normalize(context);
    let title = normalize(&card.title);
    let title_matches = !title.is_empty()
        && (context
            .split('«')
            .skip(1)
            .filter_map(|s| s.split_once('»').map(|(s, _)| s))
            .any(|s| s == title)
            || context.split('"').skip(1).step_by(2).any(|s| s == title)
            || context.split('|').any(|s| s.trim() == title));
    end == card.designation.len()
        && key.year.is_some()
        && other.year == key.year
        && other.number == key.number
        && other.family != key.family
        && card_identity_valid(card)
        && title_matches
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum VariantComponent {
    Family,
    Number,
    Series,
    Part,
    Year,
}
impl Catalog {
    /// Missing cards always mean incomplete search; an undated citation remains undated.
    pub fn review(&self, scan: &Scan, assessment_date: &str) -> Vec<ReferenceReview> {
        let index = CatalogIndex::new(self);
        scan.mentions
            .iter()
            .map(|mention| {
                let cards: Vec<_> = index
                    .key_matches(&mention.key)
                    .into_iter()
                    .cloned()
                    .collect();
                let existence = if cards.is_empty() {
                    Existence::PartialSearch
                } else if mention.key.year.is_none() {
                    Existence::Undated
                } else {
                    Existence::Exact
                };
                let date_status = cards
                    .iter()
                    .map(|card| {
                        if valid_date(assessment_date)
                            && card
                                .effective_from
                                .as_deref()
                                .is_some_and(|date| valid_date(date) && assessment_date < date)
                        {
                            DateStatus::BeforeEffective
                        } else if valid_date(assessment_date)
                            && assessment_date == self.retrieved_at
                            && !card.status_at_retrieval.trim().is_empty()
                        {
                            DateStatus::SnapshotStatus(card.status_at_retrieval.clone())
                        } else {
                            DateStatus::Unknown
                        }
                    })
                    .collect();
                ReferenceReview {
                    mention: mention.clone(),
                    existence,
                    cards,
                    variants: index
                        .variants(&mention.key)
                        .into_iter()
                        .filter_map(|card| {
                            let (component, original, alternative) =
                                variant_difference(&mention.key, &card.designation)?;
                            Some(Variant {
                                card: card.clone(),
                                component,
                                original,
                                alternative,
                            })
                        })
                        .collect(),
                    confirmed_family_variants: Vec::new(),
                    date_status,
                    snapshot_date: self.retrieved_at.clone(),
                    assessment_date: assessment_date.to_owned(),
                }
            })
            .collect()
    }
    /// Exact official titles only; no input text is transmitted.
    pub fn title_matches(&self, title: &str) -> Vec<&Card> {
        CatalogIndex::new(self).title_matches(title)
    }
}

/// An index over validated cards in this particular, potentially incomplete snapshot.
pub struct CatalogIndex<'a> {
    keys: HashMap<(String, String), Vec<&'a Card>>,
    titles: HashMap<String, Vec<&'a Card>>,
}
impl<'a> CatalogIndex<'a> {
    pub fn new(catalog: &'a Catalog) -> Self {
        let mut index = Self {
            keys: HashMap::new(),
            titles: HashMap::new(),
        };
        for card in &catalog.cards {
            if card.kind != CardKind::Standard || !card_identity_valid(card) {
                continue;
            }
            let Some((key, _)) = parse_prefix(&card.designation) else {
                continue;
            };
            index
                .keys
                .entry((key.family, key.number))
                .or_default()
                .push(card);
            index
                .titles
                .entry(card.title.trim().to_lowercase())
                .or_default()
                .push(card);
        }
        index
    }
    pub fn key_matches(&self, key: &Key) -> Vec<&'a Card> {
        self.keys
            .get(&(key.family.clone(), key.number.clone()))
            .into_iter()
            .flatten()
            .copied()
            .filter(|card| matches_reference(card, key))
            .collect()
    }
    pub fn title_matches(&self, title: &str) -> Vec<&'a Card> {
        self.titles
            .get(&title.trim().to_lowercase())
            .cloned()
            .unwrap_or_default()
    }
    /// Returns only single-component edits, with no assumption that any is correct.
    pub fn variants(&self, key: &Key) -> Vec<&'a Card> {
        let mut result: Vec<_> = self
            .keys
            .values()
            .flatten()
            .copied()
            .filter(|card| one_component_variant(key, &card.designation))
            .collect();
        result.sort_by(|a, b| (&a.designation, &a.url).cmp(&(&b.designation, &b.url)));
        result
    }
}

pub(super) fn matches_reference(card: &Card, key: &Key) -> bool {
    card.kind == CardKind::Standard
        && card_identity_valid(card)
        && parse_prefix(&card.designation).is_some_and(|(actual, end)| {
            end == card.designation.len()
                && actual.family == key.family
                && actual.number == key.number
                && (key.year.is_none() || actual.year == key.year)
        })
}

fn card_identity_valid(card: &Card) -> bool {
    let Some((key, end)) = parse_prefix(&card.designation) else {
        return false;
    };
    end == card.designation.len()
        && parse_prefix(&card.full_designation).is_some_and(|(full, _)| full == key)
}

/// A caller must bind the stated method and temperature to the same usage. This
/// check compares only those two conditions, not the rest of the standard.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Applicability {
    SupportedWithinComparedConditions,
    OutsideTemperatureRange,
    InsufficientEvidence,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicabilityReview {
    pub outcome: Applicability,
    pub method_evidence: Evidence,
    pub temperature_evidence: Evidence,
    pub official_url: String,
    pub official_field: String,
    pub official_excerpt: String,
}
pub fn compare_temperature(
    index: &MarkdownIndex,
    mention: &Mention,
    card: &Card,
    method: Evidence,
    temperature: Evidence,
) -> ApplicabilityReview {
    let bound = |e: &Evidence| {
        e.version == index.version
            && e.block_id == mention.evidence.block_id
            && index.blocks.get(e.block_id).is_some_and(|b| {
                b.text_ranges
                    .iter()
                    .any(|r| r.start <= e.range.start && e.range.end <= r.end)
            })
    };
    let exact_card = card.kind == CardKind::Standard
        && parse_prefix(&card.designation).is_some_and(|(key, end)| {
            end == card.designation.len() && key == mention.key && key.year.is_some()
        });
    let outcome = if exact_card && bound(&method) && bound(&temperature) {
        let m = index
            .source
            .get(method.range.clone())
            .unwrap_or("")
            .to_lowercase();
        let t = index.source.get(temperature.range.clone()).unwrap_or("");
        let number = t
            .trim()
            .strip_suffix("°C")
            .or_else(|| t.trim().strip_suffix("°С"))
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|n| n.is_finite());
        match (&card.temperature_scope, number) {
            (Some(scope), Some(value)) if m == scope.method.to_lowercase() => {
                if (scope.min_c..=scope.max_c).contains(&value) {
                    Applicability::SupportedWithinComparedConditions
                } else {
                    Applicability::OutsideTemperatureRange
                }
            }
            _ => Applicability::InsufficientEvidence,
        }
    } else {
        Applicability::InsufficientEvidence
    };
    ApplicabilityReview {
        outcome,
        method_evidence: method,
        temperature_evidence: temperature,
        official_url: card.url.clone(),
        official_field: "Область применения".into(),
        official_excerpt: card.scope.clone(),
    }
}

/// A single bounded edit in one designation component; this produces a candidate, not proof.
pub fn one_component_variant(key: &Key, designation: &str) -> bool {
    variant_difference(key, designation).is_some()
}

fn variant_difference(key: &Key, designation: &str) -> Option<(VariantComponent, String, String)> {
    let (other, end) = parse_prefix(designation)?;
    if end != designation.len() {
        return None;
    }
    let left_family = key.family.replace(' ', "");
    let right_family = other.family.replace(' ', "");
    let mut difference = None;
    for (component, a, b) in [
        (VariantComponent::Family, &left_family, &right_family),
        (VariantComponent::Number, &key.number, &other.number),
    ] {
        if a != b {
            if !one_edit(a, b) || difference.is_some() {
                return None;
            }
            let component = if component == VariantComponent::Number {
                let (base_a, part_a) = a.split_once('-').unwrap_or((a, ""));
                let (base_b, part_b) = b.split_once('-').unwrap_or((b, ""));
                if base_a == base_b && part_a != part_b {
                    VariantComponent::Part
                } else if base_a.split_once('.').map(|x| x.0) == base_b.split_once('.').map(|x| x.0)
                    && base_a.contains('.')
                    && base_b.contains('.')
                {
                    VariantComponent::Series
                } else {
                    VariantComponent::Number
                }
            } else {
                component
            };
            difference = Some(if component == VariantComponent::Family {
                (component, key.family.clone(), other.family.clone())
            } else {
                (component, a.clone(), b.clone())
            });
        }
    }
    if key.year != other.year {
        match (&key.year, &other.year) {
            (Some(a), Some(b)) if one_edit(a, b) && difference.is_none() => {
                difference = Some((VariantComponent::Year, a.clone(), b.clone()));
            }
            _ => return None,
        }
    }
    difference
}
fn one_edit(a: &str, b: &str) -> bool {
    let a: Vec<_> = a.chars().collect();
    let b: Vec<_> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        if a.len() >= b.len() {
            i += 1;
        }
        if b.len() >= a.len() {
            j += 1;
        }
    }
    edits + (a.len() - i) + (b.len() - j) == 1
}

fn valid_date(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .is_ok_and(|date| date.format("%Y-%m-%d").to_string() == s)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reviews_original_before_reporting_grounded_single_component_candidates() {
        let examples = [
            (
                "ГОСТ 1234-2018",
                "ГОСТ Р 1234-2018",
                VariantComponent::Family,
            ),
            (
                "ГОСТ Р 1234-2018",
                "ГОСТ 1234-2018",
                VariantComponent::Family,
            ),
            ("ГОСТ 1234-2018", "ГОСТ 1235-2018", VariantComponent::Number),
            ("ГОСТ 12.3-2018", "ГОСТ 12.4-2018", VariantComponent::Series),
            (
                "ГОСТ 1234-1-2018",
                "ГОСТ 1234-2-2018",
                VariantComponent::Part,
            ),
            ("ГОСТ 1234-2018", "ГОСТ 1234-2019", VariantComponent::Year),
        ];
        let fixture: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        for (original, alternative, component) in examples {
            let mut card = fixture.cards[0].clone();
            card.designation = alternative.into();
            card.full_designation = alternative.into();
            let catalog = Catalog {
                retrieved_at: fixture.retrieved_at.clone(),
                cards: vec![card],
            };
            let scan = scan(&MarkdownIndex::new(original).unwrap());
            let result = &catalog.review(&scan, "2026-11-13")[0];
            assert_eq!(result.mention.original, original);
            assert!(matches!(result.existence, Existence::PartialSearch));
            assert!(result.cards.is_empty());
            assert_eq!(result.variants.len(), 1);
            let variant = &result.variants[0];
            assert_eq!(variant.component, component);
            assert_ne!(variant.original, variant.alternative);
            assert_eq!(variant.card.designation, alternative);
            assert!(!variant.card.url.is_empty());
        }
    }

    #[test]
    fn ordinary_text_lists_and_unrelated_valid_standard() {
        let text = "При приемке по ГОСТ 5555-2020 допускаются ГОСТ Р ИСО 4287 и ГОСТ 10905. По действующим ГОСТ. КОСТ 9013.";
        let index = MarkdownIndex::new(text).unwrap();
        let found = scan(&index);
        assert_eq!(found.mentions.len(), 4);
        for mention in &found.mentions {
            assert_eq!(
                &index.source[mention.evidence.range.clone()],
                mention.original
            );
        }
        let mut catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let mut card = catalog.cards[0].clone();
        card.designation = "ГОСТ 5555-2020".into();
        card.full_designation = card.designation.clone();
        card.title = "Стандарт другой отрасли".into();
        catalog.cards = vec![card];
        let review = catalog.review(&found, "2026-11-13");
        assert!(matches!(review[0].existence, Existence::Exact));
        assert!(review[0].variants.is_empty());
        assert_eq!(catalog.title_matches("Стандарт другой отрасли").len(), 1);
        assert!(catalog.title_matches("Стандарт другой отрасль").is_empty());
    }

    #[test]
    fn exact_card_and_similar_card_remain_separate() {
        let mut catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let mut alternative = catalog.cards[0].clone();
        alternative.designation = "ГОСТ Р 9013-59".into();
        alternative.full_designation = alternative.designation.clone();
        catalog.cards.push(alternative);
        let found = scan(&MarkdownIndex::new("ГОСТ 9013-59").unwrap());
        let review = catalog.review(&found, "2026-11-13");
        assert!(matches!(review[0].existence, Existence::Exact));
        assert_eq!(review[0].cards[0].designation, "ГОСТ 9013-59");
        assert_eq!(review[0].variants[0].card.designation, "ГОСТ Р 9013-59");
        assert!(!one_component_variant(
            &found.mentions[0].key,
            "ГОСТ Р 9014-59"
        ));
    }
    #[test]
    fn missing_named_normative_appendix_is_grounded() {
        let missing = MarkdownIndex::new("Нормативные стандарты указаны в приложении А.").unwrap();
        let found = missing_materials(&missing);
        assert_eq!(found.len(), 1);
        assert_eq!(
            &missing.source[found[0].evidence.range.clone()],
            found[0].text
        );
        let supplied = MarkdownIndex::new(
            "Нормативные стандарты указаны в приложении А.\n\n# Приложение А\n\nСписок стандартов.",
        )
        .unwrap();
        assert!(missing_materials(&supplied).is_empty());
        let unrelated = MarkdownIndex::new("Цвет указан в приложении А.").unwrap();
        assert!(missing_materials(&unrelated).is_empty());
    }
    #[test]
    fn index_distinguishes_editions_titles_and_bounded_variants() {
        let mut catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let mut another = catalog.cards[0].clone();
        another.designation = "ГОСТ 9014-59".into();
        another.full_designation = another.designation.clone();
        another.title = "Иной метод".into();
        catalog.cards.push(another);
        let index = CatalogIndex::new(&catalog);
        let key = scan(&MarkdownIndex::new("ГОСТ 9013-59").unwrap())
            .mentions
            .remove(0)
            .key;
        assert_eq!(index.key_matches(&key).len(), 1);
        assert_eq!(
            index.title_matches("Иной метод")[0].designation,
            "ГОСТ 9014-59"
        );
        assert_eq!(index.variants(&key)[0].designation, "ГОСТ 9014-59");
        let review = catalog.review(
            &scan(&MarkdownIndex::new("ГОСТ 9013").unwrap()),
            "1959-01-01",
        );
        assert!(matches!(review[0].existence, Existence::Undated));
        assert!(matches!(
            review[0].date_status[0],
            DateStatus::BeforeEffective
        ));
        assert!(matches!(
            catalog.review(
                &scan(&MarkdownIndex::new("ГОСТ 9013-59").unwrap()),
                "2020-01-01"
            )[0]
            .date_status[0],
            DateStatus::Unknown
        ));
    }
    #[test]
    fn dated_status_and_source_details_remain_separate_for_each_edition() {
        let mut catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        catalog.retrieved_at = "2020-01-01".into();
        let old = &mut catalog.cards[0];
        old.status_at_retrieval = "Заменён".into();
        old.replacement = Some("ГОСТ 9013-2020".into());
        old.rf_note = Some("Ограничение применения в РФ".into());
        let mut new = old.clone();
        new.designation = "ГОСТ 9013-2020".into();
        new.full_designation = new.designation.clone();
        new.effective_from = Some("2021-01-01".into());
        new.replacement = None;
        new.rf_note = None;
        catalog.cards.push(new);

        let references = scan(&MarkdownIndex::new("ГОСТ 9013").unwrap());
        let review = catalog.review(&references, "2020-01-01");
        let result = &review[0];
        assert!(matches!(result.existence, Existence::Undated));
        assert_eq!(result.cards.len(), 2);
        assert!(matches!(&result.date_status[0], DateStatus::SnapshotStatus(s) if s == "Заменён"));
        assert!(matches!(result.date_status[1], DateStatus::BeforeEffective));
        assert_eq!(
            result.cards[0].replacement.as_deref(),
            Some("ГОСТ 9013-2020")
        );
        assert_eq!(
            result.cards[0].rf_note.as_deref(),
            Some("Ограничение применения в РФ")
        );
        assert!(result.cards[1].rf_note.is_none());
        assert!(matches!(
            catalog.review(&references, "2019-01-01")[0].date_status[0],
            DateStatus::Unknown
        ));
        assert!(matches!(
            catalog.review(&references, "2022-01-01")[0].date_status[0],
            DateStatus::Unknown
        ));
        catalog.cards[0].status_at_retrieval.clear();
        assert!(matches!(
            catalog.review(&references, "2020-01-01")[0].date_status[0],
            DateStatus::Unknown
        ));
    }
    #[test]
    fn addresses_and_incomplete_snapshot() {
        let index = MarkdownIndex::new("При 20 °C по ГОСТ 9013-59.\n\nПри 50 °C по ГОСТ 9013-59; по ГОСТ Р ИСО 4287 и КОСТ 9013.\n\nПо действующим ГОСТ.").unwrap();
        let found = scan(&index);
        assert_eq!(found.mentions.len(), 4);
        assert_eq!(
            &index.source[found.mentions[0].evidence.range.clone()],
            "ГОСТ 9013-59"
        );
        assert_ne!(
            found.mentions[0].evidence.range,
            found.mentions[1].evidence.range
        );
        assert_eq!(found.mentions[2].key.family, "ГОСТ Р ИСО");
        let catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let reviewed = catalog.review(&found, "2026-11-13");
        assert!(matches!(reviewed[0].existence, Existence::Exact));
        assert!(matches!(reviewed[2].existence, Existence::PartialSearch));
        assert_eq!(
            catalog
                .title_matches("Металлы. Метод измерения твердости по Роквеллу")
                .len(),
            1
        );
    }
    #[test]
    fn bounded_candidates_and_identity() {
        let index = MarkdownIndex::new("КОСТ 9013-59; ГОСТ Р 9013-59; ГОСТ 9013-69").unwrap();
        let scan = scan(&index);
        for m in &scan.mentions {
            assert!(one_component_variant(&m.key, "ГОСТ 9013-59"));
        }
        assert!(!one_component_variant(
            &scan.mentions[0].key,
            "ГОСТ 9014-59"
        ));
        let mut catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        catalog.cards[0].full_designation = "ГОСТ 9014-59".into();
        assert!(
            catalog
                .review(&scan, "2026-11-13")
                .iter()
                .all(|r| r.cards.is_empty())
        );
        assert!(
            catalog
                .title_matches("Металлы. Метод измерения твердости по Роквеллу")
                .is_empty()
        );
    }
    #[test]
    fn compound_dates_and_temperature() {
        let text = "Метод Роквелла при 20 °C по ГОСТ 9013-59.\n\nМетод Роквелла при 50 °C по ГОСТ 9013-59.\n\nГОСТ Р ИСО/МЭК 1234-1-2018. ГОСТ 9013.";
        let index = MarkdownIndex::new(text).unwrap();
        let scan = scan(&index);
        assert_eq!(scan.mentions.len(), 4);
        assert_eq!(scan.mentions[2].key.family, "ГОСТ Р ИСО МЭК");
        assert_eq!(scan.mentions[2].key.number, "1234-1");
        assert_eq!(scan.mentions[2].key.year.as_deref(), Some("2018"));
        assert_eq!(scan.mentions[3].original, "ГОСТ 9013");
        let catalog: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let reviewed = catalog.review(&scan, "bad");
        assert!(matches!(reviewed[0].date_status[0], DateStatus::Unknown));
        for (i, value) in ["20 °C", "50 °C"].iter().enumerate() {
            let mention = &scan.mentions[i];
            let method = index
                .resolve(mention.evidence.block_id, "Роквелла")
                .unwrap();
            let temperature = index.resolve(mention.evidence.block_id, value).unwrap();
            let result =
                compare_temperature(&index, mention, &catalog.cards[0], method, temperature);
            assert!(matches!(
                (i, result.outcome),
                (0, Applicability::SupportedWithinComparedConditions)
                    | (1, Applicability::OutsideTemperatureRange)
            ));
        }
        assert_eq!(
            catalog
                .title_matches("МЕТАЛЛЫ. МЕТОД ИЗМЕРЕНИЯ ТВЕРДОСТИ ПО РОКВЕЛЛУ")
                .len(),
            1
        );
    }
}
