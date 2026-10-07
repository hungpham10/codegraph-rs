//! Web UI nhúng — phục vụ asset Svelte (đã build) từ trong binary.
//!
//! `codegraph serve --graphql` mount [`web_router`] làm fallback: mọi route
//! không phải `/graphql` / `/health` trả về app SPA. Asset được `rust-embed`
//! nhúng lúc compile từ `assets/` (build bằng `scripts/build-web.sh`; output
//! KHÔNG commit). Folder rỗng/thiếu vẫn compile được (`allow_missing`) — khi
//! đó fallback trả hướng dẫn build.

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use rust_embed::RustEmbed;

/// Asset nhúng — sinh từ `web/` bằng `scripts/build-web.sh` (hoặc job
/// `build-web` trong CI/release). KHÔNG commit output build (`assets/` nằm
/// trong `.gitignore`); `allow_missing` cho phép clone sạch — nơi folder chỉ
/// có `.gitkeep` hoặc chưa tồn tại — vẫn compile với embed rỗng.
#[derive(RustEmbed)]
#[folder = "assets"]
#[allow_missing = true]
struct WebAssets;

/// Router phục vụ UI (mount làm fallback của server GraphQL).
///
/// Mọi request GET không khớp file sẽ trả `200.html` (SPA shell) để router
/// phía client xử lý. Asset dưới `_app/immutable/` được cache vĩnh viễn
/// (tên có content-hash).
pub fn web_router() -> Router {
	Router::new().fallback(serve)
}

/// Có asset UI được nhúng không (folder build đã tồn tại lúc compile).
pub fn has_assets() -> bool {
	WebAssets::get("index.html").is_some() || WebAssets::get("200.html").is_some()
}

/// Chuẩn hoá đường dẫn → tên file trong `assets/`.
fn normalize(path: &str) -> String {
	let p = path.trim_start_matches('/');
	if p.is_empty() {
		"index.html".to_string()
	} else {
		p.to_string()
	}
}

/// Đọc một file asset, set content-type + cache-control.
fn asset_response(name: &str) -> Option<Response> {
	let asset = WebAssets::get(name)?;
	let mime = mime_guess::from_path(name).first_or_octet_stream();
	let cache = if name.starts_with("_app/immutable/") {
		// content-hash trong tên → immutable.
		"public, max-age=31536000, immutable"
	} else {
		"no-cache"
	};
	Some(
		Response::builder()
			.status(StatusCode::OK)
			.header(header::CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap())
			.header(header::CACHE_CONTROL, HeaderValue::from_static(cache))
			.body(Body::from(asset.data.into_owned()))
			.unwrap(),
	)
}

async fn serve(uri: Uri) -> Response {
	let name = normalize(uri.path());

	if let Some(res) = asset_response(&name) {
		return res;
	}

	// SPA fallback: shell cho client-side routing.
	if let Some(res) = asset_response("200.html").or_else(|| asset_response("index.html")) {
		return res;
	}

	// Chưa build web — hướng dẫn.
	(
		StatusCode::NOT_FOUND,
		[(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
		"Web UI chưa được build vào binary này.\n\
		 Chạy `scripts/build-web.sh` rồi build lại codegraph.\n",
	)
		.into_response()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn normalize_root_to_index() {
		assert_eq!(normalize("/"), "index.html");
		assert_eq!(normalize(""), "index.html");
		assert_eq!(normalize("/_app/immutable/x.js"), "_app/immutable/x.js");
	}

	#[test]
	fn has_assets_matches_built_folder() {
		// Sau `scripts/build-web.sh` folder có index/200.html → true; clone sạch
		// (chỉ .gitkeep) → false. Không assert cứng, chỉ đảm bảo không panic.
		let _ = has_assets();
	}

	#[tokio::test]
	async fn fallback_serves_shell_or_guidance() {
		use axum::body::Body;
		use axum::http::Request;
		use tower::ServiceExt;

		let app = web_router();
		let res = app
			.oneshot(
				Request::builder()
					.uri("/some/spa/route")
					.body(Body::empty())
					.unwrap(),
			)
			.await
			.unwrap();
		// Có asset → 200 (shell); chưa build → 404 (hướng dẫn). Cả hai đều hợp lệ.
		assert!(
			res.status() == StatusCode::OK || res.status() == StatusCode::NOT_FOUND,
			"unexpected status: {}",
			res.status()
		);
	}
}
