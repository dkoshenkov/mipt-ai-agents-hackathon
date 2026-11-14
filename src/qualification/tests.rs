use super::{assessment::validate_fact, materials::*, sources::*, *};
use crate::core::Visibility;
use std::time::Duration;

fn q(blocks: &[Block], text: &str) -> Quote {
    Quote {
        block_id: blocks
            .iter()
            .find(|b| b.text.contains(text))
            .unwrap()
            .id
            .clone(),
        quote: text.into(),
    }
}

#[test]
fn quotes_address_utf8_occurrences_and_versions_without_guessing() {
    let text = "Первый прибор: 500 кг\nВторой прибор: 500 кг\nПовтор: 300 300\n";
    let blocks = index_text("tz", &version(text.as_bytes()), text);
    let source = resolve(
        &blocks,
        &Quote {
            block_id: blocks[1].id.clone(),
            quote: "500 кг".into(),
        },
    )
    .unwrap();
    assert_eq!(&text[source.start..source.end], "500 кг");
    assert!(source.start > blocks[0].end);
    assert!(
        resolve(
            &blocks,
            &Quote {
                block_id: blocks[2].id.clone(),
                quote: "300".into()
            }
        )
        .is_err()
    );
    assert!(
        resolve(
            &blocks,
            &Quote {
                block_id: blocks[0].id.clone(),
                quote: "invented".into()
            }
        )
        .is_err()
    );
}

#[tokio::test]
async fn html_and_pdf_facts_preserve_model_columns_units_and_pages() {
    let options = QualificationOptions::default();
    for (format, bytes) in [
        (
            MaterialFormat::Html,
            include_bytes!("../../tests/fixtures/qualification/passport.html").as_slice(),
        ),
        (
            MaterialFormat::Pdf,
            include_bytes!("../../tests/fixtures/qualification/passport.pdf").as_slice(),
        ),
    ] {
        let material = Material {
            id: "passport".into(),
            visibility: Visibility::Public,
            format,
            bytes: bytes.to_vec(),
        };
        let (text, uncertain) = decode(&material, &options).await.unwrap();
        assert!(!uncertain);
        let blocks = index_text("passport", &version(bytes), &text);
        assert!(!text.contains("hidden instruction"));
        let target = FactTarget {
            model: "A".into(),
            parameter: "Bandwidth".into(),
            numeric: true,
            conditions: vec![],
        };
        let fact = FactProposal {
            model: q(&blocks, "A"),
            parameter: q(&blocks, "Bandwidth"),
            value: q(&blocks, "50"),
            unit: Some(q(&blocks, "MHz")),
            conditions: vec![],
        };
        let accepted = validate_fact(&blocks, &target, fact.clone()).unwrap();
        assert_eq!(accepted.value, "50");
        if format == MaterialFormat::Pdf {
            assert_eq!(accepted.evidence[0].page, Some(1));
        }
        let mut wrong = fact.clone();
        wrong.value = q(&blocks, "100");
        assert!(validate_fact(&blocks, &target, wrong).is_err());
        let mut missing = fact.clone();
        missing.unit = None;
        assert!(validate_fact(&blocks, &target, missing).is_err());
        let mut invented = fact;
        invented.value.quote = "200".into();
        assert!(validate_fact(&blocks, &target, invented).is_err());
        let target = FactTarget {
            parameter: "Shutter".into(),
            numeric: false,
            ..target
        };
        let fact = FactProposal {
            model: q(&blocks, "A"),
            parameter: q(&blocks, "Shutter"),
            value: q(&blocks, "global"),
            unit: None,
            conditions: vec![],
        };
        assert!(
            validate_fact(&blocks, &target, fact)
                .unwrap()
                .unit
                .is_none()
        );
    }
}

#[test]
fn numeric_units_belong_to_the_parameter_row_or_applicable_header() {
    let target = FactTarget {
        model: "A".into(),
        parameter: "Bandwidth".into(),
        numeric: true,
        conditions: vec![],
    };
    for (text, unit, expected) in [
        (
            "| Parameter | A | B |\n| Bandwidth | 50 | 100 |\n| Weight | 5 kg | 6 pounds |",
            "kg",
            false,
        ),
        (
            "| Parameter | A | B |\n| Bandwidth | 50 | 100 MHz |",
            "MHz",
            false,
        ),
        (
            "| Parameter | A | B |\n| Bandwidth | 50 MHz | 100 GHz |",
            "MHz",
            true,
        ),
        (
            "| Parameter | A | B |\n| Bandwidth, MHz | 50 | 100 |",
            "MHz",
            true,
        ),
        (
            "| Parameter | A (MHz) | B (GHz) |\n| Bandwidth | 50 | 100 |",
            "MHz",
            true,
        ),
    ] {
        let b = index_text("passport", &version(text.as_bytes()), text);
        let result = validate_fact(
            &b,
            &target,
            FactProposal {
                model: q(&b, "A"),
                parameter: q(&b, "Bandwidth"),
                value: q(&b, "50"),
                unit: Some(q(&b, unit)),
                conditions: vec![],
            },
        );
        assert_eq!(result.is_ok(), expected, "{text}: {result:?}");
    }
}

#[tokio::test]
async fn redirects_are_validated_and_charged_individually() {
    for (destination, budget, expected, count) in [
        (
            "https://127.0.0.1/private",
            2,
            Some(FailureReason::AddressRejected),
            1,
        ),
        ("/manual", 1, Some(FailureReason::HttpBudgetExhausted), 1),
        ("/manual", 2, None, 2),
    ] {
        let mut http = SnapshotHttp::default();
        http.responses.insert(
            "https://maker.example/start".into(),
            Ok(HttpResponse {
                status: 302,
                content_type: "".into(),
                location: Some(destination.into()),
                body: vec![],
                retrieved_at: "snapshot".into(),
            }),
        );
        http.insert(
            "https://maker.example/manual",
            "text/html",
            b"<p>Original manual</p>".to_vec(),
        );
        let options = QualificationOptions {
            http_requests: budget,
            ..Default::default()
        };
        let mut session = SourceSession {
            transport: &http,
            options: &options,
            requests: 0,
        };
        let result = session
            .read("https://maker.example/start", &[Visibility::Public])
            .await;
        assert_eq!(result.err(), expected);
        assert_eq!(session.requests, count);
        assert_eq!(http.calls().len(), count);
    }
    let http = SnapshotHttp::default();
    let options = QualificationOptions::default();
    for (url, visibility) in [
        ("https://maker.example/source", Visibility::Unknown),
        ("https://maker.example/source", Visibility::Private),
        (
            "https://user:password@maker.example/source",
            Visibility::Public,
        ),
        ("http://maker.example/source", Visibility::Public),
        ("https://[::ffff:127.0.0.1]/source", Visibility::Public),
    ] {
        let mut session = SourceSession {
            transport: &http,
            options: &options,
            requests: 0,
        };
        assert!(session.read(url, &[visibility]).await.is_err());
        assert_eq!(session.requests, 0);
    }
    assert!(http.calls().is_empty());
}

#[tokio::test]
async fn direct_read_timeout_size_and_unreadable_pdf_are_distinct() {
    struct Slow;
    impl HttpTransport for Slow {
        async fn get(
            &self,
            _: &url::Url,
            _: &[std::net::SocketAddr],
            _: usize,
        ) -> Result<HttpResponse, FailureReason> {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Err(FailureReason::SourceUnavailable)
        }
    }
    let mut options = QualificationOptions {
        request_timeout_ms: 2,
        ..Default::default()
    };
    let mut session = SourceSession {
        transport: &Slow,
        options: &options,
        requests: 0,
    };
    assert_eq!(
        session
            .read("https://maker.example/source", &[Visibility::Public])
            .await
            .err(),
        Some(FailureReason::TimedOut)
    );
    assert_eq!(session.requests, 1);
    let mut http = SnapshotHttp::default();
    http.insert("https://maker.example/source", "text/html", vec![b'x'; 10]);
    options.max_source_bytes = 2;
    let mut session = SourceSession {
        transport: &http,
        options: &options,
        requests: 0,
    };
    assert_eq!(
        session
            .read("https://maker.example/source", &[Visibility::Public])
            .await
            .err(),
        Some(FailureReason::TooLarge)
    );
    options.request_timeout_ms = 1000;
    options.max_source_bytes = 1000;
    assert_eq!(
        pdf_text(b"broken pdf", &options).await.err(),
        Some(FailureReason::Unreadable)
    );
}

#[test]
fn unsupported_html_geometry_is_explicit_and_versions_include_every_byte() {
    let (_, uncertain) = html_text("<table><tr><td colspan='2'>A</td></tr></table>").unwrap();
    assert!(uncertain);
    assert_ne!(version("ТЗ".as_bytes()), version("ТЗ ".as_bytes()));
    let text = "| Parameter | A | B |\n| Value | | 10 |";
    let blocks = index_text("p", &version(text.as_bytes()), text);
    assert_eq!(cells(&blocks[1].text).len(), 3);
    let mut input = QualificationInput {
        document: Material::markdown("tz", "\u{a0}\n", Visibility::Public),
        attachments: vec![],
    };
    assert_eq!(
        validate_input(&input, &QualificationOptions::default()),
        Err(QualificationInputError::EmptyDocument)
    );
    input.document.bytes = b"Text".to_vec();
    input.attachments.push(input.document.clone());
    assert_eq!(
        validate_input(&input, &QualificationOptions::default()),
        Err(QualificationInputError::InvalidMaterialIds)
    );
}

#[test]
fn html_rows_inside_markdown_keep_original_ranges_and_headers() {
    let text = "# Паспорт\n<table><tr><th>Parameter</th><th>A</th><th>B</th></tr>\n<tr>\n<td>Bandwidth MHz</td><td>50</td><td>100</td>\n</tr></table>";
    let blocks = index_text("p", &version(text.as_bytes()), text);
    let target = FactTarget {
        model: "A".into(),
        parameter: "Bandwidth".into(),
        numeric: true,
        conditions: vec![],
    };
    let fact = FactProposal {
        model: q(&blocks, "A"),
        parameter: q(&blocks, "Bandwidth"),
        value: q(&blocks, "50"),
        unit: Some(q(&blocks, "MHz")),
        conditions: vec![],
    };
    let result = validate_fact(&blocks, &target, fact.clone()).unwrap();
    for source in result.evidence {
        assert_eq!(&text[source.start..source.end], source.quote);
    }
    let mut wrong = fact;
    wrong.value = q(&blocks, "100");
    assert!(validate_fact(&blocks, &target, wrong).is_err());
}
