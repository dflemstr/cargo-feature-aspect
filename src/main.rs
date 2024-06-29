#![warn(clippy::all, clippy::cargo)]
#![doc = include_str!("../README.md")]

use std::{borrow, cmp, collections, fmt, fs, path, process};

#[derive(Debug, clap::Parser)]
#[command(bin_name = "cargo", styles = clap_cargo::style::CLAP_STYLING, subcommand_required = true)]
enum Command {
    FeatureAspect(FeatureAspectArgs),
}

/// Creates and updates feature aspects in a workspace.
///
/// A feature aspect is a feature that should generally exist for all crates in a workspace
/// that depend on some shared crate.  This shared crate's feature will be propagated to all
/// of its dependees.
///
/// For example, if a hypothetical `logging` crate has a `enable-tracing` feature, all crates
/// that depend on `logging` might want to have their own `enable-tracing` feature, that enables
/// additional tracing stuff in the local crate, and also enables the `logging` feature on all
/// dependency crates.
///
/// This command creates and updates such a feature aspect across the crate graph.
///
/// Example usages:
///
/// ```shell
/// # Any crate that indirectly depends on the `logging` crate should have a feature
/// # `enable-tracing` that is propagated through all dependency crates.
/// cargo feature-aspect --name enable-tracing --leaf-feature logging/enable-tracing
///
/// # Same as above, but `--name` is inferred from `--leaf-feature`.
/// cargo feature-aspect --leaf-feature logging/enable-tracing
///
/// # Any such crate should also enable the `logging` optional dependency.
/// cargo feature-aspect --leaf-feature logging/enable-tracing --add-feature-param dep:logging
///
/// # Do not re-order the `enable-tracing` feature param array when it is not already in
/// # alphabetical order.
/// cargo feature-aspect --leaf-feature logging/enable-tracing --no-sort
///
/// # Dry-run to see what changes would be made
/// cargo feature-aspect --leaf-feature logging/enable-tracing --dry-run
///
/// # Verify that the feature aspect is up-to-date (useful for CI)
/// cargo feature-aspect --leaf-feature logging/enable-tracing --verify
/// ```
#[derive(Debug, clap::Args)]
struct FeatureAspectArgs {
    /// The name of the resulting feature aspect.
    ///
    /// This will be inferred from the name of the leaf feature if there's only one, but it might
    /// be useful to specify this flag if there are multiple leaf features.
    #[arg(short, long)]
    name: Option<String>,

    /// The leaf features to match/propagate, e.g. `enable-tracing` from the example above, or
    /// `logging/enable-tracing` to only match a specific crate.
    ///
    /// These are the features that identify "root crates" that should have their features spread to
    /// all dependee crates.
    #[arg(short = 'f', long = "leaf-feature")]
    leaf_features: Vec<String>,

    /// Add extra elements to the generated feature.  For example, it might be useful to add a
    /// `dep:logging` to all features that have the `enable-tracing` feature from the example above.
    #[arg(short, long = "add-feature-param")]
    add_feature_params: Vec<String>,

    /// Do not modify `Cargo.toml` files, instead print the changes that would be made.
    #[arg(short, long)]
    dry_run: bool,

    /// Do not modify `Cargo.toml` files, instead fail the command if changes would be made.
    #[arg(short, long)]
    verify: bool,

    /// Do not sort params the feature spec lexicographically.  If specified, new features are added
    /// to the end instead.
    ///
    /// For example, by default `myfeature = ["b/myfeature", "a/myfeature"]` will be changed to have
    /// `a` come before `b`, but this flag disables that behavior.
    #[arg(long)]
    no_sort: bool,

    /// Provide the path to the `Cargo.toml` to start with.
    ///
    /// This arg mainly exists to maintain compatibility with the standard `cargo` flag of the same
    /// name.
    #[arg(long)]
    manifest_path: Option<path::PathBuf>,

    /// Run without accessing the network.
    ///
    /// This arg mainly exists to maintain compatibility with the standard `cargo` flag of the same
    /// name.
    #[arg(long)]
    offline: bool,

    /// Require `Cargo.toml` to be up-to-date.
    ///
    /// This arg mainly exists to maintain compatibility with the standard `cargo` flag of the same
    /// name.
    #[arg(long)]
    locked: bool,
}

struct Context<'a> {
    feature_name: borrow::Cow<'a, str>,
    extra_feature_params: Vec<&'a str>,
    dry_run: bool,
    verify: bool,
    sort: bool,
    has_changes: bool,
    unqualified_leaf_features: Vec<&'a str>,
    qualified_leaf_features: Vec<(&'a str, &'a str)>,
    in_scope_packages: collections::HashSet<&'a str>,
}

fn main() {
    tracing_subscriber::fmt::init();
    let command: Command = clap::Parser::parse();

    match command {
        Command::FeatureAspect(args) => {
            if let Err(e) = run_feature_aspect(&args) {
                tracing::error!("fatal error: {}", e);
                for reason in e.chain().skip(1) {
                    tracing::error!("  {}", reason);
                }
                process::exit(1);
            }
        },
    }
}

fn run_feature_aspect(args: &FeatureAspectArgs) -> anyhow::Result<()> {
    tracing::debug!("resolving workspace metadata");
    let metadata = resolve_ws(args.manifest_path.as_deref(), args.locked, args.offline)?;
    tracing::debug!("enumerating workspace members");
    let mut packages = find_ws_members(metadata);
    tracing::debug!("doing topological sort of workspace members");
    topo_sort_packages(&mut packages)?;

    let mut ctx = create_context(args)?;
    for package in &packages {
        visit_package(package, &mut ctx)?;
    }

    if ctx.verify && ctx.has_changes {
        anyhow::bail!("failing because --verify was passed and changes were detected");
    }

    Ok(())
}

fn create_context(args: &FeatureAspectArgs) -> anyhow::Result<Context> {
    let feature_name = if let Some(name) = &args.name {
        name.into()
    } else if let &[name] = &args.leaf_features.as_slice() {
        // We have exactly one leaf feature, see if it is scoped by package
        if let Some((_, feature)) = name.split_once('/') {
            feature.into()
        } else {
            name.into()
        }
    } else {
        anyhow::bail!("Must specify exactly one --leaf-feature or else specify --name")
    };

    let extra_feature_params = args.add_feature_params.iter().map(String::as_str).collect();
    let dry_run = args.dry_run;
    let verify = args.verify;
    let sort = !args.no_sort;
    let has_changes = false;

    // We expect these to be tiny, so it's overkill to use a hash data structure
    let mut unqualified_leaf_features = Vec::new();
    let mut qualified_leaf_features = Vec::new();

    for leaf_feature in &args.leaf_features {
        if let Some((pkg, feature)) = leaf_feature.split_once('/') {
            if !qualified_leaf_features.contains(&(pkg, feature)) {
                qualified_leaf_features.push((pkg, feature));
            }
        } else {
            let feature = leaf_feature.as_str();
            if !unqualified_leaf_features.contains(&feature) {
                unqualified_leaf_features.push(feature);
            }
        }
    }

    // This might have relatively many elems so might make sense to hash values here
    let in_scope_packages = collections::HashSet::new();

    let context = Context {
        feature_name,
        extra_feature_params,
        dry_run,
        verify,
        sort,
        has_changes,
        unqualified_leaf_features,
        qualified_leaf_features,
        in_scope_packages,
    };

    Ok(context)
}

fn topo_sort_packages(packages: &mut [cargo_metadata::Package]) -> anyhow::Result<()> {
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

#[tracing::instrument(skip_all, fields(package = package.name))]
fn visit_package<'a>(
    package: &'a cargo_metadata::Package,
    ctx: &mut Context<'a>,
) -> anyhow::Result<()> {
    let pkg_name = &package.name;
    let mut is_in_scope = false;
    let mut referenced_leaf_features = Vec::new();

    for feature in package.features.keys() {
        if ctx.unqualified_leaf_features.contains(&feature.as_str())
            || ctx.qualified_leaf_features.contains(&(pkg_name, feature))
        {
            tracing::debug!("package has leaf feature");
            is_in_scope = true;

            if ctx.feature_name.as_ref() != feature.as_str() {
                // It might be the case that our main feature is named something totally different
                // from the leaf feature, which means that we should add the leaf feature as a
                // dependency for our main feature.
                referenced_leaf_features.push(feature.as_str());
            }
        }
    }

    for dependency in &package.dependencies {
        if ctx.in_scope_packages.contains(dependency.name.as_str()) {
            tracing::debug!(
                dependency = dependency.name,
                "package depends on in-scope dependency"
            );
            is_in_scope = true;
        }
    }

    if is_in_scope {
        tracing::debug!("package considered in scope for feature aspect; ensuring feature exists");
        ctx.in_scope_packages.insert(pkg_name);

        // Unfortunately at this point we cannot trust the `package.features` for diffing, because
        // some of the metadata features might be implicitly generated.  We will instead need to
        // check against the actual manifest file no matter what.
        visit_aspect_feature(package, ctx, &referenced_leaf_features)?;
    }

    Ok(())
}

fn visit_aspect_feature(
    package: &cargo_metadata::Package,
    ctx: &mut Context,
    referenced_leaf_features: &[&str],
) -> anyhow::Result<()> {
    let pkg_name = &package.name;
    let feature = &ctx.feature_name;

    // Params to possibly add, however a check will be made later to remove duplicates
    let mut params_to_add: Vec<borrow::Cow<str>> = Vec::new();
    // Params to remove, however a check will be made later to see if they actually exist
    let mut params_to_remove: Vec<borrow::Cow<str>> = Vec::new();

    // Here we do lots of `Vec::contains` but since these are small vecs, it is not worth it
    // to do some fancy hash set stuff, since hasing all the strings will probably take more
    // time than just traversing the vec.

    // Ensure that we propagate the feature to our dependencies.
    for dep in &package.dependencies {
        if ctx.in_scope_packages.contains(dep.name.as_str()) {
            let non_optional_dep_spec = format!("{}/{}", dep.name, feature);
            let optional_dep_spec = format!("{}?/{}", dep.name, feature);

            // Gracefully handle when a dependency might have changed its "optional status" from
            // previous runs.
            let (dep_spec_to_add, dep_spec_to_remove) = if dep.optional {
                (optional_dep_spec, non_optional_dep_spec)
            } else {
                (non_optional_dep_spec, optional_dep_spec)
            };

            params_to_add.push(dep_spec_to_add.into());
            params_to_remove.push(dep_spec_to_remove.into());
        }
    }

    // Ensure extra params are present
    for &param in ctx
        .extra_feature_params
        .as_slice()
        .iter()
        .chain(referenced_leaf_features.iter())
    {
        let should_include = if let Some((prefix, suffix)) = param.split_once(':') {
            if prefix == "dep" {
                // Special-case: we only include dep references if the dep actually exists.
                // This would otherwise be very annoying to express with some sort of CLI flags,
                // so we just handle it by default.
                package.dependencies.iter().any(|d| d.name == suffix)
            } else {
                true
            }
        } else {
            true
        };

        if should_include {
            params_to_add.push(param.into());
        }
    }

    // TODO: is it clean to just make the file change here? Feels a bit dirty/hidden away...
    let contents = fs::read_to_string(&package.manifest_path)?;

    if ctx.dry_run || ctx.verify {
        // We need to parse the actual manifest file to remove implicit features that get
        // auto-generated by cargo at runtime
        let doc: toml::Table = toml::from_str(&contents)?;

        let current_params = doc
            // [features]
            // ...
            .get("features")
            // [features]
            // my-feature = ...
            .and_then(|f| f.get(feature.as_ref()))
            // [features]
            // my-feature = [...]
            .and_then(|f| f.as_array())
            // [features]
            // my-feature = ["..."]
            .map(|values| values.iter().flat_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        params_to_add.retain(|p| !current_params.contains(&p.as_ref()));
        params_to_remove.retain(|p| current_params.contains(&p.as_ref()));

        // Describe changes that would be made
        if !params_to_add.is_empty() {
            for param in &params_to_add {
                tracing::info!(?feature, ?param, "would add param");
                ctx.has_changes = true;
            }
            eprintln!(
                "  package `{pkg_name}` feature `{feature}`: would add `{:?}` to the feature array",
                params_to_add.as_slice()
            );
        }

        if !params_to_remove.is_empty() {
            for param in &params_to_remove {
                tracing::info!(?feature, ?param, "would remove param");
                ctx.has_changes = true;
            }
            eprintln!("  package `{pkg_name}` feature `{feature}`: would remove `{:?}` from the feature array", params_to_remove.as_slice());
        }
    } else {
        let mut doc: toml_edit::DocumentMut = contents.parse()?;
        // Not in dry-run/verify mode, let's make the changes
        tracing::debug!(manifest_path=?package.manifest_path, "editing manifest file");
        if pkg_name == "cyw43-pio" {
            println!("break");
        }

        let features = doc.entry("features")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("failed to edit manifest for package `{}`: the `features` field exists but is not a table!", package.name))?;
        let feature_arr = features.entry(feature.as_ref())
            .or_insert_with(|| toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new())))
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("failed to edit manifest for package `{}`: `features.{}` exists but is not an array!", package.name, feature))?;

        params_to_add.retain(|param| !feature_arr.iter().any(|p| p.as_str() == Some(param.as_ref())));

        let mut param_indices_to_remove = Vec::new();
        for param in params_to_remove {
            if let Some(idx) = feature_arr
                .iter()
                .position(|p| p.as_str() == Some(param.as_ref()))
            {
                param_indices_to_remove.push(idx);
            }
        }

        // Reverse sort indices to make it safe to remove them one by one from the array without
        // invalidating later indices
        param_indices_to_remove.sort_by(|a, b| b.cmp(a));

        for idx in param_indices_to_remove {
            feature_arr.remove(idx);
        }

        // Awkward sorting functions because `.sort_by_key()` doesn't handle sort keys with
        // lifetimes nicely
        fn sort_key(param: &str) -> (bool, &str) {
            if param.starts_with("dep:") {
                (false, param)
            } else {
                (true, param)
            }
        }

        fn sort_ord(a: &str, b: &str) -> cmp::Ordering {
            sort_key(a).cmp(&sort_key(b))
        }

        // If sorting the existing array is disabled, at least sort the new stuff we're adding.
        // We don't need to do this if we're going to be sorting the TOML array later anyway.
        if !ctx.sort {
            params_to_add.sort_by(|a, b| sort_ord(a.as_ref(), b.as_ref()));
        }

        for param in params_to_add {
            feature_arr.push_formatted(toml_edit::Value::String(toml_edit::Formatted::new(
                param.into_owned(),
            )));
        }

        if ctx.sort {
            feature_arr
                .sort_by(|a, b| sort_ord(a.as_str().unwrap_or(""), b.as_str().unwrap_or("")));
            feature_arr.fmt();
        }

        fs::write(&package.manifest_path, doc.to_string())?;
    }

    Ok(())
}

fn resolve_ws(
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

fn find_ws_members(ws: cargo_metadata::Metadata) -> Vec<cargo_metadata::Package> {
    let workspace_members: collections::HashSet<_> = ws.workspace_members.iter().collect();
    ws.packages
        .into_iter()
        .filter(|p| workspace_members.contains(&p.id))
        .collect()
}

struct TopoNodes<'a, V>(topo_sort::SortResults<(&'a V, &'a collections::HashSet<V>)>);

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

#[test]
fn verify_cli() {
    use clap::CommandFactory as _;
    Command::command().debug_assert();
}
