//! Controlled, flat theme configuration contract shared by packages and admin HTTP.
use crate::UseCaseError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;
pub type ThemeConfig = BTreeMap<String, ThemeValue>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ThemeValue {
    Boolean(bool),
    Integer(i64),
    Text(String),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeFieldType {
    Text,
    Textarea,
    Integer,
    Boolean,
    Select,
    Color,
    Media,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeChoice {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeField {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: ThemeFieldType,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub group: String,
    pub default: ThemeValue,
    #[serde(default)]
    pub min_length: Option<usize>,
    #[serde(default)]
    pub max_length: Option<usize>,
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
    #[serde(default)]
    pub options: Vec<ThemeChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeSchema {
    pub config_schema_version: u32,
    pub fields: Vec<ThemeField>,
}
impl Default for ThemeSchema {
    fn default() -> Self {
        Self {
            config_schema_version: 1,
            fields: vec![],
        }
    }
}
fn invalid(message: impl Into<String>) -> UseCaseError {
    UseCaseError::Invalid(message.into())
}
fn safe_integer(n: i64) -> bool {
    (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&n)
}

impl ThemeField {
    pub fn validate_value(&self, value: &ThemeValue) -> Result<(), UseCaseError> {
        use ThemeFieldType as T;
        use ThemeValue as V;
        let valid = match (self.kind, value) {
            (T::Text | T::Textarea, V::Text(s)) => {
                let len = s.chars().count();
                len >= self.min_length.unwrap_or(0) && len <= self.max_length.unwrap_or(8192)
            }
            (T::Integer, V::Integer(n)) => {
                safe_integer(*n)
                    && self.min.is_none_or(|min| *n >= min)
                    && self.max.is_none_or(|max| *n <= max)
            }
            (T::Boolean, V::Boolean(_)) => true,
            (T::Select, V::Text(s)) => self.options.iter().any(|o| o.value == *s),
            (T::Color, V::Text(s)) => {
                s.len() == 7
                    && s.starts_with('#')
                    && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
            }
            (T::Media, V::Null) => true,
            (T::Media, V::Text(s)) => {
                uuid::Uuid::parse_str(s).is_ok_and(|id| id.to_string() == *s && !id.is_nil())
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(invalid(format!(
                "字段 {}（{}）：值类型或约束无效",
                self.key, self.label
            )))
        }
    }
}
impl ThemeSchema {
    pub fn parse(bytes: &[u8]) -> Result<Self, UseCaseError> {
        if bytes.len() > MAX_SCHEMA_BYTES {
            return Err(invalid("主题配置声明不能超过 64 KiB"));
        }
        let schema: Self =
            serde_json::from_slice(bytes).map_err(|e| invalid(format!("主题配置声明无效：{e}")))?;
        schema.validate()?;
        Ok(schema)
    }
    pub fn validate(&self) -> Result<(), UseCaseError> {
        if self.config_schema_version == 0
            || self.config_schema_version > i32::MAX as u32
            || self.fields.len() > 64
        {
            return Err(invalid("主题配置结构版本必须为正整数，最多允许 64 个字段"));
        }
        let mut keys = BTreeSet::new();
        for f in &self.fields {
            let key = f.key.as_bytes();
            if key.is_empty()
                || key.len() > 64
                || !key[0].is_ascii_lowercase()
                || !key
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
                || !keys.insert(&f.key)
            {
                return Err(invalid(format!("主题配置字段键非法或重复：{}", f.key)));
            }
            if f.label.trim().is_empty()
                || f.label.chars().count() > 100
                || f.description.chars().count() > 1000
                || f.group.chars().count() > 100
            {
                return Err(invalid(format!("字段 {}：标签、说明或分组长度无效", f.key)));
            }
            let text = matches!(f.kind, ThemeFieldType::Text | ThemeFieldType::Textarea);
            if (!text && (f.min_length.is_some() || f.max_length.is_some()))
                || f.max_length.is_some_and(|n| n > 8192)
                || f.min_length.unwrap_or(0) > f.max_length.unwrap_or(8192)
                || (f.kind != ThemeFieldType::Integer && (f.min.is_some() || f.max.is_some()))
                || f.min.is_some_and(|n| !safe_integer(n))
                || f.max.is_some_and(|n| !safe_integer(n))
                || matches!((f.min, f.max), (Some(a),Some(b)) if a>b)
            {
                return Err(invalid(format!("字段 {}：约束不适用或范围无效", f.key)));
            }
            if f.kind == ThemeFieldType::Select {
                let mut choices = BTreeSet::new();
                if f.options.is_empty()
                    || f.options.len() > 64
                    || f.options.iter().any(|o| {
                        o.value.is_empty()
                            || o.value.chars().count() > 200
                            || o.label.trim().is_empty()
                            || o.label.chars().count() > 100
                            || !choices.insert(&o.value)
                    })
                {
                    return Err(invalid(format!("字段 {}：选项无效或重复", f.key)));
                }
            } else if !f.options.is_empty() {
                return Err(invalid(format!("字段 {}：只有单选字段允许选项", f.key)));
            }
            // Package defaults cannot attach database media; preflight stays database-free.
            if f.kind == ThemeFieldType::Media && f.default != ThemeValue::Null {
                return Err(invalid(format!("字段 {}：媒体默认值必须为 null", f.key)));
            }
            f.validate_value(&f.default)?;
        }
        self.validate_config(&self.defaults())
    }
    pub fn defaults(&self) -> ThemeConfig {
        self.fields
            .iter()
            .map(|f| (f.key.clone(), f.default.clone()))
            .collect()
    }
    pub fn validate_config(&self, config: &ThemeConfig) -> Result<(), UseCaseError> {
        if serde_json::to_vec(config)
            .map_err(|e| invalid(e.to_string()))?
            .len()
            > MAX_CONFIG_BYTES
        {
            return Err(invalid("主题配置不能超过 64 KiB"));
        }
        for (key, value) in config {
            let field = self
                .fields
                .iter()
                .find(|f| f.key == *key)
                .ok_or_else(|| invalid(format!("未声明的主题配置字段：{key}")))?;
            field.validate_value(value)?;
        }
        Ok(())
    }
    /// Missing means use the default; explicit empty text / false / media null stays explicit.
    pub fn effective(&self, config: &ThemeConfig) -> Result<ThemeConfig, UseCaseError> {
        self.validate_config(config)?;
        let mut effective = self.defaults();
        effective.extend(config.clone());
        Ok(effective)
    }
    pub fn media_fields(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter(|f| f.kind == ThemeFieldType::Media)
            .map(|f| f.key.clone())
            .collect()
    }
    pub fn media_ids(&self, config: &ThemeConfig) -> Vec<uuid::Uuid> {
        self.fields
            .iter()
            .filter(|f| f.kind == ThemeFieldType::Media)
            .filter_map(|f| match config.get(&f.key) {
                Some(ThemeValue::Text(s)) => uuid::Uuid::parse_str(s).ok(),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeRecord {
    pub id: uuid::Uuid,
    pub slug: String,
    pub config: ThemeConfig,
    pub config_schema_version: u32,
    pub version: i64,
    pub release: String,
    pub media_fields: Vec<String>,
}
impl ThemeRecord {
    pub fn effective(
        &self,
        release: &str,
        schema: &ThemeSchema,
    ) -> Result<ThemeConfig, UseCaseError> {
        if self.release != release
            || self.config_schema_version != schema.config_schema_version
            || self.media_fields != schema.media_fields()
        {
            return Err(invalid(
                "主题发布快照与配置结构不一致，配置已保留，请检查主题升级",
            ));
        }
        schema.effective(&self.config)
    }
}

#[async_trait::async_trait]
pub trait ThemeConfigStore: Send + Sync {
    async fn find(&self, slug: &str) -> Result<Option<ThemeRecord>, UseCaseError>;
    async fn save(
        &self,
        current: &ThemeRecord,
        config: &ThemeConfig,
        schema: &ThemeSchema,
        actor: crate::audit::AuditContext,
    ) -> Result<ThemeRecord, UseCaseError>;
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeConfigView {
    pub id: uuid::Uuid,
    pub slug: String,
    pub release: String,
    pub fields: Vec<ThemeField>,
    pub config: ThemeConfig,
    pub overrides: ThemeConfig,
    pub config_schema_version: u32,
    pub version: i64,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveThemeConfigCmd {
    pub id: uuid::Uuid,
    pub expected_release: String,
    pub config_schema_version: u32,
    pub expected_version: i64,
    pub config: ThemeConfig,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schema() -> ThemeSchema {
        ThemeSchema::parse(br##"{"config_schema_version":1,"fields":[
        {"key":"title","type":"text","label":"Title","default":"fallback","min_length":0,"max_length":10},
        {"key":"enabled","type":"boolean","label":"Enabled","default":true},
        {"key":"image","type":"media","label":"Image","default":null},
        {"key":"count","type":"integer","label":"Count","default":1,"min":1,"max":3},
        {"key":"color","type":"color","label":"Color","default":"#2563eb"},
        {"key":"layout","type":"select","label":"Layout","default":"wide","options":[{"value":"wide","label":"Wide"}]}] }"##).unwrap()
    }
    #[test]
    fn missing_defaults_and_explicit_empty_values_are_distinct() {
        let s = schema();
        let mut config = ThemeConfig::new();
        config.insert("title".into(), ThemeValue::Text(String::new()));
        config.insert("enabled".into(), ThemeValue::Boolean(false));
        config.insert("image".into(), ThemeValue::Null);
        let effective = s.effective(&config).unwrap();
        assert_eq!(effective["title"], ThemeValue::Text(String::new()));
        assert_eq!(effective["enabled"], ThemeValue::Boolean(false));
        assert_eq!(effective["count"], ThemeValue::Integer(1));
        assert_eq!(effective["image"], ThemeValue::Null);
        assert!(
            ThemeSchema::default()
                .effective(&ThemeConfig::new())
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn invalid_values_and_undeclared_fields_are_rejected() {
        for (key, value) in [
            ("title", ThemeValue::Null),
            ("enabled", ThemeValue::Integer(1)),
            ("count", ThemeValue::Integer(4)),
            ("count", ThemeValue::Text("1".into())),
            ("color", ThemeValue::Text("red".into())),
            ("image", ThemeValue::Text("/media/x".into())),
            ("layout", ThemeValue::Text("narrow".into())),
            ("extra", ThemeValue::Text("x".into())),
        ] {
            assert!(
                schema()
                    .validate_config(&BTreeMap::from([(key.into(), value)]))
                    .is_err(),
                "{key}"
            );
        }
        assert!(
            schema()
                .validate_config(&BTreeMap::from([(
                    "title".into(),
                    ThemeValue::Text("😀".repeat(10))
                )]))
                .is_ok()
        );
    }
    #[test]
    fn invalid_schema_defaults_duplicate_keys_and_constraints_are_rejected() {
        let mut s = schema();
        s.fields.push(s.fields[0].clone());
        assert!(s.validate().is_err());
        let mut s = schema();
        s.fields[3].default = ThemeValue::Integer(0);
        assert!(s.validate().is_err());
        let mut s = schema();
        s.fields[0].min = Some(1);
        assert!(s.validate().is_err());
        let mut s = schema();
        s.fields[2].default = ThemeValue::Text(uuid::Uuid::now_v7().to_string());
        assert!(s.validate().is_err());
        let mut s = schema();
        let duplicate = s.fields[5].options[0].clone();
        s.fields[5].options.push(duplicate);
        assert!(s.validate().is_err());
        let mut s = schema();
        s.config_schema_version = 0;
        assert!(s.validate().is_err());
        assert!(
            ThemeSchema::parse(br#"{"config_schema_version":1,"fields":[],"script":"x"}"#).is_err()
        );
        assert!(
            ThemeSchema::parse(
                br#"{"config_schema_version":1,"fields":[{"key":"x","type":"text","label":"X"}]}"#
            )
            .is_err()
        );
    }
    #[test]
    fn field_and_payload_limits_are_enforced() {
        let mut s = schema();
        s.fields = (0..65)
            .map(|i| ThemeField {
                key: format!("field_{i}"),
                ..s.fields[0].clone()
            })
            .collect();
        assert!(s.validate().is_err());
        assert!(ThemeSchema::parse(&vec![b' '; MAX_SCHEMA_BYTES + 1]).is_err());
        let mut s = schema();
        s.fields[0].max_length = Some(8193);
        assert!(s.validate().is_err());
        let config = BTreeMap::from([(
            "title".into(),
            ThemeValue::Text("x".repeat(MAX_CONFIG_BYTES)),
        )]);
        assert!(s.validate_config(&config).is_err());
    }
}
