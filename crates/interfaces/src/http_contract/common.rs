//! Shared wire types. Application/domain values are converted at the HTTP boundary.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Serialize, TS)]
pub struct ContentPage<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}
impl<T> From<application::content_queries::ContentPage<T>> for ContentPage<T> {
    fn from(value: application::content_queries::ContentPage<T>) -> Self {
        let application::content_queries::ContentPage {
            items,
            total,
            page,
            per_page,
        } = value;
        Self {
            items,
            total,
            page,
            per_page,
        }
    }
}
#[derive(Serialize, Deserialize, TS)]
pub struct SeriesPlacement {
    pub series_id: Uuid,
    #[serde(default)]
    pub position: i32,
}
impl From<application::content::SeriesPlacement> for SeriesPlacement {
    fn from(value: application::content::SeriesPlacement) -> Self {
        Self {
            series_id: value.series_id,
            position: value.position,
        }
    }
}
impl From<SeriesPlacement> for application::content::SeriesPlacement {
    fn from(value: SeriesPlacement) -> Self {
        Self {
            series_id: value.series_id,
            position: value.position,
        }
    }
}
#[derive(Serialize, TS)]
pub struct PreviewResult {
    pub content_html: String,
}
#[derive(Serialize, TS)]
pub struct MessageResult {
    pub message: String,
}

#[derive(Serialize, TS)]
pub struct ReorderSeriesResult {
    pub series_version: i64,
    pub ordered_post_ids: Vec<Uuid>,
}
impl From<application::series::ReorderedDto> for ReorderSeriesResult {
    fn from(value: application::series::ReorderedDto) -> Self {
        let application::series::ReorderedDto {
            series_version,
            ordered_post_ids,
        } = value;
        Self {
            series_version,
            ordered_post_ids,
        }
    }
}
