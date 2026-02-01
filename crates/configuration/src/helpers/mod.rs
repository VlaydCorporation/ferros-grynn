mod database_name_validator;
mod duration_range;
mod displayable;
mod chrono_unit;

pub use database_name_validator::*;
pub use duration_range::*;
pub use displayable::*;
pub use chrono_unit::*;

pub fn is_blank_string(s: &str) -> bool {
    if s.len() == 0 {
        true
    } else {
        s.chars().all(|c| c.is_whitespace())
    }
}