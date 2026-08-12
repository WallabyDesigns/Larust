//! `#[belongs_to_many(...)]` — many-to-many relationships via a pivot
//! table (Laravel's `belongsToMany`). A separate file from `relations.rs`
//! (which owns `has_many`/`has_one`/`belongs_to`) since this needs a real
//! `JOIN`, which `QueryBuilder` deliberately doesn't support (its scope is
//! single-table `SELECT` — see `crates/larust-orm/src/query_builder.rs`).
//! Generated code hand-writes SQL and calls `sqlx::query`/`query_as`
//! directly instead, the same way `#[derive(Model)]`'s own `create`/
//! `delete` already do for shapes `QueryBuilder` doesn't cover (see
//! `model.rs`).
//!
//! Every SQL identifier this module deals with (`through`, `foreign_key`,
//! `related_pivot_key`, `related_key`) is used *only* as a SQL string —
//! never spliced as a Rust field-access expression — so none of it needs
//! `relations.rs`'s `parse_ident` raw-identifier-escaping machinery (added
//! there specifically because `load_*`'s batch loaders read a joined row's
//! foreign key back as a real struct field). That's also why eager/batch
//! loading isn't built here yet: it would need exactly that field-access
//! role, which needs a synthetic row type carrying pivot columns
//! alongside the related struct's own ones — a real, separate design
//! problem. See `docs/MACROS.md`.

use crate::model::to_snake_case;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{DeriveInput, Meta, Token};

#[derive(Debug)]
struct BelongsToManySpec {
    related: syn::Path,
    through: String,
    foreign_key: String,
    related_pivot_key: String,
    related_key: Option<String>,
    method_name: Option<syn::Ident>,
}

/// Parses every `#[belongs_to_many(...)]` attribute on `input` and
/// generates one `impl #struct_name { ... }` block with four methods per
/// relationship (empty output if none are declared): the lazy accessor
/// (`tags()`), `attach_*`/`detach_*` (single pivot row insert/delete), and
/// `sync_*` (replace the full set for `self` in one transaction).
/// `pk_ident` is the struct's own `#[primary_key]` field, bound as the
/// "this row's id" side of every pivot query.
pub fn expand(input: &DeriveInput, pk_ident: &syn::Ident) -> syn::Result<TokenStream> {
    let struct_name = &input.ident;
    let mut methods = Vec::new();

    for spec in parse_belongs_to_many_attrs(input)? {
        let accessor_name = spec.method_name.clone().unwrap_or_else(|| {
            format_ident!(
                "{}",
                pluralize(&to_snake_case(&related_type_name(&spec.related)))
            )
        });
        let singular = to_snake_case(&related_type_name(&spec.related));
        let attach_name = format_ident!("attach_{singular}");
        let detach_name = format_ident!("detach_{singular}");
        let sync_name = format_ident!("sync_{}", pluralize(&singular));

        let related = &spec.related;
        let through = &spec.through;
        let foreign_key = &spec.foreign_key;
        let related_pivot_key = &spec.related_pivot_key;
        let related_key = spec.related_key.as_deref().unwrap_or("id");

        methods.push(quote! {
            pub async fn #accessor_name(&self) -> ::std::result::Result<::std::vec::Vec<#related>, ::larust_support::AppError> {
                // Built at *call* time, not macro-expansion time (unlike
                // `#[derive(Model)]`'s own `insert_sql`/`delete_sql`
                // string-literal constants) — the related table's name
                // (`#related::TABLE`) is only known once `#related`'s own
                // `#[derive(Model)]` expansion runs, which this macro
                // invocation can't see; reading it as a real `const`
                // through a runtime `format!` is a small, one-time-per-call
                // cost in exchange for never letting this query's table
                // name drift out of sync with `#related`'s own `#[table(
                // ...)]`.
                let sql = ::std::format!(
                    "SELECT \"{}\".* FROM \"{}\" INNER JOIN \"{}\" ON \"{}\".\"{}\" = \"{}\".\"{}\" WHERE \"{}\".\"{}\" = ?",
                    #related::TABLE, #related::TABLE, #through, #related::TABLE, #related_key,
                    #through, #related_pivot_key, #through, #foreign_key,
                );
                ::larust_support::orm::sqlx::query_as::<_, #related>(&sql)
                    .bind(self.#pk_ident)
                    .fetch_all(::larust_support::orm::pool()?)
                    .await
                    .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))
            }

            /// Inserts one pivot row (Laravel's `attach($id)`); attaching an
            /// already-attached pair is a harmless no-op (`INSERT OR
            /// IGNORE`), not a `UNIQUE`-constraint error — deliberately
            /// more forgiving than Laravel's own default.
            pub async fn #attach_name(
                &self,
                related_id: i64,
            ) -> ::std::result::Result<(), ::larust_support::AppError> {
                let sql = ::std::format!(
                    "INSERT OR IGNORE INTO \"{}\" (\"{}\", \"{}\") VALUES (?, ?)",
                    #through, #foreign_key, #related_pivot_key,
                );
                ::larust_support::orm::sqlx::query(&sql)
                    .bind(self.#pk_ident)
                    .bind(related_id)
                    .execute(::larust_support::orm::pool()?)
                    .await
                    .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))?;
                ::std::result::Result::Ok(())
            }

            /// Deletes one pivot row (Laravel's `detach($id)`).
            pub async fn #detach_name(
                &self,
                related_id: i64,
            ) -> ::std::result::Result<(), ::larust_support::AppError> {
                let sql = ::std::format!(
                    "DELETE FROM \"{}\" WHERE \"{}\" = ? AND \"{}\" = ?",
                    #through, #foreign_key, #related_pivot_key,
                );
                ::larust_support::orm::sqlx::query(&sql)
                    .bind(self.#pk_ident)
                    .bind(related_id)
                    .execute(::larust_support::orm::pool()?)
                    .await
                    .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))?;
                ::std::result::Result::Ok(())
            }

            /// Replaces the full set of related rows for `self` with
            /// exactly `related_ids` (Laravel's `sync([...])`) — deletes
            /// every existing pivot row for this row, then inserts one per
            /// given id, all in one transaction, so a failure partway
            /// through leaves the original set untouched rather than
            /// half-deleted.
            pub async fn #sync_name(
                &self,
                related_ids: &[i64],
            ) -> ::std::result::Result<(), ::larust_support::AppError> {
                let pool = ::larust_support::orm::pool()?;
                let mut tx = pool
                    .begin()
                    .await
                    .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))?;

                let delete_sql = ::std::format!(
                    "DELETE FROM \"{}\" WHERE \"{}\" = ?",
                    #through, #foreign_key,
                );
                ::larust_support::orm::sqlx::query(&delete_sql)
                    .bind(self.#pk_ident)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))?;

                let insert_sql = ::std::format!(
                    "INSERT INTO \"{}\" (\"{}\", \"{}\") VALUES (?, ?)",
                    #through, #foreign_key, #related_pivot_key,
                );
                for related_id in related_ids {
                    ::larust_support::orm::sqlx::query(&insert_sql)
                        .bind(self.#pk_ident)
                        .bind(related_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))?;
                }

                tx.commit()
                    .await
                    .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))?;
                ::std::result::Result::Ok(())
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

fn related_type_name(path: &syn::Path) -> String {
    path.segments
        .last()
        .expect("a relationship's related type is a non-empty path")
        .ident
        .to_string()
}

/// Ported verbatim from `relations.rs`'s own copy (itself ported from
/// `crates/larust-cli/src/generate.rs`) rather than sharing a `pub(crate)`
/// function across two already-small files — see `docs/GOTCHAS.md` for the
/// vowel-detection bug this exact logic already fixed once.
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

/// Parses every `#[belongs_to_many(RelatedType, through = "...", foreign_key
/// = "...", related_pivot_key = "...", related_key = "...", method =
/// "...")]` attribute on `input` (repeatable). `through`/`foreign_key`/
/// `related_pivot_key` are required; `related_key` defaults to `"id"`;
/// `method` is optional.
fn parse_belongs_to_many_attrs(input: &DeriveInput) -> syn::Result<Vec<BelongsToManySpec>> {
    let mut specs = Vec::new();

    for attr in input
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("belongs_to_many"))
    {
        let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;

        let mut related: Option<syn::Path> = None;
        let mut through: Option<String> = None;
        let mut foreign_key: Option<String> = None;
        let mut related_pivot_key: Option<String> = None;
        let mut related_key: Option<String> = None;
        let mut method_name: Option<syn::Ident> = None;

        for meta in &metas {
            match meta {
                Meta::Path(path) if related.is_none() => related = Some(path.clone()),
                Meta::Path(path) => {
                    return Err(syn::Error::new_spanned(
                        path,
                        "a related model type was already given for this attribute",
                    ));
                }
                Meta::NameValue(nv) if nv.path.is_ident("through") => {
                    reject_duplicate(&through, nv, "through")?;
                    through = Some(expect_str_literal(&nv.value)?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("foreign_key") => {
                    reject_duplicate(&foreign_key, nv, "foreign_key")?;
                    foreign_key = Some(expect_str_literal(&nv.value)?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("related_pivot_key") => {
                    reject_duplicate(&related_pivot_key, nv, "related_pivot_key")?;
                    related_pivot_key = Some(expect_str_literal(&nv.value)?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("related_key") => {
                    reject_duplicate(&related_key, nv, "related_key")?;
                    related_key = Some(expect_str_literal(&nv.value)?);
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
                _ => {
                    return Err(syn::Error::new_spanned(
                        meta,
                        "unrecognized #[belongs_to_many(...)] argument (expected a related \
                         model type, `through = \"...\"`, `foreign_key = \"...\"`, \
                         `related_pivot_key = \"...\"`, `related_key = \"...\"`, or \
                         `method = \"...\"`)",
                    ));
                }
            }
        }

        let related = related.ok_or_else(|| {
            syn::Error::new_spanned(
                attr,
                "#[belongs_to_many(...)] requires a related model type, e.g. \
                 #[belongs_to_many(Tag, through = \"post_tag\", foreign_key = \"post_id\", \
                 related_pivot_key = \"tag_id\")]",
            )
        })?;
        let through = through.ok_or_else(|| {
            syn::Error::new_spanned(attr, "#[belongs_to_many(...)] requires through = \"...\"")
        })?;
        let foreign_key = foreign_key.ok_or_else(|| {
            syn::Error::new_spanned(
                attr,
                "#[belongs_to_many(...)] requires foreign_key = \"...\"",
            )
        })?;
        let related_pivot_key = related_pivot_key.ok_or_else(|| {
            syn::Error::new_spanned(
                attr,
                "#[belongs_to_many(...)] requires related_pivot_key = \"...\"",
            )
        })?;

        specs.push(BelongsToManySpec {
            related,
            through,
            foreign_key,
            related_pivot_key,
            related_key,
            method_name,
        });
    }

    Ok(specs)
}

fn reject_duplicate(
    existing: &Option<String>,
    spanned_on: &impl quote::ToTokens,
    what: &str,
) -> syn::Result<()> {
    if existing.is_some() {
        return Err(syn::Error::new_spanned(
            spanned_on,
            format!("`{what}` was already given for this attribute"),
        ));
    }
    Ok(())
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

/// `method = "..."` *is* spliced as a real Rust identifier (the accessor's
/// name), unlike `through`/`foreign_key`/`related_pivot_key`/`related_key`
/// (see the module doc comment) — so it needs the same raw-identifier
/// fallback `relations.rs`'s own `parse_ident` gives its `method`/
/// `foreign_key`/`related_key` arguments (tries the plain identifier
/// first, falls back to an `r#`-prefixed raw identifier for a keyword like
/// `"type"`, and only fails for something that isn't identifier-shaped at
/// all). Ported rather than shared across the two files for the same
/// reason `pluralize` is — see that function's doc comment.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ident_falls_back_to_a_raw_identifier_for_a_keyword() {
        let dummy = quote::quote! { "type" };
        let ident = parse_ident("type", "method", &dummy).unwrap();
        assert_eq!(ident.to_string(), "r#type");
    }

    #[test]
    fn parse_ident_rejects_identifiers_illegal_even_with_a_raw_prefix() {
        let dummy = quote::quote! { "bad" };
        assert!(parse_ident("", "method", &dummy).is_err());
        assert!(parse_ident("123bad", "method", &dummy).is_err());
        assert!(parse_ident("self", "method", &dummy).is_err());
    }

    #[test]
    fn rejects_a_related_type_given_twice() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, Tag, through = "post_tag", foreign_key = "post_id", related_pivot_key = "tag_id")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("already given"));
    }

    #[test]
    fn rejects_duplicate_through() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, through = "a", through = "b", foreign_key = "post_id", related_pivot_key = "tag_id")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("`through` was already given"));
    }

    #[test]
    fn rejects_duplicate_foreign_key() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, through = "post_tag", foreign_key = "a", foreign_key = "b", related_pivot_key = "tag_id")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("`foreign_key` was already given"));
    }

    #[test]
    fn rejects_duplicate_related_pivot_key() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, through = "post_tag", foreign_key = "post_id", related_pivot_key = "a", related_pivot_key = "b")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err
            .to_string()
            .contains("`related_pivot_key` was already given"));
    }

    #[test]
    fn rejects_duplicate_related_key() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, through = "post_tag", foreign_key = "post_id", related_pivot_key = "tag_id", related_key = "a", related_key = "b")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("`related_key` was already given"));
    }

    #[test]
    fn rejects_duplicate_method() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, through = "post_tag", foreign_key = "post_id", related_pivot_key = "tag_id", method = "a", method = "b")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("`method` was already given"));
    }

    #[test]
    fn rejects_an_invalid_method_identifier_cleanly() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, through = "post_tag", foreign_key = "post_id", related_pivot_key = "tag_id", method = "123bad")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("not a valid Rust identifier"));
    }

    #[test]
    fn requires_a_related_type() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(through = "post_tag", foreign_key = "post_id", related_pivot_key = "tag_id")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("requires a related model type"));
    }

    #[test]
    fn requires_through() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, foreign_key = "post_id", related_pivot_key = "tag_id")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("requires through"));
    }

    #[test]
    fn requires_foreign_key() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, through = "post_tag", related_pivot_key = "tag_id")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("requires foreign_key"));
    }

    #[test]
    fn requires_related_pivot_key() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, through = "post_tag", foreign_key = "post_id")]
            struct Post {}
        };
        let err = parse_belongs_to_many_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("requires related_pivot_key"));
    }

    #[test]
    fn related_key_and_method_are_optional_and_captured_when_given() {
        let input: DeriveInput = syn::parse_quote! {
            #[belongs_to_many(Tag, through = "post_tag", foreign_key = "post_id", related_pivot_key = "tag_id", related_key = "tag_key", method = "labels")]
            struct Post {}
        };
        let specs = parse_belongs_to_many_attrs(&input).unwrap();
        assert_eq!(specs[0].related_key.as_deref(), Some("tag_key"));
        assert_eq!(specs[0].method_name.as_ref().unwrap(), "labels");
    }
}
