use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct StoreCommentRequest {
    #[validate(required, length(max = 2000))]
    pub body: String,
}
