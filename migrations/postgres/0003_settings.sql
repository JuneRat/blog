-- 系统：settings 按 key 分组的 JSONB 配置。
-- 源自 docs/sql/postgres-core.sql。

-- 13. 应用按 key 校验 JSON schema、权限、容量和秘密引用。
CREATE TABLE settings (
    key varchar(64) COLLATE "C" PRIMARY KEY CHECK (key <> ''),
    value jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(value) = 'object'),
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);
