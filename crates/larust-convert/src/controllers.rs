//! `app/Http/Controllers/*.php` → enriches Phase 1's already-generated
//! `todo!()` controller stubs (`convert.rs`'s `generate_controller_stubs`,
//! driven by `routes::referenced_controllers`) with each stubbed method's
//! **original PHP body preserved as a comment** — no logic translation,
//! no smarter parameter typing (both explicitly out of scope, natural
//! later enhancements). If the real source controller file doesn't exist,
//! or a specific stubbed method isn't found in it, that method just gets
//! Phase 1's existing bare stub — this module only ever adds a comment,
//! never removes Phase 1's own guarantee that every referenced method
//! compiles.

use crate::php;

pub struct ConvertedController {
    pub content: String,
}

/// `methods` is the exact `(controller, [methods])` list Phase 1 already
/// computed (`routes::referenced_controllers`) — this module doesn't
/// re-derive which methods matter, only enriches their stub bodies.
pub fn convert(
    source: &str,
    class_name: &str,
    methods: &[String],
) -> Result<ConvertedController, String> {
    let tree = php::parse(source).map_err(|error| error.to_string())?;
    if php::has_syntax_error(&tree) {
        return Err(format!(
            "{source_len} bytes read, but the file has a syntax error the parser couldn't recover from",
            source_len = source.len()
        ));
    }
    if php::find_class(&tree, source, class_name).is_none() {
        return Err(format!("class `{class_name}` not found in this file"));
    }

    let mut out = format!("pub struct {class_name};\n\nimpl {class_name} {{\n");
    for method in methods {
        if let Some(body) = php::find_method(&tree, source, class_name, method)
            .and_then(|m| m.child_by_field_name("body"))
        {
            out.push_str(&php::body_as_comment(body, source));
        }
        out.push_str(&format!(
            "    pub async fn {method}() -> &'static str {{\n        todo!()\n    }}\n\n"
        ));
    }
    out.push_str("}\n");
    Ok(ConvertedController { content: out })
}

/// The original PHP method body, verbatim, as a comment block directly
/// above the generated stub — a reference for whoever ports the real
/// logic by hand, never translated itself.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_a_method_body_as_a_comment_above_the_stub() {
        let source = "<?php\nclass PostController extends Controller\n{\n    public function index()\n    {\n        return view('posts.index', ['posts' => Post::all()]);\n    }\n}\n";
        let result = convert(source, "PostController", &["index".to_string()]).unwrap();
        assert!(result.content.contains("// Original Laravel method body"));
        assert!(result.content.contains("// return view('posts.index'"));
        assert!(result
            .content
            .contains("pub async fn index() -> &'static str {"));
        assert!(result.content.contains("todo!()"));
    }

    #[test]
    fn a_stubbed_method_not_found_in_source_still_gets_a_bare_stub() {
        let source = "<?php\nclass PostController extends Controller\n{\n    public function index() {}\n}\n";
        let result = convert(
            source,
            "PostController",
            &["index".to_string(), "show".to_string()],
        )
        .unwrap();
        assert!(result.content.contains("pub async fn index()"));
        assert!(result.content.contains("pub async fn show()"));
    }

    #[test]
    fn rejects_when_the_class_is_not_found_in_the_file() {
        let source = "<?php\nclass UserController extends Controller {}\n";
        assert!(convert(source, "PostController", &["index".to_string()]).is_err());
    }

    #[test]
    fn preserves_bodies_for_multiple_methods_independently() {
        let source = "<?php\nclass PostController extends Controller\n{\n    public function index()\n    {\n        return Post::all();\n    }\n\n    public function show(Post $post)\n    {\n        return $post;\n    }\n}\n";
        let result = convert(
            source,
            "PostController",
            &["index".to_string(), "show".to_string()],
        )
        .unwrap();
        assert!(result.content.contains("// return Post::all();"));
        assert!(result.content.contains("// return $post;"));
    }
}
