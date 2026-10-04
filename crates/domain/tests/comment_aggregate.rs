use domain::{
    comment::{
        Comment, CommentAuthor, CommentBody, CommentError, CommentNickname, CommentSnapshot,
        CommentStatus, CommentSubmission, ModerationAction, ReplyContext,
    },
    identity::{Email, UserId},
};
use uuid::Uuid;

fn context() -> CommentSubmission {
    CommentSubmission {
        post_id: Uuid::now_v7(),
        global_enabled: true,
        moderation: domain::comment::SubmissionDecision::ReviewAll,
        post_enabled: true,
        reply: None,
    }
}
fn guest() -> CommentAuthor {
    CommentAuthor::Guest {
        nickname: CommentNickname::new(" Visitor ").unwrap(),
        email: Some(Email::new("visitor@example.com").unwrap()),
    }
}
fn submit(context: CommentSubmission) -> Result<Comment, CommentError> {
    Comment::submit(
        context,
        guest(),
        CommentBody::new(" body\r\ntext ").unwrap(),
    )
}
fn with_status(status: CommentStatus) -> Comment {
    let mut snapshot = submit(context()).unwrap().snapshot();
    snapshot.status = status;
    snapshot.version = 7;
    Comment::reconstitute(snapshot).unwrap()
}

#[test]
fn all_review_policy_keeps_guest_and_account_comments_pending() {
    let context = context();
    let guest = submit(context).unwrap().snapshot();
    let user_id = UserId::generate();
    let account = Comment::submit(
        context,
        CommentAuthor::Account {
            user_id,
            nickname: CommentNickname::from_account(Some(" Owner\n "), "owner").unwrap(),
        },
        CommentBody::new("hello").unwrap(),
    )
    .unwrap()
    .snapshot();
    for comment in [&guest, &account] {
        assert_eq!(comment.post_id, context.post_id);
        assert_eq!(comment.status, CommentStatus::Pending);
        assert_eq!(comment.version, 1);
        assert_eq!((comment.parent_id, comment.root_id), (None, None));
    }
    assert_ne!(guest.id, account.id);
    assert_eq!(guest.nickname, "Visitor");
    assert_eq!(guest.body, "body\ntext");
    assert_eq!(guest.email.as_deref(), Some("visitor@example.com"));
    assert_eq!(guest.user_id, None);
    assert_eq!(account.nickname, "Owner");
    assert_eq!(account.user_id, Some(user_id.0));
    assert_eq!(account.email, None);
}

#[test]
fn submission_policy_distinguishes_guests_and_manually_approved_accounts() {
    use CommentStatus::{Approved, Pending};
    use domain::comment::{ModerationMode::*, SubmissionDecision::*};
    for (mode, account, approved_before, expected, status) in [
        (All, true, true, ReviewAll, Pending),
        (All, false, false, ReviewAll, Pending),
        (Guests, false, false, ReviewGuest, Pending),
        (Guests, true, false, RegisteredAccount, Approved),
        (FirstComment, true, false, ReviewFirstComment, Pending),
        (FirstComment, true, true, TrustedAccount, Approved),
        (FirstComment, false, true, ReviewGuest, Pending),
        (None, false, false, Unmoderated, Approved),
        (None, true, false, Unmoderated, Approved),
    ] {
        let decision = mode.decide(account, approved_before);
        assert_eq!(decision, expected);
        let comment = submit(CommentSubmission {
            moderation: decision,
            ..context()
        })
        .unwrap();
        assert_eq!(comment.status(), status);
    }
}

#[test]
fn either_closed_switch_rejects_submission_before_reply_validation() {
    let root = with_status(CommentStatus::Pending).reference();
    for (global_enabled, post_enabled) in [(false, true), (true, false), (false, false)] {
        let context = CommentSubmission {
            global_enabled,
            post_enabled,
            reply: Some(ReplyContext { parent: root, root }),
            ..context()
        };
        assert_eq!(submit(context).unwrap_err(), CommentError::Closed);
    }
}

#[test]
fn replies_follow_the_approved_parent_even_when_its_root_is_hidden() {
    let root = with_status(CommentStatus::Approved).reference();
    let mut context = CommentSubmission {
        post_id: root.post_id,
        reply: Some(ReplyContext { parent: root, root }),
        ..context()
    };
    let mut parent = submit(context).unwrap();
    assert_eq!(parent.snapshot().parent_id, Some(root.id));
    assert_eq!(parent.snapshot().root_id, Some(root.id));
    parent
        .moderate(1, ModerationAction::SetStatus(CommentStatus::Approved))
        .unwrap();
    for status in [
        CommentStatus::Pending,
        CommentStatus::Trash,
        CommentStatus::Spam,
    ] {
        context.reply = Some(ReplyContext {
            parent: parent.reference(),
            root: domain::comment::CommentReference { status, ..root },
        });
        let reply = submit(context).unwrap().snapshot();
        assert_eq!(reply.parent_id, Some(parent.snapshot().id));
        assert_eq!(reply.root_id, Some(root.id));
        assert_eq!(reply.status, CommentStatus::Pending);
    }
}

#[test]
fn replies_reject_hidden_parents_cross_post_links_and_invalid_roots() {
    let root = with_status(CommentStatus::Approved).reference();
    let valid = ReplyContext { parent: root, root };
    let mut invalid = Vec::new();
    for status in [
        CommentStatus::Pending,
        CommentStatus::Trash,
        CommentStatus::Spam,
    ] {
        let mut reply = valid;
        reply.parent.status = status;
        invalid.push(reply);
    }
    let mut reply = valid;
    reply.parent.post_id = Uuid::now_v7();
    invalid.push(reply);
    let mut reply = valid;
    reply.root.post_id = Uuid::now_v7();
    invalid.push(reply);
    let mut reply = valid;
    reply.root.id = Uuid::now_v7();
    invalid.push(reply);
    let mut reply = valid;
    reply.root.parent_id = Some(Uuid::now_v7());
    reply.root.root_id = Some(Uuid::now_v7());
    invalid.push(reply);
    let mut reply = valid;
    reply.parent.parent_id = Some(Uuid::now_v7());
    invalid.push(reply);
    let mut reply = valid;
    reply.parent.parent_id = Some(root.id);
    reply.parent.root_id = Some(root.id);
    invalid.push(reply);
    for reply in invalid {
        assert_eq!(
            submit(CommentSubmission {
                post_id: root.post_id,
                reply: Some(reply),
                ..context()
            })
            .unwrap_err(),
            CommentError::InvalidReply
        );
    }
}

#[test]
fn moderation_transition_matrix_preserves_identity_and_checks_versions_before_noops() {
    use CommentStatus::*;
    for current in [Pending, Approved, Trash, Spam] {
        for next in [Pending, Approved, Trash, Spam] {
            let mut comment = with_status(current);
            let original = comment.snapshot();
            let action = ModerationAction::SetStatus(next);
            for stale in [0, 6, 8] {
                assert_eq!(
                    comment.moderate(stale, action),
                    Err(CommentError::VersionConflict)
                );
                assert_eq!(comment.snapshot(), original);
            }
            if matches!(current, Trash | Spam) && next == Approved {
                assert_eq!(
                    comment.moderate(7, action),
                    Err(CommentError::RestoreBeforeApproval)
                );
                assert_eq!(comment.snapshot(), original);
                comment
                    .moderate(7, ModerationAction::SetStatus(Pending))
                    .unwrap();
                assert!(comment.moderate(7, action).unwrap());
            } else {
                assert_eq!(comment.moderate(7, action).unwrap(), current != next);
            }
            // The adapter increments the version only when committing a change.
            assert_eq!(
                comment.snapshot(),
                CommentSnapshot {
                    status: next,
                    ..original
                }
            );
        }
    }
}

#[test]
fn reconstitution_validates_without_normalizing_or_exposing_mutable_state() {
    let mut stored = with_status(CommentStatus::Pending).snapshot();
    stored.nickname = " Visitor ".into();
    stored.body = " body\ntext\t ".into();
    let mut comment = Comment::reconstitute(stored.clone()).unwrap();
    comment
        .moderate(7, ModerationAction::SetStatus(CommentStatus::Approved))
        .unwrap();
    stored.status = CommentStatus::Approved;
    assert_eq!(comment.snapshot(), stored);
    let mut detached = comment.snapshot();
    detached.body.clear();
    assert_eq!(comment.snapshot(), stored);

    let mut invalid = Vec::new();
    for version in [0, -1] {
        invalid.push(CommentSnapshot {
            version,
            ..stored.clone()
        });
    }
    invalid.push(CommentSnapshot {
        parent_id: Some(Uuid::now_v7()),
        ..stored.clone()
    });
    invalid.push(CommentSnapshot {
        root_id: Some(Uuid::now_v7()),
        ..stored.clone()
    });
    invalid.push(CommentSnapshot {
        parent_id: Some(stored.id),
        root_id: Some(Uuid::now_v7()),
        ..stored.clone()
    });
    invalid.push(CommentSnapshot {
        parent_id: Some(Uuid::now_v7()),
        root_id: Some(stored.id),
        ..stored.clone()
    });
    invalid.push(CommentSnapshot {
        nickname: "bad\nname".into(),
        ..stored.clone()
    });
    invalid.push(CommentSnapshot {
        body: " ".into(),
        ..stored.clone()
    });
    invalid.push(CommentSnapshot {
        email: Some("invalid".into()),
        ..stored.clone()
    });
    invalid.push(CommentSnapshot {
        user_id: Some(Uuid::now_v7()),
        ..stored
    });
    for snapshot in invalid {
        assert!(matches!(
            Comment::reconstitute(snapshot),
            Err(CommentError::InvalidSnapshot(_))
        ));
    }
}
