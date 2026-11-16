-- Phase 1 M4: recurring series, monthly plans, rollover, and the household
-- currency (design doc §6.1, §9.2).

-- One currency per household in Phase 1. Budgets and the calm state use it.
ALTER TABLE app.households
    ADD COLUMN currency CHAR(3) NOT NULL DEFAULT 'USD';

-- Recurring series ------------------------------------------------------------
-- Found by domain-recurring from posted transactions: one merchant, one
-- direction, one currency.
CREATE TABLE app.recurring_series (
    id             UUID        PRIMARY KEY DEFAULT uuidv7(),
    household_id   UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    merchant_id    UUID        NOT NULL REFERENCES app.merchants (id) ON DELETE CASCADE,
    direction      TEXT        NOT NULL CHECK (direction IN ('in', 'out')),
    currency       CHAR(3)     NOT NULL,
    cadence        TEXT        NOT NULL
                   CHECK (cadence IN ('weekly', 'biweekly', 'monthly', 'annual')),
    -- The typical amount (median). Negative is money out.
    amount_minor   BIGINT      NOT NULL,
    -- How far amounts usually are from the typical amount (MAD).
    spread_minor   BIGINT      NOT NULL CHECK (spread_minor >= 0),
    last_date      DATE        NOT NULL,
    next_date      DATE        NOT NULL,
    occurrences    INTEGER     NOT NULL CHECK (occurrences >= 2),
    confidence_bp  INTEGER     NOT NULL CHECK (confidence_bp BETWEEN 0 AND 10000),
    -- False if the series missed its last expected date.
    active         BOOLEAN     NOT NULL,
    -- The most common category of its transactions.
    category_id    UUID        REFERENCES app.categories (id) ON DELETE SET NULL,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (household_id, merchant_id, direction, currency)
);

ALTER TABLE app.transactions
    ADD COLUMN recurring_series_id UUID
        REFERENCES app.recurring_series (id) ON DELETE SET NULL;

-- Plans -----------------------------------------------------------------------
-- A plan applies from its month until a later row changes it. `plan_minor`
-- NULL ends the plan from that month. For non-monthly categories the plan is
-- a yearly amount.
CREATE TABLE app.budgets (
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    category_id   UUID        NOT NULL REFERENCES app.categories (id) ON DELETE CASCADE,
    month         DATE        NOT NULL CHECK (extract(day FROM month) = 1),
    plan_minor    BIGINT      CHECK (plan_minor >= 0),
    currency      CHAR(3)     NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (category_id, month)
);
CREATE INDEX budgets_household_idx ON app.budgets (household_id, month);

-- Rollover: from this month on, what is left (or overspent) carries into the
-- next month. NULL is off.
ALTER TABLE app.categories
    ADD COLUMN rollover_from DATE CHECK (extract(day FROM rollover_from) = 1);

-- Row-level security -----------------------------------------------------------
ALTER TABLE app.recurring_series ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.budgets          ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant ON app.recurring_series
    USING (household_id = app.current_household_id());
CREATE POLICY tenant ON app.budgets
    USING (household_id = app.current_household_id());
