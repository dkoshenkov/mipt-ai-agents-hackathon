use procurement_review::{
    consistency::{
        catalog_http::{CatalogHttp, Outcome, Transport},
        markdown::MarkdownIndex,
        standards::{self, Catalog, CatalogIndex},
    },
    core::Visibility,
};
use serde_json::{Value, json};
use std::collections::HashSet;

/// Compare the source references independently of the catalogue used by all review modes.
pub async fn compare<T: Transport>(
    index: &MarkdownIndex,
    snapshot: &Catalog,
    session: &mut CatalogHttp<T>,
    date: &str,
) -> Value {
    let scan = standards::scan(index);
    let mut seen = HashSet::new();
    let mut results = Vec::new();
    let local = CatalogIndex::new(snapshot);
    for mention in scan.mentions {
        if !seen.insert(mention.key.clone()) {
            continue;
        }
        let lookup = session.lookup(Visibility::Public, &mention.key).await;
        let complete = matches!(lookup.outcome, Outcome::Found | Outcome::NotFound);
        let old = local.key_matches(&mention.key);
        let new = CatalogIndex::new(&lookup.catalog)
            .key_matches(&mention.key)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut changes = Vec::new();
        for before in &old {
            let matches: Vec<_> = new
                .iter()
                .filter(|card| card.designation == before.designation)
                .collect();
            if matches.len() != 1
                || old
                    .iter()
                    .filter(|card| card.designation == before.designation)
                    .count()
                    != 1
            {
                continue;
            }
            let left = serde_json::to_value(before).expect("card serialization");
            let right = serde_json::to_value(matches[0]).expect("card serialization");
            let fields: Vec<_> = left
                .as_object()
                .unwrap()
                .keys()
                .filter(|field| left[*field] != right[*field])
                .cloned()
                .collect();
            if !fields.is_empty() {
                changes.push(json!({"designation":before.designation,"fields":fields}));
            }
        }
        results.push(json!({
            "key":mention.key, "outcome":lookup.outcome, "search_complete":complete,
            "requests":lookup.requests, "snapshot_cards":old, "live_cards":new,
            "changed_fields":changes
        }));
    }
    json!({"checked_at_utc":chrono::Utc::now().to_rfc3339(), "date":date,
        "snapshot_date":snapshot.retrieved_at, "unread_ranges":scan.unread_ranges,
        "requests":session.total_requests(), "keys":results})
}

#[cfg(test)]
mod tests {
    use super::*;
    use procurement_review::consistency::catalog_http::Response;
    use std::{future::Future, pin::Pin, sync::Mutex};

    struct Fake(Mutex<Vec<Response>>);
    impl Transport for Fake {
        fn get<'a>(
            &'a self,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Response, ()>> + Send + 'a>> {
            Box::pin(async move { self.0.lock().unwrap().pop().ok_or(()) })
        }
    }
    fn response(body: &str) -> Response {
        Response {
            status: 200,
            location: None,
            body: body.into(),
        }
    }
    #[tokio::test]
    async fn shared_session_supplies_live_card_to_each_occurrence() {
        use procurement_review::{
            consistency::{ReviewOptions, review_with_catalog},
            core::{Document, ErrorCategory},
            runtime::{Runtime, grok::Model},
        };
        use std::{sync::Arc, time::Duration};

        struct Capture(Arc<Mutex<Vec<String>>>);
        impl Model for Capture {
            async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
                if prompt.contains("For this single numbered occurrence") {
                    self.0.lock().unwrap().push(prompt.to_owned());
                }
                Ok(r#"{"claims":[],"priority_rules":[],"refinements":[],"standard_mentions":[],"role":"bibliography","outcome":"insufficient_evidence"}"#.into())
            }
        }
        let text = "ГОСТ Р 57641-2017.\n\nГОСТ Р 57641-2017.\n\nГОСТ Р 57641-2017.";
        let document = Document {
            markdown: text.into(),
            visibility: Visibility::Public,
        };
        let index = MarkdownIndex::new(text).unwrap();
        let snapshot: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let baseline = Catalog {
            retrieved_at: "2026-11-14".into(),
            cards: vec![],
        };
        let list = "Найдено: <strong>1</strong><table><a href=\"/gost/details/00000000-0000-0000-0000-000000000000\"><span>ГОСТ Р 57641-2017</span></a></table>";
        let card = "<!-- Бейдж статуса --><span><span></span>Действует</span>\n</div><div><span>Обозначение</span><p class=\"value\">ГОСТ Р 57641-2017</p></div><div><span>Полное обозначение</span><p class=\"value\">ГОСТ Р 57641-2017</p></div><div><span>Заглавие на русском языке</span><p class=\"value\">Тестовая карточка</p></div><div><span>Область применения</span><p class=\"value\">Тестовая область применения</p></div>";
        let mut session = CatalogHttp::with_transport(
            Fake(Mutex::new(vec![response(card), response(list)])),
            20,
            2,
            "2026-11-14",
        );
        compare(&index, &snapshot, &mut session, "2026-11-14").await;
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let report = review_with_catalog(
            &document,
            ReviewOptions {
                model_attempts: 30,
                assessment_date: Some("2026-11-14"),
                ..Default::default()
            },
            &Runtime::new(Capture(prompts.clone()), Duration::from_secs(1)),
            &baseline,
            Some(&mut session),
        )
        .await
        .unwrap();
        assert_eq!(report.standard_references.len(), 3);
        assert!(
            report
                .standard_references
                .iter()
                .all(|r| r.cards.len() == 1)
        );
        let prompts = prompts.lock().unwrap();
        assert_eq!(prompts.len(), 3);
        assert!(
            prompts
                .iter()
                .all(|p| p.contains("Тестовая область применения"))
        );
        assert_eq!(session.total_requests(), 2);
    }

    #[tokio::test]
    async fn compares_each_unique_key_and_records_incomplete_search() {
        let index = MarkdownIndex::new("ГОСТ 9013-59 и ГОСТ 9013-59. ГОСТ 7777-20.").unwrap();
        let snapshot: Catalog = serde_json::from_str(include_str!(
            "../../tests/fixtures/consistency/catalog.json"
        ))
        .unwrap();
        let list = "Найдено: <strong>1</strong><table><a href=\"/gost/details/00000000-0000-0000-0000-000000000000\"><span>ГОСТ 9013-59</span></a></table>";
        let card = "<!-- Бейдж статуса --><span><span></span>Действует</span>\n</div><div><span>Обозначение</span><p class=\"value\">ГОСТ 9013-59</p></div><div><span>Полное обозначение</span><p class=\"value\">ГОСТ 9013-59</p></div><div><span>Заглавие на русском языке</span><p class=\"value\">Изменённое название</p></div><div><span>Область применения</span><p class=\"value\">Область</p></div>";
        let mut session = CatalogHttp::with_transport(
            Fake(Mutex::new(vec![
                response("invalid"),
                response(card),
                response(list),
            ])),
            5,
            2,
            "2026-11-13",
        );
        let result = compare(&index, &snapshot, &mut session, "2026-11-13").await;
        assert_eq!(result["keys"].as_array().unwrap().len(), 2);
        assert_eq!(result["requests"], 3);
        assert_eq!(result["keys"][0]["search_complete"], true);
        assert!(
            result["keys"][0]["changed_fields"][0]["fields"]
                .as_array()
                .unwrap()
                .contains(&json!("title"))
        );
        assert_eq!(result["keys"][1]["search_complete"], false);
        assert_eq!(result["keys"][1]["outcome"], "InvalidMarkup");
    }
}
