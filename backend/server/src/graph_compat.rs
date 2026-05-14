// Compatibility layer for converting between server types and shared/graph types

use std::str::FromStr;

use crate::db::model::{Drv as ServerDrv, DrvId as ServerDrvId};

/// Convert server DrvId to shared DrvId
pub fn to_shared_drv_id(server_id: &ServerDrvId) -> anyhow::Result<shared::types::DrvId> {
    server_id.to_string().parse()
}

/// Convert shared DrvId to server DrvId
pub fn to_server_drv_id(shared_id: &shared::types::DrvId) -> anyhow::Result<ServerDrvId> {
    ServerDrvId::from_str(&shared_id.to_string())
}

/// Convert a Vec of server DrvIds to shared DrvIds
pub fn to_shared_drv_ids(server_ids: &[ServerDrvId]) -> anyhow::Result<Vec<shared::types::DrvId>> {
    server_ids
        .iter()
        .map(to_shared_drv_id)
        .collect()
}

/// Convert a Vec of shared DrvIds to server DrvIds
pub fn to_server_drv_ids(shared_ids: &[shared::types::DrvId]) -> anyhow::Result<Vec<ServerDrvId>> {
    shared_ids
        .iter()
        .map(to_server_drv_id)
        .collect()
}

/// Convert server Drv to shared Drv
pub fn to_shared_drv(server_drv: &ServerDrv) -> anyhow::Result<shared::types::Drv> {
    Ok(shared::types::Drv {
        drv_path: to_shared_drv_id(&server_drv.drv_path)?,
        system: server_drv.system.clone(),
        prefer_local_build: server_drv.prefer_local_build,
        required_system_features: server_drv.required_system_features.clone(),
        is_fod: server_drv.is_fod,
        build_state: crate::db::graph_impl::convert_build_state(&server_drv.build_state),
        output_size: server_drv.output_size,
        closure_size: server_drv.closure_size,
        pname: server_drv.pname.clone(),
        version: server_drv.version.clone(),
        license_json: server_drv.license_json.clone(),
        maintainers_json: server_drv.maintainers_json.clone(),
        meta_position: server_drv.meta_position.clone(),
        broken: server_drv.broken,
        insecure: server_drv.insecure,
    })
}

/// Convert a Vec of server Drvs to shared Drvs
pub fn to_shared_drvs(server_drvs: &[ServerDrv]) -> anyhow::Result<Vec<shared::types::Drv>> {
    server_drvs
        .iter()
        .map(to_shared_drv)
        .collect()
}
