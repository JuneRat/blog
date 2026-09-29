use application::UseCaseError;
use application::content_queries::{
    AdminPageSummary, AdminPostSummary, ContentListFilter, ContentListRequest, ContentQueries,
    PageListFilter, PostListFilter, Visibility,
};
use application::identity::{Actor, ActorChannel};
use application::ports::{AdminPageQuery, AdminPostQuery};
use domain::identity::{PermissionSet, UserId};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Default)]
struct QuerySpy(
    Mutex<Vec<(Option<Uuid>, ObservedFilter)>>,
    Mutex<Vec<String>>,
    Mutex<Option<domain::identity::UserSnapshot>>,
);

#[async_trait::async_trait]
impl application::ports::UserQuery for QuerySpy {
    async fn find_by_id(
        &self,
        _: Uuid,
    ) -> Result<Option<domain::identity::UserSnapshot>, UseCaseError> {
        unreachable!()
    }
    async fn find_by_username(
        &self,
        name: &str,
    ) -> Result<Option<domain::identity::UserSnapshot>, UseCaseError> {
        self.1.lock().unwrap().push(name.into());
        Ok(self.2.lock().unwrap().clone())
    }
    async fn list_admin(
        &self,
        _: i64,
        _: i64,
    ) -> Result<Vec<application::ports::AdminUserRow>, UseCaseError> {
        unreachable!()
    }
}

struct ObservedFilter {
    limit: i64,
    offset: i64,
    status: Option<&'static str>,
    visibility: Option<Visibility>,
    trash: bool,
}

impl ObservedFilter {
    fn of<S: Copy>(filter: &ContentListFilter<S>, status: impl Fn(S) -> &'static str) -> Self {
        Self {
            limit: filter.limit(),
            offset: filter.offset(),
            status: filter.status().map(status),
            visibility: filter.visibility(),
            trash: filter.trash(),
        }
    }
}
#[async_trait::async_trait]
impl AdminPostQuery for QuerySpy {
    async fn list(
        &self,
        author: Uuid,
        filter: &PostListFilter,
    ) -> Result<(Vec<AdminPostSummary>, i64), UseCaseError> {
        self.0.lock().unwrap().push((
            Some(author),
            ObservedFilter::of(filter, |status| status.as_str()),
        ));
        Ok((vec![], 41))
    }
}
#[async_trait::async_trait]
impl AdminPageQuery for QuerySpy {
    async fn list(
        &self,
        filter: &PageListFilter,
    ) -> Result<(Vec<AdminPageSummary>, i64), UseCaseError> {
        self.0
            .lock()
            .unwrap()
            .push((None, ObservedFilter::of(filter, |status| status.as_str())));
        Ok((vec![], 21))
    }
}
fn actor(permissions: &[&str]) -> Actor {
    Actor::new(
        UserId::generate(),
        ActorChannel::Session,
        PermissionSet::from_keys(permissions.iter().copied()),
    )
}

#[tokio::test]
async fn normal_and_trash_lists_authorize_before_querying() {
    let spy = Arc::new(QuerySpy::default());
    let queries = ContentQueries::new(spy.clone(), spy.clone(), spy.clone());
    let own = actor(&["post.read"]);
    let any = actor(&["post.read_any"]);
    let outsider = actor(&[]);
    let page_reader = actor(&["page.read"]);
    for trash in [false, true] {
        let request = ContentListRequest {
            trash,
            ..Default::default()
        };
        for (who, author) in [(&outsider, outsider.user_id), (&own, any.user_id)] {
            assert!(matches!(
                queries.posts(who, author, request.clone()).await,
                Err(UseCaseError::Forbidden)
            ));
        }
        assert!(matches!(
            queries.pages(&own, request.clone()).await,
            Err(UseCaseError::Forbidden)
        ));
        let before = spy.0.lock().unwrap().len();
        assert_eq!(before, if trash { 3 } else { 0 });
        assert_eq!(
            queries
                .posts(&own, own.user_id, request.clone())
                .await
                .unwrap()
                .total,
            41
        );
        assert!(
            queries
                .posts(&any, own.user_id, request.clone())
                .await
                .is_ok()
        );
        assert_eq!(
            queries.pages(&page_reader, request).await.unwrap().total,
            21
        );
    }
}

#[tokio::test]
async fn invalid_filters_and_overflowing_pages_never_reach_storage() {
    let spy = Arc::new(QuerySpy::default());
    let queries = ContentQueries::new(spy.clone(), spy.clone(), spy.clone());
    let reader = actor(&["post.read", "page.read"]);
    let mut requests: Vec<_> = [0, -1, i64::MAX]
        .into_iter()
        .map(|page| ContentListRequest {
            page,
            ..Default::default()
        })
        .collect();
    requests.push(ContentListRequest {
        status: Some("deleted".into()),
        ..Default::default()
    });
    requests.push(ContentListRequest {
        visibility: Some("secret".into()),
        ..Default::default()
    });
    for request in requests {
        assert!(matches!(
            queries
                .posts(&reader, reader.user_id, request.clone())
                .await,
            Err(UseCaseError::Invalid(_))
        ));
        assert!(matches!(
            queries.pages(&reader, request).await,
            Err(UseCaseError::Invalid(_))
        ));
    }
    assert!(spy.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn page_metadata_and_validated_filters_reach_storage() {
    let spy = Arc::new(QuerySpy::default());
    let queries = ContentQueries::new(spy.clone(), spy.clone(), spy.clone());
    let reader = actor(&["post.read", "page.read"]);
    let request = ContentListRequest {
        page: 3,
        status: Some("scheduled".into()),
        visibility: Some("private".into()),
        trash: true,
    };
    let result = queries
        .posts(&reader, reader.user_id, request.clone())
        .await
        .unwrap();
    assert_eq!((result.page, result.per_page, result.total), (3, 20, 41));
    queries.pages(&reader, request).await.unwrap();
    let calls = spy.0.lock().unwrap();
    assert_eq!(calls[0].0, Some(reader.user_id.0));
    assert_eq!(calls[1].0, None);
    for (_, filter) in calls.iter() {
        assert_eq!((filter.limit, filter.offset), (20, 40));
        assert_eq!(filter.status, Some("scheduled"));
        assert_eq!(filter.visibility, Some(Visibility::Private));
        assert!(filter.trash);
    }
}

#[tokio::test]
async fn unauthorized_invalid_filters_are_rejected_before_validation() {
    let spy = Arc::new(QuerySpy::default());
    let queries = ContentQueries::new(spy.clone(), spy.clone(), spy.clone());
    let outsider = actor(&[]);
    let request = ContentListRequest {
        page: 0,
        status: Some("active".into()),
        visibility: Some("secret".into()),
        trash: false,
    };
    assert!(matches!(
        queries
            .posts(&outsider, outsider.user_id, request.clone())
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        queries.pages(&outsider, request).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(spy.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn author_lookup_authorizes_before_resolving_and_preserves_target_rules() {
    let spy = Arc::new(QuerySpy::default());
    let queries = ContentQueries::new(spy.clone(), spy.clone(), spy.clone());
    let own = actor(&["post.read"]);
    let any = actor(&["post.read_any"]);
    for trash in [false, true] {
        for name in ["self", "missing", "bad/name", " "] {
            assert!(matches!(
                queries
                    .posts_by_author(
                        &own,
                        Some(name),
                        ContentListRequest {
                            trash,
                            ..Default::default()
                        }
                    )
                    .await,
                Err(UseCaseError::Forbidden)
            ));
        }
        assert!(spy.1.lock().unwrap().is_empty());
        for name in [None, Some("")] {
            queries
                .posts_by_author(
                    &own,
                    name,
                    ContentListRequest {
                        trash,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        assert!(spy.1.lock().unwrap().is_empty());
    }
    assert!(
        spy.0
            .lock()
            .unwrap()
            .iter()
            .all(|(id, _)| *id == Some(own.user_id.0))
    );
    assert!(matches!(
        queries
            .posts_by_author(&any, Some("missing"), Default::default())
            .await,
        Err(UseCaseError::NotFound(_))
    ));
    let snapshot =
        domain::identity::User::new("author", None, None, time::OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .snapshot();
    *spy.2.lock().unwrap() = Some(snapshot.clone());
    queries
        .posts_by_author(&any, Some(" AUTHOR "), Default::default())
        .await
        .unwrap();
    assert_eq!(spy.1.lock().unwrap().last().unwrap(), "author");
    assert_eq!(spy.0.lock().unwrap().last().unwrap().0, Some(snapshot.id));
    let before = spy.0.lock().unwrap().len();
    for deleted in [false, true] {
        let mut inactive = snapshot.clone();
        if deleted {
            inactive.deleted_at = Some(time::OffsetDateTime::UNIX_EPOCH);
        } else {
            inactive.status = domain::identity::UserStatus::Disabled;
        }
        *spy.2.lock().unwrap() = Some(inactive);
        assert!(matches!(
            queries
                .posts_by_author(&any, Some("author"), Default::default())
                .await,
            Err(UseCaseError::Forbidden)
        ));
    }
    assert_eq!(spy.0.lock().unwrap().len(), before);
}

#[tokio::test]
async fn corrupt_stored_author_is_distinct_from_invalid_request() {
    let spy = Arc::new(QuerySpy::default());
    let queries = ContentQueries::new(spy.clone(), spy.clone(), spy.clone());
    let mut snapshot =
        domain::identity::User::new("author", None, None, time::OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .snapshot();
    snapshot.version = 0;
    *spy.2.lock().unwrap() = Some(snapshot);
    assert!(matches!(
        queries
            .posts_by_author(
                &actor(&["post.read_any"]),
                Some("author"),
                Default::default()
            )
            .await,
        Err(UseCaseError::DataCorrupt(_))
    ));
    assert!(spy.0.lock().unwrap().is_empty());
}
