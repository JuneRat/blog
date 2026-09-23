-- 媒体：media_assets、content_media_refs。
-- 第一版只服务 Post/Page 正文图片；封面、头像与站点 logo 走后续分段。
-- 源自 docs/content-lifecycle.md §5 与 docs/database-design.md §7。

-- 14. 媒体资产元数据。
-- 文件本身用随机标识存储（storage_key），本行是唯一权威：上传者、类型、尺寸、
-- 校验值与状态都在这里。状态机 staged → ready → pending_deletion → deleted，
-- 使「文件删除」与「数据库更新」跨系统操作可以幂等重试。
-- 行在 deleted 后保留：重放回收、审计与「同一 id 永不复用」都依赖它。
CREATE TABLE media_assets (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    storage_key varchar(200) COLLATE "C" NOT NULL UNIQUE
        CHECK (octet_length(storage_key) BETWEEN 1 AND 200),
    original_name varchar(200) NOT NULL CHECK (original_name <> ''),
    -- 只允许位图；具体取值由 domain::media::ImageFormat 决定，不接受任意 MIME。
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

-- 媒体库按时间倒序分页，只列可用资产：部分索引与查询谓词一致。
CREATE INDEX media_assets_ready ON media_assets(created_at DESC, id DESC)
    WHERE status = 'ready';
CREATE INDEX media_assets_by_owner ON media_assets(owner_id, created_at DESC, id DESC);
-- 回收流程按状态扫描（staged 补偿、pending_deletion 重试）。
CREATE INDEX media_assets_by_status ON media_assets(status) WHERE status <> 'ready';

-- 15. 内容 → 媒体的真实引用关系。
-- 保存 Post/Page 时由服务端解析正文得到的引用集合在同一事务整体替换；
-- 「是否仍被使用」以本表为唯一判据，不以搜索 Markdown 文本为依据。
-- content_id 指向 posts 或 pages，是刻意保留的多态引用（无法建 FK）：
-- 内容物理删除（post.purge / page.delete）必须在同一事务清理对应行。
CREATE TABLE content_media_refs (
    media_id uuid NOT NULL REFERENCES media_assets(id) ON DELETE RESTRICT,
    content_type varchar(16) COLLATE "C" NOT NULL
        CHECK (content_type IN ('post', 'page')),
    content_id uuid NOT NULL,
    PRIMARY KEY (media_id, content_type, content_id)
);

-- 公开可见性判定、使用位置列表与内容侧引用替换都从内容反查。
CREATE INDEX content_media_refs_by_content ON content_media_refs(content_type, content_id);
