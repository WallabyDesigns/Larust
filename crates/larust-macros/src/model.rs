use crate::belongs_to_many;
use crate::relations;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields};

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let struct_name = &input.ident;
    let table = table_attr(&input)?;
    let route_key = route_key_attr(&input)?;

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input,
            "#[derive(Model)] only supports structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "#[derive(Model)] requires named fields",
        ));
    };

    let mut primary_key: Option<(&syn::Ident, &syn::Type)> = None;
    let mut insertable: Vec<(&syn::Ident, &syn::Type)> = Vec::new();
    let mut all_fields: Vec<&syn::Ident> = Vec::new();
    let mut all_fields_with_types: Vec<(&syn::Ident, &syn::Type)> = Vec::new();

    for field in &fields.named {
        let ident = field.ident.as_ref().expect("named field has an ident");
        all_fields.push(ident);
        all_fields_with_types.push((ident, &field.ty));

        if field.attrs.iter().any(|a| a.path().is_ident("primary_key")) {
            if primary_key.is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "#[derive(Model)] only supports a single #[primary_key] field",
                ));
            }
            primary_key = Some((ident, &field.ty));
        } else {
            insertable.push((ident, &field.ty));
        }
    }

    let Some((pk_ident, pk_ty)) = primary_key else {
        return Err(syn::Error::new_spanned(
            &input,
            "#[derive(Model)] requires exactly one field marked #[primary_key]",
        ));
    };
    if !is_i64_type(pk_ty) {
        return Err(syn::Error::new_spanned(
            pk_ty,
            "#[primary_key] field must be `i64` (other key types land in a later milestone)",
        ));
    }

    let field_consts = all_fields.iter().map(|ident| {
        let name = field_name_str(ident);
        let const_ident = format_ident!("{}", name.to_uppercase());
        quote! { pub const #const_ident: &'static str = #name; }
    });
    let pk_name = field_name_str(pk_ident);
    let pk_const_ident = format_ident!("{}", pk_name.to_uppercase());

    let new_struct_ident = format_ident!("New{struct_name}");
    let new_fields = insertable
        .iter()
        .map(|(ident, ty)| quote! { pub #ident: #ty });

    // Every identifier below is quoted (`"..."`) because it's always a
    // developer-controlled name (the `#[table("...")]` literal or a struct
    // field name) rather than data — quoting protects against it colliding
    // with a SQL reserved keyword (a field named `order` or a table named
    // `group` are both real possibilities), not against injection.
    let insertable_names: Vec<String> = insertable.iter().map(|(i, _)| field_name_str(i)).collect();
    let insert_binds = insertable
        .iter()
        .map(|(ident, _)| quote! { .bind(data.#ident) });
    let insert_sql = if insertable_names.is_empty() {
        format!("INSERT INTO \"{table}\" DEFAULT VALUES RETURNING *")
    } else {
        let insert_columns = insertable_names
            .iter()
            .map(|n| format!("\"{n}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let insert_placeholders = insertable_names
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO \"{table}\" ({insert_columns}) VALUES ({insert_placeholders}) RETURNING *"
        )
    };

    let delete_sql = format!("DELETE FROM \"{table}\" WHERE \"{pk_name}\" = ?");
    let new_struct_doc =
        format!("Insertable fields for `{struct_name}` (everything except the primary key).");

    // Route model binding: `pub async fn show(post: Post)` on a route
    // declared `/posts/{post}` — the path parameter name is the
    // snake_case'd struct name (Laravel's own convention), and the lookup
    // column is the primary key by default or whatever `#[route_key("...")]`
    // names, validated against the struct's actual fields.
    let route_param_name = to_snake_case(&struct_name.to_string());
    let lookup = match &route_key {
        Some(column) => {
            if !all_fields.iter().any(|f| field_name_str(f) == *column) {
                return Err(syn::Error::new_spanned(
                    &input,
                    format!("#[route_key(\"{column}\")] does not match any field on this struct"),
                ));
            }
            let const_ident = format_ident!("{}", column.to_uppercase());
            quote! { Self::query().where_eq(Self::#const_ident, raw.clone()).first().await? }
        }
        None => quote! {
            {
                let parsed: i64 = match raw.parse() {
                    Ok(v) => v,
                    Err(_) => return ::std::result::Result::Err(::larust_support::AppError::NotFound),
                };
                Self::find(parsed).await?
            }
        },
    };

    let relations = relations::expand(&input, &all_fields_with_types, pk_ident)?;
    let belongs_to_many_relations = belongs_to_many::expand(&input, pk_ident)?;

    Ok(quote! {
        #[doc = #new_struct_doc]
        pub struct #new_struct_ident {
            #(#new_fields,)*
        }

        #relations
        #belongs_to_many_relations

        impl #struct_name {
            #(#field_consts)*
            pub const TABLE: &'static str = #table;

            pub fn query() -> ::larust_support::orm::QueryBuilder<Self> {
                ::larust_support::orm::QueryBuilder::new(Self::TABLE)
            }

            pub async fn all() -> ::std::result::Result<::std::vec::Vec<Self>, ::larust_support::AppError> {
                Self::query().get().await
            }

            pub async fn find(
                #pk_ident: #pk_ty,
            ) -> ::std::result::Result<::std::option::Option<Self>, ::larust_support::AppError> {
                Self::query().where_eq(Self::#pk_const_ident, #pk_ident).first().await
            }

            pub async fn create(
                data: #new_struct_ident,
            ) -> ::std::result::Result<Self, ::larust_support::AppError> {
                ::larust_support::orm::sqlx::query_as::<_, Self>(#insert_sql)
                    #(#insert_binds)*
                    .fetch_one(::larust_support::orm::pool()?)
                    .await
                    .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))
            }

            pub async fn delete(
                #pk_ident: #pk_ty,
            ) -> ::std::result::Result<(), ::larust_support::AppError> {
                ::larust_support::orm::sqlx::query(#delete_sql)
                    .bind(#pk_ident)
                    .execute(::larust_support::orm::pool()?)
                    .await
                    .map_err(|e| ::larust_support::AppError::Internal(::std::boxed::Box::new(e)))?;
                ::std::result::Result::Ok(())
            }
        }

        #[::larust_support::axum::async_trait]
        impl<S: ::std::marker::Send + ::std::marker::Sync>
            ::larust_support::axum::extract::FromRequestParts<S> for #struct_name
        {
            type Rejection = ::larust_support::AppError;

            async fn from_request_parts(
                parts: &mut ::larust_support::axum::http::request::Parts,
                state: &S,
            ) -> ::std::result::Result<Self, Self::Rejection> {
                let ::larust_support::axum::extract::Path(params) = <
                    ::larust_support::axum::extract::Path<
                        ::std::collections::HashMap<::std::string::String, ::std::string::String>
                    > as ::larust_support::axum::extract::FromRequestParts<S>
                >::from_request_parts(parts, state)
                    .await
                    .map_err(|_| ::larust_support::AppError::NotFound)?;

                let raw = params
                    .get(#route_param_name)
                    .ok_or(::larust_support::AppError::NotFound)?;

                let found: ::std::option::Option<Self> = #lookup;
                found.ok_or(::larust_support::AppError::NotFound)
            }
        }
    })
}

fn table_attr(input: &DeriveInput) -> syn::Result<String> {
    for attr in &input.attrs {
        if attr.path().is_ident("table") {
            let lit: syn::LitStr = attr.parse_args()?;
            return Ok(lit.value());
        }
    }
    Err(syn::Error::new_spanned(
        input,
        "#[derive(Model)] requires a #[table(\"...\")] attribute",
    ))
}

pub(crate) fn is_i64_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "i64"))
}

/// `#[route_key("slug")]` — which field route model binding looks records
/// up by, instead of the primary key. Optional.
fn route_key_attr(input: &DeriveInput) -> syn::Result<Option<String>> {
    for attr in &input.attrs {
        if attr.path().is_ident("route_key") {
            let lit: syn::LitStr = attr.parse_args()?;
            return Ok(Some(lit.value()));
        }
    }
    Ok(None)
}

/// `Post` -> `"post"`, `BlogPost` -> `"blog_post"` — the default route
/// parameter name route model binding looks for (Laravel's own convention:
/// the path segment name matches the lowercased model name). Also used by
/// `relations.rs` to derive a relationship method's default name from its
/// related type.
pub(crate) fn to_snake_case(name: &str) -> String {
    let mut result = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                result.push('_');
            }
            result.extend(c.to_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// Strips a raw-identifier prefix (`r#type` -> `type`) — this is used both
/// as the SQL column name and as the basis for a generated `CONST_NAME`,
/// neither of which should carry Rust's own keyword-escaping syntax.
/// Without this, a field named `r#type` panics `format_ident!` (rather
/// than a clean `syn::Error`) and the wrong string (`"r#type"`) would end
/// up as both the constant's value and the actual SQL column name.
pub(crate) fn field_name_str(ident: &syn::Ident) -> String {
    ident.to_string().trim_start_matches("r#").to_string()
}

#[cfg(test)]
mod tests {
    use super::to_snake_case;

    #[test]
    fn single_word_lowercases() {
        assert_eq!(to_snake_case("Post"), "post");
    }

    #[test]
    fn multi_word_inserts_underscores() {
        assert_eq!(to_snake_case("BlogPost"), "blog_post");
        assert_eq!(to_snake_case("UserProfileSetting"), "user_profile_setting");
    }
}
