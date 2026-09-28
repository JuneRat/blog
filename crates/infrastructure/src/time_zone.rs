//! 站点展示时区；时区数据库嵌入二进制，不读取操作系统默认时区。
use application::ports::DateTimeFormatter;
use jiff::{Timestamp, tz::TimeZone};
use time::OffsetDateTime;

pub struct IanaTimeZones;

impl application::ports::TimeZoneProvider for IanaTimeZones {
    fn resolve(
        &self,
        name: &str,
    ) -> Result<std::sync::Arc<dyn DateTimeFormatter>, application::UseCaseError> {
        SiteTimeZone::parse(name)
            .map(|zone| std::sync::Arc::new(zone) as _)
            .map_err(application::UseCaseError::Invalid)
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = jiff::tz::db()
            .available()
            .map(|name| name.to_string())
            .collect();
        if !names.iter().any(|name| name == "UTC") {
            names.push("UTC".into());
        }
        names.sort();
        names
    }
}

#[derive(Clone, Debug)]
pub struct SiteTimeZone {
    name: String,
    zone: TimeZone,
}

impl Default for SiteTimeZone {
    fn default() -> Self {
        Self {
            name: "UTC".into(),
            zone: TimeZone::UTC,
        }
    }
}

impl SiteTimeZone {
    pub fn parse(name: &str) -> Result<Self, String> {
        let zone = TimeZone::get(name)
            .map_err(|_| "必须是有效的 IANA 时区名称，例如 Asia/Shanghai 或 UTC".to_owned())?;
        Ok(Self {
            name: name.to_owned(),
            zone,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// 日志时间保留毫秒和显式 UTC 偏移，既可读也能按绝对时刻解析。
    pub fn log_timestamp(&self, at: OffsetDateTime) -> String {
        let Ok(timestamp) = Timestamp::from_nanosecond(at.unix_timestamp_nanos()) else {
            return application::public_site::api_datetime(at);
        };
        timestamp
            .to_zoned(self.zone.clone())
            .strftime("%Y-%m-%dT%H:%M:%S.%3f%:z")
            .to_string()
    }
}

impl DateTimeFormatter for SiteTimeZone {
    fn format(&self, at: OffsetDateTime) -> String {
        // The libraries have slightly different extreme date ranges; never panic on a DB value.
        let Ok(timestamp) = Timestamp::from_nanosecond(at.unix_timestamp_nanos()) else {
            return application::public_site::format_datetime(at);
        };
        if self.name == "UTC" {
            return application::public_site::format_datetime(at);
        }
        let local = timestamp.to_zoned(self.zone.clone());
        format!("{} ({})", local.strftime("%Y-%m-%d %H:%M %:z"), self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn shanghai_crosses_midnight_without_changing_the_instant() {
        let zone = SiteTimeZone::parse("Asia/Shanghai").unwrap();
        assert_eq!(
            zone.format(datetime!(2026-09-28 17:30 UTC)),
            "2026-09-29 01:30 +08:00 (Asia/Shanghai)"
        );
        assert_eq!(
            zone.format(datetime!(2026-09-29 01:30 +08:00)),
            "2026-09-29 01:30 +08:00 (Asia/Shanghai)"
        );
        assert_eq!(
            SiteTimeZone::parse("UTC")
                .unwrap()
                .format(datetime!(2026-09-29 01:30 +08:00)),
            "2026-09-28 17:30 UTC"
        );
    }

    #[test]
    fn iana_rules_apply_seasonal_offsets_and_reject_unknown_names() {
        let zone = SiteTimeZone::parse("America/New_York").unwrap();
        assert_eq!(
            zone.format(datetime!(2026-01-15 12:00 UTC)),
            "2026-01-15 07:00 -05:00 (America/New_York)"
        );
        assert_eq!(
            zone.format(datetime!(2026-07-15 12:00 UTC)),
            "2026-07-15 08:00 -04:00 (America/New_York)"
        );
        assert!(SiteTimeZone::parse("Asia/Invalid").is_err());
        assert!(SiteTimeZone::parse("+08:00").is_err());
    }

    #[test]
    fn log_timestamps_are_millisecond_rfc3339_with_the_instant_specific_offset() {
        let zone = SiteTimeZone::parse("America/New_York").unwrap();
        assert_eq!(
            zone.log_timestamp(datetime!(2026-01-15 12:00:00.123456 UTC)),
            "2026-01-15T07:00:00.123-05:00"
        );
        assert_eq!(
            zone.log_timestamp(datetime!(2026-07-15 12:00 UTC)),
            "2026-07-15T08:00:00.000-04:00"
        );
        assert_eq!(
            SiteTimeZone::default().log_timestamp(datetime!(2026-07-15 12:00 UTC)),
            "2026-07-15T12:00:00.000+00:00"
        );
    }
}
