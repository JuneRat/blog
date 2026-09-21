-- 身份与 RBAC：users、oauth_accounts、roles、permissions、user_roles、role_permissions。
-- 源自 docs/sql/postgres-core.sql；UUID 由应用生成，不含种子数据。
-- sqlx 会在单条迁移内自动包事务，故不写 BEGIN/COMMIT。

-- 1. 本站身份；password_hash 可空，不代表首版开放密码登录。
CREATE TABLE users (
    id uuid PRIMARY KEY,
    username varchar(64) COLLATE "C" NOT NULL UNIQUE CHECK (username <> ''),
    email varchar(320) COLLATE "C" UNIQUE,
    password_hash text,
    display_name varchar(100),
    avatar text,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);

-- 2. provider 必须标识固定提供商实例；OIDC 绑定精确 issuer，不接受用户指定。
CREATE TABLE oauth_accounts (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    provider varchar(512) COLLATE "C" NOT NULL
        CHECK (octet_length(provider) BETWEEN 1 AND 512),
    provider_user_id varchar(512) COLLATE "C" NOT NULL
        CHECK (octet_length(provider_user_id) BETWEEN 1 AND 512),
    email varchar(320),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider, provider_user_id)
);
CREATE INDEX oauth_accounts_by_user ON oauth_accounts(user_id);

-- 3. 角色可自定义；owner 等内置 slug 由应用保护，普通 API 不可伪造或改名。
CREATE TABLE roles (
    id uuid PRIMARY KEY,
    name varchar(100) NOT NULL CHECK (name <> ''),
    slug varchar(64) COLLATE "C" NOT NULL UNIQUE CHECK (slug <> ''),
    description text,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- 4. 权限目录由可信应用注册并同步；用户自定义角色只能选择已注册权限。
CREATE TABLE permissions (
    id uuid PRIMARY KEY,
    name varchar(100) NOT NULL CHECK (name <> ''),
    key varchar(100) COLLATE "C" NOT NULL UNIQUE CHECK (key <> ''),
    description text
);

-- 5. 用户与角色多对多；移除分配必须通过授权用例。
CREATE TABLE user_roles (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    role_id uuid NOT NULL REFERENCES roles(id) ON DELETE RESTRICT,
    PRIMARY KEY (user_id, role_id)
);
CREATE INDEX user_roles_by_role ON user_roles(role_id, user_id);

-- 6. 角色与权限多对多。
CREATE TABLE role_permissions (
    role_id uuid NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    permission_id uuid NOT NULL REFERENCES permissions(id) ON DELETE RESTRICT,
    PRIMARY KEY (role_id, permission_id)
);
CREATE INDEX role_permissions_by_permission ON role_permissions(permission_id, role_id);
