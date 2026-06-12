use std::path::Path;
use std::fs::{self, File, FileTimes};
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;
use crate::Config;

pub fn imagestore_keepalive(config: &Config) -> Result<Option<String>,String> {
    
    let output;
    let imagestore = &config.parallax_imagestore;

    if ! config.parallax_imagestore_keepalive {
        return Ok(None);
    }

    let path = Path::new(&imagestore);
    if ! path.exists() {
        return Err(format!("imagestore {} doesn't exist", imagestore));
    }

    let now = SystemTime::now();
    let mut num_entries = 0;
    let mut upd_entries = 0;
    for entry in WalkDir::new(&path) {
        num_entries += 1;

        // Best effort, skip errors
        let Ok(entrystr) = entry else { continue };
        let Ok(metadata) = fs::metadata(entrystr.path()) else { continue };
        let Ok(atime) = metadata.accessed() else { continue };
    
        // skip if recent
        if atime.elapsed().unwrap() < Duration::new(86400, 0) { continue }
        
        // Update atime if old
        let Ok(file) = File::open(entrystr.path()) else { continue };
        let Ok(mtime) = metadata.modified() else { continue };
        let times = FileTimes::new()
            .set_accessed(now)
            .set_modified(mtime);
        match file.set_times(times) {
            Ok(_) => (),
            Err(_) => continue,
        }
        upd_entries += 1;
    }
    output = Some(format!("Keep alive imagestore {}, refreshed {}/{} inodes", imagestore, upd_entries, num_entries));
    Ok(output)
}
