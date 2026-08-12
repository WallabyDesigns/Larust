use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct StorePostRequest {
    #[validate(required, length(max = 255))]
    pub title: String,
}
