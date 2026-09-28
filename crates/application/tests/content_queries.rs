use application::UseCaseError;
use application::content_queries::{
    AdminPageSummary, AdminPostSummary, ContentListFilter, ContentListRequest, ContentQueries,
};
use application::identity::{Actor, ActorChannel};
use application::ports::{AdminPageQuery, AdminPostQuery};
use domain::identity::{PermissionSet, UserId};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Default)]
struct QuerySpy(Mutex<Vec<(Option<Uuid>, ContentListFilter)>>);
#[async_trait::async_trait]
impl AdminPostQuery for QuerySpy {
    async fn list(
        &self,
        author: Uuid,
        filter: &ContentListFilter,
    ) -> Result<(Vec<AdminPostSummary>, i64), UseCaseError> {
        self.0.lock().unwrap().push((Some(author), filter.clone()));
        Ok((vec![], 41))
    }
}
#[async_trait::async_trait]
impl AdminPageQuery for QuerySpy {
    async fn list(
        &self,
        filter: &ContentListFilter,
    ) -> Result<(Vec<AdminPageSummary>, i64), UseCaseError> {
        self.0.lock().unwrap().push((None, filter.clone()));
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
    let queries = ContentQueries::new(spy.clone(), spy.clone());
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
    let queries = ContentQueries::new(spy.clone(), spy.clone());
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
    let queries = ContentQueries::new(spy.clone(), spy.clone());
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
        assert_eq!((filter.limit(), filter.offset()), (20, 40));
        assert_eq!(filter.status(), Some("scheduled"));
        assert_eq!(filter.visibility(), Some("private"));
        assert!(filter.trash());
    }
}
