-- P7-06 (ADR-058): the last analytics recompute per ladder, for the dashboard
-- (v1 kept it in Redis as `analytics:run:<scope>:<platform>:<queue>`, 30 days).
CREATE TABLE analytics_runs (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  at INTEGER NOT NULL,                 -- when the run ended, unix ms
  status TEXT NOT NULL,                -- completed | failed
  ms INTEGER NOT NULL,                 -- wall-clock length
  steps TEXT NOT NULL,                 -- JSON {step: seconds}, the steps that finished
  rows TEXT NOT NULL,                  -- JSON {table: rows written}
  games INTEGER NOT NULL,              -- games in the ladder's stats; 0 for a failed run
  PRIMARY KEY (key_scope, platform, queue)
);
