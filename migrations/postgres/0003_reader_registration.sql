-- Preserve existing accounts and grants while retiring the owner role.
DO $$
DECLARE old_role uuid; admin_role uuid;
BEGIN
    SELECT id INTO old_role FROM roles WHERE code = 'owner';
    SELECT id INTO admin_role FROM roles WHERE code = 'admin';
    IF old_role IS NOT NULL THEN
        IF admin_role IS NULL THEN
            UPDATE roles SET code = 'admin', name = 'Administrator', version = version + 1 WHERE id = old_role;
        ELSE
            INSERT INTO user_roles(user_id, role_id)
                SELECT user_id, admin_role FROM user_roles WHERE role_id = old_role
                ON CONFLICT DO NOTHING;
            INSERT INTO role_permissions(role_id, permission_code)
                SELECT admin_role, permission_code FROM role_permissions WHERE role_id = old_role
                ON CONFLICT DO NOTHING;
            DELETE FROM user_roles WHERE role_id = old_role;
            DELETE FROM role_permissions WHERE role_id = old_role;
            DELETE FROM roles WHERE id = old_role;
            UPDATE roles SET version = version + 1 WHERE id = admin_role;
        END IF;
    END IF;
END $$;
INSERT INTO permissions(code, name) SELECT 'admin.manage', '管理管理员'
    FROM permissions WHERE code = 'ownership.manage' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions(role_id, permission_code)
    SELECT role_id, 'admin.manage' FROM role_permissions WHERE permission_code = 'ownership.manage'
    ON CONFLICT DO NOTHING;
DELETE FROM permissions WHERE code = 'ownership.manage';
UPDATE settings SET value = (value - 'owner_id') || jsonb_build_object('admin_id', value->'owner_id'),
    version = version + 1 WHERE key = 'installation' AND value ? 'owner_id';
