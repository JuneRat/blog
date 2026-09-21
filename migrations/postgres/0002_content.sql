-- 内容：categories、series、posts、tags、post_tags、pages。
-- 源自 docs/sql/postgres-core.sql。

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

-- 10–11. 多标签；删除文章可级联删除其关系，删除被引用标签默认拒绝。
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
