use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct UpdateProfileRequest {
    #[validate(required, length(max = 255))]
    pub name: String,
    #[validate(required, email)]
    pub email: String,
}
