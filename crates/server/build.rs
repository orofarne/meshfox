fn main() {
    // A release build's rust-embed reads the built WebUI while compiling this
    // crate. Cargo otherwise reuses that artifact when only web/dist changes,
    // leaving a newly built meshfox binary serving the previous frontend
    // bundle.
    //
    // A debug build doesn't embed it (`debug-embed` is off): the server reads
    // web/dist from disk on every request, so a frontend-only change needs no
    // Rust rebuild — and watching the directory there would relink the whole
    // binary after every `npm run build` (the e2e suite's `pretest:e2e`).
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        println!("cargo:rerun-if-changed=../../web/dist");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
