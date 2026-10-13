//! `fast-aws`: a fast, keyboard-driven TUI for AWS.

use std::process::ExitCode;
use std::sync::Arc;

use fast_aws_core::profiles::ProfileCatalog;
use fast_aws_core::{ConfigFactory, Registry};
use fast_aws_ui::app::App;

const USAGE: &str = "\
usage: fast-aws [RESOURCE] [--profile NAME] [--region REGION]

  RESOURCE           resource type to open, e.g. ec2 (default: ec2)
  -p, --profile      AWS profile (default: AWS_PROFILE or `default`)
  -r, --region       AWS region (default: the profile's region)
  -h, --help         show this help
  -V, --version      show the version

Inside the app, press ? for keys.";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    resource: Option<String>,
    profile: Option<String>,
    region: Option<String>,
}

enum Parsed {
    Run(Args),
    Exit(String),
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut parsed = Args::default();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Exit(USAGE.into())),
            "-V" | "--version" => {
                return Ok(Parsed::Exit(format!(
                    "fast-aws {}",
                    env!("CARGO_PKG_VERSION")
                )));
            }
            "-p" | "--profile" => parsed.profile = Some(value(&arg)?),
            "-r" | "--region" => parsed.region = Some(value(&arg)?),
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}")),
            _ if parsed.resource.is_none() => parsed.resource = Some(arg),
            _ => return Err(format!("unexpected argument {arg}")),
        }
    }
    Ok(Parsed::Run(parsed))
}

fn registry() -> Registry {
    let mut registry = Registry::default();
    fast_aws_svc_ec2::register(&mut registry);
    registry
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Parsed::Run(args)) => args,
        Ok(Parsed::Exit(text)) => {
            println!("{text}");
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            eprintln!("fast-aws: {err}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let registry = registry();
    let resource = args.resource.as_deref().unwrap_or("ec2");
    let Some(kind) = registry.find(resource) else {
        eprintln!("fast-aws: unknown resource type `{resource}`");
        return ExitCode::from(2);
    };

    let catalog = match ProfileCatalog::load().await {
        Ok(catalog) => catalog,
        Err(err) => {
            eprintln!("fast-aws: couldn't read the AWS config files: {err}");
            return ExitCode::FAILURE;
        }
    };
    let profile = match args.profile {
        Some(profile) => profile,
        // No AWS_PROFILE and no [default]: start on the first profile rather than fail.
        None if catalog.get(&catalog.selected).is_none() && !catalog.profiles.is_empty() => {
            catalog.profiles[0].name.clone()
        }
        None => catalog.selected.clone(),
    };
    if !catalog.profiles.is_empty() && catalog.get(&profile).is_none() {
        eprintln!("fast-aws: no profile named `{profile}` in the AWS config files");
        return ExitCode::from(2);
    }

    // Loading the config resolves the region (flag, profile, environment, fallback)
    // without contacting AWS; credentials are only fetched by the first call.
    let factory = Arc::new(ConfigFactory::default());
    let region = match args.region {
        Some(region) => region,
        None => factory.ctx(&profile, None).await.region,
    };

    let (app, effects) = App::new(registry, catalog, profile, region, kind);
    match fast_aws_ui::runtime::run(app, effects, factory).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("fast-aws: terminal error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Parsed, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_resource_and_flags() {
        let Ok(Parsed::Run(args)) = parse(&["ec2", "-p", "dev", "--region", "eu-west-1"]) else {
            panic!("expected Run");
        };
        assert_eq!(
            args,
            Args {
                resource: Some("ec2".into()),
                profile: Some("dev".into()),
                region: Some("eu-west-1".into()),
            }
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(
            parse(&["--profile"]).err().unwrap(),
            "--profile needs a value"
        );
        assert_eq!(parse(&["--nope"]).err().unwrap(), "unknown option --nope");
        assert_eq!(
            parse(&["ec2", "s3"]).err().unwrap(),
            "unexpected argument s3"
        );
        assert!(matches!(parse(&["--help"]), Ok(Parsed::Exit(_))));
    }

    #[test]
    fn ec2_is_registered() {
        assert_eq!(registry().find("ec2").unwrap().id(), "ec2:instance");
    }
}
