use serde::Serialize;
use serde_json::{Map, Value};

const ALIASES: &[(&str, &str)] = &[("allow-eval-failures", "allow_eval_failures")];

pub(crate) fn find<T: Serialize>(input: &Value, parsed: &T) -> Vec<String> {
    let known = match serde_json::to_value(parsed) {
        Ok(known) => known,
        Err(e) => {
            tracing::debug!("skipping unknown-key detection: {}", e);
            return Vec::new();
        },
    };
    let mut unknown = Vec::new();
    if let (Value::Object(input), Value::Object(known)) = (input, &known) {
        collect(input, known, "", &mut unknown);
    }
    unknown
}

fn collect(
    input: &Map<String, Value>,
    known: &Map<String, Value>,
    prefix: &str,
    out: &mut Vec<String>,
) {
    for (key, value) in input {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match known
            .get(key)
            .or_else(|| canonical(key).and_then(|k| known.get(k)))
        {
            Some(Value::Object(known_child)) => {
                if let Value::Object(child) = value {
                    collect(child, known_child, &path, out);
                }
            },
            Some(_) => {},
            None => out.push(path),
        }
    }
}

fn canonical(key: &str) -> Option<&'static str> {
    ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use crate::CIConfig;

    fn unknown(json: &str) -> Vec<String> {
        let mut keys = CIConfig::from_str(json).unwrap().unknown_keys().to_vec();
        keys.sort();
        keys
    }

    #[test]
    fn reports_unknown_keys_at_every_level() {
        let json = r#"{
  "jobz": {},
  "jobs": { "pkgs": { "file": "a.nix", "allow_evl_failures": false } },
  "checks": { "fmt": { "command": "true", "network": true } },
  "flake": { "checks": { "enable": true, "systems": [] } },
  "rebuild_impact": { "enabled": true, "extra": 1 }
}"#;
        assert_eq!(
            unknown(json),
            [
                "checks.fmt.network",
                "flake.checks.systems",
                "jobs.pkgs.allow_evl_failures",
                "jobz",
                "rebuild_impact.extra",
            ]
        );
    }

    #[test]
    fn known_keys_aliases_and_map_names_are_not_reported() {
        let json = r#"{
  "jobs": {
    "allow-eval-failures": { "file": "a.nix", "allow-eval-failures": false },
    "b": { "file": "b.nix", "allow_eval_failures": true, "caches": ["c"],
           "size_check": { "max_increase_percent": 1.0 } }
  },
  "package_change_summary": { "enabled": false }
}"#;
        assert!(unknown(json).is_empty(), "{:?}", unknown(json));
    }
}
