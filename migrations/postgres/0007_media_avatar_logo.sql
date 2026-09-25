-- 媒体库第三段：用户头像与站点 logo。
-- 源自 docs/content-lifecycle.md §5 与 docs/database-design.md §8。
--
-- 头像：users.avatar（从未被应用的 text URL）→ avatar_media_id 真外键；
--       公开来源 = 账号未软删除（用户确认的规则）。
-- 站点 logo：按用户确认的取舍，id 存在 settings.site 的 JSONB 值里，但引用关系
--       仍写入 content_media_refs（同事务）——删除保护与公开来源继续以引用表为
--       唯一判据。JSON 里的 id 没有 FK 兜底，因此读取侧对「指向不可用资产」的
--       logo 按无 logo 处理，写入则由引用表的 ready 校验把关。

-- 1. 引用来源类型增加 user / site。
ALTER TABLE content_media_refs DROP CONSTRAINT content_media_refs_content_type_check;
ALTER TABLE content_media_refs ADD CONSTRAINT content_media_refs_content_type_check
    CHECK (content_type IN ('post', 'page', 'series', 'user', 'site'));

-- 2. 头像换成真外键。
ALTER TABLE users DROP COLUMN avatar;
ALTER TABLE users ADD COLUMN avatar_media_id uuid REFERENCES media_assets(id) ON DELETE RESTRICT;

-- 3. 头像反查（哪些用户把这张图当作头像）与 FK 校验走部分索引。
CREATE INDEX users_avatar_media ON users(avatar_media_id) WHERE avatar_media_id IS NOT NULL;
