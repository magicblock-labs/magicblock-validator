use magicblock_config_helpers::Merge;
use magicblock_config_macro::Mergeable;

// Test struct with fields that have merge methods
#[derive(Debug, Clone, PartialEq, Eq, Default, Mergeable)]
struct TestConfig {
    field1: u32,
    field2: String,
    field3: Option<String>,
    nested: NestedConfig,
}

// Nested config that has a merge method
#[derive(Debug, Clone, PartialEq, Eq, Default, Mergeable)]
struct NestedConfig {
    value: u32,
}

#[test]
fn test_merge_macro() {
    let mut config = TestConfig {
        field1: 0,
        field2: "".to_string(),
        field3: None,
        nested: NestedConfig { value: 0 },
    };

    let other = TestConfig {
        field1: 42,
        field2: "test".to_string(),
        field3: Some("test".to_string()),
        nested: NestedConfig { value: 100 },
    };

    config.merge(other);

    // field1 and field2 should use default-based merging
    assert_eq!(config.field1, 42);
    assert_eq!(config.field2, "test");

    // nested should use its own merge method
    assert_eq!(config.nested.value, 100);
}

#[test]
fn test_merge_macro_with_non_default_values() {
    let mut config = TestConfig {
        field1: 10,
        field2: "original".to_string(),
        field3: None,
        nested: NestedConfig { value: 50 },
    };

    let other = TestConfig {
        field1: 42,
        field2: "test".to_string(),
        field3: Some("test".to_string()),
        nested: NestedConfig { value: 100 },
    };

    config.merge(other);

    // field1 and field2 should preserve original values since they're not default
    assert_eq!(config.field1, 10);
    assert_eq!(config.field2, "original");
    assert_eq!(config.field3, Some("test".to_string()));

    // nested should use its own merge method (preserves original since both are non-default)
    assert_eq!(config.nested.value, 50);
}
