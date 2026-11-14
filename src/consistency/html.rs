use std::ops::Range;

#[derive(Debug)]
pub(super) struct HtmlCell {
    pub range: Range<usize>,
    pub context: Vec<usize>,
    pub text_ranges: Vec<Range<usize>>,
    pub rows: Range<usize>,
    pub columns: Range<usize>,
    pub section: Option<usize>,
}

struct Cell {
    range: Range<usize>,
    row: usize,
    col: usize,
    width: usize,
    height: usize,
    header: bool,
    text_ranges: Vec<Range<usize>>,
}

// Parse only structures whose geometry can be established without browser recovery.
pub(super) fn parse_html_table(source: &str, range: Range<usize>) -> Option<Vec<HtmlCell>> {
    let html = source.get(range.clone())?;
    let mut pos = 0;
    let mut stack: Vec<String> = Vec::new();
    let mut cells: Vec<Cell> = Vec::new();
    let mut grid: Vec<Vec<Option<usize>>> = Vec::new();
    let mut current: Option<(usize, usize, usize, usize, usize, bool)> = None;
    let mut rows = 0;
    let mut table_seen = false;
    let mut text_ranges = Vec::new();
    let mut allocated_slots = 0;

    while let Some(relative) = html[pos..].find('<') {
        let start = pos + relative;
        if current.is_none() && !html[pos..start].trim().is_empty() {
            return None;
        }
        if current.is_some() && start > pos {
            text_ranges.push(range.start + pos..range.start + start);
        }
        let mut quote = None;
        let mut end = None;
        for (i, ch) in html[start + 1..].char_indices() {
            match (quote, ch) {
                (Some(q), c) if q == c => quote = None,
                (None, '\'' | '"') => quote = Some(ch),
                (None, '>') => {
                    end = Some(start + 1 + i);
                    break;
                }
                _ => {}
            }
        }
        let end = end?;
        let raw = html[start + 1..end].trim();
        let closing = raw.starts_with('/');
        let raw = raw.strip_prefix('/').unwrap_or(raw).trim();
        let self_closing = raw.ends_with('/');
        let raw = raw.strip_suffix('/').unwrap_or(raw).trim();
        let name_end = raw.find(char::is_whitespace).unwrap_or(raw.len());
        let name = raw[..name_end].to_ascii_lowercase();
        let attrs = &raw[name_end..];
        let void = name == "br";
        if closing {
            if !attrs.trim().is_empty() || self_closing || stack.pop().as_deref() != Some(&name) {
                return None;
            }
            if name == "td" || name == "th" {
                let (content_start, row, col, width, height, header) = current.take()?;
                cells.push(Cell {
                    range: range.start + content_start..range.start + start,
                    row,
                    col,
                    width,
                    height,
                    header,
                    text_ranges: std::mem::take(&mut text_ranges),
                });
            }
        } else {
            let parent = stack.last().map(String::as_str);
            match name.as_str() {
                "table" if parent.is_none() && !table_seen => table_seen = true,
                "thead" | "tbody" | "tfoot" if parent == Some("table") => {}
                "tr" if matches!(parent, Some("table" | "thead" | "tbody" | "tfoot")) => {
                    rows += 1;
                    if rows > 512 {
                        return None;
                    }
                    if grid.len() < rows {
                        grid.push(Vec::new());
                    }
                }
                "td" | "th" if parent == Some("tr") && current.is_none() => {
                    let (rowspan, colspan) = spans(attrs)?;
                    let row = rows.checked_sub(1)?;
                    let col = grid[row]
                        .iter()
                        .position(Option::is_none)
                        .unwrap_or(grid[row].len());
                    // ponytail: cap the grid and cell count; a larger table becomes a coverage gap.
                    if row.checked_add(rowspan)? > 512
                        || col.checked_add(colspan)? > 512
                        || cells.len() >= 1024
                    {
                        return None;
                    }
                    while grid.len() < row + rowspan {
                        grid.push(Vec::new());
                    }
                    for slots in &mut grid[row..row + rowspan] {
                        if slots.len() < col + colspan {
                            allocated_slots += col + colspan - slots.len();
                            if allocated_slots > 65_536 {
                                return None;
                            }
                            slots.resize(col + colspan, None);
                        }
                        if slots[col..col + colspan].iter().any(Option::is_some) {
                            return None;
                        }
                        slots[col..col + colspan].fill(Some(cells.len()));
                    }
                    current = Some((end + 1, row, col, colspan, rowspan, name == "th"));
                }
                "br" | "b" | "strong" | "i" | "em" | "span" | "p" | "sup" | "sub" | "a"
                    if current.is_some() => {}
                _ => return None,
            }
            if !void && !self_closing {
                stack.push(name);
            } else if !void {
                return None;
            }
        }
        pos = end + 1;
    }
    if !html[pos..].trim().is_empty()
        || !stack.is_empty()
        || !table_seen
        || current.is_some()
        || rows == 0
        || grid.len() != rows
    {
        return None;
    }
    let width = grid.first()?.len();
    if width == 0
        || grid
            .iter()
            .any(|row| row.len() != width || row.iter().any(Option::is_none))
    {
        return None;
    }

    let notes: Vec<bool> = cells
        .iter()
        .map(|cell| {
            cell.text_ranges.first().is_some_and(|r| {
                source[r.clone()]
                    .trim_start()
                    .to_lowercase()
                    .starts_with("примечан")
            })
        })
        .collect();
    let mut seen_header = false;
    let sections: Vec<bool> = cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            let explicit_section = cell.text_ranges.first().is_some_and(|range| {
                source[range.clone()]
                    .trim_start()
                    .to_lowercase()
                    .strip_prefix("раздел")
                    .is_some_and(|rest| {
                        rest.starts_with(char::is_whitespace)
                            && rest.trim_start().starts_with(|c: char| c.is_ascii_digit())
                    })
            }) && grid[cell.row].iter().flatten().all(|&j| {
                j == i
                    || cells[j]
                        .text_ranges
                        .iter()
                        .all(|r| whitespace_html(&source[r.clone()]))
            });
            let section = width > 1
                && (cell.width == width || explicit_section)
                && cell.row + 1 < rows
                && !notes[i]
                && (!cell.header || seen_header);
            seen_header |= cell.header && !section;
            section
        })
        .collect();
    let mut result = Vec::with_capacity(cells.len());
    let mut section = None;
    for (i, cell) in cells.iter().enumerate() {
        let mut context = Vec::new();
        for (j, other) in cells.iter().enumerate() {
            let column_header = other.header
                && !sections[j]
                && other.row < cell.row
                && other.col < cell.col + cell.width
                && cell.col < other.col + other.width;
            let same_row = grid[cell.row].contains(&Some(j)) && !sections[j] && !notes[j];
            if j != i && (column_header || same_row || section == Some(j) || notes[j]) {
                context.push(j);
            }
        }
        result.push(HtmlCell {
            range: cell.range.clone(),
            context,
            text_ranges: cell.text_ranges.clone(),
            rows: cell.row..cell.row + cell.height,
            columns: cell.col..cell.col + cell.width,
            section,
        });
        if sections[i] {
            section = Some(i);
        }
    }
    Some(result)
}

pub(super) fn whitespace_html(mut text: &str) -> bool {
    while !text.is_empty() {
        text = text.trim_start();
        if text.is_empty() {
            return true;
        }
        let Some((entity, rest)) = text.strip_prefix('&').and_then(|s| s.split_once(';')) else {
            return false;
        };
        let whitespace = match entity {
            "nbsp" | "Tab" | "NewLine" | "ensp" | "emsp" | "thinsp" => true,
            _ => entity.strip_prefix('#').is_some_and(|number| {
                let value = if let Some(hex) = number.strip_prefix(['x', 'X']) {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    number.parse().ok()
                };
                value
                    .and_then(char::from_u32)
                    .is_some_and(char::is_whitespace)
            }),
        };
        if !whitespace {
            return false;
        }
        text = rest;
    }
    true
}

fn spans(mut attrs: &str) -> Option<(usize, usize)> {
    let mut row = None;
    let mut col = None;
    while !attrs.trim_start().is_empty() {
        attrs = attrs.trim_start();
        let end = attrs
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(attrs.len());
        if end == 0 {
            return None;
        }
        let name = attrs[..end].to_ascii_lowercase();
        attrs = attrs[end..].trim_start();
        let mut value = None;
        if let Some(rest) = attrs.strip_prefix('=') {
            attrs = rest.trim_start();
            if let Some(q) = attrs.chars().next().filter(|c| *c == '\'' || *c == '"') {
                attrs = &attrs[1..];
                let end = attrs.find(q)?;
                value = Some(&attrs[..end]);
                attrs = &attrs[end + 1..];
            } else {
                let end = attrs.find(char::is_whitespace).unwrap_or(attrs.len());
                if end == 0 {
                    return None;
                }
                value = Some(&attrs[..end]);
                attrs = &attrs[end..];
            }
        }
        let target = match name.as_str() {
            "rowspan" => Some(&mut row),
            "colspan" => Some(&mut col),
            _ => None,
        };
        if let Some(target) = target {
            let n = value?.parse::<usize>().ok()?;
            if n == 0 || target.replace(n).is_some() {
                return None;
            }
        }
    }
    Some((row.unwrap_or(1), col.unwrap_or(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_and_context_keep_original_bytes() {
        let text = "до<table><tr><th rowspan='2'>Позиция</th><th colspan='2'>Груз<br>кг</th></tr><tr><th>min</th><th>max</th></tr><tr><td colspan='3'>Монтаж</td></tr><tr><td>Лифт</td><td>300</td><td>500</td></tr><tr><td colspan='3'>Примечание: кроме старого</td></tr></table>после";
        let start = text.find("<table>").unwrap();
        let end = text.find("</table>").unwrap() + 8;
        let cells = parse_html_table(text, start..end).unwrap();
        let values: Vec<_> = cells.iter().map(|c| &text[c.range.clone()]).collect();
        let i = values.iter().position(|v| *v == "300").unwrap();
        let context: Vec<_> = cells[i].context.iter().map(|j| values[*j]).collect();
        assert!(context.contains(&"Груз<br>кг"));
        assert!(context.contains(&"min"));
        assert!(context.contains(&"Монтаж"));
        assert!(context.contains(&"Лифт"));
        assert!(context.contains(&"Примечание: кроме старого"));
        let malformed = "<table><tr><td rowspan='2'>x</td></tr></table>";
        assert!(parse_html_table(malformed, 0..malformed.len()).is_none());
        let header = values.iter().position(|v| *v == "Груз<br>кг").unwrap();
        assert_eq!(
            cells[header]
                .text_ranges
                .iter()
                .map(|r| &text[r.clone()])
                .collect::<Vec<_>>(),
            ["Груз", "кг"]
        );
        let nested = "<table><tr><td><table></table></td></tr></table>";
        assert!(parse_html_table(nested, 0..nested.len()).is_none());
    }

    #[test]
    fn rowspan_sections_and_limits() {
        let html = "<table><tr><th>Название</th><th>Значение</th></tr><tr><th colspan='2'>Демонтаж</th></tr><tr><td rowspan='2'>Лифт</td><td>300</td></tr><tr><td>500</td></tr></table>";
        let cells = parse_html_table(html, 0..html.len()).unwrap();
        let values: Vec<_> = cells.iter().map(|c| &html[c.range.clone()]).collect();
        let value = values.iter().position(|v| *v == "500").unwrap();
        let context: Vec<_> = cells[value].context.iter().map(|j| values[*j]).collect();
        assert!(context.contains(&"Лифт"));
        assert!(context.contains(&"Демонтаж"));
        let single = "<table><tr><td>Первое</td></tr><tr><td>Второе</td></tr></table>";
        let cells = parse_html_table(single, 0..single.len()).unwrap();
        assert!(cells[1].context.is_empty());
        let huge = "<table><tr><td rowspan='512' colspan='512'>x</td></tr></table>";
        assert!(parse_html_table(huge, 0..huge.len()).is_none());
    }

    #[test]
    fn explicit_section_rows_replace_context() {
        let html = "<table><tr><td>№</td><td>Раздел</td><td>Объем</td></tr><tr><td>&#9;</td><td>Раздел 1. Демонтажные работы.</td><td>&#x9;</td></tr><tr><td>1</td><td>Старый лифт</td><td>1</td></tr><tr><td></td><td>Раздел 2. Монтажные работы.</td><td>&nbsp;</td></tr><tr><td>2</td><td>Новый лифт</td><td>1</td></tr><tr><td></td><td>123</td><td></td></tr><tr><td>3</td><td>Дверь</td><td>1</td></tr></table>";
        let cells = parse_html_table(html, 0..html.len()).unwrap();
        let values: Vec<_> = cells.iter().map(|c| &html[c.range.clone()]).collect();
        for (target, expected, absent) in [
            (
                "Старый лифт",
                "Раздел 1. Демонтажные работы.",
                "Раздел 2. Монтажные работы.",
            ),
            (
                "Новый лифт",
                "Раздел 2. Монтажные работы.",
                "Раздел 1. Демонтажные работы.",
            ),
            ("Дверь", "Раздел 2. Монтажные работы.", "123"),
        ] {
            let cell = &cells[values.iter().position(|&v| v == target).unwrap()];
            let context: Vec<_> = cell.context.iter().map(|&i| values[i]).collect();
            assert!(context.contains(&expected));
            assert!(!context.contains(&absent));
            assert!(!context.contains(&"Раздел"));
        }
    }
}
