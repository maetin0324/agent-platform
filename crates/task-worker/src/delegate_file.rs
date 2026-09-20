//! LLM アダプタ（claude-code / codex）の委譲規約（ADR-0016 M8）。ストリームのワーカープロトコルを
//! 話さないこれらのアダプタは、run の終わりに成果物ディレクトリの `delegate.json` を読み、
//! `WorkerMessage::Delegate` と同じ意味を `EventSink::delegate` 経由でディスパッチャに伝える。
//! 置き場は `RunRequest.artifacts_dir`（ADR-0036 D2。共有 workspace では `.taskd/artifacts/<task_id>/`）。

use std::path::Path;

use serde::Deserialize;
use task_core::DelegateTask;

use crate::adapter::EventSink;

/// 委譲提案ファイルの名前（成果物ディレクトリの中）。`result.json` と同じ「run が書いた結果ファイル」の
/// 一つ（ADR-0016 M8, ADR-0036 D2）。
pub const DELEGATE_FILE_NAME: &str = "delegate.json";

/// `delegate.json` の形式。`WorkerMessage::Delegate` の `tasks` と同じ形。未知フィールドは
/// 拒否する（`DelegateTask` 自体が既に `deny_unknown_fields` だが、このラッパー自身の綴り間違いも
/// 検出するため同じ方針にする）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateFile {
    tasks: Vec<DelegateTask>,
}

/// 前回の run（リトライ）が残した提案を今回のものと誤読しないよう、run 開始時に消す
/// （`result.json` を消すのと同じ扱い。ADR-0006 D3 / ADR-0016 M8）。`artifacts_dir` は
/// `RunRequest.artifacts_dir`（ADR-0036）。
pub async fn clear_delegate_file(artifacts_dir: &Path) {
    let _ = tokio::fs::remove_file(artifacts_dir.join(DELEGATE_FILE_NAME)).await;
}

/// run の終わりに `<artifacts_dir>/delegate.json` があれば読んで `sink.delegate(&tasks)` を呼ぶ。ファイルが
/// 無ければ何もしない。JSON として読めなければ `sink.progress` に警告を残して無視する（run は失敗させない。
/// ADR-0016 M8）。戻り値は提案の件数。
pub async fn forward_delegate_file(artifacts_dir: &Path, sink: &dyn EventSink) -> usize {
    let path = artifacts_dir.join(DELEGATE_FILE_NAME);
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(_) => return 0,
    };
    match serde_json::from_str::<DelegateFile>(&text) {
        Ok(file) => {
            let count = file.tasks.len();
            sink.delegate(&file.tasks);
            count
        }
        Err(e) => {
            sink.progress(&format!("delegate.json ignored: {e}"));
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use task_core::ArtifactRef;

    #[derive(Default)]
    struct RecordingSink {
        progress: Mutex<Vec<String>>,
        delegated: Mutex<Vec<Vec<DelegateTask>>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(msg.to_string());
        }
        fn artifact(&self, _artifact: &ArtifactRef) {}
        fn delegate(&self, tasks: &[DelegateTask]) {
            self.delegated
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(tasks.to_vec());
        }
    }

    fn sample_task_json(title: &str) -> String {
        format!(
            r#"{{"title":"{title}","objective":"o","acceptance":[{{"text":"c","check":{{"type":"human"}}}}]}}"#
        )
    }

    #[tokio::test]
    async fn missing_file_forwards_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let sink = RecordingSink::default();
        let n = forward_delegate_file(dir.path(), &sink).await;
        assert_eq!(n, 0);
        assert!(sink.delegated.lock().unwrap().is_empty());
        assert!(sink.progress.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn valid_file_forwards_tasks_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(DELEGATE_FILE_NAME),
            format!(
                r#"{{"tasks":[{},{}]}}"#,
                sample_task_json("a"),
                sample_task_json("b")
            ),
        )
        .unwrap();
        let sink = RecordingSink::default();
        let n = forward_delegate_file(dir.path(), &sink).await;
        assert_eq!(n, 2);
        let delegated = sink.delegated.lock().unwrap();
        assert_eq!(delegated.len(), 1);
        assert_eq!(delegated[0].len(), 2);
    }

    #[tokio::test]
    async fn malformed_json_is_ignored_with_a_progress_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DELEGATE_FILE_NAME), "not json").unwrap();
        let sink = RecordingSink::default();
        let n = forward_delegate_file(dir.path(), &sink).await;
        assert_eq!(n, 0);
        assert!(sink.delegated.lock().unwrap().is_empty());
        let progress = sink.progress.lock().unwrap();
        assert_eq!(progress.len(), 1);
        assert!(progress[0].contains("delegate.json ignored"));
    }

    #[tokio::test]
    async fn clear_removes_stale_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DELEGATE_FILE_NAME), "{}").unwrap();
        clear_delegate_file(dir.path()).await;
        assert!(!dir.path().join(DELEGATE_FILE_NAME).exists());
        // Clearing a workspace that never had the file must not panic.
        clear_delegate_file(dir.path()).await;
    }
}
