//! Cross-origin access for the windows-deck panel, whose `WebView` page runs on
//! `http://tauri.localhost` and calls this API on another origin. Pages served from this
//! machine (the panel's development server) are allowed too; other web sites are not, so
//! a page on the Internet cannot read the API's answers.

use axum::http::{HeaderValue, Method, header::CONTENT_TYPE, request::Parts};
use tower_http::cors::{AllowOrigin, CorsLayer};

pub fn layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin: &HeaderValue, _: &Parts| {
            origin.to_str().is_ok_and(allowed)
        }))
        .allow_methods([Method::GET, Method::POST, Method::PUT])
        .allow_headers([CONTENT_TYPE])
}

/// The Tauri `WebView` origins, and `http://localhost` / `http://127.0.0.1` on any port.
fn allowed(origin: &str) -> bool {
    if matches!(
        origin,
        "http://tauri.localhost" | "https://tauri.localhost" | "tauri://localhost"
    ) {
        return true;
    }
    let Some(host_port) = origin.strip_prefix("http://") else {
        return false;
    };
    let (host, port) = host_port.split_once(':').unwrap_or((host_port, ""));
    matches!(host, "localhost" | "127.0.0.1") && port.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::Body,
        http::{Method, Request, header},
        routing::get,
    };
    use tower::ServiceExt;

    use super::{allowed, layer};

    #[test]
    fn allows_the_panel_and_local_pages_only() {
        for origin in [
            "http://tauri.localhost",
            "https://tauri.localhost",
            "tauri://localhost",
            "http://localhost:1420",
            "http://127.0.0.1:4173",
            "http://localhost",
        ] {
            assert!(allowed(origin), "{origin}");
        }
        for origin in [
            "https://example.com",
            "http://192.168.1.100:5173",
            "http://localhost.example.com",
            "http://127.0.0.1.example.com:80",
            "http://localhost:80/path",
            "https://tauri.localhost.example.com",
            "null",
        ] {
            assert!(!allowed(origin), "{origin}");
        }
    }

    async fn allow_origin_for(method: Method, origin: &str) -> Option<String> {
        let app = Router::new()
            .route("/buttons", get(|| async { "[]" }).put(|| async { "" }))
            .layer(layer());
        let mut request = Request::builder()
            .method(method.clone())
            .uri("/buttons")
            .header(header::ORIGIN, origin);
        if method == Method::OPTIONS {
            request = request
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "PUT")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type");
        }
        let response = app
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .map(|v| v.to_str().unwrap().to_owned())
    }

    #[tokio::test]
    async fn answers_carry_the_allow_origin_header_only_for_allowed_origins() {
        assert_eq!(
            allow_origin_for(Method::GET, "http://tauri.localhost").await,
            Some("http://tauri.localhost".into())
        );
        assert_eq!(
            allow_origin_for(Method::OPTIONS, "http://tauri.localhost").await,
            Some("http://tauri.localhost".into())
        );
        assert_eq!(
            allow_origin_for(Method::GET, "https://example.com").await,
            None
        );
        assert_eq!(
            allow_origin_for(Method::OPTIONS, "https://example.com").await,
            None
        );
    }
}
