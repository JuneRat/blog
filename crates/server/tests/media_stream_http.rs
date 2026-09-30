//! Public media reads are lazy, bounded, and keep admission until the body ends.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use application::{
    UseCaseError,
    media::MediaInteractor,
    ports::{
        MediaChangeOutcome, MediaReader, MediaRepository, MediaStorage, MediaUsageRow,
        MediaWithUsage, OpenedMedia,
    },
};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use domain::media::{ImageFormat, ImageInfo, Media, MediaSnapshot};
use http_body_util::BodyExt;
use interfaces::http_media::{MEDIA_READ_CONCURRENCY, MediaReadState, media_read_router};
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

struct MetadataRepository(MediaSnapshot);

#[async_trait]
impl MediaRepository for MetadataRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaSnapshot>, UseCaseError> {
        Ok((id == self.0.id).then(|| self.0.clone()))
    }
    async fn insert(
        &self,
        _: &Media,
        _: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        unreachable!("read-only fixture")
    }
    async fn find_view(&self, _: Uuid) -> Result<Option<MediaWithUsage>, UseCaseError> {
        unreachable!("read-only fixture")
    }
    async fn list(
        &self,
        _: i64,
        _: i64,
        _: bool,
        _: Option<&str>,
    ) -> Result<(Vec<MediaWithUsage>, i64), UseCaseError> {
        unreachable!("read-only fixture")
    }
    async fn usage_of(&self, _: Uuid) -> Result<Vec<MediaUsageRow>, UseCaseError> {
        unreachable!("read-only fixture")
    }
    async fn set_deleted(
        &self,
        _: Uuid,
        _: i64,
        _: bool,
        _: OffsetDateTime,
        _: application::audit::AuditContext,
    ) -> Result<MediaChangeOutcome, UseCaseError> {
        unreachable!("read-only fixture")
    }
}

#[derive(Default)]
struct ReadCounters {
    opens: AtomicUsize,
    chunks: AtomicUsize,
    active: AtomicUsize,
    missing: AtomicBool,
    fail_read: AtomicBool,
    reported_size: AtomicU64,
}

struct CountingStorage {
    bytes: Arc<Vec<u8>>,
    counts: Arc<ReadCounters>,
}

#[async_trait]
impl MediaStorage for CountingStorage {
    async fn open(&self, _: &str) -> Result<Option<OpenedMedia>, UseCaseError> {
        self.counts.opens.fetch_add(1, Ordering::SeqCst);
        if self.counts.missing.load(Ordering::SeqCst) {
            return Ok(None);
        }
        self.counts.active.fetch_add(1, Ordering::SeqCst);
        Ok(Some(OpenedMedia {
            byte_size: self.counts.reported_size.load(Ordering::SeqCst),
            reader: Box::new(CountingReader {
                bytes: self.bytes.clone(),
                counts: self.counts.clone(),
                position: 0,
            }),
        }))
    }
    async fn put_staged(&self, _: &str, _: &[u8]) -> Result<String, UseCaseError> {
        unreachable!("read-only fixture")
    }
    async fn promote(&self, _: &str) -> Result<(), UseCaseError> {
        unreachable!("read-only fixture")
    }
    async fn delete(&self, _: &str) -> Result<(), UseCaseError> {
        unreachable!("read-only fixture")
    }
    async fn discard_orphaned_staging(&self, _: OffsetDateTime) -> Result<i64, UseCaseError> {
        unreachable!("read-only fixture")
    }
}

struct CountingReader {
    bytes: Arc<Vec<u8>>,
    counts: Arc<ReadCounters>,
    position: usize,
}

impl Drop for CountingReader {
    fn drop(&mut self) {
        self.counts.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl MediaReader for CountingReader {
    async fn read_chunk(&mut self, limit: usize) -> Result<Option<Vec<u8>>, UseCaseError> {
        self.counts.chunks.fetch_add(1, Ordering::SeqCst);
        assert!(limit <= 64 * 1024, "the response must bound each read");
        if self.counts.fail_read.load(Ordering::SeqCst) {
            return Err(UseCaseError::Repository(
                "injected file read failure".into(),
            ));
        }
        if self.position == self.bytes.len() {
            return Ok(None);
        }
        let end = (self.position + limit).min(self.bytes.len());
        let bytes = self.bytes[self.position..end].to_vec();
        self.position = end;
        Ok(Some(bytes))
    }
}

struct Fixture {
    router: Router,
    id: Uuid,
    bytes: Arc<Vec<u8>>,
    counts: Arc<ReadCounters>,
}

fn fixture() -> Fixture {
    let id = Uuid::now_v7();
    let bytes = Arc::new(vec![42; 3 * 64 * 1024 + 17]);
    let counts = Arc::new(ReadCounters::default());
    counts
        .reported_size
        .store(bytes.len() as u64, Ordering::SeqCst);
    let snapshot = Media::uploaded(
        id,
        None,
        format!("objects/{id}.png"),
        "image.png",
        ImageInfo::new(ImageFormat::Png, 1, 1).unwrap(),
        bytes.len() as u64,
        "a".repeat(64),
        OffsetDateTime::now_utc(),
    )
    .unwrap()
    .snapshot();
    let media = Arc::new(MediaInteractor::new(
        Arc::new(infrastructure::image_inspection::HeaderImageInspector),
        Arc::new(MetadataRepository(snapshot)),
        Arc::new(CountingStorage {
            bytes: bytes.clone(),
            counts: counts.clone(),
        }),
        Arc::new(infrastructure::SystemClock),
    ));
    Fixture {
        router: media_read_router(MediaReadState { media }),
        id,
        bytes,
        counts,
    }
}

async fn request(f: &Fixture, method: &str, etag: Option<&str>) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("/media/{}", f.id));
    if let Some(etag) = etag {
        builder = builder.header(header::IF_NONE_MATCH, etag);
    }
    f.router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn head_verifies_file_and_returns_get_metadata_without_reading_chunks() {
    let f = fixture();
    let response = request(&f, "HEAD", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        f.bytes.len().to_string()
    );
    assert_eq!(
        response.headers()[header::ETAG],
        format!("\"{}\"", "a".repeat(64))
    );
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    assert_eq!(f.counts.opens.load(Ordering::SeqCst), 1);
    assert_eq!(f.counts.chunks.load(Ordering::SeqCst), 0);
    assert_eq!(f.counts.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn get_stream_is_lazy_and_delivers_exact_bytes_in_bounded_chunks() {
    let f = fixture();
    let response = request(&f, "GET", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        f.bytes.len().to_string()
    );
    assert_eq!(f.counts.chunks.load(Ordering::SeqCst), 0);
    assert_eq!(f.counts.active.load(Ordering::SeqCst), 1);
    let mut body = response.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert_eq!(first.len(), 64 * 1024);
    assert_eq!(f.counts.chunks.load(Ordering::SeqCst), 1);
    let mut bytes = first.to_vec();
    bytes.extend_from_slice(&body.collect().await.unwrap().to_bytes());
    assert_eq!(&bytes, &*f.bytes);
    assert_eq!(f.counts.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn conditional_reads_keep_fast_path_and_missing_or_changed_files_fail_before_headers() {
    let f = fixture();
    let etag = format!("\"{}\"", "a".repeat(64));
    let response = request(&f, "GET", Some(&etag)).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(f.counts.opens.load(Ordering::SeqCst), 0);
    assert_eq!(
        request(&f, "HEAD", Some(&etag)).await.status(),
        StatusCode::NOT_MODIFIED
    );
    assert_eq!(f.counts.opens.load(Ordering::SeqCst), 1);
    assert_eq!(f.counts.chunks.load(Ordering::SeqCst), 0);
    f.counts.missing.store(true, Ordering::SeqCst);
    for method in ["GET", "HEAD"] {
        assert_eq!(
            request(&f, method, None).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
    f.counts.missing.store(false, Ordering::SeqCst);
    f.counts.reported_size.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        request(&f, "GET", None).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(f.counts.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn slow_responses_hold_budget_until_the_body_finishes_or_is_cancelled() {
    let f = fixture();
    let mut responses = Vec::new();
    for _ in 0..MEDIA_READ_CONCURRENCY {
        let response = request(&f, "GET", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        responses.push(response);
    }
    assert_eq!(
        f.counts.active.load(Ordering::SeqCst),
        MEDIA_READ_CONCURRENCY
    );
    assert_eq!(f.counts.chunks.load(Ordering::SeqCst), 0);
    let rejected = request(&f, "GET", None).await;
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(rejected.headers()[header::RETRY_AFTER], "1");
    assert_eq!(
        f.counts.opens.load(Ordering::SeqCst),
        MEDIA_READ_CONCURRENCY
    );
    // Cancelling a slow client returns its slot immediately.
    drop(responses.pop());
    let response = request(&f, "GET", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body().collect().await.unwrap();
    for response in responses {
        response.into_body().collect().await.unwrap();
    }
    assert_eq!(f.counts.active.load(Ordering::SeqCst), 0);
    assert_eq!(request(&f, "HEAD", None).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn read_failure_terminates_body_and_releases_file() {
    let f = fixture();
    let response = request(&f, "GET", None).await;
    f.counts.fail_read.store(true, Ordering::SeqCst);
    assert!(response.into_body().collect().await.is_err());
    assert_eq!(f.counts.active.load(Ordering::SeqCst), 0);
    f.counts.fail_read.store(false, Ordering::SeqCst);
    assert_eq!(request(&f, "GET", None).await.status(), StatusCode::OK);
}
