-- 媒体库第二段：Post 与 Series 封面接入媒体库。
-- 源自 docs/content-lifecycle.md §5 与 docs/database-design.md §8。
--
-- 封面以前只是 posts.cover / series.cover 两个从未被应用的 text 列
-- （第一版媒体库只覆盖正文图片，见 0004_media.sql）。本段把它换成对
-- media_assets 的真实外键：数据库保证封面指向存在的资产，应用层则把
-- 「封面 + 正文」的并集写进 content_media_refs，让公开来源判定与删除保护
-- 对两者使用同一套规则，不再出现「文本里写着一个地址」的第二套判据。

-- 1. 引用来源类型增加 series。
--    系列目录页对**任何已存在系列**公开可达（不存在即 404），因此系列封面
--    在有系列行时即构成公开来源；这与公开系列页的可见性口径一致。
ALTER TABLE content_media_refs DROP CONSTRAINT content_media_refs_content_type_check;
ALTER TABLE content_media_refs ADD CONSTRAINT content_media_refs_content_type_check
    CHECK (content_type IN ('post', 'page', 'series'));

-- 2. 两个从未使用的 text 列替换为外键。
--    ON DELETE RESTRICT 与其他业务引用一致：媒体行按状态机保留、从不物理删除，
--    这里取 RESTRICT 只是拒绝任何绕过引用检查的删除路径。
ALTER TABLE posts DROP COLUMN cover;
ALTER TABLE posts ADD COLUMN cover_media_id uuid REFERENCES media_assets(id) ON DELETE RESTRICT;
ALTER TABLE series DROP COLUMN cover;
ALTER TABLE series ADD COLUMN cover_media_id uuid REFERENCES media_assets(id) ON DELETE RESTRICT;

-- 3. 封面反查（哪些内容把这张图当作封面）与 FK 校验走部分索引。
CREATE INDEX posts_cover_media ON posts(cover_media_id) WHERE cover_media_id IS NOT NULL;
CREATE INDEX series_cover_media ON series(cover_media_id) WHERE cover_media_id IS NOT NULL;
