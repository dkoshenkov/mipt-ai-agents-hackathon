use crate::core::{ErrorCategory, Visibility};
use crate::runtime::StepError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialFormat {
    Markdown,
    Html,
    Pdf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Material {
    pub id: String,
    pub visibility: Visibility,
    pub format: MaterialFormat,
    pub bytes: Vec<u8>,
}
impl Material {
    pub fn markdown(
        id: impl Into<String>,
        text: impl Into<String>,
        visibility: Visibility,
    ) -> Self {
        Self {
            id: id.into(),
            visibility,
            format: MaterialFormat::Markdown,
            bytes: text.into().into_bytes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualificationInput {
    pub document: Material,
    pub attachments: Vec<Material>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialVersion {
    pub id: String,
    pub version: String,
    pub visibility: Visibility,
    pub format: MaterialFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub id: String,
    pub material_id: String,
    pub version: String,
    /// Hash of the indexed text. For Markdown this is the original version.
    pub text_version: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub page: Option<usize>,
    pub headings: Vec<String>,
    /// Original table header rows, retained with every data row.
    pub table_headers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Quote {
    pub block_id: String,
    pub quote: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub material_id: String,
    pub version: String,
    pub text_version: String,
    pub block_id: String,
    /// Byte offsets into the immutable text view. Markdown offsets address original bytes.
    pub start: usize,
    pub end: usize,
    pub quote: String,
    pub page: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(schemars::JsonSchema)]
pub enum Constraint {
    Numeric {
        operator: Operator,
        value: String,
        upper: Option<String>,
        unit: String,
    },
    Qualitative {
        value: String,
    },
    Alternatives {
        values: Vec<String>,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(schemars::JsonSchema)]
pub enum Operator {
    Exactly,
    AtLeast,
    AtMost,
    Range,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RequirementDraft {
    pub position: String,
    pub component: Option<String>,
    pub property: String,
    pub constraint: Constraint,
    pub scope: String,
    pub evidence: Vec<Quote>,
    /// Required when the obligation is imposed by reference to an attachment.
    pub reference: Option<Quote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub id: String,
    pub position: String,
    pub component: Option<String>,
    pub property: String,
    pub constraint: Constraint,
    pub scope: String,
    pub evidence: Vec<SourceRef>,
    pub reference: Option<SourceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Extraction {
    pub requirements: Vec<RequirementDraft>,
    pub unresolved: Vec<Quote>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(schemars::JsonSchema)]
pub enum Assessment {
    Justified,
    Insufficient,
    PossiblyExcessive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub fact: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RequirementOutcome {
    Assessed {
        assessment: Assessment,
        operation: String,
        reason: String,
        evidence: Vec<SourceRef>,
        allowed_case: Option<String>,
        alternative: Option<String>,
        margin: Option<String>,
    },
    Pending {
        missing_fact: String,
        question: Option<Question>,
    },
    Stopped {
        failure: Failure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FactTarget {
    pub model: String,
    pub parameter: String,
    pub numeric: bool,
    pub conditions: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FactProposal {
    pub model: Quote,
    pub parameter: Quote,
    pub value: Quote,
    pub unit: Option<Quote>,
    pub conditions: Vec<Quote>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FactVerification {
    pub primary_source: bool,
    pub fact: Option<FactProposal>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedFact {
    pub target: FactTarget,
    pub value: String,
    pub unit: Option<String>,
    pub evidence: Vec<SourceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[derive(schemars::JsonSchema)]
pub enum Decision {
    Assess {
        requirement_id: String,
        assessment: Assessment,
        operation: String,
        reason: String,
        evidence: Vec<Quote>,
        allowed_case: Option<String>,
        alternative: Option<String>,
        margin: Option<String>,
    },
    Ask {
        requirement_id: String,
        fact: String,
        question: String,
    },
    Read {
        requirement_id: String,
        block_ids: Vec<String>,
    },
    TechnicalFact {
        requirement_id: String,
        target: FactTarget,
        query: String,
    },
}
impl Decision {
    pub fn requirement_id(&self) -> &str {
        match self {
            Self::Assess { requirement_id, .. }
            | Self::Ask { requirement_id, .. }
            | Self::Read { requirement_id, .. }
            | Self::TechnicalFact { requirement_id, .. } => requirement_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clarification {
    pub question_id: String,
    pub requirement_id: String,
    pub document_version: String,
    pub text: String,
    pub visibility: Visibility,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementRecord {
    pub requirement: Requirement,
    pub outcome: RequirementOutcome,
    pub facts: Vec<ConfirmedFact>,
    pub read_evidence: Vec<SourceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub stage: String,
    pub reason: FailureReason,
}
impl Failure {
    pub fn new(stage: &str, reason: FailureReason) -> Self {
        Self {
            stage: stage.into(),
            reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    RestrictedContext,
    BudgetExhausted,
    Preparation,
    Model(ErrorCategory),
    InvalidResponse,
    ContextLimit,
    Unreadable,
    SourceUnavailable,
    HttpBudgetExhausted,
    AddressRejected,
    TimedOut,
    TooLarge,
    RepeatedEvidence,
    MissingMaterial(String),
    VersionMismatch(String),
    VisibilityMismatch(String),
    InvalidQuestion,
    InvalidInput,
}
impl From<StepError> for FailureReason {
    fn from(value: StepError) -> Self {
        match value {
            StepError::Preparation => Self::Preparation,
            StepError::RestrictedContext => Self::RestrictedContext,
            StepError::BudgetExhausted => Self::BudgetExhausted,
            StepError::Model(e) => Self::Model(e),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub block_ids: Vec<String>,
    pub material_id: Option<String>,
    pub failure: Failure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockCoverage {
    pub block_id: String,
    pub extracted: bool,
    pub reviewed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReviewStatus {
    NotRun,
    Completed,
    Failed { failure: Failure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub blocks: Vec<BlockCoverage>,
    pub gaps: Vec<Gap>,
    pub review: ReviewStatus,
}
impl Coverage {
    pub fn complete(&self) -> bool {
        self.review == ReviewStatus::Completed
            && self.gaps.is_empty()
            && self.blocks.iter().all(|b| b.extracted && b.reviewed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReviewResponse {
    pub notes: Vec<ReviewNote>,
    pub blocks: Vec<BlockReconciliation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReviewNote {
    pub id: String,
    pub reason: String,
    pub evidence: Vec<Quote>,
    pub change: ReviewChange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[derive(schemars::JsonSchema)]
pub enum ReviewChange {
    Add {
        requirement: RequirementDraft,
    },
    Reassess {
        requirement_id: String,
    },
    Merge {
        requirement_ids: Vec<String>,
        requirement: RequirementDraft,
    },
    Remove {
        requirement_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConditionMapping {
    pub condition: Quote,
    /// Exactly one of these addresses a retained record or an applied add/merge note.
    pub requirement_id: Option<String>,
    pub note_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BlockReconciliation {
    pub block_id: String,
    pub conditions: Vec<ConditionMapping>,
    pub no_requirements_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteOutcome {
    pub note_id: String,
    pub applied: bool,
    pub reason: String,
    pub original_ids: Vec<String>,
    pub resulting_id: Option<String>,
    pub evidence: Vec<SourceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSource {
    pub material_id: String,
    pub url: String,
    pub version: String,
    pub retrieved_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchTrace {
    pub requirement_id: String,
    pub target: FactTarget,
    pub result: crate::runtime::search::SearchResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualificationReport {
    pub document_id: String,
    pub materials: Vec<MaterialVersion>,
    pub blocks: Vec<Block>,
    pub requirements: Vec<RequirementRecord>,
    pub coverage: Coverage,
    pub notes: Vec<NoteOutcome>,
    pub closed_questions: Vec<Question>,
    pub clarifications: Vec<Clarification>,
    pub external_sources: Vec<ExternalSource>,
    pub searches: Vec<SearchTrace>,
    pub model_attempts: usize,
    pub http_requests: usize,
    pub next_id: usize,
}
impl QualificationReport {
    pub fn questions(&self) -> impl Iterator<Item = (&Requirement, &Question)> {
        self.requirements.iter().filter_map(|r| match &r.outcome {
            RequirementOutcome::Pending {
                question: Some(q), ..
            } => Some((&r.requirement, q)),
            _ => None,
        })
    }
    pub(crate) fn next_id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }
    pub(crate) fn gap(&mut self, blocks: Vec<String>, stage: &str, reason: FailureReason) {
        self.coverage.gaps.push(Gap {
            block_ids: blocks,
            material_id: None,
            failure: Failure::new(stage, reason),
        });
    }
    pub(crate) fn visibility(&self) -> Vec<Visibility> {
        self.materials
            .iter()
            .map(|m| m.visibility)
            .chain(self.clarifications.iter().map(|c| c.visibility))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct QualificationOptions {
    pub model_attempts: usize,
    pub http_requests: usize,
    pub max_source_bytes: usize,
    pub max_context_bytes: usize,
    pub extraction_batch_bytes: usize,
    pub request_timeout_ms: u64,
    pub max_search_turns: usize,
}
impl Default for QualificationOptions {
    fn default() -> Self {
        Self {
            model_attempts: 64,
            http_requests: 8,
            max_source_bytes: 4 * 1024 * 1024,
            max_context_bytes: 120_000,
            extraction_batch_bytes: 12_000,
            request_timeout_ms: 15_000,
            max_search_turns: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualificationInputError {
    EmptyDocument,
    InvalidMaterialIds,
    InvalidOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResumeOutcome {
    Updated {
        report: QualificationReport,
    },
    Rejected {
        reason: FailureReason,
        report: QualificationReport,
    },
}
