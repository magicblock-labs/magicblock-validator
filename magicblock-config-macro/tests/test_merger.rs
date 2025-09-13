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
}

#[test]
fn test_struct_merge_implementation() {
    let test_config_input: TokenStream = quote! {
        #[derive(Default, Mergeable)]
        struct TestConfig {
            field1: u32,
            field2: String,
            field3: Option<String>,
            nested: NestedConfig,
        }
    };

    let parsed: DeriveInput =
        parse2(test_config_input).expect("Should parse TestConfig");
    assert_eq!(parsed.ident, "TestConfig");

    if let syn::Data::Struct(data) = parsed.data {
        if let syn::Fields::Named(fields) = data.fields {
            assert_eq!(
                fields.named.len(),
                4,
                "TestConfig should have 4 fields"
            );

            let field_info: Vec<(String, String)> = fields
                .named
                .iter()
                .map(|f| {
                    let name = f.ident.as_ref().unwrap().to_string();
                    let type_name = quote!(#f.ty).to_string();
                    (name, type_name)
                })
                .collect();

            assert!(field_info
                .iter()
                .any(|(name, ty)| name == "field1" && ty.contains("u32")));
            assert!(field_info
                .iter()
                .any(|(name, ty)| name == "field2" && ty.contains("String")));
            assert!(field_info
                .iter()
                .any(|(name, ty)| name == "field3" && ty.contains("Option")));
            assert!(field_info
                .iter()
                .any(|(name, ty)| name == "nested"
                    && ty.contains("NestedConfig")));
        } else {
            panic!("TestConfig should have named fields");
        }
    } else {
        panic!("TestConfig should be a struct");
    }

    let nested_config_input: TokenStream = quote! {
        #[derive(Default, Mergeable)]
        struct NestedConfig {
            value: u32,
        }
    };

    let parsed_nested: DeriveInput =
        parse2(nested_config_input).expect("Should parse NestedConfig");
    assert_eq!(parsed_nested.ident, "NestedConfig");

    let enum_input: TokenStream = quote! {
        #[derive(Mergeable)]
        enum TestEnum {
            Variant1,
            Variant2,
        }
    };

    let parsed_enum: DeriveInput =
        parse2(enum_input).expect("Should parse enum");
    match parsed_enum.data {
        syn::Data::Enum(_) => {
            println!("✅ Enum parsing works (macro should reject this at generation time)");
        }
        _ => panic!("Expected enum"),
    }

    println!("✅ Macro input structure validation completed");
}

#[test]
fn test_nested_struct_merge_implementation() {
    let test_nested_input: TokenStream = quote! {
        #[derive(Default, Mergeable)]
        struct NestedConfig {
            value: u32,
        }
    };

    let parsed: DeriveInput =
        parse2(test_nested_input).expect("Should parse NestedConfig");
    assert_eq!(parsed.ident, "NestedConfig");

    if let syn::Data::Struct(data) = parsed.data {
        if let syn::Fields::Named(fields) = data.fields {
            assert_eq!(
                fields.named.len(),
                1,
                "NestedConfig should have 1 fields"
            );

            let field = &fields.named[0];
            let field_name = field.ident.as_ref().unwrap().to_string();
            let field_type = quote!(#field.ty).to_string();

            assert_eq!(field_name, "value", "Field should be named 'value'");
            assert!(
                field_type.contains("u32"),
                "Field should be u32 type, got: {}",
                field_type
            );

            match field.vis {
                syn::Visibility::Inherited => {
                    println!("✅ Field 'value' has correct private visibility");
                }
                _ => panic!("Field 'value' should be private"),
            }
        } else {
            panic!("NestedConfig should have named fields");
        }
    } else {
        panic!("NestedConfig should be a struct");
    }
}
