use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Type};

pub struct Merger;

impl Merger {
    fn type_has_merge_method(ty: &Type) -> bool {
        match ty {
            Type::Path(type_path) => {
                let path = &type_path.path;
                let segments: Vec<String> = path
                    .segments
                    .iter()
                    .map(|seg| seg.ident.to_string())
                    .collect();

                // NOTE: this assumes that all mergeable config structs have a "Config" suffix
                segments.iter().any(|seg| seg.contains("Config"))
            }
            _ => false,
        }
    }
}

impl Merger {
    pub fn add_impl(&mut self, input: &mut DeriveInput) -> TokenStream {
        let struct_name = &input.ident;
        let generics = &input.generics;
        let (impl_generics, ty_generics, where_clause) =
            generics.split_for_impl();

        let mut compile_errors = Vec::new();

        let fields = match &input.data {
            Data::Struct(data_struct) => match &data_struct.fields {
                Fields::Named(fields_named) => &fields_named.named,
                other_fields => {
                    compile_errors.push(syn::Error::new_spanned(
                    other_fields,
                    "Merge can only be derived for structs with named fields",
                ).to_compile_error());
                    &syn::punctuated::Punctuated::new()
                }
            },
            _ => {
                compile_errors.push(
                    syn::Error::new_spanned(
                        &input,
                        "Merge can only be derived for structs",
                    )
                    .to_compile_error(),
                );
                &syn::punctuated::Punctuated::new()
            }
        };

        let merge_fields = fields.iter().map(|f| {
            let name = &f.ident;
            let field_type = &f.ty;

            if Self::type_has_merge_method(field_type) {
                // If the field type likely has a merge method, call it
                quote! {
                    self.#name.merge(other.#name);
                }
            } else {
                // Otherwise, overwrite the field if it has the default value
                quote! {
                    if self.#name == default.#name {
                        self.#name = other.#name;
                    }
                }
            }
        });

        quote! {
            #(#compile_errors)*

            impl #impl_generics ::magicblock_config_helpers::Merge for #struct_name #ty_generics #where_clause {
                fn merge(&mut self, other: #struct_name #ty_generics) {
                    let default = Self::default();

                    #(#merge_fields)*
                }
            }
        }
        .into()
    }
}
