mod qualification_support;
use procurement_review::qualification::{sources::SnapshotHttp, *};
use qualification_support::*;
use serde_json::json;

#[tokio::test]
async fn parameter_specific_evidence_and_a_new_radius_change_only_the_range_result() {
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            let b = blocks(data, "packet");
            let mut radius = numeric(&b, "Templates R7–R25", "range", "7", "mm", Operator::Range);
            if let Constraint::Numeric { upper, .. } = &mut radius.constraint {
                *upper = Some("25".into());
            }
            return Ok(extracted(vec![
                numeric(
                    &b,
                    "division <= 0,1 g",
                    "division",
                    "0,1",
                    "g",
                    Operator::AtMost,
                ),
                numeric(
                    &b,
                    "Memory >= 1000 records",
                    "memory",
                    "1000",
                    "records",
                    Operator::AtLeast,
                ),
                radius,
            ]));
        }
        let property = data["current_requirement"]["property"].as_str().unwrap();
        Ok(match property {
            "division" => assessed(
                data,
                Assessment::Insufficient,
                "Method: distinguish 0,02 g at working load.",
            ),
            "memory" => assessed(
                data,
                Assessment::PossiblyExcessive,
                "Task: maximum series including reserve is 120 records, then export; no other modes.",
            ),
            _ => {
                if blocks(data, "blocks")
                    .iter()
                    .any(|b| b.text.contains("R3 is required"))
                {
                    assessed(data, Assessment::Insufficient, "Drawing: R3 is required.")
                } else {
                    assessed(
                        data,
                        Assessment::Justified,
                        "Drawing: both R7 and R25 are required.",
                    )
                }
            }
        })
    });
    let text = "division <= 0,1 g\nMemory >= 1000 records\nTemplates R7–R25\nMethod: distinguish 0,02 g at working load.\nTask: maximum series including reserve is 120 records, then export; no other modes.\nDrawing: both R7 and R25 are required.";
    let before = review_equipment_adequacy(
        &input(text),
        &QualificationOptions::default(),
        &runtime(worker.clone()),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    let after = review_equipment_adequacy(
        &input(&format!("{text}\nDrawing: R3 is required.")),
        &QualificationOptions::default(),
        &runtime(worker),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert!(before.coverage.complete());
    assert!(after.coverage.complete());
    assert!(matches!(
        record(&before, "division").outcome,
        RequirementOutcome::Assessed {
            assessment: Assessment::Insufficient,
            ..
        }
    ));
    assert!(matches!(
        record(&before, "memory").outcome,
        RequirementOutcome::Assessed {
            assessment: Assessment::PossiblyExcessive,
            ..
        }
    ));
    assert!(matches!(
        record(&before, "range").outcome,
        RequirementOutcome::Assessed {
            assessment: Assessment::Justified,
            ..
        }
    ));
    assert!(matches!(
        record(&after, "range").outcome,
        RequirementOutcome::Assessed {
            assessment: Assessment::Insufficient,
            ..
        }
    ));
    for property in ["division", "memory"] {
        let (
            RequirementOutcome::Assessed {
                assessment: a,
                operation: op_a,
                reason: reason_a,
                evidence: ea,
                ..
            },
            RequirementOutcome::Assessed {
                assessment: b,
                operation: op_b,
                reason: reason_b,
                evidence: eb,
                ..
            },
        ) = (
            &record(&before, property).outcome,
            &record(&after, property).outcome,
        )
        else {
            panic!("missing assessment")
        };
        assert_eq!((a, op_a, reason_a), (b, op_b, reason_b));
        assert_eq!(
            ea.iter().map(|s| &s.quote).collect::<Vec<_>>(),
            eb.iter().map(|s| &s.quote).collect::<Vec<_>>()
        );
        assert_ne!(ea[0].version, eb[0].version);
    }
}

#[tokio::test]
async fn repeated_technical_fact_finishes_without_a_search_or_an_infinite_loop() {
    let mut input = input("Bandwidth >= 50 MHz");
    input.attachments.push(Material::markdown(
        "passport",
        "| Parameter | A | B |\n| Bandwidth MHz | 50 | 100 |",
        procurement_review::core::Visibility::Public,
    ));
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            return Ok(extracted(vec![numeric(
                &blocks(data, "packet"),
                "Bandwidth >= 50 MHz",
                "bandwidth",
                "50",
                "MHz",
                Operator::AtLeast,
            )]));
        }
        if instruction.starts_with("Verify") {
            let b = blocks(data, "source_blocks");
            return Ok(json!(FactVerification {
                primary_source: true,
                reason: "Original passport".into(),
                fact: Some(FactProposal {
                    model: quote(&b, "A"),
                    parameter: quote(&b, "Bandwidth"),
                    value: quote(&b, "50"),
                    unit: Some(quote(&b, "MHz")),
                    conditions: vec![]
                })
            }));
        }
        Ok(json!(Decision::TechnicalFact {
            requirement_id: data["current_requirement"]["id"].as_str().unwrap().into(),
            target: FactTarget {
                model: "A".into(),
                parameter: "Bandwidth".into(),
                numeric: true,
                conditions: vec![]
            },
            query: "Same fact in other words".into()
        }))
    });
    let report = review_equipment_adequacy(
        &input,
        &QualificationOptions::default(),
        &runtime(worker.clone()),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert_eq!(worker.count(), 4);
    assert_eq!(worker.search_count(), 0);
    assert_eq!(report.requirements[0].facts.len(), 1);
    assert!(matches!(
        report.requirements[0].outcome,
        RequirementOutcome::Pending { question: None, .. }
    ));
}

#[tokio::test]
async fn extraction_retains_a_range_and_an_alternative_as_two_conditions() {
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            let b = blocks(data, "packet");
            let mut range = numeric(&b, "R7–R25", "range", "7", "mm", Operator::Range);
            if let Constraint::Numeric { upper, .. } = &mut range.constraint {
                *upper = Some("25".into());
            }
            let mut alternatives =
                qualitative(&b, "GigE Vision or USB3 Vision", "interface", "placeholder");
            alternatives.constraint = Constraint::Alternatives {
                values: vec!["GigE Vision".into(), "USB3 Vision".into()],
            };
            return Ok(extracted(vec![range, alternatives]));
        }
        Ok(ask(data, "working task"))
    });
    let report = review_equipment_adequacy(
        &input("Templates R7–R25\nGigE Vision or USB3 Vision"),
        &QualificationOptions::default(),
        &runtime(worker),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert_eq!(report.requirements.len(), 2);
    assert!(report.coverage.complete());
    assert!(matches!(
        record(&report, "range").requirement.constraint,
        Constraint::Numeric {
            operator: Operator::Range,
            upper: Some(_),
            ..
        }
    ));
    assert!(
        matches!(&record(&report,"interface").requirement.constraint,Constraint::Alternatives {values} if values.len()==2)
    );
}
