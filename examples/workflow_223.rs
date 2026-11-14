use std::{
    env,
    error::Error,
    fs, io,
    sync::{Arc, Mutex},
    time::Duration,
};

use procurement_review::{
    core::{Document, Visibility, aggregate},
    runtime::{Check, Runtime, grok::Grok, scenario::review_evidence},
};
use rig_core::completion::{Usage, UsageTotals};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct Catalog {
    agents: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    prompt: String,
}

fn runtime(
    variable: &str,
    default: &str,
    usage: Option<&Arc<Mutex<Vec<Usage>>>>,
    cache_key: &str,
) -> Result<Runtime<Grok>, Box<dyn Error>> {
    let model = env::var(variable).unwrap_or_else(|_| default.to_owned());
    let mut grok = Grok::from_env(model)?.with_prompt_cache_key(cache_key.to_owned());
    if let Some(usage) = usage {
        grok = grok.with_usage_recording(Arc::clone(usage));
    }
    Ok(Runtime::new(grok, Duration::from_secs(180)))
}

fn log_usage(stage: &str, usage: &Mutex<Vec<Usage>>) {
    let mut totals = UsageTotals::default();
    for call in usage.lock().unwrap().iter() {
        totals.record(call);
    }
    eprintln!(
        "{}",
        serde_json::json!({"event": "usage", "stage": stage, "responses": totals.calls, "tokens": totals.tokens})
    );
}

fn record_usage(value: Option<&std::ffi::OsStr>) -> Result<bool, &'static str> {
    match value {
        None => Ok(false),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err("XAI_RECORD_USAGE must be 0 or 1"),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let record_usage = record_usage(env::var_os("XAI_RECORD_USAGE").as_deref())?;
    let catalog: Catalog = serde_json::from_slice(&fs::read("223fz_27_agents_ru_unified.json")?)?;
    let document = Document {
        markdown: fs::read_to_string("ТЗ_замена_грузового_лифта.md")?,
        visibility: Visibility::Public,
    };
    let cache_key = format!("{:x}", Sha256::digest(document.markdown.as_bytes()));
    let checks: Vec<_> = catalog
        .agents
        .into_iter()
        .map(|entry| Check {
            id: entry.id,
            prompt: entry.prompt,
            materials: vec![],
        })
        .collect();
    let selected = env::var("CHECK_ID").ok();
    let first = selected.as_deref().unwrap_or("A01");
    let first = checks
        .iter()
        .find(|check| check.id == first)
        .ok_or("Unknown check ID")?;
    let checks_usage = Arc::default();
    let agents = runtime(
        "XAI_WORKER_MODEL",
        "grok-4.20-0309-non-reasoning",
        record_usage.then_some(&checks_usage),
        &cache_key,
    )?;
    let mut outcomes = agents
        .run_parallel(&document, std::slice::from_ref(first))
        .await
        .map_err(|e| format!("Invalid check input: {e:?}"))?;
    if selected.is_some() {
        serde_json::to_writer_pretty(io::stdout().lock(), &aggregate(&document, outcomes))?;
    } else {
        let remaining: Vec<_> = checks.into_iter().filter(|c| c.id != "A01").collect();
        if !remaining.is_empty() {
            outcomes.extend(
                agents
                    .run_parallel(&document, &remaining)
                    .await
                    .map_err(|e| format!("Invalid check input: {e:?}"))?,
            );
        }
        let critic_usage = Arc::default();
        let critic = runtime(
            "XAI_CRITIC_MODEL",
            "grok-4.20-0309-reasoning",
            record_usage.then_some(&critic_usage),
            &cache_key,
        )?;
        let report = review_evidence(&critic, &document, aggregate(&document, outcomes), &[]).await;
        if record_usage {
            log_usage("critic", &critic_usage);
        }
        serde_json::to_writer_pretty(io::stdout().lock(), &report)?;
    }
    if record_usage {
        log_usage("checks", &checks_usage);
    }
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_flag_is_explicit() {
        assert_eq!(record_usage(None), Ok(false));
        assert_eq!(record_usage(Some("0".as_ref())), Ok(false));
        assert_eq!(record_usage(Some("1".as_ref())), Ok(true));
        assert!(record_usage(Some("true".as_ref())).is_err());
    }
}
