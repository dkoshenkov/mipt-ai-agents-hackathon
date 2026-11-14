mod qualification_support;
use procurement_review::{
    core::Visibility,
    qualification::{sources::SnapshotHttp, *},
};
use qualification_support::*;
use serde_json::json;

#[tokio::test]
async fn restored_report_can_read_uncited_page_in_another_process() {
    const CHILD: &str = "QUALIFICATION_RESUME_CHILD";
    if let Some(path) = std::env::var_os(CHILD) {
        let bundle: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let input: QualificationInput = serde_json::from_value(bundle["input"].clone()).unwrap();
        let report: QualificationReport = serde_json::from_value(bundle["report"].clone()).unwrap();
        let answer: Clarification = serde_json::from_value(bundle["answer"].clone()).unwrap();
        let previous_shutter = record(&report, "shutter").clone();
        let marker = report.coverage.review.clone();
        let worker = Script::new(|instruction, data| {
            assert!(!instruction.starts_with("Extract"));
            assert!(!instruction.contains("ONLY final review"));
            if instruction.starts_with("Verify") {
                let page2: Vec<_> = blocks(data, "source_blocks")
                    .into_iter()
                    .filter(|b| b.page == Some(2))
                    .collect();
                return Ok(json!(FactVerification {
                    primary_source: true,
                    reason: "Original Camera A passport, mode 2".into(),
                    fact: Some(FactProposal {
                        model: quote(&page2, "Camera A"),
                        parameter: quote(&page2, "Volume"),
                        value: quote(&page2, "80"),
                        unit: Some(quote(&page2, "L")),
                        conditions: vec![quote(&page2, "Mode2")]
                    })
                }));
            }
            if data["confirmed_facts"].as_array().unwrap().is_empty() {
                return Ok(json!(Decision::TechnicalFact {
                    requirement_id: data["current_requirement"]["id"].as_str().unwrap().into(),
                    target: FactTarget {
                        model: "Camera A".into(),
                        parameter: "Volume".into(),
                        numeric: true,
                        conditions: vec!["Mode2".into()]
                    },
                    query: "Camera A Mode2 Volume".into()
                }));
            }
            Ok(assessed(data, Assessment::Justified, "Volume 80 L"))
        });
        let http = SnapshotHttp::default();
        let ResumeOutcome::Updated { report } = resume_equipment_adequacy(
            &input,
            report,
            answer,
            &QualificationOptions::default(),
            &runtime(worker.clone()),
            &http,
        )
        .await
        else {
            panic!("late answer rejected")
        };
        assert_eq!(record(&report, "shutter"), &previous_shutter);
        assert_eq!(report.coverage.review, marker);
        let volume = record(&report, "volume");
        assert_eq!(volume.facts.len(), 1);
        assert!(volume.facts[0].evidence.iter().all(|s| s.page == Some(2)));
        assert!(matches!(
            volume.outcome,
            RequirementOutcome::Assessed {
                assessment: Assessment::Justified,
                ..
            }
        ));
        assert_eq!(worker.search_count(), 0);
        assert!(http.calls().is_empty());
        assert_eq!(worker.count(), 3);
        return;
    }
    let mut input = input(
        "Volume >= 80 L\nShutter must be global.\nTask: test sensors and assemblies; +180 C is not required.",
    );
    input.attachments.push(Material {
        id: "original-camera-passport".into(),
        visibility: Visibility::Public,
        format: MaterialFormat::Pdf,
        bytes: include_bytes!("fixtures/qualification/camera.pdf").to_vec(),
    });
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            let b = blocks(data, "packet");
            let mut r = numeric(&b, "Volume >= 80 L", "volume", "80", "L", Operator::AtLeast);
            r.position = "13".into();
            r.scope = "camera".into();
            return Ok(extracted(vec![
                r,
                qualitative(&b, "Shutter must be global", "shutter", "global"),
            ]));
        }
        if data["current_requirement"]["property"] == "volume" {
            return Ok(ask(
                data,
                "operating mode and largest assembly with fixtures",
            ));
        }
        Ok(assessed(
            data,
            Assessment::Justified,
            "Task: test sensors and assemblies; +180 C is not required.",
        ))
    });
    let review = reviewer();
    let report = review_equipment_adequacy(
        &input,
        &QualificationOptions::default(),
        &runtime(worker),
        &review,
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert!(report.coverage.complete(), "{:?}", report.coverage.gaps);
    assert!(record(&report, "volume").facts.is_empty());
    let (r, q) = report.questions().next().unwrap();
    let answer = Clarification {
        question_id: q.id.clone(),
        requirement_id: r.id.clone(),
        document_version: report.materials[0].version.clone(),
        text: "Mode2, assembly fits 80 L".into(),
        visibility: Visibility::Public,
    };
    let path =
        std::env::temp_dir().join(format!("qualification-resume-{}.json", std::process::id()));
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"input":input,"report":report,"answer":answer})).unwrap(),
    )
    .unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "restored_report_can_read_uncited_page_in_another_process",
            "--nocapture",
        ])
        .env(CHILD, &path)
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(
        child.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}
