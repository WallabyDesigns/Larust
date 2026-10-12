//! `#[has_many_through(...)]` - Laravel's `hasManyThrough` (a 3-table
//! relationship: `Country` has many `Post` *through* `User`). A separate
//! file from `relations.rs` (already ~620 lines) and from
//! `belongs_to_many.rs` (a real `JOIN`, which this isn't) - same
//! "separate file per relationship shape once it earns one" precedent
//! `belongs_to_many.rs` itself already set.
//!
//! Built on `QueryBuilder::where_in_query` (`larust-orm`, added for
//! exactly this), not a real `JOIN` - see that method's own doc comment
//! for why `QueryBuilder` can't do joins at all. Laravel's own
//! `hasManyThrough($related, $through, $firstKey, $secondKey, $localKey,
//! $secondLocalKey)`:
//!
//! ```text
//! SELECT posts.* FROM posts
//!   INNER JOIN users ON posts.user_id = users.id
//!   WHERE users.country_id = ?
//! ```
//!
//! becomes, as a subquery:
//!
//! ```text
//! SELECT posts.* FROM posts
//!   WHERE posts.user_id IN (SELECT users.id FROM users WHERE users.country_id = ?)
//! ```
//!
//! `first_key` (`users.country_id`, referencing *this* model),
//! `second_key` (`posts.user_id`, referencing `through`), and
//! `second_local_key` (`users.id`, `through`'s own key) are used *only*
//! as SQL column-name strings here - never as Rust field-access
//! expressions - so unlike `relations.rs`'s own `foreign_key`/
//! `related_key`, none of them need `parse_ident`'s raw-identifier
//! handling. Only `local_key` (this model's own field, bound into
//! `self.#local_key_ident` for the subquery's own `WHERE`) needs that
//! treatment, and needs to actually exist as an `i64` field on `Self` -
//! validated the same way `belongs_to`'s `foreign_key` already is.
//!
//! **Deliberately no batch/eager `load_*` form yet** (every other
//! relationship kind in this codebase has one) - grouping a 3-table hop
//! back onto the *input* rows needs either a `Clone` bound on the
//! related type (a real new requirement every `#[has_many_through(...)]`
//! use would impose, not just callers of the batch form, since a
//! concrete, non-generic method's `where` clause is checked at
//! definition time, not lazily per call site) or a two-map return shape
//! (ids-grouped-by-input-row plus rows-by-id, borrowed rather than
//! cloned) that's a real usability step down from every other `load_*`
//! method's single-map shape. Worth solving properly later, not worth
//! shipping either compromise just to have *a* batch form. The lazy
//! accessor (`.posts()`/`.posts_query()`) is unaffected either way.

use crate::model::{field_name_str, is_i64_type, to_snake_case};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{DeriveInput, Meta, Token};

#[derive(Debug)]
struct HasManyThroughSpec {
    related: syn::Path,
    through: syn::Path,
    first_key: String,
    second_key: String,
    local_key: Option<String>,
    second_local_key: Option<String>,
    method_name: Option<syn::Ident>,
}

/// Parses every `#[has_many_through(...)]` attribute on `input` and
/// generates one `impl #struct_name { ... }` block with a `{method}_query()`
/// (composable with the rest of `QueryBuilder`'s own chain - `.scope(...)`,
/// `.latest(...)`, etc.) plus a `{method}()` convenience wrapper, per
/// relationship (empty output if none are declared - repeatable, same as
/// every other relationship attribute). `pk_ident`/`all_fields` mirror
/// `relations::expand`'s own parameters, needed to validate `local_key`.
pub fn expand(
    input: &DeriveInput,
    all_fields: &[(&syn::Ident, &syn::Type)],
    pk_ident: &syn::Ident,
) -> syn::Result<TokenStream> {
    let struct_name = &input.ident;
    let mut methods = Vec::new();

    for spec in parse_has_many_through_attrs(input)? {
        let local_key_name = spec
            .local_key
            .clone()
            .unwrap_or_else(|| field_name_str(pk_ident));
        let Some((local_key_ident, local_key_ty)) = find_field(all_fields, &local_key_name) else {
            return Err(syn::Error::new_spanned(
                &input.ident,
                format!(
                    "#[has_many_through(...)] local_key = \"{local_key_name}\": no field named \
                     `{local_key_name}` on this struct"
                ),
            ));
        };
        if !is_i64_type(local_key_ty) {
            return Err(syn::Error::new_spanned(
                local_key_ty,
                format!(
                    "#[has_many_through(...)] local_key field `{local_key_name}` must be `i64`"
                ),
            ));
        }
        let second_local_key = spec
            .second_local_key
            .clone()
            .unwrap_or_else(|| "id".to_string());

        let method_name = match &spec.method_name {
            Some(ident) => ident.clone(),
            None => format_ident!(
                "{}",
                pluralize(&to_snake_case(&related_type_name(&spec.related)))
            ),
        };
        let query_method_name = format_ident!("{method_name}_query");

        let related = &spec.related;
        let through = &spec.through;
        let first_key = &spec.first_key;
        let second_key = &spec.second_key;

        methods.push(quote! {
            /// A composable `QueryBuilder<#related>` for this relationship -
            /// chain `.scope(...)`/`.latest(...)`/`.limit(...)` etc. on it
            /// the same as `#related::query()` itself, before running it.
            pub fn #query_method_name(&self) -> ::larust_support::orm::QueryBuilder<#related> {
                #related::query().where_in_query(
                    #second_key,
                    #second_local_key,
                    #through::query().where_eq(#first_key, self.#local_key_ident),
                )
            }

            pub async fn #method_name(&self) -> ::std::result::Result<::std::vec::Vec<#related>, ::larust_support::AppError> {
                self.#query_method_name().get().await
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

/// Ported verbatim from `relations.rs`'s own copy - see that copy's own
/// doc comment for why this is duplicated rather than shared.
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

/// Parses every `#[has_many_through(RelatedType, through = ThroughType,
/// first_key = "...", second_key = "...", local_key = "...",
/// second_local_key = "...", method = "...")]` attribute on `input`
/// (repeatable). `through`/`first_key`/`second_key` are required;
/// `local_key` defaults to this struct's own primary key, `second_local_key`
/// defaults to `"id"`, `method` is optional.
fn parse_has_many_through_attrs(input: &DeriveInput) -> syn::Result<Vec<HasManyThroughSpec>> {
    let mut specs = Vec::new();

    for attr in input
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("has_many_through"))
    {
        let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;

        let mut related: Option<syn::Path> = None;
        let mut through: Option<syn::Path> = None;
        let mut first_key: Option<String> = None;
        let mut second_key: Option<String> = None;
        let mut local_key: Option<String> = None;
        let mut second_local_key: Option<String> = None;
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
                    through = Some(expect_path(&nv.value)?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("first_key") => {
                    reject_duplicate(&first_key, nv, "first_key")?;
                    first_key = Some(expect_str_literal(&nv.value)?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("second_key") => {
                    reject_duplicate(&second_key, nv, "second_key")?;
                    second_key = Some(expect_str_literal(&nv.value)?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("local_key") => {
                    reject_duplicate(&local_key, nv, "local_key")?;
                    local_key = Some(expect_str_literal(&nv.value)?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("second_local_key") => {
                    reject_duplicate(&second_local_key, nv, "second_local_key")?;
                    second_local_key = Some(expect_str_literal(&nv.value)?);
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
                        "unrecognized #[has_many_through(...)] argument (expected a related \
                         model type, `through = ThroughType`, `first_key = \"...\"`, \
                         `second_key = \"...\"`, `local_key = \"...\"`, \
                         `second_local_key = \"...\"`, or `method = \"...\"`)",
                    ));
                }
            }
        }

        let related = related.ok_or_else(|| {
            syn::Error::new_spanned(
                attr,
                "#[has_many_through(...)] requires a related model type, e.g. \
                 #[has_many_through(Post, through = User, first_key = \"country_id\", \
                 second_key = \"user_id\")]",
            )
        })?;
        let through = through.ok_or_else(|| {
            syn::Error::new_spanned(
                attr,
                "#[has_many_through(...)] requires through = ThroughType",
            )
        })?;
        let first_key = first_key.ok_or_else(|| {
            syn::Error::new_spanned(
                attr,
                "#[has_many_through(...)] requires first_key = \"...\"",
            )
        })?;
        let second_key = second_key.ok_or_else(|| {
            syn::Error::new_spanned(
                attr,
                "#[has_many_through(...)] requires second_key = \"...\"",
            )
        })?;

        specs.push(HasManyThroughSpec {
            related,
            through,
            first_key,
            second_key,
            local_key,
            second_local_key,
            method_name,
        });
    }

    Ok(specs)
}

fn reject_duplicate<T>(
    existing: &Option<T>,
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

fn expect_path(expr: &syn::Expr) -> syn::Result<syn::Path> {
    if let syn::Expr::Path(syn::ExprPath {
        path, qself: None, ..
    }) = expr
    {
        Ok(path.clone())
    } else {
        Err(syn::Error::new_spanned(
            expr,
            "expected a type path, e.g. `through = User`",
        ))
    }
}

/// Ported verbatim from `relations.rs`'s own copy - see that module's own
/// doc comment for why this is duplicated rather than shared across tiny
/// files.
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
    fn parses_required_and_optional_arguments() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many_through(Post, through = User, first_key = "country_id", second_key = "user_id")]
            struct Country {}
        };
        let specs = parse_has_many_through_attrs(&input).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].first_key, "country_id");
        assert_eq!(specs[0].second_key, "user_id");
        assert!(specs[0].local_key.is_none());
        assert!(specs[0].second_local_key.is_none());
    }

    #[test]
    fn requires_through() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many_through(Post, first_key = "country_id", second_key = "user_id")]
            struct Country {}
        };
        let err = parse_has_many_through_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("requires through"));
    }

    #[test]
    fn requires_first_key_and_second_key() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many_through(Post, through = User, second_key = "user_id")]
            struct Country {}
        };
        let err = parse_has_many_through_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("requires first_key"));

        let input: DeriveInput = syn::parse_quote! {
            #[has_many_through(Post, through = User, first_key = "country_id")]
            struct Country {}
        };
        let err = parse_has_many_through_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("requires second_key"));
    }

    #[test]
    fn rejects_a_non_path_through_value() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many_through(Post, through = "User", first_key = "country_id", second_key = "user_id")]
            struct Country {}
        };
        let err = parse_has_many_through_attrs(&input).unwrap_err();
        assert!(err.to_string().contains("expected a type path"));
    }

    #[test]
    fn expand_rejects_a_local_key_that_does_not_exist() {
        let input: DeriveInput = syn::parse_quote! {
            #[has_many_through(Post, through = User, first_key = "country_id", second_key = "user_id", local_key = "nope")]
            struct Country {}
        };
        let pk_ident: syn::Ident = syn::parse_quote!(id);
        let id_ty: syn::Type = syn::parse_quote!(i64);
        let fields = [(&pk_ident, &id_ty)];

        let err = expand(&input, &fields, &pk_ident).unwrap_err();
        assert!(err.to_string().contains("no field named"));
    }
}
