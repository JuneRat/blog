//! Deterministic generation; --check compares without rewriting the checked-in file.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/admin/src/api/generated.ts");
    let generated = interfaces::http_contract::typescript();
    if std::env::args().any(|arg| arg == "--check") {
        if std::fs::read_to_string(&target)? != generated {
            return Err("HTTP contract changed; run cargo run -p interfaces --example export_admin_contract".into());
        }
    } else {
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::write(target, generated)?;
    }
    Ok(())
}
