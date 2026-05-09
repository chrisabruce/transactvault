//! Team page — brokers view and invite members.

use axum::Form;
use axum::extract::State;
use axum::response::Html;
use serde::Deserialize;

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
            "SELECT in.name AS name, in.email AS email, role
             FROM works_at WHERE out = $b",
        )
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    use surrealdb::types::SurrealValue;
    #[derive(serde::Deserialize, SurrealValue)]
    struct Row {
        name: String,
        email: String,
        role: String,
    }
    let rows: Vec<Row> = response.take(0)?;

    let members = rows
        .into_iter()
        .filter_map(|r| Role::parse(&r.role).map(|role| Member::new(r.name, r.email, role)))
        .collect();
    Ok(members)
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
