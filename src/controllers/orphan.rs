//! "No brokerage" landing + invite accept/decline for authenticated
//! users who aren't currently attached to any brokerage.
//!
//! Two ways to land here:
//!   1. Login of a user whose `works_at` edge is gone (newly removed
//!      agent, or brokerage was deleted). `auth::login` redirects them.
//!   2. The team page's "Remove from team" action sends the removed
//!      agent's next request here once their next page hit fails the
//!      `CurrentUser` extractor.
//!
//! From here they see any pending invitations for their email and can
//! either accept (joins the brokerage, creates the `works_at` edge) or
//! decline (marks the invitation `declined = true` so it stops appearing
//! and the broker sees it left the pending list).

use axum::extract::{Path, State};
use axum::response::{Html, Redirect};

use crate::auth::middleware::LooseCurrentUser;
use crate::controllers::render;
use crate::error::AppError;
use crate::models::Invitation;
use crate::state::AppState;
use crate::templates::{NoBrokerageInvite, NoBrokeragePage};

pub async fn landing(
    State(state): State<AppState>,
    user: LooseCurrentUser,
) -> Result<Html<String>, AppError> {
    // If the user IS attached to a brokerage, bounce them to `/app` —
    // this page only makes sense for orphaned accounts.
    if user.membership.is_some() {
        return render(&NoBrokeragePage {
            app_name: &state.config.app_name,
            base_url: &state.config.base_url,
            signed_in: true,
            user_name: &user.name,
            user_email: &user.email,
            invitations: Vec::new(),
            redirect_now: true,
        });
    }

    let invitations = load_pending_for_email(&state, &user.email).await?;
    render(&NoBrokeragePage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        user_name: &user.name,
        user_email: &user.email,
        invitations,
        redirect_now: false,
    })
}

pub async fn accept(
    State(state): State<AppState>,
    user: LooseCurrentUser,
    Path(token): Path<String>,
) -> Result<Redirect, AppError> {
    // Orphan accept path: user already exists, just needs the
    // `works_at` edge. If they somehow already have a brokerage, fall
    // through to a redirect rather than racing membership.
    if user.membership.is_some() {
        return Ok(Redirect::to("/app"));
    }

    let invite = load_invitation_for_email(&state, &token, &user.email).await?;

    state
        .db
        .query("RELATE $u->works_at->$b SET role = $r")
        .bind(("u", user.user_id.clone()))
        .bind(("b", invite.brokerage.clone()))
        .bind(("r", invite.role.clone()))
        .await?;

    state
        .db
        .query("UPDATE $i SET accepted = true")
        .bind(("i", invite.id.clone()))
        .await?;

    let target_user_id = user.user_id.clone();
    let invite_brokerage = invite.brokerage.clone();

    crate::audit::record(
        &state.db,
        "invite_accepted",
        Some(user.user_id),
        Some(user.email),
        None,
        None,
        None,
    )
    .await;

    // Brokerage-side: the joined brokerage's dashboards may want to
    // surface the new member (counts, etc.).
    state
        .events
        .publish(crate::events::Event::BrokerageMutation(invite_brokerage));
    // Target-user-side: their membership just transitioned from "none"
    // to "member" — any live stream they had open against the
    // no-brokerage landing should drop and reconnect against /app.
    state
        .events
        .publish(crate::events::Event::UserMembershipChanged(target_user_id));

    Ok(Redirect::to("/app"))
}

pub async fn decline(
    State(state): State<AppState>,
    user: LooseCurrentUser,
    Path(token): Path<String>,
) -> Result<Redirect, AppError> {
    let invite = load_invitation_for_email(&state, &token, &user.email).await?;

    state
        .db
        .query("UPDATE $i SET declined = true")
        .bind(("i", invite.id.clone()))
        .await?;

    crate::audit::record(
        &state.db,
        "invite_declined",
        Some(user.user_id),
        Some(user.email),
        None,
        None,
        None,
    )
    .await;

    Ok(Redirect::to("/app/no-brokerage"))
}

async fn load_pending_for_email(
    state: &AppState,
    email: &str,
) -> Result<Vec<NoBrokerageInvite>, AppError> {
    let mut q = state
        .db
        .query(
            "SELECT *, brokerage.name AS brokerage_name, invited_by.name AS inviter_name
             FROM invitation
             WHERE email = $e AND accepted = false AND declined = false
             ORDER BY created_at DESC",
        )
        .bind(("e", email.to_string()))
        .await?;
    use serde::Deserialize;
    use surrealdb::types::SurrealValue;
    #[derive(Debug, Deserialize, SurrealValue)]
    struct Row {
        token: String,
        role: String,
        brokerage_name: String,
        inviter_name: String,
    }
    let rows: Vec<Row> = q.take(0).unwrap_or_default();
    Ok(rows
        .into_iter()
        .map(|r| NoBrokerageInvite {
            token: r.token,
            role: r.role,
            brokerage_name: r.brokerage_name,
            inviter_name: r.inviter_name,
        })
        .collect())
}

/// Verify the token's invitation is pending and that the email on it
/// matches the signed-in user's email — otherwise this would let
/// anyone with a token claim a seat in someone else's name.
async fn load_invitation_for_email(
    state: &AppState,
    token: &str,
    email: &str,
) -> Result<Invitation, AppError> {
    let mut q = state
        .db
        .query(
            "SELECT * FROM invitation
             WHERE token = $t AND accepted = false AND declined = false
             LIMIT 1",
        )
        .bind(("t", token.to_string()))
        .await?;
    let invite: Option<Invitation> = q.take(0)?;
    let invite = invite.ok_or(AppError::NotFound)?;
    if invite.email != email {
        // Same 404 whether the row is missing or for a different user —
        // avoid leaking the existence of cross-account invitations.
        return Err(AppError::NotFound);
    }
    Ok(invite)
}
