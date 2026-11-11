//! The engine unit battery: the scripted tool loop, the child-depth
//! stamp, the MCP-gating unlock, and the goal/heartbeat handler
//! registration.
use super::*;
use crate::session_engine::tool_bridge::{bridge_tool, ToolDefinitionBridge};
use crate::tools::tool_definition::{ExecutionMode, ToolDefinition, ToolExecutionResult};
use pa_agent::scripted::ScriptedProvider;

fn echo_definition() -> ToolDefinition {
    ToolDefinition {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echoes its input".to_string(),
        prompt_snippet: String::new(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        }),
        execution_mode: Some(ExecutionMode::Sequential),
        prepare_arguments: None,
        execute: Arc::new(|_id, params, _signal, _on_update| {
            let text = params
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            Box::pin(async move { Ok(ToolExecutionResult::text(format!("echo: {text}"))) })
        }),
    }
}

#[tokio::test]
#[pa_test_scratch::pa_test]
async fn engine_runs_tool_loop_and_persists() {
    let model = pa_agent::types::Model {
        id: "m".into(),
        name: "m".into(),
        api: "test".into(),
        provider: "test".into(),
        base_url: "http://localhost".into(),
        reasoning: false,
        cost: pa_agent::types::UsageCost::default(),
        context_window: 1_000,
        max_tokens: 100,
        input: Vec::new(),
        max_tokens_explicit: false,
    };
    let provider = Arc::new(ScriptedProvider::new(model.clone()));
    provider.push_tool_call_turn(
        Some("checking"),
        vec![("call-1", "echo", serde_json::json!({ "text": "hi" }))],
    );
    provider.push_text_turn("all done");

    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let engine = create_session(SessionEngineConfig {
        on_late_sent_agent_message: None,
        context_startup_boundary: crate::session_engine::ContextStartupBoundary::Current,
        context_startup_admitted_input_ids: Vec::new(),
        on_initial_context_selection: None,
        ollama_cloud: None,
        semantic_edges: None,
        cron_store: None,
        queued_steering_probe: None,
        should_stop_after_turn: None,
        image_model_router: None,
        steering_mode: None,
        follow_up_mode: None,
        cwd: cwd.clone(),
        agent_dir: tmp.path().join("agent"),
        mcp_manager: None,
        model: Some(model),
        thinking_level: None,
        stream_fn: Some(provider.stream_fn()),
        tools: vec![bridge_tool(echo_definition())],
        custom_system_prompt: None,
        prompt_guidelines: vec![],
        generic_mcp_servers: vec![],
        allow_recursion: None,
        session_manager: None,
        extra_host_handlers: None,
        conversation_log_path: None,
        additional_skill_paths: vec![],
        additional_prompt_paths: vec![],
        extra_builtin_skill_overrides: vec![],
        rlm_subagent_host: None,
        rlm_usage_store: None,
        progress_note_emit: None,
        rlm_depth: None,
        daemonless: true,
        model_info: None,
        on_background_work_settled: None,
        prewarm_ipython_kernel: None,
        queued_goal_context_purge: None,
    })
    .await
    .unwrap();

    // The system prompt is the layered assembly: static core layer
    // first, dynamic tail after.
    assert!(engine.system_prompt.starts_with("# weave harness"));
    assert!(engine
        .system_prompt
        .contains("Recursive agent depth: 0 (root)"));

    let outcome = engine
        .prompt("run the echo tool", PromptOptions::default())
        .await
        .unwrap();
    assert_eq!(outcome, PromptOutcome::Prompt);
    engine.session.agent().wait_for_idle().await;

    let state = engine.session.agent().state().await;
    assert!(state.messages.iter().any(|message| match message {
        pa_agent::types::AgentMessage::Standard(pa_agent::types::Message::ToolResult(result)) => {
            result.tool_name == "echo"
        }
        _ => false,
    }));
    assert!(state.messages.iter().any(|message| match message {
        pa_agent::types::AgentMessage::Standard(pa_agent::types::Message::Assistant(assistant)) => {
            assistant.content.iter().any(|block| {
                matches!(
                    block,
                    pa_agent::types::AssistantContent::Text(text) if text.text == "all done"
                )
            })
        }
        _ => false,
    }));
    let entries = engine.session.entries().await;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        pa_types::session::FileEntry::Message {
            message: pa_types::session::AgentMessage::User(user),
            ..
        } if user.content.text() == "run the echo tool"
    )));
    let _ = ToolDefinitionBridge::new;
}

/// A spawned child's prompt stamps its recursion depth: `create_session`
/// at depth N reads "depth: N (not root)", never the root identity.
#[tokio::test]
#[pa_test_scratch::pa_test]
async fn spawned_child_prompt_stamps_its_depth() {
    let model = pa_agent::types::Model {
        id: "m".into(),
        name: "m".into(),
        api: "test".into(),
        provider: "test".into(),
        base_url: "http://localhost".into(),
        reasoning: false,
        cost: pa_agent::types::UsageCost::default(),
        context_window: 1_000,
        max_tokens: 100,
        input: Vec::new(),
        max_tokens_explicit: false,
    };
    let provider = Arc::new(ScriptedProvider::new(model.clone()));
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let engine = create_session(SessionEngineConfig {
        on_late_sent_agent_message: None,
        context_startup_boundary: crate::session_engine::ContextStartupBoundary::Current,
        context_startup_admitted_input_ids: Vec::new(),
        on_initial_context_selection: None,
        ollama_cloud: None,
        semantic_edges: None,
        cron_store: None,
        queued_steering_probe: None,
        should_stop_after_turn: None,
        image_model_router: None,
        steering_mode: None,
        follow_up_mode: None,
        cwd: cwd.clone(),
        agent_dir: tmp.path().join("agent"),
        mcp_manager: None,
        model: Some(model),
        thinking_level: None,
        stream_fn: Some(provider.stream_fn()),
        tools: vec![bridge_tool(echo_definition())],
        custom_system_prompt: None,
        prompt_guidelines: vec![],
        generic_mcp_servers: vec![],
        allow_recursion: None,
        session_manager: None,
        extra_host_handlers: None,
        conversation_log_path: None,
        additional_skill_paths: vec![],
        additional_prompt_paths: vec![],
        extra_builtin_skill_overrides: vec![],
        rlm_subagent_host: None,
        rlm_usage_store: None,
        progress_note_emit: None,
        rlm_depth: Some(2),
        daemonless: true,
        model_info: None,
        on_background_work_settled: None,
        prewarm_ipython_kernel: None,
        queued_goal_context_purge: None,
    })
    .await
    .unwrap();

    assert!(engine
        .system_prompt
        .contains("Recursive agent depth: 2 (not root)"));
    assert!(!engine.system_prompt.contains("depth: 0 (root)"));
}

/// The login chain's prompt-gating end to end: a settings-declared OAuth
/// server stays gated, an endpoint-bound credential unlocks it in the NEXT
/// session, and one bound to another endpoint does not.
#[tokio::test]
#[pa_test_scratch::pa_test]
async fn oauth_creds_unlock_generic_mcp_gating_in_new_sessions() {
    fn model() -> pa_agent::types::Model {
        pa_agent::types::Model {
            id: "m".into(),
            name: "m".into(),
            api: "test".into(),
            provider: "test".into(),
            base_url: "http://localhost".into(),
            reasoning: false,
            cost: pa_agent::types::UsageCost::default(),
            context_window: 1_000,
            max_tokens: 100,
            input: Vec::new(),
            max_tokens_explicit: false,
        }
    }

    fn config(
        cwd: &std::path::Path,
        agent_dir: &std::path::Path,
        stream_fn: pa_agent::stream::StreamFn,
    ) -> SessionEngineConfig {
        SessionEngineConfig {
            on_late_sent_agent_message: None,
            context_startup_boundary: crate::session_engine::ContextStartupBoundary::Current,
            context_startup_admitted_input_ids: Vec::new(),
            on_initial_context_selection: None,
            ollama_cloud: None,
            semantic_edges: None,
            cron_store: None,
            queued_steering_probe: None,
            should_stop_after_turn: None,
            image_model_router: None,
            steering_mode: None,
            follow_up_mode: None,
            cwd: cwd.to_path_buf(),
            agent_dir: agent_dir.to_path_buf(),
            mcp_manager: None,
            model: Some(model()),
            thinking_level: None,
            stream_fn: Some(stream_fn),
            tools: vec![],
            custom_system_prompt: None,
            prompt_guidelines: vec![],
            generic_mcp_servers: vec![],
            allow_recursion: None,
            session_manager: None,
            extra_host_handlers: None,
            conversation_log_path: None,
            additional_skill_paths: vec![],
            additional_prompt_paths: vec![],
            extra_builtin_skill_overrides: vec![],
            rlm_subagent_host: None,
            rlm_usage_store: None,
            progress_note_emit: None,
            rlm_depth: None,
            daemonless: true,
            model_info: None,
            on_background_work_settled: None,
            prewarm_ipython_kernel: None,
            queued_goal_context_purge: None,
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    let agent_dir = tmp.path().join("agent");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&agent_dir).unwrap();
    // The settings declaration the daemon worker's MCP manager also
    // resolves (an OAuth HTTP server, like `mcp add ... --oauth`).
    std::fs::write(
        agent_dir.join("settings.json"),
        serde_json::json!({
            "mcpServers": {
                "fixture-oauth": {
                    "type": "http",
                    "url": "https://fixture.example/mcp",
                    "oauth": true,
                },
            },
        })
        .to_string(),
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(model()));

    let engine = create_session(config(&cwd, &agent_dir, provider.stream_fn()))
        .await
        .unwrap();
    assert!(!engine.system_prompt.contains("# Generic MCP Connections"));

    // The persisted credential begin_login leaves behind (the TS
    // McpCredentials shape, endpoint-bound).
    let write_credential = |endpoint: &str| {
        std::fs::write(
            agent_dir.join("auth.json"),
            serde_json::json!({
                "mcp:fixture-oauth": {
                    "type": "oauth",
                    "access": "fixture-access",
                    "refresh": "fixture-refresh",
                    "expires": 999_999_999_999_999_i64,
                    "endpoint": endpoint,
                    "tokenEndpoint": "https://fixture.example/token",
                    "clientId": "fixture-client",
                },
            })
            .to_string(),
        )
        .unwrap();
    };

    // A credential bound to another endpoint stays gated: the token
    // must prove where it belongs.
    write_credential("https://other.example/mcp");
    let engine = create_session(config(&cwd, &agent_dir, provider.stream_fn()))
        .await
        .unwrap();
    assert!(!engine.system_prompt.contains("# Generic MCP Connections"));

    write_credential("https://fixture.example/mcp");
    let engine = create_session(config(&cwd, &agent_dir, provider.stream_fn()))
        .await
        .unwrap();
    assert!(engine.system_prompt.contains("# Generic MCP Connections"));
    assert!(engine.system_prompt.contains("`fixture-oauth`"));
}

#[tokio::test]
#[pa_test_scratch::pa_test]
async fn create_session_registers_goal_and_heartbeat_handlers() {
    let dir = tempfile::TempDir::new().unwrap();
    let registration =
        pa_ai::faux::register_faux_provider(pa_ai::faux::RegisterFauxProviderOptions {
            models: Some(vec![pa_ai::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                name: Some("Faux".to_string()),
                reasoning: Some(false),
                input: Some(vec![pa_types::ai::ModelInput::Text]),
                cost: None,
                context_window: Some(100_000),
                max_tokens: Some(4_096),
            }]),
            ..Default::default()
        });
    registration.set_responses(vec![pa_ai::faux::FauxResponseStep::Message(
        pa_ai::faux::faux_assistant_text_message(
            "ok",
            pa_ai::faux::FauxAssistantMessageOptions::default(),
        ),
    )]);
    let model = registration.get_model();
    let agent_model = crate::session_engine::provider_adapter::json_round_trip(&model).unwrap();
    let stream_fn = crate::session_engine::provider_adapter::real_stream_fn(None, model.clone());
    let engine = create_session(SessionEngineConfig {
        cron_store: None,
        cwd: dir.path().to_path_buf(),
        agent_dir: dir.path().to_path_buf(),
        model: Some(agent_model),
        stream_fn: Some(stream_fn),
        tools: Vec::new(),
        ..Default::default()
    })
    .await
    .unwrap();
    let names: Vec<String> = engine
        .session
        .agent()
        .state()
        .await
        .tools
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert!(
        names.iter().any(|name| name == "python_repl"),
        "tools: {names:?}"
    );
}

/// A daemon-shaped resume: an in-memory session manager (the daemon's
/// non-persisted engine session) already holding a message, plus a
/// `conversation_log_path`, still auto-appends the missing
/// thinking-level and service-tier rows (#3350's gate regression).
#[tokio::test]
#[pa_test_scratch::pa_test]
async fn create_session_appends_missing_resume_settings_rows() {
    let model = pa_agent::types::Model {
        id: "m".into(),
        name: "m".into(),
        api: "test".into(),
        provider: "test".into(),
        base_url: "http://localhost".into(),
        reasoning: false,
        cost: pa_agent::types::UsageCost::default(),
        context_window: 1_000,
        max_tokens: 100,
        input: Vec::new(),
        max_tokens_explicit: false,
    };
    let provider = Arc::new(ScriptedProvider::new(model.clone()));
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut session_manager =
        crate::session::manager::SessionManager::in_memory_in_session_dir(&cwd, tmp.path());
    session_manager
        .append_message(pa_types::session::AgentMessage::User(
            pa_types::ai::UserMessage {
                content: pa_types::ai::UserContent::Text("resumed".into()),
                timestamp: 0,
                rest: serde_json::Map::default(),
            },
        ))
        .unwrap();
    let engine = create_session(SessionEngineConfig {
        on_late_sent_agent_message: None,
        context_startup_boundary: crate::session_engine::ContextStartupBoundary::Current,
        context_startup_admitted_input_ids: Vec::new(),
        on_initial_context_selection: None,
        ollama_cloud: None,
        semantic_edges: None,
        cron_store: None,
        queued_steering_probe: None,
        should_stop_after_turn: None,
        image_model_router: None,
        steering_mode: None,
        follow_up_mode: None,
        cwd: cwd.clone(),
        agent_dir: tmp.path().join("agent"),
        mcp_manager: None,
        model: Some(model),
        thinking_level: None,
        stream_fn: Some(provider.stream_fn()),
        tools: vec![],
        custom_system_prompt: None,
        prompt_guidelines: vec![],
        generic_mcp_servers: vec![],
        allow_recursion: None,
        session_manager: Some(session_manager),
        extra_host_handlers: None,
        conversation_log_path: Some(tmp.path().join("session.jsonl")),
        additional_skill_paths: vec![],
        additional_prompt_paths: vec![],
        extra_builtin_skill_overrides: vec![],
        rlm_subagent_host: None,
        rlm_usage_store: None,
        progress_note_emit: None,
        rlm_depth: None,
        daemonless: true,
        model_info: None,
        on_background_work_settled: None,
        prewarm_ipython_kernel: None,
        queued_goal_context_purge: None,
    })
    .await
    .unwrap();

    let entries = engine.session.entries().await;
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            pa_types::session::FileEntry::ThinkingLevelChange { .. }
        )),
        "entries: {entries:?}"
    );
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            pa_types::session::FileEntry::ServiceTierChange { .. }
        )),
        "entries: {entries:?}"
    );
}

/// `history.index` reports the figure the engine actually sends: the same
/// assembled view, measured with the drift the engine publishes.
#[tokio::test]
#[pa_test_scratch::pa_test]
async fn history_index_matches_the_request_the_engine_sends() {
    let model = pa_agent::types::Model {
        id: "m".into(),
        name: "m".into(),
        api: "test".into(),
        provider: "test".into(),
        base_url: "http://localhost".into(),
        reasoning: false,
        cost: pa_agent::types::UsageCost::default(),
        context_window: 100_000,
        max_tokens: 100,
        input: Vec::new(),
        max_tokens_explicit: false,
    };
    let provider = Arc::new(ScriptedProvider::new(model.clone()));
    provider.push_text_turn("done");
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let engine = create_session(SessionEngineConfig {
        on_late_sent_agent_message: None,
        context_startup_boundary: crate::session_engine::ContextStartupBoundary::Current,
        context_startup_admitted_input_ids: Vec::new(),
        on_initial_context_selection: None,
        ollama_cloud: None,
        semantic_edges: None,
        cron_store: None,
        queued_steering_probe: None,
        should_stop_after_turn: None,
        image_model_router: None,
        steering_mode: None,
        follow_up_mode: None,
        cwd: cwd.clone(),
        agent_dir: tmp.path().join("agent"),
        mcp_manager: None,
        model: Some(model.clone()),
        thinking_level: None,
        stream_fn: Some(provider.stream_fn()),
        tools: vec![],
        custom_system_prompt: None,
        prompt_guidelines: vec![],
        generic_mcp_servers: vec![],
        allow_recursion: None,
        session_manager: None,
        extra_host_handlers: None,
        conversation_log_path: None,
        additional_skill_paths: vec![],
        additional_prompt_paths: vec![],
        extra_builtin_skill_overrides: vec![],
        rlm_subagent_host: None,
        rlm_usage_store: None,
        progress_note_emit: None,
        rlm_depth: None,
        daemonless: true,
        model_info: None,
        on_background_work_settled: None,
        prewarm_ipython_kernel: None,
        queued_goal_context_purge: None,
    })
    .await
    .unwrap();
    engine
        .prompt("first turn", PromptOptions::default())
        .await
        .unwrap();
    engine.session.agent().wait_for_idle().await;

    let mut handlers = crate::kernel::shared::HostRequestHandlers::default();
    crate::session_engine::history_host::register_history_handlers(
        &engine.turn_boundary,
        &mut handlers,
    );
    let index =
        handlers.get("history.index").unwrap().clone()(crate::kernel::shared::HostRequestPayload {
            data: serde_json::json!({"limit": 5}),
        })
        .await
        .unwrap();

    let (sent, _) = engine.session.assembled_context(None).await.unwrap();
    let drift = engine.session.context_usage_drift().await;
    let expected = crate::session_engine::compaction::context_tokens_with_drift(&sent, drift);
    assert_eq!(index["live_tokens"], expected);
    assert_eq!(index["window"], model.context_window);
}

/// The engine's live view is the send path, not a build-time snapshot: an edit
/// appended to the session changes both, and the compact figure follows.
#[tokio::test]
#[pa_test_scratch::pa_test]
async fn the_live_view_tracks_the_session_and_the_send_path() {
    let model = pa_agent::types::Model {
        id: "m".into(),
        name: "m".into(),
        api: "test".into(),
        provider: "test".into(),
        base_url: "http://localhost".into(),
        reasoning: false,
        cost: pa_agent::types::UsageCost::default(),
        context_window: 100_000,
        max_tokens: 100,
        input: Vec::new(),
        max_tokens_explicit: false,
    };
    let provider = Arc::new(ScriptedProvider::new(model.clone()));
    provider.push_text_turn("done");
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let engine = create_session(SessionEngineConfig {
        on_late_sent_agent_message: None,
        context_startup_boundary: crate::session_engine::ContextStartupBoundary::Current,
        context_startup_admitted_input_ids: Vec::new(),
        on_initial_context_selection: None,
        ollama_cloud: None,
        semantic_edges: None,
        cron_store: None,
        queued_steering_probe: None,
        should_stop_after_turn: None,
        image_model_router: None,
        steering_mode: None,
        follow_up_mode: None,
        cwd: cwd.clone(),
        agent_dir: tmp.path().join("agent"),
        mcp_manager: None,
        model: Some(model.clone()),
        thinking_level: None,
        stream_fn: Some(provider.stream_fn()),
        tools: vec![],
        custom_system_prompt: None,
        prompt_guidelines: vec![],
        generic_mcp_servers: vec![],
        allow_recursion: None,
        session_manager: None,
        extra_host_handlers: None,
        conversation_log_path: None,
        additional_skill_paths: vec![],
        additional_prompt_paths: vec![],
        extra_builtin_skill_overrides: vec![],
        rlm_subagent_host: None,
        rlm_usage_store: None,
        progress_note_emit: None,
        rlm_depth: None,
        daemonless: true,
        model_info: None,
        on_background_work_settled: None,
        prewarm_ipython_kernel: None,
        queued_goal_context_purge: None,
    })
    .await
    .unwrap();
    engine
        .prompt("first turn", PromptOptions::default())
        .await
        .unwrap();
    engine.session.agent().wait_for_idle().await;

    let before = engine.turn_boundary.assemble_live_view().await.unwrap();
    let (sent, _) = engine.session.assembled_context(None).await.unwrap();
    assert_eq!(before.messages, sent, "one view, two readers");

    let mut handlers = crate::kernel::shared::HostRequestHandlers::default();
    crate::session_engine::history_host::register_history_handlers(
        &engine.turn_boundary,
        &mut handlers,
    );
    handlers.get("history.edit").unwrap().clone()(crate::kernel::shared::HostRequestPayload {
        data: serde_json::json!({"changes": [{"op": "drop", "at": 0}]}),
    })
    .await
    .unwrap();

    let after = engine.turn_boundary.assemble_live_view().await.unwrap();
    let (sent, _) = engine.session.assembled_context(None).await.unwrap();
    assert_eq!(after.messages, sent);
    assert!(
        after.messages.len() < before.messages.len(),
        "the drop landed"
    );
}

fn turn_with_usage(
    steps: Vec<pa_agent::scripted::ScriptStep>,
    input: u64,
) -> pa_agent::scripted::ScriptedTurn {
    let steps = steps
        .into_iter()
        .map(|step| match step {
            pa_agent::scripted::ScriptStep::Event(event) => {
                let event = match *event {
                    pa_agent::stream::AssistantMessageEvent::Done {
                        reason,
                        mut message,
                    } => {
                        message.usage = pa_agent::types::Usage {
                            input,
                            output: 1,
                            cache_read: 0,
                            cache_write: 0,
                            total_tokens: input + 1,
                            cost: pa_agent::types::UsageCost::default(),
                        };
                        pa_agent::stream::AssistantMessageEvent::Done { reason, message }
                    }
                    other => other,
                };
                pa_agent::scripted::ScriptStep::Event(Box::new(event))
            }
            other @ pa_agent::scripted::ScriptStep::SleepMs(_) => other,
        })
        .collect();
    pa_agent::scripted::ScriptedTurn::Events(steps)
}

struct EngineEditTool {
    requests: Arc<
        std::sync::Mutex<Option<Arc<crate::session_engine::turn_boundary::TurnBoundaryRequests>>>,
    >,
    changes: serde_json::Value,
}

static ENGINE_EDIT_SCHEMA: std::sync::LazyLock<serde_json::Value> =
    std::sync::LazyLock::new(|| serde_json::json!({"type": "object"}));

impl pa_agent::types::AgentTool for EngineEditTool {
    fn name(&self) -> &'static str {
        "history_edit"
    }

    fn description(&self) -> &'static str {
        "edit the history view"
    }

    fn parameters(&self) -> &serde_json::Value {
        &ENGINE_EDIT_SCHEMA
    }

    fn execute(
        self: Arc<Self>,
        _tool_call_id: String,
        _params: serde_json::Value,
        _signal: pa_agent::abort::AbortSignal,
        _on_update: pa_agent::types::AgentToolUpdateCallback,
    ) -> pa_agent::BoxFut<'static, anyhow::Result<pa_agent::types::AgentToolResult>> {
        let changes = self.changes.clone();
        let requests = self.requests.lock().unwrap().clone().expect("bound engine");
        Box::pin(async move {
            let mut handlers = crate::kernel::shared::HostRequestHandlers::default();
            crate::session_engine::history_host::register_history_handlers(
                &requests,
                &mut handlers,
            );
            let edit = handlers.get("history.edit").unwrap().clone();
            let response = edit(crate::kernel::shared::HostRequestPayload {
                data: serde_json::json!({"changes": changes}),
            })
            .await?;
            let status = response["status"].as_str().unwrap_or_default().to_string();
            Ok(pa_agent::types::AgentToolResult::text(status))
        })
    }
}

/// An edit inside a real run: request two must lack the dropped turn, and
/// `history.index` must report the usage the provider returned after it.
#[tokio::test]
#[pa_test_scratch::pa_test]
async fn an_edit_lands_in_the_second_request_and_index_reports_the_post_edit_usage() {
    let model = pa_agent::types::Model {
        id: "m".into(),
        name: "m".into(),
        api: "test".into(),
        provider: "test".into(),
        base_url: "http://localhost".into(),
        reasoning: false,
        cost: pa_agent::types::UsageCost::default(),
        context_window: 100_000,
        max_tokens: 100,
        input: Vec::new(),
        max_tokens_explicit: false,
    };
    let provider = Arc::new(ScriptedProvider::new(model.clone()));
    provider.push_turn(turn_with_usage(
        pa_agent::scripted::tool_call_turn_steps(
            &model,
            None,
            vec![("call-1", "history_edit", serde_json::json!({}))],
        ),
        90_000,
    ));
    provider.push_turn(turn_with_usage(
        pa_agent::scripted::text_turn_steps(&model, "second"),
        40_000,
    ));
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let slot = Arc::new(std::sync::Mutex::new(None));
    let engine = create_session(SessionEngineConfig {
        on_late_sent_agent_message: None,
        context_startup_boundary: crate::session_engine::ContextStartupBoundary::Current,
        context_startup_admitted_input_ids: Vec::new(),
        on_initial_context_selection: None,
        ollama_cloud: None,
        semantic_edges: None,
        cron_store: None,
        queued_steering_probe: None,
        should_stop_after_turn: None,
        image_model_router: None,
        steering_mode: None,
        follow_up_mode: None,
        cwd: cwd.clone(),
        agent_dir: tmp.path().join("agent"),
        mcp_manager: None,
        model: Some(model.clone()),
        thinking_level: None,
        stream_fn: Some(provider.stream_fn()),
        tools: vec![Arc::new(EngineEditTool {
            requests: Arc::clone(&slot),
            changes: serde_json::json!([{"op": "drop", "at": 0}]),
        })],
        custom_system_prompt: None,
        prompt_guidelines: vec![],
        generic_mcp_servers: vec![],
        allow_recursion: None,
        session_manager: None,
        extra_host_handlers: None,
        conversation_log_path: None,
        additional_skill_paths: vec![],
        additional_prompt_paths: vec![],
        extra_builtin_skill_overrides: vec![],
        rlm_subagent_host: None,
        rlm_usage_store: None,
        progress_note_emit: None,
        rlm_depth: None,
        daemonless: true,
        model_info: None,
        on_background_work_settled: None,
        prewarm_ipython_kernel: None,
        queued_goal_context_purge: None,
    })
    .await
    .unwrap();
    *slot.lock().unwrap() = Some(Arc::clone(&engine.turn_boundary));

    let dropped = "x".repeat(200_000);
    engine
        .prompt(&dropped, PromptOptions::default())
        .await
        .unwrap();
    engine.session.agent().wait_for_idle().await;

    let calls = provider.calls();
    assert_eq!(
        calls.len(),
        2,
        "the request before the edit and the one after it"
    );
    let first = user_texts_sent(&calls[0].messages);
    assert!(
        first.iter().any(|text| text.len() > 100_000),
        "the drop has a target"
    );
    let second = user_texts_sent(&calls[1].messages);
    assert!(
        second.iter().all(|text| text.len() < 100_000),
        "the request after the edit does not carry the dropped turn"
    );

    let mut handlers = crate::kernel::shared::HostRequestHandlers::default();
    crate::session_engine::history_host::register_history_handlers(
        &engine.turn_boundary,
        &mut handlers,
    );
    let index =
        handlers.get("history.index").unwrap().clone()(crate::kernel::shared::HostRequestPayload {
            data: serde_json::json!({"limit": 5}),
        })
        .await
        .unwrap();

    let state = engine.session.agent().state().await.messages;
    let session_messages: Vec<crate::session_engine::SessionAgentMessage> = state
        .iter()
        .filter_map(crate::session_engine::loop_message_to_session)
        .collect();
    let drift = engine.session.context_usage_drift().await;
    assert!(
        drift.is_clean(),
        "the assistant that answered the edit is the anchor and resets the correction"
    );
    assert_eq!(
        index["live_tokens"],
        crate::session_engine::compaction::estimate_context_tokens(&session_messages).tokens
    );
    assert_eq!(
        index["live_tokens"], 40_001,
        "the usage the provider reported after the edit, not the pre-edit 90_001"
    );

    let settings = crate::session_engine::compaction::CompactionSettings {
        threshold_percent: 95.0,
        enabled: true,
        reserve_tokens: 0,
        keep_recent_tokens: 1_000,
    };
    assert!(
        crate::session_engine::compaction::threshold_compaction_due_with_drift(
            &session_messages,
            drift,
            40_000,
            0,
            &settings
        ),
        "the decision follows the same figure index reports"
    );
    assert!(
        !crate::session_engine::compaction::threshold_compaction_due_with_drift(
            &session_messages,
            drift,
            100_000,
            0,
            &settings
        )
    );
    let sent_standard: Vec<pa_agent::types::Message> = state
        .iter()
        .filter_map(|message| match message {
            pa_agent::types::AgentMessage::Standard(inner) => Some(inner.clone()),
            pa_agent::types::AgentMessage::Custom(_) => None,
        })
        .take(calls[1].messages.len())
        .collect();
    assert_eq!(
        sent_standard, calls[1].messages,
        "the request that was sent is the transcript index measures"
    );
}

fn user_texts_sent(messages: &[pa_agent::types::Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            pa_agent::types::Message::User(user) => Some(match &user.content {
                pa_agent::types::UserContent::Text(text) => text.clone(),
                pa_agent::types::UserContent::Parts(parts) => format!("{parts:?}"),
            }),
            _ => None,
        })
        .collect()
}
