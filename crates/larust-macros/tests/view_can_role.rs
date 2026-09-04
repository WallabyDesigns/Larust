//! End-to-end proof that `@can(...)`/`@role(...)` work through the real
//! `view!` macro pipeline (parse -> resolve -> codegen ->
//! `larust_permissions::has_permission_to`/`has_role`), mirroring
//! `view_wire.rs`'s reasoning: `larust-view`'s own parser unit tests pin the
//! AST shape in isolation; this is what actually catches a regression in
//! `codegen_node`'s `Node::Can`/`Node::Role` arms or the eager missing-
//! `user` binding check in `expand()`.

use larust_support::auth::Authenticatable;
use larust_support::permission::{
    assign_role, create_permission, create_role, grant_role_permission, PermissionName, RoleName,
};
use larust_support::view;
use larust_support::AppError;

struct TestUser {
    id: i64,
}

impl Authenticatable for TestUser {
    fn auth_id(&self) -> i64 {
        self.id
    }

    async fn find_for_auth(_id: i64) -> Result<Option<Self>, AppError> {
        unreachable!("not exercised by this test")
    }
}

#[derive(Copy, Clone)]
enum Permission {
    EditPosts,
}

impl PermissionName for Permission {
    fn name(&self) -> &'static str {
        match self {
            Permission::EditPosts => "edit-posts",
        }
    }
}

#[derive(Copy, Clone)]
enum Role {
    Admin,
}

impl RoleName for Role {
    fn name(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
        }
    }
}

async fn render(user: &TestUser) -> Result<String, AppError> {
    let view = view!("can_role_test", { user });
    Ok(view.into_html())
}

#[tokio::test]
async fn can_and_role_directives_check_the_real_permission_and_role_assignment() {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();

    create_permission(Permission::EditPosts).await.unwrap();
    create_role(Role::Admin).await.unwrap();
    grant_role_permission(Role::Admin, Permission::EditPosts)
        .await
        .unwrap();

    // Alice: no permission, no role - both directives take their "no"
    // path (@can's @else branch; @role renders nothing).
    let alice = TestUser { id: 1 };
    let html = render(&alice).await.unwrap();
    assert_eq!(html.trim(), "<div><span>readonly</span> </div>");

    // Bob: assigned the admin role, which was granted edit-posts above -
    // @can resolves through the role (not a direct grant), and @role sees
    // the role directly.
    let bob = TestUser { id: 2 };
    assign_role(&bob, Role::Admin).await.unwrap();
    let html = render(&bob).await.unwrap();
    assert_eq!(
        html.trim(),
        "<div><span>editable</span> <span>admin-badge</span></div>"
    );
}
