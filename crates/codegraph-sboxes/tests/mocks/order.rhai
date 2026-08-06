// Shared mock lib for the sandbox integration tests.
// Mock contract: `fn <callee>(args) { … }` — `args` is an array of i64.

fn seed(args) { 7 }

fn notify(args) { 1 }

fn poll(args) { 0 }

fn done(args) { 5 }

fn check_stock(args) { 3 }

fn get_stock(args) { 100 }

fn send_email(args) { 9 }

fn insert_order(args) { 42 }

fn log_skip(args) { 3 }

fn then_mock(args) { 11 }

fn else_mock(args) { 22 }
