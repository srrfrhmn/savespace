-- Phase 1 M6: notification settings, push subscriptions, and notifications
-- (design doc §10.4, D13).

ALTER TABLE app.users
    -- At most this many notifications a day (default 1). Concerns always send.
    ADD COLUMN notify_daily_limit SMALLINT NOT NULL DEFAULT 1
        CHECK (notify_daily_limit BETWEEN 0 AND 5),
    -- Insight kinds the person does not want as notifications.
    ADD COLUMN notify_muted_kinds TEXT[] NOT NULL DEFAULT '{}';

-- One row per browser that allowed notifications.
CREATE TABLE app.push_subscriptions (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    user_id       UUID        NOT NULL REFERENCES app.users (id) ON DELETE CASCADE,
    -- On a known push service only (domain-notify::webpush::allowed_endpoint).
    endpoint      TEXT        NOT NULL UNIQUE CHECK (length(endpoint) <= 1000),
    p256dh        BYTEA       NOT NULL,
    auth          BYTEA       NOT NULL,
    failures      INTEGER     NOT NULL DEFAULT 0,
    last_sent_at  TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One row per insight that may become a notification. The policy decides at
-- send time (§10.4).
CREATE TABLE app.notifications (
    id             UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id   UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    user_id        UUID        NOT NULL REFERENCES app.users (id) ON DELETE CASCADE,
    insight_id     UUID        UNIQUE REFERENCES app.insights (id) ON DELETE SET NULL,
    kind           TEXT        NOT NULL,
    status         TEXT        NOT NULL DEFAULT 'queued'
                   CHECK (status IN ('queued', 'sent', 'skipped', 'expired', 'failed')),
    -- Why it was skipped or failed: muted, daily_limit, no_subscription, ...
    reason         TEXT,
    scheduled_for  TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at        TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX notifications_due_idx ON app.notifications (scheduled_for) WHERE status = 'queued';

ALTER TABLE app.push_subscriptions ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.notifications      ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant ON app.push_subscriptions
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.notifications
    USING (household_id = app.current_household_id());

-- Households with notifications due now. The hourly run sends notifications
-- that quiet hours delayed.
CREATE FUNCTION app.households_with_due_notifications()
    RETURNS TABLE (household_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
    SELECT DISTINCT n.household_id FROM app.notifications n
    WHERE n.status = 'queued' AND n.scheduled_for <= now()
$$;
REVOKE ALL ON FUNCTION app.households_with_due_notifications() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app.households_with_due_notifications() TO savespace_runtime;
