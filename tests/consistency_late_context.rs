use std::time::Duration;

use procurement_review::{
    consistency::{
        ReviewOptions,
        investigation::{Incomplete, Verdict},
        markdown::MarkdownIndex,
        review_markdown,
    },
    core::{Document, ErrorCategory, Visibility},
    runtime::{Runtime, grok::Model},
};
use serde_json::{Value, json};

struct LateContext {
    fail_control: bool,
    note: usize,
}

impl Model for LateContext {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let request: Value = serde_json::from_str(prompt).unwrap();
        let field = |name: &str| {
            request["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == name)
                .map(|f| &f["value"])
        };
        if let Some(packet) = field("packet") {
            let control = field("existing_claims");
            if control.is_some()
                && self.fail_control
                && packet["subjects"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(self.note))
            {
                return Err(ErrorCategory::RateLimited);
            }
            let mut claims = Vec::new();
            let mut refinements = Vec::new();
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
                let original = if text.contains("архив") {
                    "архив"
                } else if text.contains("журнал") {
                    "журнал"
                } else {
                    "копии"
                };
                let claim = |object: &str| json!({"statement":quote(text),"object":{"key":object,"evidence":[quote(object)],"alternatives":[]},"property":quote("масса"),"constraint":{"kind":"numeric","operator":quote("ровно"),"value":quote(value),"unit":quote("кг")},"mandatory":quote("должен"),"stage":null,"conditions":[]});
                if let Some(existing) = control {
                    if value == "500" && text.contains("оговорка: речь идёт о резерве")
                    {
                        let i = existing
                            .as_array()
                            .unwrap()
                            .iter()
                            .position(|c| c["statement"]["text"] == text)
                            .unwrap();
                        refinements.push(json!({"claim_index":i,"replacement":claim("копии")}));
                    }
                } else {
                    claims.push(claim(original));
                }
            }
            return Ok(json!({"claims":claims,"priority_rules":[],"standard_mentions":[],"refinements":refinements}).to_string());
        }
        if let Some(candidate) = field("candidate") {
            if self.fail_control && candidate["object"] == "журнал" {
                return Ok(json!({"candidate_id":candidate["id"],"version":candidate["evidence"][0]["version"],"action":"read","block_ids":[self.note]}).to_string());
            }
            return Ok(json!({"candidate_id":candidate["id"],"version":candidate["evidence"][0]["version"],"action":"confirmed","explanation":"Different exact values","evidence":(0..candidate["evidence"].as_array().unwrap().len()).collect::<Vec<_>>()}).to_string());
        }
        Err(ErrorCategory::InvalidResponse)
    }
}

#[tokio::test]
async fn late_rebinding_updates_groups_and_failed_read_keeps_independent_result() {
    let mut markdown = String::from(
        "# Независимый раздел\n\nархив должен масса ровно 200 кг\n\nархив должен масса ровно 400 кг\n\n# Длинный раздел\n\nжурнал должен масса ровно 300 кг\n\nкопии должен масса ровно 700 кг\n\n",
    );
    for i in 0..15 {
        markdown.push_str(&format!("Строка {i}: {}\n\n", "текст ".repeat(40)));
    }
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    for (has_note, fail) in [(true, false), (true, true), (false, false)] {
        let document = Document {
            markdown: format!(
                "{markdown}журнал копии должен масса ровно 500 кг{}",
                if has_note {
                    " — оговорка: речь идёт о резерве"
                } else {
                    ""
                }
            ),
            visibility: Visibility::Public,
        };
        let index = MarkdownIndex::new(&document.markdown).unwrap();
        let note = index.blocks.last().unwrap().id;
        let report = review_markdown(
            &document,
            ReviewOptions {
                packet_bytes: 2500,
                read_bytes: 1,
                ..Default::default()
            },
            &Runtime::new(
                LateContext {
                    fail_control: fail,
                    note,
                },
                Duration::from_secs(1),
            ),
            &catalog,
        )
        .await
        .unwrap();
        assert!(report.packets.iter().filter(|p| p.control_pass).count() > 1);
        assert!(
            report
                .packets
                .iter()
                .any(|p| p.control_pass && p.subjects.contains(&note) && p.stop.is_some() == fail)
        );
        let archive = report
            .investigations
            .iter()
            .find(|i| i.candidate.object == "архив")
            .unwrap();
        assert!(matches!(archive.outcome, Ok(Verdict::Confirmed { .. })));
        assert!(
            report
                .initial_candidates
                .iter()
                .any(|c| c.id == archive.candidate.id)
        );
        assert!(
            report
                .initial_candidates
                .iter()
                .any(|c| c.object == "журнал")
        );
        if fail {
            assert!(matches!(
                report
                    .investigations
                    .iter()
                    .find(|i| i.candidate.object == "журнал")
                    .unwrap()
                    .outcome,
                Err(Incomplete::InvalidRead)
            ));
        } else if !has_note {
            assert!(matches!(
                report
                    .investigations
                    .iter()
                    .find(|i| i.candidate.object == "журнал")
                    .unwrap()
                    .outcome,
                Ok(Verdict::Confirmed { .. })
            ));
        } else {
            assert!(
                !report
                    .investigations
                    .iter()
                    .any(|i| i.candidate.object == "журнал")
            );
            let copies = report
                .investigations
                .iter()
                .find(|i| i.candidate.object == "копии")
                .unwrap();
            assert!(matches!(copies.outcome, Ok(Verdict::Confirmed { .. })));
            assert!(copies.candidate.evidence.iter().any(|e| e.block_id == note));
            assert!(
                copies
                    .candidate
                    .evidence
                    .iter()
                    .any(|e| index.source[e.range.clone()].contains("700"))
            );
            assert!(copies.candidate.context.iter().any(|e| e.block_id == note));
        }
    }
}
