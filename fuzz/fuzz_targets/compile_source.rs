//! Fuzz target for the source compiler.
//!
//! Feeds arbitrary UTF-8 input into the compiler and checks that malformed
//! programs return errors instead of panicking.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        let _ = ferrix_compiler::compile_source(source);
    }
});
