use procurement_review::{
    core::{Document, Visibility},
    runtime::{
        Runtime,
        grok::Grok,
        tz_review::{self, Material, reader},
    },
};
use rig_core::completion::{Usage, UsageTotals};
use sha2::{Digest, Sha256};
use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

fn load(path: &Path) -> Result<Document, Box<dyn Error>> {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
    {
        return Err("Ожидается файл .md".into());
    }
    let markdown = fs::read_to_string(path)?;
    if markdown.trim().is_empty() {
        return Err("Пустой Markdown".into());
    }
    Ok(Document {
        markdown,
        visibility: Visibility::Public,
    })
}
fn totals(usage: &Mutex<Vec<Usage>>) -> UsageTotals {
    let mut t = UsageTotals::default();
    for call in usage.lock().unwrap().iter() {
        t.record(call);
    }
    t
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--public")) {
        return Err("tz_review --public ТЗ.md НОВЫЙ_КАТАЛОГ [публичное-приложение.md ...]".into());
    }
    let mut args = args.peekable();
    let full = args.next_if(|v| v == "--full").is_some();
    let source = PathBuf::from(args.next().ok_or("Нужен путь к ТЗ")?);
    let output = PathBuf::from(args.next().ok_or("Нужен новый каталог вывода")?);
    let doc = load(&source)?;
    let mut docs = Vec::new();
    for arg in args {
        let path = PathBuf::from(arg);
        docs.push((
            path.file_name()
                .ok_or("Отсутствует имя приложения")?
                .to_string_lossy()
                .into_owned(),
            load(&path)?,
        ));
    }
    let materials: Vec<_> = docs
        .iter()
        .map(|(name, document)| Material { name, document })
        .collect();
    let worker = env::var("XAI_WORKER_MODEL").unwrap_or("grok-4.20-0309-non-reasoning".into());
    let critic = env::var("XAI_CRITIC_MODEL").unwrap_or("grok-4.20-0309-reasoning".into());
    let budget = env::var("TZ_REVIEW_BUDGET").unwrap_or("7".into()).parse()?;
    let cache = format!("{:x}", Sha256::digest(&doc.markdown));
    let checks = Arc::default();
    let reviews = Arc::default();
    let model = |name: &str, usage: &Arc<Mutex<Vec<Usage>>>| -> Result<Grok, &str> {
        Ok(Grok::from_env(name.to_string())?
            .with_prompt_cache_key(cache.clone())
            .with_usage_recording(usage.clone()))
    };
    let runtime = Runtime::new(model(&worker, &checks)?, Duration::from_secs(180))
        .with_reviewer(model(&critic, &reviews)?);
    fs::create_dir(&output)?;
    let report = tz_review::review_with_mode(&runtime, &doc, &materials, budget, full).await?;
    let info = serde_json::json!({"full_diagnostic":full,"worker":worker,"critic":critic,"budget":budget,"checks_usage":totals(&checks),"critic_usage":totals(&reviews),"report":report,"source":source});
    fs::write(output.join("source.md"), &doc.markdown)?;
    fs::write(
        output.join("source.html"),
        reader::document_view(&source.to_string_lossy(), &doc.markdown),
    )?;
    fs::write(output.join("remarks.md"), reader::notes(&report))?;
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&info)?,
    )?;
    println!(
        "{} · замечаний {} · попыток {}",
        output.display(),
        report.findings.len(),
        report.attempts
    );
    Ok(())
}
