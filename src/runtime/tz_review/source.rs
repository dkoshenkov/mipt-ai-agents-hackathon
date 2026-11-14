use super::Anchor;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub quote: String,
    pub heading: String,
    pub table_header: String,
}

pub(super) struct Source<'a> {
    pub text: &'a str,
    pub numbered: String,
    pub revision: String,
    offsets: Vec<usize>,
}

impl<'a> Source<'a> {
    pub fn new(text: &'a str) -> Self {
        use std::fmt::Write;
        let mut numbered = String::new();
        let mut offsets = Vec::new();
        let mut offset = 0;
        for (i, row) in text.split_inclusive('\n').enumerate() {
            offsets.push(offset);
            writeln!(
                numbered,
                "L{} {}",
                i + 1,
                row.trim_end_matches(['\r', '\n'])
            )
            .unwrap();
            offset += row.len();
        }
        Self {
            text,
            numbered,
            offsets,
            revision: format!("{:x}", Sha256::digest(text)),
        }
    }

    fn row(&self, index: usize) -> &str {
        let end = self
            .offsets
            .get(index + 1)
            .copied()
            .unwrap_or(self.text.len());
        self.text[self.offsets[index]..end].trim_end_matches(['\r', '\n'])
    }

    /// Explicit model and manufacturer fields are sufficient to schedule A06.
    /// The reviewer still decides whether there is a legal issue.
    pub fn brand_lines(&self) -> Vec<usize> {
        self.text
            .lines()
            .enumerate()
            .filter_map(|(i, row)| {
                let lower = row.to_lowercase();
                let populated =
                    ["производитель:", "референс:", "изготовитель:"]
                        .iter()
                        .any(|key| {
                            lower.split_once(key).is_some_and(|(_, value)| {
                                !value.trim_matches([' ', '|']).is_empty()
                            })
                        })
                        || lower
                            .split('|')
                            .map(str::trim)
                            .collect::<Vec<_>>()
                            .windows(2)
                            .any(|cells| cells[0] == "модель" && !cells[1].is_empty());
                populated.then_some(i + 1)
            })
            .collect()
    }

    /// Active ATX sections, innermost first. A sibling closes a section.
    fn headings(&self, number: usize) -> impl Iterator<Item = (usize, &str)> {
        let mut depth = 7;
        (0..number.min(self.offsets.len()))
            .rev()
            .filter_map(move |i| {
                let row = self.row(i).trim_start();
                let title = row.trim_start_matches('#');
                let level = row.len() - title.len();
                if level > 0 && level < depth && title.starts_with(char::is_whitespace) {
                    depth = level;
                    Some((i, title.trim()))
                } else {
                    None
                }
            })
    }

    pub fn same_section_or_parent(&self, parent: usize, child: usize) -> bool {
        let Some((index, _)) = self.headings(parent).next() else {
            return true;
        };
        self.headings(child).any(|(i, _)| i == index)
    }

    pub fn lot(&self, number: usize) -> Option<usize> {
        self.headings(number).find_map(|(line, title)| {
            let title = title.to_lowercase();
            (title.starts_with("лот ") || title.starts_with("лот №")).then_some(line)
        })
    }

    pub fn historical_at(&self, number: usize) -> bool {
        self.headings(number)
            .any(|(_, title)| super::premise::historical(title))
    }

    pub fn item_number(&self, number: usize) -> Option<u32> {
        let loc = self.line(number)?;
        if loc.quote.trim_start().starts_with('|') {
            fn cells(row: &str) -> impl Iterator<Item = &str> {
                row.trim().trim_matches('|').split('|').map(str::trim)
            }
            if let Some(id) = cells(&loc.quote)
                .next()
                .and_then(super::premise::position_number)
            {
                return Some(id);
            }
            for (column, value) in cells(&loc.table_header).zip(cells(&loc.quote)) {
                if matches!(
                    column.to_lowercase().as_str(),
                    "позиция" | "№ позиции" | "номер позиции" | "номер позиции товара"
                ) && let Ok(n) = value.trim_end_matches('.').parse()
                {
                    return Some(n);
                }
            }
        }
        self.headings(number)
            .find_map(|(_, title)| super::premise::position_number(title))
    }

    /// One-based Markdown address, including existing blank lines.
    pub fn line_text(&self, number: usize) -> Option<&str> {
        (1..=self.offsets.len())
            .contains(&number)
            .then(|| self.row(number - 1))
    }

    pub fn line(&self, number: usize) -> Option<Location> {
        let row = self.line_text(number)?;
        if row.trim().is_empty() {
            return None;
        }
        let i = number - 1;
        Some(self.location(i, self.offsets[i], row))
    }

    pub fn exact(&self, a: &Anchor) -> Option<Location> {
        let location = self.locate(a)?;
        (location.line == a.line).then_some(location)
    }

    pub fn locate(&self, a: &Anchor) -> Option<Location> {
        if a.quote.trim().is_empty() {
            return None;
        }
        let mut index = a.line.checked_sub(1)?;
        // A mistyped line number is recoverable only for a unique literal quote.
        let valid = index < self.offsets.len() && self.row(index).contains(&a.quote);
        if !valid {
            let mut occurrences = self.text.match_indices(&a.quote);
            let (at, _) = occurrences.next()?;
            if occurrences.next().is_some() {
                return None;
            }
            index = self.offsets.partition_point(|offset| *offset <= at) - 1;
        }
        let mut hits = self.row(index).match_indices(&a.quote);
        let start = self.offsets[index] + hits.next()?.0;
        if hits.next().is_some() {
            return None;
        }
        Some(self.location(index, start, &a.quote))
    }

    fn location(&self, index: usize, start: usize, quote: &str) -> Location {
        let heading = (0..=index)
            .rev()
            .map(|i| self.row(i))
            .find(|row| row.trim_start().starts_with('#'))
            .unwrap_or("");
        let mut first = index;
        while first > 0 && self.row(first - 1).trim_start().starts_with('|') {
            first -= 1;
        }
        let header = if self.row(index).trim_start().starts_with('|') {
            self.row(first).into()
        } else {
            String::new()
        };
        Location {
            line: index + 1,
            start,
            end: start + quote.len(),
            quote: quote.into(),
            heading: heading.to_owned(),
            table_header: header,
        }
    }
}
