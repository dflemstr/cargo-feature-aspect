use crate::cli;
use std::{borrow, collections};

pub struct Context<'a> {
    pub feature_name: borrow::Cow<'a, str>,
    pub extra_feature_params: Vec<&'a str>,
    pub dry_run: bool,
    pub verify: bool,
    pub sort: bool,
    pub has_changes: bool,
    pub unqualified_leaf_features: Vec<&'a str>,
    pub qualified_leaf_features: Vec<(&'a str, &'a str)>,
    pub in_scope_packages: collections::HashSet<&'a str>,
}

impl<'a> Context<'a> {
    pub fn new(args: &'a cli::FeatureAspectArgs) -> anyhow::Result<Self> {
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
            anyhow::bail!("Must specify specify --name  or else specify exactly one --leaf-feature")
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

        Ok(Self {
            feature_name,
            extra_feature_params,
            dry_run,
            verify,
            sort,
            has_changes,
            unqualified_leaf_features,
            qualified_leaf_features,
            in_scope_packages,
        })
    }
}
