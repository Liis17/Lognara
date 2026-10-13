mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::*;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get_text(t: &TestApp, path: &str) -> (StatusCode, header::HeaderMap, String) {
    let response = t
        .router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn serves_spa_with_index_fallback_and_api_404() {
    let t = start(&[("liis", "password")], &[]);
    let web = t.dir.path().join("web");
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(web.join("index.html"), "<div id=root></div>").unwrap();
    std::fs::write(web.join("assets/app.js"), "console.log(1)").unwrap();

    let (status, headers, body) = get_text(&t, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "<div id=root></div>");
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

    // Маршруты react-router открываются напрямую.
    for path in ["/login", "/logs", "/metrics", "/settings"] {
        let (status, _, body) = get_text(&t, path).await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, "<div id=root></div>"),
            "{path}"
        );
    }
    let (status, headers, body) = get_text(&t, "/assets/app.js").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "console.log(1)"));
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("javascript")
    );

    let missing = send(&t.router, request(Method::GET, "/api/unknown", None, None)).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.body["error"], "not_found");
}
