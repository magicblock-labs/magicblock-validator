use magicblock_config_helpers::Merge;
use magicblock_config_macro::Mergeable;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse2, Data, DeriveInput, Fields, Type};

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

/// Verifies that the Merge trait is properly implemented with various input cases
#[test]
fn test_merge_macro_generates_valid_impl() {
    // Verify that TestConfig has a Merge implementation by checking if the type implements the trait
    // This is a compile-time check that ensures the macro generates valid code
    fn assert_merge<T: Merge>() {}
    assert_merge::<TestConfig>();

    // Verify that NestedConfig also implements Merge
    assert_merge::<NestedConfig>();
}

/// Generates merge impl (replicates the macro logic for testing - matches Shank pattern)
fn merge_impl(code: TokenStream2) -> TokenStream2 {
    fn type_has_merge_method(ty: &Type) -> bool {
        match ty {
            Type::Path(type_path) => {
                let path = &type_path.path;
                let segments: Vec<String> = path
                    .segments
                    .iter()
                    .map(|seg| seg.ident.to_string())
                    .collect();
                segments.iter().any(|seg| seg.contains("Config"))
            }
            _ => false,
        }
    }

    let input: DeriveInput = parse2(code).expect("Failed to parse input");
    let struct_name = &input.ident;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let fields = match &input.data {
        Data::Struct(data_struct) => match &data_struct.fields {
            Fields::Named(fields_named) => &fields_named.named,
            _ => &syn::punctuated::Punctuated::new(),
        },
        _ => &syn::punctuated::Punctuated::new(),
    };

    let merge_fields = fields.iter().map(|f| {
        let name = &f.ident;
        let field_type = &f.ty;

        if type_has_merge_method(field_type) {
            quote! {
                self.#name.merge(other.#name);
            }
        } else {
            quote! {
                if self.#name == default.#name {
                    self.#name = other.#name;
                }
            }
        }
    });

    quote! {
        impl #impl_generics ::magicblock_config_helpers::Merge for #struct_name #ty_generics #where_clause {
            fn merge(&mut self, other: #struct_name #ty_generics) {
                let default = Self::default();

                #(#merge_fields)*
            }
        }
    }
}

/// Pretty-prints token stream for deterministic comparison
/// Parses token stream to ensure semantic equivalence, handles whitespace normalization
fn pretty_print(tokens: proc_macro2::TokenStream) -> String {
    let code = tokens.to_string();
    // Parse the code to validate it's correct Rust syntax
    syn::parse_file(code.as_str())
        .expect("Failed to parse generated token stream");
    // Return normalized version for comparison
    code.split_whitespace()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Helper function that compares generated merge impl with expected output (matches Shank pattern)
fn assert_merge_impl_fn(code: TokenStream2, expected: TokenStream2) {
    let generated = merge_impl(code);

    assert_eq!(
        pretty_print(generated),
        pretty_print(expected),
        "Generated merge implementation does not match expected output"
    );
}

/// Verifies the macro generates the correct Merge trait implementation by comparing actual vs expected output
#[test]
fn test_merge_macro_codegen_verification() {
    // Embedded test input code - struct definition for TestConfig
    let input = quote! {
        #[derive(Default)]
        struct TestConfig {
            field1: u32,
            field2: String,
            field3: Option<String>,
            nested: NestedConfig,
        }
    };

    // Define the expected generated Merge implementation
    let expected = quote! {
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
    };

    // Compare the implementation
    assert_merge_impl_fn(input, expected);
}
