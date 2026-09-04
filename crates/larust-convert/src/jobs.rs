//! `app/Jobs/*.php` → `#[derive(Serialize, Deserialize)] pub struct Name
//! { ... } impl Job for Name { const JOB_TYPE = "..."; async fn
//! handle(&self) -> Result<(), AppError> { todo!() } }`. Fields via
//! `constructor_props::extract`, same whole-item safety as `events.rs`.
//! `handle()`'s original body preserved as a comment via
//! `php::body_as_comment` - the same "preserve, never translate"
//! treatment as `controllers.rs`/`policies.rs`.
//!
//! **`JOB_TYPE` is always mechanically derived** as
//! `to_snake_case(struct_name)` (e.g. `notify_post_created_job`) - never
//! a hand-picked shorter slug. The real shipped `demo/app/Jobs/
//! notify_post_created_job.rs` example uses `"notify_post_created"`
//! (dropping the trailing `_job`), but that's hand-authored demo content
//! predating this phase, not a target to reproduce: mechanical
//! consistency (one deterministic rule, always) beats guessing at a
//! "nicer" name.

use crate::{codegen, constructor_props, php};

pub struct ConvertedJob {
    pub content: String,
}

/// `Ok(None)` if `class_name` isn't found in `source`. `Err` if the class
/// is found but its constructor has a field this phase can't type.
pub fn convert(source: &str, class_name: &str) -> Result<Option<ConvertedJob>, String> {
    let tree = php::parse(source).map_err(|error| error.to_string())?;
    if php::has_syntax_error(&tree) {
        return Err("file has a syntax error the parser couldn't recover from".to_string());
    }
    if php::find_class(&tree, source, class_name).is_none() {
        return Ok(None);
    }

    let fields = constructor_props::extract(&tree, source, class_name)?;
    let job_type = codegen::to_snake_case(class_name);

    let mut out = String::from(
        "use larust_support::queue::Job;\nuse serde::{Deserialize, Serialize};\n\n#[derive(Serialize, Deserialize)]\n",
    );
    out.push_str(&format!("pub struct {class_name} {{\n"));
    for field in &fields {
        out.push_str(&format!("    pub {}: {},\n", field.name, field.rust_type));
    }
    out.push_str("}\n\n");

    out.push_str(&format!("impl Job for {class_name} {{\n"));
    out.push_str(&format!(
        "    const JOB_TYPE: &'static str = \"{job_type}\";\n\n"
    ));
    if let Some(body) = php::find_method(&tree, source, class_name, "handle")
        .and_then(|m| m.child_by_field_name("body"))
    {
        out.push_str(&php::body_as_comment(body, source));
    }
    out.push_str("    async fn handle(&self) -> Result<(), larust_support::AppError> {\n        todo!()\n    }\n");
    out.push_str("}\n");

    Ok(Some(ConvertedJob { content: out }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_job_with_a_promoted_field_and_preserves_handle_body() {
        let source = "<?php\nclass NotifyPostCreatedJob\n{\n    public function __construct(public int $postId) {}\n\n    public function handle(): void\n    {\n        Log::info(\"Post {$this->postId} created\");\n    }\n}\n";
        let result = convert(source, "NotifyPostCreatedJob").unwrap().unwrap();
        assert!(result.content.contains("#[derive(Serialize, Deserialize)]"));
        assert!(result.content.contains("pub post_id"));
        assert!(result
            .content
            .contains("impl Job for NotifyPostCreatedJob {"));
        assert!(result
            .content
            .contains("const JOB_TYPE: &'static str = \"notify_post_created_job\";"));
        assert!(result.content.contains("// Log::info"));
        assert!(result
            .content
            .contains("async fn handle(&self) -> Result<(), larust_support::AppError> {"));
        assert!(result.content.contains("todo!()"));
    }

    #[test]
    fn job_type_is_always_mechanically_derived_never_a_hand_picked_slug() {
        let source =
            "<?php\nclass SendWelcomeEmailJob\n{\n    public function handle(): void {}\n}\n";
        let result = convert(source, "SendWelcomeEmailJob").unwrap().unwrap();
        assert!(result
            .content
            .contains("const JOB_TYPE: &'static str = \"send_welcome_email_job\";"));
    }

    #[test]
    fn returns_none_when_the_class_is_not_found() {
        let source = "<?php\nclass Foo {}\n";
        assert!(convert(source, "Bar").unwrap().is_none());
    }

    #[test]
    fn rejects_the_whole_job_when_a_field_type_is_unsupported() {
        let source = "<?php\nclass X\n{\n    public function __construct(public Order|string $order) {}\n}\n";
        assert!(convert(source, "X").is_err());
    }
}
