//! Owned native command simulator; never touches ambient configuration or hosts.
use std::{env, fs, io::Write, process};

fn main() {
    let image = env::current_exe().unwrap();
    let args = env::args().skip(1).collect::<Vec<_>>().join(" ");
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(image.with_extension("calls"))
            .unwrap(),
        "{args}"
    )
    .unwrap();
    match args.as_str() {
        "--version" => println!("codex-cli 0.163.0"),
        "app-server daemon version" => {
            let probe = fs::read_to_string(image.with_extension("probe")).unwrap();
            // Native0.163 version errors when no server answers; it does not
            // publish stopped-state JSON. The state file is fixture-internal.
            if probe.contains("\"status\":\"stopped\"") {
                process::exit(21);
            }
            print!("{probe}");
        }
        "app-server daemon start" => {
            let mode = fs::read_to_string(image.with_extension("mode")).unwrap_or_default();
            match mode.as_str() {
                "fail" => process::exit(31),
                "block" => std::thread::sleep(std::time::Duration::from_secs(60)),
                "unchanged" => {}
                _ => {
                    fs::copy(
                        image.with_extension("running"),
                        image.with_extension("probe"),
                    )
                    .unwrap();
                }
            }
        }
        _ => process::exit(93),
    }
}
