//! Profile discovery from the shared AWS config files.
//!
//! Profiles are grouped by the `[sso-session]` they use, because one SSO login covers
//! every profile in a session. Only property *names* are inspected to decide how a
//! profile authenticates; credential values are never read out.

use std::borrow::Cow;

use aws_config::profile::{Profile, ProfileSet};
use aws_runtime::env_config::file::EnvConfigFiles;
use aws_types::os_shim_internal::{Env, Fs};

pub use aws_config::profile::ProfileFileLoadError;

/// How a profile gets its credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// SSO through a named `[sso-session]` (`aws sso login --sso-session <name>`).
    SsoSession(String),
    /// SSO with `sso_start_url` on the profile itself (`aws sso login --profile <name>`).
    SsoLegacy,
    /// Assume-role that needs an MFA code. Not supported yet: aws-config can't prompt.
    Mfa,
    AssumeRole,
    CredentialProcess,
    StaticKeys,
    /// Nothing in the profile; credentials come from the environment or instance metadata.
    Ambient,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileInfo {
    pub name: String,
    pub region: Option<String>,
    pub auth: Auth,
}

impl ProfileInfo {
    /// The command that logs this profile in, if it uses SSO.
    pub fn login_command(&self) -> Option<Vec<String>> {
        let args = match &self.auth {
            Auth::SsoSession(session) => ["sso", "login", "--sso-session", session.as_str()],
            Auth::SsoLegacy => ["sso", "login", "--profile", self.name.as_str()],
            _ => return None,
        };
        Some(
            std::iter::once("aws")
                .chain(args)
                .map(str::to_owned)
                .collect(),
        )
    }
}

/// A group of profiles shown together in the profile switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileGroup<'a> {
    /// The sso-session name, or `None` for profiles that don't use one.
    pub sso_session: Option<&'a str>,
    pub profiles: Vec<&'a ProfileInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileCatalog {
    pub profiles: Vec<ProfileInfo>,
    /// The profile selected by `AWS_PROFILE`, or `default`.
    pub selected: String,
}

impl ProfileCatalog {
    /// Loads profiles from `~/.aws/config` and `~/.aws/credentials` (or the paths in
    /// `AWS_CONFIG_FILE` / `AWS_SHARED_CREDENTIALS_FILE`).
    pub async fn load() -> Result<Self, ProfileFileLoadError> {
        Self::load_from(&Fs::real(), &Env::real()).await
    }

    pub async fn load_from(fs: &Fs, env: &Env) -> Result<Self, ProfileFileLoadError> {
        let set = aws_config::profile::load(fs, env, &EnvConfigFiles::default(), None).await?;
        Ok(Self::from_set(&set))
    }

    fn from_set(set: &ProfileSet) -> Self {
        let mut profiles: Vec<ProfileInfo> = set
            .profiles()
            .filter_map(|name| set.get_profile(name))
            .map(|p| ProfileInfo {
                name: p.name().to_owned(),
                region: p.get("region").map(str::to_owned),
                auth: auth_of(p),
            })
            .collect();
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            profiles,
            selected: Cow::from(set.selected_profile()).into_owned(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&ProfileInfo> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Profiles grouped by sso-session (sessions sorted by name), followed by one group
    /// for every profile that doesn't use a session.
    pub fn groups(&self) -> Vec<ProfileGroup<'_>> {
        let mut groups: Vec<ProfileGroup<'_>> = Vec::new();
        let mut others = Vec::new();
        for profile in &self.profiles {
            let Auth::SsoSession(session) = &profile.auth else {
                others.push(profile);
                continue;
            };
            match groups
                .iter_mut()
                .find(|g| g.sso_session == Some(session.as_str()))
            {
                Some(group) => group.profiles.push(profile),
                None => groups.push(ProfileGroup {
                    sso_session: Some(session.as_str()),
                    profiles: vec![profile],
                }),
            }
        }
        groups.sort_by(|a, b| a.sso_session.cmp(&b.sso_session));
        if !others.is_empty() {
            groups.push(ProfileGroup {
                sso_session: None,
                profiles: others,
            });
        }
        groups
    }
}

fn auth_of(p: &Profile) -> Auth {
    if let Some(session) = p.get("sso_session") {
        Auth::SsoSession(session.to_owned())
    } else if p.get("sso_start_url").is_some() {
        Auth::SsoLegacy
    } else if p.get("mfa_serial").is_some() {
        Auth::Mfa
    } else if p.get("role_arn").is_some() {
        Auth::AssumeRole
    } else if p.get("credential_process").is_some() {
        Auth::CredentialProcess
    } else if p.get("aws_access_key_id").is_some() {
        Auth::StaticKeys
    } else {
        Auth::Ambient
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
[default]
region = us-east-1

[sso-session acme]
sso_start_url = https://acme.awsapps.com/start
sso_region = eu-west-1

[profile acme-qa]
sso_session = acme
sso_account_id = 111111111111
sso_role_name = Dev
region = eu-west-1

[profile acme-dev]
sso_session = acme
sso_account_id = 222222222222
sso_role_name = Dev

[profile old-sso]
sso_start_url = https://old.awsapps.com/start
sso_region = us-east-1
sso_account_id = 333333333333
sso_role_name = Admin

[profile admin-mfa]
role_arn = arn:aws:iam::444444444444:role/Admin
source_profile = default
mfa_serial = arn:aws:iam::444444444444:mfa/me

[profile ci]
role_arn = arn:aws:iam::555555555555:role/Ci
source_profile = default
"#;

    const CREDENTIALS: &str =
        "[default]\naws_access_key_id = AKIDEXAMPLE\naws_secret_access_key = x\n";

    async fn catalog(profile_env: Option<&str>) -> ProfileCatalog {
        let fs = Fs::from_slice(&[
            ("/home/u/.aws/config", CONFIG),
            ("/home/u/.aws/credentials", CREDENTIALS),
        ]);
        let mut vars = vec![("HOME", "/home/u")];
        if let Some(p) = profile_env {
            vars.push(("AWS_PROFILE", p));
        }
        ProfileCatalog::load_from(&fs, &Env::from_slice(&vars))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn detects_how_each_profile_authenticates() {
        let c = catalog(None).await;
        let auth = |n: &str| c.get(n).unwrap().auth.clone();
        assert_eq!(auth("acme-dev"), Auth::SsoSession("acme".into()));
        assert_eq!(auth("old-sso"), Auth::SsoLegacy);
        assert_eq!(auth("admin-mfa"), Auth::Mfa);
        assert_eq!(auth("ci"), Auth::AssumeRole);
        assert_eq!(auth("default"), Auth::StaticKeys);
        assert_eq!(
            c.get("acme-qa").unwrap().region.as_deref(),
            Some("eu-west-1")
        );
        assert_eq!(c.selected, "default");
    }

    #[tokio::test]
    async fn groups_profiles_by_sso_session() {
        let c = catalog(Some("acme-qa")).await;
        let groups = c.groups();
        assert_eq!(groups[0].sso_session, Some("acme"));
        let names: Vec<_> = groups[0].profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["acme-dev", "acme-qa"]);
        assert_eq!(groups[1].sso_session, None);
        assert_eq!(groups.len(), 2);
        assert_eq!(c.selected, "acme-qa");
    }

    #[tokio::test]
    async fn login_command_uses_the_session_when_there_is_one() {
        let c = catalog(None).await;
        let cmd = |n: &str| c.get(n).unwrap().login_command().map(|v| v.join(" "));
        assert_eq!(
            cmd("acme-dev").as_deref(),
            Some("aws sso login --sso-session acme")
        );
        assert_eq!(
            cmd("old-sso").as_deref(),
            Some("aws sso login --profile old-sso")
        );
        assert_eq!(cmd("ci"), None);
    }
}
