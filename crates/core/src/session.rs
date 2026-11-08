//! When a profile's login session ends, from the AWS CLI's token cache.
//!
//! Only the expiry time is read from `~/.aws/sso/cache/<sha1 of the session
//! name>.json`; the tokens in it are never kept or shown.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use sha1::{Digest, Sha1};

use crate::profiles::{Auth, ProfileInfo};

impl ProfileInfo {
    /// When the login behind this profile expires (Unix seconds), if it uses an
    /// SSO session and has a login cached. Other kinds of profile have no
    /// session to count down.
    pub fn session_expiry(&self) -> Option<i64> {
        let Auth::SsoSession(session) = &self.auth else {
            return None;
        };
        sso_expiry(&home()?, session)
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Where the CLI caches the token for an `[sso-session]`.
fn cache_file(home: &Path, session: &str) -> PathBuf {
    let name: String = Sha1::digest(session.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    home.join(".aws/sso/cache").join(format!("{name}.json"))
}

/// The cached access token's expiry. A token that has expired but comes with a
/// refresh token is renewed silently by the SDK on its next call, so it counts
/// as having no expiry to show.
fn sso_expiry(home: &Path, session: &str) -> Option<i64> {
    let text = std::fs::read_to_string(cache_file(home, session)).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let at = json.get("expiresAt")?.as_str()?;
    // Older CLIs wrote `...UTC` instead of `...Z`.
    let at = at
        .strip_suffix("UTC")
        .map_or(at.to_owned(), |t| format!("{t}Z"));
    let expiry = jiff::Timestamp::from_str(&at).ok()?.as_second();
    let refreshable = json.get("refreshToken").is_some();
    (!(refreshable && expiry <= crate::fmt::now_secs())).then_some(expiry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(home: &Path, session: &str, body: &str) {
        let file = cache_file(home, session);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fast-aws-session-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_cache_file_is_named_by_the_sha1_of_the_session() {
        // SHA-1 of "abc" is a well-known test vector.
        assert_eq!(
            cache_file(Path::new("/h"), "abc"),
            Path::new("/h/.aws/sso/cache/a9993e364706816aba3e25717850c26c9cd0d89d.json")
        );
    }

    #[test]
    fn reads_only_the_expiry() {
        let home = temp("reads");
        cache(
            &home,
            "acme",
            r#"{"accessToken":"secret","expiresAt":"2030-01-01T00:00:00Z"}"#,
        );
        assert_eq!(sso_expiry(&home, "acme"), Some(1_893_456_000));
        assert_eq!(sso_expiry(&home, "other"), None);
    }

    #[test]
    fn older_cli_timestamps_end_in_utc() {
        let home = temp("utc");
        cache(&home, "acme", r#"{"expiresAt":"2030-01-01T00:00:00UTC"}"#);
        assert_eq!(sso_expiry(&home, "acme"), Some(1_893_456_000));
    }

    #[test]
    fn an_expired_token_with_a_refresh_token_is_not_counted_down() {
        let home = temp("refresh");
        let past = r#""expiresAt":"2001-01-01T00:00:00Z""#;
        cache(&home, "plain", &format!("{{{past}}}"));
        cache(
            &home,
            "refreshing",
            &format!(r#"{{{past},"refreshToken":"x"}}"#),
        );
        assert!(sso_expiry(&home, "plain").is_some());
        assert_eq!(sso_expiry(&home, "refreshing"), None);
    }

    #[test]
    fn garbage_and_other_profile_kinds_have_no_expiry() {
        let home = temp("garbage");
        cache(&home, "acme", "not json");
        assert_eq!(sso_expiry(&home, "acme"), None);
        let profile = ProfileInfo {
            name: "x".into(),
            region: None,
            auth: Auth::StaticKeys,
        };
        assert_eq!(profile.session_expiry(), None);
    }
}
