// `sqlx::migrate!` in src/db.rs embeds the migrations at compile time, and
// cargo does not know it reads them. Without this, adding a migration and
// rebuilding reuses the old naw-core artifact: the binary starts, reports
// "migrations applied", and the new tables are missing. Tell cargo that the
// directory is an input.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
