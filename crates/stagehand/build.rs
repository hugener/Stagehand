fn main() {
    slint_build::compile("ui/app.slint").expect("the Slint UI should compile");

    // Gives stagehand.exe its icon, which is also what the Presenter Window's mirror
    // shows on the taskbar and in a call's list of windows to share.
    println!("cargo:rerun-if-changed=assets/stagehand.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/stagehand.ico")
            .compile()
            .expect("the icon should be built into the program");
    }
}
