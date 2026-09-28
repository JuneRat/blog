//! A restored database stays guarded until the operator releases it.
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
