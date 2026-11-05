-- Phase 0: schema, async work tables (design doc §8.6).
-- Requires PostgreSQL 18 or later (built-in uuidv7()).
-- Migrations are forward-only. Use expand and contract for changes (doc §9.3).

CREATE SCHEMA IF NOT EXISTS app;

-- Job queue. Workers claim rows with SELECT ... FOR UPDATE SKIP LOCKED.
CREATE TABLE app.jobs (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    kind          TEXT        NOT NULL,
    payload       JSONB       NOT NULL DEFAULT '{}'::jsonb,
    -- Two live jobs with the same dedupe key are not allowed.
    dedupe_key    TEXT,
    status        TEXT        NOT NULL DEFAULT 'queued'
                  CHECK (status IN ('queued', 'running', 'succeeded', 'dead')),
    run_after     TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts      INTEGER     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts  INTEGER     NOT NULL DEFAULT 10 CHECK (max_attempts > 0),
    locked_by     TEXT,
    locked_until  TIMESTAMPTZ,
    last_error    TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The claim query reads queued jobs in run_after order.
CREATE INDEX jobs_claim_idx ON app.jobs (run_after) WHERE status = 'queued';

-- Dedupe applies only to jobs that are not finished.
CREATE UNIQUE INDEX jobs_dedupe_live_idx
    ON app.jobs (kind, dedupe_key)
    WHERE dedupe_key IS NOT NULL AND status IN ('queued', 'running');

-- Transactional outbox. A domain write and its event commit in one transaction.
CREATE TABLE app.outbox (
    -- BIGSERIAL gives a strict publish order for the relay.
    seq            BIGSERIAL   PRIMARY KEY,
    id             UUID        NOT NULL UNIQUE DEFAULT uuidv7(),
    topic          TEXT        NOT NULL,
    household_id   UUID,
    payload        JSONB       NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at   TIMESTAMPTZ
);

CREATE INDEX outbox_unpublished_idx ON app.outbox (seq) WHERE published_at IS NULL;
