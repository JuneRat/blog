//! Umami collects pageviews in the visitor's browser. No credentials, events or
//! reports are stored by the blog, and no external request runs during rendering.
use super::*;
use application::plugins::{PluginConfigField, PluginConfigValue};

struct AnalyticsUmami;

fn text<'a>(config: &'a PluginConfig, key: &str) -> &'a str {
    match config.get(key) {
        Some(PluginConfigValue::Text(value)) => value.trim(),
        _ => "",
    }
}

impl PluginConfigValidator for AnalyticsUmami {
    fn validate(&self, config: &PluginConfig, enabled: bool) -> Result<(), UseCaseError> {
        let script = text(config, "script-url");
        let website = text(config, "website-id");
        if enabled && (script.is_empty() || website.is_empty()) {
            return Err(UseCaseError::Invalid(
                "启用 Umami 前请填写脚本地址和站点 ID".into(),
            ));
        }
        if !script.is_empty() && browser_url(script, "Umami 脚本地址")?.fragment().is_some() {
            return Err(UseCaseError::Invalid(
                "Umami 脚本地址不能包含 URL 片段".into(),
            ));
        }
        if !website.is_empty() {
            let id = uuid::Uuid::parse_str(website)
                .map_err(|_| UseCaseError::Invalid("Umami 站点 ID 须为有效 UUID".into()))?;
            if id.is_nil() {
                return Err(UseCaseError::Invalid(
                    "Umami 站点 ID 不能为全零 UUID".into(),
                ));
            }
        }
        let dashboard = text(config, "dashboard-url");
        if !dashboard.is_empty() {
            browser_url(dashboard, "Umami 报表地址")?;
        }
        Ok(())
    }
}

impl PageHeadHook for AnalyticsUmami {
    fn assets(&self, _: PluginPage, _: &PluginConfig) -> Result<Vec<HeadAsset>, UseCaseError> {
        Ok(vec![])
    }

    fn external_scripts(
        &self,
        page: PluginPage,
        config: &PluginConfig,
    ) -> Result<Vec<ExternalScript>, UseCaseError> {
        if page == PluginPage::Preview {
            return Ok(vec![]);
        }
        self.validate(config, true)?;
        let website = uuid::Uuid::parse_str(text(config, "website-id"))
            .map_err(|_| UseCaseError::Render("Umami 站点 ID 无效".into()))?;
        let dnt = !matches!(
            config.get("respect-dnt"),
            Some(PluginConfigValue::Boolean(false))
        );
        Ok(vec![ExternalScript {
            src: browser_url(text(config, "script-url"), "Umami 脚本地址")?.to_string(),
            data_attributes: BTreeMap::from([
                ("data-website-id".into(), website.to_string()),
                ("data-do-not-track".into(), dnt.to_string()),
                ("data-exclude-search".into(), "true".into()),
                ("data-exclude-hash".into(), "true".into()),
            ]),
        }])
    }
}

pub(super) fn registration() -> PluginRegistration {
    let hook = Arc::new(AnalyticsUmami);
    PluginRegistration {
        definition: PluginDefinition {
            id: "analytics-umami".into(),
            name: "Umami 访问统计".into(),
            description: "在公开页面接入 Umami 访问统计。请先配置脚本地址和站点 ID；可通过报表链接查看统计结果。".into(),
            version: "1.0.0".into(),
            hooks: vec![],
            config_fields: vec![
                PluginConfigField {
                    key: "script-url".into(), label: "Umami 脚本地址".into(),
                    description: "填写 Umami 跟踪代码中的完整 src 地址，例如 https://cloud.umami.is/script.js；自托管可填写自己的地址。".into(),
                    default: PluginConfigValue::Text(String::new()),
                },
                PluginConfigField {
                    key: "website-id".into(), label: "Umami 站点 ID".into(),
                    description: "填写 Umami 跟踪代码中的 data-website-id（UUID）。".into(),
                    default: PluginConfigValue::Text(String::new()),
                },
                PluginConfigField {
                    key: "dashboard-url".into(), label: "Umami 报表地址".into(),
                    description: "可选，填写站点报表或分享链接，保存后可从这里打开。请勿填写 API 密钥或登录凭据。".into(),
                    default: PluginConfigValue::Text(String::new()),
                },
                PluginConfigField {
                    key: "respect-dnt".into(), label: "尊重 Do Not Track".into(),
                    description: "访客开启浏览器 Do Not Track 时不采集访问数据。URL 查询参数和片段始终排除。".into(),
                    default: PluginConfigValue::Boolean(true),
                },
            ],
        },
        content: None,
        html_rules: vec![],
        page_head: Some(hook.clone()),
        config_validator: Some(hook),
        files: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::{
        audit::AuditContext,
        identity::Actor,
        plugins::{PluginSettings, PluginSettingsRecord, PluginStore, SavePluginCmd},
        ports::SaveOutcome,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Store(Mutex<PluginSettingsRecord>);

    #[async_trait]
    impl PluginStore for Store {
        async fn load(&self) -> Result<PluginSettingsRecord, UseCaseError> {
            Ok(self.0.lock().unwrap().clone())
        }
        async fn save(
            &self,
            value: &PluginSettings,
            expected: i64,
            _: &str,
            _: time::OffsetDateTime,
            _: AuditContext,
        ) -> Result<SaveOutcome, UseCaseError> {
            let mut state = self.0.lock().unwrap();
            if state.version != expected {
                return Ok(SaveOutcome::StaleConflict);
            }
            *state = PluginSettingsRecord {
                value: value.clone(),
                version: expected + 1,
            };
            Ok(SaveOutcome::Saved {
                new_version: expected + 1,
            })
        }
    }

    fn config() -> PluginConfig {
        BTreeMap::from([
            (
                "script-url".into(),
                PluginConfigValue::Text("https://stats.example.test/script.js?one=1&two=2".into()),
            ),
            (
                "website-id".into(),
                PluginConfigValue::Text("94db1cb1-74f4-4a40-ad6c-962362670409".into()),
            ),
        ])
    }

    #[test]
    fn public_pages_emit_one_deferred_tracker_with_privacy_defaults_and_no_report_url() {
        let catalog = PluginCatalog::builtins();
        let mut config = config();
        config.insert(
            "dashboard-url".into(),
            PluginConfigValue::Text("https://stats.example.test/websites/private-report".into()),
        );
        let snapshot = PluginSnapshot {
            active: BTreeMap::from([("analytics-umami".into(), config)]),
            ..Default::default()
        };
        for page in [
            PluginPage::Index,
            PluginPage::Post,
            PluginPage::Page,
            PluginPage::Tag,
            PluginPage::Category,
            PluginPage::Series,
        ] {
            let html = catalog.head_html(page, &snapshot).unwrap();
            assert_eq!(html.matches("<script ").count(), 1);
            assert!(
                html.contains("src=\"https://stats.example.test/script.js?one=1&amp;two=2\" defer")
            );
            assert!(html.contains("data-website-id=\"94db1cb1-74f4-4a40-ad6c-962362670409\""));
            for attr in [
                "data-do-not-track",
                "data-exclude-search",
                "data-exclude-hash",
            ] {
                assert!(html.contains(&format!("{attr}=\"true\"")));
            }
            assert!(!html.contains("private-report"));
        }
        assert!(
            catalog
                .head_html(PluginPage::Preview, &snapshot)
                .unwrap()
                .is_empty()
        );
        assert!(
            catalog
                .head_html(PluginPage::Index, &PluginSnapshot::default())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn invalid_urls_ids_and_incomplete_enabled_config_are_rejected() {
        assert!(AnalyticsUmami.validate(&PluginConfig::new(), false).is_ok());
        assert!(AnalyticsUmami.validate(&PluginConfig::new(), true).is_err());
        for key in ["script-url", "dashboard-url"] {
            for bad in [
                "javascript:alert(1)",
                "data:text/javascript,bad",
                "//stats.example.test/script.js",
                "https://user:pass@stats.example.test/script.js",
                "http://stats.example.test/script.js",
                "https://stats.example.test/\nscript.js",
            ] {
                let mut values = config();
                values.insert(key.into(), PluginConfigValue::Text(bad.into()));
                assert!(
                    AnalyticsUmami.validate(&values, false).is_err(),
                    "{key}: {bad}"
                );
            }
        }
        for bad in [
            "invalid",
            "00000000-0000-0000-0000-000000000000",
            "\"><script>bad()</script>",
        ] {
            let mut values = config();
            values.insert("website-id".into(), PluginConfigValue::Text(bad.into()));
            assert!(AnalyticsUmami.validate(&values, true).is_err());
        }
        for local in [
            "http://localhost:3000/script.js",
            "http://127.0.0.1:3000/script.js",
            "http://[::1]:3000/script.js",
        ] {
            let mut values = config();
            values.insert("script-url".into(), PluginConfigValue::Text(local.into()));
            assert!(AnalyticsUmami.validate(&values, true).is_ok());
        }
        let mut values = config();
        values.insert(
            "script-url".into(),
            PluginConfigValue::Text("https://stats.example.test/script.js#fragment".into()),
        );
        assert!(AnalyticsUmami.validate(&values, true).is_err());
    }

    #[tokio::test]
    async fn saves_validate_before_persistence_and_never_change_content_revision() {
        let store = Arc::new(Store::default());
        let plugins = Arc::new(PluginRuntime::new(
            Arc::new(PluginCatalog::builtins()),
            store.clone(),
            Arc::new(crate::SystemClock),
        ));
        let actor = Actor::bootstrap_cli();
        let view = plugins.manager.view(&actor).await.unwrap();
        let umami = view
            .plugins
            .iter()
            .find(|plugin| plugin.definition.id == "analytics-umami")
            .unwrap();
        assert!(!umami.enabled);
        assert_eq!(umami.definition.hooks, vec![PluginHook::PageHead]);
        assert!(matches!(
            umami.config["respect-dnt"],
            PluginConfigValue::Boolean(true)
        ));
        let command = |enabled, config, version| SavePluginCmd {
            id: "analytics-umami".into(),
            enabled,
            config,
            expected_version: version,
        };
        assert!(matches!(
            plugins
                .manager
                .save(&actor, command(true, PluginConfig::new(), 0))
                .await,
            Err(UseCaseError::Invalid(_))
        ));
        assert_eq!(store.load().await.unwrap().version, 0);
        let runtime = crate::RenderingRuntime::default().with_plugins(plugins.clone());
        use application::ports::ContentRenderer;
        let before = runtime.render_content("**unchanged**").await.unwrap();
        plugins
            .manager
            .save(&actor, command(false, config(), 0))
            .await
            .unwrap();
        plugins
            .manager
            .save(&actor, command(true, config(), 1))
            .await
            .unwrap();
        let snapshot = plugins.manager.snapshot().await.unwrap();
        assert_eq!(snapshot.render_revision, 0);
        assert!(snapshot.active.contains_key("analytics-umami"));
        assert_eq!(
            runtime.render_content("**unchanged**").await.unwrap(),
            before
        );
        let mut invalid = config();
        invalid.insert(
            "script-url".into(),
            PluginConfigValue::Text("javascript:alert(1)".into()),
        );
        assert!(
            plugins
                .manager
                .save(&actor, command(true, invalid, 2))
                .await
                .is_err()
        );
        assert_eq!(store.load().await.unwrap().version, 2);
        let mut updated = config();
        updated.insert("respect-dnt".into(), PluginConfigValue::Boolean(false));
        plugins
            .manager
            .save(&actor, command(true, updated, 2))
            .await
            .unwrap();
        let head = plugins
            .catalog
            .head_html(
                PluginPage::Index,
                &plugins.manager.snapshot().await.unwrap(),
            )
            .unwrap();
        assert!(head.contains("data-do-not-track=\"false\""));
        plugins
            .manager
            .save(&actor, command(false, config(), 3))
            .await
            .unwrap();
        assert!(plugins.manager.snapshot().await.unwrap().active.is_empty());
        assert_eq!(store.load().await.unwrap().value.render_revision, 0);
    }
}
