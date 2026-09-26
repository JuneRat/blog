-- PostgreSQL 当前结构参考：16 张业务表，不含 sqlx 迁移记录表。
-- 汇总 migrations/postgres/0001 至 0008，供整体阅读与空 schema 参考。
-- 实际建库和升级使用 migrations/postgres/ 中的迁移，不直接执行本文件替代迁移。
-- UUID 由应用生成，不含种子账号；version 用于并发控制，不表示修订历史。
-- 数据关系与约束说明见 docs/database-design.md。
BEGIN;

-- 1. 本站身份；password_hash 可空，存 Argon2id 的 PHC 字符串（见 ADR-0009）。
CREATE TABLE users (
    id uuid PRIMARY KEY,
    username varchar(64) COLLATE "C" NOT NULL UNIQUE CHECK (username <> ''),
    email varchar(320) COLLATE "C" UNIQUE,
    password_hash text,
    display_name varchar(100),
    -- avatar_media_id 由 0007_media_avatar_logo.sql 在 media_assets 建表后追加（见文末）。
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

-- 7. 一篇文章至多一个分类；树的多节点环由应用事务检查。
CREATE TABLE categories (
    id uuid PRIMARY KEY,
    name varchar(100) NOT NULL CHECK (name <> ''),
    slug varchar(200) COLLATE "C" NOT NULL UNIQUE
        CHECK (octet_length(slug) BETWEEN 1 AND 200),
    parent_id uuid REFERENCES categories(id) ON DELETE RESTRICT,
    description text,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (parent_id IS NULL OR parent_id <> id)
);
CREATE INDEX categories_by_parent ON categories(parent_id);

-- 8. 系列是文章的有序集合，不是分类树。
CREATE TABLE series (
    id uuid PRIMARY KEY,
    name varchar(200) NOT NULL CHECK (name <> ''),
    slug varchar(200) COLLATE "C" NOT NULL UNIQUE
        CHECK (octet_length(slug) BETWEEN 1 AND 200),
    description text,
    -- cover_media_id 由 0006_media_covers.sql 在 media_assets 建表后追加（见文末）。
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- 9. 只存一份当前正文；保存已发布文章会直接更新线上内容。
CREATE TABLE posts (
    id uuid PRIMARY KEY,
    author_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    category_id uuid REFERENCES categories(id) ON DELETE RESTRICT,
    series_id uuid REFERENCES series(id) ON DELETE RESTRICT,
    title varchar(300) NOT NULL DEFAULT '',
    slug varchar(200) COLLATE "C" NOT NULL UNIQUE
        CHECK (octet_length(slug) BETWEEN 1 AND 200),
    excerpt text CHECK (char_length(excerpt) <= 1000),
    content text NOT NULL DEFAULT '',
    content_html text NOT NULL DEFAULT '',
    content_render_version integer NOT NULL DEFAULT 0 CHECK (content_render_version >= 0),
    content_type varchar(16) NOT NULL DEFAULT 'markdown' CHECK (content_type = 'markdown'),
    -- cover_media_id 由 0006_media_covers.sql 在 media_assets 建表后追加（见文末）。
    series_order integer,
    status varchar(16) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'published', 'archived')),
    visibility varchar(16) NOT NULL DEFAULT 'public'
        CHECK (visibility IN ('public', 'private')),
    published_at timestamptz,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    CHECK (status <> 'published' OR published_at IS NOT NULL),
    CHECK ((series_id IS NULL) = (series_order IS NULL)),
    CHECK (series_order IS NULL OR series_order > 0),
    CONSTRAINT posts_series_position_unique UNIQUE (series_id, series_order)
        DEFERRABLE INITIALLY IMMEDIATE
);
CREATE INDEX posts_public_list ON posts(published_at DESC, id DESC)
    WHERE status = 'published' AND visibility = 'public' AND deleted_at IS NULL;
CREATE INDEX posts_by_author ON posts(author_id, updated_at DESC, id DESC);
CREATE INDEX posts_by_category ON posts(category_id);
CREATE INDEX posts_trash ON posts(deleted_at) WHERE deleted_at IS NOT NULL;
-- 系列查询复用 (series_id, series_order) 的唯一约束索引，无需重复建索引。

-- 10–11. 多标签；删除文章可删除其关系，删除被引用标签默认拒绝。
CREATE TABLE tags (
    id uuid PRIMARY KEY,
    name varchar(100) NOT NULL CHECK (name <> ''),
    slug varchar(200) COLLATE "C" NOT NULL UNIQUE
        CHECK (octet_length(slug) BETWEEN 1 AND 200),
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE post_tags (
    post_id uuid NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    tag_id uuid NOT NULL REFERENCES tags(id) ON DELETE RESTRICT,
    PRIMARY KEY (post_id, tag_id)
);
CREATE INDEX post_tags_by_tag ON post_tags(tag_id, post_id);

-- 12. 独立页面无作者、分类、标签或系列；权限按页面动作的站点范围判断。
CREATE TABLE pages (
    id uuid PRIMARY KEY,
    title varchar(300) NOT NULL DEFAULT '',
    slug varchar(200) COLLATE "C" NOT NULL UNIQUE
        CHECK (octet_length(slug) BETWEEN 1 AND 200),
    content text NOT NULL DEFAULT '',
    content_html text NOT NULL DEFAULT '',
    content_render_version integer NOT NULL DEFAULT 0 CHECK (content_render_version >= 0),
    content_type varchar(16) NOT NULL DEFAULT 'markdown' CHECK (content_type = 'markdown'),
    status varchar(16) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'published', 'archived')),
    visibility varchar(16) NOT NULL DEFAULT 'public'
        CHECK (visibility IN ('public', 'private')),
    published_at timestamptz,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (status <> 'published' OR published_at IS NOT NULL)
);
CREATE INDEX pages_public_list ON pages(published_at DESC, id DESC)
    WHERE status = 'published' AND visibility = 'public';

-- 13. 配置分组；应用按 key 校验 JSON schema、权限、容量和秘密引用。
CREATE TABLE settings (
    key varchar(64) COLLATE "C" PRIMARY KEY CHECK (key <> ''),
    value jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(value) = 'object'),
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- 14. 媒体资产元数据（第一段服务 Post/Page 正文图片与 Post/Series 封面）。
-- 文件用随机标识存储，本行是唯一权威；状态机让跨系统删除可幂等重试。
CREATE TABLE media_assets (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    storage_key varchar(200) COLLATE "C" NOT NULL UNIQUE
        CHECK (octet_length(storage_key) BETWEEN 1 AND 200),
    original_name varchar(200) NOT NULL CHECK (original_name <> ''),
    mime varchar(64) COLLATE "C" NOT NULL
        CHECK (mime IN ('image/png', 'image/jpeg', 'image/gif', 'image/webp')),
    byte_size bigint NOT NULL CHECK (byte_size > 0),
    width integer NOT NULL CHECK (width > 0),
    height integer NOT NULL CHECK (height > 0),
    checksum_sha256 varchar(64) COLLATE "C" NOT NULL
        CHECK (checksum_sha256 ~ '^[0-9a-f]{64}$'),
    status varchar(24) COLLATE "C" NOT NULL DEFAULT 'staged'
        CHECK (status IN ('staged', 'ready', 'pending_deletion', 'deleted')),
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX media_assets_ready ON media_assets(created_at DESC, id DESC)
    WHERE status = 'ready';
CREATE INDEX media_assets_by_owner ON media_assets(owner_id, created_at DESC, id DESC);
CREATE INDEX media_assets_by_status ON media_assets(status) WHERE status <> 'ready';

-- 15. 内容 → 媒体的真实引用关系；删除保护的唯一判据。
-- content_id 是指向 posts/pages/series/users 的多态引用（无法建 FK），
-- 内容物理删除必须在同一事务清理对应行；content_type='site' 用固定 nil UUID 占位。
CREATE TABLE content_media_refs (
    media_id uuid NOT NULL REFERENCES media_assets(id) ON DELETE RESTRICT,
    content_type varchar(16) COLLATE "C" NOT NULL
        CHECK (content_type IN ('post', 'page', 'series', 'user', 'site')),
    content_id uuid NOT NULL,
    PRIMARY KEY (media_id, content_type, content_id)
);
CREATE INDEX content_media_refs_by_content ON content_media_refs(content_type, content_id);

-- 封面外键：由 0006_media_covers.sql 追加（media_assets 之后才能建 FK）。
-- 封面与正文图片共用 content_media_refs，替换/移除封面会在同一事务释放旧引用。
ALTER TABLE posts ADD COLUMN cover_media_id uuid REFERENCES media_assets(id) ON DELETE RESTRICT;
ALTER TABLE series ADD COLUMN cover_media_id uuid REFERENCES media_assets(id) ON DELETE RESTRICT;
CREATE INDEX posts_cover_media ON posts(cover_media_id) WHERE cover_media_id IS NOT NULL;
CREATE INDEX series_cover_media ON series(cover_media_id) WHERE cover_media_id IS NOT NULL;

-- 头像外键：由 0007_media_avatar_logo.sql 追加（同 0006 的原因）。
-- 站点 logo 的 id 存在 settings.site 的值里，引用行仍写 content_media_refs(content_type='site')。
ALTER TABLE users ADD COLUMN avatar_media_id uuid REFERENCES media_assets(id) ON DELETE RESTRICT;
CREATE INDEX users_avatar_media ON users(avatar_media_id) WHERE avatar_media_id IS NOT NULL;

-- 16. 持久会话；服务端只存令牌的 SHA-256 摘要，明文仅在签发时返回一次。
-- 随持久会话交付（migrations/postgres/0005_sessions.sql），此处同步维护。
-- 时间戳由应用写入（不使用 DEFAULT now()），过期语义：
-- 有效 ⇔ expires_at >= now 且 last_seen_at >= now - 空闲 TTL。
CREATE TABLE sessions (
    token_hash text COLLATE "C" PRIMARY KEY
        CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf_token text COLLATE "C" NOT NULL
        CHECK (csrf_token ~ '^[0-9a-f]{64}$'),
    user_version bigint NOT NULL CHECK (user_version > 0),
    created_at timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    CHECK (last_seen_at >= created_at),
    CHECK (expires_at > created_at)
);
CREATE INDEX sessions_by_user ON sessions(user_id);
CREATE INDEX sessions_by_expires ON sessions(expires_at);
CREATE INDEX sessions_by_last_seen ON sessions(last_seen_at);

COMMIT;
