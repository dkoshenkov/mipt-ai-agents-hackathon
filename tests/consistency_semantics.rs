use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use procurement_review::{
    consistency::{ReviewOptions, investigation::Verdict, priority::Selection, review_markdown},
    core::{Document, ErrorCategory, Visibility},
    runtime::{Runtime, grok::Model},
};
use serde_json::{Value, json};

struct Replay {
    property: &'static str,
    numeric: bool,
    conflict: bool,
    groups: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Model for Replay {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let prompt: Value = serde_json::from_str(prompt).unwrap();
        let field = |name: &str| {
            prompt["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == name)
                .map(|f| &f["value"])
        };
        if let Some(packet) = field("packet") {
            let mut claims = Vec::new();
            for block in packet["blocks"].as_array().unwrap() {
                if !packet["subjects"]
                    .as_array()
                    .unwrap()
                    .contains(&block["id"])
                {
                    continue;
                }
                let text = block["text"][0].as_str().unwrap();
                let object = if text.contains("позиция № 1") {
                    "позиция № 1"
                } else {
                    "позиция № 2"
                };
                let quote = |s: &str| json!({"block_id":block["id"],"quote":s});
                let constraint = if self.numeric {
                    let (number, unit) = if text.contains("2 комплекта") {
                        ("2", "комплекта")
                    } else {
                        ("5", "приборов")
                    };
                    json!({"kind":"numeric","operator":quote("ровно"),"value":quote(number),"unit":quote(unit)})
                } else {
                    json!({"kind":"semantic","text":quote(text)})
                };
                claims.push(json!({"statement":quote(text),"object":{"key":object,"evidence":[quote(object)],"alternatives":[]},
                    "property":quote(self.property),"constraint":constraint,"mandatory":if self.numeric { Some(quote("обязательно")) } else { None },"stage":null,"conditions":[]}));
            }
            return Ok(json!({"claims":claims,"priority_rules":[],"standard_mentions":[],"refinements":[]}).to_string());
        }
        if prompt["instruction"]
            .as_str()
            .unwrap()
            .starts_with("Propose only explicitly documented relationships")
        {
            return Ok(json!({"relations":[]}).to_string());
        }
        if let Some(claims) = field("claims") {
            let claims = claims.as_array().unwrap();
            self.groups.lock().unwrap().push(
                claims
                    .iter()
                    .map(|c| c["statement"]["text"].as_str().unwrap().to_owned())
                    .collect(),
            );
            let relation: Vec<_> = claims.iter().map(|c| json!({"block_id":c["statement"]["evidence"]["block_id"],"quote":c["statement"]["text"]})).collect();
            return Ok(
                json!({"candidates":[{"category":"semantic","claims":[0,1],"relation":relation}]})
                    .to_string(),
            );
        }
        if let Some(candidate) = field("candidate") {
            return Ok(json!({"candidate_id":candidate["id"],"version":candidate["evidence"][0]["version"],
                "action":if self.conflict {"confirmed"} else {"dismissed"}, "explanation":"Проверены обе формулировки и их связь.","evidence":[0,1]}).to_string());
        }
        Err(ErrorCategory::InvalidResponse)
    }
}

async fn check(first: &str, second: &str, property: &'static str, conflict: bool, numeric: bool) {
    let document = Document {
        markdown: format!("{first}\n\n{second}"),
        visibility: Visibility::Public,
    };
    let groups = Arc::new(Mutex::new(Vec::new()));
    let runtime = Runtime::new(
        Replay {
            property,
            numeric,
            conflict,
            groups: groups.clone(),
        },
        Duration::from_secs(1),
    );
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    let report = review_markdown(&document, ReviewOptions::default(), &runtime, &catalog)
        .await
        .unwrap();
    assert!(
        report
            .packets
            .iter()
            .all(|p| p.stop.is_none() && p.binding_issues.is_empty()),
        "{:?}",
        report.packets
    );
    assert!(
        report.semantic_gaps.is_empty(),
        "{:?}",
        report.semantic_gaps
    );
    if numeric || second.contains("позиция № 2") {
        assert!(
            report.investigations.is_empty(),
            "{:?}",
            report.investigations
        );
        assert!(groups.lock().unwrap().is_empty());
        return;
    }
    assert!(groups.lock().unwrap().iter().any(|g| g == &[first, second]));
    assert_eq!(
        report.investigations.len(),
        1,
        "{:?}",
        report.investigations
    );
    let investigation = &report.investigations[0];
    for side in [first, second] {
        assert!(investigation.candidate.evidence.iter().any(|e| e.version == report.version && &document.markdown[e.range.clone()] == side));
    }
    if conflict {
        assert!(matches!(
            investigation.outcome,
            Ok(Verdict::Confirmed { .. })
        ));
        assert!(
            matches!(report.selections[0].1, Selection::AskUser { .. }),
            "{:?}",
            report.selections
        );
    } else {
        assert!(matches!(
            investigation.outcome,
            Ok(Verdict::Dismissed { .. })
        ));
        assert!(matches!(report.selections[0].1, Selection::NotApplicable));
    }
}

#[tokio::test]
async fn position_links_names_and_compatible_variants_are_dismissed() {
    let first = "позиция № 1: наименование лифт";
    for (second, conflict) in [
        ("позиция № 1: наименование больничный лифт", false),
        ("позиция № 1: наименование лифт (далее ЛФ)", false),
        (
            "позиция № 1: наименование лифт или разрешённый эквивалент подъёмник",
            false,
        ),
        (
            "позиция № 1: наименование лебёдка; обязательно единое написание",
            true,
        ),
        (
            "позиция № 2: наименование лебёдка; обязательно единое написание",
            false,
        ),
    ] {
        check(first, second, "наименование", conflict, false).await;
    }
    check(
        "позиция № 1: тип обязательно стационарный",
        "позиция № 1: тип обязательно переносной",
        "тип",
        true,
        false,
    )
    .await;
}

#[tokio::test]
async fn actions_components_and_counting_units() {
    check(
        "позиция № 1: монтаж обязателен",
        "позиция № 1: монтаж исключён",
        "монтаж",
        true,
        false,
    )
    .await;
    check(
        "позиция № 1: комплектность включает обязательный манометр",
        "позиция № 1: комплектность исключает манометр",
        "комплектность",
        true,
        false,
    )
    .await;
    check(
        "позиция № 1: количество обязательно ровно 2 комплекта",
        "позиция № 1: количество обязательно ровно 5 приборов в комплекте",
        "количество",
        false,
        true,
    )
    .await;
}
