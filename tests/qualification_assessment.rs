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

#[tokio::test]
async fn mixed_decode_and_evidence_failures_share_retries_and_preserve_final_review() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (budget, expected_calls, recovered) in [(6, 4, true), (5, 3, false)] {
        let n = AtomicUsize::new(0);
        let worker = Script::new(move |stage, data| {
            if stage.starts_with("Extract") {
                return Ok(extracted(vec![qualitative(
                    &blocks(data, "packet"),
                    "Required camera",
                    "camera",
                    "present",
                )]));
            }
            Ok(match n.fetch_add(1, Ordering::SeqCst) {
                0 => json!({"unexpected":true}),
                1 => assessed(data, Assessment::Justified, "Required camera"),
                2 => {
                    let mut reply = ask(data, "task");
                    reply["requirement_id"] = json!("unknown");
                    reply
                }
                _ => ask(data, "task"),
            })
        });
        let report = review_equipment_adequacy(
            &input("Required camera"),
            &QualificationOptions {
                model_attempts: budget,
                ..Default::default()
            },
            &runtime(worker.clone()),
            &reviewer(),
            &SnapshotHttp::default(),
        )
        .await
        .unwrap();
        assert_eq!(worker.count(), 1 + expected_calls);
        assert_eq!(report.coverage.review, ReviewStatus::Completed);
        assert_eq!(report.model_attempts, budget);
        assert_eq!(report.questions().count(), usize::from(recovered));
        assert_eq!(
            report
                .coverage
                .gaps
                .iter()
                .filter(|g| g.failure.stage == "assessment")
                .count(),
            usize::from(!recovered)
        );
        let calls = worker.calls.lock().unwrap();
        assert!(
            calls[3]
                .to_string()
                .contains("independent operational basis")
        );
    }
}

#[tokio::test]
async fn partial_extraction_retries_without_losing_or_duplicating_valid_requirements() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = AtomicUsize::new(0);
    let worker = Script::new(move |stage, data| {
        if stage.starts_with("Extract") {
            let packet = blocks(data, "packet");
            let valid = qualitative(&packet, "Required camera", "camera", "present");
            let mut other = qualitative(&packet, "Required lens", "lens", "present");
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                other.evidence[0].quote = "Wrong citation".into();
                return Ok(extracted(vec![valid, other]));
            }
            return Ok(extracted(vec![other]));
        }
        Ok(ask(data, "operating mode"))
    });
    let report = review_equipment_adequacy(
        &input("Required camera\nRequired lens"),
        &QualificationOptions::default(),
        &runtime(worker.clone()),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert!(report.coverage.complete());
    assert_eq!(
        report
            .requirements
            .iter()
            .map(|r| r.requirement.property.as_str())
            .collect::<Vec<_>>(),
        ["camera", "lens"]
    );
    assert_eq!(worker.count(), 4);
}

#[tokio::test]
async fn retries_and_reads_leave_first_attempts_for_later_requirements() {
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            let b = blocks(data, "packet");
            return Ok(extracted(vec![
                numeric(
                    &b,
                    "Capacity >= 2 kg",
                    "capacity",
                    "2",
                    "kg",
                    Operator::AtLeast,
                ),
                numeric(
                    &b,
                    "Memory >= 1000 records",
                    "memory",
                    "1000",
                    "records",
                    Operator::AtLeast,
                ),
            ]));
        }
        if data["current_requirement"]["property"] == "capacity" {
            return Ok(json!(Decision::Read {
                requirement_id: data["current_requirement"]["id"].as_str().unwrap().into(),
                block_ids: vec![blocks(data, "blocks")[0].id.clone()],
            }));
        }
        Ok(ask(data, "maximum records before export"))
    });
    let report = review_equipment_adequacy(
        &input("Capacity >= 2 kg\nMemory >= 1000 records"),
        &QualificationOptions {
            model_attempts: 4,
            ..Default::default()
        },
        &runtime(worker),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert!(matches!(
        record(&report, "memory").outcome,
        RequirementOutcome::Pending {
            question: Some(_),
            ..
        }
    ));
    assert!(matches!(report.coverage.review, ReviewStatus::Completed));
    assert_eq!(report.model_attempts, 4);
}

#[tokio::test]
async fn invalid_response_retries_do_not_take_later_requirements_first_attempt() {
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            let b = blocks(data, "packet");
            return Ok(extracted(vec![
                numeric(
                    &b,
                    "Capacity >= 2 kg",
                    "capacity",
                    "2",
                    "kg",
                    Operator::AtLeast,
                ),
                numeric(
                    &b,
                    "Memory >= 1000 records",
                    "memory",
                    "1000",
                    "records",
                    Operator::AtLeast,
                ),
            ]));
        }
        if data["current_requirement"]["property"] == "capacity" {
            return Ok(ask(data, ""));
        }
        Ok(ask(data, "maximum records before export"))
    });
    let report = review_equipment_adequacy(
        &input("Capacity >= 2 kg\nMemory >= 1000 records"),
        &QualificationOptions {
            model_attempts: 4,
            ..Default::default()
        },
        &runtime(worker),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert!(matches!(
        record(&report, "memory").outcome,
        RequirementOutcome::Pending {
            question: Some(_),
            ..
        }
    ));
    assert!(matches!(report.coverage.review, ReviewStatus::Completed));
    assert_eq!(report.model_attempts, 4);
}
