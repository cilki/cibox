use crate::codegen::FieldKind;
use crate::preset::{PresetFieldOpts, PresetOpts};
use proc_macro2::TokenStream;
use quote::quote;

pub fn generate_editor_preset_impl(opts: &PresetOpts, fields: &[PresetFieldOpts]) -> TokenStream {
    let preset_ident = &opts.ident;
    let preset_id = opts.preset_id();
    let preset_name = opts.display_name();
    let preset_description = opts.doc_comment();
    let preset_category = &opts.category;

    let fields_impl = generate_fields_method(fields);
    let default_config_impl = generate_default_config_method(&preset_id, fields);
    let matches_project_impl = generate_matches_project_method(&opts.match_prefix());

    quote! {
        impl crate::editor::config::EditorPreset for #preset_ident {
            fn preset_id(&self) -> &'static str {
                #preset_id
            }

            fn preset_name(&self) -> &'static str {
                #preset_name
            }

            fn preset_description(&self) -> &'static str {
                #preset_description
            }

            fn preset_category(&self) -> &'static str {
                #preset_category
            }

            #fields_impl

            #default_config_impl

            #matches_project_impl

            fn build_jobs(
                &self,
                config: &crate::editor::config::PresetConfig,
                language_version: &str,
            ) -> crate::error::Result<crate::ir::PresetJobs> {
                let preset = Self::from_config(config, language_version);
                Ok(crate::ir::PresetJobs::new(
                    #preset_id,
                    #preset_name,
                    crate::ir::ToJobs::jobs(&preset),
                ))
            }
        }
    }
}

/// Expression producing the `OptionValue` for a field's default, reading from
/// a `Self::default()` instance bound to `d`. When `detected_gate` is set,
/// bools and enum selections fall back to off/"none" unless `detected` is true.
fn default_value_expr(field: &PresetFieldOpts, detected_gate: bool) -> TokenStream {
    let field_ident = field.ident.as_ref().unwrap();

    match FieldKind::of(&field.ty) {
        FieldKind::Bool => {
            if detected_gate {
                quote! {
                    crate::editor::config::OptionValue::Bool(
                        if detected { d.#field_ident } else { false }
                    )
                }
            } else {
                quote! { crate::editor::config::OptionValue::Bool(d.#field_ident) }
            }
        }
        FieldKind::String => {
            quote! { crate::editor::config::OptionValue::String(d.#field_ident.clone()) }
        }
        FieldKind::OptionEnum(inner_ty) => {
            let selected = quote! {
                d.#field_ident.as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "none".to_string())
            };
            let selected = if detected_gate {
                quote! { if detected { #selected } else { "none".to_string() } }
            } else {
                selected
            };
            quote! {
                crate::editor::config::OptionValue::Enum {
                    selected: #selected,
                    variants: {
                        let mut v = vec!["none".to_string()];
                        v.extend(
                            <#inner_ty as strum::VariantNames>::VARIANTS
                                .iter()
                                .map(|s| s.to_string()),
                        );
                        v
                    },
                }
            }
        }
        FieldKind::Enum => {
            let field_ty = &field.ty;
            quote! {
                crate::editor::config::OptionValue::Enum {
                    selected: d.#field_ident.to_string(),
                    variants: <#field_ty as strum::VariantNames>::VARIANTS
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                }
            }
        }
        FieldKind::Other => quote! { crate::editor::config::OptionValue::Bool(false) },
    }
}

fn generate_fields_method(fields: &[PresetFieldOpts]) -> TokenStream {
    let option_metas = fields.iter().filter(|field| !field.hidden).map(|field| {
        let field_ident = field.ident.as_ref().unwrap();
        let option_id = field_ident.to_string();

        let display_name = field
            .display
            .as_ref()
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| option_id.clone());

        let description = field.doc_comment();
        let default_value = default_value_expr(field, false);

        quote! {
            crate::editor::config::OptionMeta {
                id: #option_id.to_string(),
                display_name: #display_name.to_string(),
                description: #description.to_string(),
                default_value: #default_value,
                depends_on: None,
            }
        }
    });

    quote! {
        fn fields(&self) -> Vec<crate::editor::config::OptionMeta> {
            let d = Self::default();
            vec![
                #(#option_metas),*
            ]
        }
    }
}

fn generate_default_config_method(preset_id: &str, fields: &[PresetFieldOpts]) -> TokenStream {
    let set_statements = fields.iter().filter(|field| !field.hidden).map(|field| {
        let option_id = field.ident.as_ref().unwrap().to_string();
        let default_value = default_value_expr(field, true);

        quote! {
            config.set(#option_id.to_string(), #default_value);
        }
    });

    quote! {
        fn default_config(&self, detected: bool) -> crate::editor::config::PresetConfig {
            let d = Self::default();
            let mut config = crate::editor::config::PresetConfig::new(#preset_id.to_string());
            #(#set_statements)*
            config
        }
    }
}

fn generate_matches_project_method(match_prefix: &str) -> TokenStream {
    quote! {
        fn matches_project(&self, project_type: &crate::detection::ProjectType, _working_dir: &std::path::Path) -> bool {
            project_type.to_string().starts_with(#match_prefix)
        }
    }
}
