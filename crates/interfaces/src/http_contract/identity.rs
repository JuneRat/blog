use uuid::Uuid;
#[derive(serde::Serialize, ts_rs::TS)]
pub struct Profile {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub version: i64,
    pub avatar_media_id: Option<Uuid>,
    pub avatar_url: Option<String>,
}
impl From<application::identity::ProfileView> for Profile {
    fn from(value: application::identity::ProfileView) -> Self {
        let application::identity::ProfileView {
            user_id,
            username,
            display_name,
            bio,
            version,
            avatar_media_id,
            avatar_url,
        } = value;
        Self {
            user_id,
            username,
            display_name,
            bio,
            version,
            avatar_media_id,
            avatar_url,
        }
    }
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct AdminUser {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub status: &'static str,
    pub version: i64,
    pub deleted: bool,
    pub can_login: bool,
    pub is_last_loginable_owner: bool,
    pub password_enabled: bool,
    pub external_identities: i64,
    pub roles: Vec<String>,
}
impl From<application::identity::AdminUserDto> for AdminUser {
    fn from(value: application::identity::AdminUserDto) -> Self {
        let application::identity::AdminUserDto {
            id,
            username,
            email,
            display_name,
            status,
            version,
            deleted,
            can_login,
            is_last_loginable_owner,
            password_enabled,
            external_identities,
            roles,
        } = value;
        Self {
            id,
            username,
            email,
            display_name,
            status,
            version,
            deleted,
            can_login,
            is_last_loginable_owner,
            password_enabled,
            external_identities,
            roles,
        }
    }
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct CreatedUser {
    pub id: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub created_at: String,
}
impl From<application::identity::UserDto> for CreatedUser {
    fn from(value: application::identity::UserDto) -> Self {
        let application::identity::UserDto {
            id,
            username,
            display_name,
            created_at,
        } = value;
        Self {
            id,
            username,
            display_name,
            created_at: application::public_site::api_datetime(created_at),
        }
    }
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct UserStatusResult {
    pub id: Uuid,
    pub status: &'static str,
    pub version: i64,
}
impl From<application::identity::UserStatusView> for UserStatusResult {
    fn from(value: application::identity::UserStatusView) -> Self {
        let application::identity::UserStatusView {
            id,
            status,
            version,
        } = value;
        Self {
            id,
            status,
            version,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct Me {
    #[serde(flatten)]
    pub profile: Profile,
    pub time_zone: String,
    pub permissions: Vec<String>,
    pub csrf_token: String,
    pub channel: SessionChannel,
}
#[derive(serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum SessionChannel {
    Session,
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct PasswordLoginResult {
    pub user_id: Uuid,
    pub next: String,
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct PasswordChangeResult {
    pub user_id: Uuid,
    pub csrf_token: String,
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
}
impl From<application::auth::ProviderSummary> for ProviderSummary {
    fn from(value: application::auth::ProviderSummary) -> Self {
        let application::auth::ProviderSummary { id, name, kind } = value;
        Self { id, name, kind }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct RoleSummary {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub builtin: bool,
    pub permission_count: i64,
}
impl From<application::ports::RoleDto> for RoleSummary {
    fn from(value: application::ports::RoleDto) -> Self {
        let application::ports::RoleDto {
            slug,
            name,
            description,
            builtin,
            permission_count,
        } = value;
        Self {
            slug,
            name,
            description,
            builtin,
            permission_count,
        }
    }
}
