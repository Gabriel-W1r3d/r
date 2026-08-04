fn main() {
    // Compiles the Slint UI file into Rust code at build time.
    slint_build::compile("ui/appwindow.slint").expect("Failed to compile Slint UI");
}
