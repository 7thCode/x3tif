use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::BufWriter,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};
use tauri::{Emitter, Manager};
use x3tif_core::develop::{white_balance_name, Exposure, Mode, Tuning};
use x3tif_core::preview::Loaded;

/// The saved defaults for the developed output (`settings.json` in the app config folder).
/// Missing fields, unknown fields and a damaged file all fall back to the calibrated defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    /// `None` keeps the white balance the file was shot with.
    white_balance: Option<String>,
    red: f64,
    blue: f64,
    tone: f64,
}

impl Default for Settings {
    fn default() -> Self {
        let t = Tuning::default();
        Settings {
            white_balance: None,
            red: t.red,
            blue: t.blue,
            tone: t.tone,
        }
    }
}

impl Settings {
    fn tuning(&self) -> Tuning {
        Tuning {
            white_balance: self.white_balance.as_deref().and_then(white_balance_name),
            red: self.red,
            blue: self.blue,
            tone: self.tone,
        }
        .sanitized()
    }

    /// The settings as they will actually be used: unknown names dropped, bad values reset.
    fn normalized(&self) -> Settings {
        let t = self.tuning();
        Settings {
            white_balance: t.white_balance.map(str::to_owned),
            red: t.red,
            blue: t.blue,
            tone: t.tone,
        }
    }
}

fn settings_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("settings.json"))
}

fn load_settings(path: &Path) -> Settings {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
        .map(|s| s.normalized())
        .unwrap_or_default()
}

/// Writes the normalized settings through a temporary file, so a crash never leaves half a file.
fn store_settings(path: &Path, settings: &Settings) -> Result<Settings, String> {
    let settings = settings.normalized();
    let json = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("保存できません: {e}"))?;
    }
    let tmp = path.with_extension("json.part");
    fs::write(&tmp, json)
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("保存できません: {e}")
        })?;
    Ok(settings)
}

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> Settings {
    settings_path(&app).map_or_else(Settings::default, |p| load_settings(&p))
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, settings: Settings) -> Result<Settings, String> {
    let path = settings_path(&app).ok_or("設定の保存先が見つかりません")?;
    store_settings(&path, &settings)
}

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

fn check_x3f(input: &Path) -> Result<(), String> {
    let is_x3f = input
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("x3f"));
    if is_x3f {
        Ok(())
    } else {
        Err("X3F ファイルではありません".into())
    }
}

/// The last file decoded for the preview, so that moving a slider only re-develops it.
/// Keyed by path, size and modification time, so a file replaced on disk is read again.
#[derive(Default)]
struct PreviewCache(Mutex<Option<(PreviewKey, Arc<Loaded>)>>);

type PreviewKey = (PathBuf, u64, Option<SystemTime>);

#[derive(Debug, Serialize)]
struct PreviewReply {
    /// `data:image/png;base64,...`
    image: String,
    width: usize,
    height: usize,
    /// The exposure used in EV (the automatic one when none was given).
    exposure: f64,
    /// The white balance actually used.
    white_balance: String,
}

const DEFAULT_PREVIEW_SIDE: usize = 1000;

fn render_preview(
    cache: &PreviewCache,
    input: &Path,
    exposure: Option<f64>,
    settings: &Settings,
    max_side: Option<usize>,
) -> Result<PreviewReply, String> {
    check_x3f(input)?;
    let meta = fs::metadata(input).map_err(|e| format!("読み込めません: {e}"))?;
    let key = (input.to_path_buf(), meta.len(), meta.modified().ok());
    let loaded = {
        let mut slot = cache.0.lock().unwrap_or_else(|e| e.into_inner());
        match &*slot {
            Some((k, l)) if *k == key => Arc::clone(l),
            _ => {
                *slot = None; // a failed load must not leave the previous file's picture behind
                let data = fs::read(input).map_err(|e| format!("読み込めません: {e}"))?;
                let l = Arc::new(Loaded::load(data).map_err(|e| e.to_string())?);
                *slot = Some((key, Arc::clone(&l)));
                l
            }
        }
    };
    let exposure = exposure.map_or(Exposure::Auto, Exposure::Ev);
    let r = loaded
        .render(
            exposure,
            &settings.tuning(),
            max_side.unwrap_or(DEFAULT_PREVIEW_SIDE),
        )
        .map_err(|e| e.to_string())?;
    let png = x3tif_core::png::encode_rgb8(r.image.width, r.image.height, &r.image.rgb)
        .ok_or("プレビューを作れません")?;
    Ok(PreviewReply {
        image: format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        ),
        width: r.image.width,
        height: r.image.height,
        exposure: r.exposure_ev,
        white_balance: r.white_balance,
    })
}

#[tauri::command]
async fn preview(
    cache: tauri::State<'_, Arc<PreviewCache>>,
    path: String,
    exposure: Option<f64>,
    settings: Settings,
    max_side: Option<usize>,
) -> Result<PreviewReply, String> {
    let cache = Arc::clone(&cache);
    tauri::async_runtime::spawn_blocking(move || {
        render_preview(&cache, Path::new(&path), exposure, &settings, max_side)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn convert_file(input: &Path, mode: Mode, progress: impl FnMut(f64)) -> Result<PathBuf, String> {
    check_x3f(input)?;
    let data = fs::read(input).map_err(|e| format!("読み込めません: {e}"))?;
    let out = output_path(input);
    let tmp = out.with_extension("tif.part");
    let result = (|| {
        let mut w = BufWriter::new(File::create(&tmp).map_err(|e| format!("書き込めません: {e}"))?);
        x3tif_core::convert::x3f_to_tiff_with_progress(data, &mut w, mode, progress)
            .map_err(|e| e.to_string())?;
        std::io::Write::flush(&mut w).map_err(|e| format!("書き込めません: {e}"))?;
        drop(w);
        fs::rename(&tmp, &out).map_err(|e| format!("書き込めません: {e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|()| out)
}

/// `exposure` is an EV; none means "match the camera's preview".
fn mode_for(developed: bool, exposure: Option<f64>, tuning: Tuning) -> Mode {
    match (developed, exposure) {
        (false, _) => Mode::Raw,
        (true, Some(ev)) => Mode::Developed(Exposure::Ev(ev), tuning),
        (true, None) => Mode::Developed(Exposure::Auto, tuning),
    }
}

#[derive(Clone, Serialize)]
struct ProgressEvent<'a> {
    path: &'a str,
    fraction: f64,
}

/// `settings` are the adjustments on the screen; without them the saved defaults are used.
#[tauri::command]
async fn convert(
    app: tauri::AppHandle,
    path: String,
    developed: bool,
    exposure: Option<f64>,
    settings: Option<Settings>,
) -> Result<String, String> {
    let tuning = settings
        .unwrap_or_else(|| get_settings(app.clone()))
        .tuning();
    let mode = mode_for(developed, exposure, tuning);
    tauri::async_runtime::spawn_blocking(move || {
        // The screen shows the fraction on the file's row; a lost event only skips a step.
        let progress = |fraction: f64| {
            let _ = app.emit(
                "convert-progress",
                ProgressEvent {
                    path: &path,
                    fraction,
                },
            );
        };
        convert_file(Path::new(&path), mode, progress)
    })
    .await
    .map_err(|e| e.to_string())?
    .map(|p| p.to_string_lossy().into_owned())
}

pub fn run() {
    tauri::Builder::default()
        .manage(Arc::new(PreviewCache::default()))
        .invoke_handler(tauri::generate_handler![
            convert,
            get_settings,
            save_settings,
            preview
        ])
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
    fn settings_round_trip_and_default_to_the_calibration() {
        let d = temp_dir("settings");
        let path = d.join("conf").join("settings.json");
        assert_eq!(load_settings(&path), Settings::default());
        assert_eq!(Settings::default().tuning(), Tuning::default());

        let s = Settings {
            white_balance: Some("Overcast".into()),
            red: 1.1,
            blue: 0.9,
            tone: 0.25,
        };
        assert_eq!(store_settings(&path, &s).unwrap(), s);
        assert_eq!(load_settings(&path), s);
        assert!(!path.with_extension("json.part").exists());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn damaged_or_hostile_settings_never_reach_the_developer() {
        let d = temp_dir("badsettings");
        let path = d.join("settings.json");
        fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load_settings(&path), Settings::default());

        // Unknown fields are ignored, missing ones default, bad values reset, odd names dropped.
        fs::write(
            &path,
            br#"{"white_balance":"../../etc","red":1e308,"blue":-3,"tone":"x","extra":1}"#,
        )
        .unwrap();
        assert_eq!(load_settings(&path), Settings::default());
        fs::write(&path, br#"{"red":1.2,"tone":2.0}"#).unwrap();
        let s = load_settings(&path);
        assert_eq!((s.red, s.blue, s.tone), (1.2, 1.0, Tuning::default().tone));

        let saved = store_settings(
            &path,
            &Settings {
                white_balance: Some("nonsense".into()),
                red: f64::NAN,
                ..Settings::default()
            },
        )
        .unwrap();
        assert_eq!(saved, Settings::default());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn preview_rejects_bad_files_and_keeps_no_stale_picture() {
        let d = temp_dir("pvbad");
        let cache = PreviewCache::default();
        let s = Settings::default();
        let txt = d.join("note.txt");
        fs::write(&txt, b"x").unwrap();
        assert!(render_preview(&cache, &txt, None, &s, None).is_err());
        assert!(render_preview(&cache, &d.join("missing.x3f"), None, &s, None).is_err());
        let bad = d.join("bad.x3f");
        fs::write(&bad, vec![0u8; 64]).unwrap();
        assert!(render_preview(&cache, &bad, None, &s, None).is_err());
        assert!(cache.0.lock().unwrap().is_none());
        fs::remove_dir_all(d).unwrap();
    }

    /// Runs only when `X3TIF_SAMPLE` points at a real X3F file.
    #[test]
    fn previews_a_real_sample_and_reuses_the_decoded_file() {
        let Some(sample) = std::env::var_os("X3TIF_SAMPLE") else {
            return;
        };
        let d = temp_dir("pvreal");
        let input = d.join("sample.x3f");
        fs::copy(sample, &input).unwrap();
        let cache = PreviewCache::default();

        let a = render_preview(&cache, &input, None, &Settings::default(), Some(300)).unwrap();
        assert!(a.image.starts_with("data:image/png;base64,"));
        assert_eq!(a.width.max(a.height), 300);
        let first = Arc::clone(&cache.0.lock().unwrap().as_ref().unwrap().1);

        let tuned = Settings {
            red: 1.3,
            ..Settings::default()
        };
        let b = render_preview(&cache, &input, Some(0.5), &tuned, Some(300)).unwrap();
        assert_ne!(a.image, b.image);
        assert_eq!(b.exposure, 0.5);
        assert!(Arc::ptr_eq(
            &first,
            &cache.0.lock().unwrap().as_ref().unwrap().1
        ));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn the_mode_follows_the_screen_choices() {
        let t = Tuning {
            red: 1.2,
            ..Tuning::default()
        };
        assert_eq!(mode_for(false, Some(1.0), t), Mode::Raw);
        assert_eq!(
            mode_for(true, Some(0.5), t),
            Mode::Developed(Exposure::Ev(0.5), t)
        );
        assert_eq!(mode_for(true, None, t), Mode::Developed(Exposure::Auto, t));
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
        assert!(convert_file(&bad, Mode::Raw, |_| {}).is_err());
        assert!(convert_file(&d.join("note.txt"), Mode::Raw, |_| {}).is_err());
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

        let mut steps = Vec::new();
        let out = convert_file(&input, Mode::Raw, |f| steps.push(f)).unwrap();
        assert_eq!(out, d.join("sample.tif"));
        assert_eq!(steps.first(), Some(&0.0));
        assert_eq!(steps.last(), Some(&1.0));
        assert!(
            steps.len() > 10 && steps.windows(2).all(|w| w[0] <= w[1]),
            "{steps:?}"
        );
        let head = fs::read(&out).unwrap();
        assert_eq!(&head[..4], b"II*\0");
        // A second conversion must not overwrite the first result.
        assert_eq!(
            convert_file(&input, Mode::Raw, |_| {}).unwrap(),
            d.join("sample-1.tif")
        );
        assert!(!d.join("sample.tif.part").exists());
        fs::remove_dir_all(d).unwrap();
    }
}
