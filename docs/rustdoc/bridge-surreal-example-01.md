```no_run
# #[cfg(feature = "bridge-surreal-mem")]
# async fn example() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
use ferros_grynn::{
	bridge::{BridgeData, FerrosBridge, SurrealQueryBridgeExt},
	datastore::Datastore,
};
let client =
	surrealdb::Surreal::new::<surrealdb::engine::local::Mem>(()).await.map_err(|error| {
		ferros_grynn::gql_status::FerrosGrynnError::bridge_operation_error(
			"example client failed",
		)
		.with_technical_source(error)
	})?;
let mut result = client
	.query("RETURN $bridge")
	.use_bridge(
		FerrosBridge::new(Datastore::in_memory()),
		BridgeData::InternalQuery("RETURN [none, null, 7]", None),
	)
	.await?;
let value: surrealdb::types::Value = result.take(0).map_err(|error| {
	ferros_grynn::gql_status::FerrosGrynnError::bridge_operation_error("example result failed")
		.with_technical_source(error)
})?;
# Ok(()) }
# fn main() {}
```
