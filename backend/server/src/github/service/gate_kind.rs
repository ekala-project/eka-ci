//! Classify a coalesced gate (jobs sharing a dotted prefix) for its title.
//!
//! Two kinds of attr set produce `prefix.name` jobs:
//! - variants of one package (`ruby.v3_4_6`, `ruby.v4_0_5`), which share a `pname`;
//! - a package set / scope (`python313Packages.cryptography`, `python313Packages.bcrypt`),
//!   whose members are different packages.
//!
//! The attr names alone cannot tell them apart, so the members' `pname`s decide
//! (see [`GateKind::from_pnames`]).

use std::collections::HashSet;

use crate::db::github::NewOrChangedJob;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateKind {
    /// Variants of a single package: all known `pname`s agree.
    Variants,
    /// A package set: members have differing `pname`s.
    PackageSet,
}

impl GateKind {
    /// Classify a group named `group_name` from its (changed) members.
    pub(crate) fn of(group_name: &str, jobs: &[&NewOrChangedJob]) -> Self {
        Self::from_pnames(group_name, jobs.iter().map(|j| j.pname.as_deref()))
    }

    /// Variants of one package share a `pname`, so a group is `Variants`
    /// when all its known pnames agree and either there are at least two
    /// members (`java.*` are all `openjdk`) or the lone member's pname is
    /// the group name (a PR changing only `linux.v7_3`). A group with no
    /// known `pname` stays `Variants`.
    ///
    /// Differing pnames always mean a package set; a variant package whose
    /// variants change pname is a bug in that package's expression. A
    /// single changed scope member (`python313Packages.cryptography`) is
    /// also a package set.
    fn from_pnames<'a>(group_name: &str, pnames: impl Iterator<Item = Option<&'a str>>) -> Self {
        let known: Vec<&str> = pnames.flatten().filter(|p| !p.is_empty()).collect();
        let distinct: HashSet<&str> = known.iter().copied().collect();
        let is_variants = match known.as_slice() {
            [] => true,
            [only] => only.eq_ignore_ascii_case(group_name),
            _ => distinct.len() == 1,
        };
        if is_variants {
            GateKind::Variants
        } else {
            GateKind::PackageSet
        }
    }

    /// Word placed after the group name in the check-run title.
    pub(crate) fn label(self) -> &'static str {
        match self {
            GateKind::Variants => "variants",
            GateKind::PackageSet => "pkgs",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(group: &str, pnames: &[Option<&str>]) -> GateKind {
        GateKind::from_pnames(group, pnames.iter().copied())
    }

    #[test]
    fn same_pname_is_variants() {
        assert_eq!(
            kind("ruby", &[Some("ruby"), Some("ruby")]),
            GateKind::Variants
        );
        assert_eq!(
            kind("java", &[Some("openjdk"), Some("openjdk")]),
            GateKind::Variants
        );
    }

    #[test]
    fn single_variant_named_like_group_is_variants() {
        assert_eq!(kind("linux", &[Some("linux")]), GateKind::Variants);
    }

    #[test]
    fn variants_with_differing_pnames_are_package_set() {
        let pnames = [Some("python3"), Some("python3-minimal")];
        assert_eq!(kind("python3", &pnames), GateKind::PackageSet);
    }

    #[test]
    fn differing_pnames_is_package_set() {
        let pnames = [Some("cryptography"), Some("bcrypt")];
        assert_eq!(kind("python313Packages", &pnames), GateKind::PackageSet);
        let pnames = [Some("xorgproto"), Some("xorg-server")];
        assert_eq!(kind("xorg", &pnames), GateKind::PackageSet);
    }

    #[test]
    fn single_scope_member_is_package_set() {
        let pnames = [Some("cryptography")];
        assert_eq!(kind("python313Packages", &pnames), GateKind::PackageSet);
    }

    #[test]
    fn unknown_pnames_do_not_count() {
        assert_eq!(kind("deno", &[None, None]), GateKind::Variants);
        let pnames = [Some("openjdk"), None, Some(""), Some("openjdk")];
        assert_eq!(kind("java", &pnames), GateKind::Variants);
    }

    #[test]
    fn labels() {
        assert_eq!(GateKind::Variants.label(), "variants");
        assert_eq!(GateKind::PackageSet.label(), "pkgs");
    }
}
