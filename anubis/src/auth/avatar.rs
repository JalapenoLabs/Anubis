//! Avatar upload, optimization, storage, and serving.
//!
//! Uploads accept JPEG, PNG, WebP, or GIF up to 5 MiB. The server optimizes
//! every image the same way: center-crop to a square, resize to at most
//! 512 px (never upscaled), flatten transparency onto white, and re-encode
//! as JPEG. The optimized bytes live in the `user_avatars` table, and the
//! public URL `GET /users/{user_id}/avatar` serves them with a strong `ETag`,
//! answering `304 Not Modified` to matching `If-None-Match` requests.
//!
//! Serving is versioned. Every profile payload carries an `avatar_version`
//! derived from the stored image, and consumers request
//! `/users/{user_id}/avatar?v={version}`. The token is part of the cache key,
//! so a request that names the version being served is cached for a year as
//! `immutable`, and a new upload is simply a new URL that every consumer picks
//! up the moment it refetches the profile. The bare URL names the account
//! rather than the image, so it keeps an hour of freshness and revalidates by
//! `ETag`, which is the right contract for email clients and other sites.
//!
//! This pipeline is the pattern the scaffolder's image fields will reuse.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Json};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use image::Limits;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::CurrentUser;
use crate::auth::routes::AuthState;
use crate::db::DbPool;
use crate::http::ApiError;
use crate::schema::user_avatars;

/// Largest accepted upload; the optimized result is far smaller.
const MAX_UPLOAD_BYTES: usize = 5 * 1024 * 1024;

/// Largest source dimensions the decoder will touch, bounding decode bombs.
const MAX_SOURCE_PIXELS: u32 = 8192;

/// Longest edge of a stored avatar. Small sources are not upscaled.
const TARGET_EDGE: u32 = 512;

/// JPEG quality balancing size and fidelity for photographic avatars.
const JPEG_QUALITY: u8 = 85;

/// Characters of the content hash a version token carries.
///
/// Sixteen base64 characters are 96 bits of the avatar's SHA-256: collisions
/// are not a concern, and the URL stays short enough to read.
const VERSION_CHARS: usize = 16;

/// Cache policy for a URL whose `?v=` names the image being served.
///
/// A year and `immutable`: the token changes with the bytes, so this exact URL
/// can never mean anything else, and the browser skips even the revalidation.
const VERSIONED_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// Cache policy for the bare URL, which names the account, not the image.
///
/// External consumers hold it for an hour and then revalidate against the
/// `ETag`, which costs one `304`. Application screens use the versioned URL
/// instead, so they never wait out this hour.
const BARE_CACHE_CONTROL: &str = "public, max-age=3600, stale-while-revalidate=86400";

/// Adds the signed-in avatar management routes to the account router.
pub(crate) fn account_routes() -> Router<AuthState> {
    Router::new().route(
        "/profile/avatar",
        axum::routing::post(upload_avatar)
            .delete(delete_avatar)
            .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
    )
}

/// Returns the public avatar-serving routes for an application to mount.
///
/// Serves `GET /users/{user_id}/avatar` unauthenticated, like any profile
/// picture URL.
pub fn public_router(pool: DbPool) -> Router {
    Router::new()
        .route("/users/{user_id}/avatar", get(serve_avatar))
        .layer(Extension(pool))
}

#[derive(Serialize)]
struct AvatarBody {
    avatar_url: String,
}

async fn upload_avatar(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    if body.is_empty() {
        return Err(ApiError::validation("Attach an image to upload."));
    }

    let optimized = tokio::task::spawn_blocking(move || optimize(&body))
        .await
        .map_err(log_internal)?
        .map_err(|error| {
            tracing::debug!(
                error.message = %error,
                "avatar upload rejected: {{error.message}}",
            );
            ApiError::validation(
                "That image could not be read. Upload a JPEG, PNG, WebP, or GIF up to 5 MB.",
            )
        })?;

    let etag = format!("\"{}\"", URL_SAFE_NO_PAD.encode(Sha256::digest(&optimized)));

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    diesel::insert_into(user_avatars::table)
        .values((
            user_avatars::user_id.eq(user.id),
            user_avatars::image.eq(&optimized),
            user_avatars::content_type.eq("image/jpeg"),
            user_avatars::etag.eq(&etag),
        ))
        .on_conflict(user_avatars::user_id)
        .do_update()
        .set((
            user_avatars::image.eq(&optimized),
            user_avatars::content_type.eq("image/jpeg"),
            user_avatars::etag.eq(&etag),
        ))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(Json(AvatarBody {
        avatar_url: format!("/users/{}/avatar?v={}", user.id, version_from_etag(&etag)),
    }))
}

async fn delete_avatar(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    diesel::delete(user_avatars::table.filter(user_avatars::user_id.eq(user.id)))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

/// The version a client asks for, copied from its profile payload.
#[derive(Deserialize)]
struct ServeQuery {
    v: Option<String>,
}

async fn serve_avatar(
    Extension(pool): Extension<DbPool>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<ServeQuery>,
    headers: HeaderMap,
) -> Result<axum::response::Response, ApiError> {
    let mut connection = pool.get().await.map_err(log_internal)?;

    let found: Option<(Vec<u8>, String, String)> = user_avatars::table
        .filter(user_avatars::user_id.eq(user_id))
        .select((
            user_avatars::image,
            user_avatars::content_type,
            user_avatars::etag,
        ))
        .first(&mut connection)
        .await
        .optional()
        .map_err(log_internal)?;

    let Some((bytes, content_type, etag)) = found else {
        return Err(ApiError::not_found());
    };

    let cache_control = cache_policy(query.v.as_deref(), &version_from_etag(&etag)).to_owned();

    let matches = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == etag);
    if matches {
        return Ok((
            StatusCode::NOT_MODIFIED,
            [(ETAG, etag), (CACHE_CONTROL, cache_control)],
        )
            .into_response());
    }

    Ok((
        StatusCode::OK,
        [
            (CONTENT_TYPE, content_type),
            (ETAG, etag),
            (CACHE_CONTROL, cache_control),
        ],
        bytes,
    )
        .into_response())
}

/// Picks the cache policy: `immutable` only when the caller named the version
/// being served.
///
/// A caller asking for an older version is a stale client, so it keeps the
/// bare policy rather than freezing today's bytes at yesterday's URL for a
/// year.
fn cache_policy(requested: Option<&str>, current: &str) -> &'static str {
    if requested == Some(current) {
        return VERSIONED_CACHE_CONTROL;
    }
    BARE_CACHE_CONTROL
}

/// Derives the URL version token from a stored strong `ETag`.
///
/// The `ETag` is the quoted base64 SHA-256 of the optimized bytes, so a prefix
/// of it is content-addressed: re-uploading the same picture keeps the URL,
/// and every cache entry with it.
fn version_from_etag(etag: &str) -> String {
    etag.trim_matches('"').chars().take(VERSION_CHARS).collect()
}

/// The version token for a user's avatar, or `None` when they have none.
///
/// [`crate::auth::UserResponse`] carries this so every consumer can build
/// `/users/{user_id}/avatar?v={version}`.
///
/// # Errors
/// Returns the database error when the lookup fails.
pub(crate) async fn stored_version(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<Option<String>> {
    let etag: Option<String> = user_avatars::table
        .filter(user_avatars::user_id.eq(user_id))
        .select(user_avatars::etag)
        .first(connection)
        .await
        .optional()?;

    Ok(etag.as_deref().map(version_from_etag))
}

/// Center-crops, resizes, flattens transparency, and re-encodes as JPEG.
fn optimize(raw: &[u8]) -> Result<Vec<u8>, image::ImageError> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(raw)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_PIXELS);
    limits.max_image_height = Some(MAX_SOURCE_PIXELS);
    reader.limits(limits);

    let decoded = reader.decode()?;

    let edge = TARGET_EDGE
        .min(decoded.width())
        .min(decoded.height())
        .max(1);
    let squared = decoded.resize_to_fill(edge, edge, FilterType::Lanczos3);

    // Flatten transparency onto white so JPEG has something sensible to show.
    let rgba = squared.to_rgba8();
    let mut flattened = image::RgbImage::new(rgba.width(), rgba.height());
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = f32::from(pixel[3]) / 255.0;
        let blend = |channel: u8| -> u8 {
            let value = f32::from(channel)
                .mul_add(alpha, 255.0 * (1.0 - alpha))
                .round();
            // The blend of two 0-255 channels stays within 0-255.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to the u8 range by construction"
            )]
            {
                value.clamp(0.0, 255.0) as u8
            }
        };
        flattened.put_pixel(
            x,
            y,
            image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]),
        );
    }

    let mut output = Vec::new();
    let jpeg = JpegEncoder::new_with_quality(&mut output, JPEG_QUALITY);
    flattened.write_with_encoder(jpeg)?;
    Ok(output)
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "avatar request failed: {{error.message}}",
    );
    ApiError::internal()
}

#[cfg(test)]
mod tests {
    use image::codecs::png::PngEncoder;

    use super::{
        BARE_CACHE_CONTROL, VERSION_CHARS, VERSIONED_CACHE_CONTROL, cache_policy, optimize,
        version_from_etag,
    };

    fn png_bytes(image: &image::DynamicImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        image
            .write_with_encoder(PngEncoder::new(&mut bytes))
            .expect("encoding the fixture must succeed");
        bytes
    }

    #[test]
    fn landscape_sources_become_square_jpegs_capped_at_512() {
        let source = image::DynamicImage::new_rgb8(1600, 900);
        let optimized = optimize(&png_bytes(&source)).expect("optimization must succeed");

        let decoded = image::load_from_memory(&optimized).expect("output must decode");
        assert_eq!(decoded.width(), 512);
        assert_eq!(decoded.height(), 512);
        // JPEG magic bytes.
        assert_eq!(&optimized[0..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn small_sources_are_not_upscaled() {
        let source = image::DynamicImage::new_rgb8(120, 80);
        let optimized = optimize(&png_bytes(&source)).expect("optimization must succeed");

        let decoded = image::load_from_memory(&optimized).expect("output must decode");
        assert_eq!(decoded.width(), 80);
        assert_eq!(decoded.height(), 80);
    }

    #[test]
    fn transparency_flattens_onto_white() {
        let source = image::DynamicImage::new_rgba8(64, 64);
        let optimized = optimize(&png_bytes(&source)).expect("optimization must succeed");

        let decoded = image::load_from_memory(&optimized)
            .expect("output must decode")
            .to_rgb8();
        let center = decoded.get_pixel(32, 32);
        assert!(
            center[0] > 240 && center[1] > 240 && center[2] > 240,
            "transparent input must flatten to white, got {center:?}"
        );
    }

    #[test]
    fn junk_bytes_are_rejected() {
        optimize(b"definitely not an image").expect_err("junk must be rejected");
    }

    #[test]
    fn versions_are_an_unquoted_prefix_of_the_etag() {
        let version = version_from_etag("\"WFy7Qm1s3TkPq0aZbCdEfGhIjKlMnOpQrStUvWxYz\"");

        assert_eq!(version.chars().count(), VERSION_CHARS);
        assert_eq!(version, "WFy7Qm1s3TkPq0aZ");
        assert!(!version.contains('"'), "a query value carries no quotes");
    }

    #[test]
    fn the_named_version_caches_immutably_and_everything_else_revalidates() {
        assert_eq!(
            cache_policy(Some("abc123"), "abc123"),
            VERSIONED_CACHE_CONTROL
        );
        // A bare URL names the account, and a stale client names an image we
        // no longer serve. Neither may freeze today's bytes for a year.
        assert_eq!(cache_policy(None, "abc123"), BARE_CACHE_CONTROL);
        assert_eq!(cache_policy(Some("older0"), "abc123"), BARE_CACHE_CONTROL);
    }
}
