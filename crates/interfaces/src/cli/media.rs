//! 媒体暂存及物理清理命令。

use super::fmt_error;
use application::identity::Actor;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum MediaAction {
    /// 清理超期上传暂存文件，不删除正式图片或软删除记录
    CleanupStaging,
    /// 显式生成或执行正式媒体物理清理计划
    Purge {
        /// 仅重试旧 Python Docker 计划时指定原容器名；连接仍由当前 DATABASE_URL 决定
        #[arg(long)]
        legacy_container: Option<String>,
        #[command(subcommand)]
        action: MediaPurgeAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum MediaPurgeAction {
    /// 只读校验显式选中的回收站媒体，独占创建私有计划文件
    Plan {
        #[arg(long, required = true)]
        id: Vec<uuid::Uuid>,
        #[arg(long)]
        media_dir: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
    /// 停写并确认外链失效后执行；部分失败须使用原计划重试
    Apply {
        plan: PathBuf,
        #[arg(long)]
        maintenance_confirmed: bool,
        #[arg(long)]
        break_links_confirmed: bool,
    },
}

/// 独立维护入口，只清理暂存区；正常及软删除媒体均保留。
pub async fn run_media(
    media: &application::media::MediaInteractor,
    action: MediaAction,
) -> Result<(), String> {
    match action {
        MediaAction::Purge { .. } => Err("media purge requires its maintenance assembly".into()),
        MediaAction::CleanupStaging => {
            let removed = media
                .cleanup_staging(&Actor::bootstrap_cli())
                .await
                .map_err(fmt_error)?;
            println!("已清理超期暂存文件：{removed}；正式图片保持不变。");
            Ok(())
        }
    }
}

pub async fn run_media_purge(
    cleanup: &application::media_cleanup::MediaCleanup,
    action: MediaPurgeAction,
) -> Result<(), String> {
    match action {
        MediaPurgeAction::Plan { id, output, .. } => {
            let plan = cleanup.plan(id, &output).await.map_err(fmt_error)?;
            println!(
                "{}",
                serde_json::json!({"plan":output,"items":plan.items.len(),
                "bytes":plan.items.iter().map(|item| i128::from(item.size)).sum::<i128>(),
                "operation_id":plan.operation_id,
                "notice":"Review selected IDs and outside links before applying this plan."})
            );
            Ok(())
        }
        MediaPurgeAction::Apply {
            plan,
            maintenance_confirmed,
            break_links_confirmed,
        } => {
            let result = cleanup
                .apply(&plan, maintenance_confirmed, break_links_confirmed)
                .await
                .map_err(fmt_error)?;
            println!(
                "{}",
                serde_json::to_string(&result).map_err(|e| e.to_string())?
            );
            if result.failures.is_empty() {
                Ok(())
            } else {
                Err("部分文件清理失败，请保留原计划重试".into())
            }
        }
    }
}
