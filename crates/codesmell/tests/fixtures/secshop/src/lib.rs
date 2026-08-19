// Security fixture: a hard-coded secret constant, a call to a denied sink, and a
// function that can panic.

pub const API_KEY: &str = "do-not-commit-me";

pub fn run_script(code: &str) -> i32 {
    eval_cli(code)
}

fn eval_cli(_code: &str) -> i32 {
    0
}

pub fn may_panic(x: i32) -> i32 {
    if x < 0 {
        do_panic();
    }
    x
}

fn do_panic() {}
