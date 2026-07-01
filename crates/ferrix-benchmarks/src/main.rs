//! Lightweight benchmark runner for Ferrix.
//!
//! This binary measures stable compiler/runtime scenarios without adding
//! Criterion or other benchmark dependencies to the main workspace.

use std::time::{Duration, Instant};

use ferrix_compiler::compile_source;
use ferrix_vm::Vm;

const CASES: &[BenchCase] = &[
    BenchCase {
        name: "arithmetic",
        iterations: 20_000,
        source: "return (40 + 2) * 3 - 84;",
    },
    BenchCase {
        name: "loop",
        iterations: 10_000,
        source: "\
let i = 0;
let sum = 0;
while (i < 100) {
    sum = sum + i;
    i = i + 1;
}
return sum;
",
    },
    BenchCase {
        name: "calls",
        iterations: 10_000,
        source: "\
fn fib(n) {
    if (n < 2) {
        return n;
    } else {
        return fib(n - 1) + fib(n - 2);
    }
}
return fib(8);
",
    },
    BenchCase {
        name: "allocation",
        iterations: 10_000,
        source: "\
let values = [1, 2, 3, 4, 5];
let user = { \"name\": \"Ferrix\", \"values\": values };
return len(user[\"values\"]);
",
    },
    BenchCase {
        name: "records",
        iterations: 10_000,
        source: "\
let user = { name: \"Ferrix\", score: 1 };
let i = 0;
while (i < 50) {
    user.score = user.score + 1;
    i = i + 1;
}
return user.score;
",
    },
    BenchCase {
        name: "closures",
        iterations: 10_000,
        source: "\
let base = 40;
let add = fn(value) {
    return base + value;
};
return add(2);
",
    },
];

struct BenchCase {
    /// Human-readable case label printed in the benchmark table.
    name: &'static str,
    /// Number of VM executions measured after one compile.
    iterations: usize,
    /// Ferrix source compiled for this case.
    source: &'static str,
}

fn main() {
    println!(
        "{:<12} {:>12} {:>12} {:>12} {:>12} {:>10}",
        "case", "compile_ms", "verify_ms", "run_total_ms", "run_avg_us", "iters"
    );
    for case in CASES {
        run_case(case);
    }
}

fn run_case(case: &BenchCase) {
    let compile_start = Instant::now();
    let program = compile_source(case.source).expect("benchmark source should compile");
    let compile_elapsed = compile_start.elapsed();

    let verify_elapsed = timed(case.iterations, || {
        ferrix_core::bytecode::VerifiedProgram::new(program.as_program().clone())
            .expect("benchmark bytecode should verify");
    });

    let run_elapsed = timed(case.iterations, || {
        let mut vm = Vm::new();
        ferrix_stdlib::install(&mut vm, program.as_program());
        vm.run_program(&program)
            .expect("benchmark source should execute");
    });

    println!(
        "{:<12} {:>12.3} {:>12.3} {:>12.3} {:>12.3} {:>10}",
        case.name,
        millis(compile_elapsed),
        micros(verify_elapsed) / case.iterations as f64 / 1_000.0,
        millis(run_elapsed),
        micros(run_elapsed) / case.iterations as f64,
        case.iterations
    );
}

fn timed(iterations: usize, mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    start.elapsed()
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000.0
}
