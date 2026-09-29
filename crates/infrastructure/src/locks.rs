//! PostgreSQL advisory-lock registry. Keys are cross-process protocol identifiers;
//! changes require a coordinated restart. Lock order: docs/locking.md.
pub(crate) const ACCESS_POLICY: (i32, i32) = (1129270605, 4);
pub(crate) const IDENTITY: (i32, i32) = (2048001, 1);
pub(crate) const SESSIONS: (i32, i32) = (2048002, 1);
pub(crate) const CATEGORY_TREE: (i32, i32) = (2048003, 1);
pub(crate) const CONTENT_RELATIONS: (i32, i32) = (1129270868, 1);
pub(crate) const COMMENT_POLICY: (i32, i32) = (1129270605, 1);
pub(crate) const AUDIT_POLICY: (i32, i32) = (1129270605, 2);
pub(crate) const RETENTION_CLEANUP: (i32, i32) = (1129270605, 3);

pub(crate) async fn acquire(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    key: (i32, i32),
    shared: bool,
) -> Result<(), sqlx::Error> {
    let query = if shared {
        "SELECT pg_advisory_xact_lock_shared($1::int,$2::int)"
    } else {
        "SELECT pg_advisory_xact_lock($1::int,$2::int)"
    };
    sqlx::query(query)
        .bind(key.0)
        .bind(key.1)
        .execute(executor)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_protocols_have_distinct_keys() {
        let keys = [
            ACCESS_POLICY,
            IDENTITY,
            SESSIONS,
            CATEGORY_TREE,
            CONTENT_RELATIONS,
            COMMENT_POLICY,
            AUDIT_POLICY,
            RETENTION_CLEANUP,
        ];
        assert_eq!(
            keys.into_iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            keys.len()
        );
    }
}
