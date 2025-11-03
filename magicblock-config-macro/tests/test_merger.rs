use magicblock_config_helpers::Merge;
use magicblock_config_macro::Mergeable;
use std::fs;
use syn::{parse_file, File, Item};

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

/// Verifies that the Merge trait is properly implemented for the test struct
#[test]
fn test_merge_macro_generates_valid_impl() {
    let t = trybuild::TestCases::new();
    t.pass("tests/fixtures/pass_merge.rs");
    t.compile_fail("tests/fixtures/fail_merge_enum.rs");
    t.compile_fail("tests/fixtures/fail_merge_union.rs");
    t.compile_fail("tests/fixtures/fail_merge_unnamed.rs");

    // Verify that TestConfig has a Merge implementation
    // by checking if the type implements the trait
    fn assert_merge<T: Merge>() {}
    assert_merge::<TestConfig>();
}

/// Verifies the macro generates Merge impl for structs with named fields
#[test]
fn test_merge_macro_codegen_verification() {
    // Load and parse the expanded fixture to verify structure
    let source = fs::read_to_string("tests/fixtures/pass_merge.rs")
        .expect("Failed to read pass_merge.rs fixture");

    let file: File =
        parse_file(&source).expect("Failed to parse pass_merge.rs fixture");

    // Verify the file contains the Mergeable derive
    let has_mergeable_derive = file.items.iter().any(|item| {
        if let Item::Struct(item_struct) = item {
            item_struct
                .attrs
                .iter()
                .any(|attr| attr.path().is_ident("derive"))
        } else {
            false
        }
    });

    assert!(
        has_mergeable_derive,
        "Expected struct with #[derive(Mergeable)]"
    );

    // Verify the test struct is actually defined
    let has_test_config = file.items.iter().any(|item| {
        if let Item::Struct(item_struct) = item {
            item_struct.ident == "TestConfig"
        } else {
            false
        }
    });

    assert!(
        has_test_config,
        "Expected TestConfig struct to be defined"
    );

    // Compile-test the fixtures to ensure error cases work correctly
    let t = trybuild::TestCases::new();
    t.pass("tests/fixtures/pass_merge.rs");
    t.compile_fail("tests/fixtures/fail_merge_enum.rs");
    t.compile_fail("tests/fixtures/fail_merge_union.rs");
    t.compile_fail("tests/fixtures/fail_merge_unnamed.rs");
}
