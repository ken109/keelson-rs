// The promise `docs/type-mappings.md` makes, in the negative: a replacement
// type that cannot actually bind on a backend is a *compile* error, not a
// runtime surprise. `std::net::IpAddr` implements neither `ToValue` nor
// `FromValue`, so it is exactly such a type.
//
// Two ways it is caught, one per case below:
//
//  1. At the derive, when the newtype wraps it: the generated delegation
//     cannot resolve, and the error points at the field — the line a user can
//     actually change.
//  2. At the `const _: () = keelson_exec::assert_bind::<T>();` line
//     keelson-gen emits for every `[[types.override]]`, when the override is
//     a type with no impls at all. One line naming the type, rather than an
//     inference swamp at some distant call site.

use keelson_core::Bind;

#[derive(Bind)]
struct Host(std::net::IpAddr);

struct Plain(std::net::IpAddr);

const _: () = keelson_exec::assert_bind::<Plain>();

fn main() {}
