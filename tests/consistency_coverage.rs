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
    fail_last_control: bool,
    control_subjects: Arc<Mutex<Vec<usize>>>,
}

struct TableFailure(Option<&'static str>);

impl Model for TableFailure {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let request: Value = serde_json::from_str(prompt).unwrap();
        if self.0.is_some_and(|marker| {
            request["data"].as_array().unwrap().iter().any(|field| {
                field["name"] == "packet"
                    && field["value"]["blocks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|block| {
                            field["value"]["subjects"]
                                .as_array()
                                .unwrap()
                                .contains(&block["id"])
                                && block["text"][0].as_str().unwrap().contains(marker)
                        })
            })
        }) {
            return Err(ErrorCategory::RateLimited);
        }
        let response = Replay {
            fail_last_control: false,
            control_subjects: Arc::default(),
        }
        .complete(prompt)
        .await?;
        let mut response: Value = serde_json::from_str(&response).unwrap();
        if let Some(packet) = request["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "packet")
        {
            for block in packet["value"]["blocks"].as_array().unwrap() {
                if packet["value"]["subjects"]
                    .as_array()
                    .unwrap()
                    .contains(&block["id"])
                {
                    let text = block["text"][0].as_str().unwrap();
                    if text.contains("приоритет ведомости") {
                        response["priority_rules"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!({"block_id":block["id"], "quote":text}));
                    }
                }
            }
        }
        Ok(response.to_string())
    }
}

#[tokio::test]
async fn empty_complete_report_differs_from_unread_table_and_preserves_neighbor() {
    let table =
        "# Таблица\n\n| Позиция | Примечание |\n| --- | --- |\n| UNREAD TABLE | нет данных |";
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    for (fail, neighbor) in [(false, false), (true, false), (true, true)] {
        let document = Document {
            markdown: if neighbor {
                format!(
                    "# Соседний раздел\n\nЛифт А должен иметь массу ровно 300 кг.\n\nЛифт А должен иметь массу ровно 500 кг.\n\n{}\n\n{table}",
                    "filler ".repeat(300)
                )
            } else {
                table.into()
            },
            visibility: Visibility::Public,
        };
        let index =
            procurement_review::consistency::markdown::MarkdownIndex::new(&document.markdown)
                .unwrap();
        let unread = index
            .blocks
            .iter()
            .find(|block| {
                document.markdown[block.range.clone()].contains("UNREAD TABLE")
                    && block.kind == procurement_review::consistency::markdown::BlockKind::TableCell
            })
            .unwrap()
            .id;
        let report = review_markdown(
            &document,
            ReviewOptions {
                packet_bytes: 2500,
                ..Default::default()
            },
            &Runtime::new(
                TableFailure(fail.then_some("UNREAD TABLE")),
                Duration::from_secs(1),
            ),
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(
            report
                .packets
                .iter()
                .any(|packet| packet.stop.is_some() && packet.subjects.contains(&unread)),
            fail
        );
        assert_eq!(report.investigations.len(), usize::from(neighbor));
        if !fail {
            assert!(
                report
                    .packets
                    .iter()
                    .all(|p| p.stop.is_none() && p.binding_issues.is_empty())
            );
            assert!(report.oversized_blocks.is_empty() && report.parsing_gaps.is_empty());
        }
        if neighbor {
            assert!(matches!(
                report.investigations[0].outcome,
                Ok(Verdict::Confirmed { .. })
            ));
        } else {
            assert!(report.selections.is_empty());
        }
    }
}

#[tokio::test]
async fn unavailable_priority_and_linked_note_stop_different_decisions() {
    use procurement_review::consistency::{investigation::Incomplete, markdown::MarkdownIndex};
    let mut markdown = String::from(
        "# Требования\n\n| Требование |\n| --- |\n| Лифт А должен иметь массу ровно 300 кг. |\n",
    );
    for i in 0..20 {
        markdown.push_str(&format!("| Строка {i}: {} |\n", "текст ".repeat(12)));
    }
    markdown.push_str(
        "| Примечание: NOTE LIMIT первое требование действует только при испытаниях. |\n\n",
    );
    markdown.push_str(&format!("{}\n\nЛифт А должен иметь массу ровно 500 кг.\n\nДля массы Лифт А приоритет ведомости.\n\n{}\n\n# Ведомость\n\n| Позиция | Значение |\n| --- | --- |\n| PRIORITY ROW: Лифт А | триста кг |", "filler ".repeat(250), "filler ".repeat(250)));
    let document = Document {
        markdown,
        visibility: Visibility::Public,
    };
    let index = MarkdownIndex::new(&document.markdown).unwrap();
    let id = |text| {
        index
            .blocks
            .iter()
            .find(|b| index.resolve(b.id, text).is_some())
            .unwrap()
            .id
    };
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    for marker in ["PRIORITY ROW", "NOTE LIMIT"] {
        let report = review_markdown(
            &document,
            ReviewOptions {
                packet_bytes: 2500,
                ..Default::default()
            },
            &Runtime::new(TableFailure(Some(marker)), Duration::from_secs(1)),
            &catalog,
        )
        .await
        .unwrap();
        assert!(!report.priority_mentions.is_empty());
        assert!(
            report
                .packets
                .iter()
                .any(|p| p.stop.is_some() && p.subjects.contains(&id(marker)))
        );
        assert_eq!(
            report.investigations.len(),
            1,
            "{marker}: {:?}",
            report.packets
        );
        if marker == "PRIORITY ROW" {
            assert!(matches!(
                report.investigations[0].outcome,
                Ok(Verdict::Confirmed { .. })
            ));
            assert!(matches!(
                report.selections[0].1,
                Selection::Unverified { .. }
            ));
        } else {
            assert!(
                matches!(&report.investigations[0].outcome, Err(Incomplete::UnreadContext(gaps)) if gaps.contains(&id(marker)))
            );
            assert!(matches!(report.selections[0].1, Selection::NotApplicable));
        }
    }
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
            let control = field("existing_claims").is_some();
            let subjects = packet["subjects"].as_array().unwrap();
            if control {
                self.control_subjects
                    .lock()
                    .unwrap()
                    .extend(subjects.iter().map(|v| v.as_u64().unwrap() as usize));
                if self.fail_last_control
                    && packet["blocks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|b| b["text"][0].as_str().unwrap().contains("LAST BLOCK"))
                {
                    return Err(ErrorCategory::RateLimited);
                }
            }
            let mut claims = Vec::new();
            let mut standards = Vec::new();
            for block in packet["blocks"].as_array().unwrap() {
                if !subjects.contains(&block["id"]) {
                    continue;
                }
                let text = block["text"][0].as_str().unwrap();
                if control && text.contains("ГОСТ 1234-2020") {
                    standards.push(json!({"block_id":block["id"],"quote":"ГОСТ 1234-2020"}));
                }
                let Some(value) = ["300", "500"].into_iter().find(|n| text.contains(n)) else {
                    continue;
                };
                let quote = |s: &str| json!({"block_id":block["id"],"quote":s});
                claims.push(json!({"statement":quote(text),"object":{"key":"Лифт А","evidence":[quote("Лифт А")],"alternatives":[]},"property":quote("массу"),"constraint":{"kind":"numeric","operator":quote("ровно"),"value":quote(value),"unit":quote("кг")},"mandatory":quote("должен"),"stage":null,"conditions":[]}));
            }
            return Ok(json!({"claims":claims,"priority_rules":[],"standard_mentions":standards,"refinements":[]}).to_string());
        }
        if let Some(candidate) = field("candidate") {
            return Ok(json!({"candidate_id":candidate["id"],"version":candidate["evidence"][0]["version"],"action":"confirmed","explanation":"Different exact values", "evidence":(0..candidate["evidence"].as_array().unwrap().len()).collect::<Vec<_>>()}).to_string());
        }
        Err(ErrorCategory::InvalidResponse)
    }
}

#[tokio::test]
async fn control_reads_long_document_and_reports_failed_subjects_without_losing_conflict() {
    let mut markdown = String::from(
        "# Раздел\n\nЛифт А должен иметь массу ровно 300 кг.\n\nЛифт А должен иметь массу ровно 500 кг.\n\n",
    );
    for i in 0..12 {
        markdown.push_str(&format!("Paragraph {i}: {}\n\n", "filler ".repeat(35)));
    }
    markdown.push_str("LAST BLOCK: ссылка на ГОСТ 1234-2020 в конце документа.");
    let document = Document {
        markdown,
        visibility: Visibility::Public,
    };
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    for fail in [false, true] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::new(
            Replay {
                fail_last_control: fail,
                control_subjects: seen.clone(),
            },
            Duration::from_secs(1),
        );
        let report = review_markdown(
            &document,
            ReviewOptions {
                packet_bytes: 2500,
                ..Default::default()
            },
            &runtime,
            &catalog,
        )
        .await
        .unwrap();
        let last =
            procurement_review::consistency::markdown::MarkdownIndex::new(&document.markdown)
                .unwrap()
                .blocks
                .last()
                .unwrap()
                .id;
        assert!(seen.lock().unwrap().contains(&last));
        assert_eq!(report.investigations.len(), 1);
        assert!(matches!(
            report.investigations[0].outcome,
            Ok(Verdict::Confirmed { .. })
        ));
        if fail {
            assert!(
                report
                    .packets
                    .iter()
                    .any(|p| p.control_pass && p.subjects.contains(&last) && p.stop.is_some())
            );
            assert!(matches!(
                report.selections[0].1,
                Selection::Unverified { .. }
            ));
        } else {
            assert!(report.packets.iter().all(|p| p.stop.is_none()));
            assert!(
                report
                    .recovered_standard_mentions
                    .iter()
                    .any(|g| g.text == "ГОСТ 1234-2020" && g.evidence.block_id == last)
            );
            assert!(matches!(report.selections[0].1, Selection::AskUser { .. }));
        }
    }
}

struct Rebind;

impl Model for Rebind {
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
            let mut refinements = Vec::new();
            let existing = field("existing_claims");
            for block in packet["blocks"].as_array().unwrap() {
                if !packet["subjects"]
                    .as_array()
                    .unwrap()
                    .contains(&block["id"])
                {
                    continue;
                }
                let text = block["text"][0].as_str().unwrap();
                let Some(value) = ["300", "500", "700", "200", "400"]
                    .into_iter()
                    .find(|n| text.contains(n))
                else {
                    continue;
                };
                let quote = |s: &str| json!({"block_id":block["id"],"quote":s});
                let object = if text.contains("архив") {
                    "архив"
                } else if text.contains("журнал") {
                    "журнал"
                } else {
                    "копии"
                };
                let claim = |object: &str| json!({"statement":quote(text),"object":{"key":object,"evidence":[quote(object)],"alternatives":[]},"property":quote("масса"),"constraint":{"kind":"numeric","operator":quote("ровно"),"value":quote(value),"unit":quote("кг")},"mandatory":quote("должен"),"stage":null,"conditions":[]});
                if let Some(existing) = existing {
                    if value == "500" {
                        let claim_index = existing
                            .as_array()
                            .unwrap()
                            .iter()
                            .position(|c| c["statement"]["text"] == text)
                            .unwrap();
                        refinements
                            .push(json!({"claim_index":claim_index,"replacement":claim("копии")}));
                    }
                } else {
                    claims.push(claim(object));
                }
            }
            return Ok(json!({"claims":claims,"priority_rules":[],"standard_mentions":[],"refinements":refinements}).to_string());
        }
        if let Some(candidate) = field("candidate") {
            return Ok(json!({"candidate_id":candidate["id"],"version":candidate["evidence"][0]["version"],"action":"confirmed","explanation":"Different exact values", "evidence":(0..candidate["evidence"].as_array().unwrap().len()).collect::<Vec<_>>()}).to_string());
        }
        Err(ErrorCategory::InvalidResponse)
    }
}

#[tokio::test]
async fn rebind_recomputes_both_groups_and_preserves_independent_conflict() {
    let document = Document { markdown: "журнал должен масса ровно 300 кг\n\nжурнал копии должен масса ровно 500 кг\n\nкопии должен масса ровно 700 кг\n\nархив должен масса ровно 200 кг\n\nархив должен масса ровно 400 кг".into(), visibility: Visibility::Public };
    let runtime = Runtime::new(Rebind, Duration::from_secs(1));
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    let report = review_markdown(&document, ReviewOptions::default(), &runtime, &catalog)
        .await
        .unwrap();
    let initial: Vec<_> = report
        .initial_candidates
        .iter()
        .map(|c| c.object.as_str())
        .collect();
    let final_candidates: Vec<_> = report.investigations.iter().map(|i| &i.candidate).collect();
    assert!(initial.contains(&"журнал") && initial.contains(&"архив"));
    assert!(!final_candidates.iter().any(|c| c.object == "журнал"));
    assert!(final_candidates.iter().any(|c| c.object == "копии"));
    let old_archive = report
        .initial_candidates
        .iter()
        .find(|c| c.object == "архив")
        .unwrap();
    assert!(
        final_candidates
            .iter()
            .any(|c| c.object == "архив" && c.id == old_archive.id)
    );
}
