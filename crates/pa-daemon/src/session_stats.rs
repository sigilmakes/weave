//! Recorded session totals and the engine's model-facing context usage.

use serde_json::{json, Value};

use crate::session_store::SessionFile;

pub fn session_stats(store: &SessionFile, live_context_usage: Option<Value>) -> Value {
    let (durable_messages, boundary) = kept_region_messages(store);
    let mut user_messages = 0u64;
    let mut assistant_messages = 0u64;
    let mut tool_results = 0u64;
    let mut tool_calls = 0u64;
    let mut input = 0u64;
    let mut output = 0u64;
    let mut cache_read = 0u64;
    let mut cache_write = 0u64;
    let mut cost = 0.0;
    for message in &durable_messages {
        match message.get("role").and_then(Value::as_str) {
            Some("user") => user_messages += 1,
            Some("assistant") => {
                assistant_messages += 1;
                tool_calls += tool_call_count(message);
                if let Some(usage) = message.get("usage") {
                    input += usage
                        .get("input")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    output += usage
                        .get("output")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    cache_read += usage
                        .get("cacheRead")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    cache_write += usage
                        .get("cacheWrite")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    cost += usage
                        .get("cost")
                        .and_then(|cost| cost.get("total"))
                        .and_then(Value::as_f64)
                        .unwrap_or_default();
                }
            }
            Some("toolResult") => tool_results += 1,
            _ => {}
        }
    }
    // The window is a load optimization, not a session state: without a
    // compaction boundary the kept region covers the whole chain, and with
    // a boundary the retained region IS the kept region.
    let mut older_total_messages = 0u64;
    if boundary.is_none() {
        if let Some(window) = &store.window {
            let older = &window.older_path_stats;
            user_messages += older.user_messages;
            assistant_messages += older.assistant_messages;
            tool_results += older.tool_results;
            tool_calls += older.tool_calls;
            input += older.input;
            output += older.output;
            cache_read += older.cache_read;
            cache_write += older.cache_write;
            cost += older.cost;
            older_total_messages = older.total_messages;
        }
    }
    let total_messages = durable_messages.len() as u64 + older_total_messages;
    let mut stats = json!({
        "sessionFile": store.path.display().to_string(),
        "sessionId": store.session_id(),
        "userMessages": user_messages,
        "assistantMessages": assistant_messages,
        "toolCalls": tool_calls,
        "toolResults": tool_results,
        "totalMessages": total_messages,
        "tokens": {
            "input": input,
            "output": output,
            "cacheRead": cache_read,
            "cacheWrite": cache_write,
            "total": input + output + cache_read + cache_write,
        },
        "cost": cost,
    });
    if let Some(usage) = live_context_usage {
        stats["contextUsage"] = usage;
    }
    stats
}

/// The messages after the latest ON-CHAIN compaction: the active branch,
/// gap-bridged, restricted to the kept region (an off-chain compaction never
/// bounds it). Returns the messages and the boundary position (`None` = all kept).
fn kept_region_messages(store: &SessionFile) -> (Vec<&Value>, Option<usize>) {
    let entries = store.entries();
    let chain = store.branch_bridged_positions();
    // The latest compaction ON THE CHAIN bounds the kept region; its
    // `firstKeptEntryId` names the first row the post-compaction reload keeps.
    // A torn write that lost the boundary row falls back to the compaction entry.
    let boundary = chain
        .iter()
        .rev()
        .find(|position| entries[**position].type_ == "compaction")
        .map(|compaction| {
            entries[*compaction]
                .fields
                .get("firstKeptEntryId")
                .and_then(Value::as_str)
                .and_then(|id| chain.iter().find(|position| entries[**position].id == id))
                .unwrap_or(compaction)
        });
    let messages = chain
        .iter()
        .filter(|position| match boundary {
            Some(boundary) => **position >= *boundary,
            None => true,
        })
        .filter(|position| entries[**position].type_ == "message")
        .filter_map(|position| entries[*position].fields.get("message"))
        .collect();
    (messages, boundary.copied())
}

pub(crate) fn unavailable_context_usage(context_window: Option<u64>, reason: &str) -> Value {
    json!({
        "tokens": Value::Null,
        "contextWindow": context_window.filter(|window| *window > 0),
        "percent": Value::Null,
        "unavailable": reason,
    })
}

pub(crate) async fn engine_context_usage(engine: &dyn crate::engine::SessionEngine) -> Value {
    match engine.live_context_usage().await {
        Ok(Some(usage)) => json!({
            "tokens": usage.tokens,
            "contextWindow": usage.context_window,
            "percent": usage.percent,
        }),
        Ok(None) => unavailable_context_usage(None, "model context window is unknown"),
        Err(error) => {
            unavailable_context_usage(engine.model_context_window(), &format!("{error:#}"))
        }
    }
}

/// `toolCall` content blocks on one assistant message.
fn tool_call_count(message: &Value) -> u64 {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))
                .count() as u64
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
