use axum::extract::Path;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use include_dir::{include_dir, Dir};

static EXPLORER_DIST: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/../../third_party/defradb-explorer/dist");

pub(crate) fn explorer_router() -> Router {
    Router::new()
        .route(
            "/explorer",
            get(|| async { Redirect::permanent("/explorer/") }),
        )
        .route("/explorer/", get(|| async { serve_file("index.html") }))
        .route(
            "/explorer/{*path}",
            get(|Path(path): Path<String>| async move { serve_file(&path) }),
        )
}

fn serve_file(path: &str) -> Response {
    let Some(file) = EXPLORER_DIST.get_file(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "json" | "map" => "application/json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    let cache_control = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(cache_control),
            ),
        ],
        file.contents(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    use super::*;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    async fn spawn(router: Router) -> TestResult<SocketAddr> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(addr)
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("building test client")
    }

    #[tokio::test]
    async fn explorer_without_trailing_slash_redirects() -> TestResult<()> {
        let addr = spawn(explorer_router()).await?;
        let response = client()
            .get(format!("http://{addr}/explorer"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/explorer/")
        );
        Ok(())
    }

    #[tokio::test]
    async fn explorer_index_serves_html() -> TestResult<()> {
        let addr = spawn(explorer_router()).await?;
        let response = client()
            .get(format!("http://{addr}/explorer/"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            content_type.starts_with("text/html"),
            "unexpected content type {content_type}"
        );
        let body = response.text().await?;
        assert!(
            body.contains("id=\"root\""),
            "not the explorer shell: {body}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn explorer_serves_hashed_assets_with_types_and_caching() -> TestResult<()> {
        let assets = super::EXPLORER_DIST
            .get_dir("assets")
            .expect("vendored dist has an assets directory");
        let addr = spawn(explorer_router()).await?;
        let http = client();
        for file in assets.files() {
            let path = file.path().to_str().expect("utf-8 asset path");
            let response = http
                .get(format!("http://{addr}/explorer/{path}"))
                .send()
                .await?;
            assert_eq!(response.status(), reqwest::StatusCode::OK, "asset {path}");
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let expected = match path.rsplit('.').next().unwrap_or_default() {
                "js" => "text/javascript",
                "css" => "text/css",
                extension => panic!("asset {path} has unmapped extension {extension}"),
            };
            assert_eq!(content_type, expected, "asset {path}");
            let cache_control = response
                .headers()
                .get(reqwest::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(
                cache_control.contains("immutable"),
                "hashed asset {path} should be cacheable, got {cache_control:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn explorer_serves_public_svg() -> TestResult<()> {
        let addr = spawn(explorer_router()).await?;
        let response = client()
            .get(format!("http://{addr}/explorer/defradb-logo-white.svg"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("image/svg+xml")
        );
        Ok(())
    }

    #[tokio::test]
    async fn explorer_unknown_path_is_not_found() -> TestResult<()> {
        let addr = spawn(explorer_router()).await?;
        let response = client()
            .get(format!("http://{addr}/explorer/no-such-file.js"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        Ok(())
    }
}
