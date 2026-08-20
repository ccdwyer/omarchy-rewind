use rewindd::perms;
use rewindd::run_cli;
use std::env;

fn main() {
    perms::install_umask();
    let args: Vec<String> = env::args().skip(1).collect();
    let data = perms::default_data_dir();
    let code = run_cli(&args, data);
    std::process::exit(code);
}
