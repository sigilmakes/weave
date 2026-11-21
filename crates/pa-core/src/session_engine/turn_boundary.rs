//! The turn-boundary host-request surface: `model.info` and `compact.*`,
//! reached by kernel-side skill modules through `rlm.host_request`.
//! `compact.run` only SCHEDULES: executing inside the active turn
//! would abort the requesting run, so the pending request is stored here.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pa_agent::agent::Agent;
use pa_types::session::{AgentMessage, FileEntry};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::kernel::shared::{host_handler, HostRequestHandlers};
use crate::session::manager::SessionManager;

use super::compact_session::{prepare_compaction, CompactSkip};
use super::engine::SessionEngine;

/// A scheduled compaction (kernel `compact.run`).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingCompaction {
    pub instructions: Option<String>,
}

/// The model facts `model.info` reports (TS answers nulls when the session
/// has no model; the Rust engine always resolves one).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    /// Input modalities (empty when the resolved model did not declare
    /// any, e.g. harness-converted minimal models).
    pub input: Vec<pa_types::ai::ModelInput>,
}

/// The runtime the handlers read once the session is assembled;
/// `create_session` binds this before returning.
pub struct TurnBoundaryRuntime {
    pub agent: Arc<Agent>,
    pub session: Arc<Mutex<SessionManager>>,
    /// The resolved model's context window; `None` when unknown (compact
    /// status answers null tokens then).
    pub context_window: Option<u64>,
    pub model_info: ModelInfo,
}

/// Estimated context usage (TS `getContextUsage`): the last valid assistant
/// usage plus trailing message estimates; `tokens`/`percent` are `None`
/// right after a compaction without a usable post-compaction usage.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextUsage {
    pub tokens: Option<u64>,
    pub context_window: u64,
    pub percent: Option<f64>,
}

/// One assembled live view and its request token figure, including native
/// archive input when no provider usage describes the current request.
#[derive(Debug, Clone)]
pub struct LiveViewSnapshot {
    pub messages: Vec<AgentMessage>,
    pub drift: crate::session::UsageDrift,
    pub tokens: u64,
}

/// Assembles the live view from the canonical session, through the same path a
/// send uses, so a pinned context strategy still shapes it.
pub type LiveView = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<LiveViewSnapshot>> + Send>>
        + Send
        + Sync,
>;

/// The turn-boundary state: pending requests plus the late-bound runtime,
/// shared between the host bridge and the turn loop; build it in `Arc` form.
#[derive(Default)]
pub struct TurnBoundaryRequests {
    /// The late-bound runtime; `bind` is first-wins, and `rebind_model_facts`
    /// swaps the model facts after a live model switch.
    runtime: std::sync::RwLock<Option<Arc<TurnBoundaryRuntime>>>,
    compaction: Mutex<Option<PendingCompaction>>,
    live_view: std::sync::RwLock<Option<LiveView>>,
    /// Set by `history.edit` and taken by the turn loop: the running loop still
    /// streams from a private copy of the view, so its next request must be
    /// rebuilt before the provider sees it.
    pending_refresh: std::sync::atomic::AtomicBool,
}

impl TurnBoundaryRequests {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the assembled session runtime (first bind wins).
    pub fn bind(&self, runtime: TurnBoundaryRuntime) {
        let mut cell = self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cell.is_none() {
            *cell = Some(Arc::new(runtime));
        }
    }

    /// `None` until the session is assembled.
    pub fn bound(&self) -> Option<Arc<TurnBoundaryRuntime>> {
        self.runtime
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Re-bind the model facts after a live model switch (the agent and
    /// session cells stay).
    pub fn rebind_model_facts(&self, model_info: ModelInfo, context_window: Option<u64>) {
        let Some(current) = self.bound() else {
            return;
        };
        let updated = TurnBoundaryRuntime {
            agent: Arc::clone(&current.agent),
            session: Arc::clone(&current.session),
            context_window,
            model_info,
        };
        *self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(updated));
    }

    /// Take the pending compaction (the turn boundary consumes it once).
    pub async fn take_compaction(&self) -> Option<PendingCompaction> {
        self.compaction.lock().await.take()
    }

    /// Schedule a compaction for the next turn boundary: the request's
    /// instructions win, an absent one keeps what was already scheduled.
    pub async fn schedule_compaction(&self, instructions: Option<String>) {
        let mut slot = self.compaction.lock().await;
        let merged = PendingCompaction {
            instructions: instructions.or_else(|| {
                slot.as_ref()
                    .and_then(|current| current.instructions.clone())
            }),
        };
        *slot = Some(merged);
    }

    /// Whether a compaction is scheduled (TS `compact.status` `scheduled`).
    pub async fn compaction_scheduled(&self) -> bool {
        self.compaction.lock().await.is_some()
    }

    /// Install the live-view assembly a canonical-history write uses.
    pub fn set_live_view(&self, live_view: LiveView) {
        *self
            .live_view
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(live_view);
    }

    /// The installed live-view assembly, when the embedding supplied one.
    #[must_use]
    pub fn live_view(&self) -> Option<LiveView> {
        self.live_view
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Mark the running loop's view stale after a canonical-history write.
    pub fn mark_refresh(&self) {
        self.pending_refresh
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Consume the stale marker; `true` means the loop must re-read the view
    /// before its next stream request.
    pub fn take_refresh(&self) -> bool {
        self.pending_refresh
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    /// Assemble the live view, when an embedding installed one.
    ///
    /// # Errors
    ///
    /// Returns the assembly failure, and reports a missing seam rather than
    /// silently serving a direct walk.
    pub async fn assemble_live_view(&self) -> anyhow::Result<LiveViewSnapshot> {
        let live_view = self
            .live_view()
            .ok_or_else(|| anyhow::anyhow!("the session has no live-view assembly"))?;
        live_view().await
    }

    /// The scheduled compaction without consuming it (the consumer
    /// announces the run with the pending instructions first).
    pub async fn scheduled_compaction(&self) -> Option<PendingCompaction> {
        self.compaction.lock().await.clone()
    }

    /// Drop pending requests so an aborted turn cannot leak them into the next turn.
    pub async fn clear_pending(&self) {
        *self.compaction.lock().await = None;
    }

    /// Register `model.info` (always present, like the TS `_hostHandlers`
    /// default map).
    pub fn register_model_info_handler(
        self: &Arc<Self>,
        handlers: &mut HostRequestHandlers,
        model_info: ModelInfo,
    ) {
        // Weak: the kernel holds these handlers for its whole life, so a
        // strong capture here loops the ownership graph and pins a dropped
        // session's kernel process until exit.
        let requests = Arc::downgrade(self);
        handlers.register(
            "model.info",
            host_handler(move |_payload| {
                let requests = requests.clone();
                let model_info = model_info.clone();
                Box::pin(async move {
                    // The bound runtime is authoritative once live; the
                    // registration-time facts cover pre-bind probes and a
                    // dropped session (model.info always answers).
                    let model_info = requests
                        .upgrade()
                        .and_then(|requests| {
                            requests.bound().map(|runtime| runtime.model_info.clone())
                        })
                        .unwrap_or(model_info);
                    Ok(json!({
                        "id": model_info.id,
                        "provider": model_info.provider,
                        "input": model_info.input.iter().map(|input| match input {
                            pa_types::ai::ModelInput::Text => "text",
                            pa_types::ai::ModelInput::Image => "image",
                        }).collect::<Vec<&str>>(),
                    }))
                })
            }),
        );
    }

    /// Register the model-facing history read and context-edit handlers.
    pub fn register_history_handlers(self: &Arc<Self>, handlers: &mut HostRequestHandlers) {
        super::history_host::register_history_handlers(self, handlers);
    }

    /// Register `compact.status`/`compact.run`, gated by the compaction `agentCallable`
    /// setting; `create_session` passes the resolved keep-recent budget.
    pub fn register_compact_handlers(
        self: &Arc<Self>,
        handlers: &mut HostRequestHandlers,
        keep_recent_tokens: u64,
    ) {
        let requests = Arc::downgrade(self);
        handlers.register(
            "compact.status",
            host_handler(move |_payload| {
                let requests = requests.clone();
                Box::pin(async move {
                    let Some(requests) = requests.upgrade() else {
                        return Err(anyhow::anyhow!(
                            "the session ended before the request could be served"
                        ));
                    };
                    let usage = match requests.bound() {
                        Some(runtime) => {
                            let tokens = if requests.live_view().is_some() {
                                requests.assemble_live_view().await?.tokens
                            } else {
                                let entries =
                                    runtime.session.lock().await.retained_entries().to_vec();
                                let context = crate::session::build_session_context(&entries, None);
                                if context.archive.as_ref().is_some_and(|archive| {
                                    matches!(
                                        archive.representation,
                                        pa_types::session::ContextRepresentation::Responses { .. }
                                    )
                                }) {
                                    anyhow::bail!(
                                        "native context archive needs live-view assembly"
                                    );
                                }
                                crate::session_engine::compaction::context_tokens_with_drift(
                                    &context.messages,
                                    context.usage_drift,
                                )
                            };
                            context_usage_for_tokens(tokens, runtime.context_window)
                        }
                        None => None,
                    };
                    let scheduled = requests.compaction_scheduled().await;
                    let (tokens, window, percent) = match usage {
                        Some(usage) => (
                            usage.tokens.map_or(Value::Null, Value::from),
                            Value::from(usage.context_window),
                            usage.percent.map_or(Value::Null, Value::from),
                        ),
                        None => (Value::Null, Value::Null, Value::Null),
                    };
                    Ok(json!({
                        "tokens": tokens,
                        "context_window": window,
                        "percent": percent,
                        "scheduled": scheduled,
                    }))
                })
            }),
        );
        let requests = Arc::downgrade(self);
        handlers.register(
            "compact.run",
            host_handler(move |payload| {
                let requests = requests.clone();
                Box::pin(async move {
                    let Some(requests) = requests.upgrade() else {
                        return Err(anyhow::anyhow!("the session ended before the request could be served"));
                    };
                    let instructions = string_field(
                        &payload.data,
                        "instructions",
                        "compact.run instructions must be a string when provided",
                    )?;
                    let Some(runtime) = requests.bound() else {
                        return Ok(no_active_turn(
                            "no active turn; compaction can only be requested while a turn is running",
                        ));
                    };
                    let state = runtime.agent.state().await;
                    if !state.is_streaming {
                        return Ok(no_active_turn(
                            "no active turn; compaction can only be requested while a turn is running",
                        ));
                    }
                    // TS `prepareCompaction`: only schedule a compaction
                    // that has history to summarize.
                    let entries = runtime.session.lock().await.retained_entries().to_vec();
                    if let Some(reason) =
                        compaction_request_skip_reason(&entries, keep_recent_tokens)
                    {
                        return Ok(json!({ "scheduled": false, "reason": reason }));
                    }
                    requests.schedule_compaction(instructions).await;
                    Ok(json!({
                        "scheduled": true,
                        "note": "Compaction runs when the current turn ends; you resume automatically afterwards. Continue working normally.",
                    }))
                })
            }),
        );
    }
}

/// One turn-boundary consumption: the outcomes the host runtime persists and broadcasts;
/// `Err` rows surface as failed-compaction events.
#[derive(Debug)]
pub struct TurnBoundaryConsumption {
    pub compaction: Option<anyhow::Result<super::compact_session::CompactOutcome>>,
}

impl SessionEngine {
    /// Consume a pending model-requested compaction at a turn boundary; taken regardless
    /// of outcome, so a failed run is not silently re-run. `abort` cancels the run,
    /// surfacing the abort marker error the consumer maps to its cancelled outcome.
    pub async fn consume_pending_compaction(
        &self,
        model: &pa_types::ai::Model,
        api_key: Option<String>,
        abort: Option<&pa_agent::abort::AbortSignal>,
    ) -> Option<anyhow::Result<super::compact_session::CompactOutcome>> {
        let pending = self.turn_boundary.take_compaction().await?;
        let compact = async {
            self.session
                .compact(pending.instructions.as_deref(), model, api_key, abort)
                .await
        };
        Some(match abort {
            Some(signal) => match pa_agent::abort::race_with_abort(compact, signal).await {
                Ok(inner) => inner,
                Err(error) => Err(error),
            },
            _ => compact.await,
        })
    }

    /// Consume pending turn-boundary requests after a settled turn.
    pub async fn consume_turn_boundary_requests(
        &self,
        model: &pa_types::ai::Model,
        api_key: Option<String>,
        abort: Option<&pa_agent::abort::AbortSignal>,
    ) -> TurnBoundaryConsumption {
        TurnBoundaryConsumption {
            compaction: self.consume_pending_compaction(model, api_key, abort).await,
        }
    }
}

/// An optional string field with the exact TS validation message on a
/// non-string value; `None` for absent/null.
fn string_field(data: &Value, key: &str, error: &'static str) -> anyhow::Result<Option<String>> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => anyhow::bail!("{error}"),
    }
}

/// The `{ scheduled: false, reason }` give-up shape.
fn no_active_turn(reason: &'static str) -> Value {
    json!({ "scheduled": false, "reason": reason })
}

/// The `compact.run` reason for a session that cannot prepare a
/// compaction, distinct from the `/compact` skip message.
fn compaction_request_skip_reason(
    entries: &[FileEntry],
    keep_recent_tokens: u64,
) -> Option<&'static str> {
    prepare_compaction(entries, keep_recent_tokens)
        .err()
        .map(CompactSkip::request_reason)
}

/// Estimated context usage of one assembled view: [`context_tokens_with_drift`],
/// the figure the request builder and `history.index` use. `None` when the
/// context window is unknown.
#[must_use]
pub fn context_usage(
    messages: &[AgentMessage],
    drift: crate::session::UsageDrift,
    context_window: Option<u64>,
) -> Option<ContextUsage> {
    let tokens = crate::session_engine::compaction::context_tokens_with_drift(messages, drift);
    context_usage_for_tokens(tokens, context_window)
}

fn context_usage_for_tokens(tokens: u64, context_window: Option<u64>) -> Option<ContextUsage> {
    let context_window = context_window.filter(|window| *window > 0)?;
    let percent = tokens as f64 / context_window as f64 * 100.0;
    Some(ContextUsage {
        tokens: Some(tokens),
        context_window,
        percent: Some(percent),
    })
}

#[cfg(test)]
mod tests;
