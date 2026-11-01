//! CLI harness: x3tif <in.x3f> <out.tif>
use std::{env, fs, io::BufWriter, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: x3tif <in.x3f> <out.tif>");
        return ExitCode::from(2);
    }
    let run = || -> Result<(), Box<dyn std::error::Error>> {
        let data = fs::read(&args[1])?;
        let mut out = BufWriter::new(fs::File::create(&args[2])?);
        x3tif_core::convert::x3f_to_tiff(data, &mut out)?;
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
