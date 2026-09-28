//! Comment relationships are immutable; moderation only changes one node.
use crate::{
    COMMENT_RENDER_VERSION,
    audit::{AuditEntry, append_audit_log},
};
use application::{comments::*, error::UseCaseError, ports::CommentRenderer};
use async_trait::async_trait;
use domain::{
    comment::{
        Comment, CommentAuthor as ResolvedCommentAuthor, CommentError, CommentReference,
        CommentSnapshot, CommentSubmission, ReplyContext,
    },
    identity::UserId,
};
use serde_json::json;
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use std::{net::IpAddr, sync::Arc};
use uuid::Uuid;

pub struct PostgresCommentRepository {
    pool: PgPool,
    renderer: Arc<dyn CommentRenderer>,
}
impl PostgresCommentRepository {
    pub fn new(database: crate::Database, renderer: Arc<dyn CommentRenderer>) -> Self {
        let pool = database.pool;
        Self { pool, renderer }
    }
}
fn db(e: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(e.to_string())
}
fn missing() -> UseCaseError {
    UseCaseError::NotFound("文章或评论".into())
}
fn domain_error(error: CommentError) -> UseCaseError {
    match error {
        CommentError::Closed | CommentError::RestoreBeforeApproval => {
            UseCaseError::Invalid(error.to_string())
        }
        CommentError::InvalidReply => missing(),
        CommentError::VersionConflict => UseCaseError::VersionConflict,
        CommentError::InvalidSnapshot(_) => UseCaseError::Repository(error.to_string()),
    }
}
fn status(row: &PgRow) -> Result<CommentStatus, UseCaseError> {
    CommentStatus::parse(row.get("status")).map_err(|e| UseCaseError::Repository(e.into()))
}
fn reference(row: &PgRow) -> Result<CommentReference, UseCaseError> {
    Ok(CommentReference {
        id: row.get("id"),
        post_id: row.get("post_id"),
        parent_id: row.get("parent_id"),
        root_id: row.get("root_id"),
        status: status(row)?,
    })
}
fn aggregate(row: &PgRow) -> Result<Comment, UseCaseError> {
    Comment::reconstitute(CommentSnapshot {
        id: row.get("id"),
        post_id: row.get("post_id"),
        parent_id: row.get("parent_id"),
        root_id: row.get("root_id"),
        user_id: row.get("user_id"),
        nickname: row.get("author_name"),
        email: row.get("author_email"),
        body: row.get("content"),
        status: status(row)?,
        version: row.get("version"),
    })
    .map_err(domain_error)
}
const PUBLIC: &str = "p.status='published' AND p.visibility='public' AND p.deleted_at IS NULL AND p.published_at<=now()";
// Include only ancestors needed to connect approved descendants. A hidden node's
// identity and text are never exposed, even when it connects a public thread.
const VISIBLE: &str = "WITH RECURSIVE visible(id) AS (SELECT id FROM comments WHERE post_id=$1 AND status='approved' UNION SELECT c.parent_id FROM comments c JOIN visible v ON v.id=c.id WHERE c.post_id=$1 AND c.parent_id IS NOT NULL)";

async fn global_policy(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(CommentPolicy, serde_json::Value), UseCaseError> {
    let row = sqlx::query("SELECT value,version FROM settings WHERE key='comments'")
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?;
    let (value, version) = row
        .map(|r| (r.get::<serde_json::Value, _>("value"), r.get("version")))
        .unwrap_or((json!({}), 0));
    let enabled = match value.get("enabled") {
        None => true,
        Some(v) => v
            .as_bool()
            .ok_or_else(|| UseCaseError::Repository("comments.enabled 不是布尔值".into()))?,
    };
    Ok((CommentPolicy { enabled, version }, value))
}
fn public_comment(r: PgRow) -> PublicComment {
    let status: &str = r.get("status");
    let placeholder = status != "approved";
    PublicComment {
        id: r.get("id"),
        parent_id: r.get("parent_id"),
        root_id: r.get("root_id"),
        parent_nickname: r.get("parent_nickname"),
        nickname: if placeholder {
            String::new()
        } else {
            r.get("author_name")
        },
        content_html: if placeholder {
            String::new()
        } else {
            r.get("content_html")
        },
        is_author: !placeholder && r.get::<bool, _>("is_author"),
        placeholder,
        deleted: status == "trash",
        created_at: application::public_site::format_datetime(r.get("created_at")),
    }
}
fn comment(r: PgRow) -> Result<CommentDto, UseCaseError> {
    Ok(CommentDto {
        id: r.get("id"),
        post_id: r.get("post_id"),
        post_slug: r.get("post_slug"),
        post_title: r.get("post_title"),
        parent_id: r.get("parent_id"),
        root_id: r.get("root_id"),
        parent_nickname: r.get("parent_nickname"),
        nickname: r.get("author_name"),
        body: r.get("content"),
        content_html: r.get("content_html"),
        author_email: r.get("author_email"),
        ip_address: r.get("ip_address"),
        is_author: r.get("is_author"),
        status: status(&r)?,
        version: r.get("version"),
        created_at: application::public_site::format_datetime(r.get("created_at")),
    })
}
#[async_trait]
impl CommentRepository for PostgresCommentRepository {
    async fn public_list(
        &self,
        slug: &str,
        root: Option<Uuid>,
        page: i64,
    ) -> Result<PublicCommentPage, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let post = sqlx::query(&format!(
            "SELECT p.id,p.comments_enabled FROM posts p WHERE p.slug=$1 AND {PUBLIC}"
        ))
        .bind(slug)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(missing)?;
        let post_id: Uuid = post.get("id");
        let enabled =
            global_policy(&mut tx).await?.0.enabled && post.get::<bool, _>("comments_enabled");
        if let Some(root) = root {
            let exists: bool = sqlx::query_scalar(&format!("{VISIBLE} SELECT EXISTS(SELECT 1 FROM comments c JOIN visible v ON c.id=v.id WHERE c.id=$2 AND c.post_id=$1 AND c.parent_id IS NULL)"))
                .bind(post_id).bind(root).fetch_one(&mut *tx).await.map_err(db)?;
            if !exists {
                return Err(missing());
            }
        }
        let filter = "c.post_id=$1 AND c.root_id IS NOT DISTINCT FROM $2::uuid";
        let total = sqlx::query_scalar(&format!(
            "{VISIBLE} SELECT count(*) FROM comments c JOIN visible v ON v.id=c.id WHERE {filter}"
        ))
        .bind(post_id)
        .bind(root)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        let items = sqlx::query(&format!("{VISIBLE} SELECT c.id,c.parent_id,c.root_id,c.author_name,c.content_html,c.status,c.created_at,COALESCE(c.user_id=p.author_id,false) AS is_author,CASE WHEN parent.status='approved' THEN parent.author_name END AS parent_nickname FROM comments c JOIN visible v ON v.id=c.id JOIN posts p ON p.id=c.post_id LEFT JOIN comments parent ON parent.id=c.parent_id WHERE {filter} ORDER BY c.created_at,c.id LIMIT 20 OFFSET $3"))
            .bind(post_id).bind(root).bind((page-1)*20).fetch_all(&mut *tx).await.map_err(db)?.into_iter().map(public_comment).collect();
        tx.commit().await.map_err(db)?;
        Ok(PublicCommentPage {
            items,
            total,
            enabled,
        })
    }
    async fn submit(
        &self,
        slug: &str,
        client: Option<IpAddr>,
        cmd: NewComment,
    ) -> Result<(), UseCaseError> {
        let html = self.renderer.render_comment(cmd.body.as_str()).await?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        // Shared lock also protects the default policy when no settings row exists.
        sqlx::query("SELECT pg_advisory_xact_lock_shared(1129270605,1)")
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let global_enabled = global_policy(&mut tx).await?.0.enabled;
        let post = sqlx::query(&format!(
            "SELECT p.id,p.comments_enabled FROM posts p WHERE p.slug=$1 AND {PUBLIC} FOR SHARE"
        ))
        .bind(slug)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(missing)?;
        let post_id: Uuid = post.get("id");
        let mut context = CommentSubmission {
            post_id,
            global_enabled,
            post_enabled: post.get("comments_enabled"),
            reply: None,
        };
        context.ensure_open().map_err(domain_error)?;
        if let Some(parent) = cmd.parent_id {
            let row = sqlx::query("SELECT id,post_id,parent_id,root_id,status FROM comments WHERE id=$1 AND post_id=$2 FOR SHARE")
                .bind(parent).bind(post_id).fetch_optional(&mut *tx).await.map_err(db)?.ok_or_else(missing)?;
            let parent = reference(&row)?;
            let root = if let Some(root_id) = parent.root_id {
                // Relationships are immutable; the parent's FK retains the root.
                // Its moderation status does not constrain an approved descendant.
                let row = sqlx::query("SELECT id,post_id,parent_id,root_id,status FROM comments WHERE id=$1 AND post_id=$2")
                    .bind(root_id).bind(post_id).fetch_optional(&mut *tx).await.map_err(db)?.ok_or_else(missing)?;
                reference(&row)?
            } else {
                parent
            };
            context.reply = Some(ReplyContext { parent, root });
        }
        let author = match cmd.author {
            CommentAuthor::Account(id) => {
                let account = sqlx::query("SELECT display_name,username FROM users WHERE id=$1 AND status='active' AND deleted_at IS NULL FOR SHARE")
                    .bind(id).fetch_optional(&mut *tx).await.map_err(db)?.ok_or(UseCaseError::Unauthenticated)?;
                ResolvedCommentAuthor::Account {
                    user_id: UserId(id),
                    nickname: CommentNickname::from_account(
                        account.get::<Option<&str>, _>("display_name"),
                        account.get("username"),
                    )
                    .map_err(|e| UseCaseError::Invalid(e.into()))?,
                }
            }
            CommentAuthor::Guest(nickname) => ResolvedCommentAuthor::Guest {
                nickname,
                email: cmd.email,
            },
        };
        let comment = Comment::submit(context, author, cmd.body)
            .map_err(domain_error)?
            .snapshot();
        sqlx::query("INSERT INTO comments(id,post_id,parent_id,root_id,user_id,author_name,author_email,ip_address,content,content_html,content_render_version,status,version) VALUES($1,$2,$3,$4,$5,$6,$7,$8::text::inet,$9,$10,$11,$12,$13)")
            .bind(comment.id).bind(comment.post_id).bind(comment.parent_id).bind(comment.root_id).bind(comment.user_id).bind(&comment.nickname)
            .bind(&comment.email).bind(client.map(|v|v.to_string())).bind(&comment.body).bind(html).bind(COMMENT_RENDER_VERSION)
            .bind(comment.status.as_str()).bind(comment.version)
            .execute(&mut *tx).await.map_err(db)?;
        append_audit_log(&mut tx, AuditEntry {actor_id:comment.user_id, ip_address:client, action:"comment.create", target_type:"comment", target_id:&comment.id.to_string(), metadata:json!({"post_id":comment.post_id,"parent_id":comment.parent_id,"root_id":comment.root_id,"version":comment.version})}).await?;
        tx.commit().await.map_err(db)
    }
    async fn list(
        &self,
        scope: CommentScope,
        status: Option<CommentStatus>,
        post: Option<Uuid>,
        page: i64,
    ) -> Result<CommentPage, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let filter = "($1 OR p.author_id=$2) AND ($3::text IS NULL OR c.status=$3) AND ($4::uuid IS NULL OR c.post_id=$4)";
        let total = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM comments c JOIN posts p ON p.id=c.post_id WHERE {filter}"
        ))
        .bind(scope.all)
        .bind(scope.user_id)
        .bind(status.map(CommentStatus::as_str))
        .bind(post)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        let items = sqlx::query(&format!("SELECT c.id,c.post_id,c.parent_id,c.root_id,c.author_name,c.author_email,host(c.ip_address) AS ip_address,c.content,c.content_html,c.status,c.version,c.created_at,p.slug AS post_slug,p.title AS post_title,COALESCE(c.user_id=p.author_id,false) AS is_author,parent.author_name AS parent_nickname FROM comments c JOIN posts p ON p.id=c.post_id LEFT JOIN comments parent ON parent.id=c.parent_id WHERE {filter} ORDER BY c.created_at DESC,c.id DESC LIMIT 20 OFFSET $5"))
            .bind(scope.all).bind(scope.user_id).bind(status.map(CommentStatus::as_str)).bind(post).bind((page-1)*20).fetch_all(&mut *tx).await.map_err(db)?.into_iter().map(comment).collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(db)?;
        Ok(CommentPage {
            items,
            total,
            enabled: true,
        })
    }
    async fn moderate(
        &self,
        scope: CommentScope,
        id: Uuid,
        version: i64,
        action: ModerationAction,
    ) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let post = sqlx::query("SELECT p.author_id FROM posts p JOIN comments c ON c.post_id=p.id WHERE c.id=$1 FOR SHARE OF p")
            .bind(id).fetch_optional(&mut *tx).await.map_err(db)?.ok_or_else(missing)?;
        if !scope.all && post.get::<Uuid, _>("author_id") != scope.user_id {
            return Err(UseCaseError::Forbidden);
        }
        let row = sqlx::query("SELECT id,post_id,parent_id,root_id,user_id,author_name,author_email,content,status,version FROM comments WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?
            .ok_or_else(missing)?;
        let mut comment = aggregate(&row)?;
        let current = comment.status();
        if comment.moderate(version, action).map_err(domain_error)? {
            let next = comment.status();
            let result = sqlx::query(
                "UPDATE comments SET status=$2,version=version+1,updated_at=now() WHERE id=$1 AND version=$3",
            )
            .bind(id)
            .bind(next.as_str())
            .bind(version)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            if result.rows_affected() != 1 {
                return Err(UseCaseError::VersionConflict);
            }
            append_audit_log(&mut tx, AuditEntry { actor_id:Some(scope.user_id), ip_address:scope.ip_address, action:"comment.moderate", target_type:"comment", target_id:&id.to_string(), metadata:json!({"from":current.as_str(),"to":next.as_str(),"version":version+1}) }).await?;
        }
        tx.commit().await.map_err(db)
    }
    async fn policy(
        &self,
        scope: CommentScope,
        post: Option<Uuid>,
        update: Option<CommentPolicy>,
    ) -> Result<CommentPolicy, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let (current, mut value) = if let Some(post) = post {
            let row = sqlx::query("SELECT author_id,comments_enabled,version FROM posts WHERE id=$1 AND deleted_at IS NULL FOR UPDATE")
                .bind(post).fetch_optional(&mut *tx).await.map_err(db)?.ok_or_else(missing)?;
            if !scope.all && row.get::<Uuid, _>("author_id") != scope.user_id {
                return Err(UseCaseError::Forbidden);
            }
            (
                CommentPolicy {
                    enabled: row.get("comments_enabled"),
                    version: row.get("version"),
                },
                json!({}),
            )
        } else {
            sqlx::query("SELECT pg_advisory_xact_lock(1129270605,1)")
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            global_policy(&mut tx).await?
        };
        let result = if let Some(update) = update {
            if update.version != current.version {
                return Err(UseCaseError::VersionConflict);
            }
            if current.enabled == update.enabled {
                current
            } else {
                if let Some(post) = post {
                    sqlx::query("UPDATE posts SET comments_enabled=$2,version=version+1,updated_at=now() WHERE id=$1")
                        .bind(post).bind(update.enabled).execute(&mut *tx).await.map_err(db)?;
                } else {
                    value["enabled"] = json!(update.enabled);
                    sqlx::query("INSERT INTO settings(key,value) VALUES('comments',$1) ON CONFLICT(key) DO UPDATE SET value=$1,version=settings.version+1,updated_at=now()")
                        .bind(value).execute(&mut *tx).await.map_err(db)?;
                }
                append_audit_log(
                    &mut tx,
                    AuditEntry {
                        actor_id: Some(scope.user_id),
                        ip_address: scope.ip_address,
                        action: if post.is_some() {
                            "post.comment_policy"
                        } else {
                            "settings.comments"
                        },
                        target_type: if post.is_some() { "post" } else { "settings" },
                        target_id: &post
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "comments".into()),
                        metadata: json!({"enabled":update.enabled,"version":current.version+1}),
                    },
                )
                .await?;
                CommentPolicy {
                    enabled: update.enabled,
                    version: current.version + 1,
                }
            }
        } else {
            current
        };
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
}
