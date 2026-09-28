//! 管理资源间共用的请求字段与传输格式转换。

use application::content::PostVisibility;
use application::content_queries::ContentListRequest;
use application::error::UseCaseError;
use serde::Deserialize;

/// 三态字段的反序列化：缺失 → None（不修改）；JSON null → Some(None)（清空）；
/// 值 → Some(Some(v))。serde 对 Option<Option<T>> 会把 null 折叠成 None，
/// 必须显式包一层才能区分「清空」与「不触碰」。
pub fn deserialize_double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

#[derive(Deserialize, Default, ts_rs::TS)]
#[ts(rename = "VersionInput", optional_fields = nullable)]
pub struct VersionBody {
    pub expected_version: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub author: Option<String>,
    pub page: Option<i64>,
    pub status: Option<String>,
    pub visibility: Option<String>,
}
impl ListQuery {
    pub(super) fn request(self, trash: bool) -> ContentListRequest {
        ContentListRequest {
            page: self.page.unwrap_or(1),
            status: self.status,
            visibility: self.visibility,
            trash,
        }
    }
}
pub(super) fn parse_visibility(value: Option<&str>) -> Result<PostVisibility, UseCaseError> {
    match value {
        None => Ok(PostVisibility::Public),
        Some("public") => Ok(PostVisibility::Public),
        Some("private") => Ok(PostVisibility::Private),
        Some(other) => Err(UseCaseError::Invalid(format!(
            "visibility 只支持 public/private，收到 {other}"
        ))),
    }
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(rename = "ScheduleInput", optional_fields = nullable)]
pub(super) struct ScheduleBody {
    pub(super) published_at: String,
    pub(super) expected_version: Option<i64>,
}
pub(super) fn api_datetime(at: time::OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| at.to_string())
}

pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<VersionBody>(out);
    crate::http_contract::declare::<ScheduleBody>(out);
}
