//! Configurable SMTP. Secrets and SMTP responses never enter diagnostics.
use application::{UseCaseError, account_links::AccountMailer};
use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType},
    transport::smtp::{
        authentication::Credentials,
        client::{Certificate, Tls, TlsParameters},
    },
};
use std::time::Duration;

pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub security: String,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Additional PEM trust anchors scoped to this SMTP transport only.
    pub ca_pem: Option<String>,
    pub from: String,
}
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}
impl SmtpMailer {
    pub fn new(config: &SmtpConfig) -> Result<Self, String> {
        if config.host.is_empty()
            || config
                .host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
            || config.port == 0
        {
            return Err("mail.host / mail.port 无效".into());
        }
        let from = config
            .from
            .parse::<Mailbox>()
            .map_err(|_| "mail.from 必须是有效发件地址")?;
        let mut builder = match config.security.as_str() {
            "starttls" => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
                .map_err(|_| "SMTP STARTTLS 配置无效")?,
            "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)
                .map_err(|_| "SMTP TLS 配置无效")?,
            // Explicit development-only relay. Remote cleartext is not supported.
            "local" if matches!(config.host.as_str(), "localhost" | "127.0.0.1" | "::1") => {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host)
            }
            _ => return Err("mail.security 须为 starttls、tls，或仅限回环地址的 local".into()),
        }
        .port(config.port)
        .timeout(Some(Duration::from_secs(10)));
        if let Some(pem) = &config.ca_pem {
            if config.security == "local"
                || pem.len() > 65_536
                || !pem.contains("-----BEGIN CERTIFICATE-----")
                || pem.contains("PRIVATE KEY")
            {
                return Err(
                    "mail.ca_pem 须为不超过 64 KiB 的 PEM CA 证书，且仅用于加密 SMTP".into(),
                );
            }
            let certificate =
                Certificate::from_pem(pem.as_bytes()).map_err(|_| "mail.ca_pem 证书无效")?;
            let tls = TlsParameters::builder(config.host.clone())
                .add_root_certificate(certificate)
                .build()
                .map_err(|_| "mail.ca_pem 证书无效")?;
            builder = builder.tls(if config.security == "tls" {
                Tls::Wrapper(tls)
            } else {
                Tls::Required(tls)
            });
        }
        match (&config.username, &config.password) {
            (Some(user), Some(password))
                if !user.is_empty() && !password.is_empty() && config.security != "local" =>
            {
                builder = builder.credentials(Credentials::new(user.clone(), password.clone()));
            }
            (None, None) => {}
            _ => return Err("SMTP 用户名与密码必须成对配置；local 模式不发送认证凭据".into()),
        }
        Ok(Self {
            transport: builder.build(),
            from,
        })
    }
}
#[async_trait]
impl AccountMailer for SmtpMailer {
    async fn send_link(
        &self,
        email: &str,
        url: &str,
        invitation: bool,
    ) -> Result<(), UseCaseError> {
        let failure = || UseCaseError::Repository("SMTP 邮件投递失败".into());
        let action = if invitation {
            "账号邀请：设置登录密码"
        } else {
            "重置登录密码"
        };
        let message = Message::builder().from(self.from.clone())
            .to(email.parse().map_err(|_| failure())?).subject(action)
            .header(ContentType::TEXT_PLAIN)
            .body(format!("{action}\n\n请在 30 分钟内打开以下链接设置密码：\n{url}\n\n链接仅可使用一次。修改后需要重新登录。如果并非你本人申请，可忽略此邮件。\n"))
            .map_err(|_| failure())?;
        tokio::time::timeout(Duration::from_secs(12), self.transport.send(message))
            .await
            .map_err(|_| failure())?
            .map_err(|_| failure())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_remote_cleartext_and_partial_credentials() {
        let mut config = SmtpConfig {
            host: "smtp.example.com".into(),
            port: 587,
            security: "local".into(),
            username: None,
            password: None,
            ca_pem: None,
            from: "Blog <blog@example.com>".into(),
        };
        assert!(SmtpMailer::new(&config).is_err());
        config.security = "starttls".into();
        assert!(SmtpMailer::new(&config).is_ok());
        config.password = Some("secret".into());
        assert!(SmtpMailer::new(&config).is_err());
    }
    #[test]
    fn rejects_invalid_or_cleartext_ca_configuration_without_echoing_pem() {
        let mut config = SmtpConfig {
            host: "localhost".into(),
            port: 587,
            security: "starttls".into(),
            username: None,
            password: None,
            ca_pem: None,
            from: "blog@example.com".into(),
        };
        for pem in [
            "private-invalid-input".to_owned(),
            "-----BEGIN CERTIFICATE-----\ninvalid\n-----END CERTIFICATE-----".to_owned(),
            "-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----".to_owned(),
            "-----BEGIN PRIVATE KEY-----\nprivate-key\n-----END PRIVATE KEY-----".to_owned(),
            "x".repeat(65_537),
        ] {
            config.ca_pem = Some(pem.clone());
            let error = SmtpMailer::new(&config).err().expect("invalid CA rejected");
            assert!(!error.contains(&pem));
        }
        config.security = "local".into();
        assert!(SmtpMailer::new(&config).is_err());
    }
    #[tokio::test]
    async fn delivers_invitation_over_explicit_local_smtp() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            writer.write_all(b"220 local test SMTP\r\n").await.unwrap();
            let mut data = String::new();
            let mut in_data = false;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                if in_data {
                    if line == ".\r\n" {
                        in_data = false;
                        writer.write_all(b"250 queued\r\n").await.unwrap();
                    } else {
                        data.push_str(&line);
                    }
                } else if line.starts_with("EHLO") {
                    writer
                        .write_all(b"250-local\r\n250 8BITMIME\r\n")
                        .await
                        .unwrap();
                } else if line.starts_with("MAIL FROM:") || line.starts_with("RCPT TO:") {
                    writer.write_all(b"250 OK\r\n").await.unwrap();
                } else if line.starts_with("DATA") {
                    in_data = true;
                    writer.write_all(b"354 continue\r\n").await.unwrap();
                } else if line.starts_with("QUIT") {
                    writer.write_all(b"221 bye\r\n").await.unwrap();
                    break;
                } else {
                    panic!("unexpected SMTP command");
                }
            }
            data
        });
        let sender = SmtpMailer::new(&SmtpConfig {
            host: "127.0.0.1".into(),
            port,
            security: "local".into(),
            username: None,
            password: None,
            ca_pem: None,
            from: "Blog <noreply@example.com>".into(),
        })
        .unwrap();
        sender
            .send_link(
                "member@example.com",
                "https://blog.example.com/admin/#password-reset=abc123",
                true,
            )
            .await
            .unwrap();
        let data = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        assert!(data.contains("To: member@example.com"));
        assert!(data.contains("noreply@example.com"));
        use base64::Engine;
        let body = data
            .split_once("\r\n\r\n")
            .unwrap()
            .1
            .replace(['\r', '\n'], "");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(body)
            .unwrap();
        assert!(
            String::from_utf8(decoded)
                .unwrap()
                .contains("https://blog.example.com/admin/#password-reset=abc123")
        );
    }
}
