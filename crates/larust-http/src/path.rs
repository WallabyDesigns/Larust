/// Translates Laravel-style `{param}` path segments into axum's `:param`
/// segments, so the framework's public route syntax stays Laravel-shaped
/// regardless of what the underlying router expects.
pub fn to_axum_path(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut chars = path.chars();

    while let Some(c) = chars.next() {
        if c == '{' {
            result.push(':');
            for c in chars.by_ref() {
                if c == '}' {
                    break;
                }
                result.push(c);
            }
        } else {
            result.push(c);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_static_paths_unchanged() {
        assert_eq!(to_axum_path("/posts"), "/posts");
        assert_eq!(to_axum_path("/"), "/");
    }

    #[test]
    fn translates_single_param() {
        assert_eq!(to_axum_path("/posts/{post}"), "/posts/:post");
    }

    #[test]
    fn translates_multiple_params() {
        assert_eq!(
            to_axum_path("/users/{user}/posts/{post}"),
            "/users/:user/posts/:post"
        );
    }
}
