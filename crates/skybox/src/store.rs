use std::error::Error;
use std::path::Path;

use slurm_spank::SpankHandle;

use hpc_image_store::ImageProvisioner;

use crate::{SpankSkyBox, plugin_err, skybox_log_debug};

pub(crate) fn store_pull(
    ssb: &mut SpankSkyBox,
    _spank: &mut SpankHandle,
) -> Result<(), Box<dyn Error>> {
    let edf = match &ssb.edf {
        Some(o) => o,
        None => {
            return plugin_err("couldn't find edf");
        }
    };

    let run = match &mut ssb.run {
        Some(o) => o,
        None => {
            return plugin_err("couldn't find run");
        }
    };

    let provisioner = ImageProvisioner::new(&ssb.config)?;
    let image_digest = provisioner.ensure_image(&edf.image, Path::new(&run.podman_tmp_path))?;
    skybox_log_debug!("Resolved image digest: \"{image_digest}\"");
    run.image_digest = image_digest;

    Ok(())
}
