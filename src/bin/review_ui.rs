use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Request},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use procurement_review::{
    consistency::{
        self, ConsistencyReport, ReviewOptions, catalog_http::CatalogHttp, standards::Catalog,
    },
    core::{Document, Visibility},
    qualification::{
        self, QualificationInput, QualificationOptions, QualificationReport, sources::PublicHttp,
    },
    runtime::{
        Runtime,
        grok::Grok,
        search::SearchModel,
        tz_review::{self, Material},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{env, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command, sync::Semaphore, time::timeout};

const LIMIT: usize = 20 * 1024 * 1024;
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
static CONVERTER: Semaphore = Semaphore::const_new(1);
type ApiError = (StatusCode, Json<Value>);

fn error(code: StatusCode, message: &str) -> ApiError {
    (code, Json(json!({ "error": message })))
}

async fn convert(headers: HeaderMap, body: Bytes) -> Result<Json<Value>, ApiError> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some("application/octet-stream")
    {
        return Err(error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Ожидается файл DOCX или PDF.",
        ));
    }
    if let Ok(url) = env::var("DOCX_MODEL_URL") {
        let reply = reqwest::Client::new()
            .post(url)
            .timeout(Duration::from_secs(600))
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|_| error(StatusCode::BAD_GATEWAY, "Контейнер конвертера недоступен."))?;
        let status = reply.status();
        let result: Value = reply
            .json()
            .await
            .map_err(|_| error(StatusCode::BAD_GATEWAY, "Некорректный ответ конвертера."))?;
        return if status.is_success() {
            Ok(Json(result))
        } else {
            Err((status, Json(result)))
        };
    }
    let _slot = CONVERTER.try_acquire().map_err(|_| {
        error(
            StatusCode::TOO_MANY_REQUESTS,
            "Конвертер занят. Повторите после завершения текущего файла.",
        )
    })?;
    let mut child = Command::new(Path::new(ROOT).join("target/mineru-env/bin/python"))
        .arg(Path::new(ROOT).join("scripts/docx_markdown.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Установите окружение конвертера по инструкции в README.",
            )
        })?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let output = timeout(Duration::from_secs(600), async {
        stdin.write_all(&body).await?;
        drop(stdin);
        child.wait_with_output().await
    })
    .await
    .map_err(|_| {
        error(
            StatusCode::GATEWAY_TIMEOUT,
            "Преобразование заняло более 10 минут.",
        )
    })?
    .map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Ошибка локального конвертера.",
        )
    })?;
    let result: Value = serde_json::from_slice(&output.stdout)
        .ok()
        .filter(|_: &Value| output.status.success())
        .ok_or_else(|| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Локальный конвертер завершился с ошибкой.",
            )
        })?;
    if let Some(message) = result["error"].as_str() {
        return Err(error(StatusCode::UNPROCESSABLE_ENTITY, message));
    }
    Ok(Json(result))
}

#[derive(Deserialize)]
struct File {
    name: String,
    markdown: String,
}

#[derive(Deserialize)]
struct ReviewRequest {
    public: bool,
    files: Vec<File>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Branch<T> {
    Report(T),
    Failed { branch_error: &'static str },
}

impl<T> From<Result<T, &'static str>> for Branch<T> {
    fn from(result: Result<T, &'static str>) -> Self {
        match result {
            Ok(report) => Self::Report(report),
            Err(branch_error) => Self::Failed { branch_error },
        }
    }
}

#[derive(Serialize)]
struct WorkflowReport {
    // Keep the existing 223-FZ fields at the top level for API clients.
    #[serde(flatten)]
    legal: Branch<tz_review::Report>,
    consistency: Branch<ConsistencyReport>,
    equipment: Branch<QualificationReport>,
}

async fn review_consistency<M: SearchModel>(
    doc: &Document,
    runtime: &Runtime<M>,
) -> Result<ConsistencyReport, &'static str> {
    let date = chrono::Utc::now().date_naive().to_string();
    let mut live = CatalogHttp::new(20, 2, &date)
        .map_err(|_| "Не удалось открыть официальный каталог ГОСТ.")?;
    let catalog = Catalog {
        retrieved_at: date,
        cards: vec![],
    };
    consistency::review_with_catalog(
        doc,
        ReviewOptions::default(),
        runtime,
        &catalog,
        Some(&mut live),
    )
    .await
    .map_err(|_| "Пустой текст технического задания.")
}

async fn review_workflow<M: SearchModel>(
    runtime: &Runtime<M>,
    equipment_reviewer: &Runtime<M>,
    primary: &(String, Document),
    attachments: &[(String, Document)],
    legal_budget: Result<usize, &'static str>,
) -> WorkflowReport {
    let doc = &primary.1;
    let materials: Vec<_> = attachments
        .iter()
        .map(|(name, document)| Material { name, document })
        .collect();
    let markdown = |(name, doc): &(String, Document)| {
        qualification::Material::markdown(name, &doc.markdown, doc.visibility)
    };
    let input = QualificationInput {
        document: markdown(primary),
        attachments: attachments.iter().map(markdown).collect(),
    };
    let options = QualificationOptions::default();
    let (legal, consistency, equipment) = tokio::join!(
        async { tz_review::review(runtime, doc, &materials, legal_budget?).await },
        review_consistency(doc, runtime),
        qualification::review_equipment_adequacy(
            &input,
            &options,
            runtime,
            equipment_reviewer,
            &PublicHttp
        ),
    );
    WorkflowReport {
        legal: legal.into(),
        consistency: consistency.into(),
        equipment: equipment
            .map_err(|_| "Некорректный комплект документов для проверки оборудования.")
            .into(),
    }
}

async fn review(Json(request): Json<ReviewRequest>) -> Result<Json<WorkflowReport>, ApiError> {
    if !request.public
        || request.files.is_empty()
        || request
            .files
            .iter()
            .any(|f| f.markdown.trim().is_empty() || f.name.trim().is_empty())
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Для проверки нужны публичное ТЗ и подтверждение отправки в xAI.",
        ));
    }
    let docs: Vec<_> = request
        .files
        .into_iter()
        .map(|f| {
            (
                f.name,
                Document {
                    markdown: f.markdown,
                    visibility: Visibility::Public,
                },
            )
        })
        .collect();
    let (primary, attachments) = docs.split_first().expect("validated nonempty request");
    let cache_key = format!("{:x}", Sha256::digest(&primary.1.markdown));
    let model = |name| {
        Grok::from_env(name)
            .map(|m| m.with_prompt_cache_key(&cache_key))
            .map_err(|_| {
                error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Для проверки через Grok задайте XAI_API_KEY на сервере.",
                )
            })
    };
    let timeout = Duration::from_secs(180);
    let worker = model(
        env::var("XAI_WORKER_MODEL").unwrap_or_else(|_| "grok-4.20-0309-non-reasoning".into()),
    )?;
    let critic =
        model(env::var("XAI_CRITIC_MODEL").unwrap_or_else(|_| "grok-4.20-0309-reasoning".into()))?;
    let equipment_reviewer = match env::var("XAI_REVIEWER_MODEL") {
        Ok(name) if !name.trim().is_empty() => model(name)?,
        _ => critic.clone(),
    };
    let runtime = Runtime::new(worker, timeout).with_reviewer(critic);
    let equipment_reviewer = Runtime::new(equipment_reviewer, timeout);
    let budget = env::var("TZ_REVIEW_BUDGET")
        .unwrap_or_else(|_| "7".into())
        .parse()
        .map_err(|_| "Некорректный TZ_REVIEW_BUDGET.");
    Ok(Json(
        review_workflow(&runtime, &equipment_reviewer, primary, attachments, budget).await,
    ))
}

// Only the UI served on this loopback socket may submit browser requests.
async fn local_request(request: Request, next: Next) -> Response {
    let headers = request.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !matches!(host.split(':').next(), Some("localhost" | "127.0.0.1"))
        || headers
            .get(header::ORIGIN)
            .is_some_and(|o| o.as_bytes() != format!("http://{host}").as_bytes())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let app = Router::new()
        .route("/", get(async || Html(include_str!("../../ui/index.html"))))
        .route(
            "/style.css",
            get(async || {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("../../ui/style.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(async || {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../../ui/app.js"),
                )
            }),
        )
        .route("/api/convert", post(convert))
        .route("/api/review", post(review))
        .layer(DefaultBodyLimit::max(LIMIT))
        .layer(middleware::from_fn(local_request));
    let listener = tokio::net::TcpListener::bind(
        env::var("REVIEW_BIND").unwrap_or_else(|_| "127.0.0.1:8710".into()),
    )
    .await?;
    eprintln!("Интерфейс проверки: http://{}", listener.local_addr()?);
    axum::serve(listener, app).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use procurement_review::{
        core::ErrorCategory,
        runtime::{grok::Model, search::SearchResult},
    };

    struct Replay;
    impl Model for Replay {
        async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
            let request: Value = serde_json::from_str(prompt).unwrap();
            let field = |name| {
                request["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|f| f["name"] == name)
                    .map(|f| &f["value"])
            };
            let response = if let Some(materials) = field("public_materials") {
                assert_eq!(materials[0]["text"], "APPENDIX_ONLY");
                assert!(
                    field("numbered_markdown")
                        .unwrap()
                        .to_string()
                        .contains("MAIN_ONLY")
                );
                json!({"regime":"inapplicable","reason":"Закупка вне 223-ФЗ","line":1,"assignments":[],"candidates":[]})
            } else if let Some(packet) = field("packet") {
                assert!(packet.to_string().contains("MAIN_ONLY"));
                assert!(!packet.to_string().contains("APPENDIX_ONLY"));
                json!({"claims":[], "priority_rules":[], "refinements":[], "standard_mentions":[]})
            } else if let Some(input) = field("input") {
                if let Some(blocks) = input["original_blocks"].as_array() {
                    let mappings: Vec<_> = blocks.iter().map(|id|
                        json!({"block_id":id, "conditions":[], "no_requirements_reason":"Справочный текст"})
                    ).collect();
                    json!({"notes":[], "blocks":mappings})
                } else {
                    json!({"requirements":[], "unresolved":[]})
                }
            } else {
                panic!("unexpected workflow stage");
            };
            Ok(response.to_string())
        }
    }
    impl SearchModel for Replay {
        async fn search(&self, _: &str, _: usize) -> Result<SearchResult, ErrorCategory> {
            panic!("no technical facts to search for")
        }
    }

    #[tokio::test]
    async fn all_branches_receive_their_inputs_even_when_legal_is_skipped_or_fails() {
        let runtime = Runtime::new(Replay, Duration::from_secs(1));
        let file = |name: &str, text: &str| {
            (
                name.to_owned(),
                Document {
                    markdown: text.to_owned(),
                    visibility: Visibility::Public,
                },
            )
        };
        let primary = file("ТЗ.pdf", "MAIN_ONLY");
        let attachments = [file("Приложение.docx", "APPENDIX_ONLY")];
        for budget in [Ok(7), Err("Некорректный TZ_REVIEW_BUDGET.")] {
            let report = review_workflow(&runtime, &runtime, &primary, &attachments, budget).await;
            let response = serde_json::to_value(report).unwrap();
            match budget {
                Ok(_) => assert_eq!(response["regime"], "inapplicable"),
                Err(message) => assert_eq!(response["branch_error"], message),
            }
            let consistency = &response["consistency"];
            assert_eq!(consistency["standard_coverage"]["complete"], true);
            assert!(consistency["model_attempts"].as_u64().unwrap() >= 2);
            let equipment = &response["equipment"];
            assert_eq!(equipment["document_id"], "ТЗ.pdf");
            assert_eq!(equipment["materials"][1]["id"], "Приложение.docx");
            assert_eq!(equipment["blocks"][1]["text"], "APPENDIX_ONLY");
            assert_eq!(equipment["coverage"]["gaps"], json!([]));
            assert_eq!(equipment["coverage"]["review"]["status"], "completed");
        }
    }
}
