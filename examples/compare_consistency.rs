//! Offline scoring: cargo run --example compare_consistency -- SOURCE GOLD REPORT LABELS
//! REPORT is serialized ConsistencyReport. LABELS maps candidate IDs to GOLD keys after
//! independent semantic adjudication; keys may name conflicts or controls. Missing IDs are false positives. Example labels:
//! {"candidate-id-from-report":"L1"}. Use the same labels for initial and verified IDs.
//! Input fixtures: tests/fixtures/consistency/{lift,metrology,held-out-water}.{md,gold.json}.
//! Compare scored runs by version and decisions[].key; model_attempts is the actual call count.
//! Save model, settings, budget, date, inputs, elapsed time, cost and catalogue differences
//! alongside those reports; these metadata are not inferred or fabricated here. No model is called.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
};

fn overlap(e: &Value, span: &Value, version: &str) -> bool {
    let r = &e["range"];
    e["version"] == version
        && r["start"]
            .as_u64()
            .zip(r["end"].as_u64())
            .zip(span[0].as_u64().zip(span[1].as_u64()))
            .is_some_and(|((a, b), (c, d))| a < b && a < d && c < b)
}
fn covers<'a>(items: impl Iterator<Item = &'a Value>, span: &Value, version: &str) -> bool {
    let (Some(start), Some(end)) = (span[0].as_u64(), span[1].as_u64()) else {
        return false;
    };
    let mut ranges: Vec<_> = items
        .filter(|e| e["version"] == version)
        .filter_map(|e| e["range"]["start"].as_u64().zip(e["range"]["end"].as_u64()))
        .collect();
    ranges.sort_unstable();
    let mut covered = start;
    for (a, b) in ranges {
        if a > covered {
            break;
        }
        if b > covered {
            covered = b;
        }
        if covered >= end {
            return true;
        }
    }
    false
}
fn evidence(candidate: &Value) -> impl Iterator<Item = &Value> {
    candidate["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(candidate["context"].as_array().into_iter().flatten())
}
fn score(gold: &Value, candidates: &[Value], labels: &Value) -> Value {
    let version = gold["version"].as_str().unwrap();
    let conflicts = gold["conflicts"].as_array().unwrap();
    let mut found = HashMap::<&str, Vec<&Value>>::new();
    let (mut fp, mut duplicates, mut wrong_address, mut complete) = (0, 0, 0, 0);
    let mut missing = Vec::new();
    for candidate in candidates {
        let id = candidate["id"].as_str().unwrap_or("");
        let Some(g) = labels[id]
            .as_str()
            .and_then(|key| conflicts.iter().find(|g| g["key"] == key))
        else {
            fp += 1;
            continue;
        };
        let spans = g["sides"].as_array().unwrap();
        // One addressed side identifies the conflict; completeness is measured separately.
        if !spans
            .iter()
            .any(|s| evidence(candidate).any(|e| overlap(e, s, version)))
        {
            fp += 1;
            wrong_address += 1;
            continue;
        }
        let key = g["key"].as_str().unwrap();
        let bucket = found.entry(key).or_default();
        if !bucket.is_empty() {
            duplicates += 1;
        }
        bucket.push(candidate);
    }
    for g in conflicts {
        let key = g["key"].as_str().unwrap();
        let Some(group) = found.get(key) else {
            continue;
        };
        let absent: Vec<_> = g["sides"]
            .as_array()
            .unwrap()
            .iter()
            .chain(g["qualifiers"].as_array().into_iter().flatten())
            .filter(|s| !covers(group.iter().flat_map(|c| evidence(c)), s, version))
            .cloned()
            .collect();
        if absent.is_empty() {
            complete += 1
        } else {
            missing.push(json!({"key":key,"ranges":absent}));
        }
    }
    json!({"tp":found.len(),"fp":fp,"fn":conflicts.len()-found.len(),"duplicates":duplicates,"wrong_address_fp":wrong_address,
        "complete_evidence":complete,"evidence_denominator":conflicts.len(),"missing_evidence":missing})
}
fn evaluate(gold: &Value, report: &Value, labels: &Value) -> Result<Value, String> {
    if report["version"] != gold["version"] {
        return Err("report and gold document versions differ".into());
    }
    let initial = report["initial_candidates"]
        .as_array()
        .ok_or("missing initial_candidates")?;
    let investigations = report["investigations"]
        .as_array()
        .ok_or("missing investigations")?;
    let mut verified = Vec::new();
    let mut technical = Vec::new();
    let mut dismissed = 0;
    let mut needs_review = 0;
    for inv in investigations {
        if inv["outcome"]["Ok"].get("Confirmed").is_some() {
            let mut c = inv["candidate"].clone();
            // Confirmed evidence is the critic's cited evidence, not everything it saw.
            c["evidence"] = inv["cited_evidence"].clone();
            c["context"] = json!([]);
            verified.push(c);
        } else if !inv["outcome"]["Err"].is_null() {
            technical.push(inv["candidate"]["id"].clone());
        } else if !inv["outcome"]["Ok"]["Dismissed"].is_null() {
            dismissed += 1;
        } else if !inv["outcome"]["Ok"]["NeedsReview"].is_null() {
            needs_review += 1;
        }
    }
    let version = gold["version"].as_str().unwrap();
    let mut unsupported_confirmations = Vec::new();
    for candidate in &verified {
        let id = candidate["id"].as_str().unwrap_or("");
        let conflict = labels[id].as_str().and_then(|key| {
            gold["conflicts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|g| g["key"] == key)
        });
        let reason = match conflict {
            None => Some("false_positive"),
            Some(g)
                if !g["sides"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| evidence(candidate).any(|e| overlap(e, s, version))) =>
            {
                Some("false_positive")
            }
            Some(g)
                if g["sides"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .chain(g["qualifiers"].as_array().into_iter().flatten())
                    .any(|s| !covers(evidence(candidate), s, version)) =>
            {
                Some("insufficient_citations")
            }
            _ => None,
        };
        if let Some(reason) = reason {
            unsupported_confirmations.push(
                json!({"id":id,"key":labels[id],"reason":reason,"evidence":candidate["evidence"]}),
            );
        }
    }
    let confirmed_ids: HashSet<_> = verified
        .iter()
        .filter(|c| {
            let id = c["id"].as_str().unwrap_or("");
            let single = score(gold, std::slice::from_ref(c), labels);
            !id.is_empty() && single["tp"] == 1
        })
        .filter_map(|c| c["id"].as_str())
        .collect();
    let mut actions = HashMap::<String, String>::new();
    let mut extra_questions = Vec::new();
    let mut decisions = Vec::new();
    let mut unsupported_selections = Vec::new();
    for entry in report["selections"].as_array().into_iter().flatten() {
        let id = entry[0].as_str().unwrap_or("");
        let key = labels[id].as_str();
        let selection = &entry[1];
        let is_confirmed = confirmed_ids.contains(id);
        let conflict = key.and_then(|key| {
            gold["conflicts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|g| g["key"] == key)
        });
        if !selection["AskUser"].is_null()
            && (!is_confirmed || conflict.is_none_or(|g| g["action"] != "ask"))
        {
            extra_questions.push(json!({"id":id,"key":key,"target":selection["AskUser"]["target"],"reason":if is_confirmed {"action_not_needed"} else {"unconfirmed_or_false_positive"}}));
        }
        if !selection["Selected"].is_null() {
            let supported = is_confirmed
                && conflict.is_some_and(|g| {
                    g["action"] == "selected"
                        && covers(
                            std::iter::once(
                                &selection["Selected"]["source"]["statement"]["evidence"],
                            ),
                            &g["sides"][g["selected_side"].as_u64().unwrap_or(0) as usize],
                            version,
                        )
                        && g["choice_basis"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .all(|span| {
                                covers(
                                    selection["Selected"]["basis"]
                                        .as_array()
                                        .into_iter()
                                        .flatten()
                                        .map(|b| &b["evidence"]),
                                    span,
                                    version,
                                )
                            })
                });
            if !supported {
                unsupported_selections.push(json!({"id":id,"key":key,"reason":if is_confirmed {"wrong_choice_or_missing_basis"} else {"unconfirmed_or_false_positive"},"selection":selection}));
            }
        }
        if !is_confirmed {
            continue;
        }
        let Some(key) = key else {
            continue;
        };
        let action = if !selection["AskUser"].is_null() {
            "ask"
        } else if !selection["Selected"].is_null() {
            "selected"
        } else {
            continue;
        };
        actions.insert(key.to_owned(), action.to_owned());
    }
    for inv in investigations {
        let id = inv["candidate"]["id"].as_str().unwrap_or("");
        let key = labels[id].as_str().unwrap_or(id);
        let selection = report["selections"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry[0] == id)
            .map(|entry| &entry[1]);
        let verdict = if inv["outcome"]["Ok"].get("Confirmed").is_some() {
            "confirmed"
        } else if !inv["outcome"]["Ok"]["Dismissed"].is_null() {
            "dismissed"
        } else if !inv["outcome"]["Ok"]["NeedsReview"].is_null() {
            "needs_review"
        } else {
            "technical_failure"
        };
        decisions
            .push(json!({"key":key,"candidate_id":id,"verdict":verdict,"selection":selection}));
    }
    // Object questions have no candidate label; only count a control when all its
    // independent sides are actually addressed by the question.
    for question in report["object_questions"].as_array().into_iter().flatten() {
        let addresses: Vec<_> = std::iter::once(&question["reference"]["evidence"])
            .chain(
                question["alternatives"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| &a["evidence"]),
            )
            .collect();
        for control in gold["controls"].as_array().into_iter().flatten() {
            if control["sides"]
                .as_array()
                .unwrap()
                .iter()
                .all(|side| addresses.iter().any(|e| overlap(e, side, version)))
            {
                extra_questions
                    .push(json!({"key":control["key"],"reason":"control","question":question}));
            }
        }
    }
    // Include missing decisions so reports can be aligned by the independent GOLD key.
    for g in gold["conflicts"].as_array().unwrap() {
        let key = g["key"].as_str().unwrap();
        if !decisions.iter().any(|d| d["key"] == key) {
            decisions.push(json!({"key":key,"selection":null}));
        }
    }
    decisions.sort_by(|a, b| {
        a["key"]
            .as_str()
            .cmp(&b["key"].as_str())
            .then(a["candidate_id"].as_str().cmp(&b["candidate_id"].as_str()))
    });
    let mut correct_choice = 0;
    let mut unsupported_replacement = 0;
    let mut appropriate_question = 0;
    for g in gold["conflicts"].as_array().unwrap() {
        let key = g["key"].as_str().unwrap();
        match actions.get(key).map(String::as_str) {
            Some("ask") if g["action"] == "ask" => appropriate_question += 1,
            Some("selected") => {
                let good = g["action"] == "selected"
                    && report["selections"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| {
                            labels[entry[0].as_str().unwrap_or("")] == key
                                && confirmed_ids.contains(entry[0].as_str().unwrap_or(""))
                                && covers(
                                    std::iter::once(
                                        &entry[1]["Selected"]["source"]["statement"]["evidence"],
                                    ),
                                    &g["sides"][g["selected_side"].as_u64().unwrap_or(0) as usize],
                                    gold["version"].as_str().unwrap(),
                                )
                                && g["choice_basis"].as_array().into_iter().flatten().all(|q| {
                                    covers(
                                        entry[1]["Selected"]["basis"]
                                            .as_array()
                                            .into_iter()
                                            .flatten()
                                            .map(|b| &b["evidence"]),
                                        q,
                                        gold["version"].as_str().unwrap(),
                                    )
                                })
                        });
                if good {
                    correct_choice += 1
                } else {
                    unsupported_replacement += 1;
                }
            }
            _ => (),
        }
    }
    let mut missed_mentions = 0;
    let mut wrong_edition = 0;
    let mut unresolved_edition = 0;
    let mut wrong_applicability = 0;
    let mut unresolved_applicability = 0;
    let mut technical_applicability = 0;
    for g in gold["standards"].as_array().into_iter().flatten() {
        let row = report["standard_references"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| {
                overlap(
                    &r["mention"]["evidence"],
                    &g["range"],
                    gold["version"].as_str().unwrap(),
                )
            });
        match row {
            None => missed_mentions += 1,
            Some(r) if r["mention"]["original"] != g["edition"] => wrong_edition += 1,
            Some(r) if r["existence"] != "Exact" => unresolved_edition += 1,
            _ => (),
        }
        let usage = report["standard_usage"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|u| {
                overlap(
                    &u["mention"],
                    &g["range"],
                    gold["version"].as_str().unwrap(),
                )
            });
        let expected = if g["applicability"] == "SupportedWithinComparedConditions" {
            "supported"
        } else {
            "outside_scope"
        };
        match usage.and_then(|u| u["outcome"].as_str()) {
            Some("technical_failure") => technical_applicability += 1,
            None | Some("insufficient_evidence") => unresolved_applicability += 1,
            Some(actual) if actual != expected => wrong_applicability += 1,
            _ => (),
        }
    }
    Ok(
        json!({"version":version,"decisions":decisions,"extra_questions":extra_questions,"extra_question_count":extra_questions.len(),"unsupported_confirmations":unsupported_confirmations,"unsupported_confirmation_count":unsupported_confirmations.len(),"unsupported_selections":unsupported_selections,"unsupported_selection_count":unsupported_selections.len(),"initial":score(gold, initial, labels),"verified":score(gold, &verified, labels),"technical_failures":technical,"dismissed":dismissed,"needs_review":needs_review,"packet_stops":report["packets"].as_array().into_iter().flatten().filter(|p| !p["stop"].is_null()).count(),"semantic_gaps":report["semantic_gaps"].as_array().map_or(0, Vec::len),
        "actions":{"correct_choice":correct_choice,"unsupported_replacement":unsupported_replacement,"appropriate_question":appropriate_question,"missed_conflict":score(gold,&verified,labels)["fn"],"denominator_conflicts":gold["conflicts"].as_array().unwrap().len()},
        "standards":{"missed_mentions":missed_mentions,"wrong_edition":wrong_edition,"unresolved_edition":unresolved_edition,"denominator_mentions":gold["standards"].as_array().map_or(0,Vec::len),"wrong_applicability":wrong_applicability,"unresolved_applicability":unresolved_applicability,"technical_applicability":technical_applicability},
        "model_attempts":report["model_attempts"],"catalog_requests":report["catalog_requests"]}),
    )
}
fn validate_addresses(
    value: &Value,
    index: &procurement_review::consistency::markdown::MarkdownIndex,
) -> bool {
    match value {
        Value::Object(map)
            if map.contains_key("block_id")
                && map.contains_key("range")
                && map.contains_key("version") =>
        {
            let id = value["block_id"].as_u64().map(|x| x as usize);
            let start = value["range"]["start"].as_u64().map(|x| x as usize);
            let end = value["range"]["end"].as_u64().map(|x| x as usize);
            id.zip(start.zip(end)).is_some_and(|(id, (start, end))| {
                value["version"] == index.version
                    && start < end
                    && index.source.get(start..end).is_some()
                    && index
                        .blocks
                        .get(id)
                        .is_some_and(|b| b.range.start <= start && end <= b.range.end)
            })
        }
        Value::Object(map) => map.values().all(|v| validate_addresses(v, index)),
        Value::Array(items) => items.iter().all(|v| validate_addresses(v, index)),
        _ => true,
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().collect();
    if args.len() != 5 {
        return Err("usage: compare_consistency SOURCE GOLD REPORT LABELS".into());
    }
    let source = fs::read(&args[1])?;
    let gold: Value = serde_json::from_slice(&fs::read(&args[2])?)?;
    let report: Value = serde_json::from_slice(&fs::read(&args[3])?)?;
    let labels: Value = serde_json::from_slice(&fs::read(&args[4])?)?;
    let known_ids: HashSet<&str> = report["initial_candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            report["investigations"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| &i["candidate"]),
        )
        .filter_map(|c| c["id"].as_str())
        .collect();
    let known_keys: HashSet<&str> = gold["conflicts"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(gold["controls"].as_array().into_iter().flatten())
        .filter_map(|g| g["key"].as_str())
        .collect();
    for (id, key) in labels.as_object().ok_or("labels must be a JSON object")? {
        if !known_ids.contains(id.as_str()) || !key.as_str().is_some_and(|k| known_keys.contains(k))
        {
            return Err(format!("unknown candidate ID or gold key in label: {id}").into());
        }
    }
    if format!("{:x}", Sha256::digest(&source)) != gold["version"].as_str().unwrap_or("") {
        return Err("source and gold document versions differ".into());
    }
    let index = procurement_review::consistency::markdown::MarkdownIndex::new(std::str::from_utf8(
        &source,
    )?)
    .map_err(|_| "empty source")?;
    for field in [
        "initial_candidates",
        "investigations",
        "selections",
        "standard_references",
        "standard_usage",
        "object_questions",
        "standard_material_questions",
    ] {
        if !validate_addresses(&report[field], &index) {
            return Err(format!("invalid evidence address in {field}").into());
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&evaluate(&gold, &report, &labels)?)?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicates_wrong_position_and_missing_qualifier() {
        let gold = json!({"version":"v","conflicts":[{"key":"A","sides":[[0,10],[20,30]],"qualifiers":[[40,50]]},{"key":"B","sides":[[60,70],[80,90]],"qualifiers":[]}]});
        let c = |id: &str, ranges: &[(u64, u64)]| json!({"id":id,"evidence":ranges.iter().map(|(start,end)|json!({"version":"v","range":{"start":start,"end":end}})).collect::<Vec<_>>(),"context":[]});
        let labels = json!({"a":"A","duplicate":"A","wrong":"B","b":"B"});
        let x = score(
            &gold,
            &[
                c("a", &[(0, 10), (20, 30)]),
                c("duplicate", &[(0, 5)]),
                c("wrong", &[(0, 5)]),
            ],
            &labels,
        );
        assert_eq!(
            (x["tp"].as_u64(), x["fp"].as_u64(), x["fn"].as_u64()),
            (Some(1), Some(1), Some(1))
        );
        assert_eq!(x["missing_evidence"][0]["ranges"], json!([[40, 50]]));
        assert_eq!(x["wrong_address_fp"], 1);
        assert_eq!(x["duplicates"], 1);
        assert!(!covers(
            [json!({"version":"v","range":{"start":0,"end":1}})].iter(),
            &json!([0, 10]),
            "v"
        ));
        let x = score(
            &gold,
            &[c("a", &[(0, 10), (20, 30)]), c("wrong", &[(0, 5)])],
            &labels,
        );
        assert_eq!((x["fp"].as_u64(), x["fn"].as_u64()), (Some(1), Some(1)));
        let x = score(
            &gold,
            &[c("a", &[(0, 10), (20, 30)]), c("b", &[(60, 70), (80, 90)])],
            &labels,
        );
        assert_eq!(
            (x["tp"].as_u64(), x["complete_evidence"].as_u64()),
            (Some(2), Some(1))
        );
    }
    #[test]
    fn late_failure_is_not_a_verified_finding() {
        let gold = json!({"version":"v","conflicts":[{"key":"A","sides":[[0,10],[20,30]],"qualifiers":[],"action":"ask"}]});
        let c = json!({"id":"a","evidence":[{"version":"v","range":{"start":0,"end":5}},{"version":"v","range":{"start":20,"end":25}}],"context":[]});
        let report = json!({"version":"v","initial_candidates":[c],"investigations":[{"candidate":c,"outcome":{"Err":"Runtime"}}],"selections":[],"standard_references":[]});
        let x = evaluate(&gold, &report, &json!({"a":"A"})).unwrap();
        assert_eq!(x["initial"]["tp"], 1);
        assert_eq!(x["verified"]["fn"], 1);
        assert_eq!(x["technical_failures"], json!(["a"]));
    }
    #[test]
    fn questions_confirmations_and_decisions_are_addressed() {
        let gold = json!({"version":"v","conflicts":[{"key":"A","sides":[[0,10],[20,30]],"qualifiers":[],"action":"ask"}],"controls":[{"key":"C","sides":[[40,50],[60,70]]}]});
        let e = |start, end| json!({"version":"v","range":{"start":start,"end":end}});
        let candidate = |id| json!({"id":id,"evidence":[e(0,10)],"context":[]});
        let report = json!({"version":"v","model_attempts":4,"initial_candidates":[candidate("a"),candidate("c"),candidate("x")],"investigations":[
            {"candidate":candidate("a"),"cited_evidence":[e(0,10)],"outcome":{"Ok":{"Confirmed":{}}}},
            {"candidate":candidate("c"),"cited_evidence":[e(40,50),e(60,70)],"outcome":{"Ok":{"Confirmed":{}}}},
            {"candidate":candidate("x"),"cited_evidence":[],"outcome":{"Ok":{"Confirmed":{}}}}
        ],"selections":[["a",{"AskUser":{"target":{"sides":[e(0,10)]}}}],["c",{"AskUser":{"target":{"sides":[e(40,50),e(60,70)]}}}],["x",{"Selected":{}}]],"object_questions":[{"reference":{"evidence":e(40,50)},"alternatives":[{"evidence":e(60,70)}]}]});
        let result = evaluate(&gold, &report, &json!({"a":"A","c":"C"})).unwrap();
        assert_eq!(result["extra_question_count"], 2);
        assert_eq!(result["unsupported_confirmation_count"], 3);
        assert_eq!(
            result["unsupported_confirmations"][0]["reason"],
            "insufficient_citations"
        );
        assert_eq!(result["unsupported_selection_count"], 1);
        assert_eq!(result["decisions"][0]["key"], "A");
        assert_eq!(result["decisions"][0]["verdict"], "confirmed");
        assert_eq!(result["model_attempts"], 4);
    }
}
