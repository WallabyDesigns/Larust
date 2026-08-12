use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct RegisterRequest {
    #[validate(required, length(max = 255))]
    pub name: String,
    #[validate(required, email)]
    pub email: String,
    #[validate(required, length(min = 8), confirmed)]
    pub password: String,
}
