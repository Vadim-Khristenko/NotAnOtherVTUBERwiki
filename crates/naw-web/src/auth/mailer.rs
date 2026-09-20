//! Outbound mail. The `log` backend records the letter in `mail_outbox`
//! and serves it at `/dev/mailbox`, which is how dev runs without any
//! mail infrastructure. The SMTP backend (Mailpit in compose) lands with
//! T9; the call sites stay the same.

use uuid::Uuid;

use naw_core::error::AppError;

pub struct MailDraft {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// Sends through the configured backend. Dev runs `log`; SMTP returns a
/// clear error until its transport exists (T9).
pub async fn send(state: &naw_core::state::AppState, draft: MailDraft) -> Result<(), AppError> {
    match state.config.auth.mail.backend {
        naw_core::config::MailBackend::Log => send_log(&state.db, draft).await,
        naw_core::config::MailBackend::Smtp => {
            // T9 wires lettre here; until then SMTP is an honest 500.
            tracing::error!("smtp backend selected but the transport is not wired yet");
            Err(AppError::Internal)
        }
    }
}

async fn send_log(db: &sqlx::PgPool, draft: MailDraft) -> Result<(), AppError> {
    sqlx::query!(
        "INSERT INTO mail_outbox (id, recipient, subject, body) VALUES ($1, $2, $3, $4)",
        Uuid::new_v4(),
        draft.to,
        draft.subject,
        draft.body
    )
    .execute(db)
    .await?;
    tracing::info!(to = %draft.to, subject = %draft.subject, "mail stored for /dev/mailbox");
    Ok(())
}

/// Builds the verification link and letter body. The token goes in the
/// URL once, only its sha256 is stored.
pub fn verification_email(base_url: &str, token: &str) -> MailDraft {
    MailDraft {
        to: String::new(),
        subject: "Confirm your email for NotAnotherWiki".to_string(),
        body: format!(
            "Someone signed in to NotAnotherWiki and left this address.\r\n\
             If it was you, confirm here:\r\n\r\n{base_url}/verify-email?token={token}\r\n\r\n\
             The link works once and expires in 24 hours.\r\n"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_email_carries_a_single_use_link() {
        let draft = verification_email("http://127.0.0.1:4242", "tok123");
        assert!(draft.body.contains("/verify-email?token=tok123"));
        assert!(draft.body.contains("expires in 24 hours"));
        assert!(draft.subject.contains("NotAnotherWiki"));
    }
}
