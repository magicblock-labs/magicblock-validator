enum ValidatorSetup {
    Devnet,
    Ephem,
    Both,
}

impl ValidatorSetup {
    fn devnet(&self) -> bool {
        use ValidatorSetup::*;
        matches!(self, Devnet | Both)
    }
    fn ephem(&self) -> bool {
        use ValidatorSetup::*;
        matches!(self, Ephem | Both)
    }
}

impl From<&str> for ValidatorSetup {
    fn from(value: &str) -> Self {
        use ValidatorSetup::*;
        match value.to_lowercase().as_str() {
            "devnet" => Devnet,
            "ephem" => Ephem,
            _ => Both,
        }
    }
}

pub struct TestConfigViaEnvVars {
    validators_only: Option<ValidatorSetup>,
    selected_tests: Vec<String>,
    skipped_tests: Vec<String>,
}

impl TestConfigViaEnvVars {
    pub fn skip_entirely(&self, test_name: &str) -> bool {
        !self.run_test(test_name)
            && !self.setup_devnet(test_name)
            && !self.setup_ephem(test_name)
    }

    pub fn run_test(&self, test_name: &str) -> bool {
        self.validators_only.is_none() && self.include_test(test_name)
    }

    fn include_test(&self, test_name: &str) -> bool {
        (self.selected_tests.is_empty() && !self.is_skipped(test_name))
            || self.selected_tests.contains(&test_name.to_string())
    }

    /// Returns true if `test_name` is in `SKIP_TESTS`, either as an exact
    /// match or via an umbrella alias (see `umbrella_aliases_for`).
    /// Preserves backwards-compatible behavior of e.g. `SKIP_TESTS=committor`
    /// suppressing every committor_* shard.
    fn is_skipped(&self, test_name: &str) -> bool {
        self.skipped_tests.iter().any(|skipped| {
            skipped == test_name
                || umbrella_aliases_for(skipped).contains(&test_name)
        })
    }

    pub fn setup_devnet(&self, test_name: &str) -> bool {
        self.validators_only
            .as_ref()
            .is_none_or(|setup| setup.devnet())
            && self.include_test(test_name)
    }

    pub fn setup_ephem(&self, test_name: &str) -> bool {
        self.validators_only
            .as_ref()
            .is_none_or(|setup| setup.ephem())
            && self.include_test(test_name)
    }
}

/// Map a SKIP_TESTS umbrella name to the concrete shard names it should also
/// suppress. Pre-split, `committor` referred to the entire committor suite;
/// after the split the suite became multiple shards (`committor`,
/// `committor_ix_order`, `committor_ix_multi`, `committor_bundles`,
/// `committor_bundles_heavy`, `committor_intent_executor`,
/// `committor_intent_executor_recovery`). Honor the historical name here so existing
/// automation that says `SKIP_TESTS=committor` keeps doing what it always did.
fn umbrella_aliases_for(name: &str) -> &'static [&'static str] {
    match name {
        "committor" => &[
            "committor",
            "committor_ix_order",
            "committor_ix_multi",
            "committor_bundles",
            "committor_bundles_heavy",
            "committor_intent_executor",
            "committor_intent_executor_recovery",
        ],
        _ => &[],
    }
}

impl Default for TestConfigViaEnvVars {
    fn default() -> Self {
        let validators_only =
            std::env::var("SETUP_ONLY").ok().map(|s| s.as_str().into());

        let selected_tests = std::env::var("RUN_TESTS")
            .map(|tests| {
                tests.split(',').map(|s| s.trim().to_string()).collect()
            })
            .unwrap_or_default();

        let skipped_tests = std::env::var("SKIP_TESTS")
            .map(|tests| {
                tests.split(',').map(|s| s.trim().to_string()).collect()
            })
            .unwrap_or_default();

        TestConfigViaEnvVars {
            validators_only,
            selected_tests,
            skipped_tests,
        }
    }
}
