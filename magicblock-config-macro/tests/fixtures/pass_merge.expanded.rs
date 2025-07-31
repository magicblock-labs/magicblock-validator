use magicblock_config_macro::Mergeable;
struct TestConfig {
    field1: u32,
    field2: String,
    field3: Option<String>,
    nested: NestedConfig,
}
#[automatically_derived]
impl ::core::default::Default for TestConfig {
    #[inline]
    fn default() -> TestConfig {
        TestConfig {
            field1: ::core::default::Default::default(),
            field2: ::core::default::Default::default(),
            field3: ::core::default::Default::default(),
            nested: ::core::default::Default::default(),
        }
    }
}
impl ::magicblock_config_helpers::Merge for TestConfig {
    fn merge(&mut self, other: TestConfig) {
        let default = Self::default();
        if self.field1 == default.field1 {
            self.field1 = other.field1;
        }
        if self.field2 == default.field2 {
            self.field2 = other.field2;
        }
        if self.field3 == default.field3 {
            self.field3 = other.field3;
        }
        self.nested.merge(other.nested);
    }
}
struct NestedConfig {
    value: u32,
}
#[automatically_derived]
impl ::core::default::Default for NestedConfig {
    #[inline]
    fn default() -> NestedConfig {
        NestedConfig {
            value: ::core::default::Default::default(),
        }
    }
}
impl ::magicblock_config_helpers::Merge for NestedConfig {
    fn merge(&mut self, other: NestedConfig) {
        let default = Self::default();
        if self.value == default.value {
            self.value = other.value;
        }
    }
}
fn main() {}
