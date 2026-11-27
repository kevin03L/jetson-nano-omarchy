//! The project's releases for the J202: read from GitHub, or taken from a folder on disk.

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const REPO: &str = "kevin03L/jetson-nano-omarchy";

/// The NVIDIA L4T archive, when the user put it in the local folder too.
pub const L4T_PREFIX: &str = "Jetson-210_Linux_R";

#[derive(Deserialize, Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
    /// Set when the release is a folder on disk: the files are already there, nothing is downloaded.
    pub local_dir: Option<PathBuf>,
}

fn is_image_asset(name: &str) -> bool {
    name.starts_with("j202-rootfs-") && (name.ends_with(".img.zst") || name.contains(".img.zst.part"))
}

impl Release {
    /// Ordered image assets (`j202-rootfs-*.img.zst` or `j202-rootfs-*.img.zst.part01`, ...).
    pub fn image_parts(&self) -> Vec<&Asset> {
        if let Some(single) = self
            .assets
            .iter()
            .find(|a| a.name.starts_with("j202-rootfs-") && a.name.ends_with(".img.zst"))
        {
            return vec![single];
        }
        let mut parts: Vec<&Asset> = self
            .assets
            .iter()
            .filter(|a| a.name.starts_with("j202-rootfs-") && a.name.contains(".img.zst.part"))
            .collect();
        parts.sort_by(|a, b| a.name.cmp(&b.name));
        parts
    }

    /// The system written on the eMMC's APP partition (first part if split).
    pub fn image(&self) -> Option<&Asset> {
        self.image_parts().into_iter().next()
    }

    pub fn size(&self) -> u64 {
        self.assets.iter().map(|a| a.size).sum()
    }

    /// The NVIDIA L4T archive if it sits in the local folder, so it need not be downloaded.
    pub fn local_l4t(&self) -> Option<PathBuf> {
        let dir = self.local_dir.as_ref()?;
        self.assets
            .iter()
            .find(|a| a.name.starts_with(L4T_PREFIX) && a.name.ends_with("_aarch64.tbz2"))
            .map(|a| dir.join(&a.name))
    }
}

/// A release taken from a folder on disk: its packages, boot archive, system image, and optionally
/// the NVIDIA L4T archive. Nothing is downloaded for it.
pub fn local(dir: &Path) -> Result<Release, String> {
    let mut assets = Vec::new();
    let mut tag = None;
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{} : {e}", dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_release_file = name.ends_with(".pkg.tar.zst")
            || name.ends_with(".pkg.tar.xz")
            || (name.starts_with("j202-boot-") && name.ends_with(".tar.zst"))
            || is_image_asset(&name)
            || (name.starts_with(L4T_PREFIX) && name.ends_with("_aarch64.tbz2"));
        if !is_release_file {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if let Some(rest) = name.strip_prefix("j202-rootfs-")
            && let Some(ver) = rest.split(".img.zst").next()
            && !ver.is_empty()
        {
            tag = Some(ver.to_owned());
        }
        assets.push(Asset {
            name,
            size,
            browser_download_url: String::new(),
        });
    }
    if assets.is_empty() {
        return Err("aucun fichier de release dans ce dossier (ni paquets, ni j202-boot, ni image)".into());
    }
    let tag = tag
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "local".into());
    Ok(Release {
        tag,
        prerelease: false,
        assets,
        local_dir: Some(dir.to_path_buf()),
    })
}

/// The J202 releases, newest first, as GitHub lists them.
pub fn parse(json: &str) -> Result<Vec<Release>, String> {
    let all: Vec<GhRelease> = serde_json::from_str(json).map_err(|e| format!("réponse de GitHub illisible : {e}"))?;
    Ok(all
        .into_iter()
        .filter(|r| !r.draft && r.tag_name.starts_with("j202-"))
        .map(|r| Release {
            tag: r.tag_name,
            prerelease: r.prerelease,
            assets: r.assets,
            local_dir: None,
        })
        .collect())
}

pub fn fetch() -> Result<Vec<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=30");
    let json = ureq::get(&url)
        .header("User-Agent", "j202-installer")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("GitHub ne répond pas : {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("lecture de la réponse de GitHub : {e}"))?;
    let list = parse(&json)?;
    if list.is_empty() {
        return Err("aucune release J202 sur GitHub".into());
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_finds_the_image() {
        let json = r#"[
          {"tag_name":"j202-v1.3","draft":false,"prerelease":false,"assets":[
            {"name":"j202-rootfs-v1.3.img.zst","size":10,"browser_download_url":"u1"},
            {"name":"SHA256SUMS","size":2,"browser_download_url":"u2"}]},
          {"tag_name":"j202-v1.4","draft":true,"assets":[]},
          {"tag_name":"v0.9","draft":false,"assets":[]},
          {"tag_name":"j202-v1.2.2","assets":[
            {"name":"j202-boot-v1.2.2.tar.zst","size":5,"browser_download_url":"u3"}]}
        ]"#;
        let r = parse(json).unwrap();
        assert_eq!(
            r.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(),
            ["j202-v1.3", "j202-v1.2.2"]
        );
        assert_eq!(r[0].image().unwrap().name, "j202-rootfs-v1.3.img.zst");
        assert_eq!(r[0].size(), 12);
        assert!(r[1].image().is_none());
    }

    #[test]
    fn bad_json() {
        assert!(parse("{").is_err());
    }

    #[test]
    fn finds_multipart_image_in_order() {
        let json = r#"[
          {"tag_name":"j202-v1.2.3","draft":false,"prerelease":false,"assets":[
            {"name":"j202-rootfs-v1.2.3.img.zst.part02","size":4,"browser_download_url":"u2"},
            {"name":"j202-rootfs-v1.2.3.img.zst.part01","size":6,"browser_download_url":"u1"}]}
        ]"#;
        let r = parse(json).unwrap();
        let names: Vec<&str> = r[0].image_parts().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            ["j202-rootfs-v1.2.3.img.zst.part01", "j202-rootfs-v1.2.3.img.zst.part02"]
        );
        assert_eq!(r[0].image().unwrap().name, "j202-rootfs-v1.2.3.img.zst.part01");
    }

    #[test]
    fn local_folder_scanned() {
        let dir = std::env::temp_dir().join(format!("j202-local-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "linux-nano-7.2.11-7-aarch64.pkg.tar.zst",
            "j202-boot-v1.3.tar.zst",
            "j202-rootfs-v1.3.img.zst",
            "Jetson-210_Linux_R32.7.6_aarch64.tbz2",
            "notes.txt",
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let r = local(&dir).unwrap();
        assert_eq!(r.tag, "v1.3");
        assert_eq!(r.assets.len(), 4); // notes.txt ignored
        assert!(r.image().is_some());
        assert_eq!(
            r.local_l4t().unwrap(),
            dir.join("Jetson-210_Linux_R32.7.6_aarch64.tbz2")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn local_empty_folder_errs() {
        let dir = std::env::temp_dir().join(format!("j202-local-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(local(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
