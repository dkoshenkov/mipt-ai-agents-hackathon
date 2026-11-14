use std::ops::Range;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::html::parse_html_table;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKind {
    Heading,
    Paragraph,
    ListItem,
    TableCell,
    HtmlTableCell,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub id: usize,
    pub range: Range<usize>,
    pub kind: BlockKind,
    pub headings: Vec<usize>,
    pub table_context: Vec<usize>,
    pub text_ranges: Vec<Range<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<TablePosition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TablePosition {
    pub id: usize,
    pub rows: Range<usize>,
    pub columns: Range<usize>,
    pub title: Option<usize>,
    pub section: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub version: String,
    pub block_id: usize,
    pub range: Range<usize>,
}

#[derive(Clone, Debug)]
pub struct MarkdownIndex {
    pub source: String,
    pub version: String,
    pub blocks: Vec<Block>,
    pub gaps: Vec<Range<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmptyDocument;

impl MarkdownIndex {
    pub fn new(source: &str) -> Result<Self, EmptyDocument> {
        if source.trim().is_empty() {
            return Err(EmptyDocument);
        }
        let mut index = Self {
            source: source.to_owned(),
            version: format!("{:x}", Sha256::digest(source.as_bytes())),
            blocks: Vec::new(),
            gaps: Vec::new(),
        };
        let mut active_blocks = Vec::<usize>::new();
        let mut headings: Vec<(u8, usize)> = Vec::new();
        let mut item_depth = 0;
        let mut table = None::<PipeTable>;
        let mut html_end = 0;
        for (event, range) in Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter() {
            if range.start < html_end {
                continue;
            }
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    let level = heading_level(level);
                    while headings.last().is_some_and(|(old, _)| *old >= level) {
                        headings.pop();
                    }
                    let id = index.push(range, BlockKind::Heading, &headings);
                    headings.push((level, id));
                    active_blocks.push(id);
                }
                Event::Start(Tag::Item) => {
                    active_blocks.push(index.push(range, BlockKind::ListItem, &headings));
                    item_depth += 1;
                }
                Event::End(TagEnd::Item) => {
                    item_depth -= 1;
                    active_blocks.pop();
                }
                Event::End(TagEnd::Heading(_) | TagEnd::TableCell) => {
                    active_blocks.pop();
                }
                Event::End(TagEnd::Paragraph) if item_depth == 0 => {
                    active_blocks.pop();
                }
                Event::Start(Tag::Paragraph) if item_depth == 0 => {
                    active_blocks.push(index.push(range, BlockKind::Paragraph, &headings));
                }
                Event::Start(Tag::Table(_)) => {
                    table = Some(PipeTable {
                        title: index.table_title(range.start),
                        ..PipeTable::default()
                    })
                }
                Event::Start(Tag::TableHead) => {
                    if let Some(table) = &mut table {
                        table.header = true;
                        table.rows.push(Vec::new());
                    }
                }
                Event::End(TagEnd::TableHead) => {
                    if let Some(table) = &mut table {
                        table.header = false;
                    }
                }
                Event::Start(Tag::TableRow) => {
                    if let Some(table) = &mut table {
                        table.rows.push(Vec::new());
                    }
                }
                Event::Start(Tag::TableCell) => {
                    let id = index.push(range, BlockKind::TableCell, &headings);
                    active_blocks.push(id);
                    if let Some(table) = &mut table {
                        if table.header {
                            table.headers.push(id);
                        }
                        if let Some(row) = table.rows.last_mut() {
                            row.push(id);
                        }
                    }
                }
                Event::End(TagEnd::Table) => {
                    if let Some(table) = table.take() {
                        table.apply(&mut index);
                    }
                }
                Event::Start(Tag::HtmlBlock)
                    if source[range.clone()]
                        .to_ascii_lowercase()
                        .contains("<table") =>
                {
                    let tail = source[range.start..].to_ascii_lowercase();
                    let end = tail.find("</table>").map_or(source.len(), |offset| {
                        range.start + offset + "</table>".len()
                    });
                    let range = range.start..end;
                    html_end = end;
                    match parse_html_table(source, range.clone()) {
                        Some(cells) => {
                            let base = index.blocks.len();
                            let title = index.table_title(range.start);
                            for cell in cells {
                                let id =
                                    index.push(cell.range, BlockKind::HtmlTableCell, &headings);
                                index.blocks[id].text_ranges = cell.text_ranges;
                                index.blocks[id].table = Some(TablePosition {
                                    id: base,
                                    rows: cell.rows,
                                    columns: cell.columns,
                                    title,
                                    section: cell.section.map(|i| base + i),
                                });
                                index.blocks[id].table_context = cell
                                    .context
                                    .into_iter()
                                    .map(|i| base + i)
                                    .chain(title)
                                    .collect();
                            }
                        }
                        None => index.gaps.push(range),
                    }
                }
                Event::Start(Tag::CodeBlock(_)) => {
                    html_end = range.end;
                    index.gaps.push(range);
                }
                Event::Text(_) | Event::Code(_) => {
                    if let Some(&id) = active_blocks.last() {
                        index.blocks[id].text_ranges.push(range);
                    }
                }
                Event::Start(Tag::HtmlBlock) => index.gaps.push(range),
                _ => {}
            }
        }
        Ok(index)
    }

    fn push(&mut self, range: Range<usize>, kind: BlockKind, headings: &[(u8, usize)]) -> usize {
        let id = self.blocks.len();
        self.blocks.push(Block {
            id,
            range,
            kind,
            headings: headings.iter().map(|(_, id)| *id).collect(),
            table_context: Vec::new(),
            text_ranges: Vec::new(),
            table: None,
        });
        id
    }

    fn table_title(&self, start: usize) -> Option<usize> {
        let block = self.blocks.last()?;
        if !matches!(block.kind, BlockKind::Paragraph | BlockKind::ListItem)
            || block.range.end > start
            || !self.source[block.range.end..start].trim().is_empty()
        {
            return None;
        }
        // Formatting delimiters and HTML tags are outside the parser's visible text ranges.
        let visible = |b: &Block| {
            b.text_ranges
                .iter()
                .map(|range| &self.source[range.clone()])
                .collect::<String>()
        };
        let text = visible(block);
        let text = text.trim();
        let defect_title = block.kind == BlockKind::Paragraph
            && text.to_lowercase().starts_with("на выполнение работ по ")
            && self
                .blocks
                .get(block.id.wrapping_sub(1))
                .is_some_and(|heading| {
                    matches!(heading.kind, BlockKind::Heading | BlockKind::Paragraph)
                        && self.source[heading.range.end..block.range.start]
                            .trim()
                            .is_empty()
                        && visible(heading).trim().to_lowercase() == "дефектная ведомость"
                });
        (text.ends_with(':') || defect_title).then_some(block.id)
    }

    /// Cells sharing at least one physical row, including the requested cell.
    pub fn same_row_ids(&self, id: usize) -> Vec<usize> {
        let Some(table) = self.blocks.get(id).and_then(|b| b.table.as_ref()) else {
            return Vec::new();
        };
        self.blocks
            .iter()
            .skip(table.id)
            .take_while(|b| b.table.as_ref().is_some_and(|other| other.id == table.id))
            .filter_map(|b| {
                let other = b.table.as_ref()?;
                (other.rows.start < table.rows.end && table.rows.start < other.rows.end)
                    .then_some(b.id)
            })
            .collect()
    }

    pub(super) fn table_column_ids(&self, id: usize) -> Vec<usize> {
        let Some(table) = self.blocks.get(id).and_then(|b| b.table.as_ref()) else {
            return Vec::new();
        };
        self.blocks
            .iter()
            .skip(table.id)
            .take_while(|b| {
                b.table
                    .as_ref()
                    .is_some_and(|other| other.id == table.id && other.rows.start == 0)
            })
            .filter_map(|b| {
                let other = b.table.as_ref()?;
                (other.columns.start < table.columns.end && table.columns.start < other.columns.end)
                    .then_some(b.id)
            })
            .collect()
    }

    pub(super) fn has_readable_text(&self, id: usize) -> bool {
        let block = &self.blocks[id];
        block.text_ranges.iter().any(|range| {
            let text = &self.source[range.clone()];
            match block.kind {
                BlockKind::HtmlTableCell => !super::html::whitespace_html(text),
                _ => !text.trim().is_empty(),
            }
        })
    }

    pub fn resolve(&self, block_id: usize, quote: &str) -> Option<Evidence> {
        if quote.is_empty() {
            return None;
        }
        let block = self.blocks.get(block_id)?;
        let mut found = None;
        for range in &block.text_ranges {
            let text = self.source.get(range.clone())?;
            let mut offset = 0;
            while let Some(relative) = text[offset..].find(quote) {
                let start = offset + relative;
                if found.is_some() {
                    return None;
                }
                found = Some((range.start + start)..(range.start + start + quote.len()));
                offset = start + text[start..].chars().next()?.len_utf8();
            }
        }
        Some(Evidence {
            version: self.version.clone(),
            block_id,
            range: found?,
        })
    }

    pub fn resolve_scoped(
        &self,
        block_id: usize,
        quote: &str,
        statement: &str,
        evidence: &Evidence,
    ) -> Option<Evidence> {
        self.resolve(block_id, quote).or_else(|| {
            if block_id != evidence.block_id
                || self.resolve(block_id, statement).as_ref() != Some(evidence)
                || quote.is_empty()
            {
                return None;
            }
            let offset = statement.find(quote)?;
            let next = offset + statement[offset..].chars().next()?.len_utf8();
            if statement[next..].contains(quote) {
                return None;
            }
            let start = evidence.range.start + offset;
            Some(Evidence {
                version: self.version.clone(),
                block_id,
                range: start..start + quote.len(),
            })
        })
    }
}

#[derive(Default)]
struct PipeTable {
    header: bool,
    headers: Vec<usize>,
    rows: Vec<Vec<usize>>,
    title: Option<usize>,
}

impl PipeTable {
    fn apply(self, index: &mut MarkdownIndex) {
        let mut section = None;
        let notes: Vec<usize> = self
            .rows
            .iter()
            .flatten()
            .copied()
            .filter(|&id| {
                index.source[index.blocks[id].range.clone()]
                    .trim()
                    .to_lowercase()
                    .starts_with("примеч")
            })
            .collect();
        let table_id = self.rows.iter().flatten().next().copied();
        for (row_number, row) in self.rows.into_iter().enumerate() {
            let nonempty: Vec<_> = row
                .iter()
                .copied()
                .filter(|&id| {
                    !index.source[index.blocks[id].range.clone()]
                        .trim()
                        .is_empty()
                })
                .collect();
            if row != self.headers
                && self.headers.len() > 1
                && nonempty.len() == 1
                && !notes.contains(&nonempty[0])
            {
                section = Some(nonempty[0]);
            }
            for (column, id) in row.iter().enumerate() {
                index.blocks[*id].table = Some(TablePosition {
                    id: table_id.unwrap(),
                    rows: row_number..row_number + 1,
                    columns: column..column + 1,
                    title: self.title,
                    section,
                });
                let mut context: Vec<_> = self.headers.get(column).copied().into_iter().collect();
                context.extend(self.title);
                if let Some(section) = section {
                    context.push(section);
                }
                context.extend(row.iter().copied());
                context.extend(notes.iter().copied());
                context.retain(|other| other != id);
                context.sort_unstable();
                context.dedup();
                index.blocks[*id].table_context = context;
            }
        }
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ranges_and_unique_quotes() {
        assert!(MarkdownIndex::new("").is_err());
        assert_ne!(
            MarkdownIndex::new("а").unwrap().version,
            MarkdownIndex::new("а ").unwrap().version
        );
        let text = "Введение\n\n# Раздел\n\n## Монтаж\n\n- 500 кг и 500 кг\n\n| Имя | Масса |\n| --- | --- |\n| A \\| B | 500 кг |\n| C | 500 кг |\n";
        let index = MarkdownIndex::new(text).unwrap();
        let list = index
            .blocks
            .iter()
            .find(|b| b.kind == BlockKind::ListItem)
            .unwrap();
        assert_eq!(list.headings.len(), 2);
        assert!(index.resolve(list.id, "500 кг").is_none());
        let cells: Vec<_> = index
            .blocks
            .iter()
            .filter(|b| b.kind == BlockKind::TableCell)
            .collect();
        assert_eq!(cells.len(), 6);
        assert_eq!(text[cells[2].range.clone()].trim(), "A \\| B");
        let address = index.resolve(cells[3].id, "500 кг").unwrap();
        assert_eq!(&text[address.range], "500 кг");
        assert!(index.resolve(cells[4].id, "500 кг").is_none());
        assert!(index.resolve(cells[5].id, "500 кг").is_some());
        assert!(cells[3].table_context.contains(&cells[1].id));
    }
    #[test]
    fn exact_visible_text_and_coverage() {
        assert!(MarkdownIndex::new(" \n").is_err());
        let text = "# Раздел\n\n[метка](https://example.org/secret) ааа\n\n```\ncode\n```\n\n<table>\n<tr><th>Масса</th></tr>\n\n<tr><td title=\"secret\">500 кг</td></tr>\n</table>\n";
        let index = MarkdownIndex::new(text).unwrap();
        let paragraph = index
            .blocks
            .iter()
            .find(|b| b.kind == BlockKind::Paragraph)
            .unwrap();
        assert!(index.resolve(paragraph.id, "secret").is_none());
        assert!(index.resolve(paragraph.id, "аа").is_none());
        let cell = index
            .blocks
            .iter()
            .find(|b| {
                b.kind == BlockKind::HtmlTableCell && text[b.range.clone()].contains("500 кг")
            })
            .unwrap();
        assert!(index.resolve(cell.id, "secret").is_none());
        assert!(index.resolve(cell.id, "500 кг").is_some());
        assert!(
            index
                .gaps
                .iter()
                .any(|range| text[range.clone()].contains("code"))
        );
    }
    #[test]
    fn table_section_units_and_notes_remain_addressable() {
        let text = "| Этап | Значение | Ед. |\n| --- | --- | --- |\n| Монтаж | | |\n| Лифт | 500 | кг |\n| Демонтаж | | |\n| Старый лифт | 500 | кг |\n| Примечание: условие | | |\n";
        let index = MarkdownIndex::new(text).unwrap();
        let find = |quote: &str| {
            index
                .blocks
                .iter()
                .find(|b| index.resolve(b.id, quote).is_some())
                .unwrap()
        };
        let section = find("Демонтаж").id;
        let note = find("Примечание: условие").id;
        let values: Vec<_> = index
            .blocks
            .iter()
            .filter(|b| index.resolve(b.id, "500").is_some())
            .collect();
        assert_eq!(values.len(), 2);
        assert!(!values[0].table_context.contains(&section));
        assert!(values[1].table_context.contains(&section));
        assert!(values[1].table_context.contains(&note));
        assert!(
            values[1]
                .table_context
                .iter()
                .any(|id| index.resolve(*id, "кг").is_some())
        );
    }
}
