-- Phase 1 M7: the owner's insight correctness review (design doc §16, Phase 1
-- exit: correctness of at least 99.5% over 4 weeks).

-- One verdict per insight. "This is wrong" feedback without a review counts
-- as wrong; a review overrides it.
CREATE TABLE app.insight_reviews (
    insight_id    UUID        PRIMARY KEY REFERENCES app.insights (id) ON DELETE CASCADE,
    household_id  UUID        NOT NULL REFERENCES app.households (id) ON DELETE CASCADE,
    verdict       TEXT        NOT NULL CHECK (verdict IN ('correct', 'wrong')),
    reviewed_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX insight_reviews_household_idx ON app.insight_reviews (household_id);

ALTER TABLE app.insight_reviews ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant ON app.insight_reviews
    USING (household_id = app.current_household_id());

CREATE INDEX insights_created_idx ON app.insights (household_id, created_at);
