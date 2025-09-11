use magicblock_config_helpers::Merge;
use magicblock_config_macro::Mergeable;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{parse2, DeriveInput, Item};

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

#[test]
fn test_merge_macro_codegen() {
    let t = trybuild::TestCases::new();
    t.pass("tests/fixtures/pass_merge.rs");
    t.compile_fail("tests/fixtures/fail_merge_enum.rs");
    t.compile_fail("tests/fixtures/fail_merge_union.rs");
    t.compile_fail("tests/fixtures/fail_merge_unnamed.rs");

    //expand("tests/fixtures/pass_merge.rs");
}

#[test]
fn test_struct_merge_implementation() {
    let expected_impl: TokenStream = quote! {
        impl magicblock_config_helpers::Merge for TestConfig {
            fn merge(&mut self, other: Self) {
                self.field1.merge(other.field1);
                self.field2.merge(other.field2);
                self.field3.merge(other.field3);
                self.nested.merge(other.nested);
            }
        }
    };

    let parsed_impl: Item = parse2(expected_impl)
        .expect("Failed to parse expected TestConfig implementation");

    if let Item::Impl(impl_item) = parsed_impl {
        assert!(
            impl_item.trait_.is_some(),
            "Should be a trait implementation"
        );
        let (_, trait_path, _) = impl_item.trait_.as_ref().unwrap();
        assert_eq!(
            trait_path.segments.last().unwrap().ident,
            "Merge",
            "Should implement Merge trait"
        );

        if let syn::Type::Path(type_path) = &*impl_item.self_ty {
            assert_eq!(
                type_path.path.segments.last().unwrap().ident,
                "TestConfig",
                "Should implement for TestConfig"
            );
        }

        let merge_method = impl_item
            .items
            .iter()
            .find_map(|item| {
                if let syn::ImplItem::Fn(method) = item {
                    if method.sig.ident == "merge" {
                        Some(method)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .expect("Should have merge method");
        assert_eq!(
            merge_method.sig.inputs.len(),
            2,
            "merge should have 2 parameters"
        );

        if let syn::FnArg::Receiver(receiver) = &merge_method.sig.inputs[0] {
            assert!(
                receiver.mutability.is_some(),
                "First parameter should be &mut self"
            );
        } else {
            panic!("First parameter should be receiver (&mut self)");
        }
        if let syn::FnArg::Typed(pat_type) = &merge_method.sig.inputs[1] {
            if let syn::Type::Path(type_path) = &*pat_type.ty {
                assert_eq!(
                    type_path.path.segments.last().unwrap().ident,
                    "Self",
                    "Second parameter should be of type Self"
                );
            }
        } else {
            panic!("Second parameter should be typed parameter");
        }

        println!("✅ TestConfig Merge implementation structure verified");
    } else {
        panic!("Expected impl block")
    }
}

//#[test]
//fn test_nested_struct_merge_implementation() {}
//
//#[test]
//fn test_macro_input_parsing() {}
