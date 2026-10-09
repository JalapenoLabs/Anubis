//! Avatar upload, optimization, storage, and serving.
//!
//! Uploads accept JPEG, PNG, WebP, or GIF, up to 5 MiB by default, decoded
//! under the configured edge and pixel limits
//! ([`crate::config::AvatarConfig`]), which are checked against the header
//! before the decoder allocates. When the application gave
//! [`crate::auth::router_with`] an [`ImageScreen`], the decoded
//! source goes to it next, and a refusal or a failure to decide stores
//! nothing. The server then optimizes every image the same way: center-crop
//! to a square, resize to at most 512 px (never upscaled), flatten
//! transparency onto white, and re-encode as JPEG. The optimized bytes live in
//! the `user_avatars` table, and the public URL `GET /users/{user_id}/avatar`
//! serves them with a strong `ETag`, answering `304 Not Modified` to matching
//! `If-None-Match` requests.
//!
//! Every upload, removal and refusal is audited against the account as
//! `avatar.uploaded`, `avatar.removed` and `avatar.refused`, carrying the
//! version token and never the image.
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

use std::num::NonZero;

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
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::audit;
use crate::auth::CurrentUser;
use crate::auth::model::User;
use crate::auth::routes::AuthState;
use crate::config::AvatarConfig;
use crate::db::DbPool;
use crate::http::ApiError;
use crate::images::{
    self, DecodedImage, IMAGE_REFUSED, IMAGE_SCREEN_UNAVAILABLE, ImageScreen, Verdict,
};
use crate::schema::user_avatars;

/// What a person reads when a screen refused their picture.
///
/// Deliberately says nothing about why. It is the same sentence whatever the
/// screen saw, so it can neither accuse anybody nor teach them what to change.
const REFUSED_MESSAGE: &str = "That image can't be used. Choose a different picture.";

/// What a person reads when the screen could not decide.
const SCREEN_UNAVAILABLE_MESSAGE: &str =
    "We couldn't check that image just now. Try again in a moment.";

/// The audit log's subject type for an act on an account.
const ACCOUNT_SUBJECT: &str = "User";

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

/// Adds the signed-in avatar management routes to the account router, with
/// the upload's body capped at `max_upload_bytes`.
pub(crate) fn account_routes(max_upload_bytes: NonZero<usize>) -> Router<AuthState> {
    Router::new().route(
        "/profile/avatar",
        axum::routing::post(upload_avatar)
            .delete(delete_avatar)
            .layer(DefaultBodyLimit::max(max_upload_bytes.get())),
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
    context: audit::Context,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    if body.is_empty() {
        return Err(ApiError::validation("Attach an image to upload."));
    }

    let limits = state.avatar.decode;
    let decoded = tokio::task::spawn_blocking(move || images::decode(&body, &limits))
        .await
        .map_err(log_internal)?
        .map_err(|error| undecodable(&error, &state.avatar))?;

    if let Some(screen) = &state.image_screen {
        screen_upload(screen.as_ref(), &decoded, &state, &user, &context).await?;
    }

    // Re-encoding an image that decoded is our work, not the upload's, so a
    // failure here is ours to report rather than the person's to fix.
    let optimized = tokio::task::spawn_blocking(move || optimize(&decoded))
        .await
        .map_err(log_internal)?
        .map_err(log_internal)?;

    let etag = format!("\"{}\"", URL_SAFE_NO_PAD.encode(Sha256::digest(&optimized)));
    let version = version_from_etag(&etag);

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    // The picture and its record commit together. The version it replaced is
    // read in the same transaction but not locked, because there may be no row
    // to lock: two uploads racing for one account both store atomically
    // through the upsert, and at worst both records name the same predecessor.
    connection
        .transaction::<(), diesel::result::Error, _>(async |transaction| {
            let replaced = stored_version(transaction, user.id).await?;

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
                .execute(transaction)
                .await?;

            audit::record(
                transaction,
                &context.by(&user),
                &audit::Event::new(audit::AVATAR_UPLOADED, ACCOUNT_SUBJECT)
                    .subject(user.id)
                    .changes(audit::Changes::new().field("version", replaced, version.clone())),
            )
            .await?;
            Ok(())
        })
        .await
        .map_err(log_internal)?;

    Ok(Json(AvatarBody {
        avatar_url: format!("/users/{}/avatar?v={version}", user.id),
    }))
}

/// Answers an upload that would not decode within the limits.
///
/// The bytes are the uploader's, so this is a `400` that says what to send
/// instead; the decoder's own detail goes to the debug log, never the body.
fn undecodable(error: &images::DecodeError, limits: &AvatarConfig) -> ApiError {
    tracing::debug!(
        error.message = %error,
        "avatar upload rejected: {{error.message}}",
    );

    if error.is_over_budget() {
        // Tenths of a megapixel in integer arithmetic, rounded down so the
        // sentence never promises more than the budget admits.
        let tenths = limits.decode.max_pixels / 100_000;
        return ApiError::validation(format!(
            "That image is too large. Upload one no wider or taller than {} pixels and no \
             larger than {}.{} megapixels.",
            limits.decode.max_edge,
            tenths / 10,
            tenths % 10,
        ));
    }
    ApiError::validation("That image could not be read. Upload a JPEG, PNG, WebP, or GIF.")
}

/// Asks the application's screen about `decoded`, refusing the upload unless
/// it accepts.
///
/// A refusal is recorded against the account and answered with
/// [`IMAGE_REFUSED`]. A screen that failed to decide is logged at `ERROR`,
/// which error reporting files, and answered with [`IMAGE_SCREEN_UNAVAILABLE`]:
/// it has not said yes, so nothing is stored, and the person may try again.
///
/// # Errors
/// The [`ApiError`] the upload answers with whenever the verdict is not
/// [`Verdict::Accept`].
async fn screen_upload(
    screen: &dyn ImageScreen,
    decoded: &DecodedImage,
    state: &AuthState,
    user: &User,
    context: &audit::Context,
) -> Result<(), ApiError> {
    match screen.screen(decoded).await {
        Ok(Verdict::Accept) => Ok(()),
        Ok(Verdict::Refuse) => {
            tracing::info!(
                user.id = %user.id,
                "avatar upload refused by the image screen for {{user.id}}",
            );
            record_refusal(state, user, context).await;
            Err(ApiError::validation(REFUSED_MESSAGE).with_code(IMAGE_REFUSED))
        }
        Err(error) => {
            tracing::error!(
                error.message = %error,
                "avatar image screen failed, refusing the upload: {{error.message}}",
            );
            Err(ApiError::unavailable(SCREEN_UNAVAILABLE_MESSAGE)
                .with_code(IMAGE_SCREEN_UNAVAILABLE))
        }
    }
}

/// Records that the screen refused `user`'s upload, with nothing of the image.
///
/// A failure to record does not turn the refusal into an acceptance or into a
/// `500`: the person still reads that the picture can't be used, and the lost
/// record is logged at `ERROR`, which error reporting files.
async fn record_refusal(state: &AuthState, user: &User, context: &audit::Context) {
    let recorded = match state.pool.get().await {
        Ok(mut connection) => audit::record(
            &mut connection,
            &context.by(user),
            &audit::Event::new(audit::AVATAR_REFUSED, ACCOUNT_SUBJECT).subject(user.id),
        )
        .await
        .map(drop)
        .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };

    if let Err(error) = recorded {
        tracing::error!(
            error.message = %error,
            "a refused avatar upload could not be audited: {{error.message}}",
        );
    }
}

async fn delete_avatar(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    context: audit::Context,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    // Removing a picture that is not there changes nothing, so it records
    // nothing either: the log holds acts, not requests.
    connection
        .transaction::<(), diesel::result::Error, _>(async |transaction| {
            let removed: Option<String> =
                diesel::delete(user_avatars::table.filter(user_avatars::user_id.eq(user.id)))
                    .returning(user_avatars::etag)
                    .get_result(transaction)
                    .await
                    .optional()?;

            let Some(etag) = removed else {
                return Ok(());
            };

            audit::record(
                transaction,
                &context.by(&user),
                &audit::Event::new(audit::AVATAR_REMOVED, ACCOUNT_SUBJECT)
                    .subject(user.id)
                    .changes(audit::Changes::new().field(
                        "version",
                        version_from_etag(&etag),
                        serde_json::Value::Null,
                    )),
            )
            .await?;
            Ok(())
        })
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
fn optimize(decoded: &DecodedImage) -> Result<Vec<u8>, image::ImageError> {
    let decoded = decoded.source();

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
        BARE_CACHE_CONTROL, VERSION_CHARS, VERSIONED_CACHE_CONTROL, cache_policy, version_from_etag,
    };
    use crate::images::{DecodeLimits, decode};

    /// Decodes under the default limits and optimizes, as an upload does.
    fn optimize(raw: &[u8]) -> Result<Vec<u8>, String> {
        let decoded = decode(raw, &DecodeLimits::default()).map_err(|error| error.to_string())?;
        super::optimize(&decoded).map_err(|error| error.to_string())
    }

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
