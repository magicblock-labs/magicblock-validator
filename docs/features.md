## Features

Managed by the `FeatureSet` `sdk/src/feature_set.rs`.

- works via a combination of feature flags that can be enabled/disabled and accounts
corresponding to each feature
- when trying to just activate all features while testing via `FeatureSet::all_enabled()` I ran
into the below error, most likely because I did not add the needed feature accounts
> Failed to create SBF VM: Invalid memory region at index 5
- see in `bank.rs` `process_genesis_config` which configures bank behavior based on the
enabled features
- when the solana validator is launched it configures features via deactivation (at least for
the test validator), see: `program-test/src/lib.rs:794`

```rs
for deactivate_feature_pk in &self.deactivate_feature_set {
    if FEATURE_NAMES.contains_key(deactivate_feature_pk) {
        match genesis_config.accounts.remove(deactivate_feature_pk) {
            Some(_) => debug!("Feature for {:?} deactivated", deactivate_feature_pk),
            None => warn!(
                "Feature {:?} set for deactivation not found in genesis_config account list, ignored.",
                deactivate_feature_pk
            ),
        }
    } else {
        warn!(
            "Feature {:?} set for deactivation is not a known Feature public key",
            deactivate_feature_pk
        );
    }
}
```

### Status

At this point we don't activate or deactivate any features.
