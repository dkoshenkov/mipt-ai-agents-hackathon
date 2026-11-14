//! A separate reading list with stable anchors into a local, line-numbered view.
use super::Report;
use std::fmt::Write;

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
pub fn document_view(name: &str, markdown: &str) -> String {
    let mut out = format!(
        "<!doctype html><html lang=ru><meta charset=utf-8><title>{}</title><style>body{{max-width:100ch;margin:2rem auto;font:16px/1.5 system-ui}}p{{white-space:pre-wrap;overflow-wrap:anywhere;margin:0;min-height:1em}}p:target{{background:#fff1a8}}small{{color:#666}}</style><h1>{}</h1>",
        escape(name),
        escape(name)
    );
    for (i, row) in markdown.lines().enumerate() {
        let n = i + 1;
        writeln!(
            out,
            "<p id=\"L{n}\"><small>{n}  </small>{}</p>",
            escape(row)
        )
        .unwrap();
    }
    out.push_str("</html>");
    out
}
pub fn notes(report: &Report) -> String {
    let mut out = String::from(
        "# Замечания по 223-ФЗ\n\n[Исходное ТЗ (.md)](source.md) · [ТЗ с номерами строк](source.html)\n\n",
    );
    let errors = report
        .coverage
        .iter()
        .filter(|o| matches!(o.state, "error" | "restricted" | "rejected"))
        .count();
    if errors > 0 {
        writeln!(
            out,
            "Неполное покрытие. Технические пропуски: {errors}. Подробности в report.json.\n"
        )
        .unwrap();
    }
    if report.findings.is_empty() {
        out.push_str("Проверенных замечаний для публикации нет.\n");
    }
    let mut items: Vec<_> = report.findings.iter().collect();
    items.sort_by_key(|f| f.source.line);
    for f in items {
        writeln!(
            out,
            "## {} · {}\n\n[Строка {}](source.html#L{}) · {} · {}\n\n> {}\n",
            f.check,
            f.status,
            f.source.line,
            f.source.line,
            f.category,
            f.scope,
            escape(&f.source.quote)
        )
        .unwrap();
        for (label, locations) in [
            ("Также подтверждает", &f.additional_sources),
            ("Условие", &f.context),
        ] {
            for loc in locations {
                writeln!(
                    out,
                    "{label} · [Строка {}](source.html#L{})\n\n> {}\n",
                    loc.line,
                    loc.line,
                    escape(&loc.quote)
                )
                .unwrap();
            }
        }
        writeln!(out, "{}\n", f.explanation).unwrap();
        if !f.basis.is_empty() {
            writeln!(out, "Основание: {}\n", f.basis).unwrap();
        }
        if let Some(m) = &f.missing {
            writeln!(out, "Требуется установить: {m}\n").unwrap();
        }
        if let Some(w) = &f.wording {
            writeln!(out, "Предлагаемая формулировка: {w}\n").unwrap();
        }
        if let Some(r) = &f.wording_reason {
            writeln!(out, "Основание замены: {r}\n").unwrap();
        }
    }
    out
}
