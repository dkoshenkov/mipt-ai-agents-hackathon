use procurement_review::{
    core::{Document, ErrorCategory, Visibility},
    runtime::{
        Runtime,
        grok::Model,
        tz_review::{Material, reader, review},
    },
};
use serde_json::{Value, json};
use std::{sync::Mutex, time::Duration};
struct Mock(Mutex<Vec<Value>>);
impl Model for Mock {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        assert!(!prompt.contains("PRIVATE_CONTENT"));
        let v: Value = serde_json::from_str(prompt).unwrap();
        let fields = v["data"].as_array().unwrap();
        if fields
            .iter()
            .any(|x| x["name"] == "rule" && x["value"]["id"] == "A03")
        {
            return Err(ErrorCategory::RateLimited);
        }
        Ok(self.0.lock().unwrap().remove(0).to_string())
    }
}
fn runtime(responses: Vec<Value>) -> Runtime<Mock> {
    Runtime::new(Mock(Mutex::new(responses)), Duration::from_secs(1))
}
fn document() -> Document {
    Document{markdown:"# Конкурс\r\n| Позиция | Требование |\n| --- | --- |\n| 1 | Участнику требуется положительная репутация |\n| 2 | Участнику требуется положительная репутация |\n".into(),visibility:Visibility::Public}
}
fn assignment(id: &str) -> Value {
    json!({"id":id,"scope":"Позиция 2","reason":"Условие допуска","lines":[5]})
}
fn plan(regime: &str, ids: &[&str]) -> Value {
    json!({"regime":regime,"reason":"Оценка A01","line":5,"assignments":ids.iter().map(|s|assignment(s)).collect::<Vec<_>>(),"candidates":[]})
}
fn candidate(claim: &str) -> Value {
    json!({"scope":"Позиция 2","claim":claim,"basis":"основание","lines":[5],"wording":"новый текст"})
}
fn decision(i: usize, v: &str, allow: bool) -> Value {
    let observation = if v == "dismissed" {
        "no_risk_indicator"
    } else {
        "participant_admission"
    };
    let missing = if v == "needs_review" {
        vec!["нужен материал"]
    } else {
        vec![]
    };
    json!({"index":i,"evidence":[{"line":5,"quote":"Участнику требуется положительная репутация"}],"lines":[5],"observation":observation,"explanation":format!("решение {i}"),"basis":"223-ФЗ, ст. 3, ч. 1, п. 2","missing_facts":missing,"accept_wording":allow,"wording_reason":"основание формулировки","procedure_source":{"line":1,"quote":"Конкурс"}})
}
#[tokio::test]
async fn independent_checks_and_individual_critic_verdicts() {
    let doc = document();
    let hidden = Document {
        markdown: "PRIVATE_CONTENT".into(),
        visibility: Visibility::Private,
    };
    let m = [Material {
        name: "PRIVATE_CONTENT",
        document: &hidden,
    }];
    let mut scoped = plan("applicable", &["A03", "A04", "A07"]);
    let mut second = assignment("A04");
    second["scope"] = json!("другая позиция");
    scoped["assignments"].as_array_mut().unwrap().push(second);
    let model = runtime(vec![
        scoped,
        json!({"reviewed":[0,1,2],"candidates":(0..5).map(|i|candidate(&i.to_string())).collect::<Vec<_>>()}),
        json!({"decisions":[decision(0,"confirmed",true),decision(1,"confirmed",false),decision(2,"needs_review",true),decision(3,"dismissed",false)]}),
    ]);
    let r = review(&model, &doc, &m, 4).await.unwrap();
    assert_eq!(r.attempts, 4);
    assert_eq!(r.findings.len(), 3);
    assert!(r.findings[0].wording.is_some());
    assert!(r.findings[1..].iter().all(|f| f.wording.is_none()));
    assert_eq!(r.findings[2].status, "требует проверки");
    for f in &r.findings {
        assert_eq!(f.source.line, 5);
        assert_eq!(
            &doc.markdown[f.source.start..f.source.end],
            "Участнику требуется положительная репутация"
        );
        assert!(f.source.table_header.contains("Позиция"));
    }
    assert!(
        r.coverage
            .iter()
            .any(|o| o.check == "A03" && o.state == "error")
    );
    assert!(
        r.coverage
            .iter()
            .any(|o| o.stage == "critic" && o.state == "error")
    );
    assert!(
        r.coverage
            .iter()
            .any(|o| o.check == "A07" && o.state == "rejected")
    );
    assert_eq!(reader::notes(&r).matches("source.html#L5").count(), 3);
    assert!(reader::document_view("test", &doc.markdown).contains("id=\"L5\""));
}
#[tokio::test]
async fn no_feedback_and_explicit_partial_results() {
    let mut doc = document();
    let model = runtime(vec![plan("inapplicable", &["A06"])]);
    let r = review(&model, &doc, &[], 7).await.unwrap();
    assert_eq!(r.attempts, 1);
    assert!(r.findings.is_empty());
    assert!(r.plan.is_empty());
    let model = runtime(vec![
        plan("uncertain", &["A04"]),
        json!({"reviewed":[0,1,2],"candidates":[candidate("есть риск")]}),
    ]);
    let r = review(&model, &doc, &[], 2).await.unwrap();
    assert_eq!(r.attempts, 2);
    assert!(
        r.coverage
            .iter()
            .any(|o| o.stage == "critic" && o.detail.contains("BudgetExhausted"))
    );
    doc.visibility = Visibility::Unknown;
    let r = review(&runtime(vec![]), &doc, &[], 2).await.unwrap();
    assert_eq!(r.attempts, 0);
    doc.markdown = "  ".into();
    assert!(review(&runtime(vec![]), &doc, &[], 2).await.is_err());
}
#[tokio::test]
async fn needs_review_and_equivalent_inherited_from_group() {
    let mut doc = document();
    doc.markdown="# Комплект\n| Компонент | Референс |\n|---|---|\n| Комплект | Допускаются эквиваленты для всех приборов |\n| Весы | Референс: AB |\n".into();
    let mut p = plan("uncertain", &[]);
    p["line"] = Value::Null;
    let model = runtime(vec![p, json!({"reviewed":[0,1,2],"candidates":[]})]);
    let r = review(&model, &doc, &[], 7).await.unwrap();
    assert_eq!(r.plan[0].id, "A06");
    assert_eq!(r.attempts, 2);
    assert!(r.findings.is_empty());
}

#[tokio::test]
async fn confirmation_needs_evidence_from_an_available_material() {
    let mut doc = document();
    doc.markdown = doc.markdown.replace(
        "| 2 | Участнику требуется положительная репутация |",
        "| Производитель: Завод X |",
    );
    doc.markdown.push_str("| Производитель: Завод Y |\n");
    let appendix = Document {
        markdown: "Извещение. Открытый конкурс.".into(),
        visibility: Visibility::Public,
    };
    let materials = [Material {
        name: "извещение.md",
        document: &appendix,
    }];
    let mut p = plan("applicable", &["A06"]);
    p["assignments"][0]["materials"] = json!(["извещение.md"]);
    let mut supported = decision(0, "confirmed", true);
    supported["observation"] = json!("commercial_identifier");
    supported["evidence"][0]["quote"] = json!("Завод X");
    supported["procedure_source"] = json!({"line":1,"quote":"Открытый конкурс"});
    supported["procedure_material"] = json!("извещение.md");
    let mut unsupported = decision(1, "confirmed", true);
    unsupported["observation"] = json!("commercial_identifier");
    unsupported["lines"] = json!([6]);
    unsupported["evidence"] = json!([{"line":6,"quote":"Завод Y"}]);
    unsupported["correction_reason"] = json!("Точное поле второй позиции");
    unsupported["procedure_source"] = Value::Null;
    let model = runtime(vec![
        p,
        json!({"reviewed":[0,1,2],"candidates":[candidate("первое"),candidate("второе")]}),
        json!({"decisions":[supported,unsupported]}),
    ]);
    let r = review(&model, &doc, &materials, 7).await.unwrap();
    assert_eq!(r.findings.len(), 1);
    assert_eq!(
        r.findings[0].procedure_material.as_deref(),
        Some("извещение.md")
    );
    assert_eq!(r.findings[0].status, "подтверждено");
    assert!(
        r.coverage
            .iter()
            .any(|o| o.stage == "critic" && o.state == "error")
    );
}

#[tokio::test]
async fn correction_keeps_both_addresses_and_unreviewed_positions_stay_visible() {
    let mut p = plan("uncertain", &["A04", "A04", "A04"]);
    p["assignments"][0]["lines"] = json!([2, 5]);
    p["assignments"][0]["context"] = json!([2]);
    p["assignments"][1]["scope"] = json!("позиция 1");
    p["assignments"][1]["lines"] = json!([4]);
    p["assignments"][2]["lines"] = json!([300]);
    let mut c = candidate("Условие дано в позиции 2");
    c["lines"] = json!([2]);
    let mut correct = decision(0, "needs_review", false);
    correct["correction_reason"] = json!("Условие позиции 2 видно в строке 5");
    correct["context"] = json!([2]);
    let mut invalid = correct.clone();
    invalid["index"] = json!(1);
    invalid["lines"] = json!([600]);
    let model = runtime(vec![
        p,
        json!({"reviewed":[0],"candidates":[c,c]}),
        json!({"decisions":[correct,invalid]}),
    ]);
    let r = review(&model, &document(), &[], 7).await.unwrap();
    assert_eq!(r.plan.len(), 2);
    assert_eq!(r.positions[0].state, "reviewed");
    assert_eq!(r.positions[1].state, "unacknowledged");
    assert!(r.coverage.iter().any(|o| o.state == "rejected"
        && o.detail.contains("300")
        && o.detail.contains("Позиция 2")));
    assert_eq!(r.findings.len(), 1);
    assert_eq!(r.findings[0].original_lines, [2]);
    assert_eq!(r.findings[0].source.line, 5);
    assert_eq!(r.decisions[0].reviewed_lines, [5]);
    assert!(reader::notes(&r).contains("source.html#L2"));
}

#[tokio::test]
async fn invalid_applicability_address_does_not_drop_independent_assignments() {
    let mut p = plan("applicable", &["A04"]);
    p["line"] = json!(990);
    let model = runtime(vec![p, json!({"reviewed":[0],"candidates":[]})]);
    let r = review(&model, &document(), &[], 7).await.unwrap();
    assert_eq!(
        r.regime,
        Some(procurement_review::runtime::tz_review::Regime::Uncertain)
    );
    assert_eq!(r.positions[0].state, "reviewed");
    assert!(
        r.coverage
            .iter()
            .any(|o| o.check == "A01" && o.detail.contains("990"))
    );
}

#[tokio::test]
async fn planner_filters_only_existing_blank_lines_before_dispatch() {
    let mut doc = document();
    doc.markdown.push_str("\r\n \t\r\n| Тип |  |\r\n");
    let mut p = plan("uncertain", &[]);
    let assignment = |scope: &str, lines: &[usize], context: &[usize]| json!({"id":"A05","scope":scope,"reason":"Описание предмета","lines":lines,"context":context});
    p["assignments"] = json!([
        assignment("Лифт", &[8, 6, 5], &[7, 2]),
        assignment("Лифт", &[8, 5, 6], &[2, 7]),
        assignment("Только контекст", &[6, 7], &[2]),
        assignment("Несуществующая строка", &[8, 9], &[]),
        assignment("Нулевой контекст", &[8], &[0]),
    ]);
    let model = runtime(vec![p, json!({"reviewed":[0],"candidates":[]})]);
    let r = review(&model, &doc, &[], 7).await.unwrap();
    assert_eq!(r.plan.len(), 1);
    assert_eq!(r.plan[0].lines, [8, 5]);
    assert_eq!(r.plan[0].context, [2]);
    assert_eq!(r.positions[0].state, "reviewed");
    assert_eq!(r.attempts, 2);
    let filtered = r
        .coverage
        .iter()
        .find(|o| o.state == "filtered" && o.detail.contains("Лифт"))
        .unwrap();
    assert!(filtered.detail.contains("основные [6]"));
    assert!(filtered.detail.contains("контекст [7]"));
    let rejected: Vec<_> = r
        .coverage
        .iter()
        .filter(|o| o.state == "rejected")
        .collect();
    assert_eq!(rejected.len(), 3);
    for (scope, reason) in [
        ("Только контекст", "нет непустых основных строк"),
        ("Несуществующая строка", "[9]"),
        ("Нулевой контекст", "[0]"),
    ] {
        assert!(
            rejected
                .iter()
                .any(|o| o.detail.contains(scope) && o.detail.contains(reason))
        );
    }
}

#[tokio::test]
async fn two_exact_quotes_can_share_one_reviewed_line() {
    let mut doc = document();
    doc.markdown
        .push_str("| Принтер Model X | Производитель: Завод X |\n");
    let mut p = plan("uncertain", &["A06"]);
    p["assignments"][0]["lines"] = json!([6]);
    let mut c = candidate("Принтер Model X");
    c["lines"] = json!([5, 6]);
    let mut d = decision(0, "needs_review", false);
    d["observation"] = json!("commercial_identifier");
    d["lines"] = json!([6]);
    d["evidence"] = json!([{"line":6,"quote":"Принтер Model X"},{"line":6,"quote":"Завод X"}]);
    let runtime = runtime(vec![
        p,
        json!({"reviewed":[0],"candidates":[c]}),
        json!({"decisions":[d]}),
    ]);
    let report = review(&runtime, &doc, &[], 7).await.unwrap();
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].fact, "Завод X");
}

#[tokio::test]
async fn product_parameter_cannot_be_published_as_a_participant_requirement() {
    let mut doc = document();
    let fact = "Точность позиционирования X/Y: 5.08 μm";
    doc.markdown = doc
        .markdown
        .replace("Участнику требуется положительная репутация", fact);
    for observation in ["no_risk_indicator", "participant_admission"] {
        let mut d = decision(0, "needs_review", false);
        d["observation"] = json!(observation);
        d["evidence"] = json!([{"line":5,"quote":fact}]);
        let model = runtime(vec![
            plan("uncertain", &["A04"]),
            json!({"reviewed":[0],"candidates":[candidate("характеристика принтера")]}),
            json!({"decisions":[d]}),
        ]);
        let r = review(&model, &doc, &[], 7).await.unwrap();
        assert!(r.findings.is_empty());
        assert!(
            r.coverage
                .iter()
                .any(|o| o.stage == "critic" && o.state == "dismissed")
        );
    }
}

#[tokio::test]
async fn legacy_verdict_is_not_a_publishable_observation() {
    let mut d = decision(0, "confirmed", false);
    d.as_object_mut().unwrap().remove("observation");
    d["verdict"] = json!("confirmed");
    let model = runtime(vec![
        plan("uncertain", &["A04"]),
        json!({"reviewed":[0],"candidates":[candidate("условие")]}),
        json!({"decisions":[d]}),
    ]);
    let r = review(&model, &document(), &[], 7).await.unwrap();
    assert!(r.findings.is_empty());
    assert!(
        r.coverage
            .iter()
            .any(|o| o.stage == "critic" && o.state == "error")
    );
}
