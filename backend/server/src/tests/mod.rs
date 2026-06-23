#[cfg(test)]
mod serialization {
    use eka_ci_server::nix::derivation_show::*;

    #[test]
    pub fn deserialize_structured_attrs() {
        let contents = include_str!("./structured_attrs.json");
        let drv_output: DrvOutput = serde_json::from_str(&contents).unwrap();
        let drv_info = drv_output.into_drvs().into_iter().next().unwrap().1;

        assert_eq!(drv_info.system, "x86_64-linux");

        let drv = drv_info.into_drv_info().unwrap();
        assert_eq!(drv.pname, Some("python3-minimal".to_owned()));
    }

    #[test]
    pub fn deserialize_legacy_attrs() {
        let contents = include_str!("./legacy_attrs.json");
        let drv_output: DrvOutput = serde_json::from_str(&contents).unwrap();
        let drv_info = drv_output.into_drvs().into_iter().next().unwrap().1;

        assert_eq!(drv_info.system, "x86_64-linux");

        let drv = drv_info.into_drv_info().unwrap();
        assert_eq!(drv.pname, Some("cmake".to_owned()));
    }

    #[test]
    pub fn deserialize_fod_attrs() {
        let contents = include_str!("./fod_attrs.json");
        let drv_output: DrvOutput = serde_json::from_str(&contents).unwrap();
        let drv_info = drv_output.into_drvs().into_iter().next().unwrap().1;

        assert_eq!(drv_info.system, "x86_64-linux");

        let drv = drv_info.into_drv_info().unwrap();
        assert_eq!(drv.prefer_local, true);
        assert_eq!(drv.name, "Python-3.13.7.tar.xz".to_owned());
        assert_eq!(drv.pname, None);
        assert_eq!(drv.required_system_features, None);
        assert_eq!(drv.required_system_features_str, None);
        assert_eq!(drv.system, "x86_64-linux");
        assert_eq!(
            drv.output_hash,
            Some("sha256-VGL5CZ39MOI43vg8cdkYl9jKpf9uvHpQ8U1IAs2qp5o=".to_owned())
        );
        assert_eq!(drv.is_fod(), true);
    }

    #[test]
    pub fn deserialize_versioned_format() {
        let contents = include_str!("./versioned_attrs.json");
        let drv_output: DrvOutput = serde_json::from_str(&contents).unwrap();
        let drv_info = drv_output.into_drvs().into_iter().next().unwrap().1;

        assert_eq!(drv_info.system, "x86_64-linux");

        let drv = drv_info.into_drv_info().unwrap();
        assert_eq!(drv.pname, Some("pytest".to_owned()));
        assert_eq!(drv.name, "python3.13-pytest-9.0.3");
        assert_eq!(drv.system, "x86_64-linux");
    }

    #[test]
    pub fn deserialize_versioned_fod_attrs() {
        let contents = include_str!("./versioned_fod_attrs.json");
        let drv_output: DrvOutput = serde_json::from_str(&contents).unwrap();
        let drv_info = drv_output.into_drvs().into_iter().next().unwrap().1;

        assert_eq!(drv_info.system, "x86_64-linux");

        // v4 FOD outputs have hash+method but no path
        let outputs = drv_info.outputs.as_ref().unwrap();
        assert!(outputs.get("out").unwrap().path.is_none());

        let drv = drv_info.into_drv_info().unwrap();
        assert_eq!(drv.name, "Python-3.13.7.tar.xz");
        assert!(drv.is_fod());
        assert_eq!(
            drv.output_hash,
            Some("sha256-VGL5CZ39MOI43vg8cdkYl9jKpf9uvHpQ8U1IAs2qp5o=".to_owned())
        );
    }

    #[test]
    pub fn deserialize_versioned_structured_attrs() {
        let contents = include_str!("./versioned_structured_attrs.json");
        let drv_output: DrvOutput = serde_json::from_str(&contents).unwrap();
        let drv_info = drv_output.into_drvs().into_iter().next().unwrap().1;

        assert_eq!(drv_info.system, "x86_64-linux");
        assert!(drv_info.structured_attrs.is_some());

        let drv = drv_info.into_drv_info().unwrap();
        assert_eq!(drv.pname, Some("python3-minimal".to_owned()));
        assert_eq!(drv.name, "python3-minimal-3.13.7");
        assert_eq!(drv.system, "x86_64-linux");
    }
}
