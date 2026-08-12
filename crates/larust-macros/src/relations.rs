//! `#[has_many(...)]`/`#[has_one(...)]`/`#[belongs_to(...)]` — struct-level
//! attributes recognized by `#[derive(Model)]` (see `model.rs`), generating
//! Laravel-style relationship accessor methods, both a lazy per-instance
//! form (`user.posts().await?`) and a batch/eager form (`User::load_posts(
//! &users).await?`, returning a `HashMap` keyed by each input row's id —
//! Rust has no dynamic-property mechanism to attach loaded relations back
//! onto a struct the way Laravel's `->with(...)` does, so the batch form is
//! a lookup map the caller indexes into explicitly instead). Every
//! generated method is a thin delegation to machinery `#[derive(Model)]`
//! already generates (`Self::find`, `QueryBuilder::where_eq`/`where_in`/
//! `get`/`first`) — no new ORM surface beyond `where_in` itself was needed
//! for this. See `docs/MACROS.md` for the full grammar and generated
//! shapes.

use crate::model::{field_name_str, is_i64_type, to_snake_case};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{DeriveInput, Meta, Token};

#[derive(Debug)]
struct RelationSpec {
    related: syn::Path,
    foreign_key: String,
    /// Overrides the default (related-type-derived) method name — needed
    /// when a struct has more than one relationship to the same related
    /// type (e.g. `Post`'s `author`/`editor`, both `belongs_to(User, ...)`,
    /// which would otherwise both default to a method named `user` and
    /// collide). Already validated as a legal identifier at parse time
    /// (see `parse_ident`), so codegen can splice it directly.
    method_name: Option<syn::Ident>,
    /// `belongs_to`-only: the *related* struct's primary key field name,
    /// needed by `load_*`'s batch form to group fetched related rows by
    /// their own id (something this macro invocation has no visibility
    /// into — it only sees the struct it's expanding on, not the related
    /// one). Defaults to `"id"`, the primary key field name every model in
    /// this codebase uses so far; `related_key = "..."` overrides it.
    /// Rejected as an unrecognized argument on `has_many`/`has_one`, where
    /// it has no meaning (their batch loader groups by `foreign_key`,
    /// which is already known). Kept as a plain `String` (the clean name,
    /// never `r#`-prefixed) here, same as `foreign_key` — codegen parses it
    /// into a `syn::Ident` via `parse_ident` only where it's actually
    /// needed as a field-access expression, since that parsing can add an
    /// `r#` prefix for a keyword-shaped name (`"type"` -> `r#type`) that
    /// must *not* leak into the SQL column-name string, which stays the
    /// clean form.
    related_key: Option<String>,
}

/// Parses every `#[has_many(...)]`/`#[has_one(...)]`/`#[belongs_to(...)]`
/// attribute on `input` and generates one `impl #struct_name { ... }` block
/// with a lazy instance method plus a batch `load_*` method per
/// relationship (empty output if none are declared). `all_fields` is every
/// field on the struct (not just insertable ones) — needed to validate a
/// `belongs_to` foreign key names a real `i64` field on *this* struct;
/// `pk_ident` is the struct's `#[primary_key]` field, used as the "this
/// row's id" side of `has_one`/`has_many` queries (both instance and batch
/// forms).
pub fn expand(
    input: &DeriveInput,
    all_fields: &[(&syn::Ident, &syn::Type)],
    pk_ident: &syn::Ident,
) -> syn::Result<TokenStream> {
    let struct_name = &input.ident;

    let mut methods = Vec::new();

    for spec in parse_relation_attrs(input, "belongs_to", true)? {
        let Some((field_ident, field_ty)) = find_field(all_fields, &spec.foreign_key) else {
            return Err(syn::Error::new_spanned(
                &input.ident,
                format!(
                    "#[belongs_to({}, foreign_key = \"{}\")]: no field named `{}` on this struct",
                    related_type_name(&spec.related),
                    spec.foreign_key,
                    spec.foreign_key
                ),
            ));
        };
        if !is_i64_type(field_ty) {
            return Err(syn::Error::new_spanned(
                field_ty,
                format!(
                    "#[belongs_to(...)] foreign_key field `{}` must be `i64`",
                    spec.foreign_key
                ),
            ));
        }

        let method_name =
            resolve_method_name(&spec, || to_snake_case(&related_type_name(&spec.related)));
        let load_method_name = format_ident!("load_{method_name}");
        // `related_key_column`: the clean SQL column name (never `r#`-
        // prefixed — spliced directly into `where_in`). `related_key`: the
        // same name parsed into a real field-access identifier (which
        // *does* get `r#`-prefixed for a keyword-shaped name like `type`)
        // — these must come from separately-typed values, not one
        // `Ident::to_string()`, since a raw identifier's `to_string()`
        // includes the `r#` prefix, which would silently corrupt the SQL
        // column reference (see GOTCHAS.md).
        let related_key_column = spec.related_key.clone().unwrap_or_else(|| "id".to_string());
        let related_key = parse_ident(&related_key_column, "related_key", &input.ident)?;
        let related = &spec.related;
        methods.push(quote! {
            pub async fn #method_name(&self) -> ::std::result::Result<::std::option::Option<#related>, ::larust_support::AppError> {
                #related::find(self.#field_ident).await
            }

            /// Batch-loads the related row for every row in `rows` in one
            /// query (Laravel's `::with(...)` eager loading, adapted — see
            /// this module's doc comment for why this returns a lookup map
            /// instead of attaching results back onto `rows`).
            pub async fn #load_method_name(
                rows: &[Self],
            ) -> ::std::result::Result<
                ::std::collections::HashMap<i64, #related>,
                ::larust_support::AppError,
            > {
                // Deduplicated before querying — several input rows sharing
                // the same related id (e.g. many posts by one author) would
                // otherwise send that id to `where_in` once per row instead
                // of once, total.
                let ids: ::std::vec::Vec<i64> = rows
                    .iter()
                    .map(|row| row.#field_ident)
                    .collect::<::std::collections::HashSet<i64>>()
                    .into_iter()
                    .collect();
                let related = #related::query().where_in(#related_key_column, ids).get().await?;
                ::std::result::Result::Ok(
                    related
                        .into_iter()
                        .map(|item| (item.#related_key, item))
                        .collect(),
                )
            }
        });
    }

    for spec in parse_relation_attrs(input, "has_one", false)? {
        let method_name =
            resolve_method_name(&spec, || to_snake_case(&related_type_name(&spec.related)));
        let load_method_name = format_ident!("load_{method_name}");
        let foreign_key_ident = parse_ident(&spec.foreign_key, "foreign_key", &input.ident)?;
        let related = &spec.related;
        let foreign_key = &spec.foreign_key;
        methods.push(quote! {
            pub async fn #method_name(&self) -> ::std::result::Result<::std::option::Option<#related>, ::larust_support::AppError> {
                #related::query().where_eq(#foreign_key, self.#pk_ident).first().await
            }

            /// Batch-loads the related row for every row in `rows` in one
            /// query instead of one query per row — see the sibling
            /// instance method (above) and this module's doc comment.
            pub async fn #load_method_name(
                rows: &[Self],
            ) -> ::std::result::Result<
                ::std::collections::HashMap<i64, #related>,
                ::larust_support::AppError,
            > {
                // Deduplicated before querying — see the belongs_to batch
                // loader's comment above.
                let ids: ::std::vec::Vec<i64> = rows
                    .iter()
                    .map(|row| row.#pk_ident)
                    .collect::<::std::collections::HashSet<i64>>()
                    .into_iter()
                    .collect();
                let related = #related::query().where_in(#foreign_key, ids).get().await?;
                let mut grouped: ::std::collections::HashMap<i64, #related> =
                    ::std::collections::HashMap::new();
                for item in related {
                    grouped.entry(item.#foreign_key_ident).or_insert(item);
                }
                ::std::result::Result::Ok(grouped)
            }
        });
    }

    for spec in parse_relation_attrs(input, "has_many", false)? {
        let method_name = resolve_method_name(&spec, || {
            pluralize(&to_snake_case(&related_type_name(&spec.related)))
        });
        let load_method_name = format_ident!("load_{method_name}");
        let foreign_key_ident = parse_ident(&spec.foreign_key, "foreign_key", &input.ident)?;
        let related = &spec.related;
        let foreign_key = &spec.foreign_key;
        methods.push(quote! {
            pub async fn #method_name(&self) -> ::std::result::Result<::std::vec::Vec<#related>, ::larust_support::AppError> {
                #related::query().where_eq(#foreign_key, self.#pk_ident).get().await
            }

            /// Batch-loads the related rows for every row in `rows` in one
            /// query instead of one query per row — see the sibling
            /// instance method (above) and this module's doc comment.
            pub async fn #load_method_name(
                rows: &[Self],
            ) -> ::std::result::Result<
                ::std::collections::HashMap<i64, ::std::vec::Vec<#related>>,
                ::larust_support::AppError,
            > {
                // Deduplicated before querying — see the belongs_to batch
                // loader's comment above.
                let ids: ::std::vec::Vec<i64> = rows
                    .iter()
                    .map(|row| row.#pk_ident)
                    .collect::<::std::collections::HashSet<i64>>()
                    .into_iter()
                    .collect();
                let related = #related::query().where_in(#foreign_key, ids).get().await?;
                let mut grouped: ::std::collections::HashMap<i64, ::std::vec::Vec<#related>> =
                    ::std::collections::HashMap::new();
                for item in related {
                    grouped.entry(item.#foreign_key_ident).or_default().push(item);
                }
                ::std::result::Result::Ok(grouped)
            }
        });
    }

    if methods.is_empty() {
        return Ok(quote! {});
    }

    Ok(quote! {
        impl #struct_name {
            #(#methods)*
        }
    })
}

/// `spec.method_name` if given (an override), otherwise `default()`
/// (typically a `to_snake_case`/`pluralize` derivation from the related
/// type) parsed into an identifier — factored out since all three
/// relationship kinds resolve their method name the same way, differing
/// only in what `default` computes.
fn resolve_method_name(spec: &RelationSpec, default: impl FnOnce() -> String) -> syn::Ident {
    match &spec.method_name {
        Some(ident) => ident.clone(),
        None => format_ident!("{}", default()),
    }
}

fn find_field<'a>(
    fields: &'a [(&'a syn::Ident, &'a syn::Type)],
    name: &str,
) -> Option<(&'a syn::Ident, &'a syn::Type)> {
    fields
        .iter()
        .find(|(ident, _)| field_name_str(ident) == name)
        .copied()
}

fn related_type_name(path: &syn::Path) -> String {
    path.segments
        .last()
        .expect("a relationship's related type is a non-empty path")
        .ident
        .to_string()
}

/// Parses every occurrence of `#[#attr_name(RelatedType, foreign_key =
/// "...", method = "...")]` (plus `related_key = "..."` when
/// `allow_related_key` is set) on `input` (relationships are repeatable —
/// a struct can have more than one `has_many`, etc.). `method`/
/// `related_key` are optional; `foreign_key = "..."` is required —
/// deliberately not guessed from naming convention the way Laravel does,
/// matching this macro's existing `#[route_key("...")]` precedent of an
/// explicit, unambiguous string over inferred magic that could silently
/// guess wrong.
fn parse_relation_attrs(
    input: &DeriveInput,
    attr_name: &str,
    allow_related_key: bool,
) -> syn::Result<Vec<RelationSpec>> {
    let mut specs = Vec::new();

    for attr in input.attrs.iter().filter(|a| a.path().is_ident(attr_name)) {
        let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;

        let mut related: Option<syn::Path> = None;
        let mut foreign_key: Option<String> = None;
        let mut method_name: Option<syn::Ident> = None;
        let mut related_key: Option<String> = None;

        for meta in &metas {
            match meta {
                Meta::Path(path) if related.is_none() => related = Some(path.clone()),
                Meta::Path(path) => {
                    return Err(syn::Error::new_spanned(
                        path,
                        "a related model type was already given for this attribute",
                    ));
                }
                Meta::NameValue(nv) if nv.path.is_ident("foreign_key") => {
                    if foreign_key.is_some() {
                        return Err(syn::Error::new_spanned(
                            nv,
                            "`foreign_key` was already given for this attribute",
                        ));
                    }
                    foreign_key = Some(expect_str_literal(&nv.value)?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("method") => {
                    if method_name.is_some() {
                        return Err(syn::Error::new_spanned(
                            nv,
                            "`method` was already given for this attribute",
                        ));
                    }
                    let value = expect_str_literal(&nv.value)?;
                    method_name = Some(parse_ident(&value, "method", &nv.value)?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("related_key") => {
                    if !allow_related_key {
                        return Err(syn::Error::new_spanned(
                            nv,
                            format!(
                                "`related_key` is only meaningful on #[belongs_to(...)] \
                                 (#[{attr_name}(...)] already knows its own related-row \
                                 lookup key via `foreign_key`)"
                            ),
                        ));
                    }
                    if related_key.is_some() {
                        return Err(syn::Error::new_spanned(
                            nv,
                            "`related_key` was already given for this attribute",
                        ));
                    }
                    related_key = Some(expect_str_literal(&nv.value)?);
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        meta,
                        format!(
                            "unrecognized #[{attr_name}(...)] argument (expected a related \
                             model type, `foreign_key = \"...\"`, `method = \"...\"`{})",
                            if allow_related_key {
                                ", or `related_key = \"...\"`"
                            } else {
                                ""
                            }
                        ),
                    ));
                }
            }
        }

        let related = related.ok_or_else(|| {
            syn::Error::new_spanned(
                attr,
                format!(
                    "#[{attr_name}(...)] requires a related model type, e.g. \
                     #[{attr_name}(Post, foreign_key = \"user_id\")]"
                ),
            )
        })?;
        let foreign_key = foreign_key.ok_or_else(|| {
            syn::Error::new_spanned(
                attr,
                format!("#[{attr_name}(...)] requires foreign_key = \"...\""),
            )
        })?;

        specs.push(RelationSpec {
            related,
            foreign_key,
            method_name,
            related_key,
        });
    }

    Ok(specs)
}

fn expect_str_literal(expr: &syn::Expr) -> syn::Result<String> {
    if let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(s),
        ..
    }) = expr
    {
        Ok(s.value())
    } else {
        Err(syn::Error::new_spanned(expr, "expected a string literal"))
    }
}

/// Validates `value` is usable as a Rust identifier, returning the
/// concrete `syn::Ident` to splice into codegen — *before* it's ever
/// handed to `format_ident!`, which **panics** (not a `Result`) on an
/// illegal identifier (empty, leading digit, whitespace, ...), which for a
/// hand-written struct/field name is a non-issue (`syn` already rejected
/// anything illegal when parsing the struct itself), but every string this
/// function is used for (`method = "..."`, `related_key = "..."`, and
/// `foreign_key`'s value when a batch loader needs it as a field access)
/// is free text with no such guarantee.
///
/// A plain Rust *keyword* (`type`, `move`, ...) is a real, expected case
/// here, not just a hypothetical — it's exactly the kind of column name
/// SQL happily allows and this codebase's own `#[derive(Model)]` already
/// supports on struct fields via raw-identifier syntax (`pub r#type: i64`;
/// see `model.rs`'s `field_name_str`, and the raw-identifier field test in
/// `tests/model_raw_identifier_field.rs`). `value` itself is always the
/// *clean* name with no `r#` prefix (the convention this whole macro uses
/// for column-name strings — `#[route_key("...")]`, `foreign_key`, etc. —
/// since that's also the string spliced directly into SQL, where an `r#`
/// prefix would be wrong: `WHERE "r#type" = ?` doesn't match a column
/// actually named `type`). So when `value` fails to parse as a plain
/// identifier, this retries with an `r#` prefix before giving up — turning
/// `"type"` into the raw identifier `r#type` for the *field-access* role,
/// while the caller's own `value: &str` (used for the SQL role) stays
/// unprefixed. Only a genuinely invalid identifier (empty, `"123bad"`,
/// `"has a space"`) fails both attempts.
///
/// `what` names the attribute argument in the error message;
/// `errors_spanned_on` is the token span the error should point at.
fn parse_ident(
    value: &str,
    what: &str,
    errors_spanned_on: &impl quote::ToTokens,
) -> syn::Result<syn::Ident> {
    syn::parse_str::<syn::Ident>(value)
        .or_else(|_| syn::parse_str::<syn::Ident>(&format!("r#{value}")))
        .map_err(|_| {
            syn::Error::new_spanned(
                errors_spanned_on,
                format!("`{what} = \"{value}\"` is not a valid Rust identifier"),
            )
        })
}

/// Ported verbatim from `crates/larust-cli/src/generate.rs`'s `pluralize`
/// (used there for `xr make:model`'s default table name) rather than
/// rewritten from scratch — that version fixed a real bug (checking
/// whether the character *before* a trailing `y` is a vowel, not whether
/// the whole word "ends with" a vowel, which is a contradiction for a word
/// ending in `y`); duplicating the already-correct logic avoids
/// reintroducing it. See `docs/GOTCHAS.md`.
fn pluralize(word: &str) -> String {
    let preceded_by_vowel = word
        .len()
        .checked_sub(2)
        .and_then(|i| word.as_bytes().get(i))
        .is_some_and(|b| matches!(b, b'a' | b'e' | b'i' | b'o' | b'u'));
    if word.ends_with('y') && !preceded_by_vowel {
        format!("{}ies", &word[..word.len() - 1])
    } else if word.ends_with('s')
        || word.ends_with('x')
        || word.ends_with('z')
        || word.ends_with("ch")
        || word.ends_with("sh")
    {
        format!("{word}es")
    } else {
        format!("{word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pluralize_handles_common_cases() {
        assert_eq!(pluralize("post"), "posts");
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("bus"), "buses");
        assert_eq!(pluralize("day"), "days");
    }

    #[test]
    fn related_type_name_takes_the_last_path_segment() {
        let path: syn::Path = syn::parse_str("crate::models::Post").unwrap();
        assert_eq!(related_type_name(&path), "Post");

        let bare: syn::Path = syn::parse_str("Post").unwrap();
        assert_eq!(related_type_name(&bare), "Post");
    }

    #[test]
    fn parse_ident_accepts_legal_identifiers() {
        let dummy = quote::quote! { "owner" };
        assert!(parse_ident("owner", "method", &dummy).is_ok());
        assert!(parse_ident("_leading_underscore", "method", &dummy).is_ok());
    }

    #[test]
    fn parse_ident_rejects_illegal_identifiers_without_panicking() {
        // None of these become valid even with the `r#` raw-identifier
        // fallback (unlike an ordinary keyword such as `"type"`/`"fn"` —
        // see `parse_ident_falls_back_to_a_raw_identifier_for_a_keyword`):
        // `""`/`"123bad"`/`"has a space"` aren't legal identifier syntax at
        // all, and `self` is one of the handful of keywords (along with
        // `super`/`Self`/`crate`) Rust never allows as a raw identifier —
        // `r#self` is itself a syntax error, not an escape hatch.
        let dummy = quote::quote! { "bad" };
        assert!(parse_ident("", "method", &dummy).is_err());
        assert!(parse_ident("123bad", "method", &dummy).is_err());
        assert!(parse_ident("has a space", "method", &dummy).is_err());
        assert!(parse_ident("self", "method", &dummy).is_err());
    }

    #[test]
    fn parse_relation_attrs_rejects_a_related_type_given_twice() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many(Post, Post, foreign_key = "user_id")]
            struct User {}
        };
        let err = parse_relation_attrs(&input, "has_many", false).unwrap_err();
        assert!(err.to_string().contains("already given"));
    }

    #[test]
    fn parse_relation_attrs_rejects_duplicate_foreign_key() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many(Post, foreign_key = "a", foreign_key = "b")]
            struct User {}
        };
        let err = parse_relation_attrs(&input, "has_many", false).unwrap_err();
        assert!(err.to_string().contains("already given"));
    }

    #[test]
    fn parse_relation_attrs_rejects_duplicate_method() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many(Post, foreign_key = "user_id", method = "a", method = "b")]
            struct User {}
        };
        let err = parse_relation_attrs(&input, "has_many", false).unwrap_err();
        assert!(err.to_string().contains("already given"));
    }

    #[test]
    fn parse_relation_attrs_rejects_an_invalid_method_identifier_cleanly() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many(Post, foreign_key = "user_id", method = "123bad")]
            struct User {}
        };
        let err = parse_relation_attrs(&input, "has_many", false).unwrap_err();
        assert!(err.to_string().contains("not a valid Rust identifier"));
    }

    #[test]
    fn parse_relation_attrs_requires_foreign_key() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many(Post)]
            struct User {}
        };
        let err = parse_relation_attrs(&input, "has_many", false).unwrap_err();
        assert!(err.to_string().contains("requires foreign_key"));
    }

    #[test]
    fn parse_relation_attrs_requires_a_related_type() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many(foreign_key = "user_id")]
            struct User {}
        };
        let err = parse_relation_attrs(&input, "has_many", false).unwrap_err();
        assert!(err.to_string().contains("requires a related model type"));
    }

    #[test]
    fn parse_relation_attrs_rejects_related_key_on_has_many() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many(Post, foreign_key = "user_id", related_key = "id")]
            struct User {}
        };
        let err = parse_relation_attrs(&input, "has_many", false).unwrap_err();
        assert!(err.to_string().contains("only meaningful on #[belongs_to"));
    }

    #[test]
    fn parse_relation_attrs_accepts_related_key_when_allowed() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to(User, foreign_key = "user_id", related_key = "user_id")]
            struct Post {}
        };
        let specs = parse_relation_attrs(&input, "belongs_to", true).unwrap();
        assert_eq!(specs[0].related_key.as_deref(), Some("user_id"));
    }

    #[test]
    fn parse_ident_falls_back_to_a_raw_identifier_for_a_keyword() {
        let dummy = quote::quote! { "type" };
        let ident = parse_ident("type", "foreign_key", &dummy).unwrap();
        // `r#type`'s own `to_string()` includes the `r#` prefix — this is
        // exactly why the SQL-column-name role must keep the original
        // clean string separately rather than deriving it from this ident.
        assert_eq!(ident.to_string(), "r#type");
    }

    #[test]
    fn expand_rejects_an_invalid_foreign_key_identifier_on_has_many_cleanly() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many(Post, foreign_key = "has a space")]
            struct User {}
        };
        let pk_ident: syn::Ident = syn::parse_quote!(id);
        let id_ty: syn::Type = syn::parse_quote!(i64);
        let fields = [(&pk_ident, &id_ty)];

        let err = expand(&input, &fields, &pk_ident).unwrap_err();
        assert!(err.to_string().contains("not a valid Rust identifier"));
    }

    #[test]
    fn expand_rejects_an_invalid_related_key_identifier_on_belongs_to_cleanly() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to(User, foreign_key = "user_id", related_key = "has a space")]
            struct Post { user_id: i64 }
        };
        let user_id_ident: syn::Ident = syn::parse_quote!(user_id);
        let i64_ty: syn::Type = syn::parse_quote!(i64);
        let pk_ident: syn::Ident = syn::parse_quote!(id);
        let fields = [(&user_id_ident, &i64_ty)];

        let err = expand(&input, &fields, &pk_ident).unwrap_err();
        assert!(err.to_string().contains("not a valid Rust identifier"));
    }
}
