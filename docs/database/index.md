---
title: Database
nav_order: 6
has_children: true
---

# Database

## Configuration

```
# .env
DB_CONNECTION=sqlite
# DB_HOST=127.0.0.1
# DB_PORT=3306
# DB_DATABASE=larust
# DB_USERNAME=root
# DB_PASSWORD=
# DB_CHARSET=utf8mb4
```

`DB_CONNECTION` selects one of five named connections declared in
`config/database.rs` (a plain Rust function, not a parsed file - see
[Project Structure](../getting-started/project-structure#config)):

| `DB_CONNECTION` | Driver | Notes |
|---|---|---|
| `sqlite` (default) | SQLite | `DB_DATABASE` is a file path (default `database/database.sqlite`), not a server database name. No server to run - this is what `xr new`/`migrate` work with out of the box. |
| `mysql` | MySQL | |
| `mariadb` | MySQL (same wire protocol) | |
| `pgsql` | Postgres | |
| `sqlsrv` | *Not connectable through this framework's ORM* | See below |

Every real query path - `#[derive(Model)]`'s generated methods, the
`QueryBuilder`, migrations - runs through `sqlx::AnyPool`, which is what
lets the exact same generated code work against SQLite, MySQL, or
Postgres without a separate code path per backend. **SQL Server is the
one exception**: `sqlx` has no SQL Server driver at all, so SQL Server
support (`larust-mssql`) is a separately hand-maintained path built on
[`tiberius`](https://github.com/prisma/tiberius) instead, implementing
the same `Repository<T>` contract `#[derive(Model)]` generates for the
other three (see [`examples/repository_bench`](https://github.com/wallabydesigns/Larust/tree/main/examples/repository_bench)
for all four backends proven against identical break/load tests) - but it
sits outside `AnyPool`, so newer features built directly against it
(the [SQL admin dashboard](../digging-deeper/key-value-store-and-sitemaps#database-admin-dashboard),
for one) don't cover SQL Server yet.

A plain `xr new` app's dependency tree only ever includes the *one*
sqlx backend `DB_CONNECTION` actually needs (confirmed via a `cargo tree`
regression test in this framework's own test suite) - switching backends
means updating both `.env` and the `sqlx` feature in your `Cargo.toml`,
not just an env var.

## In this section

1. [Migrations & the Query Builder](migrations-and-query-builder)
2. [Models & Relationships](models-and-relationships)
