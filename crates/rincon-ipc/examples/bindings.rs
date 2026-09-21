//! Writes the TypeScript bindings the desktop interface compiles against.
//!
//! Run via `just bindings`. A test in `rincon-ipc` compares the checked-in file with what this
//! produces, so forgetting to run it fails the build rather than the app.

// A generator's job is to write a file and say so.
#![allow(clippy::print_stdout, clippy::expect_used)]

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = root.join(rincon_ipc::bindings::BINDINGS_PATH);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("cannot create the bindings directory");
    }

    // LF regardless of host, matching `.gitattributes`. Writing CRLF here would make the
    // drift test fail on Windows for a reason that has nothing to do with the contract.
    let contents = rincon_ipc::bindings::render().replace("\r\n", "\n");
    std::fs::write(&path, contents).expect("cannot write the bindings file");

    println!("wrote {}", path.display());
}
