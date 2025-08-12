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
        (self.selected_tests.is_empty()
            && !self.skipped_tests.contains(&test_name.to_string()))
            || self.selected_tests.contains(&test_name.to_string())
    }

    pub fn setup_devnet(&self, test_name: &str) -> bool {
        self.validators_only
            .as_ref()
            .map_or(true, |setup| setup.devnet())
            && self.include_test(test_name)
    }

    pub fn setup_ephem(&self, test_name: &str) -> bool {
        self.validators_only
            .as_ref()
            .map_or(true, |setup| setup.ephem())
            && self.include_test(test_name)
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
