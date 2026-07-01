//! Lightweight benchmark runner for Ferrix.
//!
//! This binary measures a few stable compiler/runtime scenarios without adding
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
    for case in CASES {
        run_case(case);
    }
}

fn run_case(case: &BenchCase) {
    // Compile once so the benchmark separates frontend cost from repeated VM cost.
    let compile_start = Instant::now();
    let program = compile_source(case.source).expect("benchmark source should compile");
    let compile_elapsed = compile_start.elapsed();

    let run_elapsed = timed(case.iterations, || {
        let mut vm = Vm::new();
        ferrix_stdlib::install(&mut vm, program.as_program());
        vm.run_program(&program)
            .expect("benchmark source should execute");
    });

    println!(
        "{:<12} compile={:>8.3}ms run_total={:>8.3}ms run_avg={:>8.3}us iterations={}",
        case.name,
        millis(compile_elapsed),
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
