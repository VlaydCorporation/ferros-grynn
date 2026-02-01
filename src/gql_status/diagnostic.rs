use crate::gql_status::status::StatusObject;

#[derive(Debug)]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug)]
pub struct DiagnosticRecord {
    pub(crate) status: StatusObject,
    pub(crate) severity: Severity
}

impl DiagnosticRecord {
    pub fn error_record(status: StatusObject) -> Self {
        DiagnosticRecord {
            status,
            severity: Severity::Error,
        }
    }
}