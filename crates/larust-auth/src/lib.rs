//! Session-backed authentication and lightweight authorization — Laravel's
//! `Auth` facade, `auth`/`guest` middleware, and `authorize()` helper,
//! re-exported through `larust_support::auth` (see
//! `crates/larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly.

mod authenticatable;
mod authorize;
mod extractor;
mod guard;
mod hash;
mod middleware;
mod policy;

pub use authenticatable::Authenticatable;
pub use authorize::authorize;
pub use extractor::Auth;
pub use guard::{check, id, login, logout, user};
pub use hash::{hash_password, verify_password};
pub use middleware::{redirect_authenticated, require_auth};
pub use policy::Policy;
