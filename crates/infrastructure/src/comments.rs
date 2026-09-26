//! PostgreSQL comment operations. Visibility, scope and CAS are checked inside
//! the transaction; no comment operation increments posts.version.
use application::comments::*;
use application::error::UseCaseError;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

pub struct PostgresCommentRepository {
    pool: PgPool,
}
impl PostgresCommentRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
fn db(e: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(e.to_string())
}
fn missing() -> UseCaseError {
    UseCaseError::NotFound("文章或评论".into())
}
fn comment(row: PgRow) -> Comment {
    Comment {
        id: row.get("id"),
        post_id: row.get("post_id"),
        post_slug: row.get("post_slug"),
        post_title: row.get("post_title"),
        parent_id: row.get("parent_id"),
        nickname: row.get("nickname"),
        body: row.get("body"),
        is_author: row.get("is_author"),
        status: row.get("status"),
        version: row.get("version"),
        created_at: application::public_site::format_datetime(row.get("created_at")),
    }
}
const FIELDS: &str = "c.*, p.slug AS post_slug, p.title AS post_title, COALESCE(c.user_id=p.author_id,false) AS is_author";
const PUBLIC: &str = "p.status='published' AND p.visibility='public' AND p.deleted_at IS NULL";
#[async_trait]
impl CommentRepository for PostgresCommentRepository {
    async fn public_list(
        &self,
        slug: &str,
        parent: Option<Uuid>,
        page: i64,
    ) -> Result<CommentPage, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let post = sqlx::query(&format!("SELECT p.id, g.enabled AND COALESCE(s.enabled,true) AS enabled FROM posts p CROSS JOIN comment_settings g LEFT JOIN post_comment_settings s ON s.post_id=p.id WHERE p.slug=$1 AND {PUBLIC}"))
            .bind(slug).fetch_optional(&mut *tx).await.map_err(db)?.ok_or_else(missing)?;
        let id: Uuid = post.get("id");
        if let Some(parent) = parent {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM comments WHERE id=$1 AND post_id=$2 AND parent_id IS NULL AND status='approved')").bind(parent).bind(id).fetch_one(&mut *tx).await.map_err(db)?;
            if !exists {
                return Err(missing());
            }
        }
        let filter = "c.post_id=$1 AND c.parent_id IS NOT DISTINCT FROM $2 AND c.status='approved'";
        let total = sqlx::query_scalar(&format!("SELECT count(*) FROM comments c WHERE {filter}"))
            .bind(id)
            .bind(parent)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        let items = sqlx::query(&format!("SELECT {FIELDS} FROM comments c JOIN posts p ON p.id=c.post_id WHERE {filter} ORDER BY c.created_at,c.id LIMIT 20 OFFSET $3"))
            .bind(id).bind(parent).bind((page-1)*20).fetch_all(&mut *tx).await.map_err(db)?.into_iter().map(comment).collect();
        tx.commit().await.map_err(db)?;
        Ok(CommentPage {
            items,
            total,
            enabled: post.get("enabled"),
        })
    }
    async fn submit(&self, slug: &str, client: &str, cmd: NewComment) -> Result<(), UseCaseError> {
        let user = match &cmd.author {
            CommentAuthor::Account(id) => Some(*id),
            CommentAuthor::Guest(_) => None,
        };
        let guest_nickname = match &cmd.author {
            CommentAuthor::Guest(name) => Some(name.as_str()),
            _ => None,
        };
        let client = format!("{:x}", Sha256::digest(client.as_bytes()));
        let mut tx = self.pool.begin().await.map_err(db)?;
        // All instances share this per-client lock and database clock/rate window.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,913))")
            .bind(&client)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let enabled: bool =
            sqlx::query_scalar("SELECT enabled FROM comment_settings WHERE id=true FOR SHARE")
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        let post = sqlx::query(&format!(
            "SELECT p.id FROM posts p WHERE p.slug=$1 AND {PUBLIC} FOR SHARE"
        ))
        .bind(slug)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(missing)?;
        let id: Uuid = post.get("id");
        // A retry has no observable moderation information, even after rejection.
        if let Some(prior) = sqlx::query("SELECT post_id,parent_id,user_id,client_hash,body,nickname FROM comments WHERE request_id=$1")
            .bind(cmd.request_id).fetch_optional(&mut *tx).await.map_err(db)? {
            if prior.get::<Uuid,_>("post_id") == id && prior.get::<Option<Uuid>,_>("parent_id") == cmd.parent_id
                && prior.get::<Option<Uuid>,_>("user_id") == user && prior.get::<String,_>("client_hash") == client
                && prior.get::<String,_>("body") == cmd.body.as_str() && (user.is_some() || Some(prior.get::<String,_>("nickname").as_str()) == guest_nickname) {
                return Ok(());
            }
            return Err(UseCaseError::Invalid("重复请求标识与原评论不一致".into()));
        }
        let post_enabled: Option<bool> =
            sqlx::query_scalar("SELECT enabled FROM post_comment_settings WHERE post_id=$1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        if !enabled || !post_enabled.unwrap_or(true) {
            return Err(UseCaseError::Invalid("新评论已关闭".into()));
        }
        if let Some(parent) = cmd.parent_id
            && sqlx::query("SELECT id FROM comments WHERE id=$1 AND post_id=$2 AND parent_id IS NULL AND status='approved' FOR UPDATE")
                .bind(parent).bind(id).fetch_optional(&mut *tx).await.map_err(db)?.is_none() { return Err(missing()); }
        let recent: i64 = sqlx::query_scalar("SELECT count(*) FROM comments WHERE client_hash=$1 AND created_at > now()-interval '1 minute'")
            .bind(&client).fetch_one(&mut *tx).await.map_err(db)?;
        if recent >= 3 {
            return Err(UseCaseError::RateLimited {
                retry_after_secs: 60,
            });
        }
        let duplicate: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM comments WHERE client_hash=$1 AND post_id=$2 AND parent_id IS NOT DISTINCT FROM $3 AND body=$4 AND created_at > now()-interval '10 minutes')")
            .bind(&client).bind(id).bind(cmd.parent_id).bind(cmd.body.as_str()).fetch_one(&mut *tx).await.map_err(db)?;
        if duplicate {
            return Err(UseCaseError::Invalid("请勿重复提交相同评论".into()));
        }
        let nickname = match cmd.author {
            CommentAuthor::Account(user) => {
                let account = sqlx::query(
                    "SELECT display_name, username FROM users WHERE id=$1 AND deleted_at IS NULL",
                )
                .bind(user)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
                .ok_or(UseCaseError::Unauthenticated)?;
                CommentNickname::from_account(
                    account.get::<Option<&str>, _>("display_name"),
                    account.get("username"),
                )
                .map_err(|e| UseCaseError::Invalid(e.into()))?
            }
            CommentAuthor::Guest(name) => name,
        };
        sqlx::query("INSERT INTO comments(id,post_id,parent_id,user_id,nickname,body,request_id,client_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(Uuid::now_v7()).bind(id).bind(cmd.parent_id).bind(user).bind(nickname.as_str()).bind(cmd.body.as_str()).bind(cmd.request_id).bind(client)
            .execute(&mut *tx).await.map_err(|e| if e.as_database_error().is_some_and(|e| e.is_unique_violation()) { UseCaseError::Invalid("请求标识已使用".into()) } else { db(e) })?;
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
        let items = sqlx::query(&format!("SELECT {FIELDS} FROM comments c JOIN posts p ON p.id=c.post_id WHERE {filter} ORDER BY c.created_at DESC,c.id DESC LIMIT 20 OFFSET $5"))
            .bind(scope.all).bind(scope.user_id).bind(status.map(CommentStatus::as_str)).bind(post).bind((page-1)*20).fetch_all(&mut *tx).await.map_err(db)?.into_iter().map(comment).collect();
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
        // Lock the post before its comments (same lock order as submit).
        let post = sqlx::query("SELECT p.author_id FROM posts p JOIN comments c ON c.post_id=p.id WHERE c.id=$1 FOR SHARE OF p")
            .bind(id).fetch_optional(&mut *tx).await.map_err(db)?.ok_or_else(missing)?;
        if !scope.all && post.get::<Uuid, _>("author_id") != scope.user_id {
            return Err(UseCaseError::Forbidden);
        }
        let changed = match action {
            ModerationAction::SetStatus(status) => {
                sqlx::query("UPDATE comments SET status=$3,version=version+1,updated_at=now() WHERE id=$1 AND version=$2")
                    .bind(id)
                    .bind(version)
                    .bind(status.as_str())
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?
                    .rows_affected()
            }
            ModerationAction::DeletePermanently => {
                sqlx::query("DELETE FROM comments WHERE id=$1 AND version=$2")
                    .bind(id)
                    .bind(version)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?
                    .rows_affected()
            }
        };
        if changed == 0 {
            return Err(UseCaseError::VersionConflict);
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
        let current = if let Some(post) = post {
            let row = sqlx::query("SELECT author_id FROM posts WHERE id=$1 FOR UPDATE")
                .bind(post)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
                .ok_or_else(missing)?;
            if !scope.all && row.get::<Uuid, _>("author_id") != scope.user_id {
                return Err(UseCaseError::Forbidden);
            }
            sqlx::query("SELECT enabled,version FROM post_comment_settings WHERE post_id=$1")
                .bind(post)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
                .map(|r| CommentPolicy {
                    enabled: r.get("enabled"),
                    version: r.get("version"),
                })
                .unwrap_or(CommentPolicy {
                    enabled: true,
                    version: 0,
                })
        } else {
            let row = sqlx::query(
                "SELECT enabled,version FROM comment_settings WHERE id=true FOR UPDATE",
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
            CommentPolicy {
                enabled: row.get("enabled"),
                version: row.get("version"),
            }
        };
        let result = if let Some(update) = update {
            if update.version != current.version {
                return Err(UseCaseError::VersionConflict);
            }
            if let Some(post) = post {
                sqlx::query("INSERT INTO post_comment_settings(post_id,enabled) VALUES($1,$2) ON CONFLICT(post_id) DO UPDATE SET enabled=$2,version=post_comment_settings.version+1")
                    .bind(post).bind(update.enabled).execute(&mut *tx).await.map_err(db)?;
            } else {
                sqlx::query(
                    "UPDATE comment_settings SET enabled=$1,version=version+1 WHERE id=true",
                )
                .bind(update.enabled)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
            CommentPolicy {
                enabled: update.enabled,
                version: current.version + 1,
            }
        } else {
            current
        };
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
}
