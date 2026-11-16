-- Phase 1 M3: categories, merchants, person edits, splits, and
-- categorization rules (design doc §9.2, §10.2 Enrich, D14).

-- Categories ------------------------------------------------------------------
-- One set per household. `kind` drives the budget buckets (§6.1, M4):
-- fixed, flex, and non_monthly. Income and transfers are not spending.
CREATE TABLE app.categories (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    -- A stable name for seeded categories. NULL for categories the person adds.
    key           TEXT,
    name          TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 60),
    kind          TEXT        NOT NULL
                  CHECK (kind IN ('fixed', 'flex', 'non_monthly', 'income', 'transfer')),
    -- Health, giving, and similar. Never sent to an LLM (§7.5).
    sensitive     BOOLEAN     NOT NULL DEFAULT false,
    position      INTEGER     NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (household_id, key),
    UNIQUE (household_id, name)
);

-- The seed set. A new household gets it from the trigger below.
CREATE FUNCTION app.seed_categories(p_household_id uuid)
    RETURNS void
    LANGUAGE sql SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
    INSERT INTO app.categories (household_id, key, name, kind, sensitive, position)
    SELECT p_household_id, s.key, s.name, s.kind, s.sensitive, s.position
    FROM (VALUES
        ('income',         'Income',                  'income',      false, 10),
        ('housing',        'Housing',                 'fixed',       false, 20),
        ('utilities',      'Utilities',               'fixed',       false, 30),
        ('phone_internet', 'Phone and internet',      'fixed',       false, 40),
        ('insurance',      'Insurance',               'fixed',       false, 50),
        ('loan_payments',  'Loan payments',           'fixed',       false, 60),
        ('childcare',      'Childcare',               'fixed',       false, 70),
        ('subscriptions',  'Subscriptions',           'fixed',       false, 80),
        ('groceries',      'Groceries',               'flex',        false, 90),
        ('dining',         'Restaurants and coffee',  'flex',        false, 100),
        ('transport',      'Transport',               'flex',        false, 110),
        ('shopping',       'Shopping',                'flex',        false, 120),
        ('home',           'Home',                    'flex',        false, 130),
        ('entertainment',  'Entertainment',           'flex',        false, 140),
        ('personal_care',  'Personal care',           'flex',        false, 150),
        ('health',         'Health',                  'flex',        true,  160),
        ('giving',         'Gifts and giving',        'flex',        true,  170),
        ('fees',           'Fees',                    'flex',        false, 180),
        ('other',          'Other',                   'flex',        false, 190),
        ('travel',         'Travel',                  'non_monthly', false, 200),
        ('education',      'Education',               'non_monthly', false, 210),
        ('taxes',          'Taxes',                   'non_monthly', false, 220),
        ('transfer',       'Transfers',               'transfer',    false, 230)
    ) AS s(key, name, kind, sensitive, position)
    ON CONFLICT DO NOTHING
$$;
REVOKE ALL ON FUNCTION app.seed_categories(uuid) FROM PUBLIC;

CREATE FUNCTION app.households_seed_categories()
    RETURNS trigger
    LANGUAGE plpgsql SECURITY DEFINER SET search_path = app, pg_temp
    AS $$
BEGIN
    PERFORM app.seed_categories(NEW.id);
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION app.households_seed_categories() FROM PUBLIC;

CREATE TRIGGER households_seed_categories
    AFTER INSERT ON app.households
    FOR EACH ROW EXECUTE FUNCTION app.households_seed_categories();

SELECT app.seed_categories(id) FROM app.households;

-- Merchants -------------------------------------------------------------------
-- Normalized merchants, per household. `key` is the normalized form
-- (domain-categorize::normalize_merchant). `name` is for display.
CREATE TABLE app.merchants (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    key           TEXT        NOT NULL CHECK (length(key) BETWEEN 1 AND 200),
    name          TEXT        NOT NULL,
    -- True if the key comes from a provider merchant name. A description
    -- that starts with such a key joins this merchant.
    from_provider BOOLEAN     NOT NULL DEFAULT false,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (household_id, key)
);

-- Categorization rules (D14) --------------------------------------------------
-- "Always set Trader Joe's to Groceries". `match` is JSON:
--   {"merchant_id": "<uuid>"} or {"name_contains": "<text>"},
--   with optional "direction": "in" | "out".
-- The rule with the highest priority wins. A new rule gets the highest.
CREATE TABLE app.rules (
    id            UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    priority      INTEGER     NOT NULL,
    match         JSONB       NOT NULL CHECK (jsonb_typeof(match) = 'object'),
    category_id   UUID        NOT NULL REFERENCES app.categories (id) ON DELETE CASCADE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX rules_household_idx ON app.rules (household_id, priority DESC);

-- Enrichment on transactions --------------------------------------------------
-- The automatic result. A person's edit (user_overrides) wins over it.
ALTER TABLE app.transactions
    ADD COLUMN merchant_id       UUID REFERENCES app.merchants (id) ON DELETE SET NULL,
    ADD COLUMN category_id       UUID REFERENCES app.categories (id) ON DELETE SET NULL,
    -- 'rule' or 'provider'. NULL if no category was found.
    ADD COLUMN category_source   TEXT CHECK (category_source IN ('rule', 'provider')),
    ADD COLUMN category_rule_id  UUID REFERENCES app.rules (id) ON DELETE SET NULL,
    -- The raw_version that was enriched. Enrichment runs when it differs.
    ADD COLUMN enriched_version  INTEGER;

CREATE INDEX transactions_enrich_idx ON app.transactions (household_id)
    WHERE enriched_version IS NULL;

-- Person edits (§9.1): an overlay, never a change to provider data -------------
CREATE TABLE app.user_overrides (
    transaction_id  UUID        PRIMARY KEY REFERENCES app.transactions (id) ON DELETE CASCADE,
    household_id    UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    category_id     UUID        REFERENCES app.categories (id) ON DELETE SET NULL,
    merchant_name   TEXT        CHECK (length(merchant_name) BETWEEN 1 AND 100),
    note            TEXT        CHECK (length(note) BETWEEN 1 AND 500),
    hidden          BOOLEAN     NOT NULL DEFAULT false,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Splits: one transaction across categories. The parts add up to the amount
-- (checked by domain-ledger). Posted transactions only.
CREATE TABLE app.transaction_splits (
    id              UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id    UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    transaction_id  UUID        NOT NULL REFERENCES app.transactions (id) ON DELETE CASCADE,
    category_id     UUID        NOT NULL REFERENCES app.categories (id) ON DELETE RESTRICT,
    amount_minor    BIGINT      NOT NULL CHECK (amount_minor <> 0),
    position        INTEGER     NOT NULL,
    UNIQUE (transaction_id, position)
);

-- Row-level security -----------------------------------------------------------
ALTER TABLE app.categories         ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.merchants          ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.rules              ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.user_overrides     ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.transaction_splits ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant ON app.categories
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.merchants
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.rules
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.user_overrides
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.transaction_splits
    USING (household_id = app.current_household_id());
