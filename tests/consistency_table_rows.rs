use std::time::Duration;

use procurement_review::{
    consistency::{
        ReviewOptions,
        claims::{Claim, Constraint},
        markdown::MarkdownIndex,
        review_markdown,
    },
    core::{Document, ErrorCategory, Visibility},
    runtime::{Runtime, grok::Model},
};
use serde_json::{Value, json};

// Replays the same malformed proposals seen in source-lift/full.responses.jsonl:
// 300's unit points at its value cell, and the work rows yield quantity 1.
struct Replay;
impl Model for Replay {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let request: Value = serde_json::from_str(prompt).unwrap();
        let fields = request["data"].as_array().unwrap();
        let field = |name: &str| {
            fields
                .iter()
                .find(|f| f["name"] == name)
                .map(|f| &f["value"])
        };
        if let Some(candidate) = field("candidate") {
            let additional = field("additional_evidence").unwrap().as_array().unwrap();
            let response = if additional.is_empty()
                && !candidate["known_gaps"].as_array().unwrap().is_empty()
            {
                json!({"candidate_id":candidate["id"],"version":candidate["evidence"][0]["version"],
                    "action":"read","block_ids":candidate["known_gaps"]})
            } else {
                json!({"candidate_id":candidate["id"],"version":candidate["evidence"][0]["version"],
                    "action":"needs_review","explanation":"Характеристики нового оборудования расходятся.",
                    "question":"Какая грузоподъемность принята для нового оборудования?", "evidence":[0]})
            };
            return Ok(response.to_string());
        }
        let mut claims = Vec::new();
        if let Some(packet) = field("packet") {
            let blocks = packet["blocks"].as_array().unwrap();
            let block = |text: &str| {
                blocks.iter().find(|b| {
                    b["text"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|t| t.as_str().is_some_and(|s| s.trim() == text))
                })
            };
            let q = |b: &Value, text: &str| json!({"block_id":b["id"], "quote":text});
            for row in blocks {
                if !packet["subjects"].as_array().unwrap().contains(&row["id"]) {
                    continue;
                }
                let Some(text) = row["text"][0].as_str() else {
                    continue;
                };
                if ["300", "450", "500"].contains(&text.trim())
                    && let (Some(property), Some(header)) = (
                        block("Грузоподъемность, кг"),
                        block("Наименование оборудования"),
                    )
                {
                    claims.push(json!({
                        "statement":q(row,text.trim()),
                        "object":{"key":"Грузоподъемность, кг","evidence":[q(property,"Грузоподъемность, кг"),q(row,text.trim())],"alternatives":[]},
                        "property":q(header,"Наименование оборудования"), "constraint":{"kind":"numeric",
                            "operator":q(property,"Грузоподъемность,"),"value":q(row,text.trim()),"unit":q(row,"кг")},
                        "stage":null,"conditions":[],"mandatory":null
                    }));
                }
                // Quantity-cell proposals are selected using the geometry supplied in the packet.
                if text.trim().starts_with("Лифт грузовой") {
                    let Some(table) = row.get("table").filter(|v| !v.is_null()) else {
                        continue;
                    };
                    let same_row = |b: &&Value| {
                        b["table"]["id"] == table["id"] && b["table"]["rows"] == table["rows"]
                    };
                    let quantity = blocks
                        .iter()
                        .filter(same_row)
                        .rfind(|b| b["text"][0].as_str().is_some_and(|s| s.trim() == "1"));
                    let unit = blocks.iter().filter(same_row).find(|b| {
                        ["шт.", "лифт"].contains(&b["text"][0].as_str().unwrap_or("").trim())
                    });
                    if let (Some(quantity), Some(unit)) = (quantity, unit) {
                        let unit_text = unit["text"][0].as_str().unwrap().trim();
                        let stage = blocks
                            .iter()
                            .find(|b| b["id"] == table["section"])
                            .and_then(|b| b["text"][0].as_str().map(|t| q(b, t.trim())));
                        claims.push(json!({"statement":q(row,text.trim()),"object":{"key":text.trim(),"evidence":[q(row,text.trim()),q(quantity,"1"),q(unit,unit_text)],"alternatives":[]},"property":q(row,text.trim()),"constraint":{"kind":"numeric","operator":q(quantity,"1"),"value":q(quantity,"1"),"unit":q(unit,unit_text)},"stage":stage,"conditions":[],"mandatory":null}));
                    }
                }
            }
        }
        Ok(json!({"claims":claims, "priority_rules":[], "standard_mentions":[], "refinements":[], "candidates":[], "relations":[]}).to_string())
    }
}

const EXCERPT: &str = r#"# ТЕХНИЧЕСКОЕ ЗАДАНИЕ

## 5. Состав работ.

- демонтаж и вывоз старого лифта и лифтового оборудования;

- поставку нового лифта и лифтового оборудования (доставка, разгрузка оборудования на объекте Заказчика, а также транспортировка и перемещение на территории Заказчика производится силами и средствами Подрядчика);

- монтаж нового лифта и лифтового оборудования;

## 6. Исходные данные для выполнения работ.

Новый грузовой лифт устанавливается взамен отработавшего лифта, грузоподъемность 500 кг, скорость движения кабины 0,5  м/с, количество остановок 4.

Технические характеристики поставляемого для монтажа лифта:

<table>
<tr><td>№ п/п</td><td>Наименование оборудования</td><td>Технические характеристики</td></tr>
<tr><td>3</td><td>Грузоподъемность, кг</td><td>300</td></tr>
</table>

# ДЕФЕКТНАЯ ВЕДОМОСТЬ

на выполнение работ по замене грузового лифта

<table>
<tr><td>№ п/п</td><td>Наименование работ</td><td>Единица измерения</td><td>Объем</td></tr>
<tr><td></td><td>Раздел 1.  Демонтажные работы.</td><td></td><td></td></tr>
<tr><td>1</td><td>Лифт грузовой грузоодъемностью500кг</td><td>шт.</td><td>1</td></tr>
<tr><td></td><td>Раздел 2.  Монтажные работы.</td><td></td><td></td></tr>
<tr><td>2</td><td>Лифт грузовой со скоростью движения кабины до 0,5 м/с: грузоподъемностью 500 кг на 4 остановки </td><td>шт.</td><td>1</td></tr>
<tr><td></td><td>Раздел 4.  Пусконаладочные работы.</td><td></td><td></td></tr>
<tr><td>5</td><td>Лифт грузовой, грузоподъемность 500 кг, на 4 остановки</td><td>лифт</td><td>1</td></tr>
</table>
"#;

fn numeric_at<'a>(claims: &'a [Claim], source: &str, cell: &str, value: &str) -> &'a Claim {
    let start = source.find(cell).unwrap();
    let range = start..start + cell.len();
    let matches: Vec<_> = claims
        .iter()
        .filter(|claim| {
            matches!(&claim.constraint, Constraint::Numeric { value: v, .. }
            if v.text == value && range.contains(&v.evidence.range.start))
                && claim.property.text.to_lowercase().contains("грузо")
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected one carrying-capacity claim in {cell:?}, got {matches:?}"
    );
    matches[0]
}

async fn check(markdown: &str, value: &str) {
    let document = Document {
        markdown: markdown.into(),
        visibility: Visibility::Public,
    };
    let index = MarkdownIndex::new(markdown).unwrap();
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    let report = review_markdown(
        &document,
        ReviewOptions::default(),
        &Runtime::new(Replay, Duration::from_secs(1)),
        &catalog,
    )
    .await
    .unwrap();
    let value_cell = format!("<td>{value}</td>");
    let table = numeric_at(&report.claims, markdown, &value_cell, value);
    let old_cell = if value == "500" {
        "Лифт грузовой грузоодъемностью450кг"
    } else {
        "Лифт грузовой грузоодъемностью500кг"
    };
    let old = numeric_at(
        &report.claims,
        markdown,
        old_cell,
        if value == "500" { "450" } else { "500" },
    );
    let install = numeric_at(
        &report.claims,
        markdown,
        "Лифт грузовой со скоростью движения кабины до 0,5 м/с: грузоподъемностью 500 кг на 4 остановки",
        "500",
    );
    let commissioning = numeric_at(
        &report.claims,
        markdown,
        "Лифт грузовой, грузоподъемность 500 кг, на 4 остановки",
        "500",
    );
    for claim in [table, old, install, commissioning] {
        assert!(claim.complete, "{claim:?}");
        let Constraint::Numeric {
            value,
            unit: Some(unit),
            ..
        } = &claim.constraint
        else {
            panic!("missing numeric value and unit")
        };
        assert_eq!(unit.text, "кг");
        for grounded in [&claim.statement, &claim.property, value, unit]
            .into_iter()
            .chain(claim.object.evidence.iter())
        {
            assert_eq!(&markdown[grounded.evidence.range.clone()], grounded.text);
            assert_eq!(grounded.evidence.version, index.version);
        }
    }
    let Constraint::Numeric {
        unit: Some(unit), ..
    } = &table.constraint
    else {
        unreachable!()
    };
    assert_eq!(unit.evidence.block_id, table.property.evidence.block_id);
    assert_ne!(unit.evidence.block_id, table.statement.evidence.block_id);
    for (claim, section) in [
        (old, "Демонтажные работы"),
        (install, "Монтажные работы"),
        (commissioning, "Пусконаладочные работы"),
    ] {
        let stage = claim.stage.as_ref().expect("work section");
        assert!(stage.text.contains(section));
        assert_eq!(&markdown[stage.evidence.range.clone()], stage.text);
    }
    assert_eq!(table.object.key, install.object.key);
    assert_eq!(table.object.key, commissioning.object.key);
    assert_ne!(table.object.key, old.object.key);
    let candidates: Vec<_> = report
        .initial_candidates
        .iter()
        .filter(|c| c.category == "numeric")
        .collect();
    if value == "500" {
        assert!(
            candidates.is_empty(),
            "matching new-lift values must not conflict: {candidates:?}"
        );
    } else {
        let candidate = candidates
            .iter()
            .find(|c| c.evidence.contains(&table.statement.evidence))
            .expect("numeric candidate for the new lift");
        for claim in [table, install, commissioning] {
            assert!(
                candidate.evidence.contains(&claim.statement.evidence),
                "missing source: {claim:?}"
            );
            let Constraint::Numeric {
                value,
                unit: Some(unit),
                ..
            } = &claim.constraint
            else {
                unreachable!()
            };
            assert!(candidate.evidence.contains(&value.evidence));
            assert!(candidate.evidence.contains(&unit.evidence));
        }
        assert!(
            !candidate.evidence.contains(&old.statement.evidence),
            "old lift must be excluded"
        );
        let investigation = report
            .investigations
            .iter()
            .find(|i| i.candidate.id == candidate.id)
            .unwrap();
        assert_eq!(investigation.critic_calls, 2);
        assert!(
            matches!(investigation.read_decisions.as_slice(), [r] if r.outcome == procurement_review::consistency::investigation::ReadOutcome::Granted)
        );
        assert!(!investigation.additional_evidence.is_empty());
        assert!(matches!(
            investigation.outcome,
            Ok(procurement_review::consistency::investigation::Verdict::NeedsReview { .. })
        ));
    }
    assert!(report.claims.iter().all(|claim| !matches!(&claim.constraint, Constraint::Numeric { value, .. } if value.text == "1" && claim.property.text.to_lowercase().contains("грузо"))));
}

#[tokio::test]
async fn carrying_capacity_belongs_to_equipment_and_its_work_section() {
    check(EXCERPT, "300").await;
    check(&EXCERPT.replace("<td>300</td>", "<td>450</td>"), "450").await;
    check(
        &EXCERPT
            .replace("<td>300</td>", "<td>500</td>")
            .replace("грузоодъемностью500кг", "грузоодъемностью450кг"),
        "500",
    )
    .await;
}

#[tokio::test]
async fn converter_format_keeps_both_sides_of_the_new_equipment_questions() {
    // Same mixed representation emitted by /api/convert: inline HTML for merged cells,
    // Markdown pipes for the work list, emphasis and a plain-text register title.
    let converted = r#"\- поставку нового лифта и лифтового оборудования;

\- монтаж нового лифта и лифтового оборудования;

Новый грузовой лифт устанавливается взамен отработавшего лифта, грузоподъемность 500 кг.

**Технические характеристики поставляемого для монтажа лифта:**

<table><thead><tr><th><p><strong>№ п/п</strong></p></th><th><p>Наименование оборудования</p></th><th><p>Технические характеристики</p></th></tr></thead><tbody><tr><td>1</td><td>Тип лифта</td><td><p>пассажирский</p></td></tr><tr><td>3</td><td>Грузоподъемность, кг</td><td><p>300</p></td></tr><tr><td colspan="3">Кабина</td></tr></tbody></table>

ДЕФЕКТНАЯ ВЕДОМОСТЬ

на выполнение работ по замене грузового лифта

| **№ п/п** | **Наименование работ** | **Единица измерения** | **Объем** |
| --- | --- | --- | --- |
|  | **Раздел 2. Монтажные работы.** |  |  |
| 2 | Лифт грузовой со скоростью движения кабины до 0,5 м/с: грузоподъемностью 500 кг на 4 остановки | шт. | 1 |
"#;
    let runtime = Runtime::new(Replay, Duration::from_secs(1));
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    for (markdown, expected) in [
        (converted.to_owned(), 2),
        (
            converted
                .replace("пассажирский", "грузовой")
                .replace(">300<", ">500<"),
            0,
        ),
    ] {
        let document = Document {
            markdown,
            visibility: Visibility::Public,
        };
        let report = review_markdown(
            &document,
            ReviewOptions {
                model_attempts: 0,
                ..Default::default()
            },
            &runtime,
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(report.model_attempts, 0);
        assert_eq!(report.investigations.len(), expected);
        for result in &report.investigations {
            assert!(result.outcome.is_err());
            let comparison = result.source_comparison.as_ref().unwrap();
            let sides = &comparison.sides;
            assert!(sides.iter().all(|e| e.version == report.version));
            let excerpts = sides
                .each_ref()
                .map(|e| &document.markdown[e.range.clone()]);
            if result.candidate.category == "numeric" {
                assert!(excerpts.contains(&"300") && excerpts.iter().any(|s| s.contains("500 кг")));
            } else {
                assert!(
                    excerpts.contains(&"пассажирский")
                        && excerpts.iter().any(|s| s.contains("Новый грузовой лифт"))
                );
            }
        }
    }
}

#[tokio::test]
#[ignore = "requires the original experiment document"]
async fn actual_lift_document_when_available() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/openspec/changes/add-markdown-consistency-review/experiments/2026-11-14-structured/documents/lift.md"
    );
    let markdown = std::fs::read_to_string(path).expect("original lift document");
    check(&markdown, "300").await;
}

#[tokio::test]
async fn explicit_object_parameter_value_and_unit_columns_keep_rows_separate() {
    let markdown = "# Оборудование\n\n<table>\n<tr><td>Объект</td><td>Параметр</td><td>Значение</td><td>Ед. изм.</td></tr>\n<tr><td>Насос А</td><td>Масса</td><td>10</td><td>кг</td></tr>\n<tr><td>Насос А</td><td>Масса</td><td>20</td><td>кг</td></tr>\n<tr><td>Насос Б</td><td>Масса</td><td>30</td><td>кг</td></tr>\n</table>\n";
    let document = Document {
        markdown: markdown.into(),
        visibility: Visibility::Public,
    };
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    let report = review_markdown(
        &document,
        ReviewOptions::default(),
        &Runtime::new(Replay, Duration::from_secs(1)),
        &catalog,
    )
    .await
    .unwrap();
    let claim = |number: &str| {
        report.claims.iter().find(|claim| matches!(&claim.constraint, Constraint::Numeric { value, .. } if value.text == number)).expect("row claim")
    };
    let (first, second, other) = (claim("10"), claim("20"), claim("30"));
    for (claim, object, number) in [
        (first, "Насос А", "10"),
        (second, "Насос А", "20"),
        (other, "Насос Б", "30"),
    ] {
        assert!(claim.complete, "{claim:?}");
        assert_eq!(claim.object.key, Some(object.to_lowercase()));
        assert!(claim.object.evidence.iter().any(|e| e.text == object));
        assert_eq!(claim.property.text, "Масса");
        let Constraint::Numeric {
            value,
            unit: Some(unit),
            ..
        } = &claim.constraint
        else {
            panic!("missing unit")
        };
        assert_eq!(value.text, number);
        assert_eq!(unit.text, "кг");
        let row_start = markdown[..value.evidence.range.start]
            .rfind("<tr>")
            .unwrap();
        let row_end =
            markdown[value.evidence.range.end..].find("</tr>").unwrap() + value.evidence.range.end;
        for grounded in [&claim.statement, &claim.property, value, unit] {
            assert_eq!(&markdown[grounded.evidence.range.clone()], grounded.text);
            assert!((row_start..row_end).contains(&grounded.evidence.range.start));
        }
    }
    let candidate = report
        .initial_candidates
        .iter()
        .find(|c| c.category == "numeric" && c.evidence.contains(&first.statement.evidence))
        .expect("same pump conflicts");
    assert!(candidate.evidence.contains(&second.statement.evidence));
    assert!(!candidate.evidence.contains(&other.statement.evidence));
}
