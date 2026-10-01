//! Theme lifecycle through authenticated HTTP, real storage and public rendering.
use super::*;

fn custom_theme_zip(index: &str) -> Vec<u8> {
    custom_theme_zip_with_schema(index, None)
}

fn custom_theme_zip_with_schema(index: &str, schema: Option<serde_json::Value>) -> Vec<u8> {
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
    if let Some(schema) = schema {
        files.push(("settings.schema.json".into(), schema.to_string()));
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

async fn deletion(stack: &Stack, version: i64, release: &str) -> Vec<u8> {
    let (id, config_version, schema): (Uuid, i64, i32) =
        sqlx::query_as("SELECT id,version,config_schema_version FROM themes WHERE slug='custom'")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
    serde_json::to_vec(&serde_json::json!({"expected_version":version,"expected_release":release,"id":id,"expected_config_version":config_version,"config_schema_version":schema}))
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
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "{method} {path}: {status}"
    );
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
    assert_eq!(
        std::fs::read_dir(root)
            .unwrap()
            .filter(|e| !e
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.'))
            .count(),
        1
    );
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
                deletion(&stack, 1, installed["release"].as_str().unwrap()).await
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
            deletion(&stack, 2, installed["release"].as_str().unwrap()).await
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
            deletion(&stack, 1, installed["release"].as_str().unwrap()).await
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
            deletion(&stack, 2, installed["release"].as_str().unwrap()).await
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
            deletion(&stack, 2, installed["release"].as_str().unwrap()).await
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
            deletion(&stack, 2, replacement["release"].as_str().unwrap()).await
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
            deletion(&stack, 2, installed["release"].as_str().unwrap()).await
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

fn config_input(view: &serde_json::Value, config: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"id":view["id"],"expected_release":view["release"],"config_schema_version":view["config_schema_version"],"expected_version":view["version"],"config":config})).unwrap()
}
async fn public_html(stack: &Stack) -> String {
    let response = stack
        .router
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}
#[tokio::test]
async fn theme_settings_http_defaults_independent_configs_validation_refs_and_old_page_identity() {
    let _serial = SERIAL.lock().await;
    let stack = fresh_stack_with_themes(true).await;
    let (owner, csrf) = login_as(&stack.router, &stack.idp, "owner").await;
    let (author, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let schema = serde_json::json!({"config_schema_version":1,"fields":[
        {"key":"accent_color","type":"color","label":"Accent","default":"#2563eb"},
        {"key":"show","type":"boolean","label":"Show","default":true},
        {"key":"title","type":"text","label":"Title","default":"custom default","max_length":100},
        {"key":"count","type":"integer","label":"Count","default":1,"min":1,"max":10},
        {"key":"layout","type":"select","label":"Layout","default":"wide","options":[{"value":"wide","label":"Wide"},{"value":"small","label":"Small"}]},
        {"key":"image","type":"media","label":"Image","default":null}
    ]});
    let package = custom_theme_zip_with_schema(
        "<p>{{ theme.config.title }}</p><span>{{ theme.config.accent_color }}</span>{% if theme.config.show %}<b>VISIBLE</b>{% endif %}{% if theme.config.image %}<img src=\"{{ ('/media/' ~ theme.config.image) | url }}\">{% endif %}",
        Some(schema),
    );
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
        StatusCode::CREATED
    );
    let path = "/api/admin/v1/themes/custom/settings";
    assert_eq!(
        theme_request(&stack, None, None, "GET", path, vec![])
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        theme_request(&stack, Some(&author), None, "GET", path, vec![])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let original = theme_request(&stack, Some(&owner), None, "GET", path, vec![])
        .await
        .1;
    assert_eq!(original["config"]["title"], "custom default");
    assert_eq!(original["config"]["image"], serde_json::Value::Null);
    assert_eq!(original["overrides"], serde_json::json!({}));
    let input = config_input(
        &original,
        serde_json::json!({"title":"saved custom","show":false,"accent_color":"#abcdef"}),
    );
    assert_eq!(
        theme_request(&stack, Some(&owner), None, "PUT", path, input.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        theme_request(
            &stack,
            Some(&author),
            Some(&author_csrf),
            "PUT",
            path,
            input.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, saved) = theme_request(
        &stack,
        Some(&owner),
        Some(&csrf),
        "PUT",
        path,
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["version"], 2);
    assert_eq!(saved["config"]["count"], 1);
    assert_eq!(
        theme_request(&stack, Some(&owner), Some(&csrf), "PUT", path, input)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let idem = theme_request(
        &stack,
        Some(&owner),
        Some(&csrf),
        "PUT",
        path,
        config_input(&saved, saved["overrides"].clone()),
    )
    .await;
    assert_eq!(idem.0, StatusCode::OK);
    assert_eq!(idem.1["version"], 2);
    for invalid in [
        serde_json::json!({"extra":true}),
        serde_json::json!({"count":1.5}),
        serde_json::json!({"count":11}),
        serde_json::json!({"show":"true"}),
        serde_json::json!({"accent_color":"red"}),
        serde_json::json!({"layout":"unknown"}),
        serde_json::json!({"image":Uuid::now_v7()}),
        serde_json::json!({"title":null}),
    ] {
        assert_eq!(
            theme_request(
                &stack,
                Some(&owner),
                Some(&csrf),
                "PUT",
                path,
                config_input(&saved, invalid)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    for key in ["id", "expected_release", "config_schema_version"] {
        let mut stale: serde_json::Value =
            serde_json::from_slice(&config_input(&saved, saved["overrides"].clone())).unwrap();
        stale[key] = match key {
            "id" => Uuid::now_v7().to_string().into(),
            "expected_release" => "wrong".into(),
            _ => 2.into(),
        };
        assert_eq!(
            theme_request(
                &stack,
                Some(&owner),
                Some(&csrf),
                "PUT",
                path,
                serde_json::to_vec(&stale).unwrap()
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
    let default_path = "/api/admin/v1/themes/default/settings";
    let default = theme_request(&stack, Some(&owner), None, "GET", default_path, vec![])
        .await
        .1;
    let default_saved=theme_request(&stack,Some(&owner),Some(&csrf),"PUT",default_path,config_input(&default,serde_json::json!({"accent_color":"#123456","show_description":false,"footer_note":"saved default"}))).await;
    assert_eq!(default_saved.0, StatusCode::OK);
    let html = public_html(&stack).await;
    assert!(html.contains("#123456"));
    assert!(html.contains("saved default"));
    assert!(!html.contains("saved custom"));
    for (slug, version, text) in [
        ("custom", 0, "saved custom"),
        ("default", 1, "saved default"),
        ("custom", 2, "saved custom"),
    ] {
        let activation = theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "PUT",
            "/api/admin/v1/settings/theme",
            serde_json::to_vec(&serde_json::json!({"slug":slug,"expected_version":version}))
                .unwrap(),
        )
        .await;
        assert_eq!(activation.0, StatusCode::OK);
        let html = public_html(&stack).await;
        assert!(html.contains(text));
        if slug == "custom" {
            assert!(html.contains("#abcdef"));
            assert!(!html.contains("VISIBLE"));
        }
    }
    // Concurrent drafts: exactly one wins; all failed writes leave its value intact.
    let (a, b) = tokio::join!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "PUT",
            path,
            config_input(&saved, serde_json::json!({"title":"race A"}))
        ),
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "PUT",
            path,
            config_input(&saved, serde_json::json!({"title":"race B"}))
        )
    );
    assert!(matches!(
        (a.0, b.0),
        (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
    ));
    let now = theme_request(&stack, Some(&owner), None, "GET", path, vec![])
        .await
        .1;
    let mid = Uuid::now_v7();
    sqlx::query("INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256) VALUES($1,$2,'theme.png','image/png',1,1,1,$3)").bind(mid).bind(format!("objects/{mid}.png")).bind("a".repeat(64)).execute(&stack.pool).await.unwrap();
    let image_saved = theme_request(
        &stack,
        Some(&owner),
        Some(&csrf),
        "PUT",
        path,
        config_input(&now, serde_json::json!({"image":mid,"title":"with image"})),
    )
    .await;
    assert_eq!(image_saved.0, StatusCode::OK);
    assert!(public_html(&stack).await.contains(&format!("/media/{mid}")));
    let source: Uuid = sqlx::query_scalar(
        "SELECT source_id FROM media_refs WHERE media_id=$1 AND source_type='theme'",
    )
    .bind(mid)
    .fetch_one(&stack.pool)
    .await
    .unwrap();
    assert_eq!(source.to_string(), now["id"].as_str().unwrap());
    let owner_usage = theme_request(
        &stack,
        Some(&owner),
        None,
        "GET",
        &format!("/api/admin/v1/media/{mid}"),
        vec![],
    )
    .await;
    assert_eq!(owner_usage.0, StatusCode::OK);
    assert_eq!(owner_usage.1["references"][0]["kind"], "theme");
    let author_usage = theme_request(
        &stack,
        Some(&author),
        None,
        "GET",
        &format!("/api/admin/v1/media/{mid}"),
        vec![],
    )
    .await;
    assert_eq!(author_usage.0, StatusCode::OK);
    assert_eq!(author_usage.1["hidden_references"], 1);
    assert_eq!(author_usage.1["references"], serde_json::json!([]));
    // A missing selected slug falls back using Default's saved config.
    sqlx::query("UPDATE settings SET value=jsonb_set(value,'{slug}','\"missing\"'),version=version+1 WHERE key='theme'").execute(&stack.pool).await.unwrap();
    let html = public_html(&stack).await;
    assert!(html.contains("saved default"));
    assert!(html.contains("#123456"));
    assert!(!html.contains("with image"));
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "PUT",
            "/api/admin/v1/settings/theme",
            br#"{"slug":"default","expected_version":4}"#.to_vec()
        )
        .await
        .0,
        StatusCode::OK
    );
    let old_delete = deletion(&stack, 5, now["release"].as_str().unwrap()).await;
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "DELETE",
            "/api/admin/v1/themes/custom",
            old_delete.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM media_refs WHERE source_type='theme'")
            .fetch_one(&stack.pool)
            .await
            .unwrap(),
        0
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
    let fresh = theme_request(&stack, Some(&owner), None, "GET", path, vec![])
        .await
        .1;
    assert_ne!(fresh["id"], now["id"]);
    assert_eq!(fresh["release"], now["release"]);
    assert_eq!(fresh["config"]["title"], "custom default");
    assert_eq!(fresh["version"], original["version"]);
    assert_eq!(fresh["release"], original["release"]);
    let old_identity_only = serde_json::to_vec(&serde_json::json!({
        "expected_version":5,"expected_release":original["release"],"id":original["id"],
        "expected_config_version":original["version"],"config_schema_version":original["config_schema_version"]
    })).unwrap();
    assert_eq!(
        theme_request(
            &stack,
            Some(&owner),
            Some(&csrf),
            "DELETE",
            "/api/admin/v1/themes/custom",
            old_identity_only
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
            path,
            config_input(&original, serde_json::json!({"title":"stale page"}))
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
            old_delete
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
