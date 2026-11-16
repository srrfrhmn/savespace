-- Phase 1 M1: identity, tenancy, and API support tables (design doc §9.2, §12).
--
-- Tenancy: every tenant table has household_id and an RLS policy on the
-- setting app.household_id. The api and worker connect as a member of
-- savespace_runtime, which is not the table owner, so RLS always applies to
-- them. platform_db::tenant_tx sets the value for one transaction.
--
-- Some lookups happen before the household is known (sign-in, session check).
-- They use the SECURITY DEFINER functions at the end of this file. Each one
-- does one narrow task, so the runtime role never reads other tenants' rows.

-- The current household, or NULL. NULL matches no row.
CREATE FUNCTION app.current_household_id() RETURNS uuid
    LANGUAGE sql STABLE
    AS $$ SELECT nullif(current_setting('app.household_id', true), '')::uuid $$;

CREATE TABLE app.households (
    id          UUID        PRIMARY KEY DEFAULT uuidv7(),
    -- The household data key (M2), encrypted with the key-encryption key.
    -- Deleting it makes encrypted fields unreadable (crypto-shredding, §12.5).
    dek_wrapped BYTEA,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE app.users (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    display_name  TEXT        NOT NULL CHECK (length(display_name) BETWEEN 1 AND 100),
    -- IANA time zone name. Days are bucketed in this zone (§9.1).
    time_zone     TEXT        NOT NULL DEFAULT 'America/New_York',
    calm_mode     BOOLEAN     NOT NULL DEFAULT false,
    quiet_start   TIME        NOT NULL DEFAULT '21:00',
    quiet_end     TIME        NOT NULL DEFAULT '08:00',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE app.household_members (
    household_id UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    user_id      UUID        NOT NULL REFERENCES app.users (id) ON DELETE CASCADE,
    role         TEXT        NOT NULL CHECK (role IN ('owner', 'member')),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (household_id, user_id)
);

CREATE INDEX household_members_user_idx ON app.household_members (user_id);

-- WebAuthn credentials. The serialized webauthn-rs Passkey holds the public
-- key and the signature counter. No secret is stored.
CREATE TABLE app.passkeys (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    user_id       UUID        NOT NULL REFERENCES app.users (id) ON DELETE CASCADE,
    credential_id BYTEA       NOT NULL UNIQUE,
    passkey       JSONB       NOT NULL,
    name          TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at  TIMESTAMPTZ
);

CREATE INDEX passkeys_user_idx ON app.passkeys (user_id);

-- Sessions. Only the SHA-256 hash of the token is stored (§12.1).
CREATE TABLE app.sessions (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    user_id       UUID        NOT NULL REFERENCES app.users (id) ON DELETE CASCADE,
    token_hash    BYTEA       NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Absolute limit. The idle limit is checked against last_seen_at.
    expires_at    TIMESTAMPTZ NOT NULL,
    -- The last strong check (passkey or recovery code). Step-up actions need
    -- a recent value (§12.1).
    verified_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    user_agent    TEXT
);

CREATE INDEX sessions_user_idx ON app.sessions (user_id);
CREATE INDEX sessions_expires_idx ON app.sessions (expires_at);

-- Single-use recovery codes (D12). Only the SHA-256 hash is stored.
CREATE TABLE app.recovery_codes (
    id           UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    user_id      UUID        NOT NULL REFERENCES app.users (id) ON DELETE CASCADE,
    code_hash    BYTEA       NOT NULL UNIQUE CHECK (length(code_hash) = 32),
    used_at      TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX recovery_codes_user_idx ON app.recovery_codes (user_id);

-- WebAuthn ceremony state between the start and finish requests. Rows live
-- for 5 minutes and are deleted when used. Not tenant data.
CREATE TABLE app.auth_challenges (
    id         UUID        PRIMARY KEY DEFAULT uuidv7(),
    purpose    TEXT        NOT NULL CHECK (purpose IN ('setup', 'login', 'add_passkey', 'verify')),
    state      JSONB       NOT NULL,
    -- Set for challenges that a signed-in session started.
    session_id UUID        REFERENCES app.sessions (id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '5 minutes'
);

CREATE INDEX auth_challenges_expires_idx ON app.auth_challenges (expires_at);

-- Security-relevant events (§9.2). Append-only for the runtime role.
CREATE TABLE app.audit_log (
    id           UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id UUID        REFERENCES app.households (id) ON DELETE CASCADE,
    user_id      UUID,
    action       TEXT        NOT NULL,
    detail       JSONB       NOT NULL DEFAULT '{}'::jsonb,
    request_id   TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX audit_log_household_idx ON app.audit_log (household_id, created_at);

-- API idempotency store (§11.3). Rows expire after 24 hours.
CREATE TABLE app.idempotency_keys (
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    key           TEXT        NOT NULL CHECK (length(key) BETWEEN 1 AND 255),
    request_hash  BYTEA       NOT NULL,
    -- NULL while the first request runs.
    status        SMALLINT,
    response      BYTEA,
    content_type  TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (household_id, key)
);

CREATE INDEX idempotency_keys_created_idx ON app.idempotency_keys (created_at);

-- Row-level security ---------------------------------------------------------

ALTER TABLE app.households        ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.users             ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.household_members ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.passkeys          ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.sessions          ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.recovery_codes    ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.audit_log         ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.idempotency_keys  ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant ON app.households
    USING (id = app.current_household_id());

-- A user is visible to the households the user belongs to.
CREATE POLICY tenant ON app.users
    USING (EXISTS (
        SELECT 1 FROM app.household_members m
        WHERE m.user_id = users.id AND m.household_id = app.current_household_id()
    ));

CREATE POLICY tenant ON app.household_members
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.passkeys
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.sessions
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.recovery_codes
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.idempotency_keys
    USING (household_id = app.current_household_id());

-- The audit log accepts events without a household (failed sign-ins).
CREATE POLICY tenant_read ON app.audit_log FOR SELECT
    USING (household_id = app.current_household_id());
CREATE POLICY tenant_insert ON app.audit_log FOR INSERT
    WITH CHECK (household_id IS NULL OR household_id = app.current_household_id());

REVOKE UPDATE, DELETE ON app.audit_log FROM savespace_runtime;

-- Lookups before the household is known --------------------------------------
-- SECURITY DEFINER functions run as the schema owner, so RLS does not apply
-- inside them. Keep each one narrow. search_path is fixed to block hijacks.

-- True if any user exists. The owner setup flow runs only once (D12).
CREATE FUNCTION app.owner_exists() RETURNS boolean
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = app, pg_temp
    AS $$ SELECT EXISTS (SELECT 1 FROM app.users) $$;

-- Creates the first user and household. Returns false if a user exists.
-- The advisory lock makes two parallel setup requests safe.
CREATE FUNCTION app.create_owner(p_user_id uuid, p_household_id uuid, p_display_name text)
    RETURNS boolean
    LANGUAGE plpgsql SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('app.create_owner'));
    IF EXISTS (SELECT 1 FROM app.users) THEN
        RETURN false;
    END IF;
    INSERT INTO app.households (id) VALUES (p_household_id);
    INSERT INTO app.users (id, display_name) VALUES (p_user_id, p_display_name);
    INSERT INTO app.household_members (household_id, user_id, role)
        VALUES (p_household_id, p_user_id, 'owner');
    RETURN true;
END
$$;

-- Finds a passkey by credential ID for sign-in.
CREATE FUNCTION app.find_passkey(p_credential_id bytea)
    RETURNS TABLE (id uuid, household_id uuid, user_id uuid, passkey jsonb)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
    SELECT p.id, p.household_id, p.user_id, p.passkey
    FROM app.passkeys p WHERE p.credential_id = p_credential_id
$$;

-- Finds a live session by token hash. Checks the idle (30 days) and absolute
-- limits. Updates last_seen_at at most once per hour to limit writes.
CREATE FUNCTION app.find_session(p_token_hash bytea)
    RETURNS TABLE (id uuid, household_id uuid, user_id uuid, verified_at timestamptz)
    LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
    UPDATE app.sessions s
    SET last_seen_at = CASE
            WHEN s.last_seen_at < now() - interval '1 hour' THEN now()
            ELSE s.last_seen_at
        END
    WHERE s.token_hash = p_token_hash
      AND s.expires_at > now()
      AND s.last_seen_at > now() - interval '30 days'
    RETURNING s.id, s.household_id, s.user_id, s.verified_at
$$;

-- Uses a recovery code. Marks it used in the same statement, so a code works
-- once even under parallel requests.
CREATE FUNCTION app.use_recovery_code(p_code_hash bytea)
    RETURNS TABLE (household_id uuid, user_id uuid)
    LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
    UPDATE app.recovery_codes r SET used_at = now()
    WHERE r.code_hash = p_code_hash AND r.used_at IS NULL
    RETURNING r.household_id, r.user_id
$$;

-- Deletes expired rows. The worker runs this each hour.
CREATE FUNCTION app.delete_expired() RETURNS bigint
    LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
DECLARE
    n bigint := 0;
    c bigint;
BEGIN
    DELETE FROM app.auth_challenges WHERE expires_at < now();
    GET DIAGNOSTICS c = ROW_COUNT; n := n + c;
    DELETE FROM app.sessions
        WHERE expires_at < now() OR last_seen_at < now() - interval '30 days';
    GET DIAGNOSTICS c = ROW_COUNT; n := n + c;
    DELETE FROM app.idempotency_keys WHERE created_at < now() - interval '24 hours';
    GET DIAGNOSTICS c = ROW_COUNT; n := n + c;
    RETURN n;
END
$$;

REVOKE ALL ON FUNCTION app.owner_exists() FROM PUBLIC;
REVOKE ALL ON FUNCTION app.create_owner(uuid, uuid, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION app.find_passkey(bytea) FROM PUBLIC;
REVOKE ALL ON FUNCTION app.find_session(bytea) FROM PUBLIC;
REVOKE ALL ON FUNCTION app.use_recovery_code(bytea) FROM PUBLIC;
REVOKE ALL ON FUNCTION app.delete_expired() FROM PUBLIC;

GRANT EXECUTE ON FUNCTION app.owner_exists() TO savespace_runtime;
GRANT EXECUTE ON FUNCTION app.create_owner(uuid, uuid, text) TO savespace_runtime;
GRANT EXECUTE ON FUNCTION app.find_passkey(bytea) TO savespace_runtime;
GRANT EXECUTE ON FUNCTION app.find_session(bytea) TO savespace_runtime;
GRANT EXECUTE ON FUNCTION app.use_recovery_code(bytea) TO savespace_runtime;
GRANT EXECUTE ON FUNCTION app.delete_expired() TO savespace_runtime;
