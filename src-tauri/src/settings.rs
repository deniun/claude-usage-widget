use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::errors::{AppError, AppResult};
use crate::types::Settings;

fn atomic_write(path: &Path, bytes: &[u8]) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// settings.json 저장소. 디스크 접근을 최소화하려고 마지막으로 확정된 값을
/// 메모리에 들고 있는다.
///
/// 이게 없으면 3초 주기 창 위치 저장 루프가 매 사이클 파일을 읽고, 값이 하나도
/// 안 바뀌었는데도 tmp 생성 → write → fsync → rename 을 반복한다(하루 약 28,800회
/// 강제 플러시). load는 캐시에서 돌려주고, save는 값이 실제로 달라졌을 때만
/// 디스크를 건드린다.
pub struct SettingsStore {
    path: PathBuf,
    cache: RwLock<Option<Settings>>,
}

impl SettingsStore {
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self {
            path: app_data_dir.join("settings.json"),
            cache: RwLock::new(None),
        }
    }

    fn read_from_disk(&self) -> Settings {
        match fs::read_to_string(&self.path) {
            Ok(s) => serde_json::from_str::<Settings>(&s).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    pub fn load(&self) -> Settings {
        if let Some(s) = self.cache.read().unwrap().as_ref() {
            return s.clone();
        }
        let mut guard = self.cache.write().unwrap();
        // 락을 다시 잡는 사이 다른 스레드가 채웠을 수 있다.
        if let Some(s) = guard.as_ref() {
            return s.clone();
        }
        let s = self.read_from_disk();
        *guard = Some(s.clone());
        s
    }

    pub fn save(&self, settings: &Settings) -> AppResult<()> {
        let mut guard = self.cache.write().unwrap();
        if guard.as_ref() == Some(settings) {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(settings)?;
        atomic_write(&self.path, &bytes).map_err(|e| AppError::Other(e.to_string()))?;
        *guard = Some(settings.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_returns_default_when_missing() {
        let tmp = TempDir::new().unwrap();
        let store = SettingsStore::new(tmp.path().to_path_buf());
        let s = store.load();
        assert_eq!(s.refresh_interval_sec, 300);
        assert!(s.always_on_top);
    }

    #[test]
    fn save_then_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = SettingsStore::new(tmp.path().to_path_buf());
        let mut s = Settings::default();
        s.opacity = 0.7;
        s.refresh_interval_sec = 60;
        store.save(&s).unwrap();
        let loaded = store.load();
        assert!((loaded.opacity - 0.7).abs() < 1e-9);
        assert_eq!(loaded.refresh_interval_sec, 60);
    }

    #[test]
    fn load_recovers_from_corrupt_json() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(&path, "not json").unwrap();
        let store = SettingsStore::new(tmp.path().to_path_buf());
        let s = store.load();
        assert_eq!(s.refresh_interval_sec, 300);
    }

    #[test]
    fn save_skips_disk_write_when_settings_unchanged() {
        let tmp = TempDir::new().unwrap();
        let store = SettingsStore::new(tmp.path().to_path_buf());
        let s = Settings::default();
        store.save(&s).unwrap();
        // 파일을 지운 뒤 같은 값을 다시 저장하면, 변경이 없으므로 디스크를
        // 건드리지 않아야 한다(= 파일이 다시 생기지 않는다).
        fs::remove_file(tmp.path().join("settings.json")).unwrap();
        store.save(&s).unwrap();
        assert!(!tmp.path().join("settings.json").exists());
    }

    #[test]
    fn save_writes_disk_when_settings_changed() {
        let tmp = TempDir::new().unwrap();
        let store = SettingsStore::new(tmp.path().to_path_buf());
        let mut s = Settings::default();
        store.save(&s).unwrap();
        fs::remove_file(tmp.path().join("settings.json")).unwrap();
        s.window.x += 1;
        store.save(&s).unwrap();
        assert!(tmp.path().join("settings.json").exists());
        assert_eq!(store.load().window.x, Settings::default().window.x + 1);
    }

    #[test]
    fn load_is_served_from_memory_after_first_read() {
        let tmp = TempDir::new().unwrap();
        let store = SettingsStore::new(tmp.path().to_path_buf());
        let mut s = Settings::default();
        s.opacity = 0.5;
        store.save(&s).unwrap();
        // 디스크에서 파일이 사라져도 메모리 캐시로 계속 응답해야 한다
        // (3초 주기 저장 루프가 매번 디스크를 읽지 않게 하려는 목적).
        fs::remove_file(tmp.path().join("settings.json")).unwrap();
        assert!((store.load().opacity - 0.5).abs() < 1e-9);
    }
}
