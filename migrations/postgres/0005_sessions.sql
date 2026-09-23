-- 持久会话：重启后仍登录（docs/identity-and-admin.md §5）。
--
-- 服务端只保存令牌的 SHA-256 摘要（小写 hex），明文令牌仅在签发时返回一次：
-- 浏览器 cookie 里的明文无法从库里还原，库泄露也不能直接当 cookie 使用。
-- 过期语义与内存实现（InMemorySessionStore）逐字对齐：
--   * 有效 ⇔ expires_at >= now 且 last_seen_at >= now - 空闲 TTL；
--   * 绝对过期 expires_at = created_at + 绝对 TTL，签发时写入后不再改动；
--   * 每次校验刷新 last_seen_at（空闲窗口内活动续期，但不可逾越绝对上限）。
-- 容量上限（max_entries）由应用层在创建会话时用事务级 advisory lock 保证，
-- 不是表约束：它需要「清理 + 淘汰 + 插入」的跨行原子性。

CREATE TABLE sessions (
    -- 令牌摘要即查找键：validate/revoke 都是主键单点命中。
    token_hash text COLLATE "C" PRIMARY KEY
        CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    -- 会话归属；用户被硬删除时随之清理（会话是运行态，不构成业务引用）。
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- CSRF token 对同源 JS 可见（前端读取后回填请求头），不是秘密，故存明文。
    csrf_token text COLLATE "C" NOT NULL
        CHECK (csrf_token ~ '^[0-9a-f]{64}$'),
    -- 签发时的 users.version；校验时与应用层重读的当前版本比对，
    -- 使改密/改角色/软删除（含另一个进程发起的）立即失效旧会话。
    user_version bigint NOT NULL CHECK (user_version > 0),
    created_at timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    CHECK (last_seen_at >= created_at),
    CHECK (expires_at > created_at)
);

-- 撤销某用户全部会话（改密、重置、软删除、撤权）。
CREATE INDEX sessions_by_user ON sessions(user_id);
-- 过期清理：绝对过期扫描。
CREATE INDEX sessions_by_expires ON sessions(expires_at);
-- 空闲过期清理与容量淘汰（按最久未活跃排序）。
CREATE INDEX sessions_by_last_seen ON sessions(last_seen_at);
