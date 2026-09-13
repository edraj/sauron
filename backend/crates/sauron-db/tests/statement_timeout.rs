//! A pool built with a statement timeout hands out connections whose queries
//! Postgres cancels on its own once the budget is spent.
//!
//! Why this matters: the API's request layer gives up after its timeout and
//! answers 503, but the Postgres backend keeps executing the abandoned query
//! to completion — measured at 30M rows: a device-groups query 503'd at 60 s
//! and ran 182 s more, still holding its pool slot. A few dashboard reloads
//! then fill every slot with orphans and every endpoint fails, cheap ones
//! included. Bounding the statement server-side is what makes a timeout
//! actually release the resources.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common`.

mod common;

use common::TestDb;
use diesel::sql_types::Text;
use diesel_async::RunQueryDsl;

#[derive(diesel::QueryableByName)]
struct Setting {
    #[diesel(sql_type = Text)]
    statement_timeout: String,
}

async fn show_timeout(conn: &mut sauron_db::PgConn) -> String {
    diesel::sql_query("SHOW statement_timeout")
        .get_result::<Setting>(conn)
        .await
        .expect("SHOW statement_timeout")
        .statement_timeout
}

#[tokio::test]
async fn bounded_pool_connections_carry_the_timeout_and_cancel_long_statements() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };

    // The harness pool is unbounded: the server default is 0 (off).
    let mut plain = db.conn().await;
    assert_eq!(show_timeout(&mut plain).await, "0");
    drop(plain);

    let bounded = sauron_db::build_pool_with_statement_timeout(&db.database_url(), 2, 250)
        .expect("build bounded pool");
    let mut conn = sauron_db::conn(&bounded).await.expect("checkout");
    assert_eq!(show_timeout(&mut conn).await, "250ms");

    let err = diesel::sql_query("SELECT pg_sleep(2)")
        .execute(&mut conn)
        .await
        .expect_err("a 2 s statement must be cancelled by a 250 ms budget");
    let msg = err.to_string();
    assert!(
        msg.contains("statement timeout"),
        "expected a statement-timeout cancellation, got: {msg}"
    );
    assert!(
        sauron_db::is_statement_timeout(&err),
        "classifier must recognise the cancellation"
    );

    // The budget is a session default, so RESET (which background work uses
    // after a temporary SET) lands back on it — never on "off".
    diesel::sql_query("SET statement_timeout = 0")
        .execute(&mut conn)
        .await
        .expect("set");
    diesel::sql_query("RESET statement_timeout")
        .execute(&mut conn)
        .await
        .expect("reset");
    assert_eq!(show_timeout(&mut conn).await, "250ms");
    drop(conn);
    db.cleanup().await;
}

#[test]
fn options_are_appended_to_urls_with_and_without_a_query_string() {
    assert_eq!(
        sauron_db::pool::url_with_statement_timeout("postgres://u:p@h:5432/db", 60_000),
        "postgres://u:p@h:5432/db?options=-c%20statement_timeout%3D60000"
    );
    assert_eq!(
        sauron_db::pool::url_with_statement_timeout("postgres://u:p@h/db?sslmode=require", 5),
        "postgres://u:p@h/db?sslmode=require&options=-c%20statement_timeout%3D5"
    );
}
