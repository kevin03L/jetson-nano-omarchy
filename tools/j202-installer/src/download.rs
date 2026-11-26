//! Downloads in a thread: NVIDIA's files and the release's.
//! A file takes its name only once complete; a cached file of the expected size is kept.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const L4T_NAME: &str = "Jetson-210_Linux_R32.7.6_aarch64.tbz2";
pub const L4T_URL: &str =
    "https://developer.download.nvidia.com/embedded/L4T/r32_Release_v7.6/T210/Jetson-210_Linux_R32.7.6_aarch64.tbz2";

#[derive(Clone)]
pub struct Item {
    pub url: String,
    pub dest: PathBuf,
    /// None: not known before the server says it
    pub size: Option<u64>,
}

impl Item {
    pub fn is_done(&self) -> bool {
        match (fs::metadata(&self.dest), self.size) {
            (Ok(m), Some(s)) => m.len() == s,
            (Ok(m), None) => m.len() > 0,
            _ => false,
        }
    }
}

#[derive(Default)]
pub struct Progress {
    pub file: String,
    pub index: usize,
    pub count: usize,
    pub done: u64,
    pub total: Option<u64>,
    /// Some when it has ended: Ok, or why it stopped
    pub result: Option<Result<(), String>>,
}

pub struct Task {
    pub progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
}

impl Task {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn start(items: Vec<Item>, ctx: eframe::egui::Context) -> Task {
    let progress = Arc::new(Mutex::new(Progress {
        count: items.len(),
        ..Default::default()
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c) = (progress.clone(), cancel.clone());
    std::thread::spawn(move || {
        let r = items
            .iter()
            .enumerate()
            .filter(|(_, it)| !it.is_done())
            .try_for_each(|(i, it)| fetch(it, i, &p, &c, &ctx));
        p.lock().unwrap().result = Some(r);
        ctx.request_repaint();
    });
    Task { progress, cancel }
}

fn fetch(
    it: &Item,
    index: usize,
    p: &Mutex<Progress>,
    cancel: &AtomicBool,
    ctx: &eframe::egui::Context,
) -> Result<(), String> {
    let name = it.dest.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let resp = ureq::get(&it.url)
        .header("User-Agent", "j202-installer")
        .call()
        .map_err(|e| format!("{name} : {e}"))?;
    let total = resp.body().content_length().or(it.size);
    if let (Some(a), Some(b)) = (total, it.size)
        && a != b
    {
        return Err(format!("{name} : le serveur annonce {a} octets, la release {b}"));
    }
    {
        let mut g = p.lock().unwrap();
        g.file = name.clone();
        g.index = index;
        g.done = 0;
        g.total = total;
    }
    if let Some(dir) = it.dest.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{} : {e}", dir.display()))?;
    }
    let part = part_path(&it.dest);
    let mut out = File::create(&part).map_err(|e| format!("{} : {e}", part.display()))?;
    let mut body = resp.into_body().into_reader();
    let mut buf = vec![0u8; 1 << 16];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(out);
            let _ = fs::remove_file(&part);
            return Err("téléchargement annulé".into());
        }
        let n = body.read(&mut buf).map_err(|e| format!("{name} : {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])
            .map_err(|e| format!("{} : {e}", part.display()))?;
        done += n as u64;
        p.lock().unwrap().done = done;
        ctx.request_repaint();
    }
    out.sync_all().map_err(|e| format!("{} : {e}", part.display()))?;
    drop(out);
    if let Some(t) = total
        && t != done
    {
        let _ = fs::remove_file(&part);
        return Err(format!("{name} : {done} octets reçus sur {t}"));
    }
    fs::rename(&part, &it.dest).map_err(|e| format!("{} : {e}", it.dest.display()))
}

fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_only_at_the_expected_size() {
        let dir = std::env::temp_dir().join(format!("j202-dl-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("f");
        let it = |size| Item {
            url: String::new(),
            dest: dest.clone(),
            size,
        };
        assert!(!it(None).is_done());
        fs::write(&dest, b"abc").unwrap();
        assert!(it(Some(3)).is_done());
        assert!(!it(Some(4)).is_done());
        assert!(it(None).is_done());
        assert_eq!(part_path(&dest), dir.join("f.part"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
