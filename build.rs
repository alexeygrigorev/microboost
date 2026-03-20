fn main() {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    println!("cargo:rustc-env=BUILD_TIMESTAMP={}", now);
}
