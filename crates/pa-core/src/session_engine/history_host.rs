//! The `history.*` host bridge: the kernel's `history` namespace reads the
//! live view, and `history.edit` writes one `context_edit` row.

use std::sync::Arc;

use serde_json::{json, Value};

use pa_types::session::{ContextEditEntry, ContextRole, ContextSelector, ContextTarget};

use crate::kernel::shared::{host_handler, HostRequestHandlers};
use crate::session::{HistoryEntry, HistoryWalk};
use crate::session_engine::compaction::estimate_context_tokens;
use pa_types::session::FileEntry;

use super::turn_boundary::{TurnBoundaryRequests, TurnBoundaryRuntime};

/// Register `history.index`, `history.path`, `history.get`, and
/// `history.edit` against a turn-boundary runtime.
pub fn register_history_handlers(
    requests: &Arc<TurnBoundaryRequests>,
    handlers: &mut HostRequestHandlers,
) {
    let weak = Arc::downgrade(requests);
    handlers.register(
        "history.index",
        host_handler(move |payload| {
            let weak = weak.clone();
            Box::pin(async move {
                let runtime = bound_runtime(&weak)?;
                let walk = load_walk(&runtime).await?;
                let window = runtime.context_window.unwrap_or_default();
                Ok(index_payload(&walk, &payload.data, window))
            })
        }),
    );
    let weak = Arc::downgrade(requests);
    handlers.register(
        "history.path",
        host_handler(move |_payload| {
            let weak = weak.clone();
            Box::pin(async move {
                let runtime = bound_runtime(&weak)?;
                let walk = load_walk(&runtime).await?;
                Ok(json!({
                    "entries": walk.entries.iter().map(entry_payload).collect::<Vec<_>>(),
                }))
            })
        }),
    );
    let weak = Arc::downgrade(requests);
    handlers.register(
        "history.get",
        host_handler(move |payload| {
            let weak = weak.clone();
            Box::pin(async move {
                let runtime = bound_runtime(&weak)?;
                let walk = load_walk(&runtime).await?;
                get_payload(&walk, &payload.data)
            })
        }),
    );
    let weak = Arc::downgrade(requests);
    handlers.register(
        "history.edit",
        host_handler(move |payload| {
            let weak = weak.clone();
            Box::pin(async move {
                let runtime = bound_runtime(&weak)?;
                edit(&runtime, &payload.data, &weak).await
            })
        }),
    );
}

fn bound_runtime(
    weak: &std::sync::Weak<TurnBoundaryRequests>,
) -> anyhow::Result<Arc<TurnBoundaryRuntime>> {
    let requests = weak
        .upgrade()
        .ok_or_else(|| anyhow::anyhow!("the session ended before the request could be served"))?;
    requests
        .bound()
        .ok_or_else(|| anyhow::anyhow!("the session is not assembled yet; history is unavailable"))
}

async fn load_walk(runtime: &TurnBoundaryRuntime) -> anyhow::Result<HistoryWalk> {
    let (snapshot, leaf) = {
        let session = runtime.session.lock().await;
        (
            session.history_snapshot(),
            session.get_leaf_id().map(str::to_owned),
        )
    };
    let entries: Vec<FileEntry> = snapshot.await?;
    Ok(crate::session::walk_history(&entries, leaf.as_deref()))
}

fn entry_payload(entry: &HistoryEntry) -> Value {
    json!({
        "id": entry.id.clone().unwrap_or_default(),
        "turn": entry.turn,
        "role": entry.role.as_str(),
        "tokens": entry.tokens,
        "text": entry.text,
    })
}

fn preview(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 160 {
        return flat;
    }
    flat.chars().take(160).collect()
}

fn index_payload(walk: &HistoryWalk, data: &Value, window: u64) -> Value {
    let order = data
        .get("order")
        .and_then(Value::as_str)
        .unwrap_or("tokens");
    let limit = data
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(20, |limit| limit as usize);
    let role = data.get("role").and_then(Value::as_str).and_then(role_of);
    let query = data
        .get("query")
        .and_then(Value::as_str)
        .map(str::to_lowercase);
    let include_hidden = data
        .get("include_hidden")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let messages = walk.messages();
    let live_tokens = estimate_context_tokens(&messages).tokens;
    let hidden_tokens = walk.hidden.iter().map(|entry| entry.tokens).sum::<u64>();

    let mut rows: Vec<&HistoryEntry> = walk
        .entries
        .iter()
        .chain(walk.hidden.iter().filter(|_| include_hidden))
        .filter(|entry| role.is_none_or(|role| entry.role == role))
        .filter(|entry| {
            query
                .as_ref()
                .is_none_or(|query| entry.text.to_lowercase().contains(query))
        })
        .collect();
    match order {
        "recent" => rows.sort_by_key(|entry| std::cmp::Reverse((entry.seq, entry.turn))),
        "oldest" => rows.sort_by_key(|entry| (entry.seq, entry.turn)),
        "turn" => rows.sort_by_key(|entry| (entry.turn, entry.seq)),
        _ => rows.sort_by_key(|entry| (std::cmp::Reverse(entry.tokens), entry.seq)),
    }
    let share = |tokens: u64| {
        if live_tokens == 0 {
            0.0
        } else {
            tokens as f64 / live_tokens as f64
        }
    };
    let rows: Vec<Value> = rows
        .iter()
        .take(limit)
        .map(|entry| {
            json!({
                "id": entry.id.clone().unwrap_or_default(),
                "turn": entry.turn,
                "role": entry.role.as_str(),
                "tokens": entry.tokens,
                "share": share(entry.tokens),
                "preview": preview(&entry.text),
            })
        })
        .collect();

    json!({
        "entries": walk.entries.len(),
        "live_tokens": live_tokens,
        "window": window,
        "over_by": live_tokens.saturating_sub(window),
        "hidden": walk.hidden.len(),
        "hidden_tokens": hidden_tokens,
        "rows": rows,
    })
}

fn role_of(name: &str) -> Option<ContextRole> {
    match name {
        "user" => Some(ContextRole::User),
        "assistant" => Some(ContextRole::Assistant),
        "tool" => Some(ContextRole::Tool),
        "notes" => Some(ContextRole::Notes),
        _ => None,
    }
}

fn target_from_selector(data: &Value) -> anyhow::Result<ContextTarget> {
    let kind = data
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("history selector needs a kind"))?;
    match kind {
        "position" => data
            .get("value")
            .and_then(Value::as_i64)
            .map(|position| ContextTarget::Point(ContextSelector::Position(position)))
            .ok_or_else(|| anyhow::anyhow!("history selector position needs an integer value")),
        "id" => data
            .get("value")
            .and_then(Value::as_str)
            .map(|id| ContextTarget::Point(ContextSelector::EntryId(id.to_string())))
            .ok_or_else(|| anyhow::anyhow!("history selector id needs a string value")),
        "range" => {
            let end = |name: &str| -> anyhow::Result<pa_types::session::ContextRangeEnd> {
                match data.get(name) {
                    None | Some(Value::Null) => Ok(pa_types::session::ContextRangeEnd::PathBound),
                    Some(Value::Number(number)) => number
                        .as_i64()
                        .map(pa_types::session::ContextRangeEnd::Position)
                        .ok_or_else(|| anyhow::anyhow!("history range {name} must be an integer")),
                    Some(Value::String(id)) => {
                        Ok(pa_types::session::ContextRangeEnd::EntryId(id.clone()))
                    }
                    Some(_) => Err(anyhow::anyhow!(
                        "history range {name} must be an integer, an id, or null"
                    )),
                }
            };
            Ok(ContextTarget::Range(pa_types::session::ContextRange {
                start: end("start")?,
                end: end("end")?,
            }))
        }
        other => Err(anyhow::anyhow!("history selector kind {other} is unknown")),
    }
}

fn get_payload(walk: &HistoryWalk, data: &Value) -> anyhow::Result<Value> {
    let selector = data
        .get("selector")
        .ok_or_else(|| anyhow::anyhow!("history.get needs one selector"))?;
    let target = target_from_selector(selector)?;
    let limit = data
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(200, |limit| limit.max(1) as usize);
    let entries: Vec<&HistoryEntry> =
        if let ContextTarget::Point(ContextSelector::EntryId(id)) = &target {
            vec![walk.entry_by_id(id).ok_or_else(|| {
                anyhow::anyhow!(
                    "context_edit selector off the current path: {id} is not in this session"
                )
            })?]
        } else {
            walk.resolve(&target)
                .map_err(anyhow::Error::msg)?
                .into_iter()
                .filter_map(|position| walk.entries.get(position))
                .collect()
        };
    let total = entries.len();
    let shown: Vec<Value> = entries
        .iter()
        .take(limit)
        .map(|entry| entry_payload(entry))
        .collect();
    Ok(json!({
        "entries": shown,
        "total": total,
        "shown": shown.len(),
    }))
}

async fn edit(
    runtime: &TurnBoundaryRuntime,
    data: &Value,
    weak: &std::sync::Weak<TurnBoundaryRequests>,
) -> anyhow::Result<Value> {
    let changes = data
        .get("changes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("history.edit needs a changes list"))?;
    if changes.is_empty() {
        return Err(anyhow::anyhow!("history.edit needs at least one change"));
    }
    let changes = changes
        .iter()
        .map(pa_types::session::ContextChange::parse)
        .collect::<Result<Vec<_>, String>>()
        .map_err(anyhow::Error::msg)?;
    let entry = ContextEditEntry {
        by: "agent".to_string(),
        changes,
    };
    let (snapshot, leaf) = {
        let session = runtime.session.lock().await;
        (
            session.history_snapshot(),
            session.get_leaf_id().map(str::to_owned),
        )
    };
    let entries: Vec<FileEntry> = snapshot.await?;
    let mut walk = crate::session::walk_history(&entries, leaf.as_deref());
    let counts = walk
        .apply(&entry, now_millis())
        .map_err(anyhow::Error::msg)?;
    {
        let mut session = runtime.session.lock().await;
        session.append_context_edit(entry)?;
    }
    let messages = match weak
        .upgrade()
        .and_then(|requests| requests.context_rebuild())
    {
        Some(rebuild) => rebuild().await?,
        None => walk.messages(),
    };
    runtime
        .agent
        .set_messages(
            messages
                .iter()
                .filter_map(super::session_message_to_loop)
                .collect(),
        )
        .await;
    let live = estimate_context_tokens(&messages).tokens;
    let window = runtime.context_window.unwrap_or_default();
    Ok(json!({
        "status": format!(
            "dropped {}, replaced {}, inserted {} \u{2014} live {} / {}",
            counts.dropped,
            counts.replaced,
            counts.inserted,
            thousands(live),
            thousands(window)
        ),
        "live_tokens": live,
        "window": window,
    }))
}

fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::shared::HostRequestPayload;
    use crate::session::manager::SessionManager;
    use crate::session_engine::turn_boundary::{
        ModelInfo, TurnBoundaryRequests, TurnBoundaryRuntime,
    };
    use pa_agent::agent::{Agent, AgentInitialState, AgentOptions};
    use pa_agent::types::ThinkingLevel;
    use pa_types::ai::{UserContent, UserMessage};
    use pa_types::session::AgentMessage;
    use tokio::sync::Mutex;

    fn payload(data: Value) -> HostRequestPayload {
        HostRequestPayload { data }
    }

    fn agent_model() -> pa_agent::types::Model {
        pa_agent::types::Model {
            id: "faux-1".to_string(),
            name: "Faux".to_string(),
            api: "test".to_string(),
            provider: "faux".to_string(),
            base_url: "http://localhost".to_string(),
            reasoning: false,
            cost: pa_agent::types::UsageCost::default(),
            context_window: 100_000,
            max_tokens: 1_000,
            input: Vec::new(),
            max_tokens_explicit: false,
        }
    }

    fn session_with(rows: usize) -> (tempfile::TempDir, SessionManager) {
        let dir = tempfile::TempDir::new().unwrap();
        let session_dir = dir.path().join("session");
        std::fs::create_dir_all(&session_dir).unwrap();
        let mut session = SessionManager::in_memory(dir.path());
        session.materialize_session_file(Some(session_dir)).unwrap();
        for index in 0..rows {
            session
                .append_message(AgentMessage::User(UserMessage {
                    content: UserContent::Text(format!("m{index}")),
                    timestamp: 0,
                    rest: pa_types::JsonMap::new(),
                }))
                .unwrap();
        }
        (dir, session)
    }

    fn registered(
        session: SessionManager,
    ) -> (
        Arc<TurnBoundaryRequests>,
        HostRequestHandlers,
        tempfile::TempDir,
    ) {
        let requests = Arc::new(TurnBoundaryRequests::new());
        let mut handlers = HostRequestHandlers::default();
        register_history_handlers(&requests, &mut handlers);
        let session = Arc::new(Mutex::new(session));
        requests.bind(TurnBoundaryRuntime {
            agent: Arc::new(Agent::new(AgentOptions {
                initial_state: AgentInitialState {
                    system_prompt: Some("s".to_string()),
                    model: Some(agent_model()),
                    thinking_level: Some(ThinkingLevel::Off),
                    tools: None,
                    messages: None,
                },
                stream_fn: None,
                ..Default::default()
            })),
            session,
            context_window: Some(200_000),
            model_info: ModelInfo {
                id: "faux-1".to_string(),
                provider: "faux".to_string(),
                input: Vec::new(),
            },
        });
        let dir = tempfile::TempDir::new().unwrap();
        (requests, handlers, dir)
    }

    fn handler(handlers: &HostRequestHandlers, name: &str) -> crate::kernel::shared::HostHandlerFn {
        handlers.get(name).unwrap().clone()
    }

    #[tokio::test]
    async fn index_totals_match_the_request_built_at_send() {
        let (_dir, session) = session_with(40);
        let (requests, handlers, _tmp) = registered(session);
        let response = handler(&handlers, "history.index")(payload(serde_json::json!({
            "order": "tokens",
            "limit": 5,
        })))
        .await
        .unwrap();
        let runtime = requests.bound().unwrap();
        let entries = runtime.session.lock().await.retained_entries().to_vec();
        let request = crate::session::build_session_context(&entries, None).messages;
        let expected = estimate_context_tokens(&request).tokens;
        assert_eq!(response["live_tokens"], expected);
        assert_eq!(response["entries"], 40);
        assert_eq!(response["window"], 200_000);
        assert_eq!(response["rows"].as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn get_reports_the_remainder_of_an_over_limit_region() {
        let (_dir, session) = session_with(50);
        let (_requests, handlers, _tmp) = registered(session);
        let get = handler(&handlers, "history.get");
        let region = get(payload(serde_json::json!({
            "selector": {"kind": "range", "start": 0, "end": 45},
            "limit": 10,
        })))
        .await
        .unwrap();
        assert_eq!(region["entries"].as_array().unwrap().len(), 10);
        assert_eq!(region["total"], 45);
        assert_eq!(region["shown"], 10);

        let last = get(payload(serde_json::json!({
            "selector": {"kind": "position", "value": -1},
            "limit": 10,
        })))
        .await
        .unwrap();
        assert_eq!(last["entries"].as_array().unwrap().len(), 1);
        assert_eq!(last["entries"][0]["text"], "m49");
    }

    #[tokio::test]
    async fn rejected_edit_names_the_target_and_leaves_the_file_unchanged() {
        let (_dir, session) = session_with(6);
        let file = session.get_session_file().unwrap().to_owned();
        let (requests, handlers, _tmp) = registered(session);
        let before = std::fs::read(&file).unwrap();
        let edit = handler(&handlers, "history.edit");
        let error = edit(payload(serde_json::json!({
            "changes": [{"op": "drop", "at": 99}],
        })))
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("off the current path"),
            "{error}"
        );
        assert!(error.to_string().contains("99"), "{error}");
        assert_eq!(std::fs::read(&file).unwrap(), before);

        let unknown = edit(payload(serde_json::json!({
            "changes": [{"op": "explode", "at": 1}],
        })))
        .await
        .unwrap_err();
        assert!(unknown.to_string().contains("unknown op"), "{unknown}");
        assert_eq!(std::fs::read(&file).unwrap(), before);
        drop(requests);
    }

    #[tokio::test]
    async fn edit_writes_one_row_and_the_next_request_drops_the_region() {
        let (_dir, session) = session_with(10);
        let file = session.get_session_file().unwrap().to_owned();
        let (requests, handlers, _tmp) = registered(session);
        let status = handler(&handlers, "history.edit")(payload(serde_json::json!({
            "changes": [
                {"op": "drop", "at": [0, 3]},
                {"op": "replace", "at": 3, "text": "review: 28 frames admitted"},
                {"op": "insert", "text": "STATE: reading", "role": "notes"},
            ],
        })))
        .await
        .unwrap();
        assert_eq!(
            status["status"],
            "dropped 3, replaced 1, inserted 1 \u{2014} live 13 / 200,000"
        );

        let runtime = requests.bound().unwrap();
        let rows = {
            let session = runtime.session.lock().await;
            let entries = session.retained_entries().to_vec();
            entries
                .iter()
                .filter(|entry| matches!(entry, FileEntry::ContextEdit { .. }))
                .count()
        };
        assert_eq!(rows, 1, "one edit row, whatever the change count");
        let entries = {
            let session = runtime.session.lock().await;
            session.retained_entries().to_vec()
        };
        let messages = crate::session::build_session_context(&entries, None).messages;
        let texts: Vec<String> = messages
            .iter()
            .map(|message| match message {
                AgentMessage::User(user) => user.content.text(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(texts.len(), 7);
        assert_eq!(texts[0], "review: 28 frames admitted");
        assert_eq!(texts[1], "m4");
        assert!(!texts.contains(&"STATE: reading".to_string()));
        assert!(std::fs::read(&file).unwrap().starts_with(b"{"));

        let path = handler(&handlers, "history.path")(payload(serde_json::json!({})))
            .await
            .unwrap();
        let live = path["entries"].as_array().unwrap();
        assert_eq!(live.len(), 8);
        assert_eq!(live.last().unwrap()["role"], "notes");
    }
}
