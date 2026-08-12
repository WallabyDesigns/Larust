use crate::models::{Post, User};
use larust_support::auth::Policy;

/// Public blog: anyone can browse, and any logged-in user can start a
/// post — only the author of a specific post may edit or delete it.
impl Policy<User> for Post {
    fn view_any(_user: &User) -> bool {
        true
    }

    fn view(&self, _user: &User) -> bool {
        true
    }

    fn create(_user: &User) -> bool {
        true
    }

    fn update(&self, user: &User) -> bool {
        self.user_id == user.id
    }

    fn delete(&self, user: &User) -> bool {
        self.user_id == user.id
    }
}
