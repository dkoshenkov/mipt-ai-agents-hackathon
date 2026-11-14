use procurement_review::{
    core::{Document, ErrorCategory, Visibility},
    runtime::{
        Runtime,
        grok::Model,
        tz_review::{reader, review},
    },
};
use serde_json::{Value, json};
use std::{sync::Mutex, time::Duration};
struct Local(Mutex<Vec<Value>>);
impl Model for Local {
    async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
        Ok(self.0.lock().unwrap().remove(0).to_string())
    }
}
#[tokio::test]
async fn neighbors() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/tz_astra_neighbors.json")).unwrap();
    for (index, mut c) in cases.into_iter().enumerate() {
        c["expected"] = json!(match index {
            2 | 3 | 6 => "error",
            4 | 5 | 8 => "требует проверки",
            _ => "dismissed",
        });
        replay(&c).await;
    }
}
#[tokio::test]
async fn scope_and_lift_regressions() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/tz_scope.json")).unwrap();
    for c in cases {
        replay(&c).await;
    }
}
async fn replay(c: &Value) {
    let plan = json!({"regime":"applicable","reason":"Документация 223-ФЗ","line":1,"assignments":c["assignments"],"candidates":[]});
    let step = json!({"reviewed":[0,1],"candidates":[c["candidate"].clone()]});
    let mut d = c["decision"].as_object().unwrap().clone();
    let old_verdict = d.remove("verdict").unwrap();
    let missing = d.remove("missing").unwrap();
    d.insert(
        "observation".into(),
        json!(if old_verdict == "dismissed" {
            "no_risk_indicator"
        } else {
            match c["assignments"][0]["id"].as_str().unwrap() {
                "A03" => "competition_restriction",
                "A05" => "description_defect",
                "A06" => "commercial_identifier",
                other => panic!("unexpected fixture check {other}"),
            }
        }),
    );
    d.insert(
        "missing_facts".into(),
        if old_verdict == "needs_review" && missing.as_str().is_some_and(|s| !s.is_empty()) {
            json!([missing])
        } else {
            json!([])
        },
    );
    let out = json!({"decisions":[d]});
    let rt = Runtime::new(
        Local(Mutex::new(vec![plan, step, out])),
        Duration::from_secs(1),
    );
    let doc = Document {
        markdown: c["doc"].as_str().unwrap().into(),
        visibility: Visibility::Public,
    };
    let r = review(&rt, &doc, &[], 7).await.unwrap();
    let outcome = r
        .coverage
        .iter()
        .find(|o| o.stage == "critic")
        .expect("critic must run");
    let actual = r
        .findings
        .first()
        .map(|f| f.status)
        .unwrap_or(outcome.state);
    assert_eq!(
        actual,
        c["expected"].as_str().unwrap(),
        "{}: {:?}",
        c["label"],
        r.coverage
    );
    if c["assignments"][0]["id"] == "A05" && !r.findings.is_empty() {
        let basis = c["decision"]["basis"].as_str().unwrap();
        assert_eq!(
            serde_json::to_value(&r).unwrap()["findings"][0]["basis"],
            basis
        );
        let expected_line = format!("Основание: {basis}");
        assert!(reader::notes(&r).lines().any(|line| line == expected_line));
    }
    if c["label"] == "A05: обязательное поле одной позиции" {
        assert_eq!(r.findings[0].source.line, 5);
        assert_eq!(r.findings[0].additional_sources[0].line, 4);
    }
    println!(
        "CASE {} => {:?}; {:?}",
        c["label"].as_str().unwrap(),
        r.findings
            .iter()
            .map(|f| (
                f.status,
                f.scope.as_str(),
                f.fact.as_str(),
                f.procedure_source
                    .as_ref()
                    .map(|s| (s.line, s.quote.as_str()))
            ))
            .collect::<Vec<_>>(),
        r.coverage
            .iter()
            .filter(|o| o.stage == "critic")
            .map(|o| (&o.state, &o.detail))
            .collect::<Vec<_>>()
    );
}
