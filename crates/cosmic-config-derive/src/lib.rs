//! Derives `CosmicConfigEntry` for a struct of named fields.
//!
//! Each field becomes one key named after it, read and written through
//! `cosmic_config::ConfigGet` and `cosmic_config::ConfigSet`. A missing key is
//! not an error: the field keeps its `Default` value, and only errors that are
//! real problems are collected for the caller.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, Data, DeriveInput, Expr, ExprLit, Field, Fields, Lit, Meta, Token};

#[proc_macro_derive(CosmicConfigEntry, attributes(version))]
pub fn cosmic_config_entry(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let version = version(input)?;
    let name = &input.ident;
    let fields: Vec<(&syn::Ident, &syn::Type)> = named_fields(input)?
        .iter()
        .map(|field| {
            let field_name = field.ident.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(field, "CosmicConfigEntry needs named fields")
            })?;
            Ok((field_name, &field.ty))
        })
        .collect::<syn::Result<_>>()?;

    let writes = fields.iter().map(|(field_name, _)| {
        quote! {
            cosmic_config::ConfigSet::set(&tx, stringify!(#field_name), &self.#field_name)?;
        }
    });

    let reads = fields.iter().map(|(field_name, field_type)| {
        quote! {
            match cosmic_config::ConfigGet::get::<#field_type>(
                config,
                stringify!(#field_name),
            ) {
                Ok(value) => default.#field_name = value,
                Err(why) => if why.is_err() {
                    errors.push(why);
                },
            }
        }
    });

    let updates = fields.iter().map(|(field_name, field_type)| {
        quote! {
            stringify!(#field_name) => {
                match cosmic_config::ConfigGet::get::<#field_type>(
                    config,
                    stringify!(#field_name),
                ) {
                    Ok(value) => {
                        if self.#field_name != value {
                            keys.push(stringify!(#field_name));
                        }
                        self.#field_name = value;
                    }
                    Err(why) => errors.push(why),
                }
            }
        }
    });

    let setters = fields.iter().map(|(field_name, field_type)| {
        let setter = format_ident!("set_{}", field_name);
        quote! {
            pub fn #setter(
                &mut self,
                config: &cosmic_config::Config,
                value: #field_type,
            ) -> Result<bool, cosmic_config::Error> {
                if self.#field_name == value {
                    return Ok(false);
                }
                self.#field_name = value;
                cosmic_config::ConfigSet::set(
                    config,
                    stringify!(#field_name),
                    &self.#field_name,
                )?;
                Ok(true)
            }
        }
    });

    Ok(quote! {
        impl cosmic_config::CosmicConfigEntry for #name {
            const VERSION: u64 = #version;

            fn write_entry(
                &self,
                config: &cosmic_config::Config,
            ) -> Result<(), cosmic_config::Error> {
                let tx = config.transaction();
                #(#writes)*
                tx.commit()
            }

            fn get_entry(
                config: &cosmic_config::Config,
            ) -> Result<Self, (Vec<cosmic_config::Error>, Self)> {
                let mut default = Self::default();
                let mut errors = Vec::new();
                #(#reads)*
                if errors.is_empty() {
                    Ok(default)
                } else {
                    Err((errors, default))
                }
            }

            fn update_keys<T: AsRef<str>>(
                &mut self,
                config: &cosmic_config::Config,
                changed_keys: &[T],
            ) -> (Vec<cosmic_config::Error>, Vec<&'static str>) {
                let mut keys = Vec::with_capacity(changed_keys.len());
                let mut errors = Vec::new();
                for key in changed_keys {
                    match key.as_ref() {
                        #(#updates)*
                        _ => (),
                    }
                }
                (errors, keys)
            }
        }

        impl #name {
            #(#setters)*
        }
    })
}

/// The `#[version = N]` attribute, defaulting to 0 like the upstream derive.
fn version(input: &DeriveInput) -> syn::Result<u64> {
    for attr in &input.attrs {
        if !attr.path().is_ident("version") {
            continue;
        }
        let Meta::NameValue(name_value) = &attr.meta else {
            continue;
        };
        let Expr::Lit(ExprLit {
            lit: Lit::Int(int), ..
        }) = &name_value.value
        else {
            continue;
        };
        return int.base10_parse();
    }
    Ok(0)
}

fn named_fields(input: &DeriveInput) -> syn::Result<&Punctuated<Field, Token![,]>> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "CosmicConfigEntry can only be derived for a struct",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "CosmicConfigEntry needs named fields",
        ));
    };
    Ok(&named.named)
}
