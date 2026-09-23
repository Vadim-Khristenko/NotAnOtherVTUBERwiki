// `sqlx::migrate!` reads the migrations at compile time; without this cargo
// reuses a stale build after a migration is added.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
