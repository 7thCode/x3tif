//! CLI harness: x3tif <in.x3f> <out.tif> [raw|developed [ev]]
//!              x3tif <in.x3f> <out.png> preview [ev [max_side]]
use std::{env, fs, io::BufWriter, process::ExitCode};
use x3tif_core::develop::{Exposure, Mode, Tuning};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if !(3..=6).contains(&args.len()) {
        eprintln!("usage: x3tif <in.x3f> <out.tif> [raw|developed [ev]]");
        eprintln!("       x3tif <in.x3f> <out.png> preview [ev [max_side]]");
        return ExitCode::from(2);
    }
    if args.get(3).map(String::as_str) == Some("preview") {
        let exposure = match args.get(4).and_then(|v| v.parse().ok()) {
            Some(ev) => Exposure::Ev(ev),
            None => Exposure::Auto,
        };
        let max_side = args.get(5).and_then(|v| v.parse().ok()).unwrap_or(1000);
        let run = || -> Result<(), Box<dyn std::error::Error>> {
            let t0 = std::time::Instant::now();
            let loaded = x3tif_core::preview::Loaded::load(fs::read(&args[1])?)?;
            let t1 = std::time::Instant::now();
            let r = loaded.render(exposure, &Tuning::default(), max_side)?;
            let png = x3tif_core::png::encode_rgb8(r.image.width, r.image.height, &r.image.rgb)
                .ok_or("cannot encode")?;
            eprintln!("load {:?}, render + encode {:?}", t1 - t0, t1.elapsed());
            fs::write(&args[2], png)?;
            eprintln!(
                "{}x{} ev {:+.2} wb {}",
                r.image.width, r.image.height, r.exposure_ev, r.white_balance
            );
            Ok(())
        };
        return match run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let mode = match args.get(3).map(String::as_str) {
        None | Some("raw") => Mode::Raw,
        Some("developed") => Mode::Developed(
            match args.get(4).and_then(|v| v.parse().ok()) {
                Some(ev) => Exposure::Ev(ev),
                None => Exposure::Auto,
            },
            Tuning::default(),
        ),
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
