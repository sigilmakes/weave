use super::*;

#[pa_test_scratch::pa_test]
#[test]
fn stats_and_attach_follow_history_index_before_and_after_an_edit() {
    let _faux = FAUX_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (engine, dir) = faux_engine_with_settings(
        &json!({"responses": ["done"], "contextWindow": 128_000}),
        100,
    );
    let engine = Arc::new(engine);
    let path = dir.path().join("context-usage.jsonl");
    let mut store =
        crate::session_store::SessionFile::create(dir.path().to_str().unwrap(), None, 0);
    store.set_path(path.clone());
    store.append_message(&json!({
        "role": "user", "content": "long prior request ".repeat(400), "timestamp": 1,
    }));
    store.append_message(&json!({
        "role": "assistant", "content": [{"type": "text", "text": "done"}],
        "api": "openai-completions", "provider": "test", "model": "test",
        "stopReason": "stop", "timestamp": 2,
        "usage": {"input": 9000, "output": 10, "cacheRead": 0, "cacheWrite": 0,
            "totalTokens": 9010, "cost": {"input": 0, "output": 0,
            "cacheRead": 0, "cacheWrite": 0, "total": 0}},
    }));
    store.rewrite().unwrap();
    let entries = store.branch_file_entries();
    engine.set_session_file(path);
    let owner = own_log_store(&engine, store);
    engine
        .rebuild_session_context(
            entries,
            pa_core::session_engine::goal_driver::GoalBranchReload::FaithfulBranch,
        )
        .unwrap();
    let model = engine.resolve_model().unwrap();
    engine.runtime.block_on(async {
        let mut worker = crate::worker::Worker::new(
            crate::worker::WorkerConfig {
                socket_path: dir.path().join("worker.sock"),
                supervisor_socket_path: std::path::PathBuf::new(),
                token: "usage-test".into(),
                worker_instance_id: String::new(),
                active_session_id: "usage-test".into(),
                agent_dir: dir.path().join("agent"),
                recovery_journal_path: dir.path().join("recovery.jsonl"),
                script: Some(json!({"responses": []})),
            },
            None,
        );
        worker.core = owner;
        worker.engine = engine.clone();
        let unavailable = worker.dispatch("get_session_stats", &json!({})).await;
        let usage = &unavailable.data.unwrap()["contextUsage"];
        assert!(usage["unavailable"].is_string());
        assert_eq!(usage["tokens"], Value::Null);
        assert!(engine.session.lock().await.is_none());

        engine.ensure_core_session_async(&model).await.unwrap();
        let built = engine.session.lock().await.clone().unwrap();
        let mut handlers = pa_core::kernel::shared::HostRequestHandlers::default();
        built.turn_boundary.register_history_handlers(&mut handlers);
        let mut figures = Vec::new();
        for edited in [false, true] {
            if edited {
                handlers.get("history.edit").unwrap().clone()(
                    pa_core::kernel::shared::HostRequestPayload {
                        data: json!({"changes": [{"op": "drop", "at": 0}]}),
                    },
                )
                .await
                .unwrap();
            }
            let index = handlers.get("history.index").unwrap().clone()(
                pa_core::kernel::shared::HostRequestPayload { data: json!({}) },
            )
            .await
            .unwrap();
            let stats = worker.dispatch("get_session_stats", &json!({})).await;
            assert!(stats.success, "{stats:?}");
            let usage = stats.data.unwrap()["contextUsage"].clone();
            assert_eq!(usage["tokens"], index["live_tokens"]);
            assert!(usage.get("unavailable").is_none());
            let attach = worker.dispatch("get_connection_state", &json!({})).await;
            assert_eq!(attach.data.unwrap()["contextUsage"], usage);
            let tree = worker.dispatch("get_context_tree", &json!({})).await;
            assert_eq!(tree.data.unwrap()["contextUsage"], usage);
            figures.push(index["live_tokens"].as_u64().unwrap());
        }
        assert!(
            figures[1] < figures[0],
            "edit must change the measured view"
        );
    });
}
