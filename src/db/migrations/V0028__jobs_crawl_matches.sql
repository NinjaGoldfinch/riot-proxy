-- WR-01: a crawl's activity view counts the platform's crawl-found match
-- downloads (`kind = 'archive:match'` at the crawl-match priority, dedupe key
-- `OC1_…`) by state. Without this index that count read every archive:match
-- row from the table, about 1.3 s with 1.2 M jobs. The key is NOCASE so the
-- view's case-insensitive `dedupe_key LIKE 'OC1\_%'` can seek on it, and the
-- index covers the count, so payloads are never read.
CREATE INDEX jobs_crawl_matches
  ON jobs(priority, dedupe_key COLLATE NOCASE, state, run_after, finished_at)
  WHERE kind = 'archive:match';
