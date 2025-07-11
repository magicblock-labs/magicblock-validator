use quote::{format_ident, quote, ToTokens};
use syn::{
    punctuated::Punctuated, visit_mut::VisitMut, Attribute, Fields, Meta, Token,
};

pub struct ClapFromSerde {
    meta_attrs: Vec<proc_macro2::TokenStream>,
    flatten: bool,
}

impl ClapFromSerde {
    pub fn new() -> Self {
        Self {
            meta_attrs: Vec::new(),
            flatten: false,
        }
    }

    pub fn reset(&mut self) {
        self.meta_attrs.clear();
        self.flatten = false;
    }
}

impl VisitMut for ClapFromSerde {
    fn visit_fields_mut(&mut self, fields: &mut syn::Fields) {
        if let Fields::Named(fields_named) = fields {
            for field in &mut fields_named.named {
                self.visit_attributes_mut(&mut field.attrs);
            }
        }
    }

    fn visit_attributes_mut(&mut self, attrs: &mut Vec<Attribute>) {
        let mut arg_index = None;
        for (i, attr) in attrs.iter_mut().enumerate() {
            if attr.path().is_ident("arg") {
                // Save the index of the arg attribute to modify it later
                arg_index = Some(i);
            } else if attr.path().is_ident("clap_from_serde_skip") {
                // Remove the attribute
                // SAFETY: attributes have not changed yet
                attrs.remove(i);
                return;
            } else if attr.path().is_ident("command") {
                // The field is a command, so we don't need to add any attributes
                self.reset();
                return;
            } else if attr.path().is_ident("serde") {
                // Look for serde attributes
                if let syn::Meta::List(meta_list) = &attr.meta {
                    // Save those attributes in a clap compatible format
                    let parsed = meta_list
                        .parse_args_with(
                            syn::punctuated::Punctuated::<
                                syn::Meta,
                                syn::Token![,],
                            >::parse_terminated,
                        )
                        .unwrap_or_default();
                    for meta in parsed.iter() {
                        if let syn::Meta::NameValue(nv) = meta {
                            // Remove surrounding quotes from the value if it's a string literal
                            let value = &nv.value;
                            let value_token = match value {
                                syn::Expr::Lit(syn::ExprLit {
                                    lit: syn::Lit::Str(lit_str),
                                    ..
                                }) => {
                                    // Parse the string literal as a Rust expression
                                    match lit_str.parse::<syn::Expr>() {
                                        Ok(expr) => quote! { #expr },
                                        Err(_) => quote! { #lit_str },
                                    }
                                }
                                _ => value.to_token_stream(),
                            };

                            if nv.path.is_ident("default") {
                                self.meta_attrs
                                    .push(quote! { default_value_t = #value_token () });
                            } else if nv.path.is_ident("deserialize_with") {
                                let clap_deserializer = format_ident!(
                                    "clap_{}",
                                    value_token.to_string()
                                );
                                self.meta_attrs
                                    .push(quote! { value_parser = #clap_deserializer });
                            }
                        } else if let syn::Meta::Path(path) = meta {
                            if path.is_ident("flatten") {
                                self.flatten = true;
                            } else if path.is_ident("default") {
                                self.meta_attrs
                                    .push(quote! { default_value_t = Default::default() });
                            }
                        }
                    }
                }
            }
        }

        // Flatten uses the command attribute
        // Don't add any other attributes
        if self.flatten {
            let clap_attr: Attribute = syn::parse_quote! {
                #[command(flatten)]
            };
            attrs.push(clap_attr);
            self.reset();
            return;
        }

        // Build the clap attributes
        let clap_attrs = self.meta_attrs.to_vec();

        // If we found an arg attribute, modify it
        if let Some(idx) = arg_index {
            let attr = &mut attrs[idx];
            if let syn::Meta::List(meta_list) = &attr.meta {
                let mut parsed = meta_list
                    .parse_args_with(
                        Punctuated::<Meta, Token![,]>::parse_terminated,
                    )
                    .unwrap_or_default();

                for attr in &clap_attrs {
                    let meta: Meta = syn::parse2(attr.clone())
                        .expect("Failed to parse TokenStream into Meta");
                    parsed.push(meta);
                }

                *attr = syn::parse_quote! { #[arg( #parsed )] };
            }
        } else {
            // If we didn't find an arg attribute, add a new one
            let clap_attr: Attribute = syn::parse_quote! {
                #[arg( #(#clap_attrs),* )]
            };
            attrs.push(clap_attr);
        }

        // Clear the state
        self.reset();
    }
}
