#![allow(dead_code)]
use procurement_review::{
    core::{ErrorCategory, Visibility},
    qualification::*,
    runtime::{
        Runtime,
        grok::Model,
        search::{SearchModel, SearchResult},
    },
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

type Handler = dyn Fn(&str, &Value) -> Result<Value, ErrorCategory> + Send + Sync;
#[derive(Clone)]
pub struct Script {
    handler: Arc<Handler>,
    pub calls: Arc<Mutex<Vec<Value>>>,
    pub searches: Arc<AtomicUsize>,
    result: SearchResult,
}
impl Script {
    pub fn new(
        handler: impl Fn(&str, &Value) -> Result<Value, ErrorCategory> + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Arc::new(handler),
            calls: Arc::new(Mutex::new(vec![])),
            searches: Arc::new(AtomicUsize::new(0)),
            result: SearchResult::default(),
        }
    }
    pub fn with_search(mut self, result: SearchResult) -> Self {
        self.result = result;
        self
    }
    pub fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    pub fn search_count(&self) -> usize {
        self.searches.load(Ordering::SeqCst)
    }
}
impl Model for Script {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let request: Value = serde_json::from_str(prompt).unwrap();
        self.calls.lock().unwrap().push(request.clone());
        let data = &request["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "input")
            .unwrap()["value"];
        (self.handler)(request["instruction"].as_str().unwrap(), data).map(|v| v.to_string())
    }
}
impl SearchModel for Script {
    async fn search(&self, prompt: &str, _: usize) -> Result<SearchResult, ErrorCategory> {
        let _: Value = serde_json::from_str(prompt).unwrap();
        self.searches.fetch_add(1, Ordering::SeqCst);
        Ok(self.result.clone())
    }
}
pub fn runtime(script: Script) -> Runtime<Script> {
    Runtime::new(script, Duration::from_secs(1))
}
pub fn input(text: &str) -> QualificationInput {
    QualificationInput {
        document: Material::markdown("tz", text, Visibility::Public),
        attachments: vec![],
    }
}
pub fn blocks(data: &Value, key: &str) -> Vec<Block> {
    serde_json::from_value(data[key].clone()).unwrap()
}
pub fn quote(blocks: &[Block], text: &str) -> Quote {
    let block = blocks
        .iter()
        .find(|b| b.text.contains(text))
        .unwrap_or_else(|| panic!("Missing quote {text}: {blocks:?}"));
    Quote {
        block_id: block.id.clone(),
        quote: text.into(),
    }
}
pub fn numeric(
    blocks: &[Block],
    text: &str,
    property: &str,
    value: &str,
    unit: &str,
    operator: Operator,
) -> RequirementDraft {
    RequirementDraft {
        position: "14".into(),
        component: None,
        property: property.into(),
        constraint: Constraint::Numeric {
            operator,
            value: value.into(),
            upper: None,
            unit: unit.into(),
        },
        scope: "laboratory".into(),
        evidence: vec![quote(blocks, text)],
        reference: None,
    }
}
pub fn qualitative(blocks: &[Block], text: &str, property: &str, value: &str) -> RequirementDraft {
    RequirementDraft {
        position: "13".into(),
        component: None,
        property: property.into(),
        constraint: Constraint::Qualitative {
            value: value.into(),
        },
        scope: "camera".into(),
        evidence: vec![quote(blocks, text)],
        reference: None,
    }
}
pub fn extracted(requirements: Vec<RequirementDraft>) -> Value {
    json!(Extraction {
        requirements,
        unresolved: vec![]
    })
}
pub fn ask(data: &Value, fact: &str) -> Value {
    json!(Decision::Ask {
        requirement_id: data["current_requirement"]["id"].as_str().unwrap().into(),
        fact: fact.into(),
        question: format!("Please specify {fact}")
    })
}
pub fn assessed(data: &Value, assessment: Assessment, source: &str) -> Value {
    json!(Decision::Assess {
        requirement_id: data["current_requirement"]["id"].as_str().unwrap().into(),
        assessment,
        operation: "the stated laboratory operation".into(),
        reason: "The permitted configuration was compared with the cited operation".into(),
        evidence: vec![quote(&blocks(data, "blocks"), source)],
        allowed_case: Some("permitted boundary configuration".into()),
        alternative: Some("capacity for the confirmed maximum".into()),
        margin: Some("repetitions and reserve included".into())
    })
}
pub fn no_change_review(data: &Value) -> ReviewResponse {
    let records: Vec<RequirementRecord> = serde_json::from_value(data["records"].clone()).unwrap();
    let blocks = blocks(data, "blocks");
    ReviewResponse {
        notes: vec![],
        blocks: data["original_blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| {
                let id = id.as_str().unwrap();
                let conditions: Vec<_> = records
                    .iter()
                    .filter_map(|r| {
                        r.requirement
                            .evidence
                            .first()
                            .filter(|s| s.block_id == id)
                            .map(|s| ConditionMapping {
                                condition: Quote {
                                    block_id: id.into(),
                                    quote: s.quote.clone(),
                                },
                                requirement_id: Some(r.requirement.id.clone()),
                                note_id: None,
                            })
                    })
                    .collect();
                assert!(blocks.iter().any(|b| b.id == id));
                let no_requirements_reason = conditions
                    .is_empty()
                    .then(|| "Reference, context or non-technical block".into());
                BlockReconciliation {
                    block_id: id.into(),
                    conditions,
                    no_requirements_reason,
                }
            })
            .collect(),
    }
}
pub fn reviewer() -> Runtime<Script> {
    runtime(Script::new(|instruction, data| {
        assert!(instruction.contains("ONLY final review"));
        Ok(json!(no_change_review(data)))
    }))
}
pub fn record<'a>(report: &'a QualificationReport, property: &str) -> &'a RequirementRecord {
    report
        .requirements
        .iter()
        .find(|r| r.requirement.property == property)
        .unwrap()
}
