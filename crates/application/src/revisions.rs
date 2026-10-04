//! Editable fields only: restoring history never restores ownership or publication state.
use crate::{UseCaseError, content::SeriesPlacement, ports::PostRecord};
use domain::content::{PageSnapshot, Visibility};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const REVISION_LIMIT: i64 = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionContent {
    pub slug: String,
    pub title: String,
    pub content: String,
    pub visibility: String,
    pub excerpt: Option<String>,
    pub tag_ids: Vec<Uuid>,
    pub category_id: Option<Uuid>,
    pub series: Vec<SeriesPlacement>,
    pub cover_media_id: Option<Uuid>,
}
impl RevisionContent {
    pub fn post(record: &PostRecord) -> Self {
        let s = &record.snapshot;
        Self {
            slug: s.slug.clone(),
            title: s.title.clone(),
            content: s.content.clone(),
            visibility: s.visibility.as_str().into(),
            excerpt: s.excerpt.clone(),
            tag_ids: record.tag_ids.clone(),
            category_id: s.category_id,
            series: s.series.iter().copied().map(Into::into).collect(),
            cover_media_id: s.cover_media_id,
        }
    }
    pub fn page(s: &PageSnapshot) -> Self {
        Self {
            slug: s.slug.clone(),
            title: s.title.clone(),
            content: s.content.clone(),
            visibility: s.visibility.as_str().into(),
            excerpt: None,
            tag_ids: vec![],
            category_id: None,
            series: vec![],
            cover_media_id: None,
        }
    }
    pub fn visibility(&self) -> Result<Visibility, UseCaseError> {
        Visibility::parse(&self.visibility)
            .ok_or_else(|| UseCaseError::DataCorrupt("修订可见性无效".into()))
    }
    pub fn apply_post(self, record: &mut PostRecord) -> Result<(), UseCaseError> {
        let visibility = self.visibility()?;
        let s = &mut record.snapshot;
        s.slug = self.slug;
        s.title = self.title;
        s.content = self.content;
        s.visibility = visibility;
        s.excerpt = self.excerpt;
        s.category_id = self.category_id;
        s.series = self.series.into_iter().map(Into::into).collect();
        s.cover_media_id = self.cover_media_id;
        record.tag_ids = self.tag_ids;
        Ok(())
    }
    pub fn apply_page(self, s: &mut PageSnapshot) -> Result<(), UseCaseError> {
        s.visibility = self.visibility()?;
        s.slug = self.slug;
        s.title = self.title;
        s.content = self.content;
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct RevisionSummary {
    pub id: Uuid,
    pub version: i64,
    pub title: String,
    pub created_at: String,
    pub actor_id: Option<Uuid>,
}
