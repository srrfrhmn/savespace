-- Least-privilege role for the api and worker (design doc §12).
--
-- The migration role owns the schema. Row-level security does not apply to a
-- table owner, so the services must connect as a different role.
--
-- savespace_runtime has no login. An operator creates a login role that is a
-- member of it, with a password that is never in a migration:
--   CREATE ROLE savespace_app LOGIN PASSWORD '...' IN ROLE savespace_runtime;
-- See infra/README.md.

-- Roles are global to the cluster. Tests create many databases in parallel,
-- so ignore the error when another migration run created the role first.
DO $$
BEGIN
    CREATE ROLE savespace_runtime NOLOGIN;
EXCEPTION
    WHEN duplicate_object OR unique_violation THEN NULL;
END
$$;

GRANT USAGE ON SCHEMA app TO savespace_runtime;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA app TO savespace_runtime;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA app TO savespace_runtime;

-- Tables and sequences that later migrations create get the same grants.
ALTER DEFAULT PRIVILEGES IN SCHEMA app
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO savespace_runtime;
ALTER DEFAULT PRIVILEGES IN SCHEMA app
    GRANT USAGE, SELECT ON SEQUENCES TO savespace_runtime;
