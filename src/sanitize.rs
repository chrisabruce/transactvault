//! Neutralizing untrusted text before it reaches a non-HTML sink.
//!
//! Askama escapes everything rendered into a template, so HTML is covered.
//! This module covers the sinks that Askama never sees, where the danger
//! is not `<script>` but characters that *restructure* the output:
//!
//! * **Log records.** `PRETTY_LOGS` is on by default in production, and
//!   `tracing`'s `%value` sigil formats through `Display` with no escaping
//!   whatsoever. A newline inside a user-controlled field therefore ends
//!   the record and starts one an operator cannot distinguish from a real
//!   one; an ANSI escape can clear their terminal mid-`docker logs`.
//! * **Export manifests.** `MANIFEST.txt` inside a compliance ZIP is
//!   newline-delimited `Key: value`, so a newline in a property address
//!   forges lines an auditor reads as fact.
//! * **Filenames and ZIP entries.** Beyond the C0 controls that
//!   `char::is_control` already catches, the Unicode bidi overrides make a
//!   displayed name disagree with the real one: `Contract\u{202E}gpj.exe`
//!   renders as `Contractexe.jpg`.
//!
//! [`is_unsafe_text_char`] is the single definition of "character that has
//! no business in user-supplied text", so the filename, manifest, log, and
//! input-validation paths cannot drift apart.

/// Characters that are never legitimate in user-supplied single-line text.
///
/// Three families, all invisible and all capable of making rendered output
/// disagree with the bytes behind it:
///
/// * C0/C1 controls (`char::is_control`) — newline, carriage return, NUL,
///   and the ESC that starts an ANSI sequence.
/// * Bidirectional formatting and isolates (U+200E..U+200F,
///   U+202A..U+202E, U+2066..U+2069) — reorder how a string displays
///   without changing it.
/// * Zero-width and invisible spacing (U+200B..U+200D, U+2060..U+2064,
///   U+FEFF) plus the Unicode line/paragraph separators (U+2028, U+2029),
///   which several consumers treat as line breaks even though
///   `char::is_control` returns false for them.
pub fn is_unsafe_text_char(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{2029}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}')
}

/// True when `s` holds any character [`is_unsafe_text_char`] rejects.
///
/// Use at the controller boundary to refuse the input outright — better a
/// visible validation error than silently storing a name that renders as
/// something other than what it is.
pub fn has_unsafe_text(s: &str) -> bool {
    s.chars().any(is_unsafe_text_char)
}

/// Truncate to at most `max` characters, never splitting a UTF-8
/// boundary.
///
/// Character-counted rather than byte-counted so a caller reasoning about
/// "200 characters" gets 200 characters, not 200 bytes of a multi-byte
/// script.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Render untrusted text safe for a single line of a log record or a
/// manifest, capped at `max` characters.
///
/// Unsafe characters become a single space rather than being dropped:
/// deleting them would let `admin\u{202E}` and `admin` collapse to the
/// same string, which is exactly the ambiguity being defended against.
/// Runs of resulting whitespace are collapsed so the output stays compact,
/// and the result is trimmed.
///
/// # Examples
///
/// ```ignore
/// assert_eq!(scrub_line("RPA\nWARN forged", 80), "RPA WARN forged");
/// assert_eq!(scrub_line("clean text", 80), "clean text");
/// ```
pub fn scrub_line(s: &str, max: usize) -> String {
    let replaced: String = s
        .chars()
        .map(|c| if is_unsafe_text_char(c) { ' ' } else { c })
        .collect();

    let mut out = String::with_capacity(replaced.len());
    let mut last_was_space = false;
    for c in replaced.chars() {
        let is_space = c == ' ';
        if is_space && last_was_space {
            continue;
        }
        last_was_space = is_space;
        out.push(c);
    }

    truncate_chars(out.trim(), max)
}

/// Cap for values interpolated into a log record or manifest line.
///
/// Long enough for a real property address or brokerage name, short
/// enough that no single field can dominate a log line or a manifest.
pub const LINE_MAX: usize = 200;

/// [`scrub_line`] at the default [`LINE_MAX`] cap.
pub fn scrub(s: &str) -> String {
    scrub_line(s, LINE_MAX)
}

/// Replace the secret segment of a token-bearing URL path with a
/// placeholder.
///
/// Password-reset, email-verification and invitation tokens are carried
/// as **path** segments, and request paths are recorded in three places:
/// `TraceLayer`'s per-request span at INFO, the 5xx safety-net logger,
/// and the `error_event` table that backs `/admin/errors` for 30 days.
/// A single failed reset attempt therefore parked a live, unexpired
/// reset token where any super-admin could read it and use it.
///
/// `capture_errors` already drops the query string as PII-bearing; this
/// closes the same hole on the other side of the `?`.
///
/// Returns the input unchanged (borrowed) when there is nothing to
/// redact, so the common path allocates nothing.
pub fn redact_secret_path(path: &str) -> std::borrow::Cow<'_, str> {
    let mut segments: Vec<&str> = path.split('/').collect();
    // A leading '/' makes `segments[0]` empty, so route names start at 1.
    let secret_at = match segments.as_slice() {
        ["", "reset", ..] | ["", "verify", ..] | ["", "invite", ..] => Some(2),
        ["", "app", "invites", ..] => Some(3),
        _ => None,
    };

    match secret_at {
        Some(i) if segments.len() > i && !segments[i].is_empty() => {
            segments[i] = "<redacted>";
            std::borrow::Cow::Owned(segments.join("/"))
        }
        _ => std::borrow::Cow::Borrowed(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_path_segments_are_redacted() {
        for (raw, expected) in [
            ("/reset/AbC123-tok_en", "/reset/<redacted>"),
            ("/verify/AbC123", "/verify/<redacted>"),
            ("/invite/AbC123", "/invite/<redacted>"),
            (
                "/app/invites/AbC123/accept",
                "/app/invites/<redacted>/accept",
            ),
            (
                "/app/invites/AbC123/decline",
                "/app/invites/<redacted>/decline",
            ),
        ] {
            assert_eq!(redact_secret_path(raw), expected, "for {raw}");
        }

        // Routes without a secret are returned untouched and unallocated.
        for untouched in [
            "/app/transactions",
            "/login",
            "/reset",
            "/verify",
            "/app/invites",
            "/",
        ] {
            assert!(matches!(
                redact_secret_path(untouched),
                std::borrow::Cow::Borrowed(_)
            ));
            assert_eq!(redact_secret_path(untouched), untouched);
        }
    }

    #[test]
    fn newlines_and_ansi_cannot_forge_a_log_line() {
        let forged = "RPA\n  2026-07-25T12:00:00Z  WARN transactvault::auth: \
                      password_reset_completed, email: admin@victim.com";
        let safe = scrub(forged);
        assert!(!safe.contains('\n'));
        assert!(!safe.contains('\r'));
        assert_eq!(
            safe,
            "RPA 2026-07-25T12:00:00Z WARN transactvault::auth: \
             password_reset_completed, email: admin@victim.com"
        );

        // ANSI: the ESC is what makes the sequence, and it is a control.
        let ansi = "RPA\u{1b}[2J\u{1b}[1;1H";
        assert_eq!(scrub(ansi), "RPA [2J [1;1H");
    }

    #[test]
    fn bidi_overrides_and_invisibles_are_neutralized() {
        // The classic extension-spoofing payload.
        assert!(has_unsafe_text("Contract\u{202E}gpj.exe"));
        assert_eq!(scrub("Contract\u{202E}gpj.exe"), "Contract gpj.exe");

        for c in [
            '\u{200B}', '\u{200E}', '\u{202A}', '\u{202E}', '\u{2028}', '\u{2029}', '\u{2066}',
            '\u{FEFF}',
        ] {
            assert!(
                is_unsafe_text_char(c),
                "U+{:04X} must be rejected",
                c as u32
            );
        }

        // Ordinary text, including non-ASCII, is left completely alone.
        for ok in [
            "123 Main St",
            "Peña Brokerage",
            "东京 Realty",
            "O'Brien & Co",
        ] {
            assert!(!has_unsafe_text(ok));
            assert_eq!(scrub(ok), ok);
        }
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let s = "日本語のテキスト";
        assert_eq!(truncate_chars(s, 3), "日本語");
        assert_eq!(truncate_chars(s, 100), s);
        assert_eq!(scrub_line(&"a".repeat(500), 200).len(), 200);
    }

    #[test]
    fn scrubbing_replaces_rather_than_deletes() {
        // Interior unsafe characters leave a visible separator, so a
        // spoofed value does not silently become the value it imitates.
        assert_eq!(scrub("ad\u{202E}min"), "ad min");
        assert_ne!(scrub("ad\u{202E}min"), scrub("admin"));

        // A leading or trailing one trims away entirely, which is the
        // desired outcome: nothing invisible survives, and what is left
        // is an honest rendering of the visible characters.
        assert_eq!(scrub("admin\u{202E}"), "admin");
        assert_eq!(scrub("\u{202E}admin"), "admin");
    }
}
