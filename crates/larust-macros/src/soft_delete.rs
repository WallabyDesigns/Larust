//! `#[soft_deletes]` - Eloquent's `SoftDeletes` trait, ported as a single
//! opt-in struct attribute (same shape as `#[timestamps]`): `delete()`
//! sets a nullable `deleted_at` column instead of actually removing the
//! row, every `query()`/`find()`/`all()`/route-model-binding lookup
//! transparently excludes trashed rows, and `force_delete`/`restore`/
//! `find_with_trashed`/`.trashed()` round out the rest of Eloquent's own
//! surface. `model.rs` owns the *detection and validation* of the
//! `deleted_at: Option<i64>` field (the same field-scanning loop that
//! already validates `#[timestamps]`'s `created_at`/`updated_at`) - this
//! module only generates the soft-delete-specific methods that are purely
//! additive (nothing here modifies `delete()`'s own body, `query()`'s
//! extra call, or the `Model` impl's `SOFT_DELETE_COLUMN` override - all
//! three stay inline in `model.rs`, since they're small, surgical
//! branches on code already being built there, not a separable block).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::DeriveInput;

/// `#[soft_deletes]` - bare, no arguments, same reasoning as
/// `#[timestamps]`: the one column name it looks for (`deleted_at`) is
/// fixed by convention, so there's nothing to parametrize.
pub(crate) fn has_soft_deletes_attr(input: &DeriveInput) -> bool {
    input
        .attrs
        .iter()
        .any(|a| a.path().is_ident("soft_deletes"))
}

/// Generates the methods only a soft-deletable model gets:
/// `force_delete` (Eloquent's real, bypass-the-column delete),
/// `restore`, `find_with_trashed`, and an instance `.trashed()` check -
/// plus the `impl SoftDeletes for Self` marker that gates
/// `QueryBuilder::{with_trashed, only_trashed}` at compile time (see
/// `larust_orm::query_builder::SoftDeletes`'s own doc comment).
pub(crate) fn expand(
    struct_name: &syn::Ident,
    table: &str,
    pk_ident: &syn::Ident,
    pk_ty: &syn::Type,
    pk_name: &str,
    deleted_at_ident: &syn::Ident,
    deleted_at_name: &str,
) -> TokenStream {
    let pk_const_ident = format_ident!("{}", pk_name.to_uppercase());

    let force_delete_sql = format!("DELETE FROM \"{table}\" WHERE \"{pk_name}\" = ?");
    let force_delete_sql_postgres = format!("DELETE FROM \"{table}\" WHERE \"{pk_name}\" = $1");

    let restore_sql =
        format!("UPDATE \"{table}\" SET \"{deleted_at_name}\" = NULL WHERE \"{pk_name}\" = ?");
    let restore_sql_postgres =
        format!("UPDATE \"{table}\" SET \"{deleted_at_name}\" = NULL WHERE \"{pk_name}\" = $1");

    quote! {
        impl #struct_name {
            /// Permanently removes the row, bypassing `deleted_at`
            /// entirely (Eloquent's `forceDelete()`) - what plain
            /// `delete()` does on a model that *isn't* soft-deletable.
            pub async fn force_delete(
                #pk_ident: #pk_ty,
            ) -> ::std::result::Result<(), ::larust_support::AppError> {
                let __larust_sql = match ::larust_support::orm::backend() {
                    ::larust_support::orm::Backend::Sqlite
                    | ::larust_support::orm::Backend::MySql => #force_delete_sql,
                    ::larust_support::orm::Backend::Postgres => #force_delete_sql_postgres,
                };
                ::larust_support::orm::sqlx::query(__larust_sql)
                    .bind(#pk_ident)
                    .execute(::larust_support::orm::pool()?)
                    .await
                    .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))?;
                ::std::result::Result::Ok(())
            }

            /// Un-deletes a soft-deleted row (Eloquent's `restore()`).
            /// Not an error if the row wasn't trashed in the first place -
            /// same "not an error if it didn't need to do anything"
            /// posture `delete()` itself already has for a missing id.
            pub async fn restore(
                #pk_ident: #pk_ty,
            ) -> ::std::result::Result<(), ::larust_support::AppError> {
                let __larust_sql = match ::larust_support::orm::backend() {
                    ::larust_support::orm::Backend::Sqlite
                    | ::larust_support::orm::Backend::MySql => #restore_sql,
                    ::larust_support::orm::Backend::Postgres => #restore_sql_postgres,
                };
                ::larust_support::orm::sqlx::query(__larust_sql)
                    .bind(#pk_ident)
                    .execute(::larust_support::orm::pool()?)
                    .await
                    .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))?;
                ::std::result::Result::Ok(())
            }

            /// `find`, but also returns a soft-deleted row (Eloquent's
            /// `withTrashed()->find($id)`).
            pub async fn find_with_trashed(
                #pk_ident: #pk_ty,
            ) -> ::std::result::Result<::std::option::Option<Self>, ::larust_support::AppError>
            {
                Self::query()
                    .with_trashed()
                    .where_eq(Self::#pk_const_ident, #pk_ident)
                    .first()
                    .await
            }

            /// `true` if this row is currently soft-deleted.
            pub fn trashed(&self) -> bool {
                self.#deleted_at_ident.is_some()
            }
        }

        impl ::larust_support::orm::SoftDeletes for #struct_name {}
    }
}
