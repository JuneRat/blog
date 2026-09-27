-- Run as the schema owner after `blog migrate`. Both LOGIN roles must already
-- exist and must not own schema objects or inherit a more privileged role.
-- psql "$DATABASE_URL" -v app_role=blog_app -v maintenance_role=blog_maintenance -f scripts/database-roles.sql
\set ON_ERROR_STOP on
BEGIN;
SELECT set_config('blog.app_role', :'app_role', true),
       set_config('blog.maintenance_role', :'maintenance_role', true);
DO $$
DECLARE name text; role_oid oid;
BEGIN
  IF current_setting('blog.app_role') = current_setting('blog.maintenance_role') THEN
    RAISE EXCEPTION 'Application and maintenance roles must be distinct';
  END IF;
  FOREACH name IN ARRAY ARRAY[current_setting('blog.app_role'),current_setting('blog.maintenance_role')] LOOP
    SELECT oid INTO role_oid FROM pg_roles WHERE rolname=name AND rolcanlogin
      AND NOT rolsuper AND NOT rolcreaterole AND NOT rolcreatedb AND NOT rolbypassrls;
    IF role_oid IS NULL THEN RAISE EXCEPTION 'Role % must be an unprivileged LOGIN role',name; END IF;
    IF EXISTS(SELECT 1 FROM pg_auth_members WHERE member=role_oid)
      OR EXISTS(SELECT 1 FROM pg_class WHERE relowner=role_oid)
      OR EXISTS(SELECT 1 FROM pg_namespace WHERE nspowner=role_oid)
      OR EXISTS(SELECT 1 FROM pg_database WHERE datdba=role_oid) THEN
      RAISE EXCEPTION 'Role % must not own objects or have role memberships',name;
    END IF;
  END LOOP;
END $$;
REVOKE ALL ON ALL TABLES IN SCHEMA public FROM :"app_role", :"maintenance_role";
REVOKE ALL ON SCHEMA public FROM :"app_role", :"maintenance_role";
GRANT USAGE ON SCHEMA public TO :"app_role", :"maintenance_role";
GRANT SELECT,INSERT,UPDATE,DELETE ON users,oauth_accounts,sessions,roles,permissions,
  user_roles,role_permissions,media,media_refs,posts,pages,categories,series,tags,
  post_tags,post_series,comments,settings TO :"app_role";
GRANT SELECT ON _sqlx_migrations TO :"app_role";
GRANT SELECT,INSERT ON audit_logs TO :"app_role";

GRANT SELECT ON settings TO :"maintenance_role";
GRANT SELECT(id,created_at,ip_address),UPDATE(ip_address) ON comments TO :"maintenance_role";
GRANT SELECT(id,created_at),INSERT,DELETE ON audit_logs TO :"maintenance_role";

-- Detect PUBLIC/inherited grants instead of silently claiming an append-only role.
DO $$
DECLARE app text := current_setting('blog.app_role'); maint text := current_setting('blog.maintenance_role');
BEGIN
  IF has_table_privilege(app,'audit_logs','UPDATE,DELETE,TRUNCATE')
    OR has_any_column_privilege(app,'audit_logs','UPDATE')
    OR has_schema_privilege(app,'public','CREATE')
    OR has_table_privilege(maint,'audit_logs','UPDATE,TRUNCATE')
    OR has_any_column_privilege(maint,'audit_logs','UPDATE')
    OR has_column_privilege(maint,'comments','content','UPDATE')
    OR has_schema_privilege(maint,'public','CREATE') THEN
    RAISE EXCEPTION 'Unexpected privileges remain; inspect PUBLIC and column grants';
  END IF;
END $$;
COMMIT;
