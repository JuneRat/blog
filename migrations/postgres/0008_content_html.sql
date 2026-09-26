-- Markdown 是编辑源文；HTML 是受信渲染器生成并清洗的持久化派生物。
-- 版本 0 表示待生成，Rust 迁移入口在对外服务前完成补齐。
ALTER TABLE posts
    ADD COLUMN content_html text NOT NULL DEFAULT '',
    ADD COLUMN content_render_version integer NOT NULL DEFAULT 0 CHECK (content_render_version >= 0);
ALTER TABLE pages
    ADD COLUMN content_html text NOT NULL DEFAULT '',
    ADD COLUMN content_render_version integer NOT NULL DEFAULT 0 CHECK (content_render_version >= 0);
