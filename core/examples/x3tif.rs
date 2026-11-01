//! CLI harness: x3tif <in.x3f> <out.tif> [raw|developed]
use std::{env, fs, io::BufWriter, process::ExitCode};
use x3tif_core::develop::Mode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if !(3..=4).contains(&args.len()) {
        eprintln!("usage: x3tif <in.x3f> <out.tif> [raw|developed]");
        return ExitCode::from(2);
    }
    let mode = match args.get(3).map(String::as_str) {
        None | Some("raw") => Mode::Raw,
        Some("developed") => Mode::Developed,
        Some(other) => {
            eprintln!("unknown mode: {other}");
            return ExitCode::from(2);
        }
    };
    let run = || -> Result<(), Box<dyn std::error::Error>> {
        let data = fs::read(&args[1])?;
        let mut out = BufWriter::new(fs::File::create(&args[2])?);
        x3tif_core::convert::x3f_to_tiff(data, &mut out, mode)?;
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
