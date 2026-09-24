fn main() {
    // rust-embed reads the built WebUI while compiling this crate. Cargo
    // otherwise reuses that artifact when only web/dist changes, leaving a
    // newly built meshfox binary serving the previous frontend bundle.
    println!("cargo:rerun-if-changed=../../web/dist");
}
