//! Versioned deployment contract shared with installation and Python recovery.
//! Read beside the migration files so a release cannot silently mix inventories.
use std::{collections::BTreeMap, path::Path};

use application::UseCaseError;
use serde::Deserialize;
use sha2::{Digest, Sha384};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaContract {
    format: u32,
    id: String,
    migrations: Vec<Migration>,
    tables: BTreeMap<String, Policy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Migration {
    version: i64,
    file: String,
    checksum: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    app: Vec<String>,
    maintenance: Vec<String>,
}

fn invalid() -> UseCaseError {
    UseCaseError::Repository(
        "schema.json 结构清单与迁移不匹配；请使用同一发布版本的迁移与清单".into(),
    )
}

fn identifier(value: &str) -> bool {
    value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

impl SchemaContract {
    pub fn load(directory: impl AsRef<Path>) -> Result<Self, UseCaseError> {
        let directory = directory.as_ref();
        let contract: Self = serde_json::from_slice(
            &std::fs::read(directory.join("schema.json")).map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?;
        if contract.format != 1
            || contract.id.is_empty()
            || contract.tables.is_empty()
            || contract.migrations.is_empty()
            || contract.tables.keys().any(|table| !identifier(table))
        {
            return Err(invalid());
        }
        // Policies are executed only by the generated owner-run grant script.
        // Still require explicit lists for both roles in every table entry.
        for policy in contract.tables.values() {
            for grant in policy.app.iter().chain(&policy.maintenance) {
                if grant.is_empty() {
                    return Err(invalid());
                }
            }
        }
        let mut files = std::fs::read_dir(directory)
            .map_err(|_| invalid())?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid())?;
        files.retain(|path| path.extension().is_some_and(|extension| extension == "sql"));
        let mut expected_files = Vec::new();
        let mut previous = 0;
        for migration in &contract.migrations {
            let Some((version, description)) = migration.file.split_once('_') else {
                return Err(invalid());
            };
            if migration.version <= previous
                || !version.bytes().all(|c| c.is_ascii_digit())
                || version.parse::<i64>().ok() != Some(migration.version)
                || !description.strip_suffix(".sql").is_some_and(identifier)
            {
                return Err(invalid());
            }
            let path = directory.join(&migration.file);
            if path.is_symlink() {
                return Err(invalid());
            }
            let checksum = format!(
                "{:x}",
                Sha384::digest(std::fs::read(&path).map_err(|_| invalid())?)
            );
            if checksum != migration.checksum {
                return Err(invalid());
            }
            previous = migration.version;
            expected_files.push(path);
        }
        files.sort();
        expected_files.sort();
        if files != expected_files {
            return Err(invalid());
        }
        Ok(contract)
    }

    pub fn tables(&self) -> impl Iterator<Item = &str> {
        self.tables.keys().map(String::as_str)
    }

    pub fn contains(&self, table: &str) -> bool {
        self.tables.contains_key(table)
    }

    pub fn lock_tables_sql(&self) -> String {
        self.tables()
            .map(|name| format!("public.\"{name}\""))
            .collect::<Vec<_>>()
            .join(",")
    }
}
