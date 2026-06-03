//! Build script for `journey_dynamics`.
//!
//! Declares the shared migrations directory as a build input so that adding,
//! removing, or changing any migration file causes this crate (and its
//! integration tests, which embed the migrations via `sqlx::migrate!`) to be
//! recompiled.  Without this, `sqlx::migrate!` only tracks files it already
//! knew about at the previous compile; new files are invisible to cargo's
//! change detection.

fn main() {
    println!("cargo:rerun-if-changed=../../migrations/");
}
