use std::{collections, fmt};

// utility for logging topo_sort results lazily
struct TopoNodes<'a, V>(topo_sort::SortResults<(&'a V, &'a collections::HashSet<V>)>);

pub fn sort_packages(packages: &mut [cargo_metadata::Package]) -> anyhow::Result<()> {
    const DEP_CYCLE_ERR: &str = "Dependency cycle detected! Resolve this using other cargo commands first (e.g. `cargo build` should fail with a decent error message).";

    let mut topo = topo_sort::TopoSort::<&str>::with_capacity(packages.len());
    for package in packages.iter() {
        topo.insert(
            package.name.as_str(),
            package.dependencies.iter().map(|d| d.name.as_str()),
        );
    }

    tracing::debug!(nodes=?TopoNodes(topo.to_vec()), "topo sorted nodes");

    // Contains a mapping of package name to topological sort index
    let order: collections::HashMap<String, usize> = topo
        .nodes()
        .enumerate()
        .map(|(idx, node_result)| Ok(((*node_result?).to_string(), idx)))
        .collect::<anyhow::Result<_>>()
        .map_err(|_| anyhow::anyhow!(DEP_CYCLE_ERR))?;

    packages.sort_by_key(|p| order[p.name.as_str()]);

    tracing::debug!(packages=?(packages.iter().map(|p| p.name.as_str()).collect::<Vec<_>>()), "topo sorted package order");

    Ok(())
}

impl<'a, V> fmt::Debug for TopoNodes<'a, V>
where
    V: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let items = match self.0 {
            topo_sort::SortResults::Full(ref items) => {
                f.write_str("full:")?;
                items
            }
            topo_sort::SortResults::Partial(ref items) => {
                f.write_str("partial:")?;
                items
            }
        };
        f.debug_list()
            .entries(items.iter().map(|(v, _)| v))
            .finish()
    }
}
