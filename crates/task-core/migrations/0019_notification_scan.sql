CREATE TABLE notification_scan_state (id INTEGER PRIMARY KEY CHECK (id = 1), scanned_at TEXT NOT NULL);
-- 既存の送信台帳がある運用は最後の通知以降を拾い直す。
-- 履歴のない新規環境は空のままにし、最初の起動時刻から始める。
INSERT INTO notification_scan_state (id, scanned_at)
SELECT 1, MAX(created_at) FROM notifications HAVING MAX(created_at) IS NOT NULL;
