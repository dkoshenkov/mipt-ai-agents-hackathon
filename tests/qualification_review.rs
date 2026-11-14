mod qualification_support;
use procurement_review::{
    core::{ErrorCategory, Visibility},
    qualification::{sources::SnapshotHttp, *},
};
use qualification_support::*;
use serde_json::json;

#[tokio::test]
async fn single_review_recovers_omitted_conditions_even_after_empty_extraction() {
    for empty in [true, false] {
        for method in [true, false] {
            let text = if method {
                "Capacity >= 2 kg; division <= 0,1 g.\nTask: weigh samples up to 1,5 kg.\nMethod: distinguish 0,02 g."
            } else {
                "Capacity >= 2 kg; division <= 0,1 g.\nTask: weigh samples up to 1,5 kg."
            };
            let worker = Script::new(move |instruction, data| {
                if instruction.starts_with("Extract") {
                    return Ok(extracted(if empty {
                        vec![]
                    } else {
                        vec![numeric(
                            &blocks(data, "packet"),
                            "Capacity >= 2 kg",
                            "capacity",
                            "2",
                            "kg",
                            Operator::AtLeast,
                        )]
                    }));
                }
                match data["current_requirement"]["property"].as_str().unwrap() {
                    "capacity" => Ok(assessed(
                        data,
                        Assessment::Justified,
                        "Task: weigh samples up to 1,5 kg.",
                    )),
                    "division" if method => Ok(assessed(
                        data,
                        Assessment::Insufficient,
                        "Method: distinguish 0,02 g.",
                    )),
                    _ => Ok(ask(data, "minimum distinguishable mass difference")),
                }
            });
            let review = Script::new(move |instruction, data| {
                assert!(instruction.contains("ONLY final review"));
                let b = blocks(data, "blocks");
                let mut response = no_change_review(data);
                let drafts = if empty {
                    vec![
                        (
                            "capacity",
                            numeric(
                                &b,
                                "Capacity >= 2 kg",
                                "capacity",
                                "2",
                                "kg",
                                Operator::AtLeast,
                            ),
                        ),
                        (
                            "division",
                            numeric(
                                &b,
                                "division <= 0,1 g",
                                "division",
                                "0,1",
                                "g",
                                Operator::AtMost,
                            ),
                        ),
                    ]
                } else {
                    vec![(
                        "division",
                        numeric(
                            &b,
                            "division <= 0,1 g",
                            "division",
                            "0,1",
                            "g",
                            Operator::AtMost,
                        ),
                    )]
                };
                for (id, draft) in drafts {
                    let evidence = draft.evidence.clone();
                    let block_id = evidence[0].block_id.clone();
                    response.notes.push(ReviewNote {
                        id: id.into(),
                        reason: "Independent condition was omitted".into(),
                        evidence: evidence.clone(),
                        change: ReviewChange::Add { requirement: draft },
                    });
                    let row = response
                        .blocks
                        .iter_mut()
                        .find(|r| r.block_id == block_id)
                        .unwrap();
                    row.no_requirements_reason = None;
                    row.conditions.push(ConditionMapping {
                        condition: evidence[0].clone(),
                        requirement_id: None,
                        note_id: Some(id.into()),
                    });
                }
                Ok(json!(response))
            });
            let report = review_equipment_adequacy(
                &input(text),
                &QualificationOptions::default(),
                &runtime(worker.clone()),
                &runtime(review.clone()),
                &SnapshotHttp::default(),
            )
            .await
            .unwrap();
            assert_eq!(review.count(), 1);
            assert_eq!(report.requirements.len(), 2);
            assert_eq!(record(&report, "capacity").requirement.id, "requirement-1");
            assert!(report.coverage.complete(), "{:?}", report.coverage.gaps);
            assert_eq!(report.questions().count(), usize::from(!method));
            assert!(report.notes.iter().all(|n| n.applied));
        }
    }
}

#[tokio::test]
async fn merge_keeps_first_id_restores_alternatives_and_closes_old_questions() {
    let text = "GigE Vision or USB3 Vision (either is allowed).\nShutter must be global.\nTask: either interface supports the existing acquisition system.";
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            let b = blocks(data, "packet");
            return Ok(extracted(vec![
                qualitative(&b, "GigE Vision", "interface", "GigE Vision"),
                qualitative(&b, "USB3 Vision", "interface", "USB3 Vision"),
                qualitative(&b, "Shutter must be global", "shutter", "global"),
            ]));
        }
        if data["current_requirement"]["property"] == "interface"
            && data["current_requirement"]["constraint"]["kind"] == "qualitative"
        {
            Ok(ask(data, "required interface"))
        } else {
            Ok(assessed(
                data,
                Assessment::Justified,
                "Task: either interface supports the existing acquisition system.",
            ))
        }
    });
    let review = Script::new(|_, data| {
        let b = blocks(data, "blocks");
        let mut response = no_change_review(data);
        let mut draft = qualitative(
            &b,
            "GigE Vision or USB3 Vision (either is allowed).",
            "interface",
            "placeholder",
        );
        draft.constraint = Constraint::Alternatives {
            values: vec!["GigE Vision".into(), "USB3 Vision".into()],
        };
        response.notes.push(ReviewNote {
            id: "merge-interface".into(),
            reason: "One alternative condition was split".into(),
            evidence: draft.evidence.clone(),
            change: ReviewChange::Merge {
                requirement_ids: vec!["requirement-2".into(), "requirement-1".into()],
                requirement: draft.clone(),
            },
        });
        let row = response
            .blocks
            .iter_mut()
            .find(|r| r.block_id == draft.evidence[0].block_id)
            .unwrap();
        row.conditions = vec![ConditionMapping {
            condition: draft.evidence[0].clone(),
            requirement_id: None,
            note_id: Some("merge-interface".into()),
        }];
        Ok(json!(response))
    });
    let report = review_equipment_adequacy(
        &input(text),
        &QualificationOptions::default(),
        &runtime(worker),
        &runtime(review.clone()),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert_eq!(review.count(), 1);
    assert_eq!(report.requirements.len(), 2);
    assert_eq!(record(&report, "interface").requirement.id, "requirement-1");
    assert_eq!(record(&report, "shutter").requirement.id, "requirement-3");
    assert_eq!(report.closed_questions.len(), 2);
    assert_eq!(report.questions().count(), 0);
    assert!(report.coverage.complete(), "{:?}", report.coverage.gaps);
    assert_eq!(report.notes[0].original_ids.len(), 2);
    assert_eq!(
        report.notes[0].resulting_id.as_deref(),
        Some("requirement-1")
    );
}

#[tokio::test]
async fn reviewer_removes_a_reference_column_without_changing_the_required_column() {
    let mut source = input("Position 13: bandwidth is mandatory only in column A of passport.");
    source.attachments.push(Material::markdown(
        "passport",
        "| Parameter | A | B |\n| Bandwidth | 50 MHz | 100 MHz |",
        Visibility::Public,
    ));
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            let b = blocks(data, "packet");
            let doc = blocks(data, "specification");
            let mut a = numeric(&b, "50 MHz", "bandwidth A", "50", "MHz", Operator::AtLeast);
            a.position = "13".into();
            a.reference = Some(quote(
                &doc,
                "bandwidth is mandatory only in column A of passport",
            ));
            let mut b = numeric(
                &b,
                "100 MHz",
                "bandwidth B",
                "100",
                "MHz",
                Operator::AtLeast,
            );
            b.position = "13".into();
            b.reference = a.reference.clone();
            return Ok(extracted(vec![a, b]));
        }
        Ok(ask(data, "required measurement operation"))
    });
    let review = Script::new(|_, data| {
        let mut response = no_change_review(data);
        let b = blocks(data, "blocks");
        response.notes.push(ReviewNote {
            id: "remove-B".into(),
            reason: "The reference applies only to column A".into(),
            evidence: vec![quote(&b, "100 MHz")],
            change: ReviewChange::Remove {
                requirement_id: "requirement-2".into(),
            },
        });
        for row in &mut response.blocks {
            row.conditions
                .retain(|c| c.requirement_id.as_deref() != Some("requirement-2"));
            if row.conditions.is_empty() {
                row.no_requirements_reason = Some("Reference only".into());
            }
        }
        Ok(json!(response))
    });
    let report = review_equipment_adequacy(
        &source,
        &QualificationOptions::default(),
        &runtime(worker),
        &runtime(review),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert_eq!(report.requirements.len(), 1);
    assert_eq!(report.requirements[0].requirement.id, "requirement-1");
    assert_eq!(report.questions().count(), 1);
    assert_eq!(report.closed_questions.len(), 1);
    assert!(report.coverage.complete());
    assert!(report.notes[0].applied);
    assert_eq!(report.notes[0].original_ids, ["requirement-2"]);
}

#[tokio::test]
async fn invalid_review_edits_keep_records_and_partial_coverage() {
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            let b = blocks(data, "packet");
            return Ok(extracted(vec![
                qualitative(&b, "GigE Vision", "interface", "GigE Vision"),
                qualitative(&b, "USB3 Vision", "interface", "USB3 Vision"),
            ]));
        }
        Ok(ask(data, "required interface"))
    });
    let review = Script::new(|_, data| {
        let mut response = no_change_review(data);
        let mut evidence = quote(&blocks(data, "blocks"), "GigE Vision");
        evidence.quote = "invented source".into();
        response.notes.push(ReviewNote {
            id: "invalid".into(),
            reason: "Remove".into(),
            evidence: vec![evidence],
            change: ReviewChange::Remove {
                requirement_id: "requirement-1".into(),
            },
        });
        Ok(json!(response))
    });
    let result = review_equipment_adequacy(
        &input("GigE Vision or USB3 Vision"),
        &QualificationOptions::default(),
        &runtime(worker),
        &runtime(review.clone()),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert_eq!(review.count(), 1);
    assert_eq!(result.requirements.len(), 2);
    assert_eq!(result.questions().count(), 2);
    assert!(!result.notes[0].applied);
    assert!(!result.coverage.complete());
}

#[tokio::test]
async fn contradictory_reconciliation_cannot_remove_a_requirement_or_close_its_question() {
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            return Ok(extracted(vec![numeric(
                &blocks(data, "packet"),
                "Capacity >= 2 kg",
                "capacity",
                "2",
                "kg",
                Operator::AtLeast,
            )]));
        }
        Ok(ask(data, "working load"))
    });
    let review = Script::new(|_, data| {
        let mut response = no_change_review(data);
        response.notes.push(ReviewNote {
            id: "remove".into(),
            reason: "Reference only".into(),
            evidence: vec![quote(&blocks(data, "blocks"), "Capacity >= 2 kg")],
            change: ReviewChange::Remove {
                requirement_id: "requirement-1".into(),
            },
        });
        // The condition still maps to the record the same response tries to remove.
        Ok(json!(response))
    });
    let report = review_equipment_adequacy(
        &input("Capacity >= 2 kg"),
        &QualificationOptions::default(),
        &runtime(worker),
        &runtime(review),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert_eq!(report.requirements.len(), 1);
    assert_eq!(report.requirements[0].requirement.id, "requirement-1");
    assert_eq!(report.questions().count(), 1);
    assert!(report.closed_questions.is_empty());
    assert!(!report.notes[0].applied);
    assert!(report.notes[0].resulting_id.is_none());
    assert!(!report.coverage.complete());
    assert!(!report.coverage.blocks[0].reviewed);
    assert!(
        report
            .coverage
            .gaps
            .iter()
            .any(|g| g.failure.stage == "reconcile")
    );
}

#[tokio::test]
async fn failed_reassessment_keeps_the_original_question_answerable() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for failure in [ErrorCategory::RateLimited, ErrorCategory::TimedOut] {
        let attempts = Arc::new(AtomicUsize::new(0));
        let calls = attempts.clone();
        let worker = Script::new(move |instruction, data| {
            if instruction.starts_with("Extract") {
                return Ok(extracted(vec![numeric(
                    &blocks(data, "packet"),
                    "Capacity >= 2 kg",
                    "capacity",
                    "2",
                    "kg",
                    Operator::AtLeast,
                )]));
            }
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(ask(data, "working load"))
            } else {
                Err(failure)
            }
        });
        let review = Script::new(|_, data| {
            let mut response = no_change_review(data);
            response.notes.push(ReviewNote {
                id: "reassess".into(),
                reason: "Recheck the unknown load".into(),
                evidence: vec![quote(&blocks(data, "blocks"), "Capacity >= 2 kg")],
                change: ReviewChange::Reassess {
                    requirement_id: "requirement-1".into(),
                },
            });
            Ok(json!(response))
        });
        let source = input("Capacity >= 2 kg");
        let options = QualificationOptions::default();
        let report = review_equipment_adequacy(
            &source,
            &options,
            &runtime(worker),
            &runtime(review),
            &SnapshotHttp::default(),
        )
        .await
        .unwrap();
        assert_eq!(report.questions().count(), 1);
        assert!(report.closed_questions.is_empty());
        assert!(!report.coverage.complete());
        let (requirement, question) = report.questions().next().unwrap();
        let question = question.clone();
        assert_eq!(question.id, "question-2");
        let answer = Clarification {
            question_id: question.id.clone(),
            requirement_id: requirement.id.clone(),
            document_version: report.materials[0].version.clone(),
            text: "Working load is 1.5 kg".into(),
            visibility: Visibility::Public,
        };
        for fail_after_answer in [false, true] {
            let worker = Script::new(move |_, data| {
                assert_eq!(data["answers"][0]["text"], "Working load is 1.5 kg");
                if fail_after_answer {
                    Err(ErrorCategory::TimedOut)
                } else {
                    Ok(assessed(data, Assessment::Justified, "Capacity >= 2 kg"))
                }
            });
            let ResumeOutcome::Updated { report: updated } = resume_equipment_adequacy(
                &source,
                report.clone(),
                answer.clone(),
                &options,
                &runtime(worker),
                &SnapshotHttp::default(),
            )
            .await
            else {
                panic!("Original question must remain answerable")
            };
            assert_eq!(updated.questions().count(), 0);
            assert_eq!(
                updated.closed_questions.as_slice(),
                std::slice::from_ref(&question)
            );
            assert_eq!(updated.clarifications.len(), 1);
            assert_eq!(
                matches!(
                    updated.requirements[0].outcome,
                    RequirementOutcome::Stopped { .. }
                ),
                fail_after_answer
            );
        }
    }
}

#[tokio::test]
async fn read_evidence_survives_late_failure_and_wrong_requirement_ids_are_rejected() {
    for wrong_id in [false, true] {
        let worker = Script::new(move |instruction, data| {
            if instruction.starts_with("Extract") {
                return Ok(extracted(vec![numeric(
                    &blocks(data, "packet"),
                    "Capacity >= 2 kg",
                    "capacity",
                    "2",
                    "kg",
                    Operator::AtLeast,
                )]));
            }
            if wrong_id {
                let mut decision = ask(data, "operation");
                decision["requirement_id"] = "other".into();
                return Ok(decision);
            }
            if data["read_evidence"].as_array().unwrap().is_empty() {
                return Ok(json!(Decision::Read {
                    requirement_id: data["current_requirement"]["id"].as_str().unwrap().into(),
                    block_ids: vec![quote(&blocks(data, "blocks"), "Task").block_id]
                }));
            }
            Err(ErrorCategory::TimedOut)
        });
        let result = review_equipment_adequacy(
            &input("Capacity >= 2 kg\nTask: see missing drawing."),
            &QualificationOptions::default(),
            &runtime(worker),
            &reviewer(),
            &SnapshotHttp::default(),
        )
        .await
        .unwrap();
        assert!(matches!(
            result.requirements[0].outcome,
            RequirementOutcome::Stopped { .. }
        ));
        assert_eq!(result.questions().count(), 0);
        assert_eq!(!result.requirements[0].read_evidence.is_empty(), !wrong_id);
        assert!(!result.coverage.complete());
    }
}

#[tokio::test]
async fn addressed_reassessment_uses_the_method_or_keeps_a_single_open_question() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for method in [true, false] {
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let worker = Script::new(move |instruction, data| {
            if instruction.starts_with("Extract") {
                return Ok(extracted(vec![numeric(
                    &blocks(data, "packet"),
                    "division <= 0,1 g",
                    "division",
                    "0,1",
                    "g",
                    Operator::AtMost,
                )]));
            }
            let attempt = calls.fetch_add(1, Ordering::SeqCst);
            if !method {
                return Ok(ask(data, "minimum distinguishable mass difference"));
            }
            Ok(assessed(
                data,
                if attempt == 0 {
                    Assessment::Justified
                } else {
                    Assessment::Insufficient
                },
                "Method: distinguish 0,02 g.",
            ))
        });
        let review = Script::new(|_, data| {
            let mut response = no_change_review(data);
            response.notes.push(ReviewNote {
                id: "reassess".into(),
                reason: "Check the division against the task method".into(),
                evidence: vec![quote(&blocks(data, "blocks"), "division <= 0,1 g")],
                change: ReviewChange::Reassess {
                    requirement_id: "requirement-1".into(),
                },
            });
            Ok(json!(response))
        });
        let text = if method {
            "division <= 0,1 g\nMethod: distinguish 0,02 g."
        } else {
            "division <= 0,1 g"
        };
        let report = review_equipment_adequacy(
            &input(text),
            &QualificationOptions::default(),
            &runtime(worker),
            &runtime(review.clone()),
            &SnapshotHttp::default(),
        )
        .await
        .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(review.count(), 1);
        assert_eq!(report.requirements[0].requirement.id, "requirement-1");
        assert_eq!(report.questions().count(), usize::from(!method));
        assert!(report.notes[0].applied);
        assert!(report.coverage.complete());
        if method {
            assert!(matches!(
                report.requirements[0].outcome,
                RequirementOutcome::Assessed {
                    assessment: Assessment::Insufficient,
                    ..
                }
            ));
        }
    }
}
