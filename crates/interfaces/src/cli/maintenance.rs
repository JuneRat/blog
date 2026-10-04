//! 保留期清理、预约发布和 HTML 重建入口。

/// CLI 只负责把维护输入交给用例并呈现结果。
pub async fn run_maintenance(
    maintenance: &application::retention::RetentionMaintenance,
    batch_size: i64,
    max_batches: u32,
    dry_run: bool,
) -> Result<(), String> {
    let result = maintenance
        .run(batch_size, max_batches, dry_run)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string(&result).map_err(|e| e.to_string())?
    );
    Ok(())
}

pub async fn run_publish_due(
    publisher: &application::publishing::PublishDueInteractor,
) -> Result<(), String> {
    let total = publisher.run().await.map_err(|e| e.to_string())?;
    println!("已发布 {total} 条到期内容。");
    Ok(())
}

pub async fn run_html_rebuild(
    rebuilder: &application::html_rebuild::HtmlRebuildInteractor,
    options: application::html_rebuild::RebuildOptions,
) -> Result<(), String> {
    match rebuilder.run(options).await {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string(&report).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Err(error) => {
            // 部分完成结果仍输出 JSON；stderr 与退出码同时明确标记失败。
            println!(
                "{}",
                serde_json::to_string(&error.0).map_err(|e| e.to_string())?
            );
            Err(error.to_string())
        }
    }
}
