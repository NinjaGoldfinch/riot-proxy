-- ADM-02: the dashboard's job counts read an index instead of the table.
-- Every count over `jobs` (the snapshot's queues, `GET /v1/admin/jobs/stats`,
-- the `jobs_pending{kind}` gauge) groups by state and kind and looks only at
-- these four columns, so this index covers them all and the 1 M-row table's
-- payloads are never read. About 50 bytes a row.
CREATE INDEX jobs_state_kind ON jobs(state, kind, run_after, finished_at);
