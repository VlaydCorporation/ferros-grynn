// mod common;
pub mod config;
pub mod values;
pub use values::runtime::graph;
mod collections;
pub mod ecs;
pub mod gql;
pub mod gql_status;
pub mod math;
mod syn;

#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

#[cfg(target_env = "msvc")]
use mimalloc::MiMalloc;

#[cfg(target_env = "msvc")]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
