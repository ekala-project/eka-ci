use std::ffi::OsString;
use std::path::Path;

use anyhow::{Context, Result, bail};

// Anything a shellHook prints lands on stdout before the environment.
const ENV_MARKER: &[u8] = b"\0__EKACI_ENV__\0";
const PRINT_ENV: &str = "printf '\\000__EKACI_ENV__\\000'; exec env -0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevShell<'a> {
    Flake(Option<&'a str>),
    ShellNix(Option<&'a str>),
}

impl<'a> DevShell<'a> {
    pub fn for_check(command: &str, name: Option<&'a str>, shell_nix: bool) -> Option<Self> {
        if command.starts_with("nix build ") {
            return None;
        }
        Some(if shell_nix {
            DevShell::ShellNix(name)
        } else {
            DevShell::Flake(name)
        })
    }

    pub fn entry_file(&self) -> &'static str {
        match self {
            DevShell::Flake(_) => "flake.nix",
            DevShell::ShellNix(_) => "shell.nix",
        }
    }

    pub fn command(&self) -> (&'static str, Vec<OsString>) {
        match self {
            DevShell::Flake(name) => {
                let installable = name.map_or(".".to_string(), |n| format!(".#{n}"));
                let args = [
                    "--extra-experimental-features",
                    "nix-command flakes",
                    "develop",
                    // Writing a missing lock would `git add` into the read-only `.git`.
                    "--no-write-lock-file",
                    &installable,
                    "--command",
                    "sh",
                    "-c",
                    PRINT_ENV,
                ];
                ("nix", args.iter().map(OsString::from).collect())
            },
            DevShell::ShellNix(name) => {
                let mut args: Vec<OsString> = vec!["shell.nix".into()];
                if let Some(n) = name {
                    args.extend(["-A".into(), (*n).into()]);
                }
                args.extend(["--run".into(), PRINT_ENV.into()]);
                ("nix-shell", args)
            },
        }
    }
}

const TRANSIENT_VARS: &[&str] = &[
    "HOME",
    "NIX_BUILD_TOP",
    "OLDPWD",
    "PWD",
    "SHLVL",
    "TEMP",
    "TEMPDIR",
    "TMP",
    "TMPDIR",
    "_",
];

pub fn parse_env(output: &[u8]) -> Result<Vec<(String, String)>> {
    let start = output
        .windows(ENV_MARKER.len())
        .position(|w| w == ENV_MARKER)
        .context("dev shell did not print its environment")?;
    let mut vars: Vec<(String, String)> = output[start + ENV_MARKER.len()..]
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let entry = String::from_utf8_lossy(entry);
            let (k, v) = entry.split_once('=')?;
            let keep = !k.is_empty() && !TRANSIENT_VARS.contains(&k);
            keep.then(|| (k.to_string(), v.to_string()))
        })
        .collect();
    vars.sort();
    Ok(vars)
}

pub fn require_entry_file(shell: DevShell<'_>, checkout: &Path) -> Result<()> {
    let file = shell.entry_file();
    if !checkout.join(file).exists() {
        bail!(
            "no {file} found in the repository; checks with shell_nix = {} need one",
            matches!(shell, DevShell::ShellNix(_))
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(args: &[OsString]) -> String {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn nix_build_needs_no_shell() {
        assert_eq!(DevShell::for_check("nix build .#x", None, false), None);
        assert_eq!(
            DevShell::for_check("make", Some("ci"), false),
            Some(DevShell::Flake(Some("ci")))
        );
        assert_eq!(
            DevShell::for_check("make", None, true),
            Some(DevShell::ShellNix(None))
        );
    }

    #[test]
    fn commands() {
        let (p, a) = DevShell::Flake(Some("ci")).command();
        assert_eq!(p, "nix");
        assert!(joined(&a).contains("develop --no-write-lock-file .#ci --command sh -c printf"));
        let (p, a) = DevShell::Flake(None).command();
        assert_eq!(
            (p, joined(&a).contains("--no-write-lock-file . --command")),
            ("nix", true)
        );
        let (p, a) = DevShell::ShellNix(Some("x")).command();
        assert_eq!(
            (p, joined(&a).as_str()),
            (
                "nix-shell",
                "shell.nix -A x --run printf '\\000__EKACI_ENV__\\000'; exec env -0"
            )
        );
    }

    #[test]
    fn parses_multiline_values_and_drops_transient_vars() {
        let out = b"Welcome\n\0__EKACI_ENV__\0B=two\nlines\0TMPDIR=/tmp/nix-shell.x\0A=1\0PATH=/nix/store/x/bin\0junk\0";
        assert!(parse_env(b"A=1\0").is_err());
        assert_eq!(
            parse_env(out).unwrap(),
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "two\nlines".into()),
                ("PATH".into(), "/nix/store/x/bin".into()),
            ]
        );
    }
}
