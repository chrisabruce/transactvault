//! Public marketing pages — landing, pricing, and the brand book.

use axum::extract::State;
use axum::response::Html;

use crate::auth::middleware::MaybeCurrentUser;
use crate::controllers::render;
use crate::error::AppError;
use crate::state::AppState;
use crate::templates::{BrandPage, LandingPage, PRICING_PLANS, PricingPage};

pub async fn landing(
    State(state): State<AppState>,
    MaybeCurrentUser(user): MaybeCurrentUser,
) -> Result<Html<String>, AppError> {
    render(&LandingPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: user.is_some(),
    })
}

pub async fn pricing(
    State(state): State<AppState>,
    MaybeCurrentUser(user): MaybeCurrentUser,
) -> Result<Html<String>, AppError> {
    render(&PricingPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: user.is_some(),
        plans: PRICING_PLANS,
    })
}

pub async fn brand(
    State(state): State<AppState>,
    MaybeCurrentUser(user): MaybeCurrentUser,
) -> Result<Html<String>, AppError> {
    render(&BrandPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: user.is_some(),
    })
}
