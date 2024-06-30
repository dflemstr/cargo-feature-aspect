use std::{collections, path};

pub fn resolve_ws(
    manifest_path: Option<&path::Path>,
    locked: bool,
    offline: bool,
) -> anyhow::Result<cargo_metadata::Metadata> {
    let mut cmd = cargo_metadata::MetadataCommand::new();
    if let Some(manifest_path) = manifest_path {
        cmd.manifest_path(manifest_path);
    }
    cmd.features(cargo_metadata::CargoOpt::AllFeatures);
    let mut other = Vec::new();
    if locked {
        other.push("--locked".to_owned());
    }
    if offline {
        other.push("--offline".to_owned());
    }
    cmd.other_options(other);

    let ws = cmd.exec().or_else(|_| {
        cmd.no_deps();
        cmd.exec()
    })?;
    Ok(ws)
}

pub fn find_ws_members(ws: cargo_metadata::Metadata) -> Vec<cargo_metadata::Package> {
    let workspace_members: collections::HashSet<_> = ws.workspace_members.iter().collect();
    ws.packages
        .into_iter()
        .filter(|p| workspace_members.contains(&p.id))
        .collect()
}
