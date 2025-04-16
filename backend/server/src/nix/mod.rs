use anyhow::Result;
use std::collections::HashMap;
use std::process::Command;
use tracing::{debug, info};

/// Given a drv, traverse all direct drv dependencies
pub fn traverse_drvs(drv_path: &str) -> Result<HashMap<String, Vec<String>>> {
    debug!("traversing {}", drv_path);

    let mut drv_to_references: HashMap<String, Vec<String>> = HashMap::new();
    inner_traverse_drvs(&mut drv_to_references, drv_path)?;

    debug!("drvs {:?}", drv_to_references);
    Ok(drv_to_references)
}

/// To avoid lifetime issues, we do a recursive descent instead of a loop
/// instead of appending to an iterator :(
fn inner_traverse_drvs(drv_map: &mut HashMap<String, Vec<String>>, drv_path: &str) -> Result<()> {
    let references = drv_references(drv_path)?;
    drv_map.insert(drv_path.to_string(), references.clone());

    for drv in references.into_iter() {
        if drv_map.contains_key(&drv) {
            continue;
        }
        debug!("traversing {}", &drv);
        inner_traverse_drvs(drv_map, &drv)?;
    }

    Ok(())
}

/// Retreive the direct dependencies of a drv
fn drv_references(drv_path: &str) -> Result<Vec<String>> {
    let output = Command::new("nix-store")
        .args(&["--query", "--references", &drv_path])
        .output()?
        .stdout;
    let drv_str = String::from_utf8(output)?;

    let drvs = drv_str
        .lines()
        // drv references can include "inputSrcs" which are not inputDrvs
        // but rather files which were added to the nix store through `nix-store --add`
        .filter(|x| x.ends_with(".drv"))
        .map(|x| x.to_string())
        .collect::<Vec<String>>();

    Ok(drvs)
}
