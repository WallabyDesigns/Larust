use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct UpdatePasswordRequest {
    #[validate(required)]
    pub current_password: String,
    #[validate(required, length(min = 8), confirmed)]
    pub password: String,
}
