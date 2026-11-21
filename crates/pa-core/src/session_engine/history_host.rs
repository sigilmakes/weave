//! The `history.*` host bridge: the kernel's `history` namespace reads the
//! live view, and `history.edit` writes one `context_edit` row.

use std::sync::Arc;

use serde_json::{json, Value};

use pa_types::session::{ContextEditEntry, ContextRole, ContextSelector, ContextTarget};

use crate::kernel::shared::{host_handler, HostRequestHandlers};
use crate::session::{HistoryEntry, HistoryWalk};
use pa_types::session::FileEntry;

use super::turn_boundary::{TurnBoundaryRequests, TurnBoundaryRuntime};

/// Largest per-entry text `history.path` sends back; a path previews its
/// entries and `history.get` returns one entry in full.
const PATH_PREVIEW_CHARS: usize = 200;

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
                let requests = weak.upgrade().ok_or_else(|| {
                    anyhow::anyhow!("the session ended before the request could be served")
                })?;
                let walk = load_walk(&runtime).await?;
                let live = requests.assemble_live_view().await?;
                let window = runtime.context_window.unwrap_or_default();
                Ok(index_payload(&walk, &payload.data, window, &live))
            })
        }),
    );
    let weak = Arc::downgrade(requests);
    handlers.register(
        "history.path",
        host_handler(move |payload| {
            let weak = weak.clone();
            Box::pin(async move {
                let runtime = bound_runtime(&weak)?;
                let walk = load_walk(&runtime).await?;
                let limit = payload
                    .data
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map_or(500, |limit| limit.max(1) as usize);
                let total = walk.entries.len();
                let entries: Vec<Value> = walk
                    .entries
                    .iter()
                    .take(limit)
                    .map(|entry| {
                        let mut value = entry_payload(entry);
                        if let Some(object) = value.as_object_mut() {
                            let text = entry
                                .text
                                .chars()
                                .take(PATH_PREVIEW_CHARS)
                                .collect::<String>();
                            let truncated = text.len() < entry.text.len();
                            object.insert("text".to_string(), Value::String(text));
                            object.insert("truncated".to_string(), Value::Bool(truncated));
                        }
                        value
                    })
                    .collect();
                Ok(json!({
                    "entries": entries,
                    "total": total,
                    "shown": entries.len(),
                    "skipped_edits": walk.skipped_edits,
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

/// The refresh the turn loop calls before its first request and after every
/// tool batch: when `history.edit` marked the view stale, re-assemble it,
/// replace the transcript without dropping a message that arrived while it was
/// being assembled, and hand the loop the transcript it must send.
#[must_use]
pub fn refresh_hook(
    requests: std::sync::Weak<TurnBoundaryRequests>,
    agent: std::sync::Weak<pa_agent::agent::Agent>,
) -> pa_agent::agent_loop::RefreshContextFn {
    Arc::new(move || {
        let requests = requests.clone();
        let agent = agent.clone();
        Box::pin(async move {
            let Some(requests) = requests.upgrade() else {
                return Ok(None);
            };
            if !requests.take_refresh() {
                return Ok(None);
            }
            let Some(agent) = agent.upgrade() else {
                requests.mark_refresh();
                return Ok(None);
            };
            let seen = agent.state().await.messages.len();
            let live = match requests.assemble_live_view().await {
                Ok(live) => live,
                Err(error) => {
                    requests.mark_refresh();
                    return Err(error);
                }
            };
            let view: Vec<pa_agent::types::AgentMessage> = live
                .messages
                .iter()
                .filter_map(super::session_message_to_loop)
                .collect();
            let merged = agent
                .mutate_messages(move |current| {
                    let arrived: Vec<pa_agent::types::AgentMessage> =
                        current.get(seen..).unwrap_or_default().to_vec();
                    *current = view;
                    for message in arrived {
                        if !current.contains(&message) {
                            current.push(message);
                        }
                    }
                })
                .await;
            Ok(Some(merged))
        })
            as pa_agent::BoxFut<'static, anyhow::Result<Option<Vec<pa_agent::types::AgentMessage>>>>
    })
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
    let mut value = json!({
        "turn": entry.turn,
        "role": entry.role.as_str(),
        "tokens": entry.tokens,
        "text": entry.text,
    });
    if let Some(id) = entry.id.as_deref() {
        if let Some(object) = value.as_object_mut() {
            object.insert("id".to_string(), Value::String(id.to_string()));
        }
    }
    value
}

fn preview(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 160 {
        return flat;
    }
    flat.chars().take(160).collect()
}

fn index_payload(
    walk: &HistoryWalk,
    data: &Value,
    window: u64,
    live: &super::turn_boundary::LiveViewSnapshot,
) -> Value {
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

    let live_tokens = live.tokens;
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
            let mut row = json!({
                "turn": entry.turn,
                "role": entry.role.as_str(),
                "tokens": entry.tokens,
                "share": share(entry.tokens),
                "preview": preview(&entry.text),
            });
            if let Some(id) = entry.id.as_deref() {
                if let Some(object) = row.as_object_mut() {
                    object.insert("id".to_string(), Value::String(id.to_string()));
                }
            }
            row
        })
        .collect();

    json!({
        "entries": walk.entries.len(),
        "live_tokens": live_tokens,
        "window": window,
        "over_by": live_tokens.saturating_sub(window),
        "hidden": walk.hidden.len(),
        "hidden_tokens": hidden_tokens,
        "skipped_edits": walk.skipped_edits,
        "shown": rows.len(),
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
    let requests = weak
        .upgrade()
        .ok_or_else(|| anyhow::anyhow!("the session ended before the edit could be served"))?;
    requests.mark_refresh();
    let window = runtime.context_window.unwrap_or_default();
    let counts_line = format!(
        "dropped {}, replaced {}, inserted {}",
        counts.dropped, counts.replaced, counts.inserted
    );
    let Ok(live) = requests.assemble_live_view().await else {
        // The row is written and the walk validated it before the write, so a
        // failed assembly costs the figure, not the edit; retrying would only
        // append a second row.
        return Ok(json!({
            "status": format!(
                "{counts_line} \u{2014} written; live unmeasured (the view did not assemble)"
            ),
            "live_tokens": Value::Null,
            "window": window,
        }));
    };
    let live_tokens = live.tokens;
    Ok(json!({
        "status": format!(
            "{counts_line} \u{2014} live {} / {}",
            thousands(live_tokens),
            thousands(window)
        ),
        "live_tokens": live_tokens,
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
    use crate::session_engine::compaction::estimate_context_tokens;
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
        Arc<Mutex<SessionManager>>,
        tempfile::TempDir,
    ) {
        let requests = Arc::new(TurnBoundaryRequests::new());
        let mut handlers = HostRequestHandlers::default();
        register_history_handlers(&requests, &mut handlers);
        let session = Arc::new(Mutex::new(session));
        requests.set_live_view(Arc::new({
            let session = Arc::clone(&session);
            move || {
                let session = Arc::clone(&session);
                Box::pin(async move {
                    let context = { session.lock().await.active_context() };
                    let tokens = crate::session_engine::compaction::context_tokens_with_drift(
                        &context.messages,
                        context.usage_drift,
                    );
                    Ok(crate::session_engine::turn_boundary::LiveViewSnapshot {
                        messages: context.messages,
                        drift: context.usage_drift,
                        tokens,
                    })
                })
            }
        }));
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
            session: Arc::clone(&session),
            context_window: Some(200_000),
            model_info: ModelInfo {
                id: "faux-1".to_string(),
                provider: "faux".to_string(),
                input: Vec::new(),
            },
        });
        let dir = tempfile::TempDir::new().unwrap();
        (requests, handlers, session, dir)
    }

    fn handler(handlers: &HostRequestHandlers, name: &str) -> crate::kernel::shared::HostHandlerFn {
        handlers.get(name).unwrap().clone()
    }

    #[tokio::test]
    async fn index_totals_match_the_request_built_at_send() {
        let (_dir, session) = session_with(40);
        let (requests, handlers, _session, _tmp) = registered(session);
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
        let (_requests, handlers, _session, _tmp) = registered(session);
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
        let (requests, handlers, _session, _tmp) = registered(session);
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
        let (requests, handlers, _session, _tmp) = registered(session);
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

    fn user_texts(messages: &[pa_agent::types::Message]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|message| match message {
                pa_agent::types::Message::User(user) => Some(match &user.content {
                    pa_agent::types::UserContent::Text(text) => text.clone(),
                    pa_agent::types::UserContent::Parts(_) => String::new(),
                }),
                _ => None,
            })
            .collect()
    }

    struct EditTool {
        edit: crate::kernel::shared::HostHandlerFn,
        changes: Value,
    }

    static EDIT_SCHEMA: std::sync::LazyLock<Value> =
        std::sync::LazyLock::new(|| json!({"type": "object"}));

    impl pa_agent::types::AgentTool for EditTool {
        fn name(&self) -> &'static str {
            "history_edit"
        }

        fn description(&self) -> &'static str {
            "edit the history view"
        }

        fn parameters(&self) -> &Value {
            &EDIT_SCHEMA
        }

        fn execute(
            self: Arc<Self>,
            _tool_call_id: String,
            _params: Value,
            _signal: pa_agent::abort::AbortSignal,
            _on_update: pa_agent::types::AgentToolUpdateCallback,
        ) -> pa_agent::BoxFut<'static, anyhow::Result<pa_agent::types::AgentToolResult>> {
            let edit = Arc::clone(&self.edit);
            let changes = self.changes.clone();
            Box::pin(async move {
                let response = edit(payload(json!({ "changes": changes }))).await?;
                let status = response["status"].as_str().unwrap_or_default().to_string();
                Ok(pa_agent::types::AgentToolResult::text(status))
            })
        }
    }

    #[tokio::test]
    async fn an_edit_reaches_the_next_request_of_the_same_run() {
        let (_dir, session) = session_with(6);
        let (requests, handlers, session, _tmp) = registered(session);
        let initial = {
            let context = session.lock().await.active_context();
            context
                .messages
                .iter()
                .filter_map(super::super::session_message_to_loop)
                .collect::<Vec<_>>()
        };
        let provider = Arc::new(pa_agent::scripted::ScriptedProvider::new(agent_model()));
        provider.push_tool_call_turn(None, vec![("call-1", "history_edit", json!({}))]);
        provider.push_text_turn("done");
        let agent = Arc::new(Agent::new(AgentOptions {
            initial_state: AgentInitialState {
                system_prompt: Some("s".to_string()),
                model: Some(agent_model()),
                thinking_level: Some(ThinkingLevel::Off),
                tools: Some(vec![Arc::new(EditTool {
                    edit: handler(&handlers, "history.edit"),
                    changes: json!([{"op": "drop", "at": [0, 3]}]),
                })]),
                messages: Some(initial),
            },
            stream_fn: Some(provider.stream_fn()),
            ..Default::default()
        }));
        let _persistence = agent
            .subscribe({
                let session = Arc::clone(&session);
                move |event, _signal| {
                    let session = Arc::clone(&session);
                    Box::pin(async move {
                        super::super::persist_event(&session, event).await?;
                        Ok(())
                    })
                }
            })
            .await;
        agent.set_refresh_context(Some(refresh_hook(
            Arc::downgrade(&requests),
            Arc::downgrade(&agent),
        )));
        agent.prompt("go").await.unwrap();

        let calls = provider.calls();
        assert_eq!(calls.len(), 2, "one request per turn");
        let first = user_texts(&calls[0].messages);
        assert!(first.contains(&"m0".to_string()), "{first:?}");
        let second = user_texts(&calls[1].messages);
        assert!(!second.contains(&"m0".to_string()), "{second:?}");
        assert!(!second.contains(&"m2".to_string()), "{second:?}");
        assert!(second.contains(&"m3".to_string()), "{second:?}");
        assert!(second.contains(&"m5".to_string()), "{second:?}");
        let last_call = &calls[1].messages;
        assert!(
            last_call
                .iter()
                .any(|message| matches!(message, pa_agent::types::Message::ToolResult(_))),
            "the edit's own tool result stays in the request"
        );
    }

    #[tokio::test]
    async fn get_by_id_and_by_slice_return_the_same_entry() {
        let (_dir, session) = session_with(8);
        let (_requests, handlers, session, _tmp) = registered(session);
        let id = {
            let session = session.lock().await;
            let entries = session.retained_entries().to_vec();
            crate::session::walk_history(&entries, None).entries[3]
                .id
                .clone()
                .unwrap()
        };
        let get = handler(&handlers, "history.get");
        let by_id = get(payload(json!({"selector": {"kind": "id", "value": id}})))
            .await
            .unwrap();
        let by_slice = get(payload(json!({
            "selector": {"kind": "range", "start": 3, "end": 4},
        })))
        .await
        .unwrap();
        assert_eq!(by_id["entries"], by_slice["entries"]);
        assert_eq!(by_id["entries"][0]["text"], "m3");
        assert_eq!(by_id["total"], 1);

        let by_position = get(payload(
            json!({"selector": {"kind": "position", "value": 3}}),
        ))
        .await
        .unwrap();
        assert_eq!(by_position["entries"], by_id["entries"]);
    }

    #[tokio::test]
    async fn a_bounded_path_reports_its_remainder_and_previews() {
        let (_dir, session) = session_with(4);
        let (requests, handlers, _session, _tmp) = registered(session);
        handler(&handlers, "history.edit")(payload(json!({
            "changes": [{"op": "replace", "at": 1, "text": "x".repeat(300)}],
        })))
        .await
        .unwrap();
        let path = handler(&handlers, "history.path")(payload(json!({"limit": 2})))
            .await
            .unwrap();
        assert_eq!(path["entries"].as_array().unwrap().len(), 2);
        assert_eq!(path["total"], 4);
        assert_eq!(path["shown"], 2);
        assert_eq!(path["entries"][1]["truncated"], true);
        assert_eq!(
            path["entries"][1]["text"].as_str().unwrap().chars().count(),
            PATH_PREVIEW_CHARS
        );
        drop(requests);
    }

    #[tokio::test]
    async fn an_inserted_note_carries_no_id_and_addresses_by_position() {
        let (_dir, session) = session_with(3);
        let (_requests, handlers, _session, _tmp) = registered(session);
        handler(&handlers, "history.edit")(payload(json!({
            "changes": [{"op": "insert", "text": "STATE"}],
        })))
        .await
        .unwrap();
        let path = handler(&handlers, "history.path")(payload(json!({})))
            .await
            .unwrap();
        let note = path["entries"].as_array().unwrap().last().unwrap();
        assert_eq!(note["role"], "notes");
        assert!(note.get("id").is_none(), "{note}");
        let get = handler(&handlers, "history.get")(payload(json!({
            "selector": {"kind": "position", "value": -1},
        })))
        .await
        .unwrap();
        assert_eq!(get["entries"][0]["text"], "STATE");
    }

    fn user_message(text: &str) -> pa_agent::types::AgentMessage {
        pa_agent::types::AgentMessage::Standard(pa_agent::types::Message::User(
            pa_agent::types::UserMessage {
                content: pa_agent::types::UserContent::Text(text.to_string()),
                timestamp: 0,
                rest: pa_types::JsonMap::new(),
            },
        ))
    }

    #[tokio::test]
    async fn a_refresh_sends_a_message_that_arrived_while_the_view_was_assembled() {
        let (_dir, session) = session_with(4);
        let (requests, _handlers, session, _tmp) = registered(session);
        let initial: Vec<pa_agent::types::AgentMessage> = {
            let context = session.lock().await.active_context();
            context
                .messages
                .iter()
                .filter_map(super::super::session_message_to_loop)
                .collect()
        };
        let agent = Arc::new(Agent::new(AgentOptions {
            initial_state: AgentInitialState {
                system_prompt: Some("s".to_string()),
                model: Some(agent_model()),
                thinking_level: Some(ThinkingLevel::Off),
                tools: None,
                messages: Some(initial),
            },
            ..Default::default()
        }));
        let arrival = user_message("arrival");
        requests.set_live_view(Arc::new({
            let session = Arc::clone(&session);
            let agent = Arc::downgrade(&agent);
            let arrival = arrival.clone();
            move || {
                let session = Arc::clone(&session);
                let agent = agent.clone();
                let arrival = arrival.clone();
                Box::pin(async move {
                    if let Some(agent) = agent.upgrade() {
                        let arrival = arrival.clone();
                        agent
                            .mutate_messages(move |messages| {
                                if !messages.contains(&arrival) {
                                    messages.push(arrival);
                                }
                            })
                            .await;
                    }
                    let context = { session.lock().await.active_context() };
                    let tokens = crate::session_engine::compaction::context_tokens_with_drift(
                        &context.messages,
                        context.usage_drift,
                    );
                    Ok(crate::session_engine::turn_boundary::LiveViewSnapshot {
                        messages: context.messages,
                        drift: context.usage_drift,
                        tokens,
                    })
                })
            }
        }));
        let hook = refresh_hook(Arc::downgrade(&requests), Arc::downgrade(&agent));
        assert!(
            (hook)().await.unwrap().is_none(),
            "no edit is pending, so the loop keeps its transcript"
        );

        requests.mark_refresh();
        let sent = (hook)().await.unwrap().unwrap();
        assert_eq!(
            user_texts_loop(&sent),
            vec!["m0", "m1", "m2", "m3", "arrival"],
            "the request carries the rebuilt view and the late message, once"
        );
        assert_eq!(
            user_texts_loop(&agent.state().await.messages),
            vec!["m0", "m1", "m2", "m3", "arrival"],
            "the transcript the loop reads back is the one it sends"
        );
        assert!(
            (hook)().await.unwrap().is_none(),
            "one assembly serves one edit"
        );
    }

    #[tokio::test]
    async fn an_inserted_user_row_is_sent_once_and_its_drop_sticks() {
        let (_dir, session) = session_with(3);
        let (requests, handlers, session, _tmp) = registered(session);
        let initial: Vec<pa_agent::types::AgentMessage> = {
            let context = session.lock().await.active_context();
            context
                .messages
                .iter()
                .filter_map(super::super::session_message_to_loop)
                .collect()
        };
        let agent = Arc::new(Agent::new(AgentOptions {
            initial_state: AgentInitialState {
                system_prompt: Some("s".to_string()),
                model: Some(agent_model()),
                thinking_level: Some(ThinkingLevel::Off),
                tools: None,
                messages: Some(initial),
            },
            ..Default::default()
        }));
        let hook = refresh_hook(Arc::downgrade(&requests), Arc::downgrade(&agent));
        let edit = handler(&handlers, "history.edit");

        edit(payload(json!({
            "changes": [{"op": "insert", "text": "STATE", "role": "user", "after": -1}],
        })))
        .await
        .unwrap();
        let sent = (hook)().await.unwrap().unwrap();
        assert_eq!(user_texts_loop(&sent), vec!["m0", "m1", "m2", "STATE"]);

        edit(payload(json!({"changes": [{"op": "drop", "at": -1}]})))
            .await
            .unwrap();
        let sent = (hook)().await.unwrap().unwrap();
        assert_eq!(
            user_texts_loop(&sent),
            vec!["m0", "m1", "m2"],
            "a view-only row is not an arrival on the next refresh"
        );
        assert_eq!(
            user_texts_loop(&agent.state().await.messages),
            vec!["m0", "m1", "m2"]
        );
    }

    #[tokio::test]
    async fn compact_status_reports_the_figure_history_index_reports_after_an_edit() {
        let (_dir, session) = session_with(12);
        let (requests, mut handlers, _session, _tmp) = registered(session);
        requests.register_compact_handlers(&mut handlers, 1_000);
        let index = handler(&handlers, "history.index");
        let status = handler(&handlers, "compact.status");

        let before = index(payload(json!({"limit": 1}))).await.unwrap();
        handler(&handlers, "history.edit")(payload(json!({
            "changes": [{"op": "drop", "at": [0, 6]}],
        })))
        .await
        .unwrap();
        let after = index(payload(json!({"limit": 1}))).await.unwrap();
        assert!(after["live_tokens"].as_u64() < before["live_tokens"].as_u64());

        let compact = status(payload(json!({}))).await.unwrap();
        assert_eq!(compact["tokens"], after["live_tokens"]);
        assert_eq!(compact["context_window"], after["window"]);
        let expected_percent = after["live_tokens"].as_u64().unwrap() as f64 / 200_000.0 * 100.0;
        assert!((compact["percent"].as_f64().unwrap() - expected_percent).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_failed_assembly_keeps_the_refresh_pending() {
        let (_dir, session) = session_with(2);
        let (requests, _handlers, _session, _tmp) = registered(session);
        let agent = Arc::new(Agent::new(AgentOptions {
            initial_state: AgentInitialState {
                system_prompt: Some("s".to_string()),
                model: Some(agent_model()),
                thinking_level: Some(ThinkingLevel::Off),
                tools: None,
                messages: None,
            },
            ..Default::default()
        }));
        let hook = refresh_hook(Arc::downgrade(&requests), Arc::downgrade(&agent));
        requests.set_live_view(Arc::new(|| {
            Box::pin(async { Err(anyhow::anyhow!("assembly failed")) })
        }));
        requests.mark_refresh();
        assert!((hook)().await.is_err());
        assert!(
            requests.take_refresh(),
            "the next request must still rebuild the view"
        );
    }

    #[tokio::test]
    async fn the_refresh_hook_does_not_keep_its_agent_alive() {
        let (_dir, session) = session_with(1);
        let requests = Arc::new(TurnBoundaryRequests::new());
        let agent = Arc::new(Agent::new(AgentOptions {
            initial_state: AgentInitialState {
                system_prompt: Some("s".to_string()),
                model: Some(agent_model()),
                thinking_level: Some(ThinkingLevel::Off),
                tools: None,
                messages: None,
            },
            ..Default::default()
        }));
        requests.bind(TurnBoundaryRuntime {
            agent: Arc::clone(&agent),
            session: Arc::new(Mutex::new(session)),
            context_window: Some(200_000),
            model_info: ModelInfo {
                id: "faux-1".to_string(),
                provider: "faux".to_string(),
                input: Vec::new(),
            },
        });
        agent.set_refresh_context(Some(refresh_hook(
            Arc::downgrade(&requests),
            Arc::downgrade(&agent),
        )));
        let weak = Arc::downgrade(&agent);
        drop(agent);
        assert!(
            weak.upgrade().is_some(),
            "the bound runtime still holds its agent"
        );
        drop(requests);
        assert!(
            weak.upgrade().is_none(),
            "the hook must not close a cycle back to its agent: a live kernel would outlive the session"
        );
    }

    fn user_texts_loop(messages: &[pa_agent::types::AgentMessage]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|message| match message {
                pa_agent::types::AgentMessage::Standard(pa_agent::types::Message::User(user)) => {
                    Some(match &user.content {
                        pa_agent::types::UserContent::Text(text) => text.clone(),
                        pa_agent::types::UserContent::Parts(_) => String::new(),
                    })
                }
                _ => None,
            })
            .collect()
    }
}
