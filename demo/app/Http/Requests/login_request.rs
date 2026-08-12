use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct LoginRequest {
    #[validate(required, email)]
    pub email: String,
    #[validate(required)]
    pub password: String,
}
