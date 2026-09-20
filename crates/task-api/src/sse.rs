//! SSE `GET /stream`（`docs/gui/api.md` §4）。
//!
//! 接続ごとに購読ループを 1 つ動かす（購読者が 0 ならポーリングも無い）。ループは `events_since(cursor, 1000)` を
//! `poll_interval` ごとに呼んで `task.event` を送り、`watch` の変化で `daemon`、`heartbeat_interval` ごとに `heartbeat` を送る。
//! クライアントが切断すると（応答本体が捨てられて送信路が閉じ）、ループはすぐに終わって接続枠を返す。
//! celeris の停止時は `ApiState::close_streams` で全ループが終わり、接続が閉じる。

use std::convert::Infallible;
use std::sync::atomic::Ordering;

use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, header};
use axum::response::Response;
use serde::Serialize;
use task_core::{TaskId, TaskStore};
use task_ops::daemon::DaemonSnapshot;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};

use crate::STREAM_BATCH;
use crate::handlers::now_rfc3339;
use crate::problem::{ApiProblem, store_problem};
use crate::query::QueryParams;
use crate::state::{ApiState, StreamSlot};
use crate::types::{StreamHeartbeat, StreamHello, StreamReset};

const LAST_EVENT_ID: HeaderName = HeaderName::from_static("last-event-id");
pub(crate) const X_ACCEL_BUFFERING: HeaderName = HeaderName::from_static("x-accel-buffering");
pub(crate) const CHANNEL_CAPACITY: usize = 64;

pub(crate) async fn stream(
    State(state): State<ApiState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiProblem> {
    let query = QueryParams::parse(raw.as_deref(), &["after_id", "task_id"])?;
    let query_after = query.u64("after_id")?;
    let header_after = match headers.get(LAST_EVENT_ID) {
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .ok_or_else(|| ApiProblem::bad_request("Last-Event-ID must be an event id"))?,
        ),
        None => None,
    };
    let requested = header_after.or(query_after);
    let task_filter = query.task_id("task_id")?;

    let slot = state.try_open_stream().ok_or_else(ApiProblem::too_many_streams)?;
    let latest = state
        .blocking(|store| store.latest_event_id().map_err(store_problem))
        .await?;
    let (cursor, reset) = start_position(requested, latest, state.tuning.reset_threshold);

    let mut daemon = state.inner.daemon.clone();
    let snapshot = daemon.borrow_and_update().clone();
    let hello = StreamHello {
        cursor,
        now: now_rfc3339(),
        daemon: snapshot,
    };
    let (tx, rx) = mpsc::channel::<Bytes>(CHANNEL_CAPACITY);
    let first_frames = [frame("hello", None, &hello), reset.as_ref().and_then(|r| frame("reset", None, r))];
    for bytes in first_frames.into_iter().flatten() {
        if tx.try_send(bytes).is_err() {
            return Err(ApiProblem::internal("stream buffer is unavailable"));
        }
    }
    tokio::spawn(run(state, slot, tx, cursor, task_filter, daemon));

    let body = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|bytes| (Ok::<Bytes, Infallible>(bytes), rx))
    });
    let mut response = Response::new(Body::from_stream(body));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(X_ACCEL_BUFFERING, HeaderValue::from_static("no"));
    Ok(response)
}

/// 送信開始位置と、必要なら `reset`。省略時は「今」（最新 id）から。要求 id が最新より大きい（DB が入れ替わった）か、
/// 最新までが `threshold` 件を超えるなら、最新 id から続ける。
pub(crate) fn start_position(requested: Option<u64>, latest: u64, threshold: u64) -> (u64, Option<StreamReset>) {
    match requested {
        None => (latest, None),
        Some(after) if after > latest => (
            latest,
            Some(StreamReset {
                reason: "cursor_ahead".to_string(),
                cursor: latest,
            }),
        ),
        Some(after) if latest - after > threshold => (
            latest,
            Some(StreamReset {
                reason: "cursor_too_old".to_string(),
                cursor: latest,
            }),
        ),
        Some(after) => (after, None),
    }
}

async fn run(
    state: ApiState,
    _slot: StreamSlot,
    tx: mpsc::Sender<Bytes>,
    mut cursor: u64,
    task_filter: Option<TaskId>,
    mut daemon: watch::Receiver<Option<DaemonSnapshot>>,
) {
    let tuning = state.tuning;
    let mut shutdown = state.inner.shutdown.subscribe();
    let mut poll = tokio::time::interval(tuning.poll_interval);
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut heartbeat =
        tokio::time::interval_at(Instant::now() + tuning.heartbeat_interval, tuning.heartbeat_interval);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut daemon_open = true;

    loop {
        let keep_going = tokio::select! {
            biased;
            _ = tx.closed() => false,
            _ = closed(&mut shutdown) => false,
            changed = daemon.changed(), if daemon_open => {
                if changed.is_err() {
                    daemon_open = false;
                    true
                } else {
                    let snapshot = daemon.borrow_and_update().clone();
                    match snapshot {
                        Some(snapshot) => send(&state, &tx, frame("daemon", None, &snapshot)).await,
                        None => true,
                    }
                }
            }
            _ = heartbeat.tick() => {
                let beat = StreamHeartbeat { now: now_rfc3339() };
                send(&state, &tx, frame("heartbeat", None, &beat)).await
            }
            _ = poll.tick() => poll_events(&state, &tx, &mut cursor, task_filter).await,
        };
        if !keep_going {
            break;
        }
    }
}

/// 追いつくまで `events_since` を読む。送信路が閉じたら `false`。DB エラーは次のポーリングで再試行する。
async fn poll_events(
    state: &ApiState,
    tx: &mpsc::Sender<Bytes>,
    cursor: &mut u64,
    task_filter: Option<TaskId>,
) -> bool {
    loop {
        state.inner.stream_polls.fetch_add(1, Ordering::SeqCst);
        let after = *cursor;
        let rows = match state
            .blocking(move |store| store.events_since(after, STREAM_BATCH).map_err(store_problem))
            .await
        {
            Ok(rows) => rows,
            Err(problem) => {
                tracing::warn!(code = problem.code(), "SSE poll failed; retrying on the next tick");
                return true;
            }
        };
        let caught_up = rows.len() < STREAM_BATCH;
        for row in rows {
            let id = row.id;
            if task_filter.is_none_or(|t| t == row.task_id)
                && !send(state, tx, frame("task.event", Some(id), &row)).await
            {
                return false;
            }
            *cursor = id;
        }
        if caught_up {
            return true;
        }
    }
}

/// 1 フレームを送る。送信路が閉じているか停止中なら `false`。
pub(crate) async fn send(state: &ApiState, tx: &mpsc::Sender<Bytes>, frame: Option<Bytes>) -> bool {
    let Some(frame) = frame else {
        return true;
    };
    let mut shutdown = state.inner.shutdown.subscribe();
    tokio::select! {
        sent = tx.send(frame) => sent.is_ok(),
        _ = closed(&mut shutdown) => false,
    }
}

/// 停止が要求される（または送信側が消える）まで待つ。`watch::Ref` を待機点の外に持ち出さない。
pub(crate) async fn closed(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|closed| *closed).await.map(|_| ());
}

/// `event: <name>\n[id: <id>\n]data: <json>\n\n`（JSON は 1 行）。
pub(crate) fn frame<T: Serialize>(event: &str, id: Option<u64>, data: &T) -> Option<Bytes> {
    let json = match serde_json::to_string(data) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!(error = %e, event, "failed to serialize an SSE frame");
            return None;
        }
    };
    let mut out = String::with_capacity(json.len() + 48);
    out.push_str("event: ");
    out.push_str(event);
    out.push('\n');
    if let Some(id) = id {
        out.push_str("id: ");
        out.push_str(&id.to_string());
        out.push('\n');
    }
    out.push_str("data: ");
    out.push_str(&json);
    out.push_str("\n\n");
    Some(Bytes::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_position_resumes_resets_or_starts_now() {
        assert_eq!(start_position(None, 50, 10_000), (50, None));
        assert_eq!(start_position(Some(40), 50, 10_000), (40, None));
        assert_eq!(start_position(Some(0), 10_000, 10_000), (0, None));
        let (cursor, reset) = start_position(Some(0), 10_001, 10_000);
        assert_eq!(cursor, 10_001);
        assert_eq!(reset.map(|r| r.reason), Some("cursor_too_old".to_string()));
        let (cursor, reset) = start_position(Some(60), 50, 10_000);
        assert_eq!(cursor, 50);
        assert_eq!(reset.map(|r| (r.reason, r.cursor)), Some(("cursor_ahead".to_string(), 50)));
    }

    #[test]
    fn frames_carry_event_name_optional_id_and_single_line_data() {
        let bytes = frame("task.event", Some(7), &serde_json::json!({"a": "x\ny"})).unwrap_or_default();
        assert_eq!(&bytes[..], b"event: task.event\nid: 7\ndata: {\"a\":\"x\\ny\"}\n\n");
        let bytes = frame("heartbeat", None, &serde_json::json!({"now": "t"})).unwrap_or_default();
        assert_eq!(&bytes[..], b"event: heartbeat\ndata: {\"now\":\"t\"}\n\n");
    }
}
