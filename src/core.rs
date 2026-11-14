use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub markdown: String,
    pub visibility: Visibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Private,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub description: String,
    pub citations: Vec<Citation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub quote: String,
    #[serde(default)]
    pub links: Vec<SourceLink>,
}

/// Byte offsets into the original Markdown, with an exclusive end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLink {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCategory {
    RateLimited,
    TimedOut,
    InvalidResponse,
    Provider,
    RestrictedContext,
}

/// Safe diagnostic metadata; never contains prompts, credentials or response bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestError {
    pub category: ErrorCategory,
    pub stage: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}

impl RequestError {
    pub fn new(category: ErrorCategory, stage: &str, reason: impl Into<String>) -> Self {
        Self {
            category,
            stage: stage.into(),
            reason: reason.into(),
            http_status: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub check_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<RequestError>,
    pub result: Result<Vec<Finding>, ErrorCategory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Completeness {
    Complete,
    Partial,
    NoneSucceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub outcomes: Vec<CheckOutcome>,
}

impl Evidence {
    pub fn completeness(&self) -> Completeness {
        let successes = self
            .outcomes
            .iter()
            .filter(|outcome| outcome.result.is_ok())
            .count();
        if successes == 0 {
            Completeness::NoneSucceeded
        } else if successes == self.outcomes.len() {
            Completeness::Complete
        } else {
            Completeness::Partial
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub conclusion: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchReport {
    Reviewed {
        evidence: Evidence,
        verdict: Verdict,
    },
    CriticFailed {
        evidence: Evidence,
        error: ErrorCategory,
    },
}

pub fn aggregate(document: &Document, mut outcomes: Vec<CheckOutcome>) -> Evidence {
    for outcome in &mut outcomes {
        if let Ok(findings) = &mut outcome.result {
            for finding in findings {
                for citation in &mut finding.citations {
                    citation.links = if citation.quote.is_empty() {
                        Vec::new()
                    } else {
                        document
                            .markdown
                            .match_indices(&citation.quote)
                            .map(|(start, text)| SourceLink {
                                start,
                                end: start + text.len(),
                            })
                            .collect()
                    };
                }
            }
        }
    }
    Evidence { outcomes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_preserves_order_findings_and_failures() {
        let document = Document {
            markdown: "Таблица: пассажирский лифт\nОписание: грузовой лифт".into(),
            visibility: Visibility::Public,
        };
        let outcomes = vec![
            CheckOutcome {
                diagnostic: None,
                check_id: "empty".into(),
                result: Ok(vec![]),
            },
            CheckOutcome {
                diagnostic: None,
                check_id: "failed".into(),
                result: Err(ErrorCategory::RateLimited),
            },
            CheckOutcome {
                diagnostic: None,
                check_id: "found".into(),
                result: Ok(vec![Finding {
                    description: "Расхождение".into(),
                    citations: vec![
                        Citation {
                            quote: "пассажирский лифт".into(),
                            links: vec![],
                        },
                        Citation {
                            quote: "грузовой лифт".into(),
                            links: vec![],
                        },
                    ],
                }]),
            },
        ];
        let evidence = aggregate(&document, outcomes);
        assert_eq!(
            evidence
                .outcomes
                .iter()
                .map(|o| o.check_id.as_str())
                .collect::<Vec<_>>(),
            ["empty", "failed", "found"]
        );
        assert_eq!(evidence.outcomes[0].result, Ok(vec![]));
        assert_eq!(evidence.outcomes[1].result, Err(ErrorCategory::RateLimited));
        let finding = &evidence.outcomes[2].result.as_ref().unwrap()[0];
        for citation in &finding.citations {
            assert_eq!(citation.links.len(), 1);
            let link = &citation.links[0];
            assert_eq!(&document.markdown[link.start..link.end], citation.quote);
        }
        assert_eq!(evidence.completeness(), Completeness::Partial);
    }
}
