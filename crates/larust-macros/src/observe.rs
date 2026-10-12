//! `#[observe(...)]` - Eloquent's model-event observers, ported as
//! compile-time hook *paths* rather than a trait with default (optional)
//! methods.
//!
//! ```ignore
//! #[derive(Model, sqlx::FromRow)]
//! #[table("posts")]
//! #[observe(creating = PostObserver::creating, created = PostObserver::created)]
//! pub struct Post { ... }
//!
//! struct PostObserver;
//! impl PostObserver {
//!     async fn creating(data: NewPost) -> Result<NewPost, AppError> { Ok(data) }
//!     async fn created(post: &Post) -> Result<(), AppError> { Ok(()) }
//! }
//! ```
//!
//! **Why an attribute of fixed paths, not `trait Observer<M> { ... }`
//! with default no-op methods**: every optional-hook trait already in
//! this codebase (`Mailable`, `Job`, `Authenticatable`) has *zero*
//! default methods - a mismatched signature or a typo'd hook name is
//! already a compile error there, never a silently-skipped default. A
//! trait with default bodies would be the first exception to that
//! convention, for the sake of Eloquent-shaped ergonomics this attribute
//! already gets without it: a mistyped `#[observe(creatign = ...)]` is
//! rejected by the macro itself with a clear message (see
//! `parse_observe_attr` below), and a hook with the wrong signature is a
//! plain rustc error pointing at the attribute's own path expression
//! (spans carry through `quote!`), not a trait-impl mismatch several
//! layers removed from the attribute that caused it.
//!
//! Only six hooks are supported so far: `creating`/`created` (around
//! `create()`), `updating`/`updated` (around `update()`), `deleting`/
//! `deleted` (around `delete()`). Deliberately not yet supported:
//! `restoring`/`restored`/`force_deleting`/`force_deleted` (around
//! `#[soft_deletes]`'s own `restore()`/`force_delete()`) - a real,
//! documented gap, not an oversight; add them the same way if ever
//! needed. Hooks fire only through these generated static methods
//! (which includes model factories and `Repository`, since both
//! forward to them) - never for raw `sqlx` calls or pivot `attach`/
//! `detach`/`sync`, and there are no `retrieved`/`saving`-style hooks.

use syn::punctuated::Punctuated;
use syn::{DeriveInput, Meta, Token};

#[derive(Default, Debug)]
pub(crate) struct ObserveSpec {
    pub(crate) creating: Option<syn::Path>,
    pub(crate) created: Option<syn::Path>,
    pub(crate) updating: Option<syn::Path>,
    pub(crate) updated: Option<syn::Path>,
    pub(crate) deleting: Option<syn::Path>,
    pub(crate) deleted: Option<syn::Path>,
}

impl ObserveSpec {
    /// `deleting`/`deleted` need the row fetched first (there's no
    /// already-loaded instance the way `$model->delete()` has in
    /// Eloquent) - only worth the extra `SELECT` when something would
    /// actually use it.
    pub(crate) fn needs_delete_snapshot(&self) -> bool {
        self.deleting.is_some() || self.deleted.is_some()
    }
}

/// Parses `#[observe(hook = path::to::fn, ...)]` - at most one such
/// attribute per struct (combine every hook into it rather than
/// stacking several, unlike `#[has_many(...)]`'s deliberately-repeatable
/// shape - there's only one observer configuration per model, not a list
/// of independent relationships).
pub(crate) fn parse_observe_attr(input: &DeriveInput) -> syn::Result<ObserveSpec> {
    let mut spec = ObserveSpec::default();
    let mut already_seen = false;

    for attr in input.attrs.iter().filter(|a| a.path().is_ident("observe")) {
        if already_seen {
            return Err(syn::Error::new_spanned(
                attr,
                "#[observe(...)] may only be given once - combine every hook into \
                 the same attribute",
            ));
        }
        already_seen = true;

        let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
        for meta in &metas {
            let Meta::NameValue(nv) = meta else {
                return Err(syn::Error::new_spanned(
                    meta,
                    "expected `hook = path::to::fn`, e.g. `creating = PostObserver::creating`",
                ));
            };
            let path = expect_path(&nv.value)?;

            let slot = if nv.path.is_ident("creating") {
                &mut spec.creating
            } else if nv.path.is_ident("created") {
                &mut spec.created
            } else if nv.path.is_ident("updating") {
                &mut spec.updating
            } else if nv.path.is_ident("updated") {
                &mut spec.updated
            } else if nv.path.is_ident("deleting") {
                &mut spec.deleting
            } else if nv.path.is_ident("deleted") {
                &mut spec.deleted
            } else {
                return Err(syn::Error::new_spanned(
                    &nv.path,
                    "unrecognized #[observe(...)] hook (expected one of: creating, \
                     created, updating, updated, deleting, deleted)",
                ));
            };
            if slot.is_some() {
                return Err(syn::Error::new_spanned(
                    nv,
                    "this hook was already given for this attribute",
                ));
            }
            *slot = Some(path);
        }
    }

    Ok(spec)
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
            "expected a path to a function, e.g. `PostObserver::creating`",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_recognized_hook() {
        let input: DeriveInput = syn::parse_quote! {
            #[observe(
                creating = A::creating,
                created = A::created,
                updating = A::updating,
                updated = A::updated,
                deleting = A::deleting,
                deleted = A::deleted,
            )]
            struct Post {}
        };
        let spec = parse_observe_attr(&input).unwrap();
        assert!(spec.creating.is_some());
        assert!(spec.created.is_some());
        assert!(spec.updating.is_some());
        assert!(spec.updated.is_some());
        assert!(spec.deleting.is_some());
        assert!(spec.deleted.is_some());
        assert!(spec.needs_delete_snapshot());
    }

    #[test]
    fn no_attribute_at_all_is_a_fully_empty_spec() {
        let input: DeriveInput = syn::parse_quote! { struct Post {} };
        let spec = parse_observe_attr(&input).unwrap();
        assert!(spec.creating.is_none());
        assert!(!spec.needs_delete_snapshot());
    }

    #[test]
    fn rejects_an_unrecognized_hook_name() {
        let input: DeriveInput = syn::parse_quote! {
            #[observe(retrieved = A::retrieved)]
            struct Post {}
        };
        let err = parse_observe_attr(&input).unwrap_err();
        assert!(err
            .to_string()
            .contains("unrecognized #[observe(...)] hook"));
    }

    #[test]
    fn rejects_a_hook_given_twice() {
        let input: DeriveInput = syn::parse_quote! {
            #[observe(creating = A::one, creating = A::two)]
            struct Post {}
        };
        let err = parse_observe_attr(&input).unwrap_err();
        assert!(err.to_string().contains("already given"));
    }

    #[test]
    fn rejects_a_non_path_value() {
        let input: DeriveInput = syn::parse_quote! {
            #[observe(creating = "not a path")]
            struct Post {}
        };
        let err = parse_observe_attr(&input).unwrap_err();
        assert!(err.to_string().contains("expected a path to a function"));
    }

    #[test]
    fn rejects_a_second_observe_attribute() {
        let input: DeriveInput = syn::parse_quote! {
            #[observe(creating = A::one)]
            #[observe(created = A::two)]
            struct Post {}
        };
        let err = parse_observe_attr(&input).unwrap_err();
        assert!(err.to_string().contains("may only be given once"));
    }
}
