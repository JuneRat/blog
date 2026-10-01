//! Theme lifecycle through authenticated HTTP, real storage and public rendering.
use super::*;

fn custom_theme_zip(index: &str) -> Vec<u8> {
    use std::io::{Cursor, Write};
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut files = vec![
        ("theme.json".to_string(), serde_json::json!({"schema_version":1,"slug":"custom","name":"Custom","theme_api_version":1,"required_functions":[]}).to_string()),
        ("assets/style.css".to_string(), "custom css".to_string()),
    ];
    for entry in ["index", "post", "page", "tag", "category", "series"] {
        files.push((
            format!("templates/{entry}.html"),
            if entry == "index" {
                index.into()
            } else {
                "custom page".into()
            },
        ));
    }
    for (name, body) in files {
        zip.start_file(
            format!("custom/{name}"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn deletion(version: i64, release: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"expected_version":version,"expected_release":release}))
        .unwrap()
}

async fn theme_request(
    stack: &Stack,
    cookie: Option<&str>,
    csrf: Option<&str>,
    method: &str,
    path: &str,
    body: Vec<u8>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:18099")
        .header("content-type", "application/json")
        .header("origin", "http://127.0.0.1:18099");
    if let Some(cookie) = cookie {
        request = request.header("cookie", format!("blog_session={cookie}"));
    }
    if let Some(csrf) = csrf {
        request = request.header("x-csrf-token", csrf);
    }
    let response = stack
        .router
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.headers().contains_key("x-request-id"));
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn theme_management_http_lifecycle_permissions_assets_and_races() {
    let _serial = SERIAL.lock().await;
    let stack = fresh_stack_with_themes(true).await;
    let package = custom_theme_zip(
        "<p>custom {{ site.title }}</p><link href=\"{{ asset_url(path='style.css') | url }}\">",
    );
    assert_eq!(
        theme_request(
            &stack,
            None,
            None,
            "POST",
            "/api/admin/v1/themes",
            package.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (author, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;
    for path in [
        "/api/admin/v1/themes",
        "/api/admin/v1/themes/validate-package",
        "/api/admin/v1/themes/default/validate",
    ] {
        assert_eq!(
            theme_request(
                &stack,
                Some(&author),
                Some(&author_csrf),
                "POST",
                path,
                package.clone()
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let (owner, csrf) = login_as(&stack.router, &stack.idp, "owner").await;
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            None,
            "POST",
            "/api/admin/v1/themes",
            package.clone()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "POST",
            "/api/admin/v1/themes",
            b"broken".to_vec()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "POST",
            "/api/admin/v1/themes",
            vec![0; application::themes::MAX_THEME_PACKAGE_BYTES + 1]
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let (status, validated) = theme_request(
        &stack,
        Some(&owner),
        Some(&csrf),
        "POST",
        "/api/admin/v1/themes/validate-package",
        package.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(validated["slug"], "custom");
    let root = &stack.theme_root.as_ref().unwrap().0;
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 1);
    let (status, installed) = theme_request(
        &stack,
        Some(&owner),
        Some(&csrf),
        "POST",
        "/api/admin/v1/themes",
        package.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(installed["release"], validated["release"]);
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "POST",
            "/api/admin/v1/themes",
            package.clone()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let view = theme_request(
        &stack,
        Some(&owner),
        None,
        "GET",
        "/api/admin/v1/settings/theme",
        vec![],
    )
    .await
    .1;
    assert_eq!(view["effective_slug"], "default");
    assert_eq!(view["fallback_slug"], "default");
    assert!(
        view["available"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["slug"] == "custom")
    );
    let activation = br#"{"slug":"custom","expected_version":0}"#.to_vec();
    let (status, active) = theme_request(
        &stack,
        Some(&owner),
        Some(&csrf),
        "PUT",
        "/api/admin/v1/settings/theme",
        activation.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(active["effective_slug"], "custom");
    assert_eq!(active["version"], 1);
    let response = stack
        .router
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("custom 测试站点"));
    let asset_url = format!(
        "/assets/custom/{}/style.css",
        installed["release"].as_str().unwrap()
    );
    assert!(html.contains(&asset_url));
    std::fs::write(root.join("custom/assets/style.css"), "changed disk bytes").unwrap();
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&asset_url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["cache-control"],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "custom css"
    );
    let (status, checked) = theme_request(
        &stack,
        Some(&owner),
        Some(&csrf),
        "POST",
        "/api/admin/v1/themes/custom/validate",
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(checked["release"], installed["release"]);
    for slug in ["custom", "default"] {
        assert_eq!(
            theme_request(
                &stack,
                Some(&owner),
                Some(&csrf),
                "DELETE",
                &format!("/api/admin/v1/themes/{slug}"),
                deletion(1, installed["release"].as_str().unwrap())
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "PUT",
            "/api/admin/v1/settings/theme",
            activation
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "PUT",
            "/api/admin/v1/settings/theme",
            br#"{"slug":"default","expected_version":1}"#.to_vec()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        theme_request(
            &stack,
            Some(&author),
            Some(&author_csrf),
            "DELETE",
            "/api/admin/v1/themes/custom",
            deletion(2, installed["release"].as_str().unwrap())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "DELETE",
            "/api/admin/v1/themes/custom",
            deletion(1, installed["release"].as_str().unwrap())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "DELETE",
            "/api/admin/v1/themes/custom",
            deletion(2, installed["release"].as_str().unwrap())
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!root.join("custom").exists());
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&asset_url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!response.headers().contains_key("cache-control"));
    let (status, replacement) = theme_request(
        &stack,
        Some(&owner),
        Some(&csrf),
        "POST",
        "/api/admin/v1/themes",
        custom_theme_zip("replacement"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(replacement["release"], installed["release"]);
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "DELETE",
            "/api/admin/v1/themes/custom",
            deletion(2, installed["release"].as_str().unwrap())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(root.join("custom").exists());
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "DELETE",
            "/api/admin/v1/themes/custom",
            deletion(2, replacement["release"].as_str().unwrap())
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "POST",
            "/api/admin/v1/themes",
            package
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (activation, removal) = tokio::join!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "PUT",
            "/api/admin/v1/settings/theme",
            br#"{"slug":"custom","expected_version":2}"#.to_vec()
        ),
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "DELETE",
            "/api/admin/v1/themes/custom",
            deletion(2, installed["release"].as_str().unwrap())
        ),
    );
    assert!(matches!(
        (activation.0, removal.0),
        (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::BAD_REQUEST, StatusCode::OK)
    ));
    let view = theme_request(
        &stack,
        Some(&owner),
        None,
        "GET",
        "/api/admin/v1/settings/theme",
        vec![],
    )
    .await
    .1;
    assert!(
        view["available"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["slug"] == view["effective_slug"])
    );
}
