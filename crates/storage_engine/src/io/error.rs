pub type Result<T> = core::result::Result<T, Error>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[cfg(unix)]
    #[error("IO Open Error: {msg} (Errno: {source})")]
    Open {
        msg: String,
        #[source]
        source: nix::Error,
    },
    #[cfg(unix)]
    #[error("IO Read Error: {msg} (Errno: {source})")]
    Read {
        msg: String,
        #[source]
        source: nix::Error,
    },
    #[cfg(unix)]
    #[error("Sysconf error: {msg} (Errno: {source})")]
    SysConf {
        msg: String,
        #[source]
        source: Option<nix::Error>,
    },

    #[error("IO Open Error: {msg} (Errno: {source})")]
    Open {
        msg: String,
        #[source]
        source: std::io::Error,
    },
    #[error("IO Read Error: {msg} (Errno: {source})")]
    Read {
        msg: String,
        #[source]
        source: std::io::Error,
    },
    #[error("SystemInfo Error: {msg} (Errno: {source})")]
    SystemInfo {
        msg: String,
        #[source]
        source: std::io::Error,
    },

    // #[cfg(windows)]
    // #[error("IO Open Error: {msg} (Error: {source})")]
    // Open {
    //     msg: String,
    //     #[source]
    //     source: windows::core::Error,
    // }
}