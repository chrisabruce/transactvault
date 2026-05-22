//! User profile management — name, password, avatar.
//!
//! All endpoints operate on the *current* user only; there's no
//! "edit-someone-else's-profile" surface here. Brokers who need to
//! change a teammate's role do it from the team page.

use axum::Form;
use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use bytes::Bytes;
use serde::Deserialize;

use crate::audit;
use crate::auth::{CurrentUser, hash_password, verify_password};
use crate::controllers::render;
use crate::controllers::transactions::load_brokerage;
use crate::error::AppError;
use crate::state::AppState;
use crate::templates::{AppHeader, ProfilePage};

/// Hard cap on a raw avatar upload before cropping client-side. Real
/// avatars are tens of KB after the canvas re-encodes to PNG, but the
/// raw original can be a 12-megapixel phone photo. 8 MB covers that
/// without giving abusers free disk to fill the bucket.
const AVATAR_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Storage key layout for avatars: `avatars/<user-key>.png`. One file
/// per user, overwritten on re-upload — no version chain.
fn avatar_key(user_id_key: &str) -> String {
    format!("avatars/{user_id_key}.png")
}

pub async fn show(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Html<String>, AppError> {
    render_profile(&state, &user, None, None).await
}

#[derive(Debug, Deserialize)]
pub struct ProfileInput {
    pub name: String,
}

pub async fn update(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(input): Form<ProfileInput>,
) -> Result<Response, AppError> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Ok(
            render_profile(&state, &user, Some("Name can't be empty."), None)
                .await?
                .into_response(),
        );
    }

    state
        .db
        .query("UPDATE $u SET name = $n")
        .bind(("u", user.user_id.clone()))
        .bind(("n", name.clone()))
        .await?;

    audit::record(
        &state.db,
        "profile_updated",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!("name=\"{name}\"")),
    )
    .await;

    Ok(Redirect::to("/app/profile").into_response())
}

#[derive(Debug, Deserialize)]
pub struct PasswordInput {
    pub current_password: String,
    pub new_password: String,
    pub confirm_password: String,
}

pub async fn change_password(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(input): Form<PasswordInput>,
) -> Result<Response, AppError> {
    // Length + match checks before we even hash anything — fast
    // failure path, doesn't leak timing.
    if input.new_password.len() < 8 {
        return Ok(render_profile(
            &state,
            &user,
            None,
            Some("New password must be at least 8 characters."),
        )
        .await?
        .into_response());
    }
    if input.new_password != input.confirm_password {
        return Ok(render_profile(
            &state,
            &user,
            None,
            Some("The two new-password fields don't match."),
        )
        .await?
        .into_response());
    }

    // Re-verify the current password before allowing a change. Stops a
    // hijacked-session attacker from locking the legit user out.
    let mut q = state
        .db
        .query("SELECT VALUE password_hash FROM ONLY $u")
        .bind(("u", user.user_id.clone()))
        .await?;
    let hash: Option<String> = q.take(0)?;
    let hash = hash.ok_or(AppError::Unauthorized)?;

    let ok = verify_password(&input.current_password, &hash).await?;
    if !ok {
        return Ok(
            render_profile(&state, &user, None, Some("Current password is incorrect."))
                .await?
                .into_response(),
        );
    }

    let new_hash = hash_password(&input.new_password).await?;
    state
        .db
        .query("UPDATE $u SET password_hash = $h")
        .bind(("u", user.user_id.clone()))
        .bind(("h", new_hash))
        .await?;

    audit::record(
        &state.db,
        "password_changed",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        None,
    )
    .await;

    Ok(Redirect::to("/app/profile").into_response())
}

/// Receive a multipart form whose single `avatar` field carries the
/// cropped image bytes from the client-side Cropper.js (always PNG,
/// 512×512). We trust the cropper to do the resize — server just
/// stores the bytes and updates the user row.
pub async fn upload_avatar(
    State(state): State<AppState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> Result<Redirect, AppError> {
    let mut bytes: Option<Bytes> = None;
    let mut content_type = String::from("image/png");

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::invalid(format!("upload read failed: {e}")))?
    {
        if field.name().unwrap_or("") != "avatar" {
            continue;
        }
        content_type = field.content_type().unwrap_or("image/png").to_string();
        let buf = field
            .bytes()
            .await
            .map_err(|e| AppError::invalid(format!("read avatar: {e}")))?;
        if buf.len() > AVATAR_MAX_BYTES {
            return Err(AppError::invalid("Avatar file is too large (max 8 MB)."));
        }
        bytes = Some(buf);
        break;
    }

    let bytes = bytes.ok_or_else(|| AppError::invalid("No avatar file in the upload."))?;
    if !content_type.starts_with("image/") {
        return Err(AppError::invalid(
            "Avatar must be an image (PNG/JPEG/WebP).",
        ));
    }

    let key = avatar_key(&crate::db::record_key(&user.user_id));
    state
        .storage
        .put_bytes(&key, bytes.to_vec(), "image/png")
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("store avatar: {e}")))?;

    state
        .db
        .query("UPDATE $u SET avatar_storage_key = $k")
        .bind(("u", user.user_id.clone()))
        .bind(("k", key))
        .await?;

    audit::record(
        &state.db,
        "avatar_updated",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        None,
    )
    .await;

    Ok(Redirect::to("/app/profile"))
}

pub async fn delete_avatar(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Redirect, AppError> {
    let key = avatar_key(&crate::db::record_key(&user.user_id));

    state
        .db
        .query("UPDATE $u SET avatar_storage_key = NONE")
        .bind(("u", user.user_id.clone()))
        .await?;

    if let Err(e) = state.storage.delete(&key).await {
        tracing::warn!(error = %e, %key, "avatar storage delete failed; DB cleared");
    }

    audit::record(
        &state.db,
        "avatar_updated",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some("removed".into()),
    )
    .await;

    Ok(Redirect::to("/app/profile"))
}

// ---------------------------------------------------------------------------
// Avatar serving endpoint
// ---------------------------------------------------------------------------

/// Stream the requested user's avatar bytes.
///
/// Authorization: the caller must be a signed-in user in the *same*
/// brokerage as the target. We don't expose avatars across brokerages —
/// a stranger holding a key can't enumerate other firms' people.
pub async fn serve_avatar(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(target_key): Path<String>,
) -> Result<Response, AppError> {
    use surrealdb::types::{RecordId, SurrealValue};
    let target_id = RecordId::new("user", target_key.as_str());

    // Same-brokerage gate. We compare the works_at edge of the requester
    // against the works_at edge of the target — if neither member has
    // an edge, or the edges point at different brokerages, 404.
    #[derive(serde::Deserialize, SurrealValue)]
    struct Row {
        storage_key: Option<String>,
    }
    let mut q = state
        .db
        .query(
            "SELECT
                (SELECT VALUE avatar_storage_key FROM ONLY $t) AS storage_key
             FROM ONLY $t
             WHERE
                (SELECT VALUE out FROM works_at WHERE in = $t LIMIT 1)[0]
                  = (SELECT VALUE out FROM works_at WHERE in = $u LIMIT 1)[0]",
        )
        .bind(("t", target_id.clone()))
        .bind(("u", user.user_id.clone()))
        .await?;
    let row: Option<Row> = q.take(0).ok().flatten();
    let storage_key = row.and_then(|r| r.storage_key).ok_or(AppError::NotFound)?;

    let bytes = state
        .storage
        .get_bytes(&storage_key)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("fetch avatar: {e}")))?
        .ok_or(AppError::NotFound)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::CACHE_CONTROL, "private, max-age=60")
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("build response: {e}")))
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

async fn render_profile(
    state: &AppState,
    user: &CurrentUser,
    profile_error: Option<&str>,
    password_error: Option<&str>,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(state, user).await?;
    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "profile",
    )
    .with_super_admin(crate::controllers::is_super_admin(state, user))
    .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar);

    render(&ProfilePage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        name: &user.name,
        email: &user.email,
        user_key: crate::db::record_key(&user.user_id),
        has_avatar: user.has_avatar,
        profile_error,
        password_error,
    })
}
