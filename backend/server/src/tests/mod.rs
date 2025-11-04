#[cfg(test)]
mod serialization {
    use crate::nix::derivation_show::*;

    #[test]
    pub fn deserialize_structured_attrs() {
        let contents = include_str!("./structured_attrs.json");
        let drv_output: DrvOutput = serde_json::from_str(&contents).unwrap();
        let drv_info = drv_output.drvs.into_iter().next().unwrap().1;

        assert_eq!(drv_info.system, "x86_64-linux");
        matches!(drv_info.env, EnvAttrs::StructuredAttrs { __json: _ });

        // let drv = drv_info.to_drv_info().unwrap();
        // assert_eq!(drv.pname, Some("python".to_owned()));
    }

    #[test]
    pub fn deserialize_legacy_attrs() {
        let contents = include_str!("./legacy_attrs.json");
        let drv_output: DrvOutput = serde_json::from_str(&contents).unwrap();
        let drv_info = drv_output.drvs.into_iter().next().unwrap().1;

        assert_eq!(drv_info.system, "x86_64-linux");
        matches!(drv_info.env, EnvAttrs::LegacyAttrs(_));
    }

    #[test]
    pub fn deserialize_fod_attrs() {
        let contents = include_str!("./fod_attrs.json");
        let drv_output: DrvOutput = serde_json::from_str(&contents).unwrap();
        let drv_info = drv_output.drvs.into_iter().next().unwrap().1;

        assert_eq!(drv_info.system, "x86_64-linux");
        matches!(drv_info.env, EnvAttrs::LegacyAttrs(_));

        let drv = drv_info.to_drv_info().unwrap();
        assert_eq!(drv.prefer_local, true);
        assert_eq!(drv.name, "Python-3.13.7.tar.xz".to_owned());
        assert_eq!(drv.pname, None);
        assert_eq!(drv.required_system_features, None);
        assert_eq!(drv.required_system_features_str, None);
        assert_eq!(drv.system, "x86_64-linux");
    }
}
