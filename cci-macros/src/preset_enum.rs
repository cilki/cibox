use darling::{FromDeriveInput, FromVariant};
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Enum-level attributes for #[preset_enum(...)]
#[derive(Debug, FromDeriveInput)]
#[darling(attributes(preset_enum), supports(enum_any))]
struct PresetEnumOpts {
    ident: syn::Ident,
    data: darling::ast::Data<PresetVariantOpts, ()>,

    /// Default variant name
    #[darling(default)]
    default: Option<String>,
}

/// Variant-level attributes for #[preset_variant(...)]
#[derive(Debug, FromVariant)]
#[darling(attributes(preset_variant))]
struct PresetVariantOpts {
    ident: syn::Ident,

    /// String identifier
    id: String,

    /// Display name
    #[allow(dead_code)]
    display: String,
}

pub fn derive_preset_enum_impl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let opts = match PresetEnumOpts::from_derive_input(&input) {
        Ok(opts) => opts,
        Err(e) => return e.write_errors().into(),
    };

    let enum_name = &opts.ident;
    let variants: Vec<_> = opts.data.take_enum().unwrap();

    // Generate as_str method
    let as_str_arms = variants.iter().map(|v| {
        let variant_ident = &v.ident;
        let id = &v.id;
        quote! {
            #enum_name::#variant_ident => #id,
        }
    });

    // Generate from_str method
    let from_str_arms = variants.iter().map(|v| {
        let variant_ident = &v.ident;
        let id = &v.id;
        quote! {
            #id => Some(#enum_name::#variant_ident),
        }
    });

    // Generate all_variants method
    let all_variant_ids = variants.iter().map(|v| &v.id);

    // Generate Default implementation
    let default_impl = if let Some(ref default_name) = opts.default {
        let default_variant = variants
            .iter()
            .find(|v| v.ident == *default_name)
            .expect("Default variant not found");
        let default_ident = &default_variant.ident;
        quote! {
            impl Default for #enum_name {
                fn default() -> Self {
                    #enum_name::#default_ident
                }
            }
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        impl #enum_name {
            pub fn as_str(&self) -> &'static str {
                match self {
                    #(#as_str_arms)*
                }
            }

            pub fn from_str(s: &str) -> Option<Self> {
                match s {
                    #(#from_str_arms)*
                    _ => None,
                }
            }

            pub fn all_variants() -> Vec<&'static str> {
                vec![#(#all_variant_ids),*]
            }
        }

        #default_impl
    };

    TokenStream::from(expanded)
}
