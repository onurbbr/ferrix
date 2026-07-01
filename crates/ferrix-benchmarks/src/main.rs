//! Lightweight benchmark runner for Ferrix.
//!
//! This binary measures stable compiler/runtime scenarios without adding
//! Criterion or other benchmark dependencies to the main workspace.

use std::{
    collections::HashMap,
    env, fs, io,
    path::{Path, PathBuf},
    process,
    time::{Duration, Instant},
};

use ferrix_compiler::compile_source;
use ferrix_vm::{HostCapability, Vm};

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
        name: "dispatch",
        iterations: 10_000,
        source: "\
let i = 0;
let value = 1;
while (i < 250) {
    value = value + i;
    value = value - i;
    value = value * 2;
    value = value / 2;
    i = i + 1;
}
return value;
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

const DEFAULT_THRESHOLD_PCT: f64 = 500.0;
const BASELINE_HEADER: &str = "\
# Ferrix benchmark baseline v1
# Columns:
# case,compile_ms,compile_threshold_pct,verify_ms,verify_threshold_pct,run_avg_us,run_threshold_pct
";

struct BenchCase {
    /// Human-readable case label printed in the benchmark table.
    name: &'static str,
    /// Number of VM executions measured after one compile.
    iterations: usize,
    /// Ferrix source compiled for this case.
    source: &'static str,
}

#[derive(Clone, Copy)]
struct BenchMeasurement {
    /// One-time source compile duration in milliseconds.
    compile_ms: f64,
    /// Average bytecode verification duration in milliseconds.
    verify_ms: f64,
    /// Total VM run duration in milliseconds across all iterations.
    run_total_ms: f64,
    /// Average VM run duration in microseconds.
    run_avg_us: f64,
}

struct BenchResult {
    /// Benchmark scenario label.
    name: &'static str,
    /// Iteration count used for this measurement.
    iterations: usize,
    /// Measured compile/verify/run timings.
    measurement: BenchMeasurement,
}

#[derive(Clone, Copy)]
struct BaselineEntry {
    /// Baseline compile duration in milliseconds.
    compile_ms: f64,
    /// Allowed compile regression percentage.
    compile_threshold_pct: f64,
    /// Baseline average verification duration in milliseconds.
    verify_ms: f64,
    /// Allowed verification regression percentage.
    verify_threshold_pct: f64,
    /// Baseline average run duration in microseconds.
    run_avg_us: f64,
    /// Allowed runtime regression percentage.
    run_threshold_pct: f64,
}

enum Mode {
    /// Measure and print the benchmark table without threshold checks.
    Run,
    /// Compare measurements against a baseline file and fail on regressions.
    Check(PathBuf),
    /// Measure and write a new baseline file.
    Refresh(PathBuf),
    /// Print command usage.
    Help,
}

fn main() {
    let mode = parse_mode(env::args().skip(1)).unwrap_or_else(|error| {
        eprintln!("error: {error}\n");
        eprintln!("{}", usage());
        process::exit(64);
    });

    if matches!(mode, Mode::Help) {
        print!("{}", usage());
        return;
    }

    let results = run_benchmarks();
    match mode {
        Mode::Run => print_summary(&results),
        Mode::Check(path) => {
            let baselines = read_baselines(&path).unwrap_or_else(|error| {
                eprintln!(
                    "error: could not read baseline `{}`: {error}",
                    path.display()
                );
                process::exit(66);
            });
            print_summary(&results);
            if !check_thresholds(&results, &baselines) {
                process::exit(1);
            }
        }
        Mode::Refresh(path) => {
            let previous = read_baselines(&path).unwrap_or_default();
            write_baselines(&path, &results, &previous).unwrap_or_else(|error| {
                eprintln!(
                    "error: could not write baseline `{}`: {error}",
                    path.display()
                );
                process::exit(66);
            });
            print_summary(&results);
            println!("refreshed benchmark baseline at {}", path.display());
        }
        Mode::Help => unreachable!("handled before benchmarks run"),
    }
}

fn usage() -> &'static str {
    "\
Ferrix benchmark runner

Usage:
  ferrix-benchmarks
  ferrix-benchmarks --check <baseline.csv>
  ferrix-benchmarks --refresh <baseline.csv>
  ferrix-benchmarks --help
"
}

fn parse_mode(mut args: impl Iterator<Item = String>) -> Result<Mode, String> {
    let Some(first) = args.next() else {
        return Ok(Mode::Run);
    };
    match first.as_str() {
        "--help" | "-h" => Ok(Mode::Help),
        "--check" => {
            let Some(path) = args.next() else {
                return Err("expected a baseline path after --check".to_string());
            };
            reject_extra_args(args)?;
            Ok(Mode::Check(PathBuf::from(path)))
        }
        "--refresh" => {
            let Some(path) = args.next() else {
                return Err("expected a baseline path after --refresh".to_string());
            };
            reject_extra_args(args)?;
            Ok(Mode::Refresh(PathBuf::from(path)))
        }
        _ => Err(format!("unknown argument `{first}`")),
    }
}

fn reject_extra_args(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    if let Some(extra) = args.next() {
        return Err(format!("unexpected argument `{extra}`"));
    }
    Ok(())
}

fn run_benchmarks() -> Vec<BenchResult> {
    CASES.iter().map(run_case).collect()
}

fn print_summary(results: &[BenchResult]) {
    println!(
        "{:<12} {:>12} {:>12} {:>12} {:>12} {:>10}",
        "case", "compile_ms", "verify_ms", "run_total_ms", "run_avg_us", "iters"
    );
    for result in results {
        println!(
            "{:<12} {:>12.3} {:>12.3} {:>12.3} {:>12.3} {:>10}",
            result.name,
            result.measurement.compile_ms,
            result.measurement.verify_ms,
            result.measurement.run_total_ms,
            result.measurement.run_avg_us,
            result.iterations
        );
    }
}

fn run_case(case: &BenchCase) -> BenchResult {
    let compile_start = Instant::now();
    let program = compile_source(case.source).expect("benchmark source should compile");
    let compile_elapsed = compile_start.elapsed();

    let verify_elapsed = timed(case.iterations, || {
        ferrix_core::bytecode::VerifiedProgram::new(program.as_program().clone())
            .expect("benchmark bytecode should verify");
    });

    let run_elapsed = timed(case.iterations, || {
        let mut vm = Vm::new();
        vm.set_capabilities([HostCapability::NativeCall, HostCapability::IoOutput]);
        ferrix_stdlib::install(&mut vm, program.as_program());
        vm.run_program(&program)
            .expect("benchmark source should execute");
    });

    BenchResult {
        name: case.name,
        iterations: case.iterations,
        measurement: BenchMeasurement {
            compile_ms: millis(compile_elapsed),
            verify_ms: micros(verify_elapsed) / case.iterations as f64 / 1_000.0,
            run_total_ms: millis(run_elapsed),
            run_avg_us: micros(run_elapsed) / case.iterations as f64,
        },
    }
}

fn read_baselines(path: &Path) -> io::Result<HashMap<String, BaselineEntry>> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error),
    };
    let mut baselines = HashMap::new();
    for (line_index, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("case,") {
            continue;
        }
        let entry = parse_baseline_line(line, line_index + 1)?;
        baselines.insert(entry.0, entry.1);
    }
    Ok(baselines)
}

fn parse_baseline_line(line: &str, line_number: usize) -> io::Result<(String, BaselineEntry)> {
    let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
    if fields.len() != 7 {
        return Err(invalid_data(format!(
            "line {line_number}: expected 7 comma-separated fields"
        )));
    }

    Ok((
        fields[0].to_string(),
        BaselineEntry {
            compile_ms: parse_f64(fields[1], line_number, "compile_ms")?,
            compile_threshold_pct: parse_f64(fields[2], line_number, "compile_threshold_pct")?,
            verify_ms: parse_f64(fields[3], line_number, "verify_ms")?,
            verify_threshold_pct: parse_f64(fields[4], line_number, "verify_threshold_pct")?,
            run_avg_us: parse_f64(fields[5], line_number, "run_avg_us")?,
            run_threshold_pct: parse_f64(fields[6], line_number, "run_threshold_pct")?,
        },
    ))
}

fn parse_f64(value: &str, line_number: usize, field: &str) -> io::Result<f64> {
    value.parse::<f64>().map_err(|error| {
        invalid_data(format!(
            "line {line_number}: invalid {field} value `{value}`: {error}"
        ))
    })
}

fn write_baselines(
    path: &Path,
    results: &[BenchResult],
    previous: &HashMap<String, BaselineEntry>,
) -> io::Result<()> {
    let mut output = String::from(BASELINE_HEADER);
    output.push_str("case,compile_ms,compile_threshold_pct,verify_ms,verify_threshold_pct,run_avg_us,run_threshold_pct\n");
    for result in results {
        let thresholds = previous
            .get(result.name)
            .copied()
            .unwrap_or_else(default_baseline_entry);
        output.push_str(&format!(
            "{},{:.3},{:.1},{:.6},{:.1},{:.3},{:.1}\n",
            result.name,
            result.measurement.compile_ms,
            thresholds.compile_threshold_pct,
            result.measurement.verify_ms,
            thresholds.verify_threshold_pct,
            result.measurement.run_avg_us,
            thresholds.run_threshold_pct
        ));
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, output)
}

fn default_baseline_entry() -> BaselineEntry {
    BaselineEntry {
        compile_ms: 0.0,
        compile_threshold_pct: DEFAULT_THRESHOLD_PCT,
        verify_ms: 0.0,
        verify_threshold_pct: DEFAULT_THRESHOLD_PCT,
        run_avg_us: 0.0,
        run_threshold_pct: DEFAULT_THRESHOLD_PCT,
    }
}

fn check_thresholds(results: &[BenchResult], baselines: &HashMap<String, BaselineEntry>) -> bool {
    println!();
    println!(
        "{:<12} {:<10} {:>12} {:>12} {:>10} {:>10} {:>8}",
        "case", "metric", "measured", "baseline", "delta%", "limit%", "status"
    );

    let mut ok = true;
    for result in results {
        let Some(baseline) = baselines.get(result.name) else {
            ok = false;
            println!(
                "{:<12} {:<10} {:>12} {:>12} {:>10} {:>10} {:>8}",
                result.name, "all", "-", "missing", "-", "-", "FAIL"
            );
            continue;
        };

        ok &= print_threshold_row(
            result.name,
            "compile",
            result.measurement.compile_ms,
            baseline.compile_ms,
            baseline.compile_threshold_pct,
        );
        ok &= print_threshold_row(
            result.name,
            "verify",
            result.measurement.verify_ms,
            baseline.verify_ms,
            baseline.verify_threshold_pct,
        );
        ok &= print_threshold_row(
            result.name,
            "run",
            result.measurement.run_avg_us,
            baseline.run_avg_us,
            baseline.run_threshold_pct,
        );
    }
    ok
}

fn print_threshold_row(
    case: &str,
    metric: &str,
    measured: f64,
    baseline: f64,
    threshold_pct: f64,
) -> bool {
    let delta_pct = percentage_delta(measured, baseline);
    let status = if delta_pct <= threshold_pct {
        "ok"
    } else {
        "FAIL"
    };
    println!(
        "{:<12} {:<10} {:>12.3} {:>12.3} {:>9.1}% {:>9.1}% {:>8}",
        case, metric, measured, baseline, delta_pct, threshold_pct, status
    );
    status == "ok"
}

fn percentage_delta(measured: f64, baseline: f64) -> f64 {
    if baseline <= f64::EPSILON {
        if measured <= f64::EPSILON { 0.0 } else { 100.0 }
    } else {
        ((measured - baseline) / baseline) * 100.0
    }
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
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
