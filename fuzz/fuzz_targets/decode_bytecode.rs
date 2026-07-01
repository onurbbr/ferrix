//! Fuzz target for bytecode decoding.
//!
//! Feeds arbitrary bytes into the decoder/verifier boundary and checks that
//! invalid bytecode is rejected without panics.

#![no_main]

use ferrix_core::bytecode::decode_program;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = decode_program(data);
});
