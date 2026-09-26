//! 新密码的纯校验规则；哈希、凭据持久化与登录限流由外层负责。

/// 密码最短字符数。12 个字符显著抬高离线爆破成本，又不至于逼用户写便签。
pub const PASSWORD_MIN_CHARS: usize = 12;

/// 密码最长字符数。Argon2 的内存成本与输入长度无关，但首轮压缩随输入线性增长，
/// 不设上限就等于允许用超长输入放大单次校验的 CPU 开销；UTF-8 每字符至多 4 字节，
/// 该上限同时封顶了字节数。
pub const PASSWORD_MAX_CHARS: usize = 128;

/// 密码策略错误。文案面向操作者，不包含密码本身。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PasswordError {
    #[error("密码长度须在 {PASSWORD_MIN_CHARS}-{PASSWORD_MAX_CHARS} 个字符之间")]
    InvalidLength,
    #[error("密码不能包含用户名")]
    ContainsUsername,
    #[error("密码过于常见或过于规律，请更换更独特的密码")]
    TooCommon,
}

/// 常见/已泄露口令的离线兜底样例。
///
/// 只拦截最容易被字典命中的少数口令，不替代在线泄露库查询；
/// 列表全部为 ASCII，比较时忽略大小写。
pub const COMMON_PASSWORDS: &[&str] = &[
    "123456789012",
    "1234567890123",
    "12345678901234",
    "password1234",
    "password12345",
    "qwertyuiop12",
    "qwerty123456",
    "letmein12345",
    "iloveyou1234",
    "admin1234567",
    "administrator",
    "welcome12345",
    "changeme1234",
    "football1234",
    "baseball1234",
    "sunshine1234",
    "princess1234",
    "superman1234",
    "trustno1trustno1",
    "qazwsxedc123",
    "1qaz2wsx3edc",
    "aaaaaaaaaaaa",
    "abcabcabcabc",
    "000000000000",
    "111111111111",
];

/// 校验新设置的密码。`username` 必须是已规范化的用户名。
///
/// 不做 trim：首尾空格是合法密码字符，规范化会制造「输入与已存口令不一致」的陷阱。
/// 长度按字符数计（CJK、emoji 与 ASCII 同等对待）。
pub fn validate_password(password: &str, username: &str) -> Result<(), PasswordError> {
    let chars = password.chars().count();
    if !(PASSWORD_MIN_CHARS..=PASSWORD_MAX_CHARS).contains(&chars) {
        return Err(PasswordError::InvalidLength);
    }
    // 全部字符相同（"aaaaaaaaaaaa"）在长度达标后仍是最弱的一类。
    let mut unique = password.chars();
    if let Some(first) = unique.next()
        && unique.all(|c| c == first)
    {
        return Err(PasswordError::TooCommon);
    }
    if !username.is_empty()
        && username.chars().count() >= 3
        && password.to_lowercase().contains(&username.to_lowercase())
    {
        return Err(PasswordError::ContainsUsername);
    }
    if COMMON_PASSWORDS
        .iter()
        .any(|common| common.eq_ignore_ascii_case(password))
    {
        return Err(PasswordError::TooCommon);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_length_bounds_are_inclusive() {
        let short = "a".repeat(PASSWORD_MIN_CHARS - 1);
        assert_eq!(
            validate_password(&short, "sun"),
            Err(PasswordError::InvalidLength)
        );
        let ok = format!("{}aB3!", "x".repeat(PASSWORD_MIN_CHARS - 4));
        assert_eq!(ok.chars().count(), PASSWORD_MIN_CHARS);
        assert!(validate_password(&ok, "sun").is_ok());

        let too_long = "aB3!".repeat(PASSWORD_MAX_CHARS);
        assert_eq!(
            validate_password(&too_long, "sun"),
            Err(PasswordError::InvalidLength)
        );
    }

    #[test]
    fn password_must_not_contain_username() {
        // 大小写不敏感；短用户名不参与包含判断，避免误伤。
        assert_eq!(
            validate_password("Sunshine-Pass-2026", "sun"),
            Err(PasswordError::ContainsUsername)
        );
        assert!(validate_password("Sunshine-Pass-2026", "ab").is_ok());
    }

    #[test]
    fn password_rejects_common_and_repetitive_values() {
        assert_eq!(
            validate_password("password1234", "sun"),
            Err(PasswordError::TooCommon)
        );
        assert_eq!(
            validate_password("PASSWORD1234", "sun"),
            Err(PasswordError::TooCommon)
        );
        assert_eq!(
            validate_password("aaaaaaaaaaaaa", "sun"),
            Err(PasswordError::TooCommon)
        );
    }

    #[test]
    fn password_preserves_whitespace_and_accepts_unicode() {
        // 首尾空格是合法字符，不得被 trim 掉。
        assert!(validate_password("  correct horse battery  ", "sun").is_ok());
        assert!(validate_password("安静的书桌-2026-长密码", "sun").is_ok());
    }
}
