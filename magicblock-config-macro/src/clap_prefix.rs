use convert_case::{Case, Casing};
use proc_macro::TokenStream;
use quote::{quote, ToTokens};
use syn::{
    parse_quote, punctuated::Punctuated, visit_mut::VisitMut, Attribute, Expr,
    Fields, LitStr, Meta, MetaNameValue, Path, Token,
};

pub struct ClapPrefix {
    pub attr: TokenStream,
    pub compile_errors: Vec<proc_macro2::TokenStream>,
}

impl ClapPrefix {
    pub fn new(attr: TokenStream) -> Self {
        Self {
            attr,
            compile_errors: Vec::new(),
        }
    }
}

impl VisitMut for ClapPrefix {
    fn visit_fields_mut(&mut self, fields: &mut syn::Fields) {
        if let Fields::Named(fields_named) = fields {
            for field in &mut fields_named.named {
                if let Some(ident) = &field.ident {
                    let attr_str =
                        self.attr.to_string().trim_matches('"').to_string();

                    let kebab = format!(
                        "{}-{}",
                        attr_str.to_case(Case::Kebab),
                        ident.to_string().to_case(Case::Kebab)
                    );
                    let kebab_str = LitStr::new(&kebab, ident.span());
                    let constant_str = LitStr::new(
                        &kebab.to_case(Case::Constant),
                        ident.span(),
                    );

                    let mut replacer = MetaReplacer::new(
                        ident.to_string(),
                        kebab_str,
                        constant_str,
                        vec!["long".to_string(), "name".to_string()],
                    );
                    replacer.visit_attributes_mut(&mut field.attrs);
                    self.compile_errors.extend(replacer.compile_errors);
                }
            }
        }
    }
}

struct MetaReplacer {
    ident: String,
    kebab: LitStr,
    constant: LitStr,
    replacements: Vec<String>,
    compile_errors: Vec<proc_macro2::TokenStream>,
}

impl MetaReplacer {
    fn new(
        ident: String,
        kebab: LitStr,
        constant: LitStr,
        replacements: Vec<String>,
    ) -> Self {
        Self {
            ident,
            kebab,
            constant,
            replacements,
            compile_errors: Vec::new(),
        }
    }
}

impl VisitMut for MetaReplacer {
    fn visit_attributes_mut(&mut self, attrs: &mut Vec<Attribute>) {
        let ident = self.ident.to_token_stream();
        let kebab = self.kebab.to_token_stream();
        let constant = self.constant.to_token_stream();

        let mut is_flattened = false;
        let mut add_env = false;
        let mut clap_attr_index = None;
        let mut derive_env_var_index = None;

        // Locate the arg attribute to replace or any flatten attributes
        for (i, attr) in attrs.iter().enumerate() {
            if attr.path().is_ident("command") {
                // Flattened attributes are skipped
                is_flattened = true;
                break;
            } else if attr.path().is_ident("serde") {
                // Flattened attributes are skipped
                if let syn::Meta::List(list) = &attr.meta {
                    let parsed = list
                        .parse_args_with(
                            syn::punctuated::Punctuated::<
                                syn::Meta,
                                syn::Token![,],
                            >::parse_terminated,
                        )
                        .unwrap_or_default();

                    if parsed.to_token_stream().to_string().contains("flatten")
                    {
                        is_flattened = true;
                        break;
                    }
                }
            } else if attr.path().is_ident("derive_env_var") {
                derive_env_var_index = Some(i);
                add_env = true;
                continue;
            } else if attr.path().is_ident("arg") && clap_attr_index.is_none() {
                clap_attr_index = Some(i);
                break;
            } else if attr.path().is_ident("arg") && clap_attr_index.is_some() {
                self.compile_errors.push(quote! { compile_error!("Multiple #[arg] attributes are not supported for field '{}'.", #ident); });
                return;
            }
        }

        // Flattened fields are not prefixed
        if is_flattened {
            return;
        }

        let mut new_meta_list = Punctuated::<Meta, Token![,]>::new();
        for key in self.replacements.clone() {
            let key_ident =
                syn::Ident::new(&key, proc_macro2::Span::call_site());
            let path: Path = parse_quote! { #key_ident };
            let lit: Expr = syn::parse2(kebab.clone())
                .expect("Failed to parse value as Lit");
            new_meta_list.push(Meta::NameValue(MetaNameValue {
                path,
                eq_token: Default::default(),
                value: lit,
            }));
        }

        if add_env {
            let path: Path = parse_quote! { env };
            let lit: Expr = syn::parse2(constant.clone())
                .expect("Failed to parse value as Lit");
            new_meta_list.push(Meta::NameValue(MetaNameValue {
                path,
                eq_token: Default::default(),
                value: lit,
            }));
        }

        if let Some(idx) = clap_attr_index {
            // The arg attribute exists, extend it
            let attr = &mut attrs[idx];

            // Parse the list of attributes
            if let syn::Meta::List(meta_list) = &attr.meta {
                let mut parsed = meta_list
                    .parse_args_with(
                        Punctuated::<Meta, Token![,]>::parse_terminated,
                    )
                    .unwrap_or_default();
                parsed = parsed.iter().cloned().collect::<Punctuated<_, _>>();

                // Check if the field already has the attribute
                for meta in parsed.iter() {
                    for key in self.replacements.clone() {
                        if let syn::Meta::NameValue(nv) = meta {
                            if nv.path.is_ident(&key) {
                                let msg = format!(
                                "Field '{}' already has a #[arg({} = ...)] attribute, which is not allowed by #[clap_prefix].",
                                ident, key
                            );
                                let err = quote! { compile_error!(#msg); };
                                self.compile_errors.push(err);
                            }
                        }
                    }
                }

                if !self.compile_errors.is_empty() {
                    return;
                }

                parsed.extend(new_meta_list.iter().cloned());
                *attr = syn::parse_quote! { #[arg( #parsed )] };
            }
        } else if !new_meta_list.is_empty() {
            // The arg attribute is not present, so we add it
            let arg_attr: Attribute = syn::parse_quote! {
                #[arg( #new_meta_list )]
            };
            attrs.push(arg_attr);
        }

        // Remove the derive_env_var attribute if it exists
        if let Some(idx) = derive_env_var_index {
            attrs.remove(idx);
        }
    }
}
