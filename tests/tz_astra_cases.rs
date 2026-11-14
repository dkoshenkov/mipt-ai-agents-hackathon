use procurement_review::{
    core::{Document, ErrorCategory, Visibility},
    runtime::{Runtime, grok::Model, tz_review::review},
};
use serde_json::{Value, json};
use std::{sync::Mutex, time::Duration};
struct Local(Mutex<Vec<Value>>);
impl Model for Local {
    async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
        Ok(self.0.lock().unwrap().remove(0).to_string())
    }
}
#[allow(clippy::too_many_arguments)]
async fn case(
    label: &str,
    id: &str,
    doc: &str,
    claim: &str,
    original: usize,
    target: usize,
    quote: &str,
    context: Vec<usize>,
    plan_context: Vec<usize>,
    status: &str,
    proc_line: Option<(usize, &str)>,
    extra_evidence: Option<(usize, &str)>,
) {
    let mut ev = vec![json!({"line":target,"quote":quote})];
    let mut lines = vec![target];
    if let Some((line, q)) = extra_evidence {
        ev.push(json!({"line":line,"quote":q}));
        lines.push(line);
    }
    let plan = json!({"regime":"applicable","reason":"Применяется 223-ФЗ","line":1,
      "assignments":[{"id":id,"scope":"позиция 1","lines":[original],"reason":"проверить","context":plan_context}],"candidates":[]});
    let mut step = json!({"reviewed":[0,1,2],"candidates":[{"scope":"позиция 1","lines":[original],"claim":claim,"basis":"основание применения","wording":null}]});
    // Include all result fields in the real pipeline even in negative cases.
    step["reviewed"] = json!([0, 1, 2, 3, 4]);
    let dec = json!({"decisions":[{"index":0,"observation":if id=="A03" {"competition_restriction"} else {"commercial_identifier"},"lines":lines,"context":context,"evidence":ev,
        "explanation":"проверено","basis":"223-ФЗ ст. 3","missing_facts":if status=="needs_review" {vec!["материалы"]} else {vec![]},"correction_reason":"ссылка уточнена","procedure_source":proc_line.map(|(line,q)|json!({"line":line,"quote":q}))}]});
    let runtime = Runtime::new(
        Local(Mutex::new(vec![plan, step, dec])),
        Duration::from_secs(1),
    );
    let doc = Document {
        markdown: doc.into(),
        visibility: Visibility::Public,
    };
    let r = review(&runtime, &doc, &[], 7).await.unwrap();
    assert_eq!(r.decisions.len(), 1, "{label}: critic did not run");
    let expected = match label {
        "A06 unrelated equivalent"
        | "A06 negated equivalent"
        | "A06 additional invented relation"
        | "A06 column field" => Some("Alpha"),
        "A03 clear ban" => Some("Запрещается поставка эквивалентов"),
        _ => None,
    };
    assert_eq!(
        r.findings.len(),
        usize::from(expected.is_some()),
        "{label}: {:?}",
        r.coverage
    );
    if let Some(fact) = expected {
        assert_eq!(r.findings[0].fact, fact, "{label}");
        assert_eq!(r.findings[0].status, "требует проверки", "{label}");
    }
    if label != "A06 cross-position" {
        assert!(
            !r.coverage
                .iter()
                .any(|c| c.stage == "critic" && c.state == "error"),
            "{label}: technical errors cannot pass negative cases"
        );
    }
    println!(
        "{label} => {:?} {:?}",
        r.findings
            .iter()
            .map(|f| (
                f.status,
                f.scope.as_str(),
                f.fact.as_str(),
                f.procedure_source.as_ref().map(|s| s.quote.as_str())
            ))
            .collect::<Vec<_>>(),
        r.coverage
            .iter()
            .filter(|o| o.stage == "critic")
            .map(|o| (&o.state, &o.detail))
            .collect::<Vec<_>>()
    );
}
#[tokio::test]
async fn replay_cases() {
    case("A06 cross-position","A06","Процедура по 223-ФЗ\n| Позиция 1 | Насос без указания марки |\n| Позиция 2 | Производитель: Beta |","Позиция 1 без указания эквивалента",2,3,"Beta",vec![],vec![],"needs_review",None,None).await;
    case("A06 procedure","A06","Документация 223-ФЗ\nСпособ: закупка у единственного поставщика\n| 1 | Производитель: Alpha |","Предписан Alpha",3,3,"Alpha",vec![],vec![],"confirmed",Some((2,"закупка у единственного поставщика")),None).await;
    case("A06 unrelated equivalent","A06","Документация 223-ФЗ\n| Позиция 1 | Производитель: Alpha |\n| Позиция 2 | Модель | Bravo или эквивалент |","Позиция 1: Alpha",2,2,"Alpha",vec![3],vec![],"needs_review",None,None).await;
    case("A06 omitted equivalent","A06","Документация 223-ФЗ\nДля всех позиций допускаются эквиваленты.\n| 1 | Производитель: Alpha |","Позиция 1: Alpha",3,3,"Alpha",vec![],vec![2],"needs_review",None,None).await;
    case(
        "A06 negated equivalent",
        "A06",
        "Документация 223-ФЗ\n| Позиция 1 | Производитель: Alpha | Не допускаются эквиваленты |",
        "Позиция 1: Alpha",
        2,
        2,
        "Alpha",
        vec![],
        vec![],
        "needs_review",
        None,
        None,
    )
    .await;
    case("A03 quote polarity","A03","Документация 223-ФЗ\nТребование «эквиваленты не допускаются» исключено. Разрешены аналоги.","Эквиваленты запрещены",2,2,"эквиваленты не допускаются",vec![],vec![],"needs_review",None,None).await;
    case(
        "A03 clear ban",
        "A03",
        "Документация 223-ФЗ\nЗапрещается поставка эквивалентов.",
        "Эквиваленты запрещены",
        2,
        2,
        "Запрещается поставка эквивалентов",
        vec![],
        vec![],
        "needs_review",
        None,
        None,
    )
    .await;
    case("A03 common conforming requirement","A03","Документация 223-ФЗ\nДопускается только продукция, соответствующая настоящему техническому заданию.","Положение содержит ограничение",2,2,"Допускается только продукция, соответствующая настоящему техническому заданию",vec![],vec![],"needs_review",None,None).await;
    case(
        "A06 additional invented relation",
        "A06",
        "Документация 223-ФЗ\n| Позиция 1 | Производитель: Alpha |\nПозиция 2: поставка в январе.",
        "Позиция 1: Alpha",
        2,
        2,
        "Alpha",
        vec![],
        vec![],
        "needs_review",
        None,
        Some((3, "январе")),
    )
    .await;
    case(
        "A06 column field",
        "A06",
        "Документация 223-ФЗ\n| Позиция | Производитель |\n| --- | --- |\n| 1 | Alpha |",
        "Позиция 1: Alpha",
        4,
        4,
        "Alpha",
        vec![2],
        vec![2],
        "needs_review",
        None,
        None,
    )
    .await;
}
