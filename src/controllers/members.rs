//! Team page — brokers view and invite members.

use axum::Form;
use axum::extract::{Path, State};
use axum::response::{Html, Redirect};
use serde::Deserialize;
use surrealdb::types::RecordId;

use crate::auth::{CurrentUser, Role};
use crate::controllers::auth::create_invitation;
use crate::controllers::render;
use crate::controllers::transactions::load_brokerage;
use crate::error::AppError;
use crate::models::Invitation;
use crate::state::AppState;
use crate::templates::{AppHeader, Member, TeamPage};

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(&state, &user).await?;

    let members = load_members(&state, &user).await?;
    let pending = load_pending_invitations(&state, &user).await?;

    let header = AppHeader::new(&user.name, &user.email, user.role, &brokerage.name, "team")
        .with_super_admin(crate::controllers::is_super_admin(&state, &user));
    render(&TeamPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        members,
        pending,
        invite_error: None,
        invite_link: None,
    })
}

#[derive(Debug, Deserialize)]
pub struct InviteInput {
    pub email: String,
    pub role: String,
}

pub async fn invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(input): Form<InviteInput>,
) -> Result<Html<String>, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }

    let brokerage = load_brokerage(&state, &user).await?;
    let header = AppHeader::new(&user.name, &user.email, user.role, &brokerage.name, "team")
        .with_super_admin(crate::controllers::is_super_admin(&state, &user));

    let email = input.email.trim().to_ascii_lowercase();
    let role = match input.role.as_str() {
        "broker" | "agent" | "coordinator" => input.role,
        _ => {
            return render(&TeamPage {
                app_name: &state.config.app_name,
                base_url: &state.config.base_url,
                signed_in: true,
                header,
                members: load_members(&state, &user).await?,
                pending: load_pending_invitations(&state, &user).await?,
                invite_error: Some("Role must be broker, agent, or coordinator."),
                invite_link: None,
            });
        }
    };

    if email.is_empty() || !email.contains('@') {
        return render(&TeamPage {
            app_name: &state.config.app_name,
            base_url: &state.config.base_url,
            signed_in: true,
            header,
            members: load_members(&state, &user).await?,
            pending: load_pending_invitations(&state, &user).await?,
            invite_error: Some("Please enter a valid email address."),
            invite_link: None,
        });
    }

    let invitation = create_invitation(
        &state,
        email,
        role,
        user.brokerage_id.clone(),
        &brokerage.name,
        user.user_id.clone(),
        &user.name,
        &user.email,
    )
    .await?;

    let link = format!("{}/invite/{}", state.config.base_url, invitation.token);

    render(&TeamPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        members: load_members(&state, &user).await?,
        pending: load_pending_invitations(&state, &user).await?,
        invite_error: None,
        invite_link: Some(link),
    })
}

// ---------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------

async fn load_members(state: &AppState, user: &CurrentUser) -> Result<Vec<Member>, AppError> {
    let mut response = state
        .db
        .query(
            "SELECT in AS user_id, in.name AS name, in.email AS email, role
             FROM works_at WHERE out = $b",
        )
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    use surrealdb::types::SurrealValue;
    #[derive(serde::Deserialize, SurrealValue)]
    struct Row {
        user_id: RecordId,
        name: String,
        email: String,
        role: String,
    }
    let rows: Vec<Row> = response.take(0)?;

    let members = rows
        .into_iter()
        .filter_map(|r| {
            Role::parse(&r.role).map(|role| {
                let is_self = r.user_id == user.user_id;
                let key = crate::record_key(&r.user_id);
                Member::new(key, r.name, r.email, role, is_self)
            })
        })
        .collect();
    Ok(members)
}

#[derive(Debug, Deserialize)]
pub struct ChangeRoleInput {
    pub role: String,
}

pub async fn change_role(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(target_user_key): Path<String>,
    Form(input): Form<ChangeRoleInput>,
) -> Result<Redirect, AppError> {
    if !user.role.can_change_roles() {
        return Err(AppError::Forbidden);
    }

    let new_role = Role::parse(&input.role)
        .ok_or_else(|| AppError::invalid("Role must be broker, agent, or coordinator."))?;

    let target_user_id = RecordId::new("user", target_user_key.as_str());

    // Verify the target is actually a member of this brokerage. The query
    // returns the existing role if the works_at edge is found.
    let mut existing_q = state
        .db
        .query(
            "SELECT VALUE role FROM works_at \
             WHERE in = $u AND out = $b LIMIT 1",
        )
        .bind(("u", target_user_id.clone()))
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let existing: Vec<String> = existing_q.take(0)?;
    let existing_role_str = existing.into_iter().next().ok_or(AppError::NotFound)?;
    let existing_role = Role::parse(&existing_role_str).ok_or(AppError::NotFound)?;

    // Guard: prevent demoting the last broker. We count brokers in this
    // brokerage; if the target is a broker and there's only one, reject.
    if existing_role.is_broker() && !new_role.is_broker() {
        let mut count_q = state
            .db
            .query(
                "SELECT count() FROM works_at \
                 WHERE out = $b AND role = 'broker' GROUP ALL",
            )
            .bind(("b", user.brokerage_id.clone()))
            .await?;
        use surrealdb::types::SurrealValue;
        #[derive(serde::Deserialize, SurrealValue)]
        struct CountRow {
            count: i64,
        }
        let count: Option<CountRow> = count_q.take(0)?;
        let broker_count = count.map(|c| c.count).unwrap_or(0);
        if broker_count <= 1 {
            return Err(AppError::invalid(
                "There must be at least one broker on the brokerage.",
            ));
        }
    }

    state
        .db
        .query("UPDATE works_at SET role = $r WHERE in = $u AND out = $b")
        .bind(("u", target_user_id))
        .bind(("b", user.brokerage_id.clone()))
        .bind(("r", new_role.as_str().to_string()))
        .await?;

    Ok(Redirect::to("/app/team"))
}

async fn load_pending_invitations(
    state: &AppState,
    user: &CurrentUser,
) -> Result<Vec<Invitation>, AppError> {
    let mut response = state
        .db
        .query(
            "SELECT * FROM invitation
             WHERE brokerage = $b AND accepted = false
             ORDER BY created_at DESC",
        )
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let invitations: Vec<Invitation> = response.take(0)?;
    Ok(invitations)
}

// ---------------------------------------------------------------------------
// Invite management — resend + cancel
// ---------------------------------------------------------------------------

/// Look up an unaccepted invitation by token and verify it belongs to the
/// current broker's brokerage. Used by both `resend_invite` and
/// `cancel_invite` so the same authorization rule applies to both.
async fn authorize_invite(
    state: &AppState,
    user: &CurrentUser,
    token: &str,
) -> Result<Invitation, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let mut response = state
        .db
        .query(
            "SELECT * FROM invitation
             WHERE token = $t AND accepted = false LIMIT 1",
        )
        .bind(("t", token.to_string()))
        .await?;
    let invite: Option<Invitation> = response.take(0)?;
    let invite = invite.ok_or(AppError::NotFound)?;
    if invite.brokerage != user.brokerage_id {
        // Don't leak whether the token exists in another brokerage —
        // 404 is the same whether the row is absent or off-tenant.
        return Err(AppError::NotFound);
    }
    Ok(invite)
}

/// Re-fire the invite email for an existing pending invitation. Reuses the
/// same token, so any link the recipient already has stays valid.
pub async fn resend_invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(token): Path<String>,
) -> Result<Redirect, AppError> {
    let invite = authorize_invite(&state, &user, &token).await?;
    let brokerage = load_brokerage(&state, &user).await?;
    let link = format!("{}/invite/{}", state.config.base_url, invite.token);

    state
        .mailer
        .send_invite(
            &invite.email,
            &user.name,
            &user.email,
            &brokerage.name,
            &invite.role,
            &link,
        )
        .await;

    crate::audit::record(
        &state.db,
        "invite_resent",
        Some(user.user_id.clone()),
        Some(invite.email.clone()),
        None,
        None,
        Some(format!("role={}", invite.role)),
    )
    .await;

    Ok(Redirect::to("/app/team"))
}

/// Delete a pending invitation. The link stops working immediately —
/// `/invite/{token}` will return 404. Useful for typos in the email
/// address or a teammate who's no longer joining.
pub async fn cancel_invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(token): Path<String>,
) -> Result<Redirect, AppError> {
    let invite = authorize_invite(&state, &user, &token).await?;

    state
        .db
        .query("DELETE $i")
        .bind(("i", invite.id.clone()))
        .await?;

    crate::audit::record(
        &state.db,
        "invite_cancelled",
        Some(user.user_id.clone()),
        Some(invite.email.clone()),
        None,
        None,
        Some(format!("role={}", invite.role)),
    )
    .await;

    Ok(Redirect::to("/app/team"))
}
