//! Bounded local file reads for `ThemeImportDialog.tsx`; call on a background executor.
use std::{io::Read, path::PathBuf};
use t3_core::themes::importer::{MAX_FILE_BYTES, READ_FAILURE, oversized};

pub fn read_files(paths: Vec<PathBuf>) -> Vec<(String, Result<String, String>)> {
    let batch = paths.len() > 1;
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let result = (|| {
                let file = std::fs::File::open(path).map_err(|_| READ_FAILURE.to_owned())?;
                let size = file.metadata().map_err(|_| READ_FAILURE.to_owned())?.len();
                let guard =
                    |size| oversized(size).map(|e| if batch { "too large".to_owned() } else { e });
                if let Some(e) = guard(size) {
                    return Err(e);
                }
                let mut bytes = Vec::new();
                file.take(MAX_FILE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| READ_FAILURE.to_owned())?;
                if let Some(e) = guard(bytes.len() as u64) {
                    return Err(e);
                }
                String::from_utf8(bytes).map_err(|_| READ_FAILURE.to_owned())
            })();
            (name, result)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_small_files_and_reports_missing_and_oversized_files() {
        let dir = std::env::temp_dir().join(format!("t3-theme-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.json");
        let large = dir.join("large.json");
        let missing = dir.join("missing.json");
        std::fs::write(&small, "{}").unwrap();
        std::fs::File::create(&large)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert_eq!(read_files(vec![small.clone()])[0].1, Ok("{}".into()));
        assert_eq!(
            read_files(vec![missing.clone()])[0].1,
            Err(READ_FAILURE.into())
        );
        assert!(
            read_files(vec![large.clone()])[0]
                .1
                .as_ref()
                .unwrap_err()
                .contains("256 KB")
        );
        let batch = read_files(vec![small, large, missing]);
        assert_eq!(batch[1].1, Err("too large".into()));
        assert_eq!(batch[2].1, Err(READ_FAILURE.into()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
