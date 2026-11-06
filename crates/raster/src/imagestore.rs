use std::path::Path;
use std::fs::{self, File, FileTimes};
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;
use crate::Config;

pub fn imagestore_keepalive(config: &Config) -> Result<Option<String>,String> {
    
    if ! config.parallax_imagestore_keepalive {
        return Ok(None);
    }

    let result1 = imagestore_folder_keepalive(config);
    let result2 = hpc_image_store_folder_keepalive(config);

    let mut error = false;
    let mut output = match result1 {
        Ok(out) => {
            format!("{out}")
        },
        Err(e) => {
            error = true;
            format!("Error: {e}")
        },
    };
    output = match result2 {
        Ok(out) => {
            format!("{output}\n{out}")
        },
        Err(e) => {
            error = true;
            format!("{output}\nError: {e}")
        },
    };

    if error {
        return Err(output);
    }
    Ok(Some(output))
}

pub fn imagestore_folder_keepalive(config: &Config) -> Result<String,String> {
    let imagestore = &config.parallax_imagestore;
    folder_keepalive("parallax_imagestore", imagestore)
}
pub fn hpc_image_store_folder_keepalive(config: &Config) -> Result<String,String> {
    let hpc_image_store = &config.hpc_image_store;
    folder_keepalive("hpc_image_store", hpc_image_store)
}

fn folder_keepalive(name: &str, folder: &str) -> Result<String, String> {
    let path = Path::new(&folder);
    if ! path.exists() {
        return Err(format!("{} {} doesn't exist", name, folder));
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
    let output = format!("Keep alive {} {}, refreshed {}/{} inodes", name, folder, upd_entries, num_entries);
    Ok(output)
}
