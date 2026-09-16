use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct LanguageRequest {
    #[validate(required)]
    pub locale: String,
}
