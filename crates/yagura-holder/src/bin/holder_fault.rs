//! A holder that acts out the fault its launch names, for tests of what must
//! not depend on a holder answering. Never shipped: the shipped holder
//! refuses a launch that names a fault.

fn main() -> std::process::ExitCode {
    yagura_holder::run_fault_injection()
}
