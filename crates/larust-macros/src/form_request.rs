use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{Data, DeriveInput, Fields, Lit, Meta, Token};

/// Matches axum's own default body-size limit (`DefaultBodyLimit`, 2 MiB) -
/// every built-in axum extractor (`Bytes`, `Form`, `Json`, ...) enforces
/// this by default; a hand-rolled body read must not be looser than that
/// default or it becomes a memory-exhaustion DoS vector.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

#[derive(PartialEq, Eq)]
enum Rule {
    Required,
    Email,
    /// Laravel's `'string'` rule - a no-op here since raw form values are
    /// already strings; recognized so the doc's attribute spelling parses.
    StringNoop,
    MaxLength(usize),
    MinLength(usize),
    /// Laravel's `confirmed` rule - checks the field against a
    /// `{field}_confirmation` field (e.g. `password_confirmation`), whose
    /// name is computed once here at macro-expansion time.
    Confirmed,
}

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let struct_name = &input.ident;

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[derive(FormRequest)] does not support generic structs",
        ));
    }

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input,
            "#[derive(FormRequest)] only supports structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "#[derive(FormRequest)] requires named fields",
        ));
    };

    let mut field_checks = Vec::new();
    let mut field_inits = Vec::new();

    for field in &fields.named {
        let field_ident = field.ident.as_ref().expect("named field has an ident");
        // Strip a raw-identifier prefix (`r#type` -> `type`) since this is
        // used both as the externally-visible form field name and as the
        // basis for a generated identifier, neither of which should carry
        // Rust's own keyword-escaping syntax.
        let field_name = field_ident.to_string().trim_start_matches("r#").to_string();

        if !is_string_type(&field.ty) {
            return Err(syn::Error::new_spanned(
                &field.ty,
                "#[derive(FormRequest)] fields must be `String` (other types land in a later milestone)",
            ));
        }

        let rules = parse_rules(field)?;
        let value_ident = format_ident!("__{field_name}_value");
        let rule_checks = rules
            .iter()
            .map(|rule| rule_check(rule, &value_ident, &field_name));

        field_checks.push(quote! {
            let #value_ident: ::std::option::Option<&str> = raw.get(#field_name).map(::std::string::String::as_str);
            #(#rule_checks)*
        });

        field_inits.push(quote! {
            #field_ident: raw.remove(#field_name).unwrap_or_default(),
        });
    }

    Ok(quote! {
        #[::larust_support::axum::async_trait]
        impl<S> ::larust_support::axum::extract::FromRequest<S> for #struct_name
        where
            S: ::std::marker::Send + ::std::marker::Sync,
        {
            type Rejection = ::larust_support::validation::ValidationErrors;

            async fn from_request(
                req: ::larust_support::axum::extract::Request,
                _state: &S,
            ) -> ::std::result::Result<Self, Self::Rejection> {
                let bytes = match ::larust_support::axum::body::to_bytes(
                    req.into_body(),
                    #MAX_BODY_BYTES,
                )
                .await
                {
                    ::std::result::Result::Ok(bytes) => bytes,
                    ::std::result::Result::Err(_) => {
                        let mut errors = ::larust_support::validation::ValidationErrors::new();
                        errors.add(
                            "_request",
                            "The request body could not be read, or exceeded the size limit.",
                        );
                        return ::std::result::Result::Err(errors);
                    }
                };

                let mut raw: ::std::collections::HashMap<::std::string::String, ::std::string::String> =
                    ::larust_support::validation::form_urlencoded::parse(&bytes)
                        .into_owned()
                        .collect();

                let mut errors = ::larust_support::validation::ValidationErrors::new();
                #(#field_checks)*

                if !errors.is_empty() {
                    return ::std::result::Result::Err(errors);
                }

                ::std::result::Result::Ok(Self {
                    #(#field_inits)*
                })
            }
        }

        impl #struct_name {
            /// Returns the validated data (Laravel's `$request->validated()`).
            /// By the time a value of this type exists, extraction has
            /// already validated it - this exists for call-site parity
            /// with Laravel's `FormRequest`.
            pub fn validated(self) -> Self {
                self
            }
        }
    })
}

fn rule_check(rule: &Rule, value_ident: &syn::Ident, field_name: &str) -> TokenStream {
    match rule {
        Rule::Required => quote! {
            if let ::std::option::Option::Some(msg) = ::larust_support::validation::rules::required(#value_ident) {
                errors.add(#field_name, msg);
            }
        },
        Rule::Email => quote! {
            if let ::std::option::Option::Some(msg) = ::larust_support::validation::rules::email(#value_ident) {
                errors.add(#field_name, msg);
            }
        },
        Rule::StringNoop => quote! {},
        Rule::MaxLength(max) => quote! {
            if let ::std::option::Option::Some(msg) = ::larust_support::validation::rules::max_length(#value_ident, #max) {
                errors.add(#field_name, msg);
            }
        },
        Rule::MinLength(min) => quote! {
            if let ::std::option::Option::Some(msg) = ::larust_support::validation::rules::min_length(#value_ident, #min) {
                errors.add(#field_name, msg);
            }
        },
        Rule::Confirmed => {
            let confirmation_field = format!("{field_name}_confirmation");
            quote! {
                if let ::std::option::Option::Some(msg) = ::larust_support::validation::rules::confirmed(
                    #value_ident,
                    raw.get(#confirmation_field).map(::std::string::String::as_str),
                ) {
                    errors.add(#field_name, msg);
                }
            }
        }
    }
}

fn is_string_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "String"))
}

/// Parses every `#[validate(...)]` attribute on a field (there can be more
/// than one - Rust allows repeating an attribute, and silently honoring
/// only the first would drop rules with no warning) into a deduplicated
/// list of [`Rule`]s, preserving first-seen order. A field with no
/// `#[validate(...)]` attribute has no rules - it's still extracted, just
/// unchecked.
fn parse_rules(field: &syn::Field) -> syn::Result<Vec<Rule>> {
    let mut rules = Vec::new();

    for attr in field.attrs.iter().filter(|a| a.path().is_ident("validate")) {
        let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
        for meta in &metas {
            for rule in parse_rule(meta)? {
                if !rules.contains(&rule) {
                    rules.push(rule);
                }
            }
        }
    }

    Ok(rules)
}

fn parse_rule(meta: &Meta) -> syn::Result<Vec<Rule>> {
    match meta {
        Meta::Path(path) if path.is_ident("required") => Ok(vec![Rule::Required]),
        Meta::Path(path) if path.is_ident("email") => Ok(vec![Rule::Email]),
        Meta::Path(path) if path.is_ident("string") => Ok(vec![Rule::StringNoop]),
        Meta::Path(path) if path.is_ident("confirmed") => Ok(vec![Rule::Confirmed]),
        Meta::List(list) if list.path.is_ident("length") => parse_length(list),
        Meta::List(list) if list.path.is_ident("unique") => Err(syn::Error::new_spanned(
            list,
            "unique(...) requires database access and isn't implemented until M4",
        )),
        _ => Err(syn::Error::new_spanned(
            meta,
            "unrecognized validation rule (expected one of: required, email, string, confirmed, length(max = N), length(min = N))",
        )),
    }
}

/// `length(max = N)`, `length(min = N)`, or both in one call.
fn parse_length(list: &syn::MetaList) -> syn::Result<Vec<Rule>> {
    let pairs =
        list.parse_args_with(Punctuated::<syn::MetaNameValue, Token![,]>::parse_terminated)?;

    let mut rules = Vec::new();
    for pair in &pairs {
        let n = expect_usize_literal(&pair.value)?;
        if pair.path.is_ident("max") {
            rules.push(Rule::MaxLength(n));
        } else if pair.path.is_ident("min") {
            rules.push(Rule::MinLength(n));
        } else {
            return Err(syn::Error::new_spanned(
                &pair.path,
                "length(...) only accepts `max` and `min`",
            ));
        }
    }

    if rules.is_empty() {
        return Err(syn::Error::new_spanned(
            list,
            "length(...) requires `max = N` or `min = N`",
        ));
    }

    Ok(rules)
}

fn expect_usize_literal(expr: &syn::Expr) -> syn::Result<usize> {
    if let syn::Expr::Lit(syn::ExprLit {
        lit: Lit::Int(int), ..
    }) = expr
    {
        return int.base10_parse::<usize>();
    }
    Err(syn::Error::new_spanned(expr, "expected an integer literal"))
}
