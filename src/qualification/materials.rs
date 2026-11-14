use super::types::*;
use sha2::{Digest, Sha256};

pub fn version(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn manifest(input: &QualificationInput) -> Vec<MaterialVersion> {
    std::iter::once(&input.document)
        .chain(&input.attachments)
        .map(|m| MaterialVersion {
            id: m.id.clone(),
            version: version(&m.bytes),
            visibility: m.visibility,
            format: m.format,
        })
        .collect()
}

pub(crate) fn validate_input(
    input: &QualificationInput,
    options: &QualificationOptions,
) -> Result<(), QualificationInputError> {
    if input.document.bytes.iter().all(u8::is_ascii_whitespace)
        || (input.document.format == MaterialFormat::Markdown
            && std::str::from_utf8(&input.document.bytes).is_ok_and(|s| s.trim().is_empty()))
    {
        return Err(QualificationInputError::EmptyDocument);
    }
    let mut ids = std::collections::HashSet::new();
    if std::iter::once(&input.document)
        .chain(&input.attachments)
        .any(|m| m.id.trim().is_empty() || !ids.insert(&m.id))
    {
        return Err(QualificationInputError::InvalidMaterialIds);
    }
    if options.max_source_bytes == 0
        || options.max_context_bytes == 0
        || options.extraction_batch_bytes == 0
        || options.request_timeout_ms == 0
        || options.max_search_turns == 0
    {
        return Err(QualificationInputError::InvalidOptions);
    }
    Ok(())
}

pub(crate) async fn prepare(
    input: &QualificationInput,
    options: &QualificationOptions,
) -> QualificationReport {
    let mut report = QualificationReport {
        document_id: input.document.id.clone(),
        materials: manifest(input),
        blocks: vec![],
        requirements: vec![],
        coverage: Coverage {
            blocks: vec![],
            gaps: vec![],
            review: ReviewStatus::NotRun,
        },
        notes: vec![],
        closed_questions: vec![],
        clarifications: vec![],
        external_sources: vec![],
        searches: vec![],
        model_attempts: 0,
        http_requests: 0,
        next_id: 0,
    };
    for material in std::iter::once(&input.document).chain(&input.attachments) {
        match decode(material, options).await {
            Ok((text, uncertain)) => {
                let blocks = index_text(&material.id, &version(&material.bytes), &text);
                if uncertain || blocks.is_empty() {
                    report.coverage.gaps.push(Gap {
                        block_ids: blocks.iter().map(|b| b.id.clone()).collect(),
                        material_id: Some(material.id.clone()),
                        failure: Failure::new("index", FailureReason::Unreadable),
                    });
                }
                report
                    .coverage
                    .blocks
                    .extend(blocks.iter().map(|b| BlockCoverage {
                        block_id: b.id.clone(),
                        extracted: false,
                        reviewed: false,
                    }));
                report.blocks.extend(blocks);
            }
            Err(reason) => report.coverage.gaps.push(Gap {
                block_ids: vec![],
                material_id: Some(material.id.clone()),
                failure: Failure::new("decode", reason),
            }),
        }
    }
    report
}

pub(crate) async fn decode(
    material: &Material,
    options: &QualificationOptions,
) -> Result<(String, bool), FailureReason> {
    if material.bytes.len() > options.max_source_bytes {
        return Err(FailureReason::TooLarge);
    }
    match material.format {
        MaterialFormat::Markdown => String::from_utf8(material.bytes.clone())
            .map(|s| {
                let lower = s.to_ascii_lowercase();
                let uncertain = lower.contains("<table")
                    && (lower.contains("rowspan")
                        || lower.contains("colspan")
                        || (lower.matches("<table").count() != lower.matches("</table>").count()
                            || lower.matches("<tr").count() != lower.matches("</tr>").count()));
                (s, uncertain)
            })
            .map_err(|_| FailureReason::Unreadable),
        MaterialFormat::Html => html_text(
            &String::from_utf8(material.bytes.clone()).map_err(|_| FailureReason::Unreadable)?,
        ),
        MaterialFormat::Pdf => super::sources::pdf_text(&material.bytes, options)
            .await
            .map(|s| (s, false)),
    }
}

/// Immutable line blocks preserve original Markdown offsets and page boundaries in PDF views.
pub fn index_text(id: &str, raw_version: &str, text: &str) -> Vec<Block> {
    let mut blocks = vec![];
    let mut page = 1;
    let mut headings: Vec<String> = vec![];
    let mut headers: Vec<String> = vec![];
    let text_version = version(text.as_bytes());
    let mut html_table = false;
    for (segment_start, segment_end) in source_segments(text) {
        let offset = segment_start;
        let line = &text[segment_start..segment_end];
        let lower = line.to_ascii_lowercase();
        if lower.contains("<table") {
            html_table = true;
            headers.clear();
        }
        let mut local = 0;
        for part in line.split_inclusive('\u{c}') {
            let visible = part.trim_end_matches(['\n', '\r', '\u{c}']);
            if !visible.trim().is_empty() {
                let leading = visible.len() - visible.trim_start().len();
                let start = offset + local + leading;
                let value = visible.trim();
                let level = value.chars().take_while(|c| *c == '#').count();
                if level > 0 && level <= 6 && value.as_bytes().get(level) == Some(&b' ') {
                    headings.truncate(level - 1);
                    headings.push(value[level..].trim().to_owned());
                }
                if cells(value).len() > 1 {
                    if headers.is_empty() {
                        headers.push(value.to_owned());
                    }
                } else if !html_table {
                    headers.clear();
                }
                blocks.push(Block {
                    id: format!("{id}:{}", blocks.len()),
                    material_id: id.into(),
                    version: raw_version.into(),
                    text_version: text_version.clone(),
                    start,
                    end: start + value.len(),
                    text: value.into(),
                    page: text.contains('\u{c}').then_some(page),
                    headings: headings.clone(),
                    table_headers: headers.clone(),
                });
            }
            if part.ends_with('\u{c}') {
                page += 1;
                headers.clear();
            }
            local += part.len();
        }
        if line.trim().is_empty() && !html_table {
            headers.clear();
        }
        if lower.contains("</table>") {
            html_table = false;
            headers.clear();
        }
    }
    blocks
}

// HTML rows may span lines or share a line; their spans still address original Markdown.
fn source_segments(text: &str) -> Vec<(usize, usize)> {
    let lower = text.to_ascii_lowercase();
    let mut segments = vec![];
    let mut cursor = 0;
    while cursor < text.len() {
        if lower[cursor..].starts_with("<tr")
            && lower
                .as_bytes()
                .get(cursor + 3)
                .is_some_and(|c| *c == b'>' || c.is_ascii_whitespace())
            && let Some(end) = lower[cursor..].find("</tr>")
        {
            let end = cursor + end + 5;
            segments.push((cursor, end));
            cursor = end;
            continue;
        }
        let newline = text[cursor..]
            .find('\n')
            .map_or(text.len(), |n| cursor + n + 1);
        let next_row = lower[cursor..]
            .find("<tr")
            .filter(|n| *n > 0)
            .map_or(text.len(), |n| cursor + n);
        let end = newline.min(next_row).max(cursor + 1);
        segments.push((cursor, end));
        cursor = end;
    }
    segments
}

pub fn resolve(blocks: &[Block], quote: &Quote) -> Result<SourceRef, FailureReason> {
    let block = blocks
        .iter()
        .find(|b| b.id == quote.block_id)
        .ok_or(FailureReason::InvalidResponse)?;
    if quote.quote.trim().is_empty() {
        return Err(FailureReason::InvalidResponse);
    }
    let mut found = block.text.match_indices(&quote.quote);
    let (offset, _) = found.next().ok_or(FailureReason::InvalidResponse)?;
    if found.next().is_some() {
        return Err(FailureReason::InvalidResponse);
    }
    Ok(SourceRef {
        material_id: block.material_id.clone(),
        version: block.version.clone(),
        text_version: block.text_version.clone(),
        block_id: block.id.clone(),
        start: block.start + offset,
        end: block.start + offset + quote.quote.len(),
        quote: quote.quote.clone(),
        page: block.page,
    })
}

pub(crate) fn resolve_all(
    blocks: &[Block],
    quotes: &[Quote],
) -> Result<Vec<SourceRef>, FailureReason> {
    quotes.iter().map(|q| resolve(blocks, q)).collect()
}

pub(crate) fn requirement(
    report: &QualificationReport,
    id: String,
    draft: RequirementDraft,
) -> Result<Requirement, FailureReason> {
    if draft.position.trim().is_empty()
        || draft.property.trim().is_empty()
        || draft.scope.trim().is_empty()
        || draft.evidence.is_empty()
    {
        return Err(FailureReason::InvalidResponse);
    }
    match &draft.constraint {
        Constraint::Numeric {
            operator,
            value,
            upper,
            unit,
        } => {
            if number(value).is_none()
                || unit.trim().is_empty()
                || (*operator == Operator::Range && upper.as_deref().and_then(number).is_none())
            {
                return Err(FailureReason::InvalidResponse);
            }
        }
        Constraint::Qualitative { value } if value.trim().is_empty() => {
            return Err(FailureReason::InvalidResponse);
        }
        Constraint::Alternatives { values }
            if values.len() < 2 || values.iter().any(|v| v.trim().is_empty()) =>
        {
            return Err(FailureReason::InvalidResponse);
        }
        _ => {}
    }
    let evidence = resolve_all(&report.blocks, &draft.evidence)?;
    let reference = draft
        .reference
        .as_ref()
        .map(|q| resolve(&report.blocks, q))
        .transpose()?;
    if evidence.iter().any(|s| s.material_id != report.document_id)
        && reference
            .as_ref()
            .is_none_or(|r| r.material_id != report.document_id)
    {
        return Err(FailureReason::InvalidResponse);
    }
    Ok(Requirement {
        id,
        position: draft.position,
        component: draft.component,
        property: draft.property,
        constraint: draft.constraint,
        scope: draft.scope,
        evidence,
        reference,
    })
}

pub(crate) fn number(text: &str) -> Option<String> {
    let s = text.trim().replace(',', ".");
    let s = s.strip_prefix('+').unwrap_or(&s);
    if s.is_empty()
        || s.chars().filter(|c| *c == '.').count() > 1
        || !s
            .chars()
            .enumerate()
            .all(|(i, c)| c.is_ascii_digit() || c == '.' || (i == 0 && c == '-'))
        || !s.chars().any(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(s.to_owned())
}

/// Cell ranges in pipe tables (including escaped pipes) or Poppler layout columns.
pub(crate) fn cells(text: &str) -> Vec<(usize, usize)> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("<td") || lower.contains("<th") {
        let mut result = vec![];
        let mut cursor = 0;
        while cursor < text.len() {
            let next = ["<td", "<th"]
                .into_iter()
                .filter_map(|tag| lower[cursor..].find(tag).map(|n| (cursor + n, tag)))
                .min_by_key(|v| v.0);
            let Some((start, tag)) = next else {
                break;
            };
            let mut end = start + tag.len();
            let mut quoted = None;
            while end < text.len() {
                let b = text.as_bytes()[end];
                if matches!(b, b'\'' | b'"') {
                    if quoted == Some(b) {
                        quoted = None;
                    } else if quoted.is_none() {
                        quoted = Some(b);
                    }
                }
                if b == b'>' && quoted.is_none() {
                    break;
                }
                end += 1;
            }
            if end == text.len() {
                break;
            }
            let closing = if tag == "<td" { "</td>" } else { "</th>" };
            let Some(close) = lower[end + 1..].find(closing).map(|n| end + 1 + n) else {
                break;
            };
            result.push((end + 1, close));
            cursor = close + closing.len();
        }
        return result;
    }
    let bytes = text.as_bytes();
    let mut boundaries = vec![];
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'|'
            && (i == 0 || bytes[..i].iter().rev().take_while(|b| **b == b'\\').count() % 2 == 0)
        {
            boundaries.push(i);
        }
    }
    if !boundaries.is_empty() {
        let mut parts = vec![];
        let mut start = 0;
        for end in boundaries.into_iter().chain(std::iter::once(text.len())) {
            if start != end || (start != 0 && end != text.len()) {
                parts.push((start, end));
            }
            start = (end + 1).min(text.len());
        }
        return parts;
    }
    let mut parts = vec![];
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b' ' {
            let end = i + bytes[i..].iter().take_while(|b| **b == b' ').count();
            if end - i >= 2 {
                if !text[start..i].trim().is_empty() {
                    parts.push((start, i));
                }
                start = end;
            }
            i = end;
        } else {
            i += 1;
        }
    }
    if !text[start..].trim().is_empty() {
        parts.push((start, text.len()));
    }
    parts
}

pub(crate) fn same_table_column(blocks: &[Block], a: &SourceRef, b: &SourceRef) -> bool {
    let block_a = blocks.iter().find(|x| x.id == a.block_id).unwrap();
    let block_b = blocks.iter().find(|x| x.id == b.block_id).unwrap();
    if block_a.table_headers.is_empty() && block_b.table_headers.is_empty() {
        return true;
    }
    if a.material_id != b.material_id || block_a.table_headers != block_b.table_headers {
        return false;
    }
    let column = |block: &Block, source: &SourceRef| {
        cells(&block.text)
            .into_iter()
            .enumerate()
            .filter_map(|(col, (start, end))| {
                (source.start >= block.start + start && source.end <= block.start + end)
                    .then_some(col)
            })
            .collect::<Vec<_>>()
    };
    let left = column(block_a, a);
    let right = column(block_b, b);
    left.len() == 1 && right.len() == 1 && left == right
}

/// Small, conservative HTML text view. Unsupported merged/nested tables remain explicit gaps.
pub(crate) fn html_text(html: &str) -> Result<(String, bool), FailureReason> {
    let mut output = String::new();
    let mut cursor = 0;
    let mut hidden: Option<String> = None;
    let mut table_depth = 0usize;
    let mut uncertain = false;
    while cursor < html.len() {
        if html.as_bytes()[cursor] != b'<' {
            let end = html[cursor..].find('<').map_or(html.len(), |v| cursor + v);
            if hidden.is_none() {
                let decoded = entities(&html[cursor..end]);
                if !decoded.trim().is_empty() {
                    if decoded.starts_with(char::is_whitespace) {
                        output.push(' ');
                    }
                    output.push_str(&decoded.split_whitespace().collect::<Vec<_>>().join(" "));
                    if decoded.ends_with(char::is_whitespace) {
                        output.push(' ');
                    }
                }
            }
            cursor = end;
            continue;
        }
        if html[cursor..].starts_with("<!--") {
            let end = html[cursor + 4..]
                .find("-->")
                .ok_or(FailureReason::Unreadable)?
                + cursor
                + 7;
            cursor = end;
            continue;
        }
        let mut quote = None;
        let mut end = cursor + 1;
        while end < html.len() {
            let b = html.as_bytes()[end];
            if matches!(b, b'\'' | b'"') {
                if quote == Some(b) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(b);
                }
            }
            if b == b'>' && quote.is_none() {
                break;
            }
            end += 1;
        }
        if end == html.len() {
            return Err(FailureReason::Unreadable);
        }
        let tag = html[cursor + 1..end].trim().to_ascii_lowercase();
        let name = tag
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        if let Some(hide) = &hidden {
            if name == format!("/{hide}") {
                hidden = None;
            }
        } else {
            match name {
                "script" | "style" | "noscript" => hidden = Some(name.into()),
                "table" => {
                    table_depth += 1;
                    uncertain |= table_depth > 1;
                    output.push('\n');
                }
                "/table" => {
                    table_depth = table_depth.saturating_sub(1);
                    output.push('\n');
                }
                "tr" => output.push_str("\n|"),
                "/tr" => {}
                "td" | "th" => {
                    uncertain |= tag.contains("rowspan") || tag.contains("colspan");
                }
                "/td" | "/th" => output.push('|'),
                "br" => {
                    if table_depth > 0 {
                        output.push(' ');
                    } else {
                        output.push('\n');
                    }
                }
                "p" | "/p" | "div" | "/div" | "li" | "/li" | "h1" | "/h1" | "h2" | "/h2" | "h3"
                | "/h3" => output.push('\n'),
                _ => {}
            }
        }
        cursor = end + 1;
    }
    if table_depth > 0 || hidden.is_some() {
        uncertain = true;
    }
    if output.trim().is_empty() {
        return Err(FailureReason::Unreadable);
    }
    Ok((output, uncertain))
}

fn entities(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some(end) = rest.find(';').filter(|end| *end < 12) {
            let value = &rest[1..end];
            let decoded = match value {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => value
                    .strip_prefix("#x")
                    .and_then(|n| u32::from_str_radix(n, 16).ok())
                    .or_else(|| value.strip_prefix('#').and_then(|n| n.parse().ok()))
                    .and_then(char::from_u32),
            };
            if let Some(c) = decoded {
                out.push(c);
                rest = &rest[end + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}
