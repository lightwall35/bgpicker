use bgpicker_daemon::ctl::{execute, parse_args};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (global, cmd) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            if msg.starts_with("Usage:") {
                println!("{msg}");
                std::process::exit(0);
            }
            eprintln!("{msg}");
            std::process::exit(1);
        }
    };

    match execute(global, cmd) {
        Ok(Some(out)) => println!("{out}"),
        Ok(None) => {}
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
