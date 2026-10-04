use interfaces::http_admin::{EditPostBody, UpdateCategoryBody, UpdateSeriesBody};
use interfaces::http_contract::{ContentPage, Profile, SubmitCommentBody, typescript};
use serde_json::json;
use uuid::Uuid;

#[test]
fn patch_absent_null_and_value_remain_distinct() {
    let id = Uuid::from_u128(1);
    let absent: EditPostBody = serde_json::from_value(json!({})).unwrap();
    assert_eq!(absent.category_id, None);
    assert_eq!(absent.cover_media_id, None);
    let cleared: EditPostBody =
        serde_json::from_value(json!({"category_id": null, "cover_media_id": null})).unwrap();
    assert_eq!(cleared.category_id, Some(None));
    assert_eq!(cleared.cover_media_id, Some(None));
    let set: EditPostBody =
        serde_json::from_value(json!({"category_id": id, "cover_media_id": id})).unwrap();
    assert_eq!(set.category_id, Some(Some(id)));
    assert_eq!(set.cover_media_id, Some(Some(id)));
    let series: UpdateSeriesBody = serde_json::from_value(json!({"name": "series"})).unwrap();
    assert_eq!(series.cover_media_id, None);
    let series: UpdateSeriesBody =
        serde_json::from_value(json!({"name": "series", "cover_media_id": null})).unwrap();
    assert_eq!(series.cover_media_id, Some(None));
    let series: UpdateSeriesBody =
        serde_json::from_value(json!({"name": "series", "cover_media_id": id})).unwrap();
    assert_eq!(series.cover_media_id, Some(Some(id)));
    let category: UpdateCategoryBody = serde_json::from_value(json!({"name": "category"})).unwrap();
    assert_eq!(category.parent, None);
    let category: UpdateCategoryBody =
        serde_json::from_value(json!({"name": "category", "parent": null})).unwrap();
    assert_eq!(category.parent, Some(None));
    let category: UpdateCategoryBody =
        serde_json::from_value(json!({"name": "category", "parent": "parent"})).unwrap();
    assert_eq!(category.parent, Some(Some("parent".into())));
    let declarations = typescript();
    assert!(declarations.contains("category_id?: string | null"));
    assert!(!declarations.contains("bigint"));
}

#[test]
fn patch_fields_keep_rejecting_invalid_values() {
    for field in ["category_id", "cover_media_id"] {
        assert!(serde_json::from_value::<EditPostBody>(json!({field: "not-a-uuid"})).is_err());
    }
    assert!(
        serde_json::from_value::<UpdateSeriesBody>(json!({
            "name": "series", "cover_media_id": "not-a-uuid"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<UpdateCategoryBody>(json!({"name": "category", "parent": 42}))
            .is_err()
    );
}

#[test]
fn response_uuids_numbers_and_nullable_fields_match_json() {
    let profile = Profile::from(application::identity::ProfileView {
        user_id: Uuid::from_u128(1),
        username: "writer".into(),
        display_name: None,
        bio: None,
        version: 7,
        avatar_media_id: None,
        avatar_url: None,
    });
    let value = serde_json::to_value(ContentPage {
        items: vec![profile],
        total: 1,
        page: 1,
        per_page: 20,
    })
    .unwrap();
    assert_eq!(
        value["items"][0]["user_id"],
        json!(Uuid::from_u128(1).to_string())
    );
    assert_eq!(value["items"][0]["version"], 7);
    assert_eq!(
        value["items"][0].get("avatar_media_id"),
        Some(&serde_json::Value::Null)
    );
}

#[test]
fn reply_transport_allows_missing_nickname_and_rejects_unknown_fields() {
    let reply: SubmitCommentBody = serde_json::from_value(json!({"body": "reply"})).unwrap();
    let command: application::comments::SubmitComment = reply.into();
    assert!(command.nickname.is_none());
    assert!(
        serde_json::from_value::<SubmitCommentBody>(json!({"body": "reply", "is_author": true}))
            .is_err()
    );
}

#[test]
fn series_placement_keeps_the_default_zero_position() {
    let request: EditPostBody = serde_json::from_value(json!({
        "series": [{"series_id": Uuid::from_u128(1)}]
    }))
    .unwrap();
    assert_eq!(request.series.unwrap()[0].position, 0);
}

#[test]
fn settings_commands_keep_rejecting_unknown_fields() {
    assert!(
        serde_json::from_value::<interfaces::http_contract::CommentPolicy>(
            json!({"enabled": true, "version": 1, "extra": true})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<interfaces::http_contract::RetentionSettings>(json!({"comment_ip_days": 180, "comment_version": 1, "audit_days": 180, "audit_version": 1, "extra": true})).is_err());
}

#[test]
fn task_transport_normalizes_instants_and_never_exposes_the_execution_lease() {
    use application::tasks::{TaskKind, TaskReport, TaskRun, TaskStatus, TaskTrigger};
    let run = TaskRun {
        id: Uuid::from_u128(42),
        kind: TaskKind::HtmlRebuild,
        status: TaskStatus::Queued,
        trigger: TaskTrigger::Once,
        run_at: time::macros::datetime!(2026-10-02 08:15 +08:00),
        created_at: time::macros::datetime!(2026-10-01 00:00 UTC),
        started_at: None,
        finished_at: None,
        retry_of: None,
        report: TaskReport::default(),
        can_retry: false,
        can_cancel: true,
    };
    let dto = interfaces::http_contract::TaskRun::try_from(run.clone()).unwrap();
    let value = serde_json::to_value(dto).unwrap();
    assert_eq!(value["run_at"], "2026-10-02T00:15:00Z");
    assert_eq!(value["status"], "queued");
    assert_eq!(value["trigger"], "once");
    assert_eq!(value["started_at"], serde_json::Value::Null);
    assert_eq!(value["report"]["html"], serde_json::Value::Null);
    assert!(!value.as_object().unwrap().contains_key("lease_token"));
    let invalid_time = TaskRun {
        created_at: time::macros::datetime!(-0001-01-01 00:00 UTC),
        ..run
    };
    assert!(matches!(
        interfaces::http_contract::TaskRun::try_from(invalid_time),
        Err(application::UseCaseError::DataCorrupt(_))
    ));
    assert!(
        serde_json::from_value::<interfaces::http_contract::TaskStartBody>(
            json!({"kind":"shell","command":"rebuild-html"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<interfaces::http_contract::TaskScheduleBody>(
            json!({"enabled":true,"interval_seconds":3600,"version":0,"actor_id":Uuid::nil()})
        )
        .is_err()
    );
}
