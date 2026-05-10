//! Transactional email via Resend.
//!
//! The mailer is always present in `AppState` but becomes a no-op when
//! `RESEND_API_KEY` is empty — outbound messages are logged at INFO level
//! instead of delivered. This keeps local development one-command while
//! making production wiring a single env-var flip.

use resend_rs::Resend;
use resend_rs::types::CreateEmailBaseOptions;

use crate::config::EmailConfig;

/// Clonable email transport. Safe to clone — `Resend` internally wraps an
/// `Arc<reqwest::Client>`, and the config is a small struct of Strings.
#[derive(Clone)]
pub struct Mailer {
    client: Option<Resend>,
    from: String,
    reply_to: Option<String>,
}

impl Mailer {
    pub fn new(cfg: &EmailConfig) -> Self {
        let client = cfg.is_enabled().then(|| Resend::new(&cfg.api_key));
        if client.is_none() {
            tracing::warn!("RESEND_API_KEY is empty — email delivery is disabled");
        }
        Self {
            client,
            from: cfg.from.clone(),
            reply_to: cfg.reply_to.clone(),
        }
    }

    /// Send a rendered message. Logs and swallows transport errors: a flaky
    /// email provider should never cause a user-facing 500 on signup or
    /// invite, so we warn-and-continue rather than propagate.
    async fn send(&self, to: &str, subject: &str, html: String, text: String) {
        let Some(client) = self.client.as_ref() else {
            tracing::info!(%to, %subject, "email (suppressed — no RESEND_API_KEY)");
            return;
        };

        let mut opts = CreateEmailBaseOptions::new(&self.from, [to.to_string()], subject)
            .with_html(&html)
            .with_text(&text);
        if let Some(reply) = self.reply_to.as_deref() {
            opts = opts.with_reply(reply);
        }

        match client.emails.send(opts).await {
            Ok(resp) => tracing::info!(%to, id = ?resp.id, %subject, "email sent"),
            Err(err) => tracing::warn!(%to, %subject, error = %err, "email send failed"),
        }
    }

    /// Verify-your-email — first message a new signup receives. Until they
    /// click the link, the account is in `pending_verification` and they
    /// can't log in. This is what stops the "users got welcome emails but
    /// didn't sign up" abuse vector.
    pub async fn send_verify(&self, to: &str, name: &str, link: &str) {
        // Log the link in dev (when delivery is disabled) so testers can
        // copy-paste from the log instead of needing real email.
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
        );
        self.send(to, "Verify your TransactVault email", html, text)
            .await;
    }

    /// Greet a freshly-signed-up broker — sent only AFTER verification, so
    /// it's never delivered to victims of signup abuse.
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
    pub async fn send_invite(
        &self,
        to: &str,
        inviter: &str,
        inviter_email: &str,
        brokerage: &str,
        role: &str,
        link: &str,
    ) {
        let role_label = match role {
            "broker" => "Broker",
            "coordinator" => "Transaction Coordinator",
            _ => "Agent",
        };
        let subject = format!("{inviter} invited you to {brokerage} on TransactVault");

        // Conversational tone, no exclamation marks, no "Click here!",
        // and a clear opt-out path — all of which lower spam scores.
        let html = format!(
            "<!doctype html><html><body style=\"font-family:system-ui,sans-serif;color:#0f172a;\
                                                 max-width:560px;margin:0 auto;padding:1rem\">\
               <p>Hi,</p>\
               <p><strong>{inviter}</strong> added you to <strong>{brokerage}</strong> on \
                  TransactVault as a {role_label}.</p>\
               <p>To finish setting up your account, open this link and create a password:</p>\
               <p><a href=\"{link}\" \
                     style=\"background:#0f766e;color:#fff;padding:10px 18px;\
                            border-radius:8px;text-decoration:none;display:inline-block\">Accept invitation</a></p>\
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
             To finish setting up your account, open this link and create a password:\n\n\
             {link}\n\n\
             If you weren't expecting this, just ignore this email — the invitation expires \
             automatically. You can also reply directly to {inviter_email} if you have questions.\n\n\
             — The TransactVault team\n",
        );
        self.send_with_reply(to, inviter_email, &subject, html, text).await;
    }

    /// Internal: same as `send`, but with a per-message `Reply-To` override.
    /// Used for invites so replies route to the inviter.
    async fn send_with_reply(
        &self,
        to: &str,
        reply_to: &str,
        subject: &str,
        html: String,
        text: String,
    ) {
        let Some(client) = self.client.as_ref() else {
            tracing::info!(%to, %reply_to, %subject, "email (suppressed — no RESEND_API_KEY)");
            return;
        };

        let mut opts = CreateEmailBaseOptions::new(&self.from, [to.to_string()], subject)
            .with_html(&html)
            .with_text(&text)
            .with_reply(reply_to);
        // Fall back to the configured global reply-to if the per-message
        // value happens to be empty — keeps existing config-driven setups
        // working even when a caller forgets to populate it.
        if reply_to.is_empty()
            && let Some(global) = self.reply_to.as_deref()
        {
            opts = opts.with_reply(global);
        }

        match client.emails.send(opts).await {
            Ok(resp) => tracing::info!(%to, id = ?resp.id, %subject, "email sent"),
            Err(err) => tracing::warn!(%to, %subject, error = %err, "email send failed"),
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
