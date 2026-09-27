//! A restored database stays guarded until the operator releases it.
pub async fn is_isolated(pool: &sqlx::PgPool) -> Result<bool, String> {
    let comment: Option<String> = sqlx::query_scalar(
        "SELECT shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()",
    ).fetch_one(pool).await.map_err(|e| format!("读取恢复隔离标记失败：{e}"))?;
    Ok(comment.is_some_and(|s| s.starts_with("blog:recovery-isolated:")))
}

pub fn mode() -> Result<bool, String> {
    match std::env::var("BLOG_RECOVERY_MODE").as_deref() {
        Err(_) | Ok("") | Ok("0") | Ok("false") => Ok(false),
        Ok("1") | Ok("true") => Ok(true),
        _ => Err("BLOG_RECOVERY_MODE 只能是 0/1 或 false/true".into()),
    }
}

pub fn check_bind(bind: &str) -> Result<(), String> {
    let address: std::net::SocketAddr = bind
        .parse()
        .map_err(|_| "恢复模式须使用 loopback IP 监听地址")?;
    if !address.ip().is_loopback() {
        return Err("恢复模式只允许 loopback 监听，不能开放到公网".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn recovery_binding_is_loopback_only() {
        for bind in ["127.0.0.1:8080", "[::1]:8080"] {
            assert!(super::check_bind(bind).is_ok());
        }
        for bind in [
            "0.0.0.0:8080",
            "[::]:8080",
            "example.com:8080",
            "192.0.2.1:8080",
        ] {
            assert!(super::check_bind(bind).is_err());
        }
    }
}
