#[derive(serde::Serialize, ts_rs::TS)]
pub struct ThemeOption {
    pub slug: String,
    pub name: String,
    pub release: String,
}
impl From<application::themes::ThemeOption> for ThemeOption {
    fn from(value: application::themes::ThemeOption) -> Self {
        let application::themes::ThemeOption {
            slug,
            name,
            release,
        } = value;
        Self {
            slug,
            name,
            release,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum SiteSettingsSource {
    Database,
    Fallback,
}
impl From<application::settings::SiteSettingsSource> for SiteSettingsSource {
    fn from(source: application::settings::SiteSettingsSource) -> Self {
        match source {
            application::settings::SiteSettingsSource::Database => Self::Database,
            application::settings::SiteSettingsSource::Fallback => Self::Fallback,
        }
    }
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct ThemeSettings {
    pub slug: String,
    pub effective_slug: String,
    pub fallback_slug: String,
    pub source: SiteSettingsSource,
    pub version: i64,
    pub available: Vec<ThemeOption>,
}
impl From<application::settings::ThemeSettingsView> for ThemeSettings {
    fn from(value: application::settings::ThemeSettingsView) -> Self {
        let application::settings::ThemeSettingsView {
            slug,
            effective_slug,
            fallback_slug,
            source,
            version,
            available,
        } = value;
        Self {
            slug,
            effective_slug,
            fallback_slug,
            source: source.into(),
            version,
            available: available.into_iter().map(Into::into).collect(),
        }
    }
}
#[derive(serde::Serialize, ts_rs::TS, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionSettings {
    pub comment_ip_days: i32,
    pub comment_version: i64,
    pub audit_days: i32,
    pub audit_version: i64,
}
impl From<application::retention::RetentionSettings> for RetentionSettings {
    fn from(value: application::retention::RetentionSettings) -> Self {
        let application::retention::RetentionSettings {
            comment_ip_days,
            comment_version,
            audit_days,
            audit_version,
        } = value;
        Self {
            comment_ip_days,
            comment_version,
            audit_days,
            audit_version,
        }
    }
}
impl From<RetentionSettings> for application::retention::RetentionSettings {
    fn from(value: RetentionSettings) -> Self {
        Self {
            comment_ip_days: value.comment_ip_days,
            comment_version: value.comment_version,
            audit_days: value.audit_days,
            audit_version: value.audit_version,
        }
    }
}
