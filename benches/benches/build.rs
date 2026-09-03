//! What `Query::build()` costs.
//!
//! keelson's pitch for Layer 1 is that building is synchronous, driver-free
//! and inspectable — you can look at the `(String, Vec<Value>)` before anything
//! touches a socket. That is a claim about a hot path in an application's
//! request handler, and nothing else in this workspace measures it: the test
//! tiers prove the SQL is *right*, and a rendering change that quietly doubles
//! the allocations would pass every one of them.
//!
//! So this exists to be compared against itself over time. It is not a
//! comparison against other query builders — a benchmark that flatters its
//! author is not evidence — and it is not part of the merge gates, because a
//! shared CI runner cannot tell a regression from a noisy neighbour. Run it on
//! a machine you control, before and after:
//!
//! ```text
//! git stash && cargo bench -p keelson-benches -- --save-baseline before
//! git stash pop && cargo bench -p keelson-benches -- --baseline before
//! ```
//!
//! The cases are the shapes that stress different parts of the writer: a
//! typical statement (what an application actually builds), a wide `IN` list
//! (argument-vector growth), deep nesting (recursion and the placeholder
//! counter), a multi-row `INSERT` (the widest argument vectors keelson emits),
//! and the same typical statement across all three dialects (per-dialect
//! quoting and placeholder rendering, which is the only part that differs).

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use keelson_psql::{Chain as _, Query as _, arg, insert, quote, select};

/// The statement an application writes: a few columns, a filter, an order and
/// a limit.
fn typical_psql() -> keelson_psql::SelectQuery {
    keelson_psql::select((
        select::columns((quote("id"), quote("name"), quote("email"))),
        select::from(quote("users")),
        select::where_(quote("age").gte(arg(21))),
        select::where_(quote("is_active").eq(arg(true))),
        select::order_by(quote("created_at")).desc(),
        select::limit(arg(20)),
    ))
}

fn typical(c: &mut Criterion) {
    let mut group = c.benchmark_group("typical select");

    group.bench_function("psql", |b| {
        b.iter(|| black_box(typical_psql().build().unwrap()))
    });

    group.bench_function("sqlite", |b| {
        use keelson_sqlite::{Chain as _, arg, quote, select};
        b.iter(|| {
            black_box(
                keelson_sqlite::select((
                    select::columns((quote("id"), quote("name"), quote("email"))),
                    select::from(quote("users")),
                    select::where_(quote("age").gte(arg(21))),
                    select::where_(quote("is_active").eq(arg(true))),
                    select::order_by(quote("created_at")).desc(),
                    select::limit(arg(20)),
                ))
                .build()
                .unwrap(),
            )
        })
    });

    group.bench_function("mysql", |b| {
        use keelson_mysql::{Chain as _, arg, quote, select};
        b.iter(|| {
            black_box(
                keelson_mysql::select((
                    select::columns((quote("id"), quote("name"), quote("email"))),
                    select::from(quote("users")),
                    select::where_(quote("age").gte(arg(21))),
                    select::where_(quote("is_active").eq(arg(true))),
                    select::order_by(quote("created_at")).desc(),
                    select::limit(arg(20)),
                ))
                .build()
                .unwrap(),
            )
        })
    });

    group.finish();
}

/// Argument-vector growth: one `IN` list, N bound values.
fn wide_in_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("in list");
    for n in [8usize, 64, 512] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let ids: Vec<i64> = (0..n as i64).collect();
            b.iter(|| {
                let q = keelson_psql::select((
                    select::columns(quote("id")),
                    select::from(quote("users")),
                    select::where_(quote("id").in_(keelson_psql::args(ids.iter().copied()))),
                ));
                black_box(q.build().unwrap())
            })
        });
    }
    group.finish();
}

/// Recursion and the cross-level placeholder counter: a predicate nested N
/// levels deep, each level binding a value.
fn nested_predicate(c: &mut Criterion) {
    let mut group = c.benchmark_group("nested predicate");
    for depth in [4usize, 16, 64] {
        group.throughput(Throughput::Elements(depth as u64));
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, &depth| {
            b.iter(|| {
                let mut pred = quote("age").gte(arg(0i64));
                for i in 1..depth {
                    pred = pred.and(quote("age").lt(arg(i as i64)));
                }
                let q = keelson_psql::select((
                    select::columns(quote("id")),
                    select::from(quote("users")),
                    select::where_(pred),
                ));
                black_box(q.build().unwrap())
            })
        });
    }
    group.finish();
}

/// The widest argument vectors keelson emits: a multi-row `INSERT`.
fn multi_row_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi-row insert");
    for rows in [1usize, 32, 256] {
        group.throughput(Throughput::Elements(rows as u64));
        group.bench_with_input(BenchmarkId::from_parameter(rows), &rows, |b, &rows| {
            b.iter(|| {
                let mut q = keelson_psql::insert(insert::into("users").columns(["name", "age"]));
                for i in 0..rows {
                    keelson_psql::Mod::apply(
                        insert::values((arg(format!("user{i}")), arg(i as i64))),
                        &mut q,
                    );
                }
                black_box(q.build().unwrap())
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    typical,
    wide_in_list,
    nested_predicate,
    multi_row_insert
);
criterion_main!(benches);
