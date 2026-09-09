fn main() {
    println!("cargo:rerun-if-changed=syncdir.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("syncdir.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=Failed to compile Windows resource icon: {e}");
        }
    }
}
