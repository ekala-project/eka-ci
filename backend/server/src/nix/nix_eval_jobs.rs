use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::dependency_comparison::{pname_from_name, version_from_name};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum NixEvalItem {
    Error(NixEvalError),
    Drv(NixEvalDrv),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NixEvalDrv {
    /// String of full attr path
    /// E.g. "python.pkgs.setuptools"
    pub attr: String,

    /// List of attrs to access drv.
    /// "python.pkgs.setuptools" -> [ "python" "pkgs" "setuptools" ]
    #[serde(rename = "attrPath")]
    pub attr_path: Vec<String>,

    /// Store path to drv. E.g. "/nix/store/<hash>-<name>.drv"
    #[serde(rename = "drvPath")]
    pub drv_path: String,

    /// Direct references/dependencies of this drv
    #[serde(rename = "inputDrvs")]
    pub input_drvs: Option<HashMap<String, Vec<String>>>,

    /// Name of drv. Usually includes "${pname}-${version}", but doesn't need to
    pub name: String,

    /// A mapping of the multiple outputs and their respective nix store paths
    pub outputs: HashMap<String, String>,

    /// Build platform system
    pub system: String,

    /// Derivation `meta` attribute, present when `nix-eval-jobs --meta` is
    /// passed. Optional because:
    ///   * older runs / test fixtures may not include it
    ///   * a derivation may have no `meta` attribute at all
    ///   * a malformed `meta` payload is logged and dropped (see
    ///     [`crate::nix::jobs::process_nix_eval_output`])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<NixEvalMeta>,
}

impl NixEvalDrv {
    /// Best-effort `pname` accessor.
    ///
    /// Order of preference:
    /// 1. `meta.pname` (authoritative; only set if the package author writes it)
    /// 2. heuristic `pname_from_name(name)` from the trailing-version stripper
    pub fn pname_or_heuristic(&self) -> String {
        if let Some(meta) = &self.meta {
            if let Some(pname) = meta.pname.as_deref() {
                if !pname.is_empty() {
                    return pname.to_string();
                }
            }
        }
        pname_from_name(&self.name)
    }

    /// Best-effort `version` accessor.
    ///
    /// Order of preference:
    /// 1. `meta.version` (authoritative)
    /// 2. heuristic `version_from_name(name)`
    pub fn version_or_heuristic(&self) -> Option<String> {
        if let Some(meta) = &self.meta {
            if let Some(version) = meta.version.as_deref() {
                if !version.is_empty() {
                    return Some(version.to_string());
                }
            }
        }
        version_from_name(&self.name)
    }

    /// Project this `NixEvalDrv` into a [`DrvPackageMetadata`] payload ready
    /// to be persisted on the corresponding `Drv` row. Fields are normalized:
    ///
    /// * `pname` / `version`: meta-first, with `_or_heuristic` fallback
    /// * `license_json`: serialized `Vec<LicenseEntry>` (single-entry list for shorthand strings)
    ///   or `None`
    /// * `maintainers_json`: serialized `Vec<MaintainerEntry>` or `None` (an empty list is also
    ///   collapsed to `None` to avoid storing `"[]"`)
    /// * `meta_position`: pass-through of `meta.position`
    /// * `broken` / `insecure`: pass-through of the matching `meta.*` fields
    ///
    /// Returns `None` for any field where the data is unavailable so that the
    /// caller's `COALESCE` ON CONFLICT path will preserve any previously
    /// persisted value rather than overwriting it with NULL.
    pub fn to_drv_metadata(&self) -> DrvPackageMetadata {
        let pname = Some(self.pname_or_heuristic()).filter(|s| !s.is_empty());
        let version = self.version_or_heuristic().filter(|s| !s.is_empty());

        let (license_json, maintainers_json, meta_position, broken, insecure) = if let Some(meta) =
            &self.meta
        {
            let license_json = meta.license.clone().map(|lf| {
                let entries = lf.normalise();
                // serialization of a known-shape Vec<LicenseEntry> is
                // infallible; unwrap is safe.
                serde_json::to_string(&entries).expect("license entries should always serialize")
            });

            let maintainers_json = meta
                .maintainers
                .as_ref()
                .filter(|v| !v.is_empty())
                .map(|v| {
                    serde_json::to_string(v).expect("maintainer entries should always serialize")
                });

            (
                license_json,
                maintainers_json,
                meta.position.clone(),
                meta.broken,
                meta.insecure,
            )
        } else {
            (None, None, None, None, None)
        };

        DrvPackageMetadata {
            pname,
            version,
            license_json,
            maintainers_json,
            meta_position,
            broken,
            insecure,
        }
    }
}

/// Database-ready package metadata derived from a [`NixEvalDrv`].
///
/// Each field maps 1:1 to a column on the `Drv` table. A `None` value means
/// "do not overwrite the existing column on conflict" (see the
/// `COALESCE(excluded.x, Drv.x)` pattern in `insert_drv` / batch insert).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrvPackageMetadata {
    pub pname: Option<String>,
    pub version: Option<String>,
    pub license_json: Option<String>,
    pub maintainers_json: Option<String>,
    pub meta_position: Option<String>,
    pub broken: Option<bool>,
    pub insecure: Option<bool>,
}

/// Subset of nix-eval-jobs `--meta` output we currently care about.
///
/// All fields are `Option`/`default` so that:
///   * partially populated `meta` objects parse cleanly
///   * additional fields nix-eval-jobs may emit are silently ignored
///
/// We do NOT use `#[serde(deny_unknown_fields)]` here — `--meta` may include
/// arbitrary additional attributes (`platforms`, `available`, `outputsToInstall`,
/// etc.) that aren't part of v1's persistence surface.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct NixEvalMeta {
    /// `meta.pname` — only set when the package author writes it explicitly.
    /// Most nixpkgs packages embed pname in the derivation `name` field instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pname: Option<String>,

    /// `meta.version` — likewise rarely set in `meta`; usually only in `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// `meta.homepage` — may be a string or a list of strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<HomepageField>,

    /// `meta.license` — see [`LicenseField`] for the four observed shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<LicenseField>,

    /// `meta.maintainers` — list of maintainer entries (may be empty/missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintainers: Option<Vec<MaintainerEntry>>,

    /// `meta.position` — `"<file>:<line>"` pointing at the meta declaration.
    /// Useful for source-attribution in PR review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broken: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insecure: Option<bool>,
}

/// Homepage field — observed shapes:
///   * a single URL string: `"https://example.com"`
///   * a list of URL strings (rare, for multi-homepage packages)
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum HomepageField {
    Single(String),
    Multiple(Vec<String>),
}

impl HomepageField {
    /// Flatten to a `Vec<String>` for uniform downstream handling.
    pub fn into_vec(self) -> Vec<String> {
        match self {
            HomepageField::Single(s) => vec![s],
            HomepageField::Multiple(v) => v,
        }
    }
}

/// License field — observed shapes (from nixpkgs survey):
///   * Object: `{ "shortName": "mit", "spdxId": "MIT", "fullName": "...", "free": true }`
///   * List of license objects (multi-licensed packages)
///   * Bare string: `"mit"` (legacy shorthand still in nixpkgs)
///   * Unfree license object: `{ "shortName": "unfree", "free": false }`
///
/// Normalisation into a single `Vec<LicenseEntry>` happens at the storage
/// boundary (P0.S6) — at parse time we preserve the raw shape.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum LicenseField {
    Single(LicenseEntry),
    Multiple(Vec<LicenseEntry>),
    Shorthand(String),
}

impl LicenseField {
    /// Normalise into a list of license entries. Shorthand strings become a
    /// single-entry list with only `short_name` populated.
    pub fn normalise(self) -> Vec<LicenseEntry> {
        match self {
            LicenseField::Single(e) => vec![e],
            LicenseField::Multiple(v) => v,
            LicenseField::Shorthand(s) => vec![LicenseEntry {
                short_name: Some(s),
                spdx_id: None,
                full_name: None,
                free: None,
            }],
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct LicenseEntry {
    #[serde(default, rename = "shortName", skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,

    #[serde(default, rename = "spdxId", skip_serializing_if = "Option::is_none")]
    pub spdx_id: Option<String>,

    #[serde(default, rename = "fullName", skip_serializing_if = "Option::is_none")]
    pub full_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free: Option<bool>,
}

/// Maintainer entry from `meta.maintainers`. All fields optional because
/// nixpkgs entries vary (some have `github`, some only `name`/`email`).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct MaintainerEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NixEvalError {
    pub attr: String,
    #[serde(rename = "attrPath")]
    pub attr_path: Vec<String>,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialization() {
        // Generated with:
        // `nix-eval-jobs --expr 'with import ./. {}; { inherit grpc; }'`
        // while in nixpkgs directory
        let eval_drv = r#"{"attr":"cmake","attrPath":["cmake"],"drvPath":"/nix/store/3fr8b3xlygv2a64ff7fq7564j4sxv4lc-cmake-3.29.6.drv","inputDrvs":{"/nix/store/08s4j5nvddsbrjpachqwzai83xngxnc0-pkg-config-wrapper-0.29.2.drv":["out"],"/nix/store/0cgbdlz63qiqf5f8i1sljak1dfbzyrl5-openssl-3.0.14.drv":["dev"],"/nix/store/265x0i426vnqjma9khcfpi86m6hx4smr-bash-5.2p32.drv":["out"],"/nix/store/27zlixdsk0kx585j4dcjm53636mx7cis-libuv-1.48.0.drv":["dev"],"/nix/store/2vyizsckka60lhh0kylhbpdd1flb998v-cmake-3.29.6.tar.gz.drv":["out"],"/nix/store/4hzjv6r5v7h6hzad718jgc0hrm1gz8r1-gcc-wrapper-13.3.0.drv":["out"],"/nix/store/860zddz386bk0441flrg940ipbp0jp1z-xz-5.6.2.drv":["dev"],"/nix/store/9jvlq6qg9j1222w3zm3wgfv5qyqfqmxz-bzip2-1.0.8.drv":["dev"],"/nix/store/ax4q30iyf9wi95hswil021lg0cdqq6rl-libarchive-3.7.4.drv":["dev"],"/nix/store/bxq3kjf71wn92yisdbq18fzpvcl5pn31-expat-2.6.2.drv":["dev"],"/nix/store/kh6mps96srqgdvn03vq4gmqzl51s9w8h-glibc-2.39-52.drv":["bin","dev","out"],"/nix/store/lzc503qcc7f6ibq8sdbcri73wb62dj4r-zlib-1.3.1.drv":["dev"],"/nix/store/mzw7jzs6ix17ajh3z4kqzvh8l7abj4yr-rhash-1.4.4.drv":["out"],"/nix/store/v288gxsg679gyi9zpg0mhrv26vfmw4kr-stdenv-linux.drv":["out"],"/nix/store/vnq47hr4nwry8kgvfgmx0229id3q49dr-binutils-2.42.drv":["out"],"/nix/store/y99v9h2mcqbw91g7p3lnk292k0np0djr-curl-8.9.0.drv":["dev"]},"name":"cmake-3.29.6","outputs":{"debug":"/nix/store/xrh9g28kmsyjlw6qf46ngkvhac1llgvz-cmake-3.29.6-debug","out":"/nix/store/rz7j0kdkq8j522vpw6n8wjq2qv3if24g-cmake-3.29.6"},"system":"x86_64-linux"}"#;

        serde_json::from_str::<NixEvalDrv>(eval_drv).expect("Failed to deserialize output");
    }

    #[test]
    fn test_deserialization_with_meta() {
        // Synthesised from `nix-eval-jobs --meta` output shape, exercising:
        //   * structured license object with spdxId/shortName/fullName/free
        //   * single maintainer with all fields
        //   * homepage as a single string
        //   * position file:line
        //   * boolean flags
        let line = r#"{
            "attr": "hello",
            "attrPath": ["hello"],
            "drvPath": "/nix/store/abc-hello-2.12.1.drv",
            "name": "hello-2.12.1",
            "outputs": {"out": "/nix/store/xyz-hello-2.12.1"},
            "system": "x86_64-linux",
            "meta": {
                "description": "A friendly greeting",
                "homepage": "https://www.gnu.org/software/hello/",
                "license": {
                    "fullName": "GNU General Public License v3.0 or later",
                    "shortName": "gpl3Plus",
                    "spdxId": "GPL-3.0-or-later",
                    "free": true
                },
                "maintainers": [
                    {"name": "Eelco Dolstra", "email": "edolstra@gmail.com", "github": "edolstra"}
                ],
                "platforms": ["x86_64-linux", "aarch64-linux"],
                "position": "/nix/store/.../pkgs/applications/misc/hello/default.nix:34",
                "broken": false,
                "insecure": false,
                "available": true
            }
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        let meta = drv.meta.as_ref().expect("meta present");
        assert_eq!(meta.description.as_deref(), Some("A friendly greeting"));
        assert_eq!(
            meta.position.as_deref().unwrap_or(""),
            "/nix/store/.../pkgs/applications/misc/hello/default.nix:34"
        );
        assert_eq!(meta.broken, Some(false));
        assert_eq!(meta.insecure, Some(false));

        match meta.license.clone().expect("license present") {
            LicenseField::Single(e) => {
                assert_eq!(e.short_name.as_deref(), Some("gpl3Plus"));
                assert_eq!(e.spdx_id.as_deref(), Some("GPL-3.0-or-later"));
                assert_eq!(e.free, Some(true));
            },
            other => panic!("expected single license, got {:?}", other),
        }

        match meta.homepage.clone().expect("homepage present") {
            HomepageField::Single(s) => assert_eq!(s, "https://www.gnu.org/software/hello/"),
            other => panic!("expected single homepage, got {:?}", other),
        }

        let maintainers = meta.maintainers.as_ref().expect("maintainers present");
        assert_eq!(maintainers.len(), 1);
        assert_eq!(maintainers[0].github.as_deref(), Some("edolstra"));

        // Heuristic accessors fall through to name-based extraction since
        // meta.pname / meta.version are absent.
        assert_eq!(drv.pname_or_heuristic(), "hello");
        assert_eq!(drv.version_or_heuristic(), Some("2.12.1".to_string()));
    }

    #[test]
    fn test_deserialization_without_meta() {
        // Older fixture (pre --meta) must still parse — meta is Option.
        let line = r#"{
            "attr": "hello",
            "attrPath": ["hello"],
            "drvPath": "/nix/store/abc-hello-2.12.1.drv",
            "name": "hello-2.12.1",
            "outputs": {"out": "/nix/store/xyz-hello-2.12.1"},
            "system": "x86_64-linux"
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        assert!(drv.meta.is_none());
        assert_eq!(drv.pname_or_heuristic(), "hello");
        assert_eq!(drv.version_or_heuristic(), Some("2.12.1".to_string()));
    }

    #[test]
    fn test_license_shorthand_string() {
        // Some legacy nixpkgs entries emit license as a bare string.
        let line = r#"{
            "attr": "x",
            "attrPath": ["x"],
            "drvPath": "/nix/store/a-x-1.0.drv",
            "name": "x-1.0",
            "outputs": {"out": "/nix/store/b-x-1.0"},
            "system": "x86_64-linux",
            "meta": { "license": "mit" }
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        let lic = drv.meta.unwrap().license.unwrap();
        match lic {
            LicenseField::Shorthand(s) => assert_eq!(s, "mit"),
            other => panic!("expected shorthand, got {:?}", other),
        }
    }

    #[test]
    fn test_license_list_normalises() {
        // Multi-licensed package emits a list of license objects.
        let line = r#"{
            "attr": "x",
            "attrPath": ["x"],
            "drvPath": "/nix/store/a-x-1.0.drv",
            "name": "x-1.0",
            "outputs": {"out": "/nix/store/b-x-1.0"},
            "system": "x86_64-linux",
            "meta": {
                "license": [
                    {"shortName": "bsd3", "spdxId": "BSD-3-Clause", "free": true},
                    {"shortName": "mit", "spdxId": "MIT", "free": true}
                ]
            }
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        let lic = drv.meta.unwrap().license.unwrap();
        let normalised = lic.normalise();
        assert_eq!(normalised.len(), 2);
        assert_eq!(normalised[0].short_name.as_deref(), Some("bsd3"));
        assert_eq!(normalised[1].short_name.as_deref(), Some("mit"));
    }

    #[test]
    fn test_unfree_license() {
        let line = r#"{
            "attr": "x",
            "attrPath": ["x"],
            "drvPath": "/nix/store/a-x-1.0.drv",
            "name": "x-1.0",
            "outputs": {"out": "/nix/store/b-x-1.0"},
            "system": "x86_64-linux",
            "meta": {
                "license": {"shortName": "unfree", "free": false}
            }
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        let normalised = drv.meta.unwrap().license.unwrap().normalise();
        assert_eq!(normalised.len(), 1);
        assert_eq!(normalised[0].free, Some(false));
        assert_eq!(normalised[0].short_name.as_deref(), Some("unfree"));
    }

    #[test]
    fn test_meta_with_pname_version_overrides_heuristic() {
        // When meta.pname / meta.version are set explicitly, they override
        // the heuristic (which would otherwise be wrong for unusual `name` formats).
        let line = r#"{
            "attr": "x",
            "attrPath": ["x"],
            "drvPath": "/nix/store/a-weirdname.drv",
            "name": "weirdname",
            "outputs": {"out": "/nix/store/b-weirdname"},
            "system": "x86_64-linux",
            "meta": { "pname": "actual-pname", "version": "9.9.9" }
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        assert_eq!(drv.pname_or_heuristic(), "actual-pname");
        assert_eq!(drv.version_or_heuristic(), Some("9.9.9".to_string()));
    }

    #[test]
    fn test_meta_homepage_list() {
        let line = r#"{
            "attr": "x",
            "attrPath": ["x"],
            "drvPath": "/nix/store/a-x-1.0.drv",
            "name": "x-1.0",
            "outputs": {"out": "/nix/store/b-x-1.0"},
            "system": "x86_64-linux",
            "meta": {
                "homepage": ["https://a.example.com", "https://b.example.com"]
            }
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        let urls = drv.meta.unwrap().homepage.unwrap().into_vec();
        assert_eq!(
            urls,
            vec![
                "https://a.example.com".to_string(),
                "https://b.example.com".to_string()
            ]
        );
    }

    #[test]
    fn test_meta_empty_object() {
        // A package with `meta = {}` should parse with all fields None.
        let line = r#"{
            "attr": "x",
            "attrPath": ["x"],
            "drvPath": "/nix/store/a-x-1.0.drv",
            "name": "x-1.0",
            "outputs": {"out": "/nix/store/b-x-1.0"},
            "system": "x86_64-linux",
            "meta": {}
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        let meta = drv.meta.expect("meta object");
        assert!(meta.pname.is_none());
        assert!(meta.license.is_none());
        assert!(meta.maintainers.is_none());
    }

    #[test]
    fn test_to_drv_metadata_full_meta() {
        let line = r#"{
            "attr": "hello",
            "attrPath": ["hello"],
            "drvPath": "/nix/store/abc-hello-2.12.1.drv",
            "name": "hello-2.12.1",
            "outputs": {"out": "/nix/store/xyz-hello-2.12.1"},
            "system": "x86_64-linux",
            "meta": {
                "license": {
                    "shortName": "gpl3Plus",
                    "spdxId": "GPL-3.0-or-later",
                    "free": true
                },
                "maintainers": [
                    {"name": "Eelco Dolstra", "github": "edolstra"}
                ],
                "position": "pkgs/applications/misc/hello/default.nix:34",
                "broken": false,
                "insecure": false
            }
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        let m = drv.to_drv_metadata();

        assert_eq!(m.pname.as_deref(), Some("hello"));
        assert_eq!(m.version.as_deref(), Some("2.12.1"));
        assert_eq!(
            m.meta_position.as_deref(),
            Some("pkgs/applications/misc/hello/default.nix:34")
        );
        assert_eq!(m.broken, Some(false));
        assert_eq!(m.insecure, Some(false));

        // Round-trip the JSON-encoded license / maintainers payloads to
        // confirm they're well-formed and contain expected fields.
        let lic: Vec<LicenseEntry> =
            serde_json::from_str(m.license_json.as_deref().expect("license_json present"))
                .expect("license_json deserializes");
        assert_eq!(lic.len(), 1);
        assert_eq!(lic[0].spdx_id.as_deref(), Some("GPL-3.0-or-later"));

        let maints: Vec<MaintainerEntry> = serde_json::from_str(
            m.maintainers_json
                .as_deref()
                .expect("maintainers_json present"),
        )
        .expect("maintainers_json deserializes");
        assert_eq!(maints.len(), 1);
        assert_eq!(maints[0].github.as_deref(), Some("edolstra"));
    }

    #[test]
    fn test_to_drv_metadata_no_meta_falls_back_to_heuristic() {
        // No meta block — license/maintainers/position should be None so the
        // ON CONFLICT COALESCE preserves any prior value. pname/version still
        // come from the name heuristic.
        let line = r#"{
            "attr": "hello",
            "attrPath": ["hello"],
            "drvPath": "/nix/store/abc-hello-2.12.1.drv",
            "name": "hello-2.12.1",
            "outputs": {"out": "/nix/store/xyz-hello-2.12.1"},
            "system": "x86_64-linux"
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        let m = drv.to_drv_metadata();
        assert_eq!(m.pname.as_deref(), Some("hello"));
        assert_eq!(m.version.as_deref(), Some("2.12.1"));
        assert!(m.license_json.is_none());
        assert!(m.maintainers_json.is_none());
        assert!(m.meta_position.is_none());
        assert!(m.broken.is_none());
        assert!(m.insecure.is_none());
    }

    #[test]
    fn test_to_drv_metadata_empty_maintainers_collapsed_to_none() {
        // `meta.maintainers = []` should NOT serialize to `"[]"` — we collapse
        // empty lists to None to avoid the renderer needing to special-case
        // an empty JSON array later.
        let line = r#"{
            "attr": "x",
            "attrPath": ["x"],
            "drvPath": "/nix/store/a-x-1.0.drv",
            "name": "x-1.0",
            "outputs": {"out": "/nix/store/b-x-1.0"},
            "system": "x86_64-linux",
            "meta": { "maintainers": [] }
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        let m = drv.to_drv_metadata();
        assert!(m.maintainers_json.is_none());
    }

    #[test]
    fn test_to_drv_metadata_shorthand_license_normalised() {
        // Bare-string `license: "mit"` is normalised into a single-entry list
        // with only `shortName` populated. Round-trip the JSON to verify.
        let line = r#"{
            "attr": "x",
            "attrPath": ["x"],
            "drvPath": "/nix/store/a-x-1.0.drv",
            "name": "x-1.0",
            "outputs": {"out": "/nix/store/b-x-1.0"},
            "system": "x86_64-linux",
            "meta": { "license": "mit" }
        }"#;
        let drv: NixEvalDrv = serde_json::from_str(line).expect("parse");
        let m = drv.to_drv_metadata();
        let lic: Vec<LicenseEntry> =
            serde_json::from_str(m.license_json.as_deref().unwrap()).unwrap();
        assert_eq!(lic.len(), 1);
        assert_eq!(lic[0].short_name.as_deref(), Some("mit"));
        assert!(lic[0].spdx_id.is_none());
    }

    #[test]
    fn test_error() {
        let err = r##"{"attr":"adoptopenjdk-openj9-bin-15","attrPath":["adoptopenjdk-openj9-bin-15"],"error":"error:\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:7:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |       ^\n          218|     ) aliases;\n\n       … while calling anonymous lambda\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:10:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |          ^\n          218|     ) aliases;\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:17:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |                 ^\n          218|     ) aliases;\n\n       … while calling 'removeDistribute'\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:34:22:\n           33|   # sets from building on Hydra.\n           34|   removeDistribute = alias: if lib.isDerivation alias then lib.dontDistribute alias else alias;\n             |                      ^\n           35|\n\n       … while evaluating a branch condition\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:34:29:\n           33|   # sets from building on Hydra.\n           34|   removeDistribute = alias: if lib.isDerivation alias then lib.dontDistribute alias else alias;\n             |                             ^\n           35|\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:34:32:\n           33|   # sets from building on Hydra.\n           34|   removeDistribute = alias: if lib.isDerivation alias then lib.dontDistribute alias else alias;\n             |                                ^\n           35|\n\n       … while calling 'isDerivation'\n         at /home/jon/projects/nixpkgs/lib/attrsets.nix:1251:18:\n         1250|   */\n         1251|   isDerivation = value: value.type or null == \"derivation\";\n             |                  ^\n         1252|\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:35:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |                                   ^\n          218|     ) aliases;\n\n       … while calling 'removeRecurseForDerivations'\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:26:5:\n           25|   removeRecurseForDerivations =\n           26|     alias:\n             |     ^\n           27|     if alias.recurseForDerivations or false then\n\n       … while evaluating a branch condition\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:27:5:\n           26|     alias:\n           27|     if alias.recurseForDerivations or false then\n             |     ^\n           28|       lib.removeAttrs alias [ \"recurseForDerivations\" ]\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:64:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |                                                                ^\n          218|     ) aliases;\n\n       … while calling 'checkInPkgs'\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:211:8:\n          210|   checkInPkgs =\n          211|     n: alias:\n             |        ^\n          212|     if builtins.hasAttr n super then throw \"Alias ${n} is still in all-packages.nix\" else alias;\n\n       … while calling the 'throw' builtin\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:257:32:\n          256|   adoptopenjdk-openj9-bin-11 = throw \"adoptopenjdk has been removed as the upstream project is deprecated. Consider using `semeru-bin-11`.\"; # Added 2024-05-09\n          257|   adoptopenjdk-openj9-bin-15 = throw \"adoptopenjdk has been removed as the upstream project is deprecated. JDK 15 is also EOL. Consider using `semeru-bin-17`.\"; # Added 2024-05-09\n             |                                ^\n          258|   adoptopenjdk-openj9-bin-16 = throw \"adoptopenjdk has been removed as the upstream project is deprecated. JDK 16 is also EOL. Consider using `semeru-bin-17`.\"; # Added 2024-05-09\n\n       error: adoptopenjdk has been removed as the upstream project is deprecated. JDK 15 is also EOL. Consider using `semeru-bin-17`."}"##;
        let _item = serde_json::from_str::<NixEvalItem>(err).expect("Failed to deserialize output");
    }
}
