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
    pub async fn send_invite(
        &self,
        to: &str,
        inviter: &str,
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
        let html = format!(
            "<!doctype html><html><body style=\"font-family:system-ui,sans-serif;color:#0f172a\">\
               <p>Hi,</p>\
               <p><strong>{inviter}</strong> added you to <strong>{brokerage}</strong> on \
                  TransactVault as a <strong>{role_label}</strong>.</p>\
               <p>Accept the invitation and create your login:</p>\
               <p><a href=\"{link}\" \
                     style=\"background:#0f766e;color:#fff;padding:10px 18px;\
                            border-radius:8px;text-decoration:none;display:inline-block\">Accept invitation</a></p>\
               <p>If the button doesn't work, copy this link into your browser:<br>{link}</p>\
             </body></html>",
            inviter = html_escape(inviter),
            brokerage = html_escape(brokerage),
            role_label = role_label,
            link = link,
        );
        let text = format!(
            "{inviter} added you to {brokerage} on TransactVault as a {role_label}.\n\n\
             Accept the invitation: {link}\n",
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
