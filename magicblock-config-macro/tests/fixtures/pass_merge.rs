use magicblock_config_macro::Mergeable;

#[derive(Default, Mergeable)]
struct TestConfig {
    field1: u32,
    field2: String,
    field3: Option<String>,
    nested: NestedConfig,
}

#[derive(Default, Mergeable)]
struct NestedConfig {
    value: u32,
}

fn main() {}
