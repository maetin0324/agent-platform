CREATE TABLE deliveries (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    json TEXT NOT NULL
);
