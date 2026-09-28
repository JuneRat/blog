use super::DeploymentConfig;
use interfaces::cli::ConfigAction;

/// Configuration diagnostics are read-only and need no database connection.
pub fn run_command(config: &DeploymentConfig, action: ConfigAction) -> Result<(), String> {
    match action {
        ConfigAction::Check { scope } => {
            config.check(scope)?;
            println!("配置检查通过（未连接数据库、未修改文件）。");
        }
        ConfigAction::Show { scope, sources } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&config.show(scope, sources)?)
                    .map_err(|_| "无法输出配置")?
            );
        }
    }
    Ok(())
}
