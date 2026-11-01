use std::{
    fs::{self, File},
    io::BufWriter,
    path::{Path, PathBuf},
};
use x3tif_core::develop::{Exposure, Mode};

/// `<dir>/<stem>.tif`, or `<stem>-1.tif`, `<stem>-2.tif`... if that already exists.
fn output_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = input.parent().unwrap_or_else(|| Path::new("."));
    let first = dir.join(format!("{stem}.tif"));
    if !first.exists() {
        return first;
    }
    (1..)
        .map(|n| dir.join(format!("{stem}-{n}.tif")))
        .find(|p| !p.exists())
        .expect("unbounded range")
}

fn convert_file(input: &Path, mode: Mode) -> Result<PathBuf, String> {
    let is_x3f = input
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("x3f"));
    if !is_x3f {
        return Err("X3F ファイルではありません".into());
    }
    let data = fs::read(input).map_err(|e| format!("読み込めません: {e}"))?;
    let out = output_path(input);
    let tmp = out.with_extension("tif.part");
    let result = (|| {
        let mut w = BufWriter::new(File::create(&tmp).map_err(|e| format!("書き込めません: {e}"))?);
        x3tif_core::convert::x3f_to_tiff(data, &mut w, mode).map_err(|e| e.to_string())?;
        std::io::Write::flush(&mut w).map_err(|e| format!("書き込めません: {e}"))?;
        drop(w);
        fs::rename(&tmp, &out).map_err(|e| format!("書き込めません: {e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|()| out)
}

#[tauri::command]
async fn convert(path: String, developed: bool, exposure: Option<f64>) -> Result<String, String> {
    // `exposure` is an EV; none means "match the camera's preview".
    let mode = match (developed, exposure) {
        (false, _) => Mode::Raw,
        (true, Some(ev)) => Mode::Developed(Exposure::Ev(ev)),
        (true, None) => Mode::Developed(Exposure::Auto),
    };
    tauri::async_runtime::spawn_blocking(move || convert_file(Path::new(&path), mode))
        .await
        .map_err(|e| e.to_string())?
        .map(|p| p.to_string_lossy().into_owned())
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![convert])
        .run(tauri::generate_context!())
        .expect("failed to run x3tif");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("x3tif-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn output_path_never_overwrites() {
        let d = temp_dir("out");
        let input = d.join("a.X3F");
        assert_eq!(output_path(&input), d.join("a.tif"));
        fs::write(d.join("a.tif"), b"").unwrap();
        assert_eq!(output_path(&input), d.join("a-1.tif"));
        fs::write(d.join("a-1.tif"), b"").unwrap();
        assert_eq!(output_path(&input), d.join("a-2.tif"));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn invalid_input_leaves_no_files_behind() {
        let d = temp_dir("bad");
        let bad = d.join("bad.x3f");
        fs::write(&bad, vec![0u8; 64]).unwrap();
        assert!(convert_file(&bad, Mode::Raw).is_err());
        assert!(convert_file(&d.join("note.txt"), Mode::Raw).is_err());
        let left: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("bad.x3f")]);
        fs::remove_dir_all(d).unwrap();
    }

    /// Runs only when `X3TIF_SAMPLE` points at a real X3F file (sample photos are not in the repo).
    #[test]
    fn converts_a_real_sample_when_provided() {
        let Some(sample) = std::env::var_os("X3TIF_SAMPLE") else {
            return;
        };
        let d = temp_dir("real");
        let input = d.join("sample.x3f");
        fs::copy(sample, &input).unwrap();

        let out = convert_file(&input, Mode::Raw).unwrap();
        assert_eq!(out, d.join("sample.tif"));
        let head = fs::read(&out).unwrap();
        assert_eq!(&head[..4], b"II*\0");
        // A second conversion must not overwrite the first result.
        assert_eq!(
            convert_file(&input, Mode::Raw).unwrap(),
            d.join("sample-1.tif")
        );
        assert!(!d.join("sample.tif.part").exists());
        fs::remove_dir_all(d).unwrap();
    }
}
