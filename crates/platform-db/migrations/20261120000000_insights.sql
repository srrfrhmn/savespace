-- Phase 1 M5: insights, feedback, mutes, kill switches, and check-ins
-- (design doc §7, §6.1).

-- Insights ---------------------------------------------------------------------
-- One row per rule and scope while it is current. The text is the template
-- text (D10: no LLM). `facts` holds what the text uses, for "Why am I seeing
-- this?".
CREATE TABLE app.insights (
    id              UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id    UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    rule_id         TEXT        NOT NULL,
    rule_version    INTEGER     NOT NULL,
    kind            TEXT        NOT NULL CHECK (kind IN
                        ('reassurance', 'awareness', 'anticipation', 'suggestion', 'concern')),
    -- What the insight is about, for example `month:2026-11` or `txn:<id>`.
    scope           TEXT        NOT NULL,
    text            TEXT        NOT NULL CHECK (length(text) <= 280),
    facts           JSONB       NOT NULL,
    confidence_bp   INTEGER     NOT NULL CHECK (confidence_bp BETWEEN 0 AND 10000),
    relevance_bp    INTEGER     NOT NULL CHECK (relevance_bp BETWEEN 0 AND 10000),
    render_mode     TEXT        NOT NULL DEFAULT 'template' CHECK (render_mode IN ('template', 'llm')),
    transaction_id  UUID        REFERENCES app.transactions (id) ON DELETE CASCADE,
    -- open: can show. expired: the condition no longer holds. dismissed: "Not
    -- helpful". wrong: "This is wrong" (hidden at once, in the review queue).
    status          TEXT        NOT NULL DEFAULT 'open'
                    CHECK (status IN ('open', 'expired', 'dismissed', 'wrong')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX insights_open_idx ON app.insights (household_id) WHERE status = 'open';
CREATE INDEX insights_scope_idx ON app.insights (household_id, rule_id, scope, created_at DESC);

CREATE TABLE app.insight_feedback (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    insight_id    UUID        NOT NULL REFERENCES app.insights (id) ON DELETE CASCADE,
    rule_id       TEXT        NOT NULL,
    rule_version  INTEGER     NOT NULL,
    kind          TEXT        NOT NULL CHECK (kind IN ('wrong', 'not_helpful')),
    reason        TEXT        CHECK (reason IN ('amount', 'merchant', 'category', 'date', 'other')),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX insight_feedback_rule_idx ON app.insight_feedback (household_id, rule_id, created_at);

-- A rule the person muted (3 "Not helpful" in 30 days, §7.7).
CREATE TABLE app.insight_mutes (
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    rule_id       TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (household_id, rule_id)
);

-- Kill switch per rule (§13: a flag turns off one rule in less than 1 minute).
-- Global, not per household. Operators change it with SQL as the owner role.
CREATE TABLE app.insight_rule_flags (
    rule_id     TEXT        PRIMARY KEY,
    enabled     BOOLEAN     NOT NULL,
    reason      TEXT,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
REVOKE INSERT, UPDATE, DELETE ON app.insight_rule_flags FROM savespace_runtime;

-- The local date of the last daily insight run.
ALTER TABLE app.households ADD COLUMN insights_run_on DATE;

-- Check-ins (§6.1) ------------------------------------------------------------
-- "How do you feel about money today?" 1 to 5. The note is encrypted with the
-- household data key (platform-crypto, purpose Note).
CREATE TABLE app.check_ins (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    user_id       UUID        NOT NULL REFERENCES app.users (id) ON DELETE CASCADE,
    score         SMALLINT    NOT NULL CHECK (score BETWEEN 1 AND 5),
    note_enc      BYTEA,
    local_date    DATE        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX check_ins_user_idx ON app.check_ins (user_id, created_at DESC);

-- Row-level security -----------------------------------------------------------
ALTER TABLE app.insights         ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.insight_feedback ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.insight_mutes    ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.check_ins        ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant ON app.insights
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.insight_feedback
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.insight_mutes
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.check_ins
    USING (household_id = app.current_household_id());

-- The daily run: households and their owner's time zone ------------------------
CREATE FUNCTION app.households_for_daily_run()
    RETURNS TABLE (id uuid, time_zone text, insights_run_on date)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
    SELECT h.id, coalesce((
        SELECT u.time_zone FROM app.household_members m JOIN app.users u ON u.id = m.user_id
        WHERE m.household_id = h.id ORDER BY m.created_at LIMIT 1), 'America/New_York'),
      h.insights_run_on
    FROM app.households h
$$;
REVOKE ALL ON FUNCTION app.households_for_daily_run() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app.households_for_daily_run() TO savespace_runtime;
