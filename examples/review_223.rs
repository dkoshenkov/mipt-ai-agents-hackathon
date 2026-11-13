use std::time::Duration;

use procurement_review::{
    core::{BranchReport, Document, ErrorCategory, Visibility},
    runtime::{Check, Runtime, grok::Model, scenario::review_223},
};

struct Checks;

impl Model for Checks {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        if prompt.contains("rate limit") {
            // Local equivalent of an HTTP 429 response.
            return Err(ErrorCategory::RateLimited);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(r#"[{"description":"Таблица и описание указывают разные типы лифта","citations":[{"quote":"пассажирский лифт"},{"quote":"грузовой лифт"}]}]"#.into())
    }
}

struct Critic {
    timeout: bool,
}

impl Model for Critic {
    async fn complete(&self, _prompt: &str) -> Result<String, ErrorCategory> {
        if self.timeout {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(r#"{"conclusion":"Обнаружено расхождение; одна проверка не выполнена."}"#.into())
    }
}

#[tokio::main]
async fn main() {
    let document = Document {
        markdown: "Таблица: пассажирский лифт\nОписание: грузовой лифт".into(),
        visibility: Visibility::Public,
    };
    let checks = vec![
        Check {
            id: "rate-limit".into(),
            prompt: "rate limit".into(),
            materials: vec![],
        },
        Check {
            id: "consistency".into(),
            prompt: "Сравнить таблицу и описание".into(),
            materials: vec![],
        },
    ];
    let checks_runtime = Runtime::new(Checks, Duration::from_millis(100));

    for timeout in [false, true] {
        let critic_runtime = Runtime::new(Critic { timeout }, Duration::from_millis(100));
        let report = review_223(&checks_runtime, &critic_runtime, &document, &checks)
            .await
            .expect("valid input");
        let evidence = match &report {
            BranchReport::Reviewed { evidence, verdict } => {
                println!("Критик: {}", verdict.conclusion);
                evidence
            }
            BranchReport::CriticFailed { evidence, error } => {
                println!("Ошибка критика: {error:?}");
                evidence
            }
        };
        assert_eq!(evidence.outcomes[0].result, Err(ErrorCategory::RateLimited));
        let finding = &evidence.outcomes[1].result.as_ref().unwrap()[0];
        println!(
            "Ошибка соседней проверки: {:?}",
            evidence.outcomes[0].result
        );
        println!("Расхождение: {}", finding.description);
        for citation in &finding.citations {
            assert_eq!(citation.links.len(), 1);
            let link = &citation.links[0];
            assert_eq!(&document.markdown[link.start..link.end], citation.quote);
            println!(
                "Цитата: {} (байты {}..{})",
                citation.quote, link.start, link.end
            );
        }
        assert_eq!(finding.citations.len(), 2);
        assert_eq!(
            matches!(
                report,
                BranchReport::CriticFailed {
                    error: ErrorCategory::TimedOut,
                    ..
                }
            ),
            timeout
        );
    }
}
