use std::error::Error;
use std::path::PathBuf;

use slurm_spank::SpankHandle;

use sarus_suite_podman_driver::PodmanCtx;
use hpc_image_store::{
    ImageSelector, OverlayStorage, PullContext, PullOptions, Store, StoreConfig, TransferContext,
};

use crate::config::setup_imagestores;
use crate::podman::{pmd_image_exists, pmd_parallax_migrate, pmd_rmi};
use crate::{SpankSkyBox, plugin_err, skybox_log_debug, skybox_log_error};

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

    let config = &ssb.config;
    //let parallax_path = PathBuf::from(&config.parallax_path);
    setup_imagestores(config)?;

    let graphroot = format!("{}/graphroot", run.podman_tmp_path);
    let runroot = format!("{}/runroot", run.podman_tmp_path);

    let ro_ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        conmon_path: Some(PathBuf::from(&config.conmon_path)),
        module: None,
        graphroot: Some(PathBuf::from(&graphroot)),
        runroot: Some(PathBuf::from(&runroot)),
        parallax_mount_program: None,
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        mksquashfs_path: None,
        podman_env: None,
    }
    .with_env("PARALLAX_MP_UID", config.parallax_mp_uid.to_string())
    .with_env("PARALLAX_MP_GID", config.parallax_mp_gid.to_string())
    .with_env(
        "PARALLAX_MP_SQUASHFUSE_CMD",
        config.parallax_mp_squashfuse_path.clone(),
    )
    .with_env("PARALLAX_MP_LOGFILE", config.parallax_mp_logfile.clone());

    let local_ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        conmon_path: Some(PathBuf::from(&config.conmon_path)),
        module: None,
        graphroot: Some(PathBuf::from(&graphroot)),
        runroot: Some(PathBuf::from(&runroot)),
        parallax_mount_program: None,
        ro_store: None,
        mksquashfs_path: None,
        podman_env: None,
    }
    .with_env("PARALLAX_MP_UID", config.parallax_mp_uid.to_string())
    .with_env("PARALLAX_MP_GID", config.parallax_mp_gid.to_string())
    .with_env(
        "PARALLAX_MP_SQUASHFUSE_CMD",
        config.parallax_mp_squashfuse_path.clone(),
    )
    .with_env("PARALLAX_MP_LOGFILE", config.parallax_mp_logfile.clone());

    let migrate_ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        conmon_path: Some(PathBuf::from(&config.conmon_path)),
        module: None,
        graphroot: Some(PathBuf::from(&graphroot)),
        runroot: Some(PathBuf::from(&runroot)),
        parallax_mount_program: None,
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        mksquashfs_path: Some(PathBuf::from(&config.mksquashfs_path)),
        podman_env: None,
    }
    .with_env("PARALLAX_MP_UID", config.parallax_mp_uid.to_string())
    .with_env("PARALLAX_MP_GID", config.parallax_mp_gid.to_string())
    .with_env(
        "PARALLAX_MP_SQUASHFUSE_CMD",
        config.parallax_mp_squashfuse_path.clone(),
    )
    .with_env("PARALLAX_MP_LOGFILE", config.parallax_mp_logfile.clone());

    let store = Store::new(StoreConfig::new(&config.hpc_image_store)?);
    let selector = ImageSelector::new(&edf.image)?;

    let image_digest = match store.resolve(&selector) {
        Ok(option) => match option {
            Some(digest) => digest,
            None => {
                store_pull_resolve(&store, &selector)? 
            },
        },
        Err(_) => {
            store_pull_resolve(&store, &selector)? 
        },
    };
    skybox_log_debug!("Resolved image digest: \"{image_digest}\"");
    run.image_digest = image_digest.clone();

    if !pmd_image_exists(&image_digest, &ro_ctx)? {
        let image_digest_selector = ImageSelector::new(&image_digest)?;
        skybox_log_debug!("exporting image \"{image_digest}\" from hpc_image_store");

        let transfer_ctx = TransferContext {
            podman: PathBuf::from(&config.podman_path),
            skopeo: PathBuf::from(&config.skopeo_path),
            platform: None,
            storage: Some(OverlayStorage::new(&graphroot, &runroot)?),
        };

        store.export(&image_digest_selector, None, &transfer_ctx)?;

        skybox_log_debug!("migrating image \"{image_digest}\" to parallax imagestore");
        pmd_parallax_migrate(&config.parallax_path, &migrate_ctx, &image_digest)?;

        skybox_log_debug!("removing image \"{image_digest}\" from local graphroot");
        if let Err(error) = pmd_rmi(&image_digest, &local_ctx) {
            skybox_log_error!(
                "failed to remove image \"{}\" from local graphroot: {}",
                image_digest,
                error
            );
        }

        if !pmd_image_exists(&image_digest, &ro_ctx)? {
            return plugin_err("couldn't find image on parallax imagestore after migration");
        }
    }

    Ok(())
}

fn store_pull_resolve(store: &Store, selector: &ImageSelector) -> Result<String, Box<dyn Error>> {

    skybox_log_debug!("pulling image \"{selector}\" from remote in hpc_image_store");
    store.pull(selector, None, &PullContext::default(), &PullOptions::default())?;
    let image_digest = match store.resolve(&selector)? {
        Some(digest) => digest,
        None => {
            return Err("hpc_image_store pull failed, cannot find image in hpc_image_store".into());
        },
    };

    Ok(image_digest)
}
