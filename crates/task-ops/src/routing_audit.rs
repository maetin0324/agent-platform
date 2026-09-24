//! ADR-0068 D5（Phase 114）: タスク 1 件の routing の監査を、ストアのイベントから組み立てる
//! （集計そのものは純粋関数 `task_core::routing_audit::routing_audit`）。GUI / API の表示は別 Phase。

use task_core::{RoutingAudit, TaskId, TaskStore};

use crate::error::OpsError;

/// そのタスクのワーカー run ごとの routing の監査（古い run が先）。知らないタスクは `NotFound`。
pub fn task_routing_audit(
    store: &dyn TaskStore,
    task_id: TaskId,
) -> Result<Vec<RoutingAudit>, OpsError> {
    let task = store.get(task_id)?.ok_or(OpsError::NotFound(task_id))?;
    let events: Vec<task_core::Event> = store
        .events_for(task_id)?
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    Ok(task_core::routing_audit(&task, &events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::add::{NewTaskSpec, create_task};
    use task_core::{Event, SqliteStore};

    #[test]
    fn reads_the_audit_from_the_store_and_rejects_unknown_tasks() {
        let store = SqliteStore::open_in_memory().unwrap();
        let spec: NewTaskSpec = serde_json::from_value(serde_json::json!({
            "title": "t", "objective": "o",
            "acceptance": [{"type": "command", "cmd": "true", "expect_exit": 0}]
        }))
        .unwrap();
        let task = create_task(&store, spec, time::OffsetDateTime::now_utc()).unwrap();
        store
            .append_event(
                task.id,
                &Event::WorkerFinished {
                    run_id: "r1".into(),
                    outcome: "done: ok".into(),
                    usage: None,
                    role: None,
                    metrics: Some(task_core::RunMetrics {
                        wall_ms: 5,
                        retries: 0,
                    }),
                },
            )
            .unwrap();
        let audit = task_routing_audit(&store, task.id).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].wall_ms, Some(5));
        assert!(matches!(
            task_routing_audit(&store, TaskId::new()),
            Err(OpsError::NotFound(_))
        ));
    }
}
