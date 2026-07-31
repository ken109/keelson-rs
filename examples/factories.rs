//! **Test data.** Factories that create whole parent chains, keep unique
//! columns unique, and fire the model's hooks.
//!
//!     cargo run -p keelson-examples --example factories
//!
//! `src/models/factories.rs` was generated alongside the models -- one
//! template per writable table, from the same foreign keys the models were
//! built from. `[output] factories = true` in `keelson.toml` asks for it; it
//! is off by default, because a production crate has no reason to carry
//! test-data machinery it never calls.
//!
//! The four decisions worth knowing:
//!
//! - **Factories insert through the model's own path,** so `before_insert`
//!   and `after_insert` run. A factory that bypassed hooks would manufacture
//!   rows the application could never have written.
//! - **A non-null foreign key auto-creates its parent** (recursively): a
//!   comment needs a post needs a user, and `create` makes all three. Each
//!   created row gets its *own* chain unless you say otherwise.
//! - **A nullable foreign key does not.** It stays NULL unless a mod opts in,
//!   so a factory never invents rows the schema does not require.
//! - **Unique columns come from a sequence, not the faker,** and sequences
//!   are deliberately outside the seed -- reproducing a primary key against a
//!   shared server would reproduce a collision.

use keelson::exec::{ExecError, Execute as _};
use keelson::factory::Faker;
use keelson::sqlite::{quote, select};
use keelson_examples::Sandbox;
use keelson_examples::models::factories as fac;
use keelson_examples::models::{posts, users};

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    // An empty database: everything below is created by the factories.
    let sandbox = Sandbox::empty().await?;
    let db = &sandbox.db;

    // ── one row, with defaults ──────────────────────────────────────────
    //
    // Mods again -- keelson's house style. `factory(())` takes the defaults.
    let ada = fac::users::factory((fac::users::name("Ada"), fac::users::age(36)))
        .create(db)
        .await?;
    println!("── one row\n  #{} {} age {:?}", ada.id, ada.name, ada.age);
    assert_eq!(ada.name, "Ada");

    // The `after_insert` hook ran, because the factory used the model's own
    // insert path.
    let audits: i64 = keelson::sqlite::select((
        select::columns(keelson::sqlite::f("count", quote("id"))),
        select::from(quote("audit_logs")),
    ))
    .fetch_scalar(db)
    .await?;
    assert_eq!(audits, 1, "the model's after_insert hook fired");

    // ── the parent chain ────────────────────────────────────────────────
    //
    // A comment needs a post, which needs a user. Neither was named, so both
    // were created from their own default templates.
    let comment = fac::comments::factory(fac::comments::body("first!"))
        .create(db)
        .await?;
    let post = posts::table()
        .query(posts::id().eq(comment.post_id))
        .one(db)
        .await?;
    println!(
        "\n── auto-created chain\n  comment {} → post {} → user {}",
        comment.id, post.id, post.user_id
    );
    assert_eq!(count(db, "users").await?, 2, "the chain made its own user");

    // ── sharing a parent ────────────────────────────────────────────────
    //
    // `create_many` gives each row its own chain, which is usually what a
    // test wants and occasionally not. To share, create the parent once and
    // pass it back in -- `post(&row)` for an existing row, `post_id(k)` for a
    // bare key, `for_post(template)` to shape the one that gets created.
    let shared = fac::posts::factory(fac::posts::user(&ada))
        .create(db)
        .await?;
    let batch = fac::comments::factory(fac::comments::post(&shared))
        .create_many(db, 5)
        .await?;
    println!(
        "\n── shared parent\n  {} comments, all on post {}",
        batch.len(),
        shared.id
    );
    assert!(batch.iter().all(|c| c.post_id == shared.id));

    // Each of *these* gets its own post, and so its own user.
    let independent = fac::comments::factory(()).create_many(db, 3).await?;
    let distinct: std::collections::BTreeSet<i64> = independent.iter().map(|c| c.post_id).collect();
    assert_eq!(distinct.len(), 3, "one chain each");

    // ── the nullable foreign key ────────────────────────────────────────
    //
    // `comments.user_id` is nullable, so the template has an `OptionalParent`
    // that is absent by default. Opting in is a mod.
    assert!(
        comment.user_id.is_none(),
        "a nullable parent is not invented"
    );
    let attributed = fac::comments::factory(fac::comments::user(&ada))
        .create(db)
        .await?;
    assert_eq!(attributed.user_id, Some(ada.id));
    println!(
        "\n── nullable parent\n  opted in: user_id = {:?}",
        attributed.user_id
    );

    // ── children, queued on the parent ──────────────────────────────────
    //
    // The other direction: a template can queue children, which are created
    // after the row itself exists.
    let prolific = fac::users::factory((
        fac::users::name("Grace"),
        fac::users::with_new_post(fac::posts::factory(fac::posts::title("Compilers"))),
        fac::users::with_new_post(fac::posts::factory(fac::posts::title("Bugs"))),
    ))
    .create(db)
    .await?;
    let theirs = users::table()
        .query((users::id().eq(prolific.id), users::then_load::posts()))
        .one(db)
        .await?;
    println!(
        "\n── queued children\n  {} has {} posts",
        theirs.name,
        theirs.rel.posts.len()
    );
    assert_eq!(theirs.rel.posts.len(), 2);

    // ── build(): no database at all ─────────────────────────────────────
    //
    // `build` takes no executor, so "this touches no database" is a fact
    // about the signature rather than a promise in the docs. It hands back
    // the `Setter` that `create` would have inserted.
    //
    // The consequence, recorded rather than hidden: a required parent whose
    // template would have to be *created* cannot be filled without a
    // database, so `build` leaves that column unset.
    let mut faker = Faker::seeded(42);
    let setter = fac::users::factory(fac::users::name("Offline")).build(&mut faker);
    println!("\n── build (no executor)\n  {setter:?}");

    // ── seeded runs reproduce ───────────────────────────────────────────
    //
    // Two fakers with the same seed draw the same random values...
    let a = fac::posts::factory(()).build(&mut Faker::seeded(7));
    let b = fac::posts::factory(()).build(&mut Faker::seeded(7));
    println!("  seeded title A: {:?}", a.title);
    println!("  seeded title B: {:?}", b.title);
    assert_eq!(format!("{:?}", a.title), format!("{:?}", b.title));

    // ...but the sequence-backed id is deliberately *not* seeded, so two
    // runs against a shared server do not collide on the primary key.
    assert_ne!(format!("{:?}", a.id), format!("{:?}", b.id));
    println!("  ids differ anyway: {:?} vs {:?}", a.id, b.id);

    println!("\nok");
    Ok(())
}

async fn count(db: &keelson::sqlx::sqlite::Pool, table: &'static str) -> Result<i64, ExecError> {
    keelson::sqlite::select((
        select::columns(keelson::sqlite::f("count", keelson::sqlite::raw("*"))),
        select::from(quote(table)),
    ))
    .fetch_scalar(db)
    .await
}
