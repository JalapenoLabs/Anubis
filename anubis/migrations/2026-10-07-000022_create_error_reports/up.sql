-- One row per distinct fault the application noticed in itself, and the issue
-- it was filed as.
--
-- A 500, a panic, a browser that crashed rendering a page, and a background
-- pass that fell over all end up here, and from here as an issue in the
-- tracker the application configured. This table exists for one reason: **an
-- error in a loop must be one issue, not one per occurrence.** Asking the
-- tracker "is this already filed" is the obvious design and the wrong one:
-- GitHub's search index lags by seconds to minutes, which is thousands of
-- occurrences of a hot loop, and every question costs rate limit. A row here
-- with a unique fingerprint, counted under its own row lock, answers the same
-- question atomically and costs nothing.
CREATE TABLE error_reports (
    -- What two occurrences of one fault have in common: the source, the kind,
    -- and a normalized signature, hashed. Normalization is what stops a
    -- message carrying a uuid or a line number from reading as a new fault
    -- every time. See `anubis::reporting::fingerprint`.
    fingerprint TEXT PRIMARY KEY,
    -- `server`, `browser` or `worker`, and a dotted kind such as
    -- `http.request.failed`. Copied from the report so the row reads alone.
    source TEXT NOT NULL,
    kind TEXT NOT NULL,
    -- The issue title, kept so a row says what it is about without a round
    -- trip to the tracker.
    title TEXT NOT NULL,
    -- The latest occurrence, already redacted: its summary, its facts and its
    -- detail. The filing job renders the issue from this, which is what lets
    -- the job's own payload carry nothing but the fingerprint.
    sample JSONB NOT NULL,
    -- Where the fingerprint stands with the tracker.
    --
    -- `queued`: a filing job is in the queue, or was and its process died.
    -- `open`: an issue exists and was open the last time anybody looked.
    -- `failed`: the tracker refused. `withheld`: the hourly ceiling was taken.
    -- `unconfigured`: nothing is configured to file with.
    --
    -- Every state but `open` and a fresh `queued` is retried on a backoff by
    -- the next occurrence. A closed issue is not a state of its own: the
    -- recheck that finds one files the replacement in the same breath.
    state TEXT NOT NULL DEFAULT 'queued'
        CHECK (state IN ('queued', 'open', 'failed', 'withheld', 'unconfigured')),
    -- The current issue. Null until one exists, and again after a failure.
    issue_number BIGINT,
    issue_url TEXT,
    occurrences BIGINT NOT NULL DEFAULT 1,
    -- How many occurrences the issue already says, so a recheck that finds it
    -- open can comment once with the difference rather than once per
    -- occurrence.
    reported_occurrences BIGINT NOT NULL DEFAULT 0,
    -- Why the last attempt did not produce an issue. Null when it did.
    failure TEXT,
    -- Whether a create may have reached the tracker without its outcome being
    -- recorded here.
    --
    -- "May", precisely. It is set when a filing is granted, before the request
    -- goes out, so true means the create might have happened. The next attempt
    -- reads it and searches the tracker for the fingerprint before creating,
    -- which is the difference between adopting an issue a dead process filed
    -- and filing a duplicate of it.
    create_unconfirmed BOOLEAN NOT NULL DEFAULT false,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When anything was last asked of the tracker about this fingerprint,
    -- which is what both the recheck and the retry backoff are measured from.
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When this row last created an issue. **It records the filing, not the
    -- issue's continued existence**: the hourly ceiling counts issues created
    -- in the window, and one created and then closed was still created.
    filed_at TIMESTAMPTZ
);

-- The hourly ceiling counts filings inside the window.
CREATE INDEX error_reports_filed_at ON error_reports (filed_at) WHERE filed_at IS NOT NULL;

-- An application bounding how many new faults one source may introduce in a
-- window, which is how an anonymous endpoint is kept from minting fingerprints.
CREATE INDEX error_reports_source_first_seen ON error_reports (source, first_seen_at);

-- What an operator's screen lists: the faults seen most recently.
CREATE INDEX error_reports_last_seen ON error_reports (last_seen_at DESC);
