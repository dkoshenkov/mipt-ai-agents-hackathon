//! Offline, synthetic replay. The scripted extraction recognizes only the supplied demo wording.
//! It is not a general-purpose model and must not be used to assess arbitrary documents.
use procurement_review::{
    consistency::{
        ReviewOptions,
        catalog_http::{CatalogHttp, Response, Transport},
        priority::{Selection, UserResponse},
        review_with_catalog,
    },
    core::{Document, ErrorCategory, Visibility},
    runtime::{Runtime, grok::Model},
};
use serde_json::{Value, json};
use std::{env, fs, time::Duration};

struct OfflineCatalog;
impl Transport for OfflineCatalog {
    fn get<'a>(
        &'a self,
        _: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, ()>> + Send + 'a>>
    {
        Box::pin(async { Err(()) })
    }
}
struct DemoReplay;
impl Model for DemoReplay {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let prompt: Value =
            serde_json::from_str(prompt).map_err(|_| ErrorCategory::InvalidResponse)?;
        let field = |name: &str| {
            prompt["data"]
                .as_array()?
                .iter()
                .find(|f| f["name"] == name)
                .map(|f| &f["value"])
        };
        if field("mention").is_some() {
            return Ok(json!({"role":"bibliography","title":null,"statement":null,"method":null,"temperature":null,"caveats":[],"attributed_threshold":null,"official_excerpt":null,"outcome":"insufficient_evidence"}).to_string());
        }
        if let Some(packet) = field("packet") {
            let mut claims = Vec::new();
            for block in packet["blocks"]
                .as_array()
                .ok_or(ErrorCategory::InvalidResponse)?
            {
                if !packet["subjects"]
                    .as_array()
                    .unwrap()
                    .contains(&block["id"])
                {
                    continue;
                }
                for text in block["text"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                {
                    let Some((object, rest)) = text.split_once(" должен иметь массу ровно ")
                    else {
                        continue;
                    };
                    let Some(value) = rest.strip_suffix(" кг при работе.") else {
                        continue;
                    };
                    if !["Лифт А", "Лифт Б"].contains(&object) || !["300", "500"].contains(&value)
                    {
                        continue;
                    }
                    let quote = |s: &str| json!({"block_id":block["id"],"quote":s});
                    claims.push(json!({"statement":quote(text),"object":{"key":object,"evidence":[quote(object)],"alternatives":[]},
                        "property":quote("массу"),"constraint":{"kind":"numeric","operator":quote("ровно"),"value":quote(value),"unit":quote("кг")},
                        "mandatory":quote("должен"),"stage":null,"conditions":[quote("при работе")]}));
                }
            }
            return Ok(json!({"claims":claims,"priority_rules":[],"standard_mentions":[],"refinements":[]}).to_string());
        }
        if field("mentions").is_some() {
            return Ok(json!({"rules":[{"mention":0,"source":0,"source_reference":"ведомость","object":"лифт","property":"масса","conditions":[],"effective_date":null,"previous_source":null,"transfer_entire_requirement":false}]}).to_string());
        }
        if let Some(candidate) = field("candidate") {
            let action = match candidate["category"].as_str() {
                Some("drawing") => "needs_review",
                Some("dismissed_demo") => "dismissed",
                _ => "confirmed",
            };
            return Ok(json!({"candidate_id":candidate["id"],"version":candidate["evidence"][0]["version"],"action":action,"question":"Какой чертёж применяется?",
                "explanation":if action == "dismissed" { "Значения относятся к разным режимам." } else if action == "needs_review" { "Для согласования нужен чертёж." } else { "Для одного объекта заданы разные точные значения." },"evidence":(0..candidate["evidence"].as_array().unwrap().len()).collect::<Vec<_>>()}).to_string());
        }
        Err(ErrorCategory::InvalidResponse)
    }
}

async fn run(source: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let document = Document {
        markdown: source.into(),
        visibility: Visibility::Public,
    };
    let runtime = Runtime::new(DemoReplay, Duration::from_secs(1));
    let catalog = serde_json::from_str(include_str!("../tests/fixtures/consistency/catalog.json"))?;
    let options = ReviewOptions {
        assessment_date: Some("2026-11-13"),
        ..Default::default()
    };
    let mut catalog_session = CatalogHttp::with_transport(OfflineCatalog, 20, 2, "2026-11-13");
    let initial = review_with_catalog(
        &document,
        options,
        &runtime,
        &catalog,
        Some(&mut catalog_session),
    )
    .await
    .map_err(|_| "empty document")?;
    let answer = initial
        .selections
        .iter()
        .find_map(|(_, selection)| match selection {
            Selection::AskUser { target, .. } if initial.claims.len() == 2 => Some(UserResponse {
                visibility: Visibility::Public,
                target: target.clone(),
                change: initial.claims[1].statement.evidence.clone(),
                use_requirement: initial.claims[0].statement.evidence.clone(),
                revise_priority: false,
                provenance: "Синтетическое решение заказчика".into(),
            }),
            _ => None,
        });
    let after_answer = if let Some(answer) = answer.as_ref() {
        Some(
            review_with_catalog(
                &document,
                ReviewOptions {
                    user_response: Some(answer),
                    ..options
                },
                &runtime,
                &catalog,
                Some(&mut CatalogHttp::with_transport(
                    OfflineCatalog,
                    20,
                    2,
                    "2026-11-13",
                )),
            )
            .await
            .map_err(|_| "empty document")?,
        )
    } else {
        None
    };
    let mut output = json!({"mode":"synthetic scripted replay; not a general model", "initial":initial,"after_answer":after_answer, "independent_cases": independent_cases(&runtime).await?});
    for report in ["initial", "after_answer"] {
        if let Some(report) = output[report].as_object_mut() {
            report.remove("run_id");
        }
    }
    Ok(output)
}

// Small, explicitly scripted cases exercise the same public investigation and selection APIs.
async fn independent_cases(
    runtime: &Runtime<DemoReplay>,
) -> Result<Value, Box<dyn std::error::Error>> {
    use procurement_review::{
        consistency::{
            claims::{Claim, Constraint, Grounded, Object, Operator},
            investigation::{Candidate, investigate},
            markdown::MarkdownIndex,
            priority::{self, ChoiceContext},
        },
        runtime::ReviewRun,
    };
    let index = MarkdownIndex::new("ведомость лифт масса ровно 300 кг должен\n\nлифт масса ровно 500 кг должен\n\nДля лифт масса приоритет ведомость.\n\nСогласовать по чертежу.\n\nЗначения относятся к разным режимам.").map_err(|_| "empty case")?;
    let g = |id, text: &str| Grounded {
        text: text.into(),
        evidence: index.resolve(id, text).unwrap(),
    };
    let claim = |id, value| Claim {
        complete: true,
        statement: g(
            id,
            if id == 0 {
                "ведомость лифт масса ровно 300 кг должен"
            } else {
                "лифт масса ровно 500 кг должен"
            },
        ),
        object: Object {
            key: Some("лифт".into()),
            evidence: vec![g(id, "лифт")],
            alternatives: vec![],
        },
        property: g(id, "масса"),
        constraint: Constraint::Numeric {
            operator: Operator::Exact,
            operator_evidence: g(id, "ровно"),
            value: g(id, value),
            unit: Some(g(id, "кг")),
        },
        stage: None,
        conditions: vec![],
        mandatory: Some(g(id, "должен")),
    };
    let claims = vec![claim(0, "300"), claim(1, "500")];
    let candidate = |category: &str, extra: Option<Grounded>| Candidate {
        id: category.into(),
        category: category.into(),
        object: "лифт".into(),
        property: "масса".into(),
        evidence: claims
            .iter()
            .map(|c| c.statement.evidence.clone())
            .chain(extra.into_iter().map(|g| g.evidence))
            .collect(),
        context: vec![],
        known_gaps: vec![],
    };
    let run = ReviewRun::new(10);
    let confirmed = investigate(
        runtime,
        &run,
        &index,
        Visibility::Public,
        candidate("numeric", None),
        &[],
        0,
    )
    .await;
    let mentions = [g(2, "Для лифт масса приоритет ведомость.")];
    let choose = |mentions| ChoiceContext {
        index: &index,
        visibility: Visibility::Public,
        claims: &claims,
        mentions,
        assessment_date: Some("2026-11-13"),
        selection_read_error: None,
    };
    let without_rule = priority::select(runtime, &run, &confirmed, None, choose(&[])).await;
    let with_rule = priority::select(runtime, &run, &confirmed, None, choose(&mentions)).await;
    let drawing = investigate(
        runtime,
        &run,
        &index,
        Visibility::Public,
        candidate("drawing", Some(g(3, "Согласовать по чертежу."))),
        &[],
        0,
    )
    .await;
    let dismissed = investigate(
        runtime,
        &run,
        &index,
        Visibility::Public,
        candidate(
            "dismissed_demo",
            Some(g(4, "Значения относятся к разным режимам.")),
        ),
        &[],
        0,
    )
    .await;
    Ok(
        json!({"source":index.source,"without_rule":without_rule,"with_rule":with_rule,"drawing":drawing,"dismissed":dismissed}),
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    let source = match args.as_slice() {
        [] => include_str!("../tests/fixtures/consistency/demo/paragraphs.md").to_owned(),
        [path] => fs::read_to_string(path)?,
        _ => {
            return Err("usage: cargo run --example review_markdown -- [synthetic-demo.md]".into());
        }
    };
    let supported = [
        include_str!("../tests/fixtures/consistency/demo/paragraphs.md"),
        include_str!("../tests/fixtures/consistency/demo/list.md"),
        include_str!("../tests/fixtures/consistency/demo/table.md"),
    ];
    if !supported.contains(&source.as_str()) {
        return Err("This replay only supports the supplied demo fixtures; it is not a general review model".into());
    }
    println!("{}", serde_json::to_string_pretty(&run(&source).await?)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn equivalent_layouts_and_changed_object() {
        for source in [
            include_str!("../tests/fixtures/consistency/demo/paragraphs.md"),
            include_str!("../tests/fixtures/consistency/demo/list.md"),
            include_str!("../tests/fixtures/consistency/demo/table.md"),
        ] {
            let output = run(source).await.unwrap();
            assert_eq!(
                output["initial"]["investigations"]
                    .as_array()
                    .unwrap()
                    .len(),
                1,
                "{output}"
            );
            assert!(
                output["initial"]["investigations"][0]["outcome"]["Ok"]["Confirmed"].is_object()
            );
            assert!(output["initial"]["selections"][0][1]["AskUser"].is_object());
            assert!(output["after_answer"]["patch_verification"]["Incomplete"].is_object());
            assert_eq!(
                output["initial"]["catalog_trace"][0]["outcome"],
                "NetworkError"
            );
            assert!(
                !output["initial"]["standard_references"][0]["cards"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert!(output["independent_cases"]["without_rule"]["AskUser"].is_object());
            assert!(output["independent_cases"]["with_rule"]["Selected"].is_object());
            assert!(
                output["independent_cases"]["drawing"]["outcome"]["Ok"]["NeedsReview"].is_object()
            );
            assert!(
                output["independent_cases"]["dismissed"]["outcome"]["Ok"]["Dismissed"].is_object()
            );
            assert_eq!(output, run(source).await.unwrap());
            for claim in output["initial"]["claims"].as_array().unwrap() {
                assert_eq!(claim["conditions"][0]["text"], "при работе");
                let e = &claim["statement"]["evidence"]["range"];
                let a = e["start"].as_u64().unwrap() as usize;
                let b = e["end"].as_u64().unwrap() as usize;
                assert_eq!(&source[a..b], claim["statement"]["text"].as_str().unwrap());
            }
            for evidence in output["initial"]["investigations"][0]["cited_evidence"]
                .as_array()
                .unwrap()
            {
                let a = evidence["range"]["start"].as_u64().unwrap() as usize;
                let b = evidence["range"]["end"].as_u64().unwrap() as usize;
                assert!(source.get(a..b).is_some_and(|s| !s.is_empty()));
            }
            let changed = source.replace(
                "Лифт А должен иметь массу ровно 500",
                "Лифт Б должен иметь массу ровно 500",
            );
            assert!(
                run(&changed).await.unwrap()["initial"]["investigations"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
