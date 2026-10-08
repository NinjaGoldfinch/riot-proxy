-- SCH-01 (ADR-089): the limiter scope a job's requests hit (`lane`: a platform
-- or region) and the endpoint it mainly calls (`method`), so a claim can skip
-- work the rate limiter has no room for. NULL for kinds that never call Riot.
-- Rows queued before this migration get theirs at boot (`assign_lanes`).
ALTER TABLE jobs ADD COLUMN lane TEXT;
ALTER TABLE jobs ADD COLUMN method TEXT;
-- The claim reads each lane's best ready row, and each lane's running count.
CREATE INDEX jobs_lane_claim ON jobs(lane, priority, run_after, id) WHERE state = 'pending';
CREATE INDEX jobs_lane_running ON jobs(lane) WHERE state = 'running';
