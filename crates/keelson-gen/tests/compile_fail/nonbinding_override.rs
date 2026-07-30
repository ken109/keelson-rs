// The line keelson-gen emits for every configured type override (see
// `type_overrides_change_the_emitted_type_and_assert_the_bind` in
// config_effects.rs for the emission): a replacement type that cannot bind
// fails to compile here, at one line naming the type — not in an inference
// swamp at some distant call site. `std::net::IpAddr` implements neither
// `ToValue` nor `FromValue`, so it is exactly such a type.
const _: () = keelson_exec::assert_bind::<std::net::IpAddr>();

fn main() {}
