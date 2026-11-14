use std::{future::Future, pin::Pin, sync::Mutex, time::Duration};

use procurement_review::{
    consistency::{
        ReviewOptions,
        catalog_http::{CatalogHttp, Outcome as CatalogOutcome, Response, Transport},
        review_markdown, review_with_catalog,
        standard_usage::{Outcome, Role},
        standards::{Catalog, Existence},
    },
    core::{Document, ErrorCategory, Visibility},
    runtime::{Runtime, grok::Model},
};
use serde_json::{Value, json};

struct Replay;

impl Model for Replay {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let request: Value = serde_json::from_str(prompt).unwrap();
        let field = |name: &str| {
            request["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|field| field["name"] == name)
                .map(|field| &field["value"])
        };
        if let Some(mention) = field("mention") {
            let packet = field("packet").unwrap();
            let id = mention["evidence"]["block_id"].as_u64().unwrap();
            let block = packet["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|block| block["id"] == id)
                .unwrap();
            let text = block["text"][0].as_str().unwrap();
            if text.contains("ОТКАЗ") {
                return Err(ErrorCategory::RateLimited);
            }
            let quote = |s: &str| json!({"block_id":id,"quote":s});
            let cards = field("cards").unwrap().as_array().unwrap();
            let scope = cards
                .first()
                .map(|card| card["scope"].clone())
                .unwrap_or(Value::Null);
            let role = if text.contains("Библиография") {
                "bibliography"
            } else {
                "direct"
            };
            let (method, temperature) = if text.contains("Роквелла") && text.contains("20 °C")
            {
                (quote("Роквелла"), quote("20 °C"))
            } else {
                (Value::Null, Value::Null)
            };
            let outcome = if text.contains("чужой метод") {
                "outside_scope"
            } else {
                "supported"
            };
            return Ok(
                json!({"role":role,"title":null,"statement":quote(text),"method":method,
                "temperature":temperature,"caveats":[],"attributed_threshold":null,
                "action":text.contains("чужой метод").then(|| quote("чужой метод")),
                "official_excerpt":scope,"outcome":outcome})
                .to_string(),
            );
        }
        if field("packet").is_some() {
            let packet = field("packet").unwrap();
            let mentions: Vec<_> = if field("existing_claims").is_some() {
                packet["blocks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|block| {
                        block["text"][0]
                            .as_str()
                            .filter(|text| text.contains("ВОССТАНОВЛЕНО"))
                            .map(|_| json!({"block_id": block["id"], "quote": "ГОСТ 9013-59"}))
                    })
                    .collect()
            } else {
                vec![]
            };
            return Ok(
                json!({"claims":[],"priority_rules":[],"standard_mentions":mentions,"refinements":[]})
                    .to_string(),
            );
        }
        Err(ErrorCategory::InvalidResponse)
    }
}

struct PartialCatalog(Mutex<Vec<Response>>);
impl Transport for PartialCatalog {
    fn get<'a>(
        &'a self,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Response, ()>> + Send + 'a>> {
        Box::pin(async move {
            let mut responses = self.0.lock().unwrap();
            if responses.is_empty() {
                Err(())
            } else {
                Ok(responses.remove(0))
            }
        })
    }
}

#[tokio::test]
async fn partial_catalog_with_card_remains_incomplete() {
    let mut list = String::from("Найдено: <strong>21</strong><table>");
    for i in 0..20 {
        let designation = if i == 0 {
            "ГОСТ 9013-59"
        } else {
            "ГОСТ 1234-20"
        };
        list.push_str(&format!("<a href=\"/gost/details/{i:08x}-0000-0000-0000-000000000000\"><span>{designation}</span></a>"));
    }
    list.push_str("</table>");
    let mut card =
        String::from("<!-- Бейдж статуса --><span><span></span>Действует</span>\n</div>");
    for (label, value) in [
        ("Обозначение", "ГОСТ 9013-59"),
        ("Полное обозначение", "ГОСТ 9013-59"),
        ("Заглавие на русском языке", "Стандарт"),
        ("Область применения", "Область"),
    ] {
        card.push_str(&format!(
            "<div><span>{label}</span><p class=\"value\">{value}</p></div>"
        ));
    }
    let responses = [list, card]
        .into_iter()
        .map(|body| Response {
            status: 200,
            location: None,
            body,
        })
        .collect();
    let mut client =
        CatalogHttp::with_transport(PartialCatalog(Mutex::new(responses)), 3, 2, "2026-11-13");
    let report = review_with_catalog(
        &Document {
            markdown: "Библиография: ГОСТ 9013-59.".into(),
            visibility: Visibility::Public,
        },
        ReviewOptions::default(),
        &Runtime::new(Replay, Duration::from_secs(1)),
        &catalog(),
        Some(&mut client),
    )
    .await
    .unwrap();
    assert_eq!(report.catalog_trace[0].catalog.cards.len(), 1);
    assert!(matches!(
        report.catalog_trace[0].outcome,
        CatalogOutcome::NetworkError
    ));
    assert_eq!(report.standard_coverage.failed_catalog, vec![0]);
    assert!(!report.standard_coverage.complete);
}

fn catalog() -> Catalog {
    serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap()
}

async fn review(
    text: &str,
    catalog: &Catalog,
) -> procurement_review::consistency::ConsistencyReport {
    review_markdown(
        &Document {
            markdown: text.into(),
            visibility: Visibility::Public,
        },
        ReviewOptions {
            assessment_date: Some("2026-11-13"),
            ..Default::default()
        },
        &Runtime::new(Replay, Duration::from_secs(1)),
        catalog,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn per_occurrence_outcomes_preserve_failures_and_official_basis() {
    let report = review("Библиография: ГОСТ 9013-59.\n\nВОССТАНОВЛЕНО: Метод Роквелла при 20 °C по xГОСТ 9013-59.\n\nОТКАЗ: ГОСТ 9013-59.", &catalog()).await;
    assert_eq!(report.standard_usage.len(), 3);
    assert_eq!(report.recovered_standard_mentions.len(), 1);
    assert_eq!(report.standard_coverage.uncertain_usage, vec![0]);
    assert_eq!(report.standard_coverage.failed_usage, vec![2]);
    assert!(!report.standard_coverage.complete);
    assert!(matches!(report.standard_usage[0].role, Role::Bibliography));
    assert!(matches!(
        report.standard_usage[0].outcome,
        Outcome::InsufficientEvidence
    ));
    assert!(matches!(
        report.standard_usage[1].outcome,
        Outcome::Supported
    ));
    assert!(report.standard_usage[1].official_url.is_some());
    assert!(report.standard_usage[1].official_excerpt.is_some());
    assert!(matches!(
        report.standard_usage[2].outcome,
        Outcome::TechnicalFailure
    ));
    assert!(report.standard_usage[2].limitation.is_some());
    assert_ne!(
        report.standard_usage[0].mention.range,
        report.standard_usage[1].mention.range
    );
}

#[tokio::test]
async fn unsupported_scope_and_old_or_undated_designations_do_not_become_supported() {
    let mut catalog = catalog();
    let mut lift = catalog.cards[0].clone();
    lift.designation = "ГОСТ 1234-2020".into();
    lift.full_designation = lift.designation.clone();
    lift.scope =
        "Настоящий стандарт распространяется на ввод новых лифтов. Стандарт исключает чужой метод."
            .into();
    lift.temperature_scope = None;
    catalog.cards.push(lift);
    let report = review("Ежегодное обслуживание лифтов по ГОСТ 1234-2020.\n\nчужой метод по ГОСТ 1234-2020.\n\nПересказ назначения по ГОСТ 9013.\n\nМетод по ГОСТ 9013-49.", &catalog).await;
    assert!(matches!(
        report.standard_usage[0].outcome,
        Outcome::InsufficientEvidence
    ));
    assert!(matches!(
        report.standard_usage[1].outcome,
        Outcome::OutsideScope
    ));
    assert!(matches!(
        report.standard_references[2].existence,
        Existence::Undated
    ));
    assert!(matches!(
        report.standard_usage[2].outcome,
        Outcome::InsufficientEvidence
    ));
    assert!(matches!(
        report.standard_usage[3].outcome,
        Outcome::InsufficientEvidence
    ));
    assert!(
        report
            .standard_usage
            .iter()
            .all(|u| u.editorial_correction.is_none())
    );
    assert!(report.standard_usage[2].action.is_none());
}

#[tokio::test]
async fn no_mentions_and_missing_appendix_are_distinct() {
    let empty = review("Требования к поставке оборудования.", &catalog()).await;
    assert!(empty.standard_references.is_empty());
    assert!(empty.missing_standard_materials.is_empty());
    assert!(empty.packets.iter().all(|p| p.stop.is_none()));
    assert!(empty.standard_coverage.complete);
    let missing = review(
        "Метод испытаний и стандарт приведены в приложении 2.",
        &catalog(),
    )
    .await;
    assert!(missing.standard_references.is_empty());
    assert_eq!(missing.missing_standard_materials.len(), 1);
    assert_eq!(missing.standard_coverage.missing_materials, vec![0]);
    assert!(!missing.standard_coverage.complete);
    assert_eq!(
        missing.standard_material_questions[0].reference,
        missing.missing_standard_materials[0]
    );
    assert!(
        missing.standard_material_questions[0]
            .question
            .contains("Предоставьте")
    );

    let unread = review_markdown(
        &Document {
            markdown: "Недоступный блок".into(),
            visibility: Visibility::Public,
        },
        ReviewOptions {
            packet_bytes: 1,
            ..Default::default()
        },
        &Runtime::new(Replay, Duration::from_secs(1)),
        &catalog(),
    )
    .await
    .unwrap();
    assert!(!unread.standard_coverage.complete);
    assert!(!unread.standard_coverage.oversized_blocks.is_empty());
}
