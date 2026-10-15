fn main() {
    let version = std::fs::read_to_string("../VERSION").expect("VERSION file");
    println!("cargo:rustc-env=ZA2_VERSION={}", version.trim());
    println!("cargo:rerun-if-changed=../VERSION");

    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app.slint", config).unwrap();
}
