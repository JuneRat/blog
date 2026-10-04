//! 登录能力只取决于已登记的凭据事实，不持有密码哈希或外部身份明细。

#[derive(Debug, Clone, Copy)]
pub enum LoginMethod {
    Password,
    ExternalIdentity,
}

#[derive(Debug, Clone, Copy)]
pub struct LoginMethods {
    pub password_enabled: bool,
    pub external_identities: i64,
}

impl LoginMethods {
    pub fn can_login(self) -> bool {
        self.password_enabled || self.external_identities > 0
    }

    /// 已不存在的方式可幂等移除；实际移除须至少保留一种登录方式。
    /// ExternalIdentity 表示其中一条绑定，具体绑定是否存在由调用方确认。
    pub fn can_remove(self, method: LoginMethod) -> bool {
        match method {
            LoginMethod::Password => !self.password_enabled || self.external_identities > 0,
            LoginMethod::ExternalIdentity => {
                self.external_identities == 0
                    || self.password_enabled
                    || self.external_identities > 1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_a_method_preserves_login_or_is_an_idempotent_noop() {
        for (password, external, can_login, clear_password, unbind) in [
            (false, 0, false, true, true),
            (true, 0, true, false, true),
            (false, 1, true, true, false),
            (true, 1, true, true, true),
            (false, 2, true, true, true),
            (true, 2, true, true, true),
        ] {
            let methods = LoginMethods {
                password_enabled: password,
                external_identities: external,
            };
            assert_eq!(methods.can_login(), can_login);
            assert_eq!(methods.can_remove(LoginMethod::Password), clear_password);
            assert_eq!(methods.can_remove(LoginMethod::ExternalIdentity), unbind);
        }
    }
}
