-- Phase 1 M2: bank connections, accounts, balances, and transactions
-- (design doc §9.2, §10.1, §10.2).
--
-- Amounts are BIGINT minor units plus an ISO 4217 code. Sign: negative is
-- money out of the account, positive is money in. (Plaid uses the opposite
-- sign. The adapter converts it.)

-- Job queue change -----------------------------------------------------------
-- A sync must be able to queue while another sync for the same connection
-- runs, or a webhook that arrives during a sync is lost until the next poll.
-- So the dedupe key now applies to queued jobs only.
--
-- Deploy note: code from before this migration uses the old ON CONFLICT
-- target, so its enqueue fails between this migration and the new worker.
-- That code enqueues only in worker schedule(), which logs and continues.
DROP INDEX app.jobs_dedupe_live_idx;
CREATE UNIQUE INDEX jobs_dedupe_queued_idx
    ON app.jobs (kind, dedupe_key)
    WHERE dedupe_key IS NOT NULL AND status = 'queued';

-- A retry that is not needed because the same work is already queued.
ALTER TABLE app.jobs DROP CONSTRAINT jobs_status_check;
ALTER TABLE app.jobs ADD CONSTRAINT jobs_status_check
    CHECK (status IN ('queued', 'running', 'succeeded', 'dead', 'superseded'));

-- Connections ----------------------------------------------------------------

-- One aggregator item: one login at one institution.
CREATE TABLE app.connections (
    id                UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id      UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    provider          TEXT        NOT NULL CHECK (provider IN ('plaid')),
    provider_item_id  TEXT        NOT NULL,
    -- Encrypted with the household data key (platform-crypto, §12.3).
    access_token_enc  BYTEA       NOT NULL,
    institution_id    TEXT,
    institution_name  TEXT,
    status            TEXT        NOT NULL DEFAULT 'syncing' CHECK (status IN (
                          'syncing', 'ok', 'login_required', 'pending_expiration',
                          'revoked', 'error')),
    -- The provider error code for the last failure, if any.
    error_code        TEXT,
    sync_cursor       TEXT,
    last_synced_at    TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, provider_item_id)
);

CREATE INDEX connections_household_idx ON app.connections (household_id);

CREATE TABLE app.accounts (
    id                   UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id         UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    connection_id        UUID        NOT NULL REFERENCES app.connections (id) ON DELETE CASCADE,
    provider_account_id  TEXT        NOT NULL,
    name                 TEXT        NOT NULL,
    official_name        TEXT,
    mask                 TEXT,
    type                 TEXT        NOT NULL,
    subtype              TEXT,
    currency             CHAR(3)     NOT NULL,
    -- The latest balances. History is in app.balances.
    current_minor        BIGINT,
    available_minor      BIGINT,
    limit_minor          BIGINT,
    balance_at           TIMESTAMPTZ,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (connection_id, provider_account_id)
);

CREATE INDEX accounts_household_idx ON app.accounts (household_id);

-- Balance time series: one row per account per sync.
CREATE TABLE app.balances (
    id              UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id    UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    account_id      UUID        NOT NULL REFERENCES app.accounts (id) ON DELETE CASCADE,
    currency        CHAR(3)     NOT NULL,
    current_minor   BIGINT,
    available_minor BIGINT,
    limit_minor     BIGINT,
    observed_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX balances_account_idx ON app.balances (account_id, observed_at);

-- Provider payloads, append-only. A modification adds a version. A removal
-- sets removed_at on the latest version (§9.1).
CREATE TABLE app.transactions_raw (
    id                        UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id              UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    connection_id             UUID        NOT NULL REFERENCES app.connections (id) ON DELETE CASCADE,
    provider_transaction_id   TEXT        NOT NULL,
    version                   INTEGER     NOT NULL CHECK (version >= 1),
    payload                   JSONB       NOT NULL,
    received_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    removed_at                TIMESTAMPTZ,
    UNIQUE (connection_id, provider_transaction_id, version)
);

-- The current form of each transaction, derived from the latest raw version.
-- Person edits come in M3 as an overlay (user_overrides).
CREATE TABLE app.transactions (
    id                               UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id                     UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    connection_id                    UUID        NOT NULL REFERENCES app.connections (id) ON DELETE CASCADE,
    account_id                       UUID        NOT NULL REFERENCES app.accounts (id) ON DELETE CASCADE,
    provider_transaction_id          TEXT        NOT NULL,
    raw_version                      INTEGER     NOT NULL,
    amount_minor                     BIGINT      NOT NULL,
    currency                         CHAR(3)     NOT NULL,
    -- The bank calendar date (§9.1).
    date                             DATE        NOT NULL,
    authorized_date                  DATE,
    name                             TEXT        NOT NULL,
    merchant_name                    TEXT,
    pending                          BOOLEAN     NOT NULL,
    -- For a posted transaction: the pending transaction it replaces.
    pending_provider_transaction_id  TEXT,
    provider_category_primary        TEXT,
    provider_category_detailed       TEXT,
    created_at                       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (connection_id, provider_transaction_id)
);

-- The list view: newest first, with a stable tie-break for cursors.
CREATE INDEX transactions_list_idx ON app.transactions (household_id, date DESC, id DESC);
CREATE INDEX transactions_account_idx ON app.transactions (account_id, date DESC);

-- Webhooks as received, before the household is known. The worker processes
-- them. Plaid webhook bodies hold item IDs and status codes, not personal data.
CREATE TABLE app.webhook_events (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    provider      TEXT        NOT NULL,
    item_id       TEXT,
    webhook_type  TEXT        NOT NULL,
    webhook_code  TEXT        NOT NULL,
    body          JSONB       NOT NULL,
    received_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at  TIMESTAMPTZ
);

CREATE INDEX webhook_events_received_idx ON app.webhook_events (received_at);

-- Row-level security -----------------------------------------------------------

ALTER TABLE app.connections      ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.accounts         ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.balances         ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.transactions_raw ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.transactions     ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant ON app.connections
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.accounts
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.balances
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.transactions_raw
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.transactions
    USING (household_id = app.current_household_id());

-- Raw data is append-only for the runtime role, except removed_at.
REVOKE DELETE ON app.transactions_raw FROM savespace_runtime;

-- Lookups before the household is known ---------------------------------------

-- Finds the connection for a webhook's item ID.
CREATE FUNCTION app.find_connection_by_item(p_provider text, p_item_id text)
    RETURNS TABLE (id uuid, household_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
    SELECT c.id, c.household_id FROM app.connections c
    WHERE c.provider = p_provider AND c.provider_item_id = p_item_id
$$;

-- Connections that need a fallback poll: healthy and not synced recently.
CREATE FUNCTION app.connections_due(p_max_age interval)
    RETURNS TABLE (id uuid, household_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
    SELECT c.id, c.household_id FROM app.connections c
    WHERE c.status IN ('ok', 'syncing', 'error')
      AND (c.last_synced_at IS NULL OR c.last_synced_at < now() - p_max_age)
$$;

REVOKE ALL ON FUNCTION app.find_connection_by_item(text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION app.connections_due(interval) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app.find_connection_by_item(text, text) TO savespace_runtime;
GRANT EXECUTE ON FUNCTION app.connections_due(interval) TO savespace_runtime;
