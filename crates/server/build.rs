fn main() {
    println!("cargo:rerun-if-env-changed=BLOG_BUILD_REVISION");
    let revision = std::env::var("BLOG_BUILD_REVISION").unwrap_or_else(|_| "unknown".into());
    assert!(
        revision == "unknown"
            || ((7..=64).contains(&revision.len())
                && revision.bytes().all(|byte| byte.is_ascii_hexdigit())),
        "BLOG_BUILD_REVISION must be a Git hash or unknown"
    );
    println!("cargo:rustc-env=BLOG_BUILD_REVISION={revision}");
}
