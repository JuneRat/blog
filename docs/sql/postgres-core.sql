-- PostgreSQL：用户确认的 13 张博客核心表。
-- 面向空 schema 的设计草案；UUID 由应用生成，不含种子数据或生产升级迁移。
-- 保留贴文中的业务字段；version 为并发编辑补充，不创建路径/修订/会话等辅助表。
BEGIN;

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
    cover text,
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
    content_type varchar(16) NOT NULL DEFAULT 'markdown' CHECK (content_type = 'markdown'),
    cover text,
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

COMMIT;
