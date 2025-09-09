use anyhow::Context;
use tracing::{info, warn};

#[derive(Debug)]
pub struct RemoteBuilder {
    /// E.g. builder@10.0.0.11
    pub uri: String,
    /// E.g. x86_64-linux,i686-linux
    pub platforms: Vec<String>,
    /// E.g. /var/lib/nix/remote_builders.pub
    pub identity_file: Option<std::path::PathBuf>,
    /// E.g. 4
    pub max_jobs: u8,
    /// Higher is preferred for scheduling. E.g. 1
    pub speed_factor: u8,
    /// Support Features E.g. nixos-test,big-parallel,kvm
    pub supported_features: Vec<String>,
    /// Mandatory Features. Builds should only get scheduled if they require
    ///   this feature. E.g. nixos-test
    pub mandatory_features: Vec<String>,
    /// Used to verify remote builder
    pub remote_public_key: Option<String>,
}

pub(crate) fn read_nix_machines_file() -> Vec<RemoteBuilder> {
    let nix_machines_file = std::path::Path::new("/etc/nix/machines");
    if !nix_machines_file.exists() {
        info!("/etc/nix/machines was not found, skipping remote build setup");
        return Vec::new();
    }

    let contents = match std::fs::read_to_string(&nix_machines_file) {
        Err(e) => {
            warn!("Failed to read {:?}, {:?}", &nix_machines_file, e);
            return Vec::new();
        }
        Ok(s) => s,
    };

    parse_nix_machine_contents(contents)
}

fn parse_nix_machine_contents(contents: String) -> Vec<RemoteBuilder> {
    contents
        .lines()
        .into_iter()
        .flat_map(parse_nix_machines_line)
        .collect()
}

fn parse_nix_machines_line(line: &str) -> anyhow::Result<RemoteBuilder> {
    let mut items = line.split_whitespace().into_iter();

    let uri: String = items.next().context("empty line")?.to_string();
    let platforms = parse_many_values(items.next());
    let identity_file = match items.next() {
        Some("-") => None,
        Some(s) => {
            let mut x = std::path::PathBuf::new();
            x.push(s);
            Some(x)
        }
        _ => None,
    };
    let max_jobs = items
        .next()
        .and_then(|s| s.parse::<u8>().ok())
        .context("invalid max_jobs")?;
    let speed_factor = items
        .next()
        .and_then(|s| s.parse::<u8>().ok())
        .context("invalid max_jobs")?;
    let supported_features = parse_many_values(items.next());
    let mandatory_features = parse_many_values(items.next());
    let remote_public_key = items.next().map(|s| s.to_string());

    Ok(RemoteBuilder {
        uri,
        platforms,
        identity_file,
        max_jobs,
        speed_factor,
        supported_features,
        mandatory_features,
        remote_public_key,
    })
}

/// Reads a comma separate list into a list of Strings.
/// Also accounts for the value not being specified, or using the "-" placeholder
fn parse_many_values(value: Option<&str>) -> Vec<String> {
    if value == None {
        return Vec::new();
    }
    let inner_value = value.unwrap();
    if inner_value == "-" {
        return Vec::new();
    }

    inner_value.split(",").map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nix_machines() {
        let nix_machines = r#"
nix@scratchy.labs.cs.uu.nl  i686-linux      /home/nix/.ssh/id_scratchy_auto        8 1 kvm
nix@itchy.labs.cs.uu.nl     i686-linux      /home/nix/.ssh/id_scratchy_auto        8 2
nix@poochie.labs.cs.uu.nl   i686-linux      -                                      1 2 kvm benchmark
        "#;
        let machines = parse_nix_machine_contents(nix_machines.to_string());
        assert_eq!(machines.len(), 3);
        assert_eq!(machines[0].max_jobs, 8);
        assert_eq!(machines[1].max_jobs, 8);
        assert_eq!(machines[2].max_jobs, 1);

        assert_eq!(machines[0].mandatory_features, Vec::<String>::new());
        assert_eq!(machines[1].uri, "nix@itchy.labs.cs.uu.nl");
        assert_eq!(machines[2].identity_file, None);
    }
}
