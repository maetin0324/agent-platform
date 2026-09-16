//! ファイル系（`docs/gui/api.md` §3.7〜§3.9、ADR-0013 D11）: run のログと成果物のパス解決・検査、配信、成果物一覧。
//!
//! ユーザ入力のパスは受け取らない。`run_id` は ULID 形式を検査し、成果物は `events` に記録された `ArtifactRef.path` を使う。
//! 対象はワークスペース（`WorkspaceSpec::Local`）に結合して `canonicalize` し、ワークスペースの canonical パス配下で
//! なければ 403。`Content-Type` は閉じた表で決め、それ以外は `application/octet-stream`（能動的な型は返さない）。

use std::fmt::Write as _;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use task_core::{ArtifactRef, Event, EventRow, Task, WorkspaceSpec};
use task_ops::inbox::EvidenceView;
use task_ops::view::RunFiles;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::SHA256_MAX_BYTES;
use crate::problem::{ApiProblem, X_TASKD_SIZE};
use crate::query::QueryParams;
use crate::types::ArtifactView;

const X_TASKD_SHA256: HeaderName = HeaderName::from_static("x-taskd-sha256");
const X_TASKD_SHA256_CURRENT: HeaderName = HeaderName::from_static("x-taskd-sha256-current");
const CHUNK_BYTES: usize = 64 * 1024;
/// 受信箱の evidence のために読む `result.json` の上限。
const RESULT_JSON_MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunFile {
    Stdout,
    Stderr,
    Result,
    /// ADR-0023 D2: ワーカーに渡した `RunRequest`（`run_subprocess` が書く）。
    Request,
}

impl RunFile {
    pub(crate) fn file_name(self) -> &'static str {
        match self {
            RunFile::Stdout => "stdout.jsonl",
            RunFile::Stderr => "stderr.log",
            RunFile::Result => "result.json",
            RunFile::Request => "request.json",
        }
    }
}

/// `^[0-9A-HJKMNP-TV-Z]{26}$`（Crockford base32 の大文字）。
pub(crate) fn is_ulid_text(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z'))
}

/// ワークスペースの canonical パス。存在しなければ 404 `file_not_found`。
/// `Remote` は手元の写し `root/<task_id>`（run のログと成果物はそこにある。ADR-0018 D1）。
pub(crate) fn canonical_workspace(task: &Task, root: &Path) -> Result<PathBuf, ApiProblem> {
    let dir = match &task.workspace {
        WorkspaceSpec::Local { path } => root.join(path),
        WorkspaceSpec::Remote { .. } => root.join(task.id.to_string()),
    };
    dir.canonicalize()
        .map_err(|_| ApiProblem::file_not_found("workspace directory does not exist"))
}

/// `candidate` を canonicalize し、`ws` 配下の通常ファイルであることを確かめる。
fn contained_file(ws: &Path, candidate: &Path, missing: impl FnOnce() -> ApiProblem) -> Result<PathBuf, ApiProblem> {
    let canonical = candidate.canonicalize().map_err(|_| missing())?;
    if !canonical.starts_with(ws) {
        return Err(ApiProblem::path_forbidden("path resolves outside the workspace"));
    }
    let metadata = std::fs::metadata(&canonical).map_err(|_| ApiProblem::file_not_found("file does not exist"))?;
    if !metadata.is_file() {
        return Err(ApiProblem::path_forbidden("path is not a regular file"));
    }
    Ok(canonical)
}

/// `<ws>/runs/<run_id>/<file>`。`run_id` が ULID でなければ（ファイルシステムに触る前に）403。
pub(crate) fn resolve_run_file(task: &Task, root: &Path, run_id: &str, file: RunFile) -> Result<PathBuf, ApiProblem> {
    if !is_ulid_text(run_id) {
        return Err(ApiProblem::path_forbidden("run_id must be a ULID"));
    }
    let ws = canonical_workspace(task, root)?;
    let run_dir = ws
        .join("runs")
        .join(run_id)
        .canonicalize()
        .map_err(|_| ApiProblem::run_not_found(run_id))?;
    if !run_dir.starts_with(&ws) {
        return Err(ApiProblem::path_forbidden("run directory resolves outside the workspace"));
    }
    if !run_dir.is_dir() {
        return Err(ApiProblem::run_not_found(run_id));
    }
    contained_file(&ws, &run_dir.join(file.file_name()), || {
        ApiProblem::file_not_found(format!("{} does not exist", file.file_name()))
    })
}

/// 記録された成果物のパス。空・絶対パス・`..` を含むものはワーカー側の規則（ADR-0003 D5）と同じく 403。
pub(crate) fn resolve_artifact(ws: &Path, recorded: &str) -> Result<PathBuf, ApiProblem> {
    let relative = Path::new(recorded);
    let escapes = recorded.is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)));
    if escapes {
        return Err(ApiProblem::path_forbidden("artifact path is not workspace-relative"));
    }
    contained_file(ws, &ws.join(relative), || {
        ApiProblem::file_not_found("artifact file does not exist")
    })
}

pub(crate) fn file_size(path: &Path) -> Result<u64, ApiProblem> {
    std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|_| ApiProblem::file_not_found("file does not exist"))
}

/// `GET /tasks/{id}/runs` の `files`: ファイル系エンドポイントで取得できるものを `true` にする。
pub(crate) fn run_files(task: &Task, root: &Path, run_id: &str) -> RunFiles {
    let has = |file| resolve_run_file(task, root, run_id, file).is_ok();
    RunFiles {
        stdout: has(RunFile::Stdout),
        stderr: has(RunFile::Stderr),
        result: has(RunFile::Result),
        request: has(RunFile::Request),
    }
}

#[derive(Deserialize)]
struct EvidenceLine {
    criterion: usize,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    exit: Option<i32>,
    #[serde(default)]
    stdout_tail: Option<String>,
}

/// 受信箱の `evidence`: `<ws>/runs/<run_id>/result.json` が `done` なら `evidence[]`（読めなければ空）。
/// 形の合わない要素は読み飛ばす。
pub(crate) fn read_evidence(task: &Task, root: &Path, run_id: &str) -> Vec<EvidenceView> {
    let Ok(path) = resolve_run_file(task, root, run_id, RunFile::Result) else {
        return Vec::new();
    };
    if file_size(&path).map_or(true, |size| size > RESULT_JSON_MAX_BYTES) {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
        return Vec::new();
    };
    if value.get("type").and_then(serde_json::Value::as_str) != Some("done") {
        return Vec::new();
    }
    value
        .get("evidence")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<EvidenceLine>(item.clone()).ok())
                .map(|e| EvidenceView {
                    criterion: e.criterion,
                    command: e.command,
                    exit: e.exit,
                    stdout_tail: e.stdout_tail,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn sha256_hex(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let mut out = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

/// 64 MiB 以下のときだけ現在の sha256 を計算する。
pub(crate) fn current_sha256(path: &Path, size: u64) -> Option<String> {
    if size > SHA256_MAX_BYTES {
        return None;
    }
    sha256_hex(path).ok()
}

/// `ArtifactProduced` を出現順に `(row, run_id, artifact)` で列挙する。
fn produced(rows: &[EventRow]) -> impl Iterator<Item = (&EventRow, &String, &ArtifactRef)> {
    rows.iter().filter_map(|row| match &row.event {
        Event::ArtifactProduced { run_id, artifact } => Some((row, run_id, artifact)),
        _ => None,
    })
}

pub(crate) fn nth_artifact(rows: &[EventRow], idx: usize) -> Option<&ArtifactRef> {
    produced(rows).nth(idx).map(|(_, _, artifact)| artifact)
}

/// `GET /tasks/{id}/artifacts`（api.md §3.9）。
pub(crate) fn artifact_views(task: &Task, root: &Path, rows: &[EventRow]) -> Vec<ArtifactView> {
    let ws = canonical_workspace(task, root);
    produced(rows)
        .enumerate()
        .map(|(idx, (row, run_id, artifact))| {
            let resolved = match &ws {
                Ok(ws) => resolve_artifact(ws, &artifact.path),
                Err(problem) => Err(problem.clone()),
            };
            let base = ArtifactView {
                idx,
                run_id: run_id.clone(),
                ts: row.ts.clone(),
                artifact: artifact.clone(),
                exists: false,
                forbidden: false,
                size: None,
                sha256_current: None,
                sha256_matches: None,
            };
            match resolved {
                Ok(path) => {
                    let size = file_size(&path).ok();
                    let current = size.and_then(|s| current_sha256(&path, s));
                    let matches = current.as_ref().map(|c| c.eq_ignore_ascii_case(&artifact.sha256));
                    ArtifactView {
                        exists: size.is_some(),
                        size,
                        sha256_current: current,
                        sha256_matches: matches,
                        ..base
                    }
                }
                Err(problem) => ArtifactView {
                    forbidden: problem.code() == "path_forbidden",
                    ..base
                },
            }
        })
        .collect()
}

/// 閉じた Content-Type の表（api.md §3.8）。表に無い拡張子は `application/octet-stream`。
pub(crate) fn content_type_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some(
            "txt" | "log" | "jsonl" | "diff" | "patch" | "csv" | "tsv" | "toml" | "yaml" | "yml" | "rs" | "py" | "sh"
            | "ts" | "js" | "c" | "h" | "cpp" | "go" | "java" | "sql",
        ) => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        Some("md") => "text/markdown; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

/// `inline` / `attachment` と、ASCII の `filename` と RFC 8187 の `filename*`。
pub(crate) fn content_disposition(attachment: bool, filename: &str) -> String {
    let kind = if attachment { "attachment" } else { "inline" };
    let fallback: String = filename
        .chars()
        .map(|c| {
            if (c.is_ascii_graphic() && c != '"' && c != '\\') || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(filename.len());
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    format!("{kind}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// ファイル系の要求の `Range` / `offset` / `length` / `download`。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct FileRequest {
    pub(crate) range: Option<String>,
    pub(crate) offset: Option<u64>,
    pub(crate) length: Option<u64>,
    pub(crate) download: bool,
}

impl FileRequest {
    pub(crate) fn parse(raw: Option<&str>, headers: &HeaderMap) -> Result<Self, ApiProblem> {
        let query = QueryParams::parse(raw, &["offset", "length", "download"])?;
        let range = match headers.get(header::RANGE) {
            Some(value) => Some(
                value
                    .to_str()
                    .map_err(|_| ApiProblem::bad_request("Range header must be ASCII"))?
                    .to_string(),
            ),
            None => None,
        };
        let offset = query.u64("offset")?;
        let length = query.u64("length")?;
        if range.is_some() && (offset.is_some() || length.is_some()) {
            return Err(ApiProblem::bad_request(
                "Range cannot be combined with offset/length",
            ));
        }
        Ok(Self {
            range,
            offset,
            length,
            download: query.bool("download")?.unwrap_or(false),
        })
    }
}

/// 返す範囲。`partial` は 206 + `Content-Range`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Slice {
    pub(crate) start: u64,
    pub(crate) len: u64,
    pub(crate) partial: bool,
}

/// `Range: bytes=a-b | a- | -n`（単一範囲のみ。複数・不正・開始 >= サイズは 416。`bytes` 以外の単位は無視）、
/// または `offset` / `length`（`offset == size` は 200 の空本体、`offset > size` は 416）。
pub(crate) fn plan_slice(request: &FileRequest, size: u64) -> Result<Slice, ApiProblem> {
    if let Some(range) = &request.range
        && let Some(slice) = parse_range(range, size)?
    {
        return Ok(slice);
    }
    if request.offset.is_some() || request.length.is_some() {
        let start = request.offset.unwrap_or(0);
        if start > size {
            return Err(ApiProblem::range_not_satisfiable(size));
        }
        let remaining = size - start;
        let len = request.length.map_or(remaining, |l| l.min(remaining));
        return Ok(Slice {
            start,
            len,
            partial: false,
        });
    }
    Ok(Slice {
        start: 0,
        len: size,
        partial: false,
    })
}

fn parse_range(value: &str, size: u64) -> Result<Option<Slice>, ApiProblem> {
    let unsatisfiable = || ApiProblem::range_not_satisfiable(size);
    let Some((unit, spec)) = value.split_once('=') else {
        return Err(unsatisfiable());
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return Ok(None);
    }
    if spec.contains(',') {
        return Err(unsatisfiable());
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Err(unsatisfiable());
    };
    let (first, last) = (first.trim(), last.trim());
    let (start, end) = if first.is_empty() {
        let suffix: u64 = last.parse().map_err(|_| unsatisfiable())?;
        if suffix == 0 || size == 0 {
            return Err(unsatisfiable());
        }
        (size - suffix.min(size), size - 1)
    } else {
        let start: u64 = first.parse().map_err(|_| unsatisfiable())?;
        if start >= size {
            return Err(unsatisfiable());
        }
        let end = if last.is_empty() {
            size - 1
        } else {
            let end: u64 = last.parse().map_err(|_| unsatisfiable())?;
            if end < start {
                return Err(unsatisfiable());
            }
            end.min(size - 1)
        };
        (start, end)
    };
    Ok(Some(Slice {
        start,
        len: end - start + 1,
        partial: true,
    }))
}

/// 検査済みの配信対象。
pub(crate) struct FileTarget {
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
    pub(crate) recorded_sha256: Option<String>,
    pub(crate) current_sha256: Option<String>,
}

/// 本体をストリーミングで返す。`X-Taskd-Size` は常に、成果物は `X-Taskd-Sha256(-Current)` も付ける。
pub(crate) async fn respond_file(target: FileTarget, request: &FileRequest) -> Result<Response, ApiProblem> {
    let slice = plan_slice(request, target.size)?;
    let body = if slice.len == 0 {
        Body::empty()
    } else {
        open_slice(&target.path, slice.start, slice.len)
            .await
            .map_err(|_| ApiProblem::file_not_found("file could not be read"))?
    };
    let mut response = Response::new(body);
    *response.status_mut() = if slice.partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type_for(&target.path)));
    let name = target
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Ok(value) = HeaderValue::from_str(&content_disposition(request.download, &name)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(slice.len));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(X_TASKD_SIZE, HeaderValue::from(target.size));
    if slice.partial {
        let end = slice.start + slice.len - 1;
        if let Ok(value) = HeaderValue::from_str(&format!("bytes {}-{end}/{}", slice.start, target.size)) {
            headers.insert(header::CONTENT_RANGE, value);
        }
    }
    if let Some(value) = target.recorded_sha256.as_deref().and_then(|s| HeaderValue::from_str(s).ok()) {
        headers.insert(X_TASKD_SHA256, value);
    }
    if let Some(value) = target.current_sha256.as_deref().and_then(|s| HeaderValue::from_str(s).ok()) {
        headers.insert(X_TASKD_SHA256_CURRENT, value);
    }
    Ok(response)
}

async fn open_slice(path: &Path, start: u64, len: u64) -> io::Result<Body> {
    let mut file = tokio::fs::File::open(path).await?;
    if start > 0 {
        file.seek(io::SeekFrom::Start(start)).await?;
    }
    let reader = file.take(len);
    let stream = futures_util::stream::unfold(Some(reader), |state| async move {
        let mut reader = state?;
        let mut buf = vec![0u8; CHUNK_BYTES];
        match reader.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Bytes, io::Error>(Bytes::from(buf)), Some(reader)))
            }
            Err(e) => Some((Err(e), None)),
        }
    });
    Ok(Body::from_stream(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(range: Option<&str>, offset: Option<u64>, length: Option<u64>) -> FileRequest {
        FileRequest {
            range: range.map(str::to_string),
            offset,
            length,
            download: false,
        }
    }

    #[test]
    fn ulid_text_accepts_only_crockford_uppercase_26_chars() {
        assert!(is_ulid_text("01J9ZX5T3K8Q7W6V5R4P3N2M1H"));
        assert!(!is_ulid_text("01j9zx5t3k8q7w6v5r4p3n2m1h"));
        assert!(!is_ulid_text("01J9ZX5T3K8Q7W6V5R4P3N2M1I"));
        assert!(!is_ulid_text(".."));
        assert!(!is_ulid_text("01J9ZX5T3K8Q7W6V5R4P3N2M1"));
    }

    #[test]
    fn content_type_table_is_closed() {
        let ct = |name: &str| content_type_for(Path::new(name));
        assert_eq!(ct("stdout.jsonl"), "text/plain; charset=utf-8");
        assert_eq!(ct("main.RS"), "text/plain; charset=utf-8");
        assert_eq!(ct("result.json"), "application/json");
        assert_eq!(ct("report.md"), "text/markdown; charset=utf-8");
        assert_eq!(ct("a.png"), "image/png");
        assert_eq!(ct("a.jpeg"), "image/jpeg");
        assert_eq!(ct("a.webp"), "image/webp");
        assert_eq!(ct("index.html"), "application/octet-stream");
        assert_eq!(ct("icon.svg"), "application/octet-stream");
        assert_eq!(ct("noext"), "application/octet-stream");
    }

    #[test]
    fn content_disposition_escapes_filenames() {
        assert_eq!(
            content_disposition(false, "bench.json"),
            "inline; filename=\"bench.json\"; filename*=UTF-8''bench.json"
        );
        assert_eq!(
            content_disposition(true, "レポート \"x\".md"),
            "attachment; filename=\"____ _x_.md\"; filename*=UTF-8''%E3%83%AC%E3%83%9D%E3%83%BC%E3%83%88%20%22x%22.md"
        );
    }

    #[test]
    fn range_and_offset_planning_follows_the_spec() {
        let ok = |r: &FileRequest, size| plan_slice(r, size).ok();
        let code = |r: &FileRequest, size| plan_slice(r, size).err().map(|p| p.code());
        assert_eq!(ok(&req(None, None, None), 10), Some(Slice { start: 0, len: 10, partial: false }));
        assert_eq!(ok(&req(Some("bytes=2-5"), None, None), 10), Some(Slice { start: 2, len: 4, partial: true }));
        assert_eq!(ok(&req(Some("bytes=8-"), None, None), 10), Some(Slice { start: 8, len: 2, partial: true }));
        assert_eq!(ok(&req(Some("bytes=5-100"), None, None), 10), Some(Slice { start: 5, len: 5, partial: true }));
        assert_eq!(ok(&req(Some("bytes=-3"), None, None), 10), Some(Slice { start: 7, len: 3, partial: true }));
        assert_eq!(code(&req(Some("bytes=10-"), None, None), 10), Some("range_not_satisfiable"));
        assert_eq!(code(&req(Some("bytes=0-1,4-5"), None, None), 10), Some("range_not_satisfiable"));
        assert_eq!(code(&req(Some("bytes=5-2"), None, None), 10), Some("range_not_satisfiable"));
        assert_eq!(code(&req(Some("bytes=abc"), None, None), 10), Some("range_not_satisfiable"));
        assert_eq!(ok(&req(Some("items=0-1"), None, None), 10), Some(Slice { start: 0, len: 10, partial: false }));
        assert_eq!(ok(&req(None, Some(4), None), 10), Some(Slice { start: 4, len: 6, partial: false }));
        assert_eq!(ok(&req(None, Some(4), Some(3)), 10), Some(Slice { start: 4, len: 3, partial: false }));
        assert_eq!(ok(&req(None, Some(10), None), 10), Some(Slice { start: 10, len: 0, partial: false }));
        assert_eq!(code(&req(None, Some(11), None), 10), Some("range_not_satisfiable"));
    }

    #[test]
    fn range_and_offset_together_are_a_bad_request() {
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-1"));
        let err = FileRequest::parse(Some("offset=1"), &headers).err().map(|p| p.code());
        assert_eq!(err, Some("bad_request"));
    }

    #[test]
    fn artifact_paths_that_are_not_workspace_relative_are_forbidden() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let ws = dir.path().canonicalize().unwrap_or_else(|e| panic!("{e}"));
        for bad in ["", "/etc/passwd", "../x", "artifacts/../../x"] {
            assert_eq!(resolve_artifact(&ws, bad).err().map(|p| p.code()), Some("path_forbidden"), "{bad}");
        }
        assert_eq!(
            resolve_artifact(&ws, "artifacts/missing.txt").err().map(|p| p.code()),
            Some("file_not_found")
        );
    }
}
