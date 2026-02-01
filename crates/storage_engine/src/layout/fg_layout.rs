use std::path::PathBuf;

const STORE_LOCK_FILENAME: &str = "store_lock";


pub struct FerrosGrynnLayout {
    home_dir: PathBuf,
    data_dir: PathBuf,
    databases_root_dir: PathBuf,
    tx_logs_root_dir: PathBuf,
    scripts_root_dir: PathBuf,
}

impl FerrosGrynnLayout {

}