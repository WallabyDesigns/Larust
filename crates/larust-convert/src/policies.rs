//! `app/Policies/*.php` → `impl Policy<User> for Model` — mirrors
//! `xr make:policy`'s own `POLICY_TEMPLATE` exactly (all 5 abilities
//! stubbed `false`, deny-by-default), with each Laravel ability method's
//! original body preserved as a comment above its stub, the same
//! "preserve, never translate" treatment as `controllers.rs`.

use crate::php;

pub struct ConvertedPolicy {
    /// The model type name (`PostPolicy` → `Post`), used both for the
    /// output filename and the `impl Policy<User> for {model}` line.
    pub model_name: String,
    pub content: String,
}

/// Laravel's camelCase ability method name -> `Policy<U>`'s snake_case
/// one, in the fixed order every ability is emitted.
const ABILITIES: &[(&str, &str)] = &[
    ("viewAny", "view_any"),
    ("view", "view"),
    ("create", "create"),
    ("update", "update"),
    ("delete", "delete"),
];

/// `Ok(None)` if `class_name` isn't found in `source` at all — not every
/// file under `app/Policies/` is guaranteed to match what the caller
/// expects. `Err` for a class that *is* found but whose name doesn't end
/// in `Policy` (nothing to infer the model name from) or has a syntax
/// error.
pub fn convert(
    source: &str,
    class_name: &str,
    user_type: &str,
) -> Result<Option<ConvertedPolicy>, String> {
    let tree = php::parse(source).map_err(|error| error.to_string())?;
    if php::has_syntax_error(&tree) {
        return Err("file has a syntax error the parser couldn't recover from".to_string());
    }
    if php::find_class(&tree, source, class_name).is_none() {
        return Ok(None);
    }
    let Some(model_name) = class_name.strip_suffix("Policy") else {
        return Err(format!(
            "policy class `{class_name}` doesn't end in `Policy`; can't infer the model type name"
        ));
    };

    let imports = if model_name == user_type {
        model_name.to_string()
    } else {
        format!("{{{model_name}, {user_type}}}")
    };

    let mut out = format!(
        "use crate::models::{imports};\nuse larust_support::auth::Policy;\n\nimpl Policy<{user_type}> for {model_name} {{\n"
    );
    for (laravel_name, larust_name) in ABILITIES {
        if let Some(body) = php::find_method(&tree, source, class_name, laravel_name)
            .and_then(|m| m.child_by_field_name("body"))
        {
            out.push_str(&php::body_as_comment(body, source));
        }
        out.push_str(&render_ability_stub(larust_name, user_type));
    }
    out.push_str("}\n");

    Ok(Some(ConvertedPolicy {
        model_name: model_name.to_string(),
        content: out,
    }))
}

fn render_ability_stub(larust_name: &str, user_type: &str) -> String {
    match larust_name {
        "view_any" => {
            format!("    fn view_any(_user: &{user_type}) -> bool {{\n        false\n    }}\n\n")
        }
        "view" => {
            format!("    fn view(&self, _user: &{user_type}) -> bool {{\n        false\n    }}\n\n")
        }
        "create" => {
            format!("    fn create(_user: &{user_type}) -> bool {{\n        false\n    }}\n\n")
        }
        "update" => format!(
            "    fn update(&self, _user: &{user_type}) -> bool {{\n        false\n    }}\n\n"
        ),
        "delete" => format!(
            "    fn delete(&self, _user: &{user_type}) -> bool {{\n        false\n    }}\n\n"
        ),
        other => unreachable!("every ABILITIES entry is handled above, got `{other}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_policy_with_all_five_abilities_stubbed_false() {
        let source = "<?php\nclass PostPolicy\n{\n    public function viewAny(User $user): bool { return true; }\n    public function view(User $user, Post $post): bool { return true; }\n    public function create(User $user): bool { return true; }\n    public function update(User $user, Post $post): bool { return $post->user_id === $user->id; }\n    public function delete(User $user, Post $post): bool { return $post->user_id === $user->id; }\n}\n";
        let result = convert(source, "PostPolicy", "User").unwrap().unwrap();
        assert_eq!(result.model_name, "Post");
        assert!(result.content.contains("impl Policy<User> for Post {"));
        assert!(result
            .content
            .contains("fn view_any(_user: &User) -> bool {\n        false"));
        assert!(result
            .content
            .contains("fn update(&self, _user: &User) -> bool {\n        false"));
        assert!(result
            .content
            .contains("// return $post->user_id === $user->id;"));
    }

    #[test]
    fn dedupes_the_import_when_model_and_user_are_the_same_type() {
        let source = "<?php\nclass UserPolicy {}\n";
        let result = convert(source, "UserPolicy", "User").unwrap().unwrap();
        assert!(result.content.contains("use crate::models::User;"));
        assert!(!result.content.contains("User, User"));
    }

    #[test]
    fn rejects_a_class_name_not_ending_in_policy() {
        let source = "<?php\nclass PostRules {}\n";
        assert!(convert(source, "PostRules", "User").is_err());
    }

    #[test]
    fn returns_none_when_the_class_is_not_found() {
        let source = "<?php\nclass UserPolicy {}\n";
        assert!(convert(source, "PostPolicy", "User").unwrap().is_none());
    }
}
