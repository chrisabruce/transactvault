//! Transactional email via Postmark.
//!
//! The mailer is always present in `AppState` but becomes a no-op when
//! `POSTMARK_SERVER_TOKEN` is empty — outbound messages are logged at
//! INFO level instead of delivered. This keeps local development
//! one-command while making production wiring a single env-var flip.
//!
//! ## Why a hand-rolled client
//!
//! The Postmark REST API we touch is a single `POST /email` call. A
//! third-party `postmark`-named crate would add more transitive deps
//! than the ~60 lines of HTTP plumbing here. `reqwest` is already a
//! direct dependency, so the client is just a typed JSON body + a
//! token header.
//!
//! ## Failure mode
//!
//! Every send is fire-and-forget at the call site: we **log and
//! swallow** transport / API errors rather than propagating them up
//! through `?`. The reason is the same as the original Resend
//! implementation — a flaky email provider should never cause a
//! user-facing 500 on signup or invite. The audit log captures the
//! action either way (invite issued, account verified, etc.), so a
//! lost message is recoverable via "Resend email" from the team page.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::EmailConfig;

/// Postmark API endpoint. Their docs (postmarkapp.com/developer) pin
/// this URL — it has been stable since 2014 and there's no regional
/// variant for us to care about.
const POSTMARK_ENDPOINT: &str = "https://api.postmarkapp.com/email";

/// Clonable email transport. Cheap to clone — every member is either a
/// String or an `Arc`-wrapped reqwest client. When `client` is `None`
/// the mailer is in soft-off mode (logs instead of delivers).
#[derive(Clone)]
pub struct Mailer {
    /// `None` when `POSTMARK_SERVER_TOKEN` was empty at startup. Every
    /// send path checks this first and short-circuits to a log line.
    client: Option<PostmarkClient>,
    from: String,
    reply_to: Option<String>,
    message_stream: String,
}

/// Inner Postmark HTTP client. Holds the server token + a reqwest
/// client wrapped in `Arc` so cheap `Clone` of `Mailer` doesn't rebuild
/// the connection pool.
#[derive(Clone)]
struct PostmarkClient {
    http: Arc<reqwest::Client>,
    server_token: String,
}

/// Wire shape Postmark expects. Field names use the `PascalCase` the
/// Postmark JSON API demands — `#[serde(rename_all)]` rewrites them on
/// serialization so the Rust struct stays idiomatic.
#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct SendRequest<'a> {
    from: &'a str,
    to: &'a str,
    subject: &'a str,
    html_body: &'a str,
    text_body: &'a str,
    message_stream: &'a str,
    /// Postmark accepts `ReplyTo` as a comma-separated list, but we
    /// only ever populate a single address. `None` omits the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to: Option<&'a str>,
}

/// Postmark response. We care about `ErrorCode` (`0` = success) and
/// `MessageID` (for the success log) — everything else is logged as
/// the wire string on failure.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SendResponse {
    #[serde(default)]
    error_code: i64,
    #[serde(default)]
    message_id: String,
    #[serde(default)]
    message: String,
}

impl Mailer {
    pub fn new(cfg: &EmailConfig) -> Self {
        let client = if cfg.is_enabled() {
            // The default reqwest client is fine: connection pooling
            // is on, default TLS via rustls (per Cargo.toml feature).
            // Wrap in Arc so cheap-clone semantics survive into every
            // `AppState` clone the router hands out per request.
            Some(PostmarkClient {
                http: Arc::new(reqwest::Client::new()),
                server_token: cfg.server_token.clone(),
            })
        } else {
            tracing::warn!(
                "POSTMARK_SERVER_TOKEN is empty — email delivery is disabled (messages will be logged)"
            );
            None
        };
        Self {
            client,
            from: cfg.from.clone(),
            reply_to: cfg.reply_to.clone(),
            message_stream: cfg.message_stream.clone(),
        }
    }

    /// Send a rendered message with the configured default `Reply-To`
    /// (or none, if not configured). Logs and swallows transport errors
    /// so a flaky provider never produces a user-facing 500.
    async fn send(&self, to: &str, subject: &str, html: String, text: String) {
        self.send_inner(to, subject, &html, &text, self.reply_to.as_deref())
            .await;
    }

    /// Send with a per-message `Reply-To` override. Falls back to the
    /// configured global reply-to when the per-message value is empty,
    /// so callers that forget to populate it still route replies
    /// somewhere sensible.
    async fn send_with_reply(
        &self,
        to: &str,
        reply_to: &str,
        subject: &str,
        html: String,
        text: String,
    ) {
        let effective = if reply_to.is_empty() {
            self.reply_to.as_deref()
        } else {
            Some(reply_to)
        };
        self.send_inner(to, subject, &html, &text, effective).await;
    }

    /// The one place that talks to Postmark. Both public `send` paths
    /// funnel through here so the soft-off log line, error handling,
    /// and request shape all live in a single function.
    async fn send_inner(
        &self,
        to: &str,
        subject: &str,
        html: &str,
        text: &str,
        reply_to: Option<&str>,
    ) {
        let Some(client) = self.client.as_ref() else {
            tracing::info!(%to, %subject, "email (suppressed — no POSTMARK_SERVER_TOKEN)");
            return;
        };

        let body = SendRequest {
            from: &self.from,
            to,
            subject,
            html_body: html,
            text_body: text,
            message_stream: &self.message_stream,
            reply_to,
        };

        let response = client
            .http
            .post(POSTMARK_ENDPOINT)
            .header("Accept", "application/json")
            .header("X-Postmark-Server-Token", &client.server_token)
            .json(&body)
            .send()
            .await;

        match response {
            Ok(resp) => {
                let status = resp.status();
                match resp.json::<SendResponse>().await {
                    Ok(parsed) if parsed.error_code == 0 => {
                        tracing::info!(
                            %to,
                            id = %parsed.message_id,
                            %subject,
                            "email sent"
                        );
                    }
                    Ok(parsed) => {
                        // Postmark non-zero ErrorCode — message rejected
                        // by the API (e.g. unverified Sender Signature,
                        // recipient on suppression list). We log every
                        // detail Postmark gives us so the next admin
                        // troubleshooting an undeliverable invite can
                        // act on the code, not guess.
                        tracing::warn!(
                            %to,
                            %subject,
                            error_code = parsed.error_code,
                            message = %parsed.message,
                            "postmark rejected message"
                        );
                    }
                    Err(e) => {
                        // 2xx with an unparseable body shouldn't happen,
                        // but if it does we still want to know the
                        // status that came back.
                        tracing::warn!(
                            %to,
                            %subject,
                            %status,
                            error = %e,
                            "postmark response body could not be parsed"
                        );
                    }
                }
            }
            Err(err) => {
                tracing::warn!(%to, %subject, error = %err, "postmark request failed");
            }
        }
    }

    /// Verify-your-email — first message a new signup receives. Until
    /// they click the link the account is in `pending_verification`
    /// and they can't log in. This is what stops the "users got
    /// welcome emails but didn't sign up" abuse vector.
    pub async fn send_verify(&self, to: &str, name: &str, link: &str) {
        // Log the link in dev (when delivery is disabled) so testers
        // can copy-paste from the log instead of needing real email.
        if self.client.is_none() {
            tracing::info!(%to, %link, "verify link (dev — email suppressed)");
        }
        let html = format!(
            "<!doctype html><html><body style=\"font-family:system-ui,sans-serif;color:#0f172a\">\
               <p>Hi {name},</p>\
               <p>Someone — hopefully you — created a TransactVault account with this email \
                  address. Click below within the next 24 hours to activate it.</p>\
               <p><a href=\"{link}\" \
                     style=\"background:#0f766e;color:#fff;padding:10px 18px;\
                            border-radius:8px;text-decoration:none;display:inline-block\">Verify my email</a></p>\
               <p>If the button doesn't work, copy this URL into your browser:<br>{link}</p>\
               <p>If you didn't sign up, you can safely ignore this — the account never \
                  activates and no further emails will be sent.</p>\
               <p>— The TransactVault team</p>\
             </body></html>",
            name = html_escape(name),
            link = link,
        );
        let text = format!(
            "Hi {name},\n\n\
             Someone — hopefully you — created a TransactVault account with this email \
             address. Activate it within 24 hours by visiting:\n\n\
             {link}\n\n\
             If you didn't sign up, ignore this and the account will never activate.\n\n\
             — The TransactVault team\n",
            name = name,
            link = link,
        );
        self.send(to, "Verify your TransactVault email", html, text)
            .await;
    }

    /// Greet a freshly-signed-up broker — sent only AFTER verification,
    /// so it's never delivered to victims of signup abuse.
    pub async fn send_welcome(&self, to: &str, name: &str, brokerage: &str, app_url: &str) {
        let html = format!(
            "<!doctype html><html><body style=\"font-family:system-ui,sans-serif;color:#0f172a\">\
               <p>Hi {name},</p>\
               <p>Welcome to TransactVault — {brokerage} is ready to go.</p>\
               <p>Jump in and create your first transaction:</p>\
               <p><a href=\"{app_url}/app\" \
                     style=\"background:#0f766e;color:#fff;padding:10px 18px;\
                            border-radius:8px;text-decoration:none;display:inline-block\">Open dashboard</a></p>\
               <p>Need a hand? Just reply to this email.</p>\
               <p>— The TransactVault team</p>\
             </body></html>",
            name = html_escape(name),
            brokerage = html_escape(brokerage),
            app_url = app_url,
        );
        let text = format!(
            "Hi {name},\n\n\
             Welcome to TransactVault — {brokerage} is ready to go.\n\n\
             Open your dashboard: {app_url}/app\n\n\
             — The TransactVault team\n",
        );
        self.send(to, "Welcome to TransactVault", html, text).await;
    }

    /// Notify the invitee that somebody added them to a brokerage.
    ///
    /// `inviter_email` is wired into the message's `Reply-To` so that
    /// replies bypass the no-reply From address and land in the actual
    /// inviter's inbox. This also nudges Gmail / Outlook spam filters
    /// to treat the message as person-to-person rather than bulk —
    /// the single biggest deliverability lever we have without touching
    /// DNS records.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_invite(
        &self,
        to: &str,
        inviter: &str,
        inviter_email: &str,
        brokerage: &str,
        role: &str,
        link: &str,
        is_existing_user: bool,
    ) {
        let role_label = match role {
            "broker" => "Broker",
            "coordinator" => "Compliance Officer",
            _ => "Agent",
        };
        let subject = format!("{inviter} invited you to {brokerage} on TransactVault");
        // Existing accounts don't need to pick a password — they just
        // need to log in and accept. New accounts get the "create a
        // password" framing instead.
        let action_line = if is_existing_user {
            "To join, open this link and sign in to your existing TransactVault account:"
        } else {
            "To finish setting up your account, open this link and create a password:"
        };
        let cta_label = if is_existing_user {
            "Sign in to accept"
        } else {
            "Accept invitation"
        };

        // Conversational tone, no exclamation marks, no "Click here!",
        // and a clear opt-out path — all of which lower spam scores.
        let html = format!(
            "<!doctype html><html><body style=\"font-family:system-ui,sans-serif;color:#0f172a;\
                                                 max-width:560px;margin:0 auto;padding:1rem\">\
               <p>Hi,</p>\
               <p><strong>{inviter}</strong> added you to <strong>{brokerage}</strong> on \
                  TransactVault as a {role_label}.</p>\
               <p>{action_line}</p>\
               <p><a href=\"{link}\" \
                     style=\"background:#0f766e;color:#fff;padding:10px 18px;\
                            border-radius:8px;text-decoration:none;display:inline-block\">{cta_label}</a></p>\
               <p style=\"color:#475569;font-size:0.9em\">If the button doesn't work, paste this URL into your browser:<br>\
                  <span style=\"word-break:break-all\">{link}</span></p>\
               <p style=\"color:#475569;font-size:0.9em\">If you weren't expecting this, just ignore the email — \
                  the invitation expires automatically. You can also reply directly to {inviter_email} \
                  if you have questions.</p>\
             </body></html>",
            inviter = html_escape(inviter),
            brokerage = html_escape(brokerage),
            inviter_email = html_escape(inviter_email),
            role_label = role_label,
            link = link,
        );
        let text = format!(
            "Hi,\n\n\
             {inviter} added you to {brokerage} on TransactVault as a {role_label}.\n\n\
             {action_line}\n\n\
             {link}\n\n\
             If you weren't expecting this, just ignore this email — the invitation expires \
             automatically. You can also reply directly to {inviter_email} if you have questions.\n\n\
             — The TransactVault team\n",
        );
        self.send_with_reply(to, inviter_email, &subject, html, text)
            .await;
    }

    /// Notify a broker that their tier's monthly price has changed.
    /// The new amount applies on the next billing cycle, so this is
    /// purely informational — Stripe handles the proration when their
    /// subscription renews.
    pub async fn send_price_change(
        &self,
        to: &str,
        broker_name: &str,
        tier_name: &str,
        old_price_display: &str,
        new_price_display: &str,
        app_url: &str,
    ) {
        let subject = format!("Update to your {tier_name} plan pricing");
        let html = format!(
            "<!doctype html><html><body style=\"font-family:system-ui,sans-serif;color:#0f172a;\
                                                 max-width:560px;margin:0 auto;padding:1rem\">\
               <p>Hi {name},</p>\
               <p>We're writing to let you know that the price of the <strong>{tier}</strong> plan is changing.</p>\
               <p style=\"font-size:1.1em\"><span style=\"color:#475569;text-decoration:line-through\">{old}/mo</span> &rarr; <strong>{new}/mo</strong></p>\
               <p>Your current billing cycle is unaffected — the new rate applies on your next renewal.</p>\
               <p>If you'd like to switch plans or cancel, you can do that any time from <a href=\"{app_url}/app/billing/portal\">Manage subscription</a>.</p>\
               <p>— The TransactVault team</p>\
             </body></html>",
            name = html_escape(broker_name),
            tier = html_escape(tier_name),
            old = html_escape(old_price_display),
            new = html_escape(new_price_display),
            app_url = app_url,
        );
        let text = format!(
            "Hi {broker_name},\n\n\
             The price of the {tier_name} plan is changing.\n\
             {old_price_display}/mo -> {new_price_display}/mo\n\n\
             Your current billing cycle is unaffected — the new rate applies on your next renewal.\n\
             Manage subscription: {app_url}/app/billing/portal\n\n\
             — The TransactVault team\n",
        );
        self.send(to, &subject, html, text).await;
    }

    /// Friendly reminder that the broker's free trial ends in a few
    /// days. Triggered by Stripe's `customer.subscription.trial_will_end`
    /// webhook (3 days before the trial actually ends).
    pub async fn send_trial_ending(
        &self,
        to: &str,
        broker_name: &str,
        trial_end_display: &str,
        app_url: &str,
    ) {
        let subject = "Your TransactVault trial ends soon".to_string();
        let html = format!(
            "<!doctype html><html><body style=\"font-family:system-ui,sans-serif;color:#0f172a;\
                                                 max-width:560px;margin:0 auto;padding:1rem\">\
               <p>Hi {name},</p>\
               <p>Just a heads-up — your free trial ends on <strong>{when}</strong>. After that, your card will be charged for the first paid month.</p>\
               <p>If you'd like to switch plans or cancel before then, you can do that from <a href=\"{app_url}/app/billing/portal\">Manage subscription</a>.</p>\
               <p>No action needed if you want to continue — we'll keep things running.</p>\
               <p>— The TransactVault team</p>\
             </body></html>",
            name = html_escape(broker_name),
            when = html_escape(trial_end_display),
            app_url = app_url,
        );
        let text = format!(
            "Hi {broker_name},\n\n\
             Your free trial ends on {trial_end_display}. After that, your card will be charged for the first paid month.\n\n\
             Manage subscription: {app_url}/app/billing/portal\n\n\
             No action needed if you want to continue.\n\n\
             — The TransactVault team\n",
        );
        self.send(to, &subject, html, text).await;
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    //! The mailer's network paths can't be exercised without a live
    //! Postmark server, so these tests pin the construction surface
    //! and the soft-off behavior — the two things most likely to
    //! silently regress when somebody touches `EmailConfig`.

    use super::*;

    #[test]
    fn empty_token_yields_soft_off_mailer() {
        let cfg = EmailConfig {
            server_token: String::new(),
            from: "x@y".into(),
            reply_to: None,
            message_stream: "outbound".into(),
        };
        let m = Mailer::new(&cfg);
        assert!(
            m.client.is_none(),
            "empty POSTMARK_SERVER_TOKEN must produce a no-op mailer"
        );
    }

    #[test]
    fn token_present_builds_a_client() {
        let cfg = EmailConfig {
            server_token: "test-token".into(),
            from: "x@y".into(),
            reply_to: Some("reply@y".into()),
            message_stream: "outbound".into(),
        };
        let m = Mailer::new(&cfg);
        assert!(
            m.client.is_some(),
            "non-empty token must yield a Postmark client"
        );
        assert_eq!(m.reply_to.as_deref(), Some("reply@y"));
        assert_eq!(m.message_stream, "outbound");
    }

    #[tokio::test]
    async fn soft_off_send_does_not_panic_and_returns_quickly() {
        // Belt-and-suspenders: if anything is wired wrong, the soft-off
        // path that runs in dev (and on every empty-token deploy) would
        // be the first thing to break. Tokio's runtime is enough; no
        // network reachable here.
        let cfg = EmailConfig {
            server_token: String::new(),
            from: "x@y".into(),
            reply_to: None,
            message_stream: "outbound".into(),
        };
        let m = Mailer::new(&cfg);
        m.send_verify("user@example", "Test", "https://app.test/verify/abc")
            .await;
        m.send_welcome("user@example", "Test", "Acme RE", "https://app.test")
            .await;
    }
}
